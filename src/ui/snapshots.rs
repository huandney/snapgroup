use crate::group::{self, Group, GroupId};
use crate::snapper;
use crate::ui::checkpoints::{
    CheckpointColumns, KERNEL_HEADER, NAME_HEADER, PickerTail, ReviewDecision, kernel_label,
    name_cell, picker_header, picker_prompt, picker_row, review_irreversible,
};
use crate::ui::term::{
    AltScreen, HINT_MULTI, THEME, app_header, clear_screen, ellipsize, header, input_line, line,
    prompt_hint, section_header, short_datetime, truncate_for_terminal,
};
use anyhow::{Context, Result};
use console::style;
use std::collections::HashMap;

/// Wizard do `snapg save` sem nome: membros e kernel como informação (a
/// seleção do que restaurar mora no restore — checkpoint completo é grátis no
/// CoW e parcial quebraria o sinal "grupo incompleto = suspeito" do doctor),
/// campo de nome no editor padrão. Primeira página do fluxo: Esc sai.
pub(crate) fn prompt_save_name(
    mountpoints: &mut [String],
    kernel: &str,
    placeholder: &str,
) -> Result<Option<String>> {
    let _alt = AltScreen::enter();
    let badges = member_badges(mountpoints);
    let kernel = kernel.to_string();
    input_line("Nome", "", placeholder, "enter confirma · esc sai", move || {
        clear_screen();
        header("Salvar checkpoint");
        line(format_args!("{:<9} {}", "membros", badges));
        line(format_args!("{:<9} {}", "kernel", kernel));
        println!();
    })
}

pub(crate) fn print_save_cancelled() {
    println!("save cancelado");
}

/// Confirmação do save no vocabulário do `list`: nome primeiro, badges de
/// mountpoint (root primeiro) e ID em dim. Os números por-config do snapper
/// são detalhe interno — nenhum outro comando os pede.
pub(crate) fn print_save_created(id: i64, desc: &str, mountpoints: &mut [String]) {
    println!(
        "{} {} salvo  {}  {}  {}  {}",
        style("✓").green().bold(),
        ellipsize(desc, NAME_COL_MAX),
        style("·").dim(),
        style(member_badges(mountpoints)).dim(),
        style("·").dim(),
        style(format!("#{id}")).dim()
    );
}

/// Roda o wizard de exclusão (modo → seleção → confirmação) no alternate screen
/// e devolve os índices a apagar, ou `None` se cancelado. Esc na confirmação
/// volta pra seleção (um passo). A exclusão em si roda fora, no terminal normal.
pub(crate) fn select_delete_plan(
    groups: &[Group],
    kernel_labels: &HashMap<GroupId, String>,
    purge: bool,
) -> Result<Option<Vec<usize>>> {
    let _alt = AltScreen::enter();
    // Uma tela de revisão para qualquer caminho (seleção ou "todos"): mostra o
    // que será afetado e confirma. Trash ou purge só muda o texto. Esc na
    // revisão volta pra seleção (um passo).
    loop {
        let Some(indices) = select_delete_targets(groups, kernel_labels)? else {
            return Ok(None);
        };
        let targets: Vec<&Group> = indices.iter().map(|&i| &groups[i]).collect();
        match confirm_delete_targets(&targets, kernel_labels, purge)? {
            ReviewDecision::Proceed => return Ok(Some(indices)),
            ReviewDecision::Back => continue,
            ReviewDecision::Cancel => return Ok(None),
        }
    }
}

fn select_delete_targets(
    groups: &[Group],
    kernel_labels: &HashMap<GroupId, String>,
) -> Result<Option<Vec<usize>>> {
    loop {
        clear_screen();
        header("Apagar checkpoints");
        let action = dialoguer::Select::with_theme(&THEME)
            .items(&["Selecionar", "Apagar todos"])
            .default(0)
            .clear(true)
            .report(false)
            .interact_opt()
            .context("seleção cancelada")?;

        match action {
            Some(0) => match select_delete_targets_manually(groups, kernel_labels)? {
                Some(targets) => return Ok(Some(targets)),
                None => continue,
            },
            // "Apagar todos" só seleciona; a revisão única em select_delete_plan
            // já cobre a confirmação — sem segunda pergunta redundante.
            Some(1) => return Ok(Some((0..groups.len()).collect())),
            _ => return Ok(None),
        }
    }
}

fn select_delete_targets_manually(
    groups: &[Group],
    kernel_labels: &HashMap<GroupId, String>,
) -> Result<Option<Vec<usize>>> {
    let columns = CheckpointColumns::new(
        groups,
        kernel_labels,
        NAME_HEADER.len(),
        NAME_COL_MAX,
        KERNEL_HEADER.len(),
    );

    let mut items: Vec<String> = Vec::new();
    for g in groups {
        let text = picker_row(g, kernel_labels, &columns, None, PickerTail::MembersAndId);
        items.push(truncate_for_terminal(&text, crate::ui::term::MULTI_MARKER));
    }
    let columns_header = picker_header(&columns, None, PickerTail::MembersAndId);
    let prompt = picker_prompt(
        &prompt_hint("Selecione os checkpoints para apagar", HINT_MULTI),
        &columns_header,
        crate::ui::term::MULTI_MARKER,
    );

    clear_screen();
    header("Apagar checkpoints");
    let Some(selections) = dialoguer::MultiSelect::with_theme(&THEME)
        .with_prompt(prompt)
        .items(&items)
        .clear(true)
        .report(false)
        .interact_opt()
        .context("seleção cancelada")?
    else {
        return Ok(None);
    };

    if selections.is_empty() {
        println!("nenhum checkpoint selecionado");
        return Ok(None);
    }

    Ok(Some(selections))
}

fn confirm_delete_targets(
    targets: &[&Group],
    kernel_labels: &HashMap<GroupId, String>,
    purge: bool,
) -> Result<ReviewDecision> {
    let (prompt, hint) = if purge {
        ("Apagar permanentemente?", "ignora a lixeira")
    } else {
        ("Mover para a lixeira?", "recuperável em snapg trash")
    };
    review_irreversible(targets, kernel_labels, "Apagar checkpoints", prompt, hint)
}

pub(crate) fn print_delete_cancelled() {
    println!("cancelado");
}

pub(crate) fn print_delete_done(g: &Group) {
    println!(
        "{} grupo {} apagado ({} membros)",
        style("✓").green().bold(),
        g.id,
        g.members.len()
    );
}

pub(crate) fn print_trash_done(n: usize) {
    println!(
        "{} {}",
        style("✓").green().bold(),
        count_text(n, "checkpoint movido para a lixeira", "checkpoints movidos para a lixeira"),
    );
}

pub(crate) fn print_trash_member_failed(config: &str, number: u32, e: &anyhow::Error) {
    eprintln!(
        "{} falha ao marcar {config} #{number} como lixeira: {e:#}",
        style("!").yellow().bold()
    );
}

pub(crate) fn print_cleanup_done(n: usize) {
    println!(
        "{} cleanup: {}",
        style("✓").green().bold(),
        count_text(n, "grupo antigo movido para a lixeira", "grupos antigos movidos para a lixeira"),
    );
}

pub(crate) fn print_cleanup_failed(e: &anyhow::Error) {
    eprintln!("{} cleanup falhou: {e:#}", style("!").yellow().bold());
}

pub(crate) fn print_purge_done(n: usize) {
    println!(
        "{} lixeira: {}",
        style("✓").green().bold(),
        count_text(n, "grupo expirado apagado", "grupos expirados apagados"),
    );
}

pub(crate) fn print_purge_failed(e: &anyhow::Error) {
    eprintln!("{} purge da lixeira falhou: {e:#}", style("!").yellow().bold());
}

pub(crate) fn print_purge_member_failed(id: GroupId, e: &anyhow::Error) {
    eprintln!("{} falha ao purgar grupo {id}: {e:#}", style("!").yellow().bold());
}

pub(crate) fn print_trash_empty() {
    println!("lixeira vazia");
}

pub(crate) fn print_trash_cancelled() {
    println!("cancelado");
}

pub(crate) fn print_untrash_done(n: usize) {
    println!(
        "{} {}",
        style("✓").green().bold(),
        count_text(n, "grupo restaurado da lixeira", "grupos restaurados da lixeira"),
    );
}

fn count_text(n: usize, singular: &str, plural: &str) -> String {
    let word = if n == 1 { singular } else { plural };
    format!("{n} {word}")
}

pub(crate) fn print_untrash_member_failed(config: &str, number: u32, e: &anyhow::Error) {
    eprintln!(
        "{} falha ao restaurar {config} #{number} da lixeira: {e:#}",
        style("!").yellow().bold()
    );
}

pub(crate) fn print_no_groups() {
    println!("nenhum grupo snapg save encontrado");
}

const ID_COL: usize = 10;
const DATE_HEADER: &str = "Data";
const MEMBERS_HEADER: &str = "Membros";
const DATE_COL: usize = 16;
/// Limite da coluna de nome no `list`: respeita o maior nome comum, mas impede
/// uma descrição muito longa de empurrar kernel/data para fora da tela.
const NAME_COL_MAX: usize = 36;

pub(crate) fn print_groups(
    groups: &[Group],
    kernel_labels: &HashMap<GroupId, String>,
    show_app_header: bool,
) -> Result<()> {
    if show_app_header {
        header("Checkpoints");
    } else {
        section_header("▪", "Checkpoints");
    }
    let mut rows = Vec::with_capacity(groups.len());
    for g in groups {
        let mut mountpoints = Vec::with_capacity(g.members.len());
        for m in &g.members {
            mountpoints.push(snapper::config_subvolume(&m.config)?);
        }
        rows.push((g, member_badges(&mut mountpoints)));
    }

    let columns = CheckpointColumns::new(
        groups,
        kernel_labels,
        NAME_HEADER.len(),
        NAME_COL_MAX,
        KERNEL_HEADER.len(),
    );
    let kernel_col = columns.kernel;
    let members_col = rows
        .iter()
        .map(|(_, members)| members.chars().count())
        .max()
        .unwrap_or(MEMBERS_HEADER.len())
        .max(MEMBERS_HEADER.len());
    let name_col = columns.name.min(list_name_col_for_terminal(kernel_col, members_col));

    line(format_args!(
        "{}   {}   {}   {}   {}",
        style(format!("{:<ID_COL$}", "ID")).bold(),
        style(format!("{:<name_col$}", NAME_HEADER)).bold(),
        style(format!("{:<kernel_col$}", KERNEL_HEADER)).bold(),
        style(format!("{:<DATE_COL$}", DATE_HEADER)).bold(),
        style(MEMBERS_HEADER).bold()
    ));

    for (g, members) in rows {
        let desc_cell = name_cell(g, name_col);
        let kernel = kernel_label(kernel_labels, g.id);
        line(format_args!(
            "{}   {}   {:<kernel_col$}   {:<DATE_COL$}   {}",
            style(format!("{:>ID_COL$}", g.id)).dim(),
            desc_cell,
            style(kernel).dim(),
            style(short_datetime(group::date(g))).dim(),
            style(members).dim(),
        ));
    }
    Ok(())
}

fn list_name_col_for_terminal(kernel_col: usize, members_col: usize) -> usize {
    let width = console::Term::stdout().size().1 as usize;
    let fixed = ID_COL + kernel_col + DATE_COL + members_col + (4 * 3);
    width
        .saturating_sub(crate::ui::term::CONTENT_INDENT.chars().count())
        .saturating_sub(fixed)
        .max(NAME_HEADER.len())
}

fn member_badges(mountpoints: &mut [String]) -> String {
    mountpoints.sort_by(|a, b| match (a.as_str(), b.as_str()) {
        ("/", "/") => std::cmp::Ordering::Equal,
        ("/", _) => std::cmp::Ordering::Less,
        (_, "/") => std::cmp::Ordering::Greater,
        _ => a.cmp(b),
    });
    mountpoints
        .iter()
        .map(|m| format!("[{m}]"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn print_pending_restore_status() {
    app_header();
    section_header("⏳", "Restauração pendente de reboot");
    line(format_args!(
        "Rode 'snapg restore' para concluir (reiniciar) ou cancelar a restauração."
    ));
    println!();
}

pub(crate) fn print_regret_status(creation_time: &str, kernel: &str, saved: bool) {
    app_header();
    let title = if saved { "Regret ativo guardado" } else { "Regret ativo" };
    section_header("↺", title);
    let kernel_col = kernel.chars().count().max(KERNEL_HEADER.len());
    line(format_args!(
        "{}   {}   {}",
        style(format!("{:<kernel_col$}", KERNEL_HEADER)).bold(),
        style(format!("{:<DATE_COL$}", DATE_HEADER)).bold(),
        style("Ação").bold()
    ));
    line(format_args!(
        "{}   {}   {}",
        style(format!("{:<kernel_col$}", kernel)).dim(),
        style(format!("{:<DATE_COL$}", short_datetime(creation_time))).dim(),
        style("snapg restore").dim()
    ));
    println!();
}
