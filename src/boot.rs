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
pub fn sync_fat32(restored_root: &Path) -> Result<()> {
    sync_fat32_paths(restored_root, Path::new("/boot"))
}

pub fn sync_fat32_paths(restored_root: &Path, boot: &Path) -> Result<()> {
    if !is_fat32_path(boot) {
        return Ok(());
    }

    let groups = discover_kernel_groups(boot)?;
    if groups.is_empty() {
        bail!("nenhum vmlinuz/initramfs ativo encontrado em {}", boot.display());
    }

    // Gate: se o /boot já casa com o snapshot (restore de mesmo kernel), não há
    // o que sincronizar. Pula backup (~130MB), cópia e mkinitcpio — e a janela
    // de interrupção junto. O caminho pesado só roda quando o kernel difere.
    if boot_matches_snapshot(restored_root, &groups)? {
        println!("  boot sync: /boot já corresponde ao snapshot, nada a fazer");
        return Ok(());
    }

    let critical = critical_boot_files(boot, &groups);
    backup_boot_files(boot, &critical)?;

    let result = sync_inner(restored_root, boot, &groups)
        .and_then(|()| verify_synced(restored_root, &groups));
    if let Err(e) = result {
        eprintln!("  boot sync: falhou, restaurando backup de /boot");
        if let Err(re) = restore_backup_path(boot) {
            eprintln!("  boot sync: restauração do backup falhou: {re:#}");
        }
        return Err(e);
    }

    let _ = fs::remove_dir_all(boot_backup_dir(boot));
    println!("  boot sync: kernel, initramfs e limine.conf sincronizados");
    Ok(())
}

fn sync_inner(restored_root: &Path, boot: &Path, groups: &[KernelGroup]) -> Result<()> {
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
            fs::copy(&snap_vmlinuz, dest).with_context(|| {
                format!(
                    "copiar vmlinuz {} → {}",
                    snap_vmlinuz.display(),
                    dest.display()
                )
            })?;
            println!("  boot sync: vmlinuz ({kver}) → {}", dest.display());
        }

        for dest in &group.initramfs_paths {
            regen_initramfs(&config, kver, restored_root, dest)?;
            println!(
                "  boot sync: initramfs regenerado ({kver}) → {}",
                dest.display()
            );
        }
    }

    refresh_limine_boot_hashes(boot).context("atualizar hashes do limine.conf")?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootHealth {
    NativeBoot,
    Synced,
    NeedsSync,
}

#[derive(Debug, Clone)]
pub struct BootDiagnosis {
    pub fstype: String,
    pub kernel_groups: usize,
    pub initramfs_files: usize,
    pub health: BootHealth,
}

pub fn diagnose_boot(root: &Path, boot: &Path) -> Result<BootDiagnosis> {
    let fstype = boot_fstype(boot)?;
    if !fstype.eq_ignore_ascii_case("vfat") {
        if !fstype.eq_ignore_ascii_case("btrfs") {
            bail!(
                "{} usa filesystem de boot '{fstype}', não suportado pelo doctor",
                boot.display()
            );
        }
        return Ok(BootDiagnosis {
            fstype,
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
    let health = if boot_matches_snapshot(root, &groups)? {
        BootHealth::Synced
    } else {
        BootHealth::NeedsSync
    };
    Ok(BootDiagnosis {
        fstype,
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

/// Verificação pós-sync: se o /boot não casa com o snapshot depois de copiar
/// kernel e regenerar initramfs, o sync não surtiu efeito (parcial/interrompido).
/// Bootar nesse estado cai em emergency mode (kernel não acha seus módulos, ex:
/// "unknown filesystem type vfat"), então vira erro explícito em vez de um
/// reboot silencioso pra um sistema que não sobe.
fn verify_synced(restored_root: &Path, groups: &[KernelGroup]) -> Result<()> {
    if boot_matches_snapshot(restored_root, groups)? {
        return Ok(());
    }
    bail!("/boot dessincronizado: não corresponde ao snapshot após o sync");
}

fn regen_initramfs(config: &Path, kver: &str, restored_root: &Path, out: &Path) -> Result<()> {
    // Gera para um temporário no mesmo diretório e renomeia atomicamente.
    // Escrever direto em `out` deixaria um initramfs parcial no caminho ativo
    // se o mkinitcpio fosse interrompido (SIGKILL no reboot, queda de luz) —
    // bootar com initramfs truncado cai em emergency mode. O `.` inicial impede
    // que o temp seja classificado como artefato de boot por `scan_boot_dir`.
    let file_name = out
        .file_name()
        .with_context(|| format!("initramfs sem nome de arquivo: {}", out.display()))?;
    let tmp = out.with_file_name(format!(".{}.snapg_tmp", file_name.to_string_lossy()));

    let res = Command::new("mkinitcpio")
        .args(["--nopost", "-c"])
        .arg(config)
        .args(["-k", kver, "-r"])
        .arg(restored_root)
        .arg("-g")
        .arg(&tmp)
        .output()
        .context("mkinitcpio falhou")?;
    if !res.status.success() {
        let _ = fs::remove_file(&tmp);
        bail!(
            "mkinitcpio {kver} → {}: {}",
            out.display(),
            String::from_utf8_lossy(&res.stderr)
        );
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

/// Varre /boot recursivamente e agrupa vmlinuz-*/initramfs-* por kernel_name
/// extraído do nome do arquivo (padrão Arch: vmlinuz-linux-cachyos,
/// initramfs-linux-cachyos[.img]). Layouts BLS e flat saem agrupados juntos
/// sob o mesmo kernel_name.
fn discover_kernel_groups(boot: &Path) -> Result<Vec<KernelGroup>> {
    let mut by_name: HashMap<String, KernelGroup> = HashMap::new();
    scan_boot_dir(boot, &mut by_name)?;
    let mut groups: Vec<KernelGroup> = by_name.into_values().collect();
    groups.sort_by(|a, b| a.kernel_name.cmp(&b.kernel_name));
    Ok(groups)
}

fn scan_boot_dir(dir: &Path, out: &mut HashMap<String, KernelGroup>) -> Result<()> {
    let walk = fs::read_dir(dir).with_context(|| format!("ler {}", dir.display()))?;
    for entry in walk {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if is_ignored_boot_dir(&name) {
                continue;
            }
            scan_boot_dir(&path, out)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Some((kernel_name, is_vmlinuz)) = classify_boot_file(&name) else {
            continue;
        };
        let group = out
            .entry(kernel_name.to_string())
            .or_insert_with(|| KernelGroup {
                kernel_name: kernel_name.to_string(),
                vmlinuz_paths: Vec::new(),
                initramfs_paths: Vec::new(),
            });
        if is_vmlinuz {
            group.vmlinuz_paths.push(path);
        } else {
            group.initramfs_paths.push(path);
        }
    }
    Ok(())
}

fn is_ignored_boot_dir(name: &str) -> bool {
    matches!(name, "limine_history" | ".snapg_boot_backup")
}

/// Classifica um arquivo em /boot como `(kernel_name, is_vmlinuz)`.
/// - `vmlinuz-linux-cachyos`       → `("linux-cachyos", true)`
/// - `initramfs-linux-cachyos.img` → `("linux-cachyos", false)`
///
/// Fallback initramfs (`initramfs-*-fallback*`) é ignorado: sem preset não
/// dá pra reconstruir um fallback corretamente, e o do backup vai cobrir a
/// recuperação se algo falhar.
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

fn backup_boot_files(boot: &Path, files: &[PathBuf]) -> Result<()> {
    let backup = boot_backup_dir(boot);
    if backup.exists() {
        let _ = fs::remove_dir_all(&backup);
    }
    fs::create_dir_all(&backup).context("criar diretório de backup do boot")?;

    for src in files {
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
    println!("  boot sync: backup em {}", backup.display());
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
    println!("  boot sync: ficheiros de boot restaurados do backup");
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
    for line in content.lines() {
        let refreshed = refresh_limine_hash_for_line(boot, line)?;
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
    println!("  boot sync: hashes BLAKE2B atualizados em /boot/limine.conf");
    Ok(())
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
    use super::{classify_boot_file, limine_boot_path_from_line};
    use std::path::Path;

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
}
