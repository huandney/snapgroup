use crate::boot;
use crate::btrfs;
use crate::doctor;
use crate::group::{self, Group};
use crate::rollback::{self, RollbackError};
use crate::ui::restore::{RegretEntry, RegretInfo, RestorePlan, select_restore_plan};
use crate::snapper;
use crate::ui::snapshots;
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
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

    // Preflight: aborta antes de tocar estado se alguma config vive em outro FS.
    rollback::ensure_single_filesystem(&configs)?;

    // Highlander: save mata regret existente.
    // btrfs subvolume delete é quase instantâneo (marca pra GC assíncrono do kernel).
    kill_regrets(&configs)?;

    let mut created = Vec::new();
    for cfg in &configs {
        let n = snapper::create(cfg, &desc, id)
            .with_context(|| format!("criar snapshot em '{cfg}'"))?;
        created.push((cfg.clone(), n));
    }

    snapshots::print_save_created(id, &desc, &created);
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

    // Preflight: aborta antes de montar/deletar se alguma config vive em outro FS.
    rollback::ensure_single_filesystem(&configs)?;

    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;

    let result = restore_inner(&groups, &configs, &mount_path);
    let _ = btrfs::umount_toplevel(&mount_path);
    result
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

fn restore_inner(groups: &[Group], configs: &[String], mount_path: &Path) -> Result<()> {
    let regret = discover_regrets(mount_path, configs)?;

    if groups.is_empty() && regret.is_none() {
        crate::ui::restore::print_no_restore_points();
        return Ok(());
    }

    // Wizard interativo no alternate screen; ao sair, executa no terminal normal.
    match select_restore_plan(groups, regret.as_ref())? {
        None => {
            crate::ui::restore::print_cancelled();
            Ok(())
        }
        Some(RestorePlan::Checkpoint(selected)) => {
            execute_restore_checkpoint(&selected, mount_path)
        }
        Some(RestorePlan::Regret(selected)) => execute_restore_regret(selected, mount_path),
    }
}

fn group_includes_root(group: &Group) -> Result<bool> {
    for member in &group.members {
        let mountpoint = snapper::config_subvolume(&member.config)?;
        if mountpoint == "/" {
            return Ok(true);
        }
    }
    Ok(false)
}

/// True se /boot está montado em FAT32 (vfat). Isso significa que kernel e
/// initramfs vivem fora do BTRFS e precisam de sincronização no rollback.
fn boot_is_fat32() -> bool {
    boot::is_fat32()
}

/// Emite warning: /boot em FAT32 é modo legado e menos transacional que BTRFS.
/// Retorna false se o utilizador cancelar.
fn warn_fat32_boot() -> Result<bool> {
    if !boot_is_fat32() {
        return Ok(true);
    }
    crate::ui::restore::confirm_fat32_boot()
}

fn execute_restore_checkpoint(
    group: &Group,
    mount_path: &Path,
) -> Result<()> {
    if group_includes_root(group)? && !warn_fat32_boot()? {
        crate::ui::restore::print_cancelled_boot_risk();
        return Ok(());
    }

    let configs: Vec<String> = group.members.iter().map(|m| m.config.clone()).collect();

    // Move o regret anterior pra aside (não deleta): preserva o botão de
    // arrependimento até o novo rollback commitar. Em falha que volte a um
    // estado limpo, o aside é restaurado; em estado ambíguo, é preservado.
    let asides = rollback::aside_existing_regrets(mount_path, &configs)?;

    match rollback::rollback_group(group, mount_path) {
        Ok(done) => {
            // Rollback commitou: o regret anterior virou obsoleto. Best-effort.
            rollback::delete_asides(&asides, mount_path);
            crate::ui::restore::print_checkpoint_rollback_done(group.id, &done);

            // Sincroniza kernel/initramfs em /boot (FAT32) com o snapshot restaurado.
            // Em FAT32, falha de sync = /boot descasado do root → reboot bricka o
            // sistema. Bloqueia o reboot em vez de só avisar.
            if let Some(root) = done.iter().find(|d| d.mountpoint == "/") {
                let restored_root = mount_path.join(&root.current_subvol);
                if let Err(e) = boot::sync_fat32(&restored_root) {
                    return abort_reboot_boot_desync(
                        &restored_root,
                        e,
                        "rode 'snapg restore' e selecione o Regret \
                         (⟲ Estado Anterior à Restauração) para voltar ao \
                         sistema bootável atual antes de qualquer reboot.",
                    );
                }
            }

            prompt_reboot()
        }
        Err(rerr) => {
            match handle_partial(group, rerr, mount_path)? {
                // Estado limpo conhecido: slots de regret canônicos livres.
                PartialOutcome::Clean => {
                    rollback::restore_asides(&asides, mount_path)
                        .context("restaurar regret anterior após reversão limpa")?;
                    if !asides.is_empty() {
                        crate::ui::restore::print_previous_regret_restored();
                    }
                }
                // Estado ambíguo: preserva o aside e instrui recuperação manual.
                PartialOutcome::Indeterminate => {
                    crate::ui::restore::print_preserved_asides(&asides, mount_path);
                }
            }
            bail!("rollback do grupo {} não concluído", group.id)
        }
    }
}

fn execute_restore_regret(regret: RegretInfo, mount_path: &Path) -> Result<()> {
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

    crate::ui::restore::print_regret_restore_done(done.len());

    // Sincroniza kernel/initramfs em /boot (FAT32) com o regret restaurado.
    if let Some(root_member) = done.iter().find(|d| d.mountpoint == "/") {
        let restored_root_path = mount_path.join(&root_member.current_subvol);

        if let Err(e) = boot::sync_fat32(&restored_root_path) {
            return abort_reboot_boot_desync(
                &restored_root_path,
                e,
                "verifique /boot manualmente (vmlinuz/initramfs vs \
                 /usr/lib/modules do root) antes de reiniciar; não reinicie \
                 enquanto não corresponderem.",
            );
        }

        // Arma o cleanup no rootfs RESTAURADO (o que vai bootar).
        match arm_boot_cleanup(&restored_root_path) {
            Ok(()) => crate::ui::restore::print_cleanup_armed(),
            Err(e) => crate::ui::restore::print_cleanup_arm_failed(&e),
        }
    } else {
        match arm_boot_cleanup(Path::new("/")) {
            Ok(()) => crate::ui::restore::print_cleanup_armed(),
            Err(e) => crate::ui::restore::print_cleanup_arm_failed(&e),
        }
    }

    prompt_reboot()
}

/// Veredito do estado do sistema vivo após uma falha de rollback. Decide se o
/// regret anterior (aside) pode voltar automaticamente ao nome canônico.
enum PartialOutcome {
    /// Estado limpo conhecido: slots `_snapg_regret` canônicos livres.
    /// Fase 1 falhou (nada tocado), ou `revert_partial` concluiu.
    Clean,
    /// Estado ambíguo: recuperação manual escolhida, ou `revert_partial`
    /// falhou no meio. O slot canônico pode estar ocupado — não mexer.
    Indeterminate,
}

fn handle_partial(g: &Group, rerr: RollbackError, mount_path: &Path) -> Result<PartialOutcome> {
    crate::ui::restore::print_partial_failure(g.id, &rerr);

    // Fase 1 falhou: sistema vivo 100% intocado, nenhum regret novo criado.
    if rerr.done.is_empty() {
        return Ok(PartialOutcome::Clean);
    }

    if !crate::ui::restore::confirm_revert_partial(rerr.done.len())? {
        crate::ui::restore::print_manual_recovery(&rerr.done, mount_path);
        return Ok(PartialOutcome::Indeterminate);
    }

    if let Err(re) = rollback::revert_partial(&rerr.done, mount_path) {
        crate::ui::restore::print_auto_revert_failed(&re, mount_path);
        return Ok(PartialOutcome::Indeterminate);
    }

    crate::ui::restore::print_partial_reverted();
    Ok(PartialOutcome::Clean)
}

pub fn delete(yes: bool) -> Result<()> {
    let groups = group::list_groups()?;
    if groups.is_empty() {
        snapshots::print_no_groups();
        return Ok(());
    }

    // -y: deleta o mais recente sem TUI (backward compat / scripting).
    if yes {
        let g = &groups[0];
        delete_group(g)?;
        return Ok(());
    }

    // Wizard interativo no alternate screen; a exclusão roda fora, no normal.
    let Some(target_indices) = snapshots::select_delete_plan(&groups)? else {
        snapshots::print_delete_cancelled();
        return Ok(());
    };

    for i in target_indices {
        delete_group(&groups[i])?;
    }
    Ok(())
}

fn delete_group(g: &Group) -> Result<()> {
    for m in &g.members {
        snapper::delete(&m.config, m.snapshot.number)
            .with_context(|| format!("apagar {} #{}", m.config, m.snapshot.number))?;
    }
    snapshots::print_delete_done(g);
    Ok(())
}

pub fn list() -> Result<()> {
    let groups = group::list_groups()?;
    if groups.is_empty() {
        snapshots::print_no_groups();
    } else {
        snapshots::print_groups(&groups)?;
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
            snapshots::print_regret_status(&r.creation_time);
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
        crate::ui::boot_clean::print_disarm_failed(&e);
    }
    Ok(())
}

fn boot_clean_inner(mount_path: &Path) -> Result<()> {
    let discards = discover_discards(mount_path)?;
    if discards.is_empty() {
        crate::ui::boot_clean::print_no_discards();
        return Ok(());
    }

    let total = discards.len();
    let mut ok = 0usize;
    for (name, path) in &discards {
        match btrfs::delete_subvolume(path) {
            Ok(()) => {
                crate::ui::boot_clean::print_discard_removed(name);
                ok += 1;
            }
            Err(e) => crate::ui::boot_clean::print_discard_remove_failed(name, &e),
        }
    }
    crate::ui::boot_clean::print_discard_summary(ok, total);
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

fn epoch_now() -> Result<i64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("epoch agora")?
        .as_secs() as i64)
}

/// /boot (FAT32) ficou dessincronizado do root restaurado. Bootar agora cai
/// em emergency mode (kernel não acha seus módulos). Bloqueia o reboot e
/// instrui a recuperação específica do caminho que falhou.
fn abort_reboot_boot_desync(restored_root: &Path, e: anyhow::Error, recovery: &str) -> Result<()> {
    doctor::handle_boot_sync_failure(restored_root, Path::new("/boot"), e, recovery)
}

fn prompt_reboot() -> Result<()> {
    if !crate::ui::restore::confirm_reboot_now()? {
        crate::ui::restore::print_manual_reboot();
        return Ok(());
    }
    // -i ignora inhibitors (ex: sessão GNOME bloqueando reboot).
    // Sem isso, o reboot falha silenciosamente e o utilizador fica
    // rodando no subvolume antigo sem saber.
    let status = std::process::Command::new("systemctl")
        .args(["reboot", "-i"])
        .status()
        .context("invocar systemctl reboot -i")?;
    if !status.success() {
        bail!("systemctl reboot -i falhou com status {status}");
    }
    Ok(())
}
