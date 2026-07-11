use crate::group::{self, Group, GroupId};
use crate::ui::checkpoints::{
    CheckpointColumns, KERNEL_HEADER, NAME_HEADER, PickerTail, ReviewDecision, picker_row,
    picker_header, picker_prompt, review_irreversible,
};
use crate::ui::term::{
    AltScreen, HINT_MULTI, MULTI_MARKER, THEME, clear_screen, header, prompt_hint,
    truncate_for_terminal,
};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) enum TrashAction {
    Restore(Vec<usize>),
    Purge(Vec<usize>),
}

/// Wizard da lixeira: escolhe a ação (restaurar / apagar de vez) e os grupos.
/// `prune_days` alimenta a coluna "purga em ~Nd". `None` = cancelado/sem seleção.
pub(crate) fn select_trash_action(
    groups: &[Group],
    kernel_labels: &HashMap<GroupId, String>,
    prune_days: u64,
) -> Result<Option<TrashAction>> {
    let _alt = AltScreen::enter();
    loop {
        clear_screen();
        header("Lixeira");
        let action = dialoguer::Select::with_theme(&THEME)
            .items(&["Restaurar para o pool", "Apagar de vez"])
            .default(0)
            .clear(true)
            .report(false)
            .interact_opt()
            .context("seleção cancelada")?;

        match action {
            Some(0) => {
                if let Some(sel) =
                    select_groups(groups, kernel_labels, prune_days, "Selecione para restaurar")?
                {
                    return Ok(Some(TrashAction::Restore(sel)));
                }
            }
            Some(1) => {
                loop {
                    let Some(sel) = select_groups(
                        groups,
                        kernel_labels,
                        prune_days,
                        "Selecione para apagar de vez",
                    )?
                    else {
                        break;
                    };
                    let targets: Vec<&Group> = sel.iter().map(|&index| &groups[index]).collect();
                    match review_irreversible(
                        &targets,
                        kernel_labels,
                        "Lixeira",
                        "Apagar permanentemente?",
                        "não dá para desfazer",
                    )? {
                        ReviewDecision::Proceed => return Ok(Some(TrashAction::Purge(sel))),
                        ReviewDecision::Back => continue,
                        ReviewDecision::Cancel => break,
                    }
                }
            }
            _ => return Ok(None),
        }
    }
}

fn select_groups(
    groups: &[Group],
    kernel_labels: &HashMap<GroupId, String>,
    prune_days: u64,
    prompt: &str,
) -> Result<Option<Vec<usize>>> {
    let columns = CheckpointColumns::new(
        groups,
        kernel_labels,
        NAME_HEADER.len(),
        NAME_COL_MAX,
        KERNEL_HEADER.len(),
    );
    let purge_labels: Vec<String> = groups
        .iter()
        .map(|group| purge_due_label(group, prune_days))
        .collect();
    let purge_width = purge_labels
        .iter()
        .map(|label| label.chars().count())
        .max()
        .unwrap_or(PURGE_HEADER.len())
        .max(PURGE_HEADER.len());
    let items: Vec<String> = groups
        .iter()
        .zip(&purge_labels)
        .map(|(group, purge)| trash_row(group, kernel_labels, purge, purge_width, &columns))
        .collect();
    let columns_header = picker_header(
        &columns,
        Some((PURGE_HEADER, purge_width)),
        PickerTail::None,
    );
    let prompt = picker_prompt(
        &prompt_hint(prompt, HINT_MULTI),
        &columns_header,
        MULTI_MARKER,
    );
    clear_screen();
    header("Lixeira");
    let Some(sel) = dialoguer::MultiSelect::with_theme(&THEME)
        .with_prompt(prompt)
        .items(&items)
        .clear(true)
        .report(false)
        .interact_opt()
        .context("seleção cancelada")?
    else {
        return Ok(None);
    };
    if sel.is_empty() {
        return Ok(None);
    }
    Ok(Some(sel))
}

fn trash_row(
    g: &Group,
    kernel_labels: &HashMap<GroupId, String>,
    purge: &str,
    purge_width: usize,
    columns: &CheckpointColumns,
) -> String {
    let text = picker_row(
        g,
        kernel_labels,
        columns,
        Some((purge, purge_width)),
        PickerTail::None,
    );
    truncate_for_terminal(&text, MULTI_MARKER)
}

const NAME_COL_MAX: usize = 36;
const PURGE_HEADER: &str = "Purga";

/// Quanto falta pro purge automático: `prune_days` menos os dias já passados
/// desde a marca de trash. <= 0 → vencido (sai no próximo save/delete).
fn purge_due_label(g: &Group, prune_days: u64) -> String {
    let Some(marked) = group::trash_epoch(g) else {
        return "?".to_string();
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(marked);
    let left = prune_days as i64 - (now - marked) / 86_400;
    if left <= 0 {
        return "vencida".to_string();
    }
    format!("em ~{left}d")
}
