use crate::btrfs;
use crate::group::{self, Group};
use crate::rollback::{self, RollbackError};
use crate::snapper;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn save(description: Option<String>) -> Result<()> {
    let id = epoch_now()?;
    let desc = description.unwrap_or_else(|| format!("snapg save {id}"));

    let configs = snapper::list_configs()?;
    if configs.is_empty() {
        bail!(
            "nenhuma config snapper encontrada. crie ao menos uma:\n  \
             sudo snapper -c root create-config /\n  \
             sudo snapper -c home create-config /home"
        );
    }

    let mut created = Vec::new();
    for cfg in &configs {
        let n = snapper::create(cfg, &desc, id)
            .with_context(|| format!("criar snapshot em '{cfg}'"))?;
        created.push((cfg.clone(), n));
    }

    println!("✓ grupo {id} criado ({} membros):", created.len());
    for (cfg, n) in &created {
        println!("    {cfg}: #{n}");
    }
    println!("  descrição: {desc}");
    Ok(())
}

pub fn undo(yes: bool) -> Result<()> {
    let g = group::latest_group()?.context("nenhum grupo snapg save encontrado")?;
    print_group("REVERTER", &g);

    if !yes && !confirm("Reverter todos os snapshots do grupo? (s/N) ")? {
        println!("cancelado");
        return Ok(());
    }

    // Pre-flight: assume / e demais mountpoints na mesma fs btrfs.
    // Se o setup tiver fs diferentes, isto precisa virar lookup por config.
    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;

    let result = rollback::rollback_group(&g, &mount_path);

    match result {
        Ok(done) => {
            // Umount best-effort — não impede sucesso se falhar (mount em /run vai sumir no boot).
            let _ = btrfs::umount_toplevel(&mount_path);
            println!("✓ rollback completo do grupo {} ({} membros)", g.id, done.len());
            for d in &done {
                println!("    {}: subvol antigo arquivado como {}", d.config, d.backup_subvol);
            }

            if confirm("Reiniciar agora? (s/N) ")? {
                std::process::Command::new("systemctl")
                    .arg("reboot")
                    .status()?;
                return Ok(());
            }
            println!("⚠ reinicie manualmente para concluir o rollback");
            Ok(())
        }
        Err(rerr) => handle_partial(&g, rerr, &mount_path),
    }
}

fn handle_partial(g: &Group, rerr: RollbackError, mount_path: &Path) -> Result<()> {
    eprintln!();
    eprintln!("⚠ FALHA PARCIAL no rollback do grupo {}", g.id);
    let is_empty = rerr.done.is_empty();
    if is_empty {
        eprintln!("  nenhum membro foi feito (falhou no primeiro)");
    }
    if !is_empty {
        let names: Vec<&str> = rerr.done.iter().map(|d| d.config.as_str()).collect();
        eprintln!("  já feito ({}): {}", rerr.done.len(), names.join(", "));
    }
    eprintln!("  falhou em: {}", rerr.failed_config);
    eprintln!("  erro: {:#}", rerr.error);
    eprintln!();
    eprintln!("Estado atual: nada aplicado ao sistema rodando ainda (rollback é staged).");
    eprintln!("⚠ NÃO REINICIE até decidir.");
    eprintln!();

    if rerr.done.is_empty() {
        let _ = btrfs::umount_toplevel(mount_path);
        return Err(rerr.error);
    }

    let prompt = format!(
        "Reverter os {} membros já feitos automaticamente? (s/N) ",
        rerr.done.len()
    );
    if !confirm(&prompt)? {
        print_manual_recovery(&rerr.done, mount_path);
        let _ = btrfs::umount_toplevel(mount_path);
        return Err(rerr.error);
    }

    if let Err(re) = rollback::revert_partial_undo(&rerr.done, mount_path) {
        eprintln!();
        eprintln!("✗ revert automático falhou no meio: {re:#}");
        eprintln!("  toplevel ainda montado em {}", mount_path.display());
        eprintln!("  resolva manualmente lá e depois: sudo umount {}", mount_path.display());
        return Err(rerr.error);
    }

    let _ = btrfs::umount_toplevel(mount_path);
    println!();
    println!("✓ rollback parcial revertido — sistema voltou ao estado pré-undo");
    Err(rerr.error)
}

fn print_manual_recovery(done: &[rollback::Done], mount_path: &Path) {
    eprintln!();
    eprintln!("Pra reverter manualmente os já feitos (toplevel montado em {}):", mount_path.display());
    for d in done {
        let mp = mount_path.display();
        eprintln!("  # {} (mountpoint {})", d.config, d.mountpoint);
        eprintln!("  sudo mv {mp}/{} {mp}/{}.discard", d.current_subvol, d.current_subvol);
        eprintln!("  sudo mv {mp}/{} {mp}/{}", d.backup_subvol, d.current_subvol);
        eprintln!("  sudo btrfs subvolume delete {mp}/{}.discard", d.current_subvol);
    }
    eprintln!("  sudo umount {}", mount_path.display());
}

pub fn delete(yes: bool) -> Result<()> {
    let g = group::latest_group()?.context("nenhum grupo snapg save encontrado")?;
    print_group("APAGAR", &g);

    if !yes && !confirm("Apagar todos os snapshots do grupo? (s/N) ")? {
        println!("cancelado");
        return Ok(());
    }

    for m in &g.members {
        snapper::delete(&m.config, m.snapshot.number)
            .with_context(|| format!("apagar {} #{}", m.config, m.snapshot.number))?;
    }
    println!("✓ grupo {} apagado ({} membros)", g.id, g.members.len());
    Ok(())
}

/// Tipo de subvol residual encontrado no top-level.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BackupKind {
    /// `<current>_snapg_undo_<label>` — deixado por undo. Usado pelo redo pra
    /// restaurar o estado pré-undo.
    UndoBackup,
    /// `<current>_snapg_discard_<label>` — deixado por redo.
    /// É o subvol vivo *anterior* ao redo, preservado pra cleanup pós-reboot.
    RedoDiscard,
}

impl BackupKind {
    fn label_str(self) -> &'static str {
        match self {
            BackupKind::UndoBackup => "undo-backup",
            BackupKind::RedoDiscard => "redo-discard",
        }
    }
}

/// Estado dos subvols residuais encontrados no top-level.
struct BackupEntry {
    config: String,
    mountpoint: String,
    current_subvol: String,
    backup_subvol: String, // nome no top-level
    label: String,         // ex: "2026-04-26_19:57:24"
    path: PathBuf,
    kind: BackupKind,
}

/// Mapeia config → (mountpoint, current_subvol). Lookup base pra redo/gc.
fn config_subvol_map() -> Result<Vec<(String, String, String)>> {
    let mut out = Vec::new();
    for cfg in snapper::list_configs()? {
        let mp = snapper::config_subvolume(&cfg)?;
        let current = btrfs::subvol_relative_path(Path::new(&mp))
            .with_context(|| format!("descobrir subvol ativo de '{cfg}'"))?;
        out.push((cfg, mp, current));
    }
    Ok(out)
}

/// Tenta casar `name` com algum prefixo conhecido pra `current`.
/// Retorna (kind, label) se casar.
fn match_backup_name(name: &str, current: &str) -> Option<(BackupKind, String)> {
    let undo_prefix = format!("{current}_snapg_undo_");
    if let Some(label) = name.strip_prefix(&undo_prefix) {
        return Some((BackupKind::UndoBackup, label.to_string()));
    }
    let redo_prefix = format!("{current}_snapg_discard_");
    if let Some(label) = name.strip_prefix(&redo_prefix) {
        return Some((BackupKind::RedoDiscard, label.to_string()));
    }
    None
}

/// Varre o top-level e retorna subvols residuais (UndoBackup + RedoDiscard).
fn discover_backups(toplevel: &Path) -> Result<Vec<BackupEntry>> {
    let cfg_map = config_subvol_map()?;
    let mut found = Vec::new();
    for entry in fs::read_dir(toplevel).context("ler toplevel pra descobrir backups")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        for (cfg, mp, current) in &cfg_map {
            if let Some((kind, label)) = match_backup_name(&name, current) {
                found.push(BackupEntry {
                    config: cfg.clone(),
                    mountpoint: mp.clone(),
                    current_subvol: current.clone(),
                    backup_subvol: name.clone(),
                    label,
                    path: entry.path(),
                    kind,
                });
                break;
            }
        }
    }
    Ok(found)
}

pub fn redo(yes: bool) -> Result<()> {
    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;

    let result = redo_inner(yes, &mount_path);
    let _ = btrfs::umount_toplevel(&mount_path);
    result
}

fn redo_inner(yes: bool, mount_path: &Path) -> Result<()> {
    // Redo só considera UndoBackup — RedoDiscard é o subvol vivo prévio,
    // não algo a "restaurar de volta".
    let backups: Vec<BackupEntry> = discover_backups(mount_path)?
        .into_iter()
        .filter(|b| b.kind == BackupKind::UndoBackup)
        .collect();
    if backups.is_empty() {
        bail!("nenhum backup de undo encontrado — nada pra desfazer");
    }

    // Agrupa por label (= timestamp do undo). Lex sort funciona pelo formato ISO.
    let mut by_label: HashMap<String, Vec<BackupEntry>> = HashMap::new();
    for b in backups {
        by_label.entry(b.label.clone()).or_default().push(b);
    }
    let latest = by_label.keys().max().expect("by_label não vazio").clone();
    let mut group = by_label.remove(&latest).unwrap();
    group.sort_by(|a, b| a.config.cmp(&b.config));

    println!("== REDO último undo [{latest}] ({} membros) ==", group.len());
    for b in &group {
        println!("  {}: {} → restaurar como {}", b.config, b.backup_subvol, b.current_subvol);
    }

    if !yes && !confirm("Desfazer último undo (restaurar esses backups)? (s/N) ")? {
        println!("cancelado");
        return Ok(());
    }

    let done: Vec<rollback::Done> = group
        .into_iter()
        .map(|b| rollback::Done {
            config: b.config,
            mountpoint: b.mountpoint,
            current_subvol: b.current_subvol,
            backup_subvol: b.backup_subvol,
        })
        .collect();

    rollback::revert_for_redo(&done, mount_path, &latest).context("revert_for_redo")?;

    println!("✓ redo aplicado — sistema voltou ao estado pré-undo ({latest})");
    println!("  subvols antigos preservados como `<subvol>_snapg_discard_{latest}`");

    // Arma o serviço fantasma no rootfs RESTAURADO (o que vai bootar).
    // O sistema atual virou "discard" e seu /etc/systemd não será lido no próximo boot.
    let root_member = done.iter().find(|d| d.mountpoint == "/");
    if let Some(rm) = root_member {
        let restored_root_path = mount_path.join(&rm.current_subvol);
        match arm_boot_cleanup(&restored_root_path) {
            Ok(()) => println!("  cleanup automático armado para o próximo boot"),
            Err(e) => eprintln!("⚠ não consegui armar cleanup automático: {e:#}\n  use `snapg gc` manualmente após reboot"),
        }
    }
    if root_member.is_none() {
        eprintln!("⚠ grupo não inclui a raiz ('/'), não é possível armar o cleanup automático");
    }

    if confirm("Reiniciar agora? (s/N) ")? {
        std::process::Command::new("systemctl")
            .arg("reboot")
            .status()?;
        return Ok(());
    }
    println!("⚠ reinicie manualmente para concluir o redo");
    Ok(())
}

const BOOT_CLEANUP_UNIT: &str = "snapg-cleanup.service";

fn arm_boot_cleanup(root_fs: &Path) -> Result<()> {
    let root_arg = format!("--root={}", root_fs.display());
    let out = std::process::Command::new("systemctl")
        .args([&root_arg, "enable", BOOT_CLEANUP_UNIT])
        .output()
        .context("invocar systemctl enable")?;
    if !out.status.success() {
        bail!(
            "systemctl enable {BOOT_CLEANUP_UNIT}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn disarm_boot_cleanup() -> Result<()> {
    let out = std::process::Command::new("systemctl")
        .args(["disable", BOOT_CLEANUP_UNIT])
        .output()
        .context("invocar systemctl disable")?;
    if !out.status.success() {
        bail!(
            "systemctl disable {BOOT_CLEANUP_UNIT}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Subcomando interno chamado pelo `snapg-cleanup.service` no boot.
/// Apaga todos os redo-discards no top-level e desarma o serviço.
/// Output vai pro journal (stdout/stderr capturados pelo systemd).
pub fn boot_clean() -> Result<()> {
    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;

    let result = (|| -> Result<()> {
        let discards: Vec<BackupEntry> = discover_backups(&mount_path)?
            .into_iter()
            .filter(|b| b.kind == BackupKind::RedoDiscard)
            .collect();

        if discards.is_empty() {
            println!("snapg boot-clean: nenhum redo-discard encontrado");
            return Ok(());
        }

        let total = discards.len();
        let mut ok = 0usize;
        for b in &discards {
            match btrfs::delete_subvolume(&b.path) {
                Ok(()) => {
                    println!("snapg boot-clean: removido {}", b.backup_subvol);
                    ok += 1;
                }
                Err(e) => eprintln!("snapg boot-clean: falha em {}: {e:#}", b.backup_subvol),
            }
        }
        println!("snapg boot-clean: {ok}/{total} discards removidos");
        Ok(())
    })();

    let _ = btrfs::umount_toplevel(&mount_path);
    result?;

    // Desarma o serviço — independente de ter discards ou não, a missão
    // (rodar uma vez pós-redo) acabou. Erro aqui não é fatal: log e segue.
    if let Err(e) = disarm_boot_cleanup() {
        eprintln!("snapg boot-clean: falha ao desarmar serviço: {e:#}");
    }
    Ok(())
}

pub fn gc(yes: bool) -> Result<()> {
    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;

    let result = gc_inner(yes, &mount_path);
    let _ = btrfs::umount_toplevel(&mount_path);
    result
}

fn gc_inner(yes: bool, mount_path: &Path) -> Result<()> {
    let mut backups = discover_backups(mount_path)?;
    if backups.is_empty() {
        println!("nenhum subvol residual pra coletar");
        return Ok(());
    }
    // Mais antigos primeiro pra leitura humana; UndoBackup antes de RedoDiscard
    // dentro do mesmo label (ordem cronológica de criação).
    backups.sort_by(|a, b| {
        a.label
            .cmp(&b.label)
            .then((a.kind as u8).cmp(&(b.kind as u8)))
            .then(a.config.cmp(&b.config))
    });

    let undo_count = backups
        .iter()
        .filter(|b| b.kind == BackupKind::UndoBackup)
        .count();
    let redo_count = backups.len() - undo_count;
    println!("Subvols residuais encontrados ({}): {undo_count} undo-backup, {redo_count} redo-discard", backups.len());
    for b in &backups {
        println!("  [{}] {} ({}) [{}]", b.label, b.backup_subvol, b.config, b.kind.label_str());
    }
    println!();
    println!("⚠ apagar undo-backup invalida `snapg redo` para esse ponto no tempo.");
    println!("  redo-discard é seguro de apagar a qualquer momento pós-reboot.");

    if !yes && !confirm("Apagar TODOS os subvols listados? (s/N) ")? {
        println!("cancelado");
        return Ok(());
    }

    let mut errors = 0usize;
    for b in &backups {
        match btrfs::delete_subvolume(&b.path) {
            Ok(()) => println!("✓ removido {}", b.backup_subvol),
            Err(e) => {
                eprintln!("✗ {}: {e:#}", b.backup_subvol);
                errors += 1;
            }
        }
    }
    if errors > 0 {
        bail!("{errors} backup(s) não puderam ser deletados");
    }
    Ok(())
}

pub fn list() -> Result<()> {
    let groups = group::list_groups()?;
    if groups.is_empty() {
        println!("nenhum grupo snapg save encontrado");
        return Ok(());
    }
    for g in &groups {
        let date = g
            .members
            .first()
            .map(|m| m.snapshot.date.as_str())
            .unwrap_or("");
        println!("[{}]  {} membros  {}", g.id, g.members.len(), date);
        for m in &g.members {
            println!("    {}: #{}  {}", m.config, m.snapshot.number, m.snapshot.description);
        }
    }
    Ok(())
}

fn print_group(action: &str, g: &Group) {
    println!("== {action} grupo {} ({} membros) ==", g.id, g.members.len());
    for m in &g.members {
        println!("  {}: #{}  {}  {}", m.config, m.snapshot.number, m.snapshot.date, m.snapshot.description);
    }
}

fn epoch_now() -> Result<i64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("epoch agora")?
        .as_secs() as i64)
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    Ok(matches!(
        buf.trim().to_lowercase().as_str(),
        "s" | "sim" | "y" | "yes"
    ))
}
