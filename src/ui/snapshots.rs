use crate::group::{self, Group, GroupId};
use crate::snapper;
use crate::ui::term::{
    AltScreen, HINT_BACK, HINT_MULTI, THEME, app_header, branch, clear_screen, confirm, header,
    line, prompt_bold_hint, prompt_hint, section_header, short_datetime, tree_branch,
    truncate_for_terminal,
};
use anyhow::{Context, Result};
use console::style;
use std::collections::HashMap;

pub(crate) fn print_save_created(id: i64, desc: &str, created: &[(String, u32)]) {
    println!(
        "{} grupo {id} criado ({} membros)  {}  {desc}",
        style("✓").green().bold(),
        created.len(),
        style("·").dim()
    );
    let total = created.len();
    for (i, (cfg, n)) in created.iter().enumerate() {
        println!("{} {:<10} #{n}", branch(i + 1 == total), cfg);
    }
}

/// Roda o wizard de exclusão (modo → seleção → confirmação) no alternate screen
/// e devolve os índices a apagar, ou `None` se cancelado. Esc na confirmação
/// volta pra seleção (um passo). A exclusão em si roda fora, no terminal normal.
pub(crate) fn select_delete_plan(groups: &[Group]) -> Result<Option<Vec<usize>>> {
    let _alt = AltScreen::enter();
    loop {
        let Some(indices) = select_delete_targets(groups)? else {
            return Ok(None);
        };
        let targets: Vec<&Group> = indices.iter().map(|&i| &groups[i]).collect();
        match confirm_delete_targets(&targets)? {
            DeleteFlow::Proceed => return Ok(Some(indices)),
            DeleteFlow::Back => continue,
            DeleteFlow::Cancel => return Ok(None),
        }
    }
}

fn select_delete_targets(groups: &[Group]) -> Result<Option<Vec<usize>>> {
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
            Some(0) => match select_delete_targets_manually(groups)? {
                Some(targets) => return Ok(Some(targets)),
                None => continue,
            },
            Some(1) => return select_all_delete_targets(groups),
            _ => return Ok(None),
        }
    }
}

fn select_delete_targets_manually(groups: &[Group]) -> Result<Option<Vec<usize>>> {
    let name_col = groups
        .iter()
        .map(|g| group::description(g).chars().count())
        .max()
        .unwrap_or(NAME_HEADER.len())
        .max(NAME_HEADER.len())
        .min(NAME_COL_MAX);

    let mut items: Vec<String> = Vec::new();
    for g in groups {
        let desc = group::description(g);
        let desc_cell = if desc.chars().count() > name_col {
            let cut: String = desc.chars().take(name_col - 1).collect();
            format!("{cut}…")
        } else {
            format!("{desc:<name_col$}")
        };
        let text = format!(
            "{desc_cell}   {:<DATE_COL$}   {} membros   #{}",
            short_datetime(group::date(g)),
            g.members.len(),
            g.id
        );
        items.push(truncate_for_terminal(&text, crate::ui::term::MULTI_MARKER));
    }

    clear_screen();
    header("Apagar checkpoints");
    let Some(selections) = dialoguer::MultiSelect::with_theme(&THEME)
        .with_prompt(prompt_hint("Selecione os checkpoints para apagar", HINT_MULTI))
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

fn select_all_delete_targets(groups: &[Group]) -> Result<Option<Vec<usize>>> {
    clear_screen();
    header("Apagar checkpoints");
    if !confirm("Apagar TODOS os checkpoints?")? {
        return Ok(None);
    }
    Ok(Some((0..groups.len()).collect()))
}

/// Decisão na tela de confirmação de exclusão. `Back` = Esc (volta pra seleção,
/// um passo); `Cancel` = "Cancelar" explícito (encerra).
enum DeleteFlow {
    Proceed,
    Back,
    Cancel,
}

fn confirm_delete_targets(targets: &[&Group]) -> Result<DeleteFlow> {
    clear_screen();
    header("Apagar checkpoints");
    let total = targets.len();
    for (i, g) in targets.iter().enumerate() {
        println!(
            "{} {}  {}  {}  {}  {}  {} membros",
            tree_branch(i + 1 == total),
            style(g.id).dim(),
            style("·").dim(),
            short_datetime(group::date(g)),
            style("·").dim(),
            group::description(g),
            g.members.len()
        );
    }

    let Some(choice) = dialoguer::Select::with_theme(&THEME)
        .with_prompt(prompt_bold_hint("Apagar estes checkpoints?", HINT_BACK))
        .items(&["Sim", "Não"])
        .default(1)
        .clear(true)
        .report(false)
        .interact_opt()
        .context("seleção cancelada")?
    else {
        return Ok(DeleteFlow::Back);
    };
    Ok(match choice {
        0 => DeleteFlow::Proceed,
        _ => DeleteFlow::Cancel,
    })
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

pub(crate) fn print_no_groups() {
    println!("nenhum grupo snapg save encontrado");
}

const ID_COL: usize = 10;
const NAME_HEADER: &str = "Nome";
const KERNEL_HEADER: &str = "Kernel";
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

    let kernel_col = groups
        .iter()
        .filter_map(|g| kernel_labels.get(&g.id))
        .map(|k| k.chars().count())
        .max()
        .unwrap_or(KERNEL_HEADER.len())
        .max(KERNEL_HEADER.len());
    let members_col = rows
        .iter()
        .map(|(_, members)| members.chars().count())
        .max()
        .unwrap_or(MEMBERS_HEADER.len())
        .max(MEMBERS_HEADER.len());
    let natural_name_col = groups
        .iter()
        .map(|g| group::description(g).chars().count())
        .max()
        .unwrap_or(NAME_HEADER.len())
        .max(NAME_HEADER.len())
        .min(NAME_COL_MAX);
    let name_col = natural_name_col.min(list_name_col_for_terminal(kernel_col, members_col));

    line(format_args!(
        "{}   {}   {}   {}   {}",
        style(format!("{:<ID_COL$}", "ID")).bold(),
        style(format!("{:<name_col$}", NAME_HEADER)).bold(),
        style(format!("{:<kernel_col$}", KERNEL_HEADER)).bold(),
        style(format!("{:<DATE_COL$}", DATE_HEADER)).bold(),
        style(MEMBERS_HEADER).bold()
    ));

    for (g, members) in rows {
        let desc = group::description(g);
        let desc_cell = if desc.chars().count() > name_col {
            let cut: String = desc.chars().take(name_col - 1).collect();
            format!("{cut}…")
        } else {
            format!("{desc:<name_col$}")
        };
        let kernel = kernel_labels.get(&g.id).map(String::as_str).unwrap_or("?");
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

pub(crate) fn print_regret_status(creation_time: &str, kernel: &str) {
    app_header();
    section_header("↺", "Regret ativo");
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
