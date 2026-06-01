use crate::group::{self, Group};
use crate::snapper;
use crate::ui::term::{
    AltScreen, HINT_BACK, HINT_MULTI, THEME, branch, clear_screen, confirm, header, short_datetime,
    stem, truncate_for_terminal,
};
use anyhow::{Context, Result};
use console::style;

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
    // MultiSelect prefix: "> [ ] " = 6 chars
    let prefix_len = 6;
    let mut items: Vec<String> = Vec::new();
    for g in groups {
        let text = format!(
            "checkpoint {}  ·  {}  ·  {} membros  ·  {}",
            g.id,
            short_datetime(group::date(g)),
            g.members.len(),
            group::description(g)
        );
        items.push(truncate_for_terminal(&text, prefix_len));
    }

    clear_screen();
    header("Apagar checkpoints");
    let Some(selections) = dialoguer::MultiSelect::with_theme(&THEME)
        .with_prompt(format!("Selecione os checkpoints para apagar  {HINT_MULTI}"))
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
            branch(i + 1 == total),
            style(g.id).dim(),
            style("·").dim(),
            short_datetime(group::date(g)),
            style("·").dim(),
            group::description(g),
            g.members.len()
        );
    }

    let Some(choice) = dialoguer::Select::with_theme(&THEME)
        .with_prompt(format!("Apagar estes checkpoints?  {HINT_BACK}"))
        .items(&["Confirmar", "Cancelar"])
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

pub(crate) fn print_groups(groups: &[Group]) -> Result<()> {
    header("Checkpoints");
    let gtotal = groups.len();
    for (gi, g) in groups.iter().enumerate() {
        let glast = gi + 1 == gtotal;
        println!(
            "{} {}  {}  {}  {}  {}  {}  {} membros",
            branch(glast),
            style(g.id).dim(),
            style("·").dim(),
            short_datetime(group::date(g)),
            style("·").dim(),
            group::description(g),
            style("·").dim(),
            g.members.len()
        );
        let mtotal = g.members.len();
        for (mi, m) in g.members.iter().enumerate() {
            let mountpoint = snapper::config_subvolume(&m.config)?;
            println!(
                "{}{} {:<10} {:<8} #{}",
                stem(glast),
                branch(mi + 1 == mtotal),
                m.config,
                mountpoint,
                m.snapshot.number
            );
        }
    }
    Ok(())
}

pub(crate) fn print_regret_status(creation_time: &str) {
    println!();
    println!(
        "{} Regret ativo ({}) — use 'snapg restore' para restaurar",
        style("⚠").yellow().bold(),
        short_datetime(creation_time)
    );
}
