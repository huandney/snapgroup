use crate::boot;
use crate::btrfs;
use crate::config;
use crate::doctor;
use crate::group::{self, Group, Member};
use crate::rollback::{self, RollbackError};
use crate::snapper;
use crate::ui::restore::{
    Fat32BootAction, PendingAction, PendingPromptInfo, PendingSyncAction, PostRestoreAction,
    RegretEntry, RegretInfo, RestorePlan, select_restore_plan,
};
use crate::ui::snapshots;
use crate::ui::term::{clear_screen, confirm};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy)]
enum RestoreRegretPolicy {
    Update,
    PreserveExisting,
}

impl RestoreRegretPolicy {
    fn needs_rescue_root_scan(self) -> bool {
        matches!(self, RestoreRegretPolicy::PreserveExisting)
    }
}

pub fn save(description: Option<String>) -> Result<()> {
    let cfg = config::load();
    let id = epoch_now()?;

    let configs = snapper::list_configs()?;
    if configs.is_empty() {
        bail!(
            "nenhuma config snapper encontrada. crie ao menos uma:\n  \
             sudo snapper -c root create-config /\n  \
             sudo snapper -c home create-config /home"
        );
    }

    // Gate: não criar checkpoints com uma restauração pendente de reboot —
    // resolve antes (concluir ou cancelar). Sem isto, o save prossegue num
    // estado transitório (root vivo = `_snapg_regret`, `.snapshots` já movido
    // pro destino) e só não destrói o Regret por acidente de derivação de nome.
    if gate_pending_restore_if_any(&configs)? {
        return Ok(());
    }

    // Purga de carona: apaga de vez grupos cuja carência na lixeira venceu.
    purge_expired_trash(&cfg);

    let mut mountpoints = Vec::new();
    for cfg in &configs {
        mountpoints.push(snapper::config_subvolume(cfg)?);
    }

    // Sem nome: wizard interativo (membros + kernel como informação, campo de
    // nome com o auto-nome como placeholder — Enter direto o aceita). Sem TTY
    // (script/cron), usa o mesmo auto-nome sem prompt — interatividade
    // quebraria automações que nunca a pediram.
    let auto_name = format!("snapg save {id}");
    let desc = match description {
        Some(d) => d,
        None if crate::ui::term::stdout_is_tty() => {
            let kernel = boot::kernel_label(Path::new("/"));
            let Some(d) = snapshots::prompt_save_name(&mut mountpoints, &kernel, &auto_name)?
            else {
                snapshots::print_save_cancelled();
                return Ok(());
            };
            d
        }
        None => auto_name,
    };

    // Preflight: aborta antes de tocar estado se alguma config vive em outro FS.
    rollback::ensure_single_filesystem(&configs)?;

    // Highlander: save mata regret existente.
    // btrfs subvolume delete é quase instantâneo (marca pra GC assíncrono do kernel).
    kill_regrets(&configs)?;

    for cfg in &configs {
        snapper::create(cfg, &desc, id)
            .with_context(|| format!("criar snapshot em '{cfg}'"))?;
    }

    snapshots::print_save_created(id, &desc, &mut mountpoints);

    // Cleanup group-aware: o grupo recém-criado é o mais novo, logo protegido
    // pelo "manter os N mais novos". Best-effort — o snapshot já está no disco.
    run_cleanup(&cfg);
    Ok(())
}

/// Razão da ida pra lixeira, gravada no userdata pra futura tela `snapg trash`
/// distinguir poda automática de exclusão manual (janelas de carência podem
/// divergir depois).
#[derive(Clone, Copy)]
enum TrashReason {
    Cleanup,
    Manual,
}

impl TrashReason {
    fn as_str(self) -> &'static str {
        match self {
            TrashReason::Cleanup => "cleanup",
            TrashReason::Manual => "manual",
        }
    }
}

/// Marca todos os membros do grupo como lixeira. Best-effort por membro: um
/// membro que falha não impede os outros — o grupo fica meio-marcado, mas
/// `is_trashed` (qualquer membro) já o esconde e o próximo purge completa.
fn trash_group(g: &Group, trash_epoch: i64, reason: TrashReason) -> usize {
    let mut ok = 0;
    for m in &g.members {
        match snapper::trash(&m.config, m.snapshot.number, g.id, trash_epoch, reason.as_str()) {
            Ok(()) => ok += 1,
            Err(e) => snapshots::print_trash_member_failed(&m.config, m.snapshot.number, &e),
        }
    }
    ok
}

fn run_cleanup(cfg: &config::Config) {
    if cfg.keep_groups == 0 {
        return; // ilimitado
    }
    if let Err(e) = try_cleanup(cfg.keep_groups) {
        snapshots::print_cleanup_failed(&e);
    }
}

fn try_cleanup(keep: usize) -> Result<()> {
    let groups = group::list_groups()?; // visíveis, newest-first
    let prune = group::groups_to_prune(&groups, keep);
    if prune.is_empty() {
        return Ok(());
    }
    let trash_epoch = epoch_now()?;
    let mut moved = 0;
    for g in prune {
        if trash_group(g, trash_epoch, TrashReason::Cleanup) > 0 {
            moved += 1;
        }
    }
    if moved > 0 {
        snapshots::print_cleanup_done(moved);
    }
    Ok(())
}

fn purge_expired_trash(cfg: &config::Config) {
    if let Err(e) = try_purge_expired_trash(cfg) {
        snapshots::print_purge_failed(&e);
    }
}

fn try_purge_expired_trash(cfg: &config::Config) -> Result<()> {
    let trashed = group::list_trashed_groups()?;
    if trashed.is_empty() {
        return Ok(());
    }
    // O parser limita trash_prune_days a MAX_PRUNE_DAYS, então o cast e a
    // multiplicação não estouram i64.
    let cutoff = epoch_now()? - cfg.trash_prune_days as i64 * 86_400;
    let mut purged = 0;
    for g in &trashed {
        let Some(marked_at) = group::trash_epoch(g) else {
            continue;
        };
        if marked_at > cutoff {
            continue; // ainda na carência
        }
        match delete_group_members(g) {
            Ok(()) => purged += 1,
            Err(e) => snapshots::print_purge_member_failed(g.id, &e),
        }
    }
    if purged > 0 {
        snapshots::print_purge_done(purged);
    }
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
    restore_with_policy(RestoreRegretPolicy::Update)
}

pub fn restore_preserving_regret() -> Result<()> {
    restore_with_policy(RestoreRegretPolicy::PreserveExisting)
}

fn restore_with_policy(regret_policy: RestoreRegretPolicy) -> Result<()> {
    let configs = snapper::list_configs()?;
    if configs.is_empty() {
        bail!("nenhuma config snapper encontrada");
    }

    // Gate: uma restauração pendente de reboot não convive com um novo restore.
    // Resolve antes — concluir (reboot) ou cancelar — em vez de empilhar.
    if gate_pending_restore_if_any(&configs)? {
        return Ok(());
    }

    // Preflight: aborta antes de montar/deletar se alguma config vive em outro FS.
    rollback::ensure_single_filesystem(&configs)?;

    let mut groups = group::list_groups()?;

    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;

    let result = (|| -> Result<()> {
        if regret_policy.needs_rescue_root_scan() {
            groups = scan_groups_from_toplevel(&mount_path, &configs)?;
        }
        restore_inner(&groups, &configs, &mount_path, regret_policy)
    })();
    let _ = btrfs::umount_toplevel(&mount_path);
    result
}

/// Restaura só o `/` para um snapshot escolhido, preservando `home`/`root_home`.
/// O doctor usa isto para "manter o kernel do `/boot`" ajustando a outra ponta
/// (o `/`). Varre os snapshots de root direto do toplevel (funciona inclusive
/// num boot de resgate) e reverte com o subvol-alvo `@` explícito.
pub fn restore_root_only() -> Result<()> {
    let root_subvol = boot::default_root_subvol()
        .context("não foi possível determinar o subvol de / pelo fstab")?;

    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;

    let result = restore_root_only_inner(&mount_path, &root_subvol);
    let _ = btrfs::umount_toplevel(&mount_path);
    result
}

fn pending_restore_from_live(configs: &[String]) -> Result<Option<Vec<rollback::Done>>> {
    let mut done = Vec::new();

    for cfg in configs {
        let mountpoint = snapper::config_subvolume(cfg)?;
        let live_subvol = btrfs::subvol_relative_path(Path::new(&mountpoint))
            .with_context(|| format!("descobrir subvol ativo de '{cfg}'"))?;
        let Some(current_subvol) = pending_current_subvol(&live_subvol) else {
            continue;
        };

        done.push(rollback::Done {
            config: cfg.clone(),
            mountpoint,
            current_subvol,
            backup_subvol: live_subvol,
        });
    }

    if done.is_empty() {
        return Ok(None);
    }
    Ok(Some(done))
}

pub(crate) fn gate_pending_restore_if_any(configs: &[String]) -> Result<bool> {
    let Some(done) = pending_restore_from_live(configs)? else {
        return Ok(false);
    };
    gate_pending_restore(done)?;
    Ok(true)
}

fn pending_current_subvol(live_subvol: &str) -> Option<String> {
    if let Some(current) = live_subvol.strip_suffix("_snapg_regret") {
        return (!current.is_empty()).then(|| current.to_string());
    }

    if let Some((current, label)) = live_subvol.split_once("_snapg_undo_")
        && !current.is_empty()
        && !label.is_empty()
    {
        return Some(current.to_string());
    }

    let (current, label) = live_subvol.split_once("_snapg_discard_")?;
    (!current.is_empty() && !label.is_empty()).then(|| current.to_string())
}

/// Há uma restauração aplicada mas ainda não reiniciada? Leitura pura, sem lock.
pub fn has_pending_restore() -> Result<bool> {
    let configs = snapper::list_configs()?;
    Ok(pending_restore_from_live(&configs)?.is_some())
}

/// Ponto de entrada do doctor para um pending: sincronizar o /boot com o destino
/// e reiniciar, ou cancelar. O lock global DEVE estar pego pelo caller.
pub fn doctor_resolve_pending() -> Result<()> {
    let configs = snapper::list_configs()?;
    let Some(done) = pending_restore_from_live(&configs)? else {
        return Ok(());
    };
    if pending_boot_synced(&done)? {
        resolve_pending_clean(&done)
    } else {
        resolve_pending_sync(&done)
    }
}

/// Gate de `restore`/`delete` (sob o lock do caller) quando há uma restauração
/// aplicada mas ainda não reiniciada. Sem checkpoints a oferecer:
/// - /boot casa com o destino → reiniciar é seguro: reiniciar / cancelar;
/// - /boot FAT32 ou não comprovadamente seguro → sincronizar antes do reboot.
fn gate_pending_restore(done: Vec<rollback::Done>) -> Result<()> {
    if pending_boot_synced(&done)? {
        resolve_pending_clean(&done)
    } else if crate::ui::restore::confirm_run_doctor()? {
        resolve_pending_sync(&done)
    } else {
        Ok(())
    }
}

/// /boot casa com o destino: reiniciar é seguro. Reiniciar ou cancelar.
fn resolve_pending_clean(done: &[rollback::Done]) -> Result<()> {
    let info = pending_prompt_info(done, false)?;
    match crate::ui::restore::select_pending_action(&info)? {
        PendingAction::Reboot => {
            arm_pending_cleanup(done)?;
            reboot_now()
        }
        PendingAction::Cancel => cancel_pending_restore(done),
        PendingAction::Nothing => Ok(()),
    }
}

/// Arma o boot-cleanup antes de um reboot que resolve um pending cujo subvol
/// deslocado (`_snapg_undo_*`/`_snapg_discard_*`) precisa ser varrido no
/// próximo boot. O arm do fluxo de restore normal não cobre o gate: se a
/// operação original foi interrompida antes de armar, resolver o pending por
/// aqui deixaria o subvol órfão no top-level para sempre.
fn arm_pending_cleanup(done: &[rollback::Done]) -> Result<()> {
    if !done.iter().any(done_needs_boot_cleanup) {
        return Ok(());
    }
    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;
    let _guard = ToplevelMountGuard { path: mount_path.clone() };
    arm_cleanup_for_restore(done, &mount_path)
}

/// Sincronizar o /boot com o destino, ou cancelar. Compartilhado pelo doctor e
/// pelo gate quando o pending precisa provar /boot antes do reboot.
fn resolve_pending_sync(done: &[rollback::Done]) -> Result<()> {
    let info = pending_prompt_info(done, true)?;
    match crate::ui::restore::select_pending_sync_action(&info)? {
        PendingSyncAction::SyncBoot => complete_pending_restore(done),
        PendingSyncAction::Cancel => cancel_pending_restore(done),
        PendingSyncAction::Nothing => Ok(()),
    }
}

/// O /boot já casa com o subvol de destino (o que vai bootar)?
///
/// Em pending com root + FAT32, nunca dá reboot direto: um sync anterior pode ter
/// sido interrompido, ou o /boot pode ter sido alterado por pacman/DKMS antes do
/// reboot. `boot_already_synced` compara vmlinuz/hashes, mas não prova que o
/// initramfs corresponde aos módulos do root de destino em restores de mesmo
/// kernel. Força resync antes de liberar reboot.
///
/// Em /boot não-FAT32 (nativo) é coerente — o kernel vive dentro do snapshot.
fn pending_boot_synced(done: &[rollback::Done]) -> Result<bool> {
    let Some(root) = done.iter().find(|d| d.mountpoint == "/") else {
        return Ok(true);
    };
    if boot::is_fat32() {
        return Ok(false);
    }
    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;
    let dest_root = mount_path.join(&root.current_subvol);
    let result = boot::boot_already_synced(&dest_root);
    let _ = btrfs::umount_toplevel(&mount_path);
    result
}

fn pending_prompt_info(
    done: &[rollback::Done],
    boot_needs_sync: bool,
) -> Result<PendingPromptInfo> {
    let first = done.first().context("pending sem membros")?;
    let root = done.iter().find(|d| d.mountpoint == "/");
    let primary = root.unwrap_or(first);
    let mut destination_kernel = String::from("?");

    if let Some(root) = root {
        let uuid = btrfs::fs_uuid("/")?;
        let mount_path = rollback::toplevel_mount_path(&uuid);
        btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;
        let _guard = ToplevelMountGuard { path: mount_path.clone() };
        destination_kernel = boot::kernel_label(&mount_path.join(&root.current_subvol));
    }

    let current_kernel = root
        .map(|_| boot::kernel_label(Path::new("/")))
        .unwrap_or_else(|| String::from("?"));

    Ok(PendingPromptInfo {
        boot_needs_sync,
        undo_regret: done.iter().any(done_is_regret_undo),
        destination_subvol: primary.current_subvol.clone(),
        destination_kernel,
        current_subvol: primary.backup_subvol.clone(),
        current_kernel,
        regret_subvol: rollback::regret_name(&primary.current_subvol),
    })
}

/// Desmonta o top-level ao sair de escopo, mesmo em erro/early-return.
struct ToplevelMountGuard {
    path: PathBuf,
}

impl Drop for ToplevelMountGuard {
    fn drop(&mut self) {
        let _ = btrfs::umount_toplevel(&self.path);
    }
}

/// Diagnóstico de `/boot` contra o subvol de destino de uma restauração pendente
/// (o que vai bootar), não contra o `*_snapg_regret` vivo. No pending o `/` é o
/// chão antigo renomeado; diagnosticar contra ele acusa um falso "difere", já que
/// o `/boot` está coerente com o destino. `Ok(None)` quando não há pending — o
/// caller cai no diagnóstico normal.
pub(crate) fn pending_dest_diagnosis(boot_path: &Path) -> Result<Option<boot::BootDiagnosis>> {
    let configs = snapper::list_configs()?;
    let Some(done) = pending_restore_from_live(&configs)? else {
        return Ok(None);
    };
    let Some(root) = done.iter().find(|d| d.mountpoint == "/") else {
        return Ok(None);
    };
    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;
    let _guard = ToplevelMountGuard { path: mount_path.clone() };
    let dest_root = mount_path.join(&root.current_subvol);
    Ok(Some(boot::diagnose_boot(&dest_root, boot_path)?))
}

/// Garante o /boot coerente com o subvol que vai bootar (o destino do restore)
/// e então pergunta se reinicia agora.
fn complete_pending_restore(done: &[rollback::Done]) -> Result<()> {
    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;
    let result = (|| -> Result<()> {
        if let Some(root) = done.iter().find(|d| d.mountpoint == "/") {
            let dest_root = mount_path.join(&root.current_subvol);
            boot::sync_fat32_after_restore(&dest_root)
                .context("sincronizar /boot com o destino do restore")?;
        }
        // O fluxo original pode ter sido interrompido antes de armar o
        // cleanup; sem isto, o subvol deslocado sobrevive ao reboot.
        arm_cleanup_for_restore(done, &mount_path)
    })();
    let _ = btrfs::umount_toplevel(&mount_path);
    result?;
    prompt_reboot()
}

fn cancel_pending_restore(done: &[rollback::Done]) -> Result<()> {
    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;
    let result = undo_restore_before_reboot(done, &mount_path);
    let _ = btrfs::umount_toplevel(&mount_path);
    result
}

fn restore_root_only_inner(mount_path: &Path, root_subvol: &str) -> Result<()> {
    let snaps = scan_root_snapshots(mount_path, root_subvol)?;
    if snaps.is_empty() {
        crate::ui::restore::print_no_root_snapshots();
        return Ok(());
    }

    // Deduplica por kernel: o propósito da Opção C é casar o root com o kernel do
    // /boot, e todos os snapshots de um mesmo kernel bootam igual. `snaps` vem
    // ordenado por número desc, então o primeiro de cada kernel é o mais recente.
    // Escolher um ponto-no-tempo específico é assunto do restore completo.
    let mut seen = std::collections::HashSet::new();
    let rows: Vec<crate::ui::restore::RootSnapshotRow> = snaps
        .iter()
        .filter(|s| seen.insert(s.kernel.clone()))
        .map(|s| crate::ui::restore::RootSnapshotRow {
            number: s.number,
            date: s.date.clone(),
            kernel: s.kernel.clone(),
            name: s.name.clone(),
        })
        .collect();

    let current_kernel = boot::running_kernel().unwrap_or_default();
    let Some(number) = crate::ui::restore::select_root_snapshot(&rows, &current_kernel)? else {
        crate::ui::restore::print_cancelled();
        return Ok(());
    };

    if !confirm(&format!(
        "Restaurar o / para o snapshot #{number}? Troca o root que boota (home e /root ficam intactos)."
    ))? {
        crate::ui::restore::print_cancelled();
        return Ok(());
    }

    execute_restore_root_to_snapshot(mount_path, root_subvol, number)
}

/// Descobre o Regret archived (estado anterior a um restore já efetivado por
/// reboot) no toplevel. O pending (restauração não reiniciada) não entra aqui —
/// é tratado pelo gate em `restore`/`delete`, não como Regret restaurável.
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
    Ok(Some(RegretInfo {
        creation_time: regret_creation_time(toplevel, &entries),
        entries,
    }))
}

fn regret_creation_time(toplevel: &Path, entries: &[RegretEntry]) -> String {
    let Some(first) = entries.first() else {
        return String::from("data desconhecida");
    };
    // otime do subvol restaurado (o ativo) = instante do restore: a Fase 1 o
    // cria com create_snapshot. O regret_subvol é o chão antigo renomeado, cujo
    // otime é a data de nascimento daquele subvol (instalação ou restore antigo)
    // — irrelevante pro evento que o usuário associa ao Regret.
    btrfs::subvol_creation_time(&toplevel.join(&first.current_subvol))
        .unwrap_or_else(|_| String::from("data desconhecida"))
}

fn restore_inner(
    groups: &[Group],
    configs: &[String],
    mount_path: &Path,
    regret_policy: RestoreRegretPolicy,
) -> Result<()> {
    let regret = discover_regrets(mount_path, configs)?;

    if groups.is_empty() && regret.is_none() {
        crate::ui::restore::print_no_restore_points();
        return Ok(());
    }

    // Wizard interativo no alternate screen; ao sair, executa no terminal normal.
    let kernel_labels = group::kernel_labels(groups, mount_path);
    let regret_kernel = regret.as_ref().map(|r| regret_kernel_label(r, mount_path));

    loop {
        match select_restore_plan(
            groups,
            regret.as_ref(),
            &kernel_labels,
            regret_kernel.as_deref(),
        )? {
            None => {
                crate::ui::restore::print_cancelled();
                return Ok(());
            }
            Some(RestorePlan::Checkpoint(selected)) => {
                if should_warn_fat32(&selected, mount_path) {
                    match crate::ui::restore::confirm_fat32_boot()? {
                        Fat32BootAction::Continue => {}
                        Fat32BootAction::Back => continue,
                        Fat32BootAction::Cancel => {
                            crate::ui::restore::print_cancelled_boot_risk();
                            return Ok(());
                        }
                    }
                }
                return execute_restore_checkpoint(&selected, mount_path, regret_policy);
            }
            Some(RestorePlan::Regret(selected)) => {
                return execute_restore_regret(selected, mount_path);
            }
        }
    }
}

fn regret_kernel_label(regret: &RegretInfo, toplevel: &Path) -> String {
    let Some(root_e) = regret.entries.iter().find(|e| e.mountpoint == "/") else {
        return "?".to_string();
    };
    boot::kernel_label(&toplevel.join(&root_e.regret_subvol))
}

fn scan_groups_from_toplevel(toplevel: &Path, configs: &[String]) -> Result<Vec<Group>> {
    let mut by_id: HashMap<group::GroupId, Vec<Member>> = HashMap::new();
    for cfg in configs {
        let Some(subvol) = config_snapshot_subvol(cfg)? else {
            continue;
        };
        for member in scan_config_members_from_toplevel(toplevel, &subvol, cfg)? {
            let Some(id) = group::extract_id(&member.snapshot) else {
                continue;
            };
            by_id.entry(id).or_default().push(member);
        }
    }

    let mut groups: Vec<Group> = by_id
        .into_iter()
        .filter_map(|(id, mut members)| {
            if members.len() != configs.len() {
                return None;
            }
            members.sort_by(|a, b| a.config.cmp(&b.config));
            Some(Group { id, members })
        })
        .collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.id));
    Ok(groups)
}

fn config_snapshot_subvol(config: &str) -> Result<Option<String>> {
    let mountpoint = snapper::config_subvolume(config)?;
    if mountpoint == "/" {
        return Ok(boot::default_root_subvol());
    }
    Ok(Some(rollback::base_subvol_of_mountpoint(&mountpoint)?))
}

/// Um snapshot de root varrido direto do toplevel (`@/.snapshots/N/snapshot`),
/// sem snapper — funciona inclusive de dentro de um boot de resgate.
struct RootSnap {
    number: u32,
    kernel: String,
    date: String,
    /// Nome do backup, se foi feito pelo snapgroup (descrição do `info.xml`).
    name: Option<String>,
}

/// Varre os snapshots de root no toplevel, contornando a cegueira do snapper num
/// boot de resgate (`/.snapshots` do snapshot está vazio). Lê kernel e data
/// direto do subvol, e o nome do backup do `info.xml` (só quando é snapgroup).
fn scan_root_snapshots(toplevel: &Path, root_subvol: &str) -> Result<Vec<RootSnap>> {
    let dir = toplevel.join(root_subvol).join(".snapshots");
    let mut snaps = Vec::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("ler {}", dir.display()))? {
        let entry = entry?;
        let Some(number) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let src = entry.path().join("snapshot");
        if !src.join("usr/lib/modules").exists() {
            continue; // não é um snapshot de root válido
        }
        snaps.push(RootSnap {
            number,
            kernel: boot::kernel_label(&src),
            date: btrfs::subvol_creation_time(&src).unwrap_or_default(),
            name: snapgroup_backup_name(&entry.path().join("info.xml")),
        });
    }
    // Mais recente primeiro.
    snaps.sort_by_key(|s| std::cmp::Reverse(s.number));
    Ok(snaps)
}

fn scan_config_members_from_toplevel(
    toplevel: &Path,
    subvol: &str,
    config: &str,
) -> Result<Vec<Member>> {
    let dir = toplevel.join(subvol).join(".snapshots");
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut members = Vec::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("ler {}", dir.display()))? {
        let entry = entry?;
        let Some(number) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let src = entry.path().join("snapshot");
        let Some(snapshot) = snapshot_from_info_xml(&entry.path().join("info.xml"), &src, number)?
        else {
            continue;
        };
        members.push(Member {
            config: config.to_string(),
            snapshot,
        });
    }
    Ok(members)
}

fn snapshot_from_info_xml(
    info_xml: &Path,
    snapshot_path: &Path,
    number: u32,
) -> Result<Option<snapper::Snapshot>> {
    // Fallback de resgate para o mesmo contrato que `snapper::list` entrega no
    // caminho normal. O Snapper fica cego ao root quando `/` é um snapshot
    // Limine, então este parser manual lê só os campos necessários do info.xml.
    let content = fs::read_to_string(info_xml).with_context(|| format!("ler {}", info_xml.display()))?;
    let Some(group_id) = snapgroup_id_from_info_xml(&content) else {
        return Ok(None);
    };
    let mut userdata = serde_json::Map::new();
    userdata.insert(
        "snapgroup-id".to_string(),
        serde_json::Value::String(group_id),
    );
    Ok(Some(snapper::Snapshot {
        number,
        kind: xml_text(&content, "type").unwrap_or_default(),
        date: xml_text(&content, "date")
            .unwrap_or_else(|| btrfs::subvol_creation_time(snapshot_path).unwrap_or_default()),
        user: xml_text(&content, "user").unwrap_or_default(),
        description: xml_text(&content, "description").unwrap_or_default(),
        cleanup: xml_text(&content, "cleanup").unwrap_or_default(),
        userdata: Some(serde_json::Value::Object(userdata)),
    }))
}

/// Nome de um backup feito pelo snapgroup: a `<description>` do `info.xml`, mas
/// só quando o snapshot tem `snapgroup-id` no userdata (senão é timeline/pacman,
/// sem nome útil). Extração mínima de string — sem crate de XML. `None` se não
/// for snapgroup, o arquivo não existir, ou a descrição for vazia.
fn snapgroup_backup_name(info_xml: &Path) -> Option<String> {
    let content = fs::read_to_string(info_xml).ok()?;
    snapgroup_id_from_info_xml(&content)?;
    xml_text(&content, "description").filter(|val| !val.is_empty())
}

fn snapgroup_id_from_info_xml(content: &str) -> Option<String> {
    for block in xml_blocks(content, "userdata") {
        let Some(key) = xml_text(block, "key") else {
            continue;
        };
        if key != "snapgroup-id" {
            continue;
        }
        let value = xml_text(block, "value")?;
        if is_digits(&value) {
            return Some(value);
        }
        return None;
    }

    snapgroup_id_from_inline_userdata(content)
}

fn snapgroup_id_from_inline_userdata(content: &str) -> Option<String> {
    let marker = "snapgroup-id=";
    let start = content.find(marker)? + marker.len();
    let id: String = content[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    is_digits(&id).then_some(id)
}

fn is_digits(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_digit())
}

fn xml_blocks<'a>(content: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut blocks = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find(&open) {
        rest = &rest[start + open.len()..];
        let Some(end) = rest.find(&close) else {
            break;
        };
        blocks.push(&rest[..end]);
        rest = &rest[end + close.len()..];
    }
    blocks
}

fn xml_text(content: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = content.find(&open)? + open.len();
    let rest = &content[start..];
    let end = rest.find(&close)?;
    Some(unescape_xml(rest[..end].trim()))
}

fn unescape_xml(value: &str) -> String {
    if !value.contains('&') {
        return value.to_string();
    }

    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Restaura SÓ o root (`/`) para o snapshot `number`, com o subvol-alvo
/// explícito (`@` do fstab) — preserva `home` e `root_home`. O sync pós-rollback
/// garante consistência do `/boot`: no-op se o snapshot já casa com o `/boot`,
/// senão reescreve. Em falha de sync, bloqueia o reboot (não bricka).
fn execute_restore_root_to_snapshot(
    toplevel: &Path,
    root_subvol: &str,
    number: u32,
) -> Result<()> {
    let src = toplevel
        .join(root_subvol)
        .join(".snapshots")
        .join(number.to_string())
        .join("snapshot");

    let label = btrfs::now_local_label().context("obter label de tempo")?;
    let pb = crate::ui::term::spinner(format!("restaurando / → snapshot #{number}…"));
    let result = rollback::rollback_root_explicit_preserving_regret(toplevel, root_subvol, &src, &label);
    pb.finish_and_clear();
    let done = result.with_context(|| format!("rollback de / para o snapshot #{number}"))?;
    crate::ui::restore::print_root_restore_done(number, &done);

    let restored_root = toplevel.join(root_subvol);
    if let Err(e) = boot::sync_fat32_after_restore(&restored_root) {
        return abort_reboot_boot_desync(
            &restored_root,
            e,
            "verifique /boot manualmente (vmlinuz/initramfs vs /usr/lib/modules \
             do root) antes de reiniciar; não reinicie enquanto não corresponderem.",
        );
    }

    finish_restore_with_undo(&[done], toplevel)
}

/// Decide se o aviso de /boot FAT32 deve aparecer antes do rollback. Se o root
/// participa, o sync de boot roda completo: vmlinuz idêntico não prova que o
/// initramfs casa com o root restaurado (DKMS/hooks/config).
fn should_warn_fat32(group: &Group, toplevel: &Path) -> bool {
    if !boot::is_fat32() {
        return false;
    }
    // Err (detecção falhou) → true: fail-safe mantém o aviso.
    boot_will_change(group, toplevel).unwrap_or(true)
}

/// Ok(true) se o sync pós-rollback vai tocar /boot. Em FAT32, qualquer restore
/// do root exige regenerar o initramfs para fechar o caso de DKMS sem troca de
/// kernel. Ok(false) só para grupos que não restauram root.
fn boot_will_change(group: &Group, _toplevel: &Path) -> Result<bool> {
    Ok(group::root_member(group)?.is_some())
}

fn execute_restore_checkpoint(
    group: &Group,
    mount_path: &Path,
    regret_policy: RestoreRegretPolicy,
) -> Result<()> {
    let pb = crate::ui::term::spinner(format!(
        "restaurando checkpoint {} ({} membros)…",
        group.id,
        group.members.len()
    ));
    let outcome = match regret_policy {
        RestoreRegretPolicy::Update => rollback::rollback_group(group, mount_path),
        RestoreRegretPolicy::PreserveExisting => {
            let label = btrfs::now_local_label().context("obter label de tempo")?;
            rollback::rollback_group_preserving_regret(group, mount_path, &label)
        }
    };
    pb.finish_and_clear();
    match outcome {
        Ok(done) => {
            crate::ui::restore::print_checkpoint_rollback_done(group.id, &done);

            // Sincroniza kernel/initramfs em /boot (FAT32) com o snapshot restaurado.
            // Em FAT32, falha de sync = /boot descasado do root → reboot bricka o
            // sistema. Bloqueia o reboot em vez de só avisar.
            if let Some(root) = done.iter().find(|d| d.mountpoint == "/") {
                let restored_root = mount_path.join(&root.current_subvol);
                if let Err(e) = boot::sync_fat32_after_restore(&restored_root) {
                    return abort_reboot_boot_desync(
                        &restored_root,
                        e,
                        "rode 'snapg restore' e escolha 'Cancelar a restauração' \
                         para voltar ao sistema bootável atual antes de qualquer reboot.",
                    );
                }
            }

            finish_restore_with_undo(&done, mount_path)
        }
        Err(rerr) => {
            handle_partial(group, rerr, mount_path)?;
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

        if let Err(e) = boot::sync_fat32_after_restore(&restored_root_path) {
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

fn handle_partial(g: &Group, rerr: RollbackError, mount_path: &Path) -> Result<()> {
    crate::ui::restore::print_partial_failure(g.id, &rerr);

    // Fase 1 falhou: sistema vivo 100% intocado, nenhum regret novo criado.
    if rerr.done.is_empty() {
        return Ok(());
    }

    if !crate::ui::restore::confirm_revert_partial(rerr.done.len())? {
        crate::ui::restore::print_manual_recovery(&rerr.done, mount_path);
        return Ok(());
    }

    if let Err(re) = rollback::revert_partial(&rerr.done, mount_path) {
        crate::ui::restore::print_auto_revert_failed(&re, mount_path);
        return Ok(());
    }

    crate::ui::restore::print_partial_reverted();
    Ok(())
}

pub fn delete(yes: bool, purge: bool) -> Result<()> {
    // Gate: não apagar checkpoints com uma restauração pendente de reboot —
    // resolve antes (concluir ou cancelar).
    let cfg = config::load();
    let configs = snapper::list_configs()?;
    if gate_pending_restore_if_any(&configs)? {
        return Ok(());
    }

    // Purga de carona, como no save.
    purge_expired_trash(&cfg);

    let groups = group::list_groups()?;
    if groups.is_empty() {
        snapshots::print_no_groups();
        return Ok(());
    }

    // -y: opera no mais recente sem TUI (scripting / emergência com --purge).
    if yes {
        return apply_delete(&[&groups[0]], purge);
    }

    // Wizard interativo no alternate screen; a ação roda fora, no normal.
    let Some(target_indices) = snapshots::select_delete_plan(&groups, purge)? else {
        snapshots::print_delete_cancelled();
        return Ok(());
    };
    let targets: Vec<&Group> = target_indices.iter().map(|&i| &groups[i]).collect();
    apply_delete(&targets, purge)
}

/// Purge = deleção permanente imediata (irreversível). Senão move pra lixeira,
/// reversível até o purge automático após `TRASH_PRUNE_DAYS`.
fn apply_delete(targets: &[&Group], purge: bool) -> Result<()> {
    if purge {
        for g in targets {
            delete_group(g)?;
        }
        return Ok(());
    }
    let trash_epoch = epoch_now()?;
    let mut moved = 0;
    for g in targets {
        if trash_group(g, trash_epoch, TrashReason::Manual) > 0 {
            moved += 1;
        }
    }
    if moved > 0 {
        snapshots::print_trash_done(moved);
    }
    Ok(())
}

fn delete_group(g: &Group) -> Result<()> {
    delete_group_members(g)?;
    snapshots::print_delete_done(g);
    Ok(())
}

fn delete_group_members(g: &Group) -> Result<()> {
    for m in &g.members {
        snapper::delete(&m.config, m.snapshot.number)
            .with_context(|| format!("apagar {} #{}", m.config, m.snapshot.number))?;
    }
    Ok(())
}

/// Gestão da lixeira: lista grupos marcados e oferece restaurar (volta ao pool)
/// ou apagar de vez. Não gateia pending — flip de userdata e delete de
/// snapshots na lixeira não tocam a máquina de restauração pendente.
pub fn trash() -> Result<()> {
    let cfg = config::load();
    let groups = group::list_trashed_groups()?;
    if groups.is_empty() {
        snapshots::print_trash_empty();
        return Ok(());
    }

    use crate::ui::trash::TrashAction;
    match crate::ui::trash::select_trash_action(&groups, cfg.trash_prune_days)? {
        None => {
            snapshots::print_trash_cancelled();
            Ok(())
        }
        Some(TrashAction::Restore(indices)) => {
            let mut restored = 0;
            for i in indices {
                if untrash_group(&groups[i]) > 0 {
                    restored += 1;
                }
            }
            if restored > 0 {
                snapshots::print_untrash_done(restored);
            }
            Ok(())
        }
        Some(TrashAction::Purge(indices)) => {
            for i in indices {
                delete_group(&groups[i])?;
            }
            Ok(())
        }
    }
}

/// Tira o grupo da lixeira. Best-effort por membro, como `trash_group`.
fn untrash_group(g: &Group) -> usize {
    let mut ok = 0;
    for m in &g.members {
        match snapper::untrash(&m.config, m.snapshot.number, g.id) {
            Ok(()) => ok += 1,
            Err(e) => snapshots::print_untrash_member_failed(&m.config, m.snapshot.number, &e),
        }
    }
    ok
}

pub fn list() -> Result<()> {
    clear_screen();
    let groups = group::list_groups()?;

    let configs = snapper::list_configs()?;
    if groups.is_empty() && configs.is_empty() {
        snapshots::print_no_groups();
        return Ok(());
    }

    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;

    let result = (|| -> Result<()> {
        if pending_restore_from_live(&configs)?.is_some() {
            snapshots::print_pending_restore_status();
        }
        let regret = discover_regrets(&mount_path, &configs)?;
        let mut printed_regret = false;
        if let Some(r) = regret {
            let kernel = regret_kernel_label(&r, &mount_path);
            snapshots::print_regret_status(&r.creation_time, &kernel);
            printed_regret = true;
        }

        if groups.is_empty() {
            snapshots::print_no_groups();
        } else {
            let kernel_labels = group::kernel_labels(&groups, &mount_path);
            snapshots::print_groups(&groups, &kernel_labels, !printed_regret)?;
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
/// Apaga sobras pós-restore no top-level e desarma o serviço.
/// Output vai pro journal (stdout/stderr capturados pelo systemd).
pub fn boot_clean() -> Result<()> {
    let uuid = btrfs::fs_uuid("/")?;
    let mount_path = rollback::toplevel_mount_path(&uuid);
    btrfs::mount_toplevel(&uuid, &mount_path).context("mount toplevel falhou")?;

    let result = boot_clean_inner(&mount_path);
    let _ = btrfs::umount_toplevel(&mount_path);
    result?;

    // Desarma o serviço — independente de ter sobras ou não.
    if let Err(e) = disarm_boot_cleanup() {
        crate::ui::boot_clean::print_disarm_failed(&e);
    }
    Ok(())
}

fn boot_clean_inner(mount_path: &Path) -> Result<()> {
    let targets = discover_boot_cleanup_targets(mount_path)?;
    if targets.is_empty() {
        crate::ui::boot_clean::print_no_cleanup_targets();
        return Ok(());
    }

    let total = targets.len();
    let mut ok = 0usize;
    for (name, path) in &targets {
        match btrfs::delete_subvolume(path) {
            Ok(()) => {
                crate::ui::boot_clean::print_cleanup_target_removed(name);
                ok += 1;
            }
            Err(e) => crate::ui::boot_clean::print_cleanup_target_remove_failed(name, &e),
        }
    }
    crate::ui::boot_clean::print_cleanup_summary(ok, total);
    Ok(())
}

/// Descobre sobras pós-restore no toplevel:
/// - `_snapg_discard_*`, deixados pelo doctor/restore preservando Regret.
/// - `_snapg_undo_*`, deixados por `revert_regret`.
fn discover_boot_cleanup_targets(toplevel: &Path) -> Result<Vec<(String, std::path::PathBuf)>> {
    let cfg_map = config_subvol_map()?;
    let mut found = Vec::new();
    for entry in fs::read_dir(toplevel).context("ler toplevel pra descobrir sobras pós-restore")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        for (_, _, current) in &cfg_map {
            let discard_prefix = rollback::discard_prefix(current);
            let undo_prefix = rollback::undo_prefix(current);
            if name.starts_with(&discard_prefix) || name.starts_with(&undo_prefix) {
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

fn finish_restore_with_undo(
    done: &[rollback::Done],
    mount_path: &Path,
) -> Result<()> {
    match crate::ui::restore::select_post_restore_action()? {
        PostRestoreAction::Undo => undo_restore_before_reboot(done, mount_path),
        PostRestoreAction::RebootNow => {
            arm_cleanup_for_restore(done, mount_path)?;
            reboot_now()
        }
        PostRestoreAction::RebootLater => {
            arm_cleanup_for_restore(done, mount_path)?;
            crate::ui::restore::print_manual_reboot();
            Ok(())
        }
    }
}

fn undo_restore_before_reboot(
    done: &[rollback::Done],
    mount_path: &Path,
) -> Result<()> {
    let any_undo = done.iter().any(done_is_regret_undo);
    if any_undo && !done.iter().all(done_is_regret_undo) {
        bail!("pending misto entre undo de Regret e restore normal; abortando cancelamento");
    }

    // Sincroniza o /boot com o sistema que vai permanecer ANTES do swap dos
    // subvols: o backup_subvol é o chão vivo, o mesmo conteúdo que ocupará o
    // nome ativo depois. Se o sync for interrompido (Ctrl-C), o pending ainda
    // existe e o gate FAT32 ressincroniza na próxima execução. Na ordem
    // inversa (swap primeiro), uma interrupção aqui deixava o /boot
    // dessincronizado já sem pending — invisível para o gate, só um doctor
    // explícito detectava.
    let cancel_boot_sync = if let Some(root) = done.iter().find(|d| d.mountpoint == "/") {
        let remaining_root = mount_path.join(&root.backup_subvol);
        boot::sync_fat32_before_cancel(&remaining_root)
            .context("sincronizar /boot com o sistema atual; nada foi desfeito")?
    } else {
        None
    };

    if any_undo {
        rollback::cancel_regret_undo(done, mount_path)
            .context("cancelar restauração de Regret antes do reboot")?;
    } else {
        rollback::revert_partial(done, mount_path).context("desfazer restauração antes do reboot")?;
    }
    if let Some(sync) = cancel_boot_sync {
        sync.finish_btrfs();
    }

    crate::ui::restore::print_restore_undone(done.len());
    Ok(())
}

fn arm_cleanup_for_restore(
    done: &[rollback::Done],
    mount_path: &Path,
) -> Result<()> {
    if !done.iter().any(done_needs_boot_cleanup) {
        return Ok(());
    }

    let cleanup_root = done
        .iter()
        .find(|d| d.mountpoint == "/")
        .map(|root| mount_path.join(&root.current_subvol));
    match arm_boot_cleanup(cleanup_root.as_deref().unwrap_or(Path::new("/"))) {
        Ok(()) => crate::ui::restore::print_cleanup_armed(),
        Err(e) => crate::ui::restore::print_cleanup_arm_failed(&e),
    }
    Ok(())
}

fn done_needs_boot_cleanup(done: &rollback::Done) -> bool {
    done.backup_subvol
        .starts_with(&rollback::discard_prefix(&done.current_subvol))
        || done
            .backup_subvol
            .starts_with(&rollback::undo_prefix(&done.current_subvol))
}

fn done_is_regret_undo(done: &rollback::Done) -> bool {
    done.backup_subvol
        .starts_with(&rollback::undo_prefix(&done.current_subvol))
}

fn prompt_reboot() -> Result<()> {
    if !crate::ui::restore::confirm_reboot_now()? {
        crate::ui::restore::print_manual_reboot();
        return Ok(());
    }
    reboot_now()
}

fn reboot_now() -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::{pending_current_subvol, snapgroup_id_from_info_xml, xml_text};

    #[test]
    fn detects_pending_current_from_live_subvol_names() {
        assert_eq!(
            pending_current_subvol("@_snapg_regret").as_deref(),
            Some("@")
        );
        assert_eq!(
            pending_current_subvol("@home_snapg_discard_20260609-120000").as_deref(),
            Some("@home")
        );
        assert_eq!(
            pending_current_subvol("@root_snapg_undo_20260609-120000").as_deref(),
            Some("@root")
        );
        assert_eq!(pending_current_subvol("@home"), None);
        assert_eq!(pending_current_subvol("_snapg_regret"), None);
        assert_eq!(pending_current_subvol("@home_snapg_discard_"), None);
        assert_eq!(pending_current_subvol("@root_snapg_undo_"), None);
    }

    #[test]
    fn extracts_snapgroup_id_from_key_value_userdata() {
        let xml = r#"
            <snapshot>
              <userdata>
                <key>snapgroup-id</key>
                <value>1790000000</value>
              </userdata>
            </snapshot>
        "#;

        assert_eq!(
            snapgroup_id_from_info_xml(xml).as_deref(),
            Some("1790000000")
        );
    }

    #[test]
    fn extracts_snapgroup_id_from_equals_userdata() {
        let xml = r#"<userdata>snapgroup-id=1790000001</userdata>"#;

        assert_eq!(
            snapgroup_id_from_info_xml(xml).as_deref(),
            Some("1790000001")
        );
    }

    #[test]
    fn extracts_snapgroup_id_from_matching_userdata_block() {
        let xml = r#"
            <snapshot>
              <userdata>
                <key>numeric-other</key>
                <value>111</value>
              </userdata>
              <userdata>
                <key>snapgroup-id</key>
                <value>1790000002</value>
              </userdata>
            </snapshot>
        "#;

        assert_eq!(
            snapgroup_id_from_info_xml(xml).as_deref(),
            Some("1790000002")
        );
    }

    #[test]
    fn rejects_non_numeric_snapgroup_id_value() {
        let xml = r#"
            <userdata>
              <key>snapgroup-id</key>
              <value>179x</value>
            </userdata>
        "#;

        assert_eq!(snapgroup_id_from_info_xml(xml), None);
    }

    #[test]
    fn extracts_snapgroup_id_with_whitespace_and_newlines() {
        // xml_text faz trim do conteúdo interno, então key/value indentados em
        // múltiplas linhas ainda casam. Guarda de regressão dessa tolerância.
        let xml = r#"
            <userdata>
              <key>
                snapgroup-id
              </key>
              <value>
                1790000003
              </value>
            </userdata>
        "#;

        assert_eq!(
            snapgroup_id_from_info_xml(xml).as_deref(),
            Some("1790000003")
        );
    }

    #[test]
    fn reads_and_unescapes_xml_text() {
        let xml = r#"<description>root &amp; boot &lt;ok&gt;</description>"#;

        assert_eq!(
            xml_text(xml, "description").as_deref(),
            Some("root & boot <ok>")
        );
    }
}
