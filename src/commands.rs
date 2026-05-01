use crate::boot;
use crate::btrfs;
use crate::group::{self, Group, GroupId};
use crate::rollback::{self, RollbackError};
use crate::snapper;
use anyhow::{Context, Result, bail};
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Trunca texto pra caber na largura do terminal.
/// Previne wrapping que causa bug visual no dialoguer (linhas "comendo" o conteúdo acima).
fn truncate_for_terminal(text: &str, prefix_len: usize) -> String {
    let width = console::Term::stdout().size().1 as usize;
    let max = width.saturating_sub(prefix_len);
    if text.chars().count() <= max {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{truncated}…")
}

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

    // Highlander: save mata regret existente.
    // btrfs subvolume delete é quase instantâneo (marca pra GC assíncrono do kernel).
    kill_regrets(&configs)?;

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

/// Monta toplevel, varre regrets existentes e deleta. Idempotente.
fn kill_regrets(configs: &[String]) -> Result<()> {
    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;

    let result = rollback::delete_existing_regrets(&mount_path, configs);
    let _ = btrfs::umount_toplevel(&mount_path);
    result
}

pub fn restore() -> Result<()> {
    let configs = snapper::list_configs()?;
    if configs.is_empty() {
        bail!("nenhuma config snapper encontrada");
    }

    let groups = group::list_groups()?;

    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;

    let result = restore_inner(&groups, &configs, &mount_path);
    let _ = btrfs::umount_toplevel(&mount_path);
    result
}

/// Entrada de regret descoberta no toplevel.
struct RegretEntry {
    config: String,
    mountpoint: String,
    current_subvol: String,
    regret_subvol: String,
}

/// Regret ativo com data de criação (metadata BTRFS).
struct RegretInfo {
    entries: Vec<RegretEntry>,
    creation_time: String,
}

/// Descobre regrets existentes no toplevel.
fn discover_regrets(toplevel: &Path, configs: &[String]) -> Result<Option<RegretInfo>> {
    let mut entries = Vec::new();
    for cfg in configs {
        let mp = snapper::config_subvolume(cfg)?;
        let current = btrfs::subvol_relative_path(std::path::Path::new(&mp))
            .with_context(|| format!("descobrir subvol ativo de '{cfg}'"))?;
        let rname = rollback::regret_name(&current);
        let regret_path = toplevel.join(&rname);
        if !regret_path.exists() {
            continue;
        }
        entries.push(RegretEntry {
            config: cfg.clone(),
            mountpoint: mp,
            current_subvol: current,
            regret_subvol: rname,
        });
    }
    if entries.is_empty() {
        return Ok(None);
    }
    // Creation time do primeiro regret (todos criados no mesmo instante).
    let first_path = toplevel.join(&entries[0].regret_subvol);
    let creation_time = btrfs::subvol_creation_time(&first_path)
        .unwrap_or_else(|_| String::from("data desconhecida"));
    Ok(Some(RegretInfo {
        entries,
        creation_time,
    }))
}

/// Ação selecionada na TUI.
enum RestoreAction {
    Checkpoint(GroupId),
    Regret,
}

fn restore_inner(groups: &[Group], configs: &[String], mount_path: &Path) -> Result<()> {
    let regret = discover_regrets(mount_path, configs)?;
    let has_regret = regret.is_some();

    if groups.is_empty() && !has_regret {
        println!("nenhum checkpoint ou regret encontrado — nada pra restaurar");
        return Ok(());
    }

    // Monta lista de opções pra TUI.
    let mut items: Vec<String> = Vec::new();
    let mut actions: Vec<RestoreAction> = Vec::new();

    // Select prefix: "> " = 2 chars
    let prefix_len = 4;

    if let Some(ref r) = regret {
        let text = format!(
            "⟲ Estado Anterior à Restauração (Regret) — {}",
            r.creation_time
        );
        items.push(truncate_for_terminal(&text, prefix_len));
        actions.push(RestoreAction::Regret);
    }

    for g in groups {
        let date = g
            .members
            .first()
            .map(|m| m.snapshot.date.as_str())
            .unwrap_or("");
        let desc = g
            .members
            .first()
            .map(|m| m.snapshot.description.as_str())
            .unwrap_or("");
        let text = format!(
            "Checkpoint {} ({} — {} membros) {}",
            g.id,
            date,
            g.members.len(),
            desc
        );
        items.push(truncate_for_terminal(&text, prefix_len));
        actions.push(RestoreAction::Checkpoint(g.id));
    }

    let selection = dialoguer::Select::new()
        .with_prompt("Selecione o ponto de restauração")
        .items(&items)
        .default(0)
        .interact()
        .context("seleção cancelada")?;

    match &actions[selection] {
        RestoreAction::Checkpoint(group_id) => {
            let group = groups.iter().find(|g| g.id == *group_id).unwrap();
            execute_restore_checkpoint(group, configs, mount_path)
        }
        RestoreAction::Regret => {
            execute_restore_regret(regret.unwrap(), mount_path)
        }
    }
}

/// True se /boot está montado em FAT32 (vfat). Isso significa que o kernel
/// vive fora do BTRFS e precisa de sincronização manual no rollback.
fn boot_is_fat32() -> bool {
    boot::is_fat32()
}

/// Emite warning fatal: /boot em FAT32 pode dessincronizar kernel ↔ módulos.
/// Retorna false se o utilizador cancelar.
fn warn_fat32_boot() -> Result<bool> {
    if !boot_is_fat32() {
        return Ok(true);
    }
    eprintln!();
    eprintln!("⚠ ATENÇÃO: /boot está em FAT32 (vfat)");
    eprintln!("  O rollback reverte o BTRFS (módulos do kernel), mas o kernel");
    eprintln!("  em /boot (FAT32) NÃO será revertido automaticamente.");
    eprintln!("  Se o kernel mudou entre o snapshot e o estado atual,");
    eprintln!("  o sistema pode não arrancar (kernel panic por mismatch).");
    eprintln!();
    confirm("Continuar mesmo assim? (s/N) ")
}

fn execute_restore_checkpoint(
    group: &Group,
    configs: &[String],
    mount_path: &Path,
) -> Result<()> {
    print_group("RESTAURAR", group);

    if !warn_fat32_boot()? {
        println!("cancelado (risco de dessincronização de boot)");
        return Ok(());
    }

    if !confirm("Restaurar este checkpoint? (s/N) ")? {
        println!("cancelado");
        return Ok(());
    }

    // Highlander: deleta regret existente antes de criar o novo.
    rollback::delete_existing_regrets(mount_path, configs)?;

    match rollback::rollback_group(group, mount_path) {
        Ok(done) => {
            println!(
                "✓ rollback completo do grupo {} ({} membros)",
                group.id,
                done.len()
            );
            for d in &done {
                println!(
                    "    {}: sistema atual arquivado como {}",
                    d.config, d.backup_subvol
                );
            }

            // Sincroniza kernel/initramfs em /boot (FAT32) com o snapshot restaurado.
            if let Some(root) = done.iter().find(|d| d.mountpoint == "/") {
                let restored_root = mount_path.join(&root.current_subvol);
                if let Err(e) = boot::sync_fat32(&restored_root) {
                    eprintln!("⚠ sincronização do boot falhou: {e:#}");
                    eprintln!("  o rollback BTRFS foi aplicado, mas /boot pode estar dessincronizado.");
                    eprintln!("  verifique manualmente antes de reiniciar.");
                }
            }

            prompt_reboot()
        }
        Err(rerr) => handle_partial(group, rerr, mount_path),
    }
}

fn execute_restore_regret(regret: RegretInfo, mount_path: &Path) -> Result<()> {
    println!("== RESTAURAR Regret ({}) ==", regret.creation_time);
    for e in &regret.entries {
        println!(
            "  {}: {} → restaurar como {}",
            e.config, e.regret_subvol, e.current_subvol
        );
    }

    if !confirm("Restaurar o estado anterior (regret)? (s/N) ")? {
        println!("cancelado");
        return Ok(());
    }

    let done: Vec<rollback::Done> = regret
        .entries
        .into_iter()
        .map(|e| rollback::Done {
            config: e.config,
            mountpoint: e.mountpoint,
            current_subvol: e.current_subvol,
            backup_subvol: e.regret_subvol,
        })
        .collect();

    let label = btrfs::now_local_label().context("obter label de tempo")?;
    rollback::revert_regret(&done, mount_path, &label).context("restaurar regret")?;

    println!("✓ regret restaurado — sistema voltou ao estado anterior");
    println!("  subvols atuais preservados como discard (limpos no próximo boot)");

    // Sincroniza kernel/initramfs em /boot (FAT32) com o regret restaurado.
    if let Some(root_member) = done.iter().find(|d| d.mountpoint == "/") {
        let restored_root_path = mount_path.join(&root_member.current_subvol);

        if let Err(e) = boot::sync_fat32(&restored_root_path) {
            eprintln!("⚠ sincronização do boot falhou: {e:#}");
            eprintln!("  verifique manualmente antes de reiniciar.");
        }

        // Arma o cleanup no rootfs RESTAURADO (o que vai bootar).
        match arm_boot_cleanup(&restored_root_path) {
            Ok(()) => println!("  cleanup automático armado para o próximo boot"),
            Err(e) => eprintln!(
                "⚠ não consegui armar cleanup automático: {e:#}\n  \
                 limpe manualmente após reboot: snapg boot-clean"
            ),
        }
    } else {
        eprintln!("⚠ grupo não inclui a raiz ('/'), cleanup automático não armado");
    }

    prompt_reboot()
}

fn handle_partial(g: &Group, rerr: RollbackError, mount_path: &Path) -> Result<()> {
    eprintln!();
    eprintln!("⚠ FALHA PARCIAL no rollback do grupo {}", g.id);
    if rerr.done.is_empty() {
        eprintln!("  nenhum membro foi feito (falhou no primeiro)");
    } else {
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
        return Err(rerr.error);
    }

    let prompt = format!(
        "Reverter os {} membros já feitos automaticamente? (s/N) ",
        rerr.done.len()
    );
    if !confirm(&prompt)? {
        print_manual_recovery(&rerr.done, mount_path);
        return Err(rerr.error);
    }

    if let Err(re) = rollback::revert_partial(&rerr.done, mount_path) {
        eprintln!();
        eprintln!("✗ revert automático falhou no meio: {re:#}");
        eprintln!(
            "  toplevel ainda montado em {}",
            mount_path.display()
        );
        eprintln!(
            "  resolva manualmente lá e depois: sudo umount {}",
            mount_path.display()
        );
        return Err(rerr.error);
    }

    println!();
    println!("✓ rollback parcial revertido — sistema voltou ao estado pré-restore");
    Err(rerr.error)
}

fn print_manual_recovery(done: &[rollback::Done], mount_path: &Path) {
    eprintln!();
    eprintln!(
        "Pra reverter manualmente os já feitos (toplevel montado em {}):",
        mount_path.display()
    );
    for d in done {
        let mp = mount_path.display();
        eprintln!("  # {} (mountpoint {})", d.config, d.mountpoint);
        eprintln!(
            "  sudo mv {mp}/{} {mp}/{}.discard",
            d.current_subvol, d.current_subvol
        );
        eprintln!(
            "  sudo mv {mp}/{} {mp}/{}",
            d.backup_subvol, d.current_subvol
        );
        eprintln!(
            "  sudo btrfs subvolume delete {mp}/{}.discard",
            d.current_subvol
        );
    }
    eprintln!("  sudo umount {}", mount_path.display());
}

pub fn delete(yes: bool) -> Result<()> {
    let groups = group::list_groups()?;
    if groups.is_empty() {
        println!("nenhum grupo snapg save encontrado");
        return Ok(());
    }

    // -y: deleta o mais recente sem TUI (backward compat / scripting).
    if yes {
        let g = &groups[0];
        delete_group(g)?;
        return Ok(());
    }

    // MultiSelect prefix: "> [ ] " = 6 chars
    let prefix_len = 6;
    let mut items: Vec<String> = vec![
        truncate_for_terminal("⚠ TODOS os checkpoints", prefix_len),
    ];
    for g in &groups {
        let date = g
            .members
            .first()
            .map(|m| m.snapshot.date.as_str())
            .unwrap_or("");
        let desc = g
            .members
            .first()
            .map(|m| m.snapshot.description.as_str())
            .unwrap_or("");
        let text = format!(
            "Checkpoint {} ({} — {} membros) {}",
            g.id, date, g.members.len(), desc
        );
        items.push(truncate_for_terminal(&text, prefix_len));
    }

    let selections = dialoguer::MultiSelect::new()
        .with_prompt("Selecione checkpoints para apagar (Espaço=marcar, Enter=confirmar)")
        .items(&items)
        .interact()
        .context("seleção cancelada")?;

    if selections.is_empty() {
        println!("nenhum checkpoint selecionado");
        return Ok(());
    }

    let select_all = selections.contains(&0);
    let targets: Vec<&Group> = if select_all {
        groups.iter().collect()
    } else {
        selections.iter()
            .filter(|&&i| i > 0)
            .map(|&i| &groups[i - 1])
            .collect()
    };

    println!("== APAGAR {} checkpoint(s) ==", targets.len());
    for g in &targets {
        println!("  Checkpoint {} ({} membros)", g.id, g.members.len());
    }

    if !confirm("Confirmar exclusão? (s/N) ")? {
        println!("cancelado");
        return Ok(());
    }

    for g in &targets {
        delete_group(g)?;
    }
    Ok(())
}

fn delete_group(g: &Group) -> Result<()> {
    for m in &g.members {
        snapper::delete(&m.config, m.snapshot.number)
            .with_context(|| format!("apagar {} #{}", m.config, m.snapshot.number))?;
    }
    println!("✓ grupo {} apagado ({} membros)", g.id, g.members.len());
    Ok(())
}

pub fn list() -> Result<()> {
    let groups = group::list_groups()?;
    if groups.is_empty() {
        println!("nenhum grupo snapg save encontrado");
    } else {
        for g in &groups {
            let date = g
                .members
                .first()
                .map(|m| m.snapshot.date.as_str())
                .unwrap_or("");
            println!("[{}]  {} membros  {}", g.id, g.members.len(), date);
            for m in &g.members {
                println!(
                    "    {}: #{}  {}",
                    m.config, m.snapshot.number, m.snapshot.description
                );
            }
        }
    }

    // Mostra regret ativo, se existir.
    show_regret_status()?;
    Ok(())
}

/// Monta toplevel, verifica se há regret ativo e exibe info.
fn show_regret_status() -> Result<()> {
    let configs = snapper::list_configs()?;
    if configs.is_empty() {
        return Ok(());
    }

    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;

    let result = (|| -> Result<()> {
        let regret = discover_regrets(&mount_path, &configs)?;
        if let Some(r) = regret {
            println!();
            println!(
                "⚠ Regret ativo ({}) — use 'snapg restore' para restaurar",
                r.creation_time
            );
        }
        Ok(())
    })();

    let _ = btrfs::umount_toplevel(&mount_path);
    result
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
/// Apaga todos os discards no top-level e desarma o serviço.
/// Output vai pro journal (stdout/stderr capturados pelo systemd).
pub fn boot_clean() -> Result<()> {
    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;

    let result = boot_clean_inner(&mount_path);
    let _ = btrfs::umount_toplevel(&mount_path);
    result?;

    // Desarma o serviço — independente de ter discards ou não.
    if let Err(e) = disarm_boot_cleanup() {
        eprintln!("snapg boot-clean: falha ao desarmar serviço: {e:#}");
    }
    Ok(())
}

fn boot_clean_inner(mount_path: &Path) -> Result<()> {
    let discards = discover_discards(mount_path)?;
    if discards.is_empty() {
        println!("snapg boot-clean: nenhum discard encontrado");
        return Ok(());
    }

    let total = discards.len();
    let mut ok = 0usize;
    for (name, path) in &discards {
        match btrfs::delete_subvolume(path) {
            Ok(()) => {
                println!("snapg boot-clean: removido {name}");
                ok += 1;
            }
            Err(e) => eprintln!("snapg boot-clean: falha em {name}: {e:#}"),
        }
    }
    println!("snapg boot-clean: {ok}/{total} discards removidos");
    Ok(())
}

/// Descobre subvols `_snapg_discard_*` no toplevel (deixados por revert_regret).
fn discover_discards(toplevel: &Path) -> Result<Vec<(String, std::path::PathBuf)>> {
    let cfg_map = config_subvol_map()?;
    let mut found = Vec::new();
    for entry in fs::read_dir(toplevel).context("ler toplevel pra descobrir discards")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        for (_, _, current) in &cfg_map {
            let prefix = format!("{current}_snapg_discard_");
            if name.starts_with(&prefix) {
                found.push((name, entry.path()));
                break;
            }
        }
    }
    Ok(found)
}

/// Mapeia config → (mountpoint, current_subvol).
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

fn print_group(action: &str, g: &Group) {
    println!(
        "== {action} grupo {} ({} membros) ==",
        g.id,
        g.members.len()
    );
    for m in &g.members {
        println!(
            "  {}: #{}  {}  {}",
            m.config, m.snapshot.number, m.snapshot.date, m.snapshot.description
        );
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

fn prompt_reboot() -> Result<()> {
    if !confirm("Reiniciar agora? (s/N) ")? {
        println!("⚠ reinicie manualmente para concluir a restauração");
        return Ok(());
    }
    // -i ignora inhibitors (ex: sessão GNOME bloqueando reboot).
    // Sem isso, o reboot falha silenciosamente e o utilizador fica
    // rodando no subvolume antigo sem saber.
    std::process::Command::new("systemctl")
        .args(["reboot", "-i"])
        .status()?;
    Ok(())
}
