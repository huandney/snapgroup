use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// True se /boot está montado em FAT32 (vfat).
pub fn is_fat32() -> bool {
    is_fat32_path(Path::new("/boot"))
}

pub fn is_fat32_path(boot: &Path) -> bool {
    boot_fstype(boot)
        .map(|fstype| fstype.eq_ignore_ascii_case("vfat"))
        .unwrap_or(false)
}

pub fn boot_fstype(boot: &Path) -> Result<String> {
    Command::new("findmnt")
        .args(["-no", "FSTYPE", "--target"])
        .arg(boot)
        .output()
        .context("findmnt falhou")
        .and_then(|out| {
            if !out.status.success() {
                bail!(
                    "findmnt FSTYPE {}: {}",
                    boot.display(),
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            let fstype = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if fstype.is_empty() {
                bail!("FSTYPE vazio para {}", boot.display());
            }
            Ok(fstype)
        })
}

/// Sincroniza kernel e initramfs em /boot (FAT32) com o subvolume restaurado.
///
/// Para cada kernel ativo em /boot:
///   - localiza /usr/lib/modules/<kver>/ no snapshot cujo `pkgbase` casa
///   - copia o vmlinuz daquele kver para /boot
///   - regenera o initramfs com `mkinitcpio -k <kver> -r <restored_root>`
///
/// No final, recalcula hashes BLAKE2B em /boot/limine.conf. Sempre roda
/// quando /boot é FAT32 — o running kernel reportado por `uname -r` é só
/// o que carregou no boot atual, não reflete o estado escrito em /boot
/// (um `pacman -Syu` sem reboot deixa kernel novo no FAT32 e antigo
/// rodando; pular sync nesse caso quebra o boot seguinte).
const LIMINE_MUTEX_LIB: &str = "/usr/lib/limine/limine-mutex";
const LIMINE_LOCK_PATH: &str = "/tmp/limine-global.lock";

/// Adquire o mutex global do ecossistema limine antes de escrever em /boot.
///
/// Os pacman hooks, o limine-entry-tool e os scripts de mkinitcpio serializam
/// escritas no /boot com `flock` em /tmp/limine-global.lock (timeout de 30s).
/// O lock próprio do snapg só serializa instâncias do snapg; sem este, snapg e
/// um hook de pacman podiam escrever no /boot ao mesmo tempo. `Ok(None)`
/// quando o limine não está instalado — não cria lixo em /tmp.
fn lock_limine_mutex() -> Result<Option<fs::File>> {
    if !Path::new(LIMINE_MUTEX_LIB).exists() {
        return Ok(None);
    }
    // Sem truncate: o arquivo é compartilhado com o tooling do limine e o
    // lock é no inode, não no conteúdo.
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(LIMINE_LOCK_PATH)
        .with_context(|| format!("abrir {LIMINE_LOCK_PATH}"))?;

    use std::os::fd::AsRawFd;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(Some(file));
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(err).with_context(|| format!("flock {LIMINE_LOCK_PATH}"));
        }
        if std::time::Instant::now() >= deadline {
            bail!(
                "outro processo segura o lock do limine ({LIMINE_LOCK_PATH}) há mais de 30s \
                 (pacman/mkinitcpio em andamento?). Aguarde ele terminar e tente de novo."
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

pub fn sync_fat32_paths(restored_root: &Path, boot: &Path) -> Result<()> {
    let mut panel = crate::ui::boot_sync::BootSyncPanel::new();
    sync_fat32_paths_with_panel(restored_root, boot, &mut panel)
}

pub(crate) fn sync_fat32_after_restore(restored_root: &Path) -> Result<()> {
    let mut panel = crate::ui::boot_sync::BootSyncPanel::restore_after_btrfs();
    sync_fat32_paths_with_panel(restored_root, Path::new("/boot"), &mut panel)
}

pub(crate) struct CancelBootSync {
    panel: crate::ui::boot_sync::BootSyncPanel,
}

impl CancelBootSync {
    pub(crate) fn finish_btrfs(mut self) {
        self.panel.finish_btrfs();
    }
}

pub(crate) fn sync_fat32_before_cancel(remaining_root: &Path) -> Result<Option<CancelBootSync>> {
    if !is_fat32() {
        return Ok(None);
    }
    let mut panel = crate::ui::boot_sync::BootSyncPanel::cancel_before_btrfs();
    sync_fat32_paths_with_panel(remaining_root, Path::new("/boot"), &mut panel)?;
    Ok(Some(CancelBootSync { panel }))
}

fn sync_fat32_paths_with_panel(
    restored_root: &Path,
    boot: &Path,
    panel: &mut crate::ui::boot_sync::BootSyncPanel,
) -> Result<()> {
    if !is_fat32_path(boot) {
        return Ok(());
    }

    // Segura o mutex do limine durante backup, sync e verify. Liberado no drop
    // (fim da função), inclusive em erro.
    let _limine_mutex = lock_limine_mutex()?;

    let groups = discover_kernel_groups(boot)?;
    if groups.is_empty() {
        bail!("nenhum vmlinuz/initramfs ativo encontrado em {}", boot.display());
    }

    let interrupted = boot_backup_remnant(boot);

    let critical = critical_boot_files(boot, &groups);
    // No resync de interrupção NÃO recriar o backup: o remanescente é o último
    // estado bom conhecido de /boot (pré-primeira tentativa). `backup_boot_files`
    // apaga e recria, então sobrescrevê-lo com o /boot meio-sincronizado atual
    // destruiria o único rollback existente se esta passada também cair.
    if !interrupted {
        panel.start_backup();
        // Cópia de ~130MB: I/O mudo de alguns segundos. Spinner + nome do arquivo
        // em curso (não há subprocesso pra streamar; o "processo" aqui são os
        // arquivos sendo copiados).
        let pb = crate::ui::term::spinner("copiando arquivos de /boot…".to_string());
        let r = backup_boot_files(boot, &critical, |name| pb.set_message(format!("copiando {name}")));
        pb.finish_and_clear();
        if let Err(e) = r {
            panel.fail_current("backup falhou");
            return Err(e);
        }
        panel.finish_backup();
    } else {
        panel.reuse_backup();
    }

    if let Err(e) = sync_inner(restored_root, boot, &groups, panel) {
        panel.fail_current("sincronização falhou");
        restore_backup_after_failure(boot);
        return Err(e);
    }

    // Verify + limpeza do backup: dois I/O mudos de ~130MB, entre "limine.conf
    // concluído" e "estado sincronizado". Um spinner cobre os dois pra esse
    // trecho não ficar parado.
    panel.start_verify();
    let pb = crate::ui::term::spinner("verificando /boot contra o snapshot…".to_string());
    if let Err(e) = verify_synced(restored_root, boot, &groups) {
        pb.finish_and_clear();
        panel.fail_current("sincronização falhou");
        restore_backup_after_failure(boot);
        return Err(e);
    }

    // Sync verificado: /boot está bootável. Remover o backup é só limpeza —
    // falha aqui não invalida o sync nem pode bloquear o reboot (o caller trata
    // Err como desync), só avisa. Um backup que sobra vira NeedsSync no próximo
    // doctor, que reroda este mesmo caminho seguro.
    pb.set_message("removendo backup de /boot…".to_string());
    pb.disable_steady_tick();
    pb.tick();
    pb.enable_steady_tick(std::time::Duration::from_millis(120));
    let cleanup = fs::remove_dir_all(boot_backup_dir(boot));
    pb.finish_and_clear();
    if let Err(e) = cleanup {
        crate::ui::boot_sync::print_backup_cleanup_failed(&e);
    }
    panel.finish_synced();
    Ok(())
}

/// Restaura o backup de /boot após uma falha de sync/verify e reporta. Caminho
/// de erro compartilhado pelas duas falhas possíveis (sync_inner e verify).
fn restore_backup_after_failure(boot: &Path) {
    crate::ui::boot_sync::print_restore_backup_after_failure();
    if let Err(re) = restore_backup_path(boot) {
        crate::ui::boot_sync::print_backup_restore_failed(&re);
    }
}

/// True se há um backup de boot remanescente. O backup só é removido após o
/// `verify_synced`, então sua presença significa que um sync começou e não
/// fechou limpo (queda de energia / interrupção) — o sinal durável que
/// sobrevive a um reboot, ao contrário de qualquer estado em memória.
fn boot_backup_remnant(boot: &Path) -> bool {
    boot_backup_dir(boot).exists()
}

fn sync_inner(
    restored_root: &Path,
    boot: &Path,
    groups: &[KernelGroup],
    panel: &mut crate::ui::boot_sync::BootSyncPanel,
) -> Result<()> {
    let modules_root = restored_root.join("usr/lib/modules");
    if !modules_root.exists() {
        bail!(
            "/usr/lib/modules não existe no snapshot: {}",
            modules_root.display()
        );
    }
    let config = restored_root.join("etc/mkinitcpio.conf");
    if !config.exists() {
        bail!("mkinitcpio.conf não encontrado em {}", config.display());
    }
    let pkgbase_map = read_pkgbase_map(&modules_root)?;

    for group in groups {
        let kver = pkgbase_map.get(&group.kernel_name).with_context(|| {
            format!(
                "snapshot não contém módulos para o kernel '{}' (procurado em {})",
                group.kernel_name,
                modules_root.display()
            )
        })?;
        let snap_vmlinuz = modules_root.join(kver).join("vmlinuz");
        if !snap_vmlinuz.exists() {
            bail!(
                "vmlinuz não encontrado para {kver}: {}",
                snap_vmlinuz.display()
            );
        }
        for dest in &group.vmlinuz_paths {
            panel.start_vmlinuz();
            fs::copy(&snap_vmlinuz, dest).with_context(|| {
                format!(
                    "copiar vmlinuz {} → {}",
                    snap_vmlinuz.display(),
                    dest.display()
                )
            })?;
            panel.finish_vmlinuz();
        }

        for dest in &group.initramfs_paths {
            panel.start_initramfs();
            // Painel fixo no topo; a saída ao vivo do mkinitcpio vai num spinner
            // que se atualiza no lugar (sem clear_screen por linha, que empilhava
            // em alguns terminais). Limpa o spinner mesmo em falha, antes do `?`.
            let pb = crate::ui::term::spinner(String::new());
            let r = regen_initramfs(&config, kver, restored_root, dest, |l| {
                pb.set_message(clean_mkinitcpio_line(l));
            });
            pb.finish_and_clear();
            r?;
            panel.finish_initramfs();
        }
    }

    panel.start_limine();
    let pb = crate::ui::term::spinner("atualizando hashes do limine.conf…".to_string());
    let r = refresh_limine_boot_hashes(boot).context("atualizar hashes do limine.conf");
    pb.finish_and_clear();
    r?;
    panel.finish_limine();
    Ok(())
}

/// True se /boot já casa com o kernel de `candidate_root` — i.e. o sync
/// pós-rollback seria no-op. Permite ao caller suprimir o aviso de FAT32
/// antes do rollback, usando o mesmo sinal byte-a-byte que o gate do sync.
/// /boot não-FAT32 → `Ok(true)` (nada a sincronizar/avisar). `Err` só em
/// falha real de leitura; o caller trata isso como fail-safe (mantém o aviso).
pub fn boot_already_synced(candidate_root: &Path) -> Result<bool> {
    boot_already_synced_paths(candidate_root, Path::new("/boot"))
}

pub fn boot_already_synced_paths(candidate_root: &Path, boot: &Path) -> Result<bool> {
    if !is_fat32_path(boot) {
        return Ok(true);
    }
    let groups = discover_kernel_groups(boot)?;
    if groups.is_empty() {
        bail!("nenhum vmlinuz/initramfs ativo encontrado em {}", boot.display());
    }
    boot_ready(candidate_root, boot, &groups)
}

/// Motivo de um `/boot` FAT32 estar dessincronizado, para a UI explicar a ação.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootIssue {
    /// Backup de boot remanescente: um sync anterior começou e não fechou limpo
    /// (queda de energia / interrupção). O initramfs pode ter ficado velho.
    InterruptedSync,
    /// O vmlinuz em /boot diverge do kernel do root alvo.
    KernelMismatch,
    /// Os hashes BLAKE2B do limine.conf não batem com os arquivos do /boot. O
    /// Limine recusa a entrada no estágio do bootloader (boot trava antes do
    /// kernel). Vmlinuz pode até casar — um sync interrompido deixa o hash e o
    /// arquivo dessincronizados entre si.
    HashMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootHealth {
    NativeBoot,
    Synced,
    NeedsSync(BootIssue),
    /// O fstab do root alvo declara /boot como FAT32, mas ele aparece com outro
    /// filesystem — sinal de que /boot não está montado. Não há o que diagnosticar
    /// ou sincronizar até montá-lo; a UI orienta a recuperação.
    Unmounted,
}

#[derive(Debug, Clone)]
pub struct BootDiagnosis {
    pub fstype: String,
    pub root_kernel: String,
    pub boot_kernel: String,
    pub kernel_groups: usize,
    pub initramfs_files: usize,
    pub health: BootHealth,
}

/// True se o fstab do root alvo declara o mountpoint de `boot` como vfat.
/// Distingue um /boot BTRFS nativo de um /boot FAT32 separado apenas desmontado.
/// Best-effort: fstab ausente/ilegível ou sem a entrada → false (mantém o
/// caminho atual de diagnóstico em vez de quebrar com erro).
fn fstab_declares_vfat_boot(root: &Path, boot: &Path) -> bool {
    let mountpoint = boot_mountpoint_in(root, boot);
    let Ok(content) = fs::read_to_string(root.join("etc/fstab")) else {
        return false;
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let (_spec, mp, fstype) = (fields.next(), fields.next(), fields.next());
        if mp == Some(mountpoint.as_str())
            && fstype.is_some_and(|t| t.eq_ignore_ascii_case("vfat"))
        {
            return true;
        }
    }
    false
}

/// Mountpoint de `boot` no namespace do root alvo, como apareceria no fstab:
/// "/boot" tanto para (root=/, boot=/boot) quanto para (root=/mnt, boot=/mnt/boot).
fn boot_mountpoint_in(root: &Path, boot: &Path) -> String {
    match boot.strip_prefix(root) {
        Ok(rel) => format!("/{}", rel.display()),
        Err(_) => boot.display().to_string(),
    }
}

/// Contexto de um boot de resgate: `/` está montado de um subvolume diferente
/// do que o fstab declara como `/`. Quem deve receber o sync de `/boot` é o
/// subvol padrão (o que boota), não o snapshot de resgate montado agora.
#[derive(Debug, Clone)]
pub struct RescueContext {
    pub current_subvol: String,
    pub default_subvol: String,
    pub device: String,
}

/// Detecta boot de resgate: compara o subvol montado em `/` (`findmnt FSROOT`)
/// com o subvol que o fstab declara para `/`. `None` quando casam (boot normal)
/// ou quando não dá para determinar (sem `subvol=` no fstab — ex.: root direto
/// em partição). É o mesmo princípio do gate de FAT32 (fstab como verdade),
/// aplicado ao root: impede o doctor de sincronizar `/boot` para o ambiente de
/// resgate em vez do `/@` que vai bootar.
pub fn detect_rescue_boot() -> Result<Option<RescueContext>> {
    let Some(default) = fstab_root_subvol() else {
        return Ok(None);
    };
    let current = findmnt_field("FSROOT", Path::new("/"))?;
    if !subvols_diverge(&current, &default) {
        return Ok(None);
    }
    let source = findmnt_field("SOURCE", Path::new("/"))?;
    let device = source.split('[').next().unwrap_or(&source).trim().to_string();
    Ok(Some(RescueContext {
        current_subvol: normalize_subvol(&current),
        default_subvol: normalize_subvol(&default),
        device,
    }))
}

fn findmnt_field(field: &str, target: &Path) -> Result<String> {
    let out = Command::new("findmnt")
        .args(["-no", field, "--target"])
        .arg(target)
        .output()
        .context("findmnt falhou")?;
    if !out.status.success() {
        bail!(
            "findmnt {field} {}: {}",
            target.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let val = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if val.is_empty() {
        bail!("findmnt {field} vazio para {}", target.display());
    }
    Ok(val)
}

fn fstab_root_subvol() -> Option<String> {
    let content = fs::read_to_string("/etc/fstab").ok()?;
    parse_fstab_root_subvol(&content)
}

/// Subvol que o fstab declara para `/` (normalizado, sem "/" inicial — ex: "@").
/// É o root que boota por padrão. Serve para operar no `@` certo mesmo de dentro
/// de um boot de resgate, onde `/` é um snapshot e não o `@`.
pub fn default_root_subvol() -> Option<String> {
    fstab_root_subvol().map(|s| normalize_subvol(&s))
}

/// Extrai o `subvol=` da entrada `/` do fstab. Pura, para teste.
fn parse_fstab_root_subvol(content: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let (_spec, mp, _fstype, opts) =
            (fields.next(), fields.next(), fields.next(), fields.next());
        if mp != Some("/") {
            continue;
        }
        for opt in opts?.split(',') {
            if let Some(v) = opt.strip_prefix("subvol=") {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Subvol sem a `/` inicial, para comparar fstab ("/@") e FSROOT ("/@/...").
fn normalize_subvol(s: &str) -> String {
    s.trim().trim_start_matches('/').to_string()
}

fn subvols_diverge(current: &str, default: &str) -> bool {
    normalize_subvol(current) != normalize_subvol(default)
}

/// Kernel rodando agora (uname -r), de `/proc/sys/kernel/osrelease`.
pub fn running_kernel() -> Option<String> {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|s| s.trim().to_string())
}

/// Versão do kernel no `/boot` ativo, identificada comparando o `vmlinuz` de
/// `/boot` com o do kernel rodando. `?` se não casar (ex: `/boot` tem outro
/// kernel que não o em execução) — o nome só sai de uma comparação byte-a-byte,
/// já que o arquivo não traz a versão.
pub fn boot_kernel_label(boot: &Path) -> String {
    boot_kernel_version(boot).unwrap_or_else(|| "?".to_string())
}

fn boot_kernel_version(boot: &Path) -> Option<String> {
    let kver = running_kernel()?;
    let running_vmlinuz = Path::new("/usr/lib/modules").join(&kver).join("vmlinuz");
    let running_bytes = fs::read(&running_vmlinuz).ok()?;
    let groups = discover_kernel_groups(boot).ok()?;
    for group in groups {
        for vmlinuz_path in group.vmlinuz_paths {
            if let Ok(boot_bytes) = fs::read(&vmlinuz_path)
                && boot_bytes == running_bytes
            {
                return Some(kver);
            }
        }
    }
    None
}

/// Kernels presentes em `<root>/usr/lib/modules`, ordenados e juntos por
/// vírgula — rótulo curto para a UI mostrar a transição (montado vs boota).
/// `?` quando o diretório não existe ou está vazio.
pub fn kernel_label(root: &Path) -> String {
    let modules = root.join("usr/lib/modules");
    let Ok(entries) = fs::read_dir(&modules) else {
        return "?".to_string();
    };
    let mut kvers: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    kvers.sort();
    if kvers.is_empty() {
        return "?".to_string();
    }
    kvers.join(", ")
}

pub fn diagnose_boot(root: &Path, boot: &Path) -> Result<BootDiagnosis> {
    let fstype = boot_fstype(boot)?;
    let root_kernel = kernel_label(root);
    if !fstype.eq_ignore_ascii_case("vfat") {
        // /boot FAT32 separado que não está montado: `findmnt --target` resolve
        // para o mount pai (o root BTRFS), então `fstype` vem "btrfs" e seria
        // classificado como nativo — falso "nada a fazer". O fstab do root alvo
        // é a fonte da verdade sobre o que /boot deveria ser.
        if fstab_declares_vfat_boot(root, boot) {
            return Ok(BootDiagnosis {
                fstype,
                root_kernel,
                boot_kernel: "?".to_string(),
                kernel_groups: 0,
                initramfs_files: 0,
                health: BootHealth::Unmounted,
            });
        }
        if !fstype.eq_ignore_ascii_case("btrfs") {
            bail!(
                "{} usa filesystem de boot '{fstype}', não suportado pelo doctor",
                boot.display()
            );
        }
        return Ok(BootDiagnosis {
            fstype,
            boot_kernel: root_kernel.clone(),
            root_kernel,
            kernel_groups: 0,
            initramfs_files: 0,
            health: BootHealth::NativeBoot,
        });
    }

    let groups = discover_kernel_groups(boot)?;
    if groups.is_empty() {
        bail!("nenhum vmlinuz/initramfs ativo encontrado em {}", boot.display());
    }
    let initramfs_files = groups
        .iter()
        .map(|group| group.initramfs_paths.len())
        .sum();
    // Backup remanescente denuncia um sync interrompido antes da comparação de
    // vmlinuz: na janela entre copiar o vmlinuz e regenerar o initramfs, o
    // vmlinuz pode já casar enquanto o initramfs segue velho. Tratar como
    // problema corrigível em vez de confiar no vmlinuz isolado.
    let health = if boot_backup_remnant(boot) {
        BootHealth::NeedsSync(BootIssue::InterruptedSync)
    } else if !boot_matches_snapshot(root, &groups)? {
        BootHealth::NeedsSync(BootIssue::KernelMismatch)
    } else if !limine_hashes_match(boot)? {
        // Vmlinuz casa, mas o Limine valida cada entrada pelo hash BLAKE2B do
        // limine.conf no boot. Hash velho com arquivo novo (sync interrompido)
        // faz o bootloader recusar a entrada antes de carregar o kernel.
        BootHealth::NeedsSync(BootIssue::HashMismatch)
    } else {
        BootHealth::Synced
    };
    Ok(BootDiagnosis {
        fstype,
        root_kernel,
        boot_kernel: boot_kernel_label(boot),
        kernel_groups: groups.len(),
        initramfs_files,
        health,
    })
}

/// True se cada vmlinuz ativo em /boot já é byte-idêntico ao vmlinuz do kver
/// correspondente no snapshot restaurado — a pergunta "o /boot já casa com o
/// root?". Serve a dois usos: gate de entrada (pular o sync quando nada mudou)
/// e verificação pós-sync. O sinal certo é o snapshot, não `uname -r` (que só
/// reflete o kernel que carregou no boot atual). `Err` só para falha real de
/// leitura; kernel ausente no snapshot ou conteúdo divergente é `Ok(false)`
/// (mismatch) — leva ao caminho de sync, que falha com mensagem por-kernel.
///
/// LIMITAÇÃO: compara só o `vmlinuz`, não o initramfs. O initramfs não é um
/// artefato armazenado no snapshot (é regenerado), então não há o que comparar
/// byte a byte. Consequências: (a) num restore de mesmo kernel o gate pula a
/// regeneração, mantendo o initramfs do sistema vivo — se o snapshot tiver
/// `mkinitcpio.conf`/hooks diferentes, o initramfs pode não casar; (b) um
/// mkinitcpio que sai 0 com initramfs errado passa no verify. O caso crítico
/// (módulos version-locked) é coberto pelo vmlinuz; o gap do initramfs só
/// some de vez com `/boot` no BTRFS (ver ADR boot-and-standalone-decision §B2.3).
fn boot_matches_snapshot(restored_root: &Path, groups: &[KernelGroup]) -> Result<bool> {
    let modules_root = restored_root.join("usr/lib/modules");
    let pkgbase_map = read_pkgbase_map(&modules_root)?;
    for group in groups {
        let Some(kver) = pkgbase_map.get(&group.kernel_name) else {
            return Ok(false);
        };
        let snap_vmlinuz = modules_root.join(kver).join("vmlinuz");
        let expected =
            fs::read(&snap_vmlinuz).with_context(|| format!("ler {}", snap_vmlinuz.display()))?;
        for dest in &group.vmlinuz_paths {
            if !dest.exists() {
                return Ok(false);
            }
            let got = fs::read(dest).with_context(|| format!("ler {}", dest.display()))?;
            if got != expected {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// `/boot` está pronto pra bootar `restored_root`? Une os dois sinais que o
/// Limine valida no boot: vmlinuz idêntico ao do snapshot E hashes do
/// limine.conf coerentes com os arquivos. Esse predicado serve para diagnóstico
/// e fluxo pendente; o sync FAT32 não usa isso como licença para pular
/// mkinitcpio, porque initramfs pode divergir sem troca de vmlinuz.
fn boot_ready(restored_root: &Path, boot: &Path, groups: &[KernelGroup]) -> Result<bool> {
    Ok(boot_matches_snapshot(restored_root, groups)? && limine_hashes_match(boot)?)
}

/// Verificação pós-sync: se o /boot não casa com o snapshot depois de copiar
/// kernel e regenerar initramfs, o sync não surtiu efeito (parcial/interrompido).
/// Bootar nesse estado cai em emergency mode (kernel não acha seus módulos, ex:
/// "unknown filesystem type vfat"), então vira erro explícito em vez de um
/// reboot silencioso pra um sistema que não sobe.
fn verify_synced(restored_root: &Path, boot: &Path, groups: &[KernelGroup]) -> Result<()> {
    if !boot_matches_snapshot(restored_root, groups)? {
        bail!("/boot dessincronizado: não corresponde ao snapshot após o sync");
    }
    // Mesmo com vmlinuz e initramfs no lugar, o Limine recusa entradas cujo hash
    // no limine.conf não bate com o arquivo. Verificar aqui evita declarar o
    // restore pronto pra reboot e o bootloader travar antes do kernel.
    if !limine_hashes_match(boot)? {
        bail!("/boot dessincronizado: hashes do limine.conf não batem com os arquivos");
    }
    Ok(())
}

/// Normaliza uma linha de progresso do mkinitcpio pra linha viva: remove a
/// indentação inicial e os marcadores `==>` / `->`, deixando só o texto num
/// alinhamento constante. Sem isso, a margem do texto pula entre passos maiores
/// (`==>`, sem indentação) e sub-passos (`  -> `, indentados). O spinner já
/// sinaliza atividade — o marcador é ruído.
fn clean_mkinitcpio_line(l: &str) -> String {
    let t = l.trim();
    let t = t.strip_prefix("==>").unwrap_or(t);
    let t = t.strip_prefix("->").unwrap_or(t);
    t.trim_start().to_string()
}

fn regen_initramfs(
    config: &Path,
    kver: &str,
    restored_root: &Path,
    out: &Path,
    on_line: impl FnMut(&str),
) -> Result<()> {
    // Gera para um temporário no mesmo diretório e renomeia atomicamente.
    // Escrever direto em `out` deixaria um initramfs parcial no caminho ativo
    // se o mkinitcpio fosse interrompido (SIGKILL no reboot, queda de luz) —
    // bootar com initramfs truncado cai em emergency mode. O `.` inicial impede
    // que o temp seja classificado como artefato de boot por `scan_boot_dir`.
    let file_name = out
        .file_name()
        .with_context(|| format!("initramfs sem nome de arquivo: {}", out.display()))?;
    let tmp = out.with_file_name(format!(".{}.snapg_tmp", file_name.to_string_lossy()));

    let mut cmd = Command::new("mkinitcpio");
    cmd.args(["--nopost", "-c"])
        .arg(config)
        .args(["-k", kver, "-r"])
        .arg(restored_root)
        .arg("-g")
        .arg(&tmp);
    if let Err(e) = crate::proc::run_streamed(cmd, on_line) {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("mkinitcpio {kver} → {}", out.display()));
    }

    fs::rename(&tmp, out).with_context(|| {
        format!(
            "mover initramfs {} → {}",
            tmp.display(),
            out.display()
        )
    })?;
    Ok(())
}

/// Mapa pkgbase → kver. Lê /usr/lib/modules/<kver>/pkgbase do snapshot e
/// inverte para "linux-cachyos" → "7.0.1-1-cachyos". O snapshot precisa
/// ter `pkgbase` em cada dir de módulos (padrão Arch desde 2021); kver dirs
/// sem pkgbase são ignorados.
fn read_pkgbase_map(modules_root: &Path) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for entry in
        fs::read_dir(modules_root).with_context(|| format!("ler {}", modules_root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let pkgbase_file = entry.path().join("pkgbase");
        if !pkgbase_file.exists() {
            continue;
        }
        let pkgbase = fs::read_to_string(&pkgbase_file)
            .with_context(|| format!("ler {}", pkgbase_file.display()))?
            .trim()
            .to_string();
        let kver = entry.file_name().to_string_lossy().into_owned();
        map.insert(pkgbase, kver);
    }
    Ok(map)
}

/// Grupo de artefatos em /boot pertencentes ao mesmo kernel.
struct KernelGroup {
    kernel_name: String,
    vmlinuz_paths: Vec<PathBuf>,
    initramfs_paths: Vec<PathBuf>,
}

/// Descobre os artefatos de kernel em /boot de forma afirmativa.
/// Prioriza o bootloader ativo (Limine, BLS/machine-id, UKI, Flat) e NUNCA
/// faz varredura recursiva cega em diretórios de outros sistemas operacionais (ex: Windows/EFI).
fn discover_kernel_groups(boot: &Path) -> Result<Vec<KernelGroup>> {
    let mut by_name: HashMap<String, KernelGroup> = HashMap::new();
    scan_limine_conf(boot, &mut by_name)?;
    scan_bls_dirs(boot, &mut by_name)?;
    scan_uki_dir(boot, &mut by_name)?;
    scan_flat_boot_dir(boot, &mut by_name)?;

    let mut groups: Vec<KernelGroup> = by_name.into_values().collect();
    for group in &mut groups {
        group.vmlinuz_paths.sort_unstable();
        group.vmlinuz_paths.dedup();
        group.initramfs_paths.sort_unstable();
        group.initramfs_paths.dedup();
    }
    groups.sort_by(|a, b| a.kernel_name.cmp(&b.kernel_name));
    Ok(groups)
}

/// Varre entradas BLS Type #1 diretamente em /boot/<machine-id>/<kernel_name>/
fn scan_bls_dirs(boot: &Path, out: &mut HashMap<String, KernelGroup>) -> Result<()> {
    let Ok(entries) = fs::read_dir(boot) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.len() == 32 && name.chars().all(|c| c.is_ascii_hexdigit()) {
            let machine_dir = entry.path();
            if let Ok(kernel_dirs) = fs::read_dir(&machine_dir) {
                for k_entry in kernel_dirs.flatten() {
                    let k_name = k_entry.file_name().to_string_lossy().into_owned();
                    if k_name == "limine_history"
                        || !k_entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
                    {
                        continue;
                    }
                    let k_dir = k_entry.path();
                    let vmlinuz = k_dir.join("vmlinuz");
                    let initramfs = k_dir.join("initramfs");
                    if vmlinuz.exists() || initramfs.exists() {
                        let group = out.entry(k_name.clone()).or_insert_with(|| KernelGroup {
                            kernel_name: k_name,
                            vmlinuz_paths: Vec::new(),
                            initramfs_paths: Vec::new(),
                        });
                        if vmlinuz.exists() && !group.vmlinuz_paths.contains(&vmlinuz) {
                            group.vmlinuz_paths.push(vmlinuz);
                        }
                        if initramfs.exists() && !group.initramfs_paths.contains(&initramfs) {
                            group.initramfs_paths.push(initramfs);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Varre o layout Flat tradicional do Arch apenas no topo de /boot (sem recursão).
fn scan_flat_boot_dir(boot: &Path, out: &mut HashMap<String, KernelGroup>) -> Result<()> {
    let Ok(entries) = fs::read_dir(boot) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if let Some((kernel_name, is_vmlinuz)) = classify_boot_file(&name) {
            let group = out.entry(kernel_name.to_string()).or_insert_with(|| KernelGroup {
                kernel_name: kernel_name.to_string(),
                vmlinuz_paths: Vec::new(),
                initramfs_paths: Vec::new(),
            });
            if is_vmlinuz && !group.vmlinuz_paths.contains(&path) {
                group.vmlinuz_paths.push(path);
            } else if !is_vmlinuz && !group.initramfs_paths.contains(&path) {
                group.initramfs_paths.push(path);
            }
        }
    }
    Ok(())
}

/// Varre UKIs especificamente dentro do diretório padrão /boot/EFI/Linux/*.efi
fn scan_uki_dir(boot: &Path, out: &mut HashMap<String, KernelGroup>) -> Result<()> {
    let uki_dir = boot.join("EFI/Linux");
    if !uki_dir.exists() {
        return Ok(());
    }
    let Ok(entries) = fs::read_dir(&uki_dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if name.ends_with(".efi")
            && !name.contains("fallback")
            && let Some(base) = name.strip_suffix(".efi")
        {
            let kernel = if let Some((_, k)) = base.split_once('_') {
                k
            } else {
                base
            };
            if kernel.starts_with("linux") || kernel.starts_with("vmlinuz") {
                let group = out.entry(kernel.to_string()).or_insert_with(|| KernelGroup {
                    kernel_name: kernel.to_string(),
                    vmlinuz_paths: Vec::new(),
                    initramfs_paths: Vec::new(),
                });
                if !group.vmlinuz_paths.contains(&path) {
                    group.vmlinuz_paths.push(path);
                }
            }
        }
    }
    Ok(())
}

/// Classifica um arquivo em /boot pelo nome como `(kernel_name, is_vmlinuz)`.
/// - `vmlinuz-linux-cachyos`       → `("linux-cachyos", true)`
/// - `initramfs-linux-cachyos.img` → `("linux-cachyos", false)`
///
/// Fallback initramfs (`initramfs-*-fallback*`) é ignorado.
fn classify_boot_file(name: &str) -> Option<(&str, bool)> {
    if let Some(rest) = name.strip_prefix("vmlinuz-") {
        return Some((strip_img_ext(rest), true));
    }
    if let Some(rest) = name.strip_prefix("initramfs-") {
        let stripped = strip_img_ext(rest);
        if stripped.contains("fallback") {
            return None;
        }
        return Some((stripped, false));
    }
    None
}

fn strip_img_ext(s: &str) -> &str {
    s.strip_suffix(".img").unwrap_or(s)
}

/// Extrai entradas de kernel ativas diretamente do limine.conf (se existir),
/// ignorando subentradas de histórico/snapshots e entradas de outros SOs (protocol: efi).
fn scan_limine_conf(boot: &Path, out: &mut HashMap<String, KernelGroup>) -> Result<()> {
    let conf_path = boot.join("limine.conf");
    if !conf_path.exists() {
        return Ok(());
    }
    let content =
        fs::read_to_string(&conf_path).with_context(|| format!("ler {}", conf_path.display()))?;
    let mut in_snapshots = false;
    let mut current_kernel_name: Option<String> = None;
    let mut current_protocol: Option<String> = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//Snapshots") || trimmed.starts_with("///Snapshots") {
            in_snapshots = true;
            continue;
        }
        if in_snapshots {
            if trimmed.starts_with("/+")
                || (trimmed.starts_with('/')
                    && !trimmed.starts_with("///")
                    && !trimmed.starts_with("////"))
            {
                in_snapshots = false;
            } else {
                continue;
            }
        }
        if !in_snapshots {
            if trimmed.starts_with('/')
                && !trimmed.starts_with("///")
                && !trimmed.starts_with("////")
            {
                current_protocol = None;
                current_kernel_name = None;
            }
            if let Some(proto) = trimmed.strip_prefix("protocol:") {
                current_protocol = Some(proto.trim().to_lowercase());
            }
            if let Some(entry_name) = trimmed.strip_prefix("//") {
                let name = entry_name.trim();
                if !name.is_empty() && !name.starts_with('/') {
                    current_kernel_name = Some(name.to_string());
                }
            }
            if let Some(k_id) = trimmed.strip_prefix("comment: kernel-id=") {
                let id = k_id.trim();
                if !id.is_empty() {
                    current_kernel_name = Some(id.to_string());
                }
            }
        }

        // Ignora entradas de outros sistemas operacionais / chainloaders (ex: protocol: efi do Windows)
        if current_protocol.as_deref() == Some("efi") {
            continue;
        }

        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim();
            let val = value.trim();
            let uri_without_hash = val.split_once('#').map(|(u, _)| u).unwrap_or(val).trim();
            if let Some(rel) = uri_without_hash.strip_prefix("boot():/") {
                let full_path = boot.join(rel);
                if full_path.exists() {
                    let is_vmlinuz = matches!(key, "path" | "kernel_path");
                    let is_initramfs = matches!(key, "module_path" | "image_path");
                    if is_vmlinuz || is_initramfs {
                        let k_name = current_kernel_name.clone().unwrap_or_else(|| {
                            full_path
                                .parent()
                                .and_then(|p| p.file_name())
                                .and_then(|n| n.to_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "default".to_string())
                        });
                        let group = out.entry(k_name.clone()).or_insert_with(|| KernelGroup {
                            kernel_name: k_name,
                            vmlinuz_paths: Vec::new(),
                            initramfs_paths: Vec::new(),
                        });
                        if is_vmlinuz && !group.vmlinuz_paths.contains(&full_path) {
                            group.vmlinuz_paths.push(full_path);
                        } else if is_initramfs && !group.initramfs_paths.contains(&full_path) {
                            group.initramfs_paths.push(full_path);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Conjunto de arquivos críticos para backup: vmlinuz/initramfs ativos de
/// todos os kernels descobertos + limine.conf (e .old, se existir).
fn critical_boot_files(boot: &Path, groups: &[KernelGroup]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for g in groups {
        files.extend(g.vmlinuz_paths.iter().cloned());
        files.extend(g.initramfs_paths.iter().cloned());
    }
    for extra in ["limine.conf", "limine.conf.old"] {
        let p = boot.join(extra);
        if p.exists() {
            files.push(p);
        }
    }
    files
}

fn boot_backup_dir(boot: &Path) -> PathBuf {
    boot.join(".snapg_boot_backup")
}

fn backup_boot_files(boot: &Path, files: &[PathBuf], mut on_file: impl FnMut(&str)) -> Result<()> {
    let backup = boot_backup_dir(boot);
    if backup.exists() {
        let _ = fs::remove_dir_all(&backup);
    }
    fs::create_dir_all(&backup).context("criar diretório de backup do boot")?;

    for src in files {
        if let Some(name) = src.file_name().and_then(|n| n.to_str()) {
            on_file(name);
        }
        let rel = src
            .strip_prefix(boot)
            .with_context(|| format!("{} não está dentro de {}", src.display(), boot.display()))?;
        let dest = backup.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).with_context(|| format!("criar {}", parent.display()))?;
        }
        fs::copy(src, &dest)
            .with_context(|| format!("backup {} → {}", src.display(), dest.display()))?;
    }
    Ok(())
}

pub fn restore_backup_path(boot: &Path) -> Result<()> {
    if !is_fat32_path(boot) {
        return Ok(());
    }
    let backup = boot_backup_dir(boot);
    if !backup.exists() {
        return Ok(());
    }
    restore_backup_dir(&backup, boot)?;
    let _ = fs::remove_dir_all(&backup);
    crate::ui::boot_sync::print_backup_restored();
    Ok(())
}

fn restore_backup_dir(src: &Path, dest: &Path) -> Result<()> {
    for entry in fs::read_dir(src).with_context(|| format!("ler backup {}", src.display()))? {
        let entry = entry?;
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&dest_path)
                .with_context(|| format!("criar {}", dest_path.display()))?;
            restore_backup_dir(&src_path, &dest_path)?;
        } else {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("criar {}", parent.display()))?;
            }
            fs::copy(&src_path, &dest_path).with_context(|| {
                format!(
                    "restaurar backup {} → {}",
                    src_path.display(),
                    dest_path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn refresh_limine_boot_hashes(boot: &Path) -> Result<()> {
    let path = boot.join("limine.conf");
    if !path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(&path).with_context(|| format!("ler {}", path.display()))?;
    let had_trailing_newline = content.ends_with('\n');
    let mut changed = false;
    let mut lines = Vec::new();
    let mut current_protocol = String::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(proto) = trimmed.strip_prefix("protocol:") {
            current_protocol = proto.trim().to_lowercase();
        } else if trimmed.starts_with('/') {
            current_protocol.clear();
        }

        // Não injeta hash em entradas EFI limpas (ex: Windows / chainloader)
        let refreshed = if current_protocol == "efi" && !line.contains('#') {
            line.to_string()
        } else {
            refresh_limine_hash_for_line(boot, line)?
        };

        if refreshed != line {
            changed = true;
        }
        lines.push(refreshed);
    }
    if !changed {
        return Ok(());
    }
    let mut updated = lines.join("\n");
    if had_trailing_newline {
        updated.push('\n');
    }
    let tmp = path.with_extension("conf.snapg_tmp");
    fs::write(&tmp, updated).context("escrever limine.conf temporário")?;
    fs::rename(&tmp, &path).with_context(|| format!("substituir {}", path.display()))?;
    Ok(())
}

/// Verifica que cada entrada do limine.conf com hash registrado bate com o
/// BLAKE2B do arquivo referenciado — o mesmo check que o Limine faz no boot. Só
/// conta linhas que têm `#hash` E cujo arquivo existe: hash ausente não é
/// validado pelo Limine, e arquivo ausente já é pego pelo scan de kernels.
/// `limine.conf` ausente → `Ok(true)` (nada a verificar / sistema sem limine).
fn limine_hashes_match(boot: &Path) -> Result<bool> {
    let path = boot.join("limine.conf");
    if !path.exists() {
        return Ok(true);
    }
    let content = fs::read_to_string(&path).with_context(|| format!("ler {}", path.display()))?;
    let mut current_protocol = String::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(proto) = trimmed.strip_prefix("protocol:") {
            current_protocol = proto.trim().to_lowercase();
        } else if trimmed.starts_with('/') {
            current_protocol.clear();
        }

        // Ignora entradas EFI da verificação de hash do kernel
        if current_protocol == "efi" {
            continue;
        }

        let Some(recorded) = limine_recorded_hash(line) else {
            continue;
        };
        let Some(boot_path) = limine_boot_path_from_line(boot, line) else {
            continue;
        };
        if !boot_path.exists() {
            continue;
        }
        if blake2b_hex(&boot_path)? != recorded {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Hash registrado após o `#` numa linha de path do limine.conf, se houver. A URI
/// `boot():/...` não contém `#`, então o último `#` separa o hash. Linha sem `#`
/// ou com hash vazio → `None` (nada registrado para validar).
fn limine_recorded_hash(line: &str) -> Option<&str> {
    let (_, hash) = line.rsplit_once('#')?;
    let hash = hash.trim();
    if hash.is_empty() {
        return None;
    }
    Some(hash)
}

fn refresh_limine_hash_for_line(boot: &Path, line: &str) -> Result<String> {
    let Some(boot_path) = limine_boot_path_from_line(boot, line) else {
        return Ok(line.to_string());
    };
    if !boot_path.exists() {
        return Ok(line.to_string());
    }

    let hash = blake2b_hex(&boot_path)?;
    let Some(hash_pos) = line.find('#') else {
        return Ok(format!("{}#{hash}", line.trim_end()));
    };
    Ok(format!("{}#{hash}", line[..hash_pos].trim_end()))
}

fn limine_boot_path_from_line(boot: &Path, line: &str) -> Option<PathBuf> {
    let trimmed = line.trim_start();
    let (key, value) = trimmed.split_once(':')?;
    let key = key.trim();
    if !matches!(key, "path" | "kernel_path" | "module_path" | "image_path") {
        return None;
    }

    let uri = value.trim();
    let uri_without_hash = uri.split_once('#').map(|(uri, _)| uri).unwrap_or(uri);
    let boot_relative = uri_without_hash.strip_prefix("boot():/")?;
    if boot_relative.contains(char::is_whitespace) {
        return None;
    }
    Some(boot.join(boot_relative))
}

fn blake2b_hex(path: &Path) -> Result<String> {
    let out = Command::new("b2sum")
        .arg(path)
        .output()
        .with_context(|| format!("calcular BLAKE2B de {}", path.display()))?;
    if !out.status.success() {
        bail!(
            "b2sum {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .split_whitespace()
        .next()
        .map(String::from)
        .context("b2sum não retornou hash")
}

#[cfg(test)]
mod tests {
    use super::{
        boot_backup_remnant, boot_mountpoint_in, classify_boot_file, clean_mkinitcpio_line,
        fstab_declares_vfat_boot, kernel_label, limine_boot_path_from_line, limine_recorded_hash,
        parse_fstab_root_subvol, subvols_diverge,
    };
    use std::path::Path;

    #[test]
    fn clean_mkinitcpio_line_strips_markers_and_indent() {
        // Passos maiores e sub-passos devem virar texto no mesmo alinhamento.
        assert_eq!(
            clean_mkinitcpio_line("==> Starting build: '7.0'"),
            "Starting build: '7.0'"
        );
        assert_eq!(
            clean_mkinitcpio_line("  -> Running build hook: [base]"),
            "Running build hook: [base]"
        );
        // Linha sem marcador continua intacta (só sem espaços nas pontas).
        assert_eq!(clean_mkinitcpio_line("  pronto  "), "pronto");
    }

    #[test]
    fn kernel_label_lists_module_dirs() {
        let base = std::env::temp_dir().join(format!("snapg_kver_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // sem usr/lib/modules → "?"
        std::fs::create_dir_all(&base).unwrap();
        assert_eq!(kernel_label(&base), "?");
        // com um kver
        std::fs::create_dir_all(base.join("usr/lib/modules/7.0.3-1-cachyos")).unwrap();
        assert_eq!(kernel_label(&base), "7.0.3-1-cachyos");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn limine_recorded_hash_extracts_only_present_hashes() {
        // Linha de path com hash: pega o que vem após o último '#', sem espaços.
        assert_eq!(
            limine_recorded_hash("    kernel_path: boot():/vmlinuz-linux#abc123 "),
            Some("abc123")
        );
        // Sem '#' → nada registrado pra validar.
        assert_eq!(limine_recorded_hash("    kernel_path: boot():/vmlinuz-linux"), None);
        // Hash vazio após '#' → None (não trata "" como hash).
        assert_eq!(limine_recorded_hash("    kernel_path: boot():/vmlinuz#"), None);
        assert_eq!(limine_recorded_hash("    kernel_path: boot():/vmlinuz#   "), None);
    }

    #[test]
    fn parses_fstab_root_subvol() {
        let fstab = "# /etc/fstab\n\
             UUID=aaa\t/\tbtrfs\tsubvol=/@,defaults,noatime,compress=zstd:1\t0 0\n\
             UUID=bbb /boot vfat defaults 0 2\n";
        assert_eq!(parse_fstab_root_subvol(fstab).as_deref(), Some("/@"));
        // entrada "/" sem subvol= (root direto em partição) → None
        assert_eq!(parse_fstab_root_subvol("UUID=aaa / btrfs defaults 0 0\n"), None);
        // sem entrada "/" → None (não confundir com /home)
        assert_eq!(
            parse_fstab_root_subvol("UUID=bbb /home btrfs subvol=/@home 0 0\n"),
            None
        );
    }

    #[test]
    fn detects_subvol_divergence() {
        // boot normal: / é o subvol padrão
        assert!(!subvols_diverge("/@", "/@"));
        // resgate: snapshot montado em /
        assert!(subvols_diverge("/@/.snapshots/660/snapshot", "/@"));
        // normalização tolera a "/" inicial divergente
        assert!(!subvols_diverge("@", "/@"));
    }

    #[test]
    fn boot_mountpoint_relative_to_root() {
        assert_eq!(boot_mountpoint_in(Path::new("/"), Path::new("/boot")), "/boot");
        assert_eq!(
            boot_mountpoint_in(Path::new("/mnt"), Path::new("/mnt/boot")),
            "/boot"
        );
    }

    #[test]
    fn fstab_vfat_boot_detection() {
        let base = std::env::temp_dir().join(format!("snapg_fstab_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("etc")).unwrap();
        let boot = base.join("boot");

        // Sem fstab: não dá pra afirmar nada → false (mantém caminho atual).
        assert!(!fstab_declares_vfat_boot(&base, &boot));

        // /boot declarado vfat → true (comentários e outras linhas ignorados).
        std::fs::write(
            base.join("etc/fstab"),
            "# /etc/fstab\nUUID=aaa\t/\tbtrfs\tsubvol=/@\t0 0\nUUID=bbb  /boot  vfat  defaults  0 2\n",
        )
        .unwrap();
        assert!(fstab_declares_vfat_boot(&base, &boot));

        // /boot em btrfs (boot dentro do snapshot) → false.
        std::fs::write(base.join("etc/fstab"), "UUID=bbb /boot btrfs defaults 0 0\n").unwrap();
        assert!(!fstab_declares_vfat_boot(&base, &boot));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn backup_remnant_reflects_dir_presence() {
        let base = std::env::temp_dir().join(format!("snapg_test_boot_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        assert!(!boot_backup_remnant(&base));
        std::fs::create_dir_all(base.join(".snapg_boot_backup")).unwrap();
        assert!(boot_backup_remnant(&base));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn parses_limine_kernel_path() {
        let line = "  path: boot():/linux-cachyos/vmlinuz-linux-cachyos#deadbeef";
        assert_eq!(
            limine_boot_path_from_line(Path::new("/boot"), line).as_deref(),
            Some(Path::new("/boot/linux-cachyos/vmlinuz-linux-cachyos"))
        );
    }

    #[test]
    fn parses_limine_module_path() {
        let line = "  module_path: boot():/linux-cachyos/initramfs-linux-cachyos#cafebabe";
        assert_eq!(
            limine_boot_path_from_line(Path::new("/boot"), line).as_deref(),
            Some(Path::new("/boot/linux-cachyos/initramfs-linux-cachyos"))
        );
    }

    #[test]
    fn keeps_non_boot_path_lines_unchanged() {
        let line = "  cmdline: quiet rw rootflags=subvol=/@";
        assert_eq!(limine_boot_path_from_line(Path::new("/boot"), line), None);
    }

    #[test]
    fn classifies_vmlinuz() {
        assert_eq!(
            classify_boot_file("vmlinuz-linux-cachyos"),
            Some(("linux-cachyos", true))
        );
    }

    #[test]
    fn classifies_initramfs_plain() {
        assert_eq!(
            classify_boot_file("initramfs-linux-cachyos"),
            Some(("linux-cachyos", false))
        );
    }

    #[test]
    fn classifies_initramfs_with_img_ext() {
        assert_eq!(
            classify_boot_file("initramfs-linux-cachyos.img"),
            Some(("linux-cachyos", false))
        );
    }

    #[test]
    fn skips_fallback_initramfs() {
        assert_eq!(
            classify_boot_file("initramfs-linux-cachyos-fallback.img"),
            None
        );
    }

    #[test]
    fn ignores_unrelated_files() {
        assert_eq!(classify_boot_file("intel-ucode.img"), None);
        assert_eq!(classify_boot_file("limine.conf"), None);
        assert_eq!(classify_boot_file("limine-splash.png"), None);
    }

    #[test]
    fn scan_bls_dirs_discovers_machine_id_kernels() {
        use super::scan_bls_dirs;
        let base = std::env::temp_dir().join(format!("snapg_test_bls_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let kdir = base.join("0262533c4ca04359a5f379b8e3f83042/linux-cachyos");
        std::fs::create_dir_all(&kdir).unwrap();
        std::fs::write(kdir.join("vmlinuz"), "vmlinuz").unwrap();
        std::fs::write(kdir.join("initramfs"), "initramfs").unwrap();

        // Cria pasta de histórico que deve ser ignorada
        let hdir = base.join("0262533c4ca04359a5f379b8e3f83042/limine_history");
        std::fs::create_dir_all(&hdir).unwrap();
        std::fs::write(hdir.join("vmlinuz"), "vmlinuz").unwrap();

        let mut groups = std::collections::HashMap::new();
        scan_bls_dirs(&base, &mut groups).unwrap();

        assert_eq!(groups.len(), 1);
        let g = groups.get("linux-cachyos").expect("linux-cachyos");
        assert_eq!(g.vmlinuz_paths, vec![kdir.join("vmlinuz")]);
        assert_eq!(g.initramfs_paths, vec![kdir.join("initramfs")]);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scan_flat_boot_dir_only_scans_root_level() {
        use super::scan_flat_boot_dir;
        let base = std::env::temp_dir().join(format!("snapg_test_flat_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("vmlinuz-linux"), "vmlinuz").unwrap();
        std::fs::write(base.join("initramfs-linux.img"), "initramfs").unwrap();

        // Cria pasta Windows com arquivos que não devem ser tocados
        let win_dir = base.join("EFI/Microsoft/Boot");
        std::fs::create_dir_all(&win_dir).unwrap();
        std::fs::write(win_dir.join("bootmgfw.efi"), "win").unwrap();

        let mut groups = std::collections::HashMap::new();
        scan_flat_boot_dir(&base, &mut groups).unwrap();

        assert_eq!(groups.len(), 1);
        let g = groups.get("linux").expect("linux");
        assert_eq!(g.vmlinuz_paths, vec![base.join("vmlinuz-linux")]);
        assert_eq!(g.initramfs_paths, vec![base.join("initramfs-linux.img")]);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scan_uki_dir_discovers_only_linux_ukis() {
        use super::scan_uki_dir;
        let base = std::env::temp_dir().join(format!("snapg_test_uki_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let uki_dir = base.join("EFI/Linux");
        std::fs::create_dir_all(&uki_dir).unwrap();
        std::fs::write(uki_dir.join("0262533c4ca04359a5f379b8e3f83042_linux-cachyos.efi"), "uki").unwrap();
        std::fs::write(uki_dir.join("linux-cachyos-fallback.efi"), "fallback").unwrap();

        let mut groups = std::collections::HashMap::new();
        scan_uki_dir(&base, &mut groups).unwrap();

        assert_eq!(groups.len(), 1);
        let g = groups.get("linux-cachyos").expect("linux-cachyos");
        assert_eq!(g.vmlinuz_paths.len(), 1);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scan_limine_conf_extracts_active_entries_and_skips_windows_and_snapshots() {
        use super::scan_limine_conf;
        let base = std::env::temp_dir().join(format!("snapg_test_limine_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("0262533c4ca04359a5f379b8e3f83042/linux-cachyos")).unwrap();
        std::fs::write(base.join("0262533c4ca04359a5f379b8e3f83042/linux-cachyos/vmlinuz-linux-cachyos"), "vmlinuz").unwrap();
        std::fs::write(base.join("0262533c4ca04359a5f379b8e3f83042/linux-cachyos/initramfs-linux-cachyos"), "initramfs").unwrap();
        std::fs::create_dir_all(base.join("EFI/Microsoft/Boot")).unwrap();
        std::fs::write(base.join("EFI/Microsoft/Boot/bootmgfw.efi"), "win").unwrap();

        let limine_conf_content = "\
/+CachyOS
  //linux-cachyos
  comment: Kernel version: 7.0.1-1-cachyos
  comment: kernel-id=linux-cachyos 
  protocol: linux
  module_path: boot():/0262533c4ca04359a5f379b8e3f83042/linux-cachyos/initramfs-linux-cachyos#cabfa235
  path: boot():/0262533c4ca04359a5f379b8e3f83042/linux-cachyos/vmlinuz-linux-cachyos#daada949
  cmdline: quiet rootflags=subvol=/@

     //Snapshots
     ///287 │ 2026-04-30 20:49:34
     path: boot():/0262533c4ca04359a5f379b8e3f83042/limine_history/vmlinuz_snap#1234

/Windows
  protocol: efi
  path: boot():/EFI/Microsoft/Boot/bootmgfw.efi
";
        std::fs::write(base.join("limine.conf"), limine_conf_content).unwrap();

        let mut groups = std::collections::HashMap::new();
        scan_limine_conf(&base, &mut groups).unwrap();

        assert_eq!(groups.len(), 1);
        let cachy_group = groups.get("linux-cachyos").expect("grupo linux-cachyos deve existir");
        assert_eq!(cachy_group.vmlinuz_paths.len(), 1);
        assert_eq!(cachy_group.initramfs_paths.len(), 1);
        assert!(!groups.contains_key("Windows"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn refresh_limine_boot_hashes_preserves_clean_windows_entry() {
        use super::refresh_limine_boot_hashes;
        let base = std::env::temp_dir().join(format!("snapg_test_limine_hash_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("kernel")).unwrap();
        std::fs::write(base.join("kernel/vmlinuz"), "kernel_bytes").unwrap();
        std::fs::create_dir_all(base.join("EFI/Microsoft/Boot")).unwrap();
        std::fs::write(base.join("EFI/Microsoft/Boot/bootmgfw.efi"), "win_bytes").unwrap();

        let limine_conf_content = "\
/+CachyOS
  protocol: linux
  path: boot():/kernel/vmlinuz

/Windows
  protocol: efi
  path: boot():/EFI/Microsoft/Boot/bootmgfw.efi
";
        std::fs::write(base.join("limine.conf"), limine_conf_content).unwrap();
        refresh_limine_boot_hashes(&base).unwrap();

        let updated = std::fs::read_to_string(base.join("limine.conf")).unwrap();
        // O Linux deve receber hash, mas o Windows NÃO pode receber hash
        assert!(updated.contains("path: boot():/kernel/vmlinuz#"));
        assert!(updated.contains("path: boot():/EFI/Microsoft/Boot/bootmgfw.efi\n") || updated.ends_with("path: boot():/EFI/Microsoft/Boot/bootmgfw.efi"));
        assert!(!updated.contains("bootmgfw.efi#"));

        let _ = std::fs::remove_dir_all(&base);
    }
}
