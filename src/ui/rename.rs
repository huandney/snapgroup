use crate::group::{self, Group, GroupId};
use crate::ui::checkpoints::{
    CheckpointColumns, KERNEL_HEADER, NAME_HEADER, PickerTail, picker_header, picker_prompt,
    picker_row,
};
use crate::ui::term::{
    AltScreen, SELECT_MARKER, THEME, clear_screen, header, input_line, line,
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
    let (items, columns_header) = group_picker_items(groups, kernel_labels);
    let prompt = picker_prompt("Escolha o checkpoint", &columns_header, SELECT_MARKER);
    // Primeira página do fluxo: Esc sai (padrão), sem hint de "volta".
    dialoguer::Select::with_theme(&THEME)
        .with_prompt(prompt)
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
) -> (Vec<String>, String) {
    let columns = CheckpointColumns::new(
        groups,
        kernel_labels,
        NAME_HEADER.len(),
        NAME_COL_MAX,
        KERNEL_HEADER.len(),
    );

    let items = groups
        .iter()
        .map(|g| {
            let text = picker_row(g, kernel_labels, &columns, None, PickerTail::MembersAndId);
            truncate_for_terminal(&text, SELECT_MARKER)
        })
        .collect();
    (
        items,
        picker_header(&columns, None, PickerTail::MembersAndId),
    )
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

const NAME_COL_MAX: usize = 36;
