use crate::boot;
use crate::btrfs;
use crate::doctor;
use crate::group::{self, Group, Member};
use crate::rollback::{self, RollbackError};
use crate::snapper;
use crate::ui::restore::{
    PostRestoreAction, RegretEntry, RegretInfo, RegretKind, RestorePlan, select_restore_plan,
};
use crate::ui::snapshots;
use crate::ui::term::{clear_screen, confirm};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
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

    fn updates_regret(self) -> bool {
        matches!(self, RestoreRegretPolicy::Update)
    }
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
        let Some(current_subvol) = live_subvol.strip_suffix("_snapg_regret") else {
            continue;
        };
        let current_subvol = current_subvol.to_string();

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

fn ensure_no_pending_restore(configs: &[String]) -> Result<()> {
    if pending_restore_from_live(configs)?.is_none() {
        return Ok(());
    }

    bail!(
        "já existe uma restauração pendente de reboot.\n  \
         Reinicie para concluir a restauração atual ou rode 'snapg restore' \
         e selecione o Regret no menu para desfazê-la."
    )
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

/// Descobre regrets existentes no toplevel.
fn discover_regrets(toplevel: &Path, configs: &[String]) -> Result<Option<RegretInfo>> {
    if let Some(done) = pending_restore_from_live(configs)? {
        return Ok(Some(regret_from_pending_restore(toplevel, done)));
    }

    discover_archived_regrets(toplevel, configs)
}

fn discover_archived_regrets(toplevel: &Path, configs: &[String]) -> Result<Option<RegretInfo>> {
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
        kind: RegretKind::Archived,
    }))
}

fn regret_from_pending_restore(toplevel: &Path, done: Vec<rollback::Done>) -> RegretInfo {
    let entries: Vec<RegretEntry> = done
        .into_iter()
        .map(|d| RegretEntry {
            config: d.config,
            mountpoint: d.mountpoint,
            current_subvol: d.current_subvol,
            regret_subvol: d.backup_subvol,
        })
        .collect();
    RegretInfo {
        creation_time: regret_creation_time(toplevel, &entries),
        entries,
        kind: RegretKind::PendingRestore,
    }
}

fn regret_creation_time(toplevel: &Path, entries: &[RegretEntry]) -> String {
    let Some(first) = entries.first() else {
        return String::from("data desconhecida");
    };
    btrfs::subvol_creation_time(&toplevel.join(&first.regret_subvol))
        .unwrap_or_else(|_| String::from("data desconhecida"))
}

fn restore_inner(
    groups: &[Group],
    configs: &[String],
    mount_path: &Path,
    regret_policy: RestoreRegretPolicy,
) -> Result<()> {
    let regret = discover_regrets(mount_path, configs)?;
    let groups = if matches!(
        regret.as_ref().map(|r| r.kind),
        Some(RegretKind::PendingRestore)
    ) {
        &[]
    } else {
        groups
    };

    if groups.is_empty() && regret.is_none() {
        crate::ui::restore::print_no_restore_points();
        return Ok(());
    }

    // Wizard interativo no alternate screen; ao sair, executa no terminal normal.
    let kernel_labels = group_kernel_labels(groups, mount_path);
    let regret_kernel = regret.as_ref().map(|r| regret_kernel_label(r, mount_path));

    match select_restore_plan(
        groups,
        regret.as_ref(),
        &kernel_labels,
        regret_kernel.as_deref(),
    )? {
        None => {
            crate::ui::restore::print_cancelled();
            Ok(())
        }
        Some(RestorePlan::Checkpoint(selected)) => {
            execute_restore_checkpoint(&selected, mount_path, regret_policy)
        }
        Some(RestorePlan::Regret(selected)) => execute_restore_regret(selected, mount_path),
    }
}

fn group_kernel_labels(groups: &[Group], toplevel: &Path) -> HashMap<group::GroupId, String> {
    groups
        .iter()
        .map(|g| (g.id, group_kernel_label(g, toplevel)))
        .collect()
}

fn group_kernel_label(group: &Group, toplevel: &Path) -> String {
    let Some(root_m) = root_member(group).ok().flatten() else {
        return "?".to_string();
    };
    rollback::member_snapshot_path(root_m, toplevel)
        .map(|path| boot::kernel_label(&path))
        .unwrap_or_else(|_| "?".to_string())
}

fn regret_kernel_label(regret: &RegretInfo, toplevel: &Path) -> String {
    let Some(root_e) = regret.entries.iter().find(|e| e.mountpoint == "/") else {
        return "?".to_string();
    };
    boot::kernel_label(&toplevel.join(&root_e.regret_subvol))
}

fn root_member(group: &Group) -> Result<Option<&Member>> {
    for member in &group.members {
        let mountpoint = snapper::config_subvolume(&member.config)?;
        if mountpoint == "/" {
            return Ok(Some(member));
        }
    }
    Ok(None)
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
    if let Err(e) = boot::sync_fat32(&restored_root) {
        return abort_reboot_boot_desync(
            &restored_root,
            e,
            "verifique /boot manualmente (vmlinuz/initramfs vs /usr/lib/modules \
             do root) antes de reiniciar; não reinicie enquanto não corresponderem.",
        );
    }

    finish_restore_with_undo(&[done], toplevel)
}

/// Decide se o aviso de /boot FAT32 deve aparecer antes do rollback. Avisa só
/// quando o sync vai de fato mexer no /boot legado: kernel idêntico ao do
/// snapshot → sync no-op → não incomoda. Qualquer falha de detecção cai no
/// aviso (fail-safe) — nunca silencia por incerteza.
fn should_warn_fat32(group: &Group, toplevel: &Path) -> bool {
    if !boot::is_fat32() {
        return false;
    }
    // Err (detecção falhou) → true: fail-safe mantém o aviso.
    boot_will_change(group, toplevel).unwrap_or(true)
}

/// Ok(true) se o sync pós-rollback vai escrever em /boot (kernel do snapshot
/// difere do /boot atual). Ok(false) se será no-op: mesmo kernel, ou grupo que
/// não restaura root. Err em falha real de leitura — o caller trata como aviso.
fn boot_will_change(group: &Group, toplevel: &Path) -> Result<bool> {
    let Some(root_m) = root_member(group)? else {
        return Ok(false);
    };
    let snapshot_root = rollback::member_snapshot_path(root_m, toplevel)?;
    Ok(!boot::boot_already_synced(&snapshot_root)?)
}

fn execute_restore_checkpoint(
    group: &Group,
    mount_path: &Path,
    regret_policy: RestoreRegretPolicy,
) -> Result<()> {
    if should_warn_fat32(group, mount_path) && !crate::ui::restore::confirm_fat32_boot()? {
        crate::ui::restore::print_cancelled_boot_risk();
        return Ok(());
    }

    let configs: Vec<String> = group.members.iter().map(|m| m.config.clone()).collect();
    if regret_policy.updates_regret() {
        ensure_no_pending_restore(&configs)?;
    }

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
                if let Err(e) = boot::sync_fat32(&restored_root) {
                    return abort_reboot_boot_desync(
                        &restored_root,
                        e,
                        "rode 'snapg restore' e selecione o Regret \
                         (↺ Regret — Estado Anterior à Restauração) para voltar ao \
                         sistema bootável atual antes de qualquer reboot.",
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
    let kind = regret.kind;
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

    match kind {
        RegretKind::Archived => restore_archived_regret(&done, mount_path),
        RegretKind::PendingRestore => undo_restore_before_reboot(&done, mount_path),
    }
}

fn restore_archived_regret(done: &[rollback::Done], mount_path: &Path) -> Result<()> {
    let label = btrfs::now_local_label().context("obter label de tempo")?;
    rollback::revert_regret(done, mount_path, &label).context("restaurar regret")?;

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
            let kernel_labels = group_kernel_labels(&groups, &mount_path);
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
/// - `_snapg_discard_*`, deixados por `revert_regret` e pelo doctor.
fn discover_boot_cleanup_targets(toplevel: &Path) -> Result<Vec<(String, std::path::PathBuf)>> {
    let cfg_map = config_subvol_map()?;
    let mut found = Vec::new();
    for entry in fs::read_dir(toplevel).context("ler toplevel pra descobrir sobras pós-restore")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        for (_, _, current) in &cfg_map {
            let prefix = rollback::discard_prefix(current);
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
    rollback::revert_partial(done, mount_path).context("desfazer restauração antes do reboot")?;

    if let Some(root) = done.iter().find(|d| d.mountpoint == "/") {
        let restored_root = mount_path.join(&root.current_subvol);
        if let Err(e) = boot::sync_fat32(&restored_root) {
            return abort_reboot_boot_desync(
                &restored_root,
                e,
                "desfazer aplicado, mas /boot precisa casar com o root restaurado. \
                 Rode 'snapg doctor --apply' antes de reiniciar.",
            );
        }
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
    use super::{snapgroup_id_from_info_xml, xml_text};

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
    fn reads_and_unescapes_xml_text() {
        let xml = r#"<description>root &amp; boot &lt;ok&gt;</description>"#;

        assert_eq!(
            xml_text(xml, "description").as_deref(),
            Some("root & boot <ok>")
        );
    }
}
