use crate::group::{self, Group, GroupId};
use crate::ui::delete::{self as delete_ui, DeleteContext, Estimate, Mode, PurgeDue};
use crate::ui::term::{AltScreen, THEME, clear_screen, header};
use anyhow::{Context, Result};
use std::collections::{BTreeSet, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) enum TrashAction {
    Restore(Vec<usize>),
    Purge(Vec<usize>),
}

/// Wizard da lixeira: escolhe a ação (restaurar / apagar de vez) e os grupos,
/// na mesma tela do delete. `prune_days` alimenta a coluna "Purga";
/// `estimate` responde quanto a seleção libera ao apagar. `None` = cancelado.
pub(crate) fn select_trash_action(
    groups: &[Group],
    kernel_labels: &HashMap<GroupId, String>,
    prune_days: u64,
    estimate: &mut dyn FnMut(&BTreeSet<usize>, &BTreeSet<usize>) -> Estimate,
) -> Result<Option<TrashAction>> {
    let purge_due: Vec<PurgeDue> = groups.iter().map(|g| purge_due(g, prune_days)).collect();
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
        let mode = match action {
            Some(0) => Mode::TrashRestore,
            Some(1) => Mode::TrashPurge,
            _ => return Ok(None),
        };
        let ctx = DeleteContext { groups, kernel_labels, orphans: &[], mode, purge_due: &purge_due };
        let Some(sel) = delete_ui::select_trash_groups(&ctx, estimate)? else {
            continue;
        };
        if mode == Mode::TrashRestore {
            return Ok(Some(TrashAction::Restore(sel)));
        }
        return Ok(Some(TrashAction::Purge(sel)));
    }
}

/// Quanto falta pro purge automático: `prune_days` menos os dias já passados
/// desde a marca de trash. <= 0 → vencido (sai no próximo save/delete).
fn purge_due(g: &Group, prune_days: u64) -> PurgeDue {
    let Some(marked) = group::trash_epoch(g) else {
        return PurgeDue { label: "?".to_string(), expired: false };
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(marked);
    let left = prune_days as i64 - (now - marked) / 86_400;
    if left <= 0 {
        return PurgeDue { label: "vencida".to_string(), expired: true };
    }
    PurgeDue { label: format!("em ~{left}d"), expired: false }
}
