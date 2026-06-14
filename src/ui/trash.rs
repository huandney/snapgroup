use crate::group::{self, Group};
use crate::ui::term::{
    AltScreen, HINT_MULTI, MULTI_MARKER, THEME, clear_screen, confirm, header, prompt_hint,
    short_datetime, truncate_for_terminal,
};
use anyhow::{Context, Result};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) enum TrashAction {
    Restore(Vec<usize>),
    Purge(Vec<usize>),
}

/// Wizard da lixeira: escolhe a ação (restaurar / apagar de vez) e os grupos.
/// `prune_days` alimenta a coluna "purga em ~Nd". `None` = cancelado/sem seleção.
pub(crate) fn select_trash_action(
    groups: &[Group],
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
                if let Some(sel) = select_groups(groups, prune_days, "Selecione para restaurar")? {
                    return Ok(Some(TrashAction::Restore(sel)));
                }
            }
            Some(1) => {
                let Some(sel) = select_groups(groups, prune_days, "Selecione para apagar de vez")?
                else {
                    continue;
                };
                clear_screen();
                header("Lixeira");
                if confirm("Apagar permanentemente os selecionados? Não dá pra desfazer.")? {
                    return Ok(Some(TrashAction::Purge(sel)));
                }
            }
            _ => return Ok(None),
        }
    }
}

fn select_groups(groups: &[Group], prune_days: u64, prompt: &str) -> Result<Option<Vec<usize>>> {
    let items: Vec<String> = groups.iter().map(|g| trash_row(g, prune_days)).collect();
    clear_screen();
    header("Lixeira");
    let Some(sel) = dialoguer::MultiSelect::with_theme(&THEME)
        .with_prompt(prompt_hint(prompt, HINT_MULTI))
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

fn trash_row(g: &Group, prune_days: u64) -> String {
    let desc = group::description(g);
    let name = if desc.is_empty() {
        format!("#{}", g.id)
    } else {
        desc.to_string()
    };
    let text = format!(
        "{name}   {}   {}   {} membros",
        short_datetime(group::date(g)),
        purge_due_label(g, prune_days),
        g.members.len()
    );
    truncate_for_terminal(&text, MULTI_MARKER)
}

/// Quanto falta pro purge automático: `prune_days` menos os dias já passados
/// desde a marca de trash. <= 0 → vencido (sai no próximo save/delete).
fn purge_due_label(g: &Group, prune_days: u64) -> String {
    let Some(marked) = group::trash_epoch(g) else {
        return "purga: ?".to_string();
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(marked);
    let left = prune_days as i64 - (now - marked) / 86_400;
    if left <= 0 {
        return "purga: vencida".to_string();
    }
    format!("purga em ~{left}d")
}
