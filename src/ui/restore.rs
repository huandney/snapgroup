use crate::btrfs;
use crate::group::{self, Group, GroupId};
use crate::rollback;
use crate::rollback::RollbackError;
use crate::snapper;
use crate::ui::term::{
    AltScreen, CONTENT_INDENT, HINT_BACK, HINT_MULTI, MULTI_MARKER, PAGE_INDENT, SELECT_MARKER,
    THEME, branch, clear_screen, confirm, header, line, prompt_bold_hint, prompt_hint, regret_title,
    short_datetime, tree_branch, tree_stem, truncate_for_terminal,
};
use anyhow::{Context, Result};
use console::style;
use std::collections::HashMap;
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

#[derive(Clone, Copy)]
pub(crate) enum PostRestoreAction {
    RebootNow,
    Undo,
    RebootLater,
}

/// Ação selecionada na TUI.
pub(crate) enum RestoreAction {
    Checkpoint(GroupId),
    Regret,
    Abort,
}

/// Plano de restauração escolhido pelo wizard, pronto pra executar no terminal
/// normal (já fora do alternate screen).
pub(crate) enum RestorePlan {
    Checkpoint(Group),
    Regret(RegretInfo),
}

/// Roda o wizard interativo inteiro (pontos → membros → revisão) dentro do
/// alternate screen e devolve o plano escolhido, ou `None` se cancelado/abortado.
/// Esc nos membros volta pros pontos; Esc/Abortar na revisão volta pros membros.
pub(crate) fn select_restore_plan(
    groups: &[Group],
    regret: Option<&RegretInfo>,
    kernel_labels: &HashMap<GroupId, String>,
    regret_kernel: Option<&str>,
) -> Result<Option<RestorePlan>> {
    let _alt = AltScreen::enter();
    loop {
        match select_restore_action(groups, regret, kernel_labels, regret_kernel)? {
            RestoreAction::Checkpoint(group_id) => {
                let group = groups.iter().find(|g| g.id == group_id).unwrap();
                loop {
                    let Some(selected) = select_checkpoint_members(group)? else {
                        break;
                    };
                    match review_checkpoint_restore(group, &selected)? {
                        RestoreFlow::Continue => {
                            return Ok(Some(RestorePlan::Checkpoint(selected)));
                        }
                        RestoreFlow::Back => continue,
                        RestoreFlow::Abort => return Ok(None),
                    }
                }
            }
            RestoreAction::Regret => {
                let info = regret.unwrap();
                loop {
                    let Some(selected) = select_regret_members(info)? else {
                        break;
                    };
                    match review_regret_restore(info, &selected)? {
                        RestoreFlow::Continue => {
                            return Ok(Some(RestorePlan::Regret(selected)));
                        }
                        RestoreFlow::Back => continue,
                        RestoreFlow::Abort => return Ok(None),
                    }
                }
            }
            RestoreAction::Abort => return Ok(None),
        }
    }
}

pub(crate) fn select_restore_action(
    groups: &[Group],
    regret: Option<&RegretInfo>,
    kernel_labels: &HashMap<GroupId, String>,
    regret_kernel: Option<&str>,
) -> Result<RestoreAction> {
    let mut items: Vec<String> = Vec::new();
    let mut actions: Vec<RestoreAction> = Vec::new();

    let prefix_len = SELECT_MARKER;

    clear_screen();
    header("Pontos de restauração");
    let name_col = restore_name_col(groups, regret.is_some());
    let kernel_col = restore_kernel_col(groups, kernel_labels, regret_kernel);

    if let Some(r) = regret {
        let kernel = regret_kernel.unwrap_or("?");
        let text = format!(
            "{:<name_col$}   {:<kernel_col$}   {}   {} membros",
            "↺ Regret",
            kernel,
            short_datetime(&r.creation_time),
            r.entries.len()
        );
        items.push(truncate_for_terminal(&text, prefix_len));
        actions.push(RestoreAction::Regret);
    }

    for g in groups {
        let kernel = kernel_labels.get(&g.id).map(String::as_str).unwrap_or("?");
        let desc = group::description(g);
        let name = if desc.chars().count() > name_col {
            let cut: String = desc.chars().take(name_col - 1).collect();
            format!("{cut}…")
        } else {
            format!("{desc:<name_col$}")
        };
        let text = format!(
            "{}   {:<kernel_col$}   {}   {} membros   #{}",
            name,
            kernel,
            short_datetime(group::date(g)),
            g.members.len(),
            g.id
        );
        items.push(truncate_for_terminal(&text, prefix_len));
        actions.push(RestoreAction::Checkpoint(g.id));
    }

    let Some(selection) = dialoguer::Select::with_theme(&THEME)
        .items(&items)
        .default(0)
        .clear(true)
        .report(false)
        .interact_opt()
        .context("seleção cancelada")?
    else {
        return Ok(RestoreAction::Abort);
    };

    Ok(actions.remove(selection))
}

const RESTORE_NAME_COL_MAX: usize = 28;

fn restore_name_col(groups: &[Group], has_regret: bool) -> usize {
    let regret_len = if has_regret { "↺ Regret".chars().count() } else { 0 };
    groups
        .iter()
        .map(|g| group::description(g).chars().count())
        .max()
        .unwrap_or(regret_len)
        .max(regret_len)
        .min(RESTORE_NAME_COL_MAX)
}

fn restore_kernel_col(
    groups: &[Group],
    kernel_labels: &HashMap<GroupId, String>,
    regret_kernel: Option<&str>,
) -> usize {
    let group_max = groups
        .iter()
        .filter_map(|g| kernel_labels.get(&g.id))
        .map(|k| k.chars().count())
        .max()
        .unwrap_or(1);
    regret_kernel
        .map(|k| k.chars().count())
        .unwrap_or(1)
        .max(group_max)
}

/// Linha do picker de restore escopado ao `root`: o kernel daquele snapshot é
/// anotado para o usuário escolher o que casa com o `/boot`.
pub(crate) struct RootSnapshotRow {
    pub(crate) number: u32,
    pub(crate) date: String,
    pub(crate) kernel: String,
    /// Nome do backup quando feito pelo snapgroup (ex: "Atual 2").
    pub(crate) name: Option<String>,
}

/// Picker para "restaurar só o /": lista os kernels disponíveis (deduplicados),
/// com o nome do backup snapgroup quando houver e a data esmaecida. Retorna o
/// número do snapshot escolhido (o mais recente daquele kernel), ou `None` no ESC.
pub(crate) fn select_root_snapshot(
    rows: &[RootSnapshotRow],
    current_kernel: &str,
) -> Result<Option<u32>> {
    let mut items: Vec<String> = Vec::new();
    let max = (console::Term::stdout().size().1 as usize)
        .saturating_sub(CONTENT_INDENT.chars().count() + SELECT_MARKER);

    clear_screen();
    header("Restaurar só o / — qual kernel?");

    for r in rows {
        let marked = if r.kernel == current_kernel {
            format!("{} (atual)", r.kernel)
        } else {
            r.kernel.clone()
        };
        let kver = format!("{marked:<26}");
        let name = format!("{:<16}", r.name.as_deref().unwrap_or("—"));
        let date = short_datetime(&r.date);
        let plain = format!("kernel {kver} {name} {date}");
        // Dim na data só quando a linha plana cabe (ANSI dentro de item do Select
        // quebra a medição/wrap). Se não couber, cai no plano truncado.
        let item = if plain.chars().count() <= max {
            format!("kernel {kver} {name} {}", style(date).dim())
        } else {
            truncate_for_terminal(&plain, SELECT_MARKER)
        };
        items.push(item);
    }

    let Some(selection) = dialoguer::Select::with_theme(&THEME)
        .with_prompt("Qual kernel manter no /")
        .items(&items)
        .default(0)
        .clear(true)
        .report(false)
        .interact_opt()
        .context("seleção cancelada")?
    else {
        return Ok(None);
    };

    Ok(Some(rows[selection].number))
}

pub(crate) fn print_no_restore_points() {
    println!("nenhum checkpoint ou regret encontrado — nada pra restaurar");
}

pub(crate) fn print_no_root_snapshots() {
    clear_screen();
    header("Restaurar só o /");
    println!();
    line(format_args!(
        "{} nenhum snapshot de / disponível aqui",
        style("✗").red().bold()
    ));
    line(format_args!(
        "um boot de resgate não enxerga os snapshots do config root: eles ficam"
    ));
    line(format_args!(
        "fora da visão do snapshot atual. Restaurar o / precisa de um boot normal."
    ));
    println!();
    line(format_args!("{}", style("enter para voltar").dim()));
    // Pausa até Enter — sem isso o loop do doctor limparia a tela e a mensagem
    // sumiria antes de ser lida.
    let mut discard = String::new();
    let _ = std::io::stdin().read_line(&mut discard);
}

pub(crate) fn print_cancelled() {
    println!("cancelado");
}

pub(crate) fn confirm_fat32_boot() -> Result<bool> {
    clear_screen();
    header("Restauração");
    println!();
    println!("{PAGE_INDENT}{} /boot está em FAT32 separado", style("!").yellow().bold());
    line(format_args!("Kernel e initramfs não fazem parte do snapshot BTRFS."));
    line(format_args!("Ao restaurar este checkpoint, o snapg também vai sincronizar"));
    line(format_args!("esses arquivos e o limine.conf em /boot."));
    println!();
    line(format_args!("Se a sincronização for interrompida, rode {}.", style("snapg doctor").bold()));
    println!();
    confirm("Continuar?")
}

pub(crate) fn print_cancelled_boot_risk() {
    println!("restauração cancelada");
    println!("/boot não foi alterado");
}

pub(crate) fn print_root_restore_done(number: u32, done: &rollback::Done) {
    println!(
        "{} / restaurado para o snapshot #{} (home e /root intactos)",
        style("✓").green().bold(),
        number
    );
    println!("    root anterior arquivado como {}", done.backup_subvol);
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

pub(crate) fn select_post_restore_action() -> Result<PostRestoreAction> {
    let actions = [
        PostRestoreAction::RebootNow,
        PostRestoreAction::Undo,
        PostRestoreAction::RebootLater,
    ];
    let Some(choice) = dialoguer::Select::with_theme(&THEME)
        .with_prompt("Próximo passo")
        .items(&["Reiniciar agora", "Desfazer restauração", "Reiniciar depois"])
        .default(0)
        .clear(true)
        .report(false)
        .interact_opt()
        .context("seleção cancelada")?
    else {
        return Ok(PostRestoreAction::RebootLater);
    };
    Ok(actions[choice])
}

pub(crate) fn print_restore_undone(done_len: usize) {
    println!(
        "{} restauração desfeita sem reboot ({} membros)",
        style("✓").green().bold(),
        done_len
    );
}

pub(crate) fn select_checkpoint_members(group: &Group) -> Result<Option<Group>> {
    let mut items: Vec<String> = Vec::new();
    for m in &group.members {
        let mountpoint = snapper::config_subvolume(&m.config)?;
        let text = format!(
            "{:<10}   {:<8}   #{:<5}   {}",
            m.config,
            mountpoint,
            m.snapshot.number,
            short_datetime(&m.snapshot.date)
        );
        items.push(truncate_for_terminal(&text, MULTI_MARKER));
    }

    clear_screen();
    header("Restaurar checkpoint");
    print_checkpoint_summary(group);
    println!();

    let Some(selections) = dialoguer::MultiSelect::with_theme(&THEME)
        .with_prompt(prompt_hint("Selecione os membros para restaurar", HINT_MULTI))
        .items(&items)
        .defaults(&vec![true; group.members.len()])
        .clear(true)
        .report(false)
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

/// Linha-resumo do checkpoint no picker de membros (caso "resumo": id · data ·
/// descrição). Trunca à largura para caber numa linha só — o bloco multi-linha
/// que existia aqui quebrava ao redimensionar o terminal (ver
/// docs/roadmap/ui-layout-padronizacao.md). Descrição vazia some com o separador.
fn print_checkpoint_summary(group: &Group) {
    let date = short_datetime(group::date(group));
    let desc = group::description(group);
    let desc = desc.trim();
    if desc.is_empty() {
        line(format_args!("checkpoint {}  {}  {date}", group.id, style("·").dim()));
        return;
    }
    let head = format!("checkpoint {}  ·  {date}  ·  ", group.id);
    let avail = content_width().saturating_sub(head.chars().count()).max(8);
    let desc = if desc.chars().count() > avail {
        format!("{}…", desc.chars().take(avail - 1).collect::<String>())
    } else {
        desc.to_string()
    };
    line(format_args!(
        "checkpoint {}  {}  {date}  {}  {desc}",
        group.id,
        style("·").dim(),
        style("·").dim(),
    ));
}

fn content_width() -> usize {
    let width = console::Term::stdout().size().1 as usize;
    width.saturating_sub(CONTENT_INDENT.chars().count()).max(20)
}

pub(crate) fn select_regret_members(regret: &RegretInfo) -> Result<Option<RegretInfo>> {
    let mut items: Vec<String> = Vec::new();
    for e in &regret.entries {
        let text = format!(
            "{:<10}   {:<8}   {} → {}",
            e.config, e.mountpoint, e.regret_subvol, e.current_subvol
        );
        items.push(truncate_for_terminal(&text, MULTI_MARKER));
    }

    clear_screen();
    header("Restaurar Regret");
    line(format_args!(
        "{}  {}  estado anterior à última restauração  {}  criado {}",
        regret_title("↺ Regret"),
        style("·").dim(),
        style("·").dim(),
        short_datetime(&regret.creation_time)
    ));
    println!();

    let Some(selections) = dialoguer::MultiSelect::with_theme(&THEME)
        .with_prompt(prompt_hint("Selecione os membros do Regret para restaurar", HINT_MULTI))
        .items(&items)
        .defaults(&vec![true; regret.entries.len()])
        .clear(true)
        .report(false)
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
    header("Restauração");
    line(format_args!(
        "checkpoint {}  {}  {}/{} membros",
        style(original.id).dim(),
        style("·").dim(),
        selected.members.len(),
        original.members.len()
    ));

    let has_skip = !skipped.is_empty();
    println!("{} aplicar", tree_branch(!has_skip));
    let total = selected.members.len();
    for (i, m) in selected.members.iter().enumerate() {
        let mountpoint = snapper::config_subvolume(&m.config)?;
        let current = btrfs::subvol_relative_path(Path::new(&mountpoint))
            .with_context(|| format!("descobrir subvol ativo de '{}'", m.config))?;
        println!(
            "{}{} {:<10}   {:<8}   #{} → {}",
            tree_stem(!has_skip),
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
    header("Restauração");
    line(format_args!(
        "regret  {}  {}/{} membros",
        style("·").dim(),
        selected.entries.len(),
        original.entries.len()
    ));

    let has_skip = !skipped.is_empty();
    println!("{} aplicar", tree_branch(!has_skip));
    let total = selected.entries.len();
    for (i, e) in selected.entries.iter().enumerate() {
        println!(
            "{}{} {:<10}   {:<8}   {} → {}",
            tree_stem(!has_skip),
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
        .with_prompt(prompt_bold_hint("Confirma a restauração?", HINT_BACK))
        .items(&["Sim", "Não"])
        .default(1)
        .clear(true)
        .report(false)
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
    println!("{} manter", tree_branch(true));
    let total = skipped.len();
    for (i, config) in skipped.iter().enumerate() {
        println!("{}{} {}", tree_stem(true), branch(i + 1 == total), config);
    }
}
