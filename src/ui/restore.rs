use crate::btrfs;
use crate::group::{self, Group, GroupId};
use crate::rollback;
use crate::rollback::RollbackError;
use crate::snapper;
use crate::ui::term::{THEME, branch, clear_screen, confirm, stem, truncate_for_terminal};
use anyhow::{Context, Result};
use console::style;
use std::path::Path;

/// Entrada de regret descoberta no toplevel.
#[derive(Clone)]
pub(crate) struct RegretEntry {
    pub(crate) config: String,
    pub(crate) mountpoint: String,
    pub(crate) current_subvol: String,
    pub(crate) regret_subvol: String,
}

/// Regret ativo com data de criação (metadata BTRFS).
pub(crate) struct RegretInfo {
    pub(crate) entries: Vec<RegretEntry>,
    pub(crate) creation_time: String,
}

#[derive(Clone, Copy)]
pub(crate) enum RestoreFlow {
    Continue,
    Back,
    Abort,
}

/// Ação selecionada na TUI.
pub(crate) enum RestoreAction {
    Checkpoint(GroupId),
    Regret,
    Abort,
}

pub(crate) fn select_restore_action(
    groups: &[Group],
    regret: Option<&RegretInfo>,
) -> Result<RestoreAction> {
    let mut items: Vec<String> = Vec::new();
    let mut actions: Vec<RestoreAction> = Vec::new();

    // Select prefix: "> " = 2 chars
    let prefix_len = 4;

    clear_screen();
    println!("{}", style("Pontos de restauração").bold());
    println!();

    if let Some(r) = regret {
        let text = format!(
            "⟲ regret  ·  {}  ·  {} membros  ·  estado anterior",
            r.creation_time,
            r.entries.len()
        );
        items.push(truncate_for_terminal(&text, prefix_len));
        actions.push(RestoreAction::Regret);
    }

    for g in groups {
        let date = group::date(g);
        let desc = group::description(g);
        let text = format!(
            "checkpoint {}  ·  {}  ·  {} membros  ·  {}",
            g.id,
            date,
            g.members.len(),
            desc
        );
        items.push(truncate_for_terminal(&text, prefix_len));
        actions.push(RestoreAction::Checkpoint(g.id));
    }

    let Some(selection) = dialoguer::Select::with_theme(&THEME)
        .with_prompt("Selecione o ponto de restauração")
        .items(&items)
        .default(0)
        .clear(true)
        .interact_opt()
        .context("seleção cancelada")?
    else {
        return Ok(RestoreAction::Abort);
    };

    Ok(actions.remove(selection))
}

pub(crate) fn print_no_restore_points() {
    println!("nenhum checkpoint ou regret encontrado — nada pra restaurar");
}

pub(crate) fn print_cancelled() {
    println!("cancelado");
}

pub(crate) fn confirm_fat32_boot() -> Result<bool> {
    eprintln!();
    eprintln!("{} ATENÇÃO: /boot está em FAT32 (vfat)", style("⚠").yellow().bold());
    eprintln!("  O snapg tentará sincronizar kernel/initramfs em /boot,");
    eprintln!("  mas este é um modo legado: /boot fica fora do snapshot BTRFS.");
    eprintln!("  Se a sincronização falhar, o backup de /boot será restaurado,");
    eprintln!("  mas o modo nativo recomendado continua sendo /boot em BTRFS.");
    eprintln!();
    confirm("Continuar mesmo assim?")
}

pub(crate) fn print_cancelled_boot_risk() {
    println!("cancelado (risco de dessincronização de boot)");
}

pub(crate) fn print_checkpoint_rollback_done(group_id: GroupId, done: &[rollback::Done]) {
    println!(
        "{} rollback completo do grupo {} ({} membros)",
        style("✓").green().bold(),
        group_id,
        done.len()
    );
    for d in done {
        println!(
            "    {}: sistema atual arquivado como {}",
            d.config, d.backup_subvol
        );
    }
}

pub(crate) fn print_previous_regret_restored() {
    eprintln!("  regret anterior restaurado");
}

pub(crate) fn print_regret_restore_done(done_len: usize) {
    println!("{} regret restaurado ({} membros)", style("✓").green().bold(), done_len);
    println!("  subvols atuais preservados como discard (limpos no próximo boot)");
}

pub(crate) fn print_cleanup_armed() {
    println!("  cleanup automático armado para o próximo boot");
}

pub(crate) fn print_cleanup_arm_failed(error: &anyhow::Error) {
    eprintln!(
        "{} não consegui armar cleanup automático: {error:#}\n  \
         limpe manualmente após reboot: snapg boot-clean",
        style("⚠").yellow().bold()
    );
}

pub(crate) fn select_checkpoint_members(group: &Group) -> Result<Option<Group>> {
    let mut items: Vec<String> = Vec::new();
    for m in &group.members {
        let mountpoint = snapper::config_subvolume(&m.config)?;
        let text = format!(
            "{:<10} {:<8} #{:<5} {}",
            m.config,
            mountpoint,
            m.snapshot.number,
            m.snapshot.date
        );
        items.push(truncate_for_terminal(&text, 6));
    }

    clear_screen();
    println!(
        "{} {}  {}  {}  {}  {}",
        style("Checkpoint").bold(),
        group.id,
        style("·").dim(),
        group::date(group),
        style("·").dim(),
        group::description(group)
    );
    println!();

    let Some(selections) = dialoguer::MultiSelect::with_theme(&THEME)
        .with_prompt("Selecione os membros para restaurar  (espaço marca · enter confirma · esc volta)")
        .items(&items)
        .defaults(&vec![true; group.members.len()])
        .clear(true)
        .interact_opt()
        .context("seleção cancelada")?
    else {
        return Ok(None);
    };

    if selections.is_empty() {
        println!("nenhum membro selecionado");
        return Ok(None);
    }

    let members = selections
        .into_iter()
        .filter_map(|i| group.members.get(i).cloned())
        .collect();

    Ok(Some(Group {
        id: group.id,
        members,
    }))
}

pub(crate) fn select_regret_members(regret: &RegretInfo) -> Result<Option<RegretInfo>> {
    let mut items: Vec<String> = Vec::new();
    for e in &regret.entries {
        let text = format!(
            "{:<10} {:<8} {} → {}",
            e.config, e.mountpoint, e.regret_subvol, e.current_subvol
        );
        items.push(truncate_for_terminal(&text, 6));
    }

    clear_screen();
    println!(
        "{}  {}  estado anterior à última restauração  {}  criado {}",
        style("Regret").bold(),
        style("·").dim(),
        style("·").dim(),
        regret.creation_time
    );
    println!();

    let Some(selections) = dialoguer::MultiSelect::with_theme(&THEME)
        .with_prompt("Selecione os membros do Regret para restaurar  (espaço marca · enter confirma · esc volta)")
        .items(&items)
        .defaults(&vec![true; regret.entries.len()])
        .clear(true)
        .interact_opt()
        .context("seleção cancelada")?
    else {
        return Ok(None);
    };

    if selections.is_empty() {
        println!("nenhum membro selecionado");
        return Ok(None);
    }

    let entries = selections
        .into_iter()
        .filter_map(|i| regret.entries.get(i).cloned())
        .collect();

    Ok(Some(RegretInfo {
        entries,
        creation_time: regret.creation_time.clone(),
    }))
}

pub(crate) fn review_checkpoint_restore(
    original: &Group,
    selected: &Group,
) -> Result<RestoreFlow> {
    let skipped: Vec<&str> = original
        .members
        .iter()
        .filter(|m| !selected.members.iter().any(|s| s.config == m.config))
        .map(|m| m.config.as_str())
        .collect();

    clear_screen();
    println!(
        "{} {} checkpoint {}  {}  {}/{} membros",
        style("Restauração").bold(),
        style("·").dim(),
        original.id,
        style("·").dim(),
        selected.members.len(),
        original.members.len()
    );

    let has_skip = !skipped.is_empty();
    println!("{} aplicar", branch(!has_skip));
    let total = selected.members.len();
    for (i, m) in selected.members.iter().enumerate() {
        let mountpoint = snapper::config_subvolume(&m.config)?;
        let current = btrfs::subvol_relative_path(Path::new(&mountpoint))
            .with_context(|| format!("descobrir subvol ativo de '{}'", m.config))?;
        println!(
            "{}{} {:<10} {:<8} #{} → {}",
            stem(!has_skip),
            branch(i + 1 == total),
            m.config,
            mountpoint,
            m.snapshot.number,
            rollback::regret_name(&current)
        );
    }
    print_keep_branch(&skipped);
    read_restore_flow()
}

pub(crate) fn review_regret_restore(
    original: &RegretInfo,
    selected: &RegretInfo,
) -> Result<RestoreFlow> {
    let skipped: Vec<&str> = original
        .entries
        .iter()
        .filter(|e| !selected.entries.iter().any(|s| s.config == e.config))
        .map(|e| e.config.as_str())
        .collect();

    clear_screen();
    println!(
        "{} {} regret  {}  {}/{} membros",
        style("Restauração").bold(),
        style("·").dim(),
        style("·").dim(),
        selected.entries.len(),
        original.entries.len()
    );

    let has_skip = !skipped.is_empty();
    println!("{} aplicar", branch(!has_skip));
    let total = selected.entries.len();
    for (i, e) in selected.entries.iter().enumerate() {
        println!(
            "{}{} {:<10} {:<8} {} → {}",
            stem(!has_skip),
            branch(i + 1 == total),
            e.config,
            e.mountpoint,
            e.regret_subvol,
            e.current_subvol
        );
    }
    print_keep_branch(&skipped);
    read_restore_flow()
}

pub(crate) fn print_partial_failure(group_id: GroupId, rerr: &RollbackError) {
    eprintln!();
    eprintln!("{} FALHA PARCIAL no rollback do grupo {}", style("⚠").yellow().bold(), group_id);
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
    eprintln!("{} NÃO REINICIE até decidir.", style("⚠").yellow().bold());
    eprintln!();
}

pub(crate) fn confirm_revert_partial(done_len: usize) -> Result<bool> {
    let prompt = format!("Reverter os {done_len} membros já feitos automaticamente?");
    confirm(&prompt)
}

pub(crate) fn print_auto_revert_failed(error: &anyhow::Error, mount_path: &Path) {
    eprintln!();
    eprintln!("{} revert automático falhou no meio: {error:#}", style("✗").red().bold());
    eprintln!(
        "  toplevel ainda montado em {}",
        mount_path.display()
    );
    eprintln!(
        "  resolva manualmente lá e depois: sudo umount {}",
        mount_path.display()
    );
}

pub(crate) fn print_partial_reverted() {
    println!();
    println!("{} rollback parcial revertido — sistema voltou ao estado pré-restore", style("✓").green().bold());
}

/// Imprime onde cada regret anterior ficou preservado (estado ambíguo) e o
/// comando de reativação por config. O usuário está em recuperação manual:
/// só deve renomear de volta após confirmar que o slot canônico está livre.
pub(crate) fn print_preserved_asides(asides: &[rollback::AsidedRegret], mount_path: &Path) {
    if asides.is_empty() {
        return;
    }
    eprintln!();
    eprintln!("regret anterior preservado (não restaurado: estado ambíguo):");
    for a in asides {
        eprintln!(
            "  {}: {}",
            a.config,
            mount_path.join(&a.aside_subvol).display()
        );
    }
    eprintln!("  reative só após confirmar que {{subvol}}_snapg_regret está livre:");
    for a in asides {
        eprintln!(
            "  sudo mv {} {}",
            mount_path.join(&a.aside_subvol).display(),
            mount_path.join(&a.regret_subvol).display()
        );
    }
}

pub(crate) fn print_manual_recovery(done: &[rollback::Done], mount_path: &Path) {
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

pub(crate) fn confirm_reboot_now() -> Result<bool> {
    confirm("Reiniciar agora?")
}

pub(crate) fn print_manual_reboot() {
    println!("{} reinicie manualmente para concluir a restauração", style("⚠").yellow().bold());
}

fn read_restore_flow() -> Result<RestoreFlow> {
    println!();
    let flows = [RestoreFlow::Continue, RestoreFlow::Abort];
    let Some(choice) = dialoguer::Select::with_theme(&THEME)
        .with_prompt("Confirma a restauração?  (esc volta)")
        .items(&["Continuar", "Abortar"])
        .default(0)
        .clear(true)
        .interact_opt()
        .context("seleção cancelada")?
    else {
        // Esc volta um passo: pra seleção de membros (loop interno do restore).
        return Ok(RestoreFlow::Back);
    };
    Ok(flows[choice])
}

/// Ramo "manter" da árvore de restauração: lista os membros não selecionados.
/// Vazio = nada a imprimir (o ramo "aplicar" já foi o último).
fn print_keep_branch(skipped: &[&str]) {
    if skipped.is_empty() {
        return;
    }
    println!("{} manter", branch(true));
    let total = skipped.len();
    for (i, config) in skipped.iter().enumerate() {
        println!("{}{} {}", stem(true), branch(i + 1 == total), config);
    }
}
