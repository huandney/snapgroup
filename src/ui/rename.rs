use crate::group::{self, Group, GroupId};
use crate::ui::term::{
    AltScreen, SELECT_MARKER, THEME, clear_screen, header, input_line, line, short_datetime,
    truncate_for_terminal,
};
use anyhow::{Context, Result};
use console::style;
use std::collections::HashMap;

pub(crate) fn select_plan(
    groups: &[Group],
    kernel_labels: &HashMap<GroupId, String>,
) -> Result<Option<(usize, String)>> {
    let _alt = AltScreen::enter();
    loop {
        let Some(index) = select_target(groups, kernel_labels)? else {
            return Ok(None);
        };
        // Esc no nome volta para a seleção de checkpoint, não cancela tudo.
        if let Some(description) =
            prompt_description(&groups[index], "enter confirma · esc volta")?
        {
            return Ok(Some((index, description)));
        }
    }
}

/// Caminho direto (`snapg rename <id>`): mesma tela do interativo, com
/// AltScreen próprio. É a primeira página — Esc sai, sem hint de "volta".
pub(crate) fn prompt_description_screen(group: &Group) -> Result<Option<String>> {
    let _alt = AltScreen::enter();
    prompt_description(group, "enter confirma")
}

fn prompt_description(group: &Group, footer: &str) -> Result<Option<String>> {
    input_line("Novo nome", group::description(group), "", footer, || {
        clear_screen();
        header("Renomear checkpoint");
        line(format_args!("ID atual     #{}", group.id));
        line(format_args!("Nome atual   {}", group::description(group)));
        println!();
    })
}

fn select_target(
    groups: &[Group],
    kernel_labels: &HashMap<GroupId, String>,
) -> Result<Option<usize>> {
    clear_screen();
    header("Renomear checkpoint");
    let items = group_picker_items(groups, kernel_labels);
    // Primeira página do fluxo: Esc sai (padrão), sem hint de "volta".
    dialoguer::Select::with_theme(&THEME)
        .with_prompt("Escolha o checkpoint")
        .items(&items)
        .default(0)
        .clear(true)
        .report(false)
        .interact_opt()
        .context("selecionar checkpoint para renomear")
}

fn group_picker_items(
    groups: &[Group],
    kernel_labels: &HashMap<GroupId, String>,
) -> Vec<String> {
    let name_col = groups
        .iter()
        .map(|g| group::description(g).chars().count())
        .max()
        .unwrap_or(NAME_HEADER.len())
        .max(NAME_HEADER.len())
        .min(NAME_COL_MAX);
    let kernel_col = groups
        .iter()
        .filter_map(|g| kernel_labels.get(&g.id))
        .map(|k| k.chars().count())
        .max()
        .unwrap_or(KERNEL_HEADER.len())
        .max(KERNEL_HEADER.len());

    groups
        .iter()
        .map(|g| {
            let desc = group::description(g);
            let name = if desc.chars().count() > name_col {
                let cut: String = desc.chars().take(name_col - 1).collect();
                format!("{cut}…")
            } else {
                format!("{desc:<name_col$}")
            };
            let kernel = kernel_labels.get(&g.id).map(String::as_str).unwrap_or("?");
            let text = format!(
                "{}   {:<kernel_col$}   {}   {} membros   #{}",
                name,
                kernel,
                short_datetime(group::date(g)),
                g.members.len(),
                g.id
            );
            truncate_for_terminal(&text, SELECT_MARKER)
        })
        .collect()
}

pub(crate) fn print_cancelled() {
    println!("renomeação cancelada");
}

pub(crate) fn print_unchanged(group_id: GroupId) {
    println!("checkpoint #{group_id}: nome mantido");
}

pub(crate) fn print_done(group: &Group, description: &str, expected_members: usize) {
    println!(
        "{} checkpoint #{} renomeado para {} ({} membros)",
        style("✓").green().bold(),
        group.id,
        description,
        group.members.len()
    );
    if group.members.len() < expected_members {
        println!(
            "  checkpoint parcial: {} de {} configs encontradas",
            group.members.len(),
            expected_members
        );
    }
}

pub(crate) fn print_no_groups() {
    println!("nenhum grupo snapg save encontrado");
}

const NAME_HEADER: &str = "Nome";
const KERNEL_HEADER: &str = "Kernel";
const NAME_COL_MAX: usize = 36;
