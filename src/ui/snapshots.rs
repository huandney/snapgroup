use crate::group::{self, Group};
use crate::snapper;
use crate::ui::term::{
    THEME, branch, clear_screen, confirm, short_datetime, stem, truncate_for_terminal,
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

pub(crate) fn select_delete_targets(groups: &[Group]) -> Result<Option<Vec<usize>>> {
    loop {
        clear_screen();
        let action = dialoguer::Select::with_theme(&THEME)
            .with_prompt("Apagar checkpoints")
            .items(&["Selecionar", "Apagar todos"])
            .default(0)
            .clear(true)
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
    let Some(selections) = dialoguer::MultiSelect::with_theme(&THEME)
        .with_prompt("Selecione checkpoints para apagar (Espaço=marcar, Enter=confirmar)")
        .items(&items)
        .clear(true)
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
    if !confirm("Apagar TODOS os checkpoints?")? {
        return Ok(None);
    }
    Ok(Some((0..groups.len()).collect()))
}

/// Decisão na tela de confirmação de exclusão. `Back` = Esc (volta pra seleção,
/// um passo); `Cancel` = "Cancelar" explícito (encerra).
pub(crate) enum DeleteFlow {
    Proceed,
    Back,
    Cancel,
}

pub(crate) fn confirm_delete_targets(targets: &[&Group]) -> Result<DeleteFlow> {
    clear_screen();
    println!("{}", style("Apagar checkpoints").bold());
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
        .with_prompt("Apagar estes checkpoints?  (esc volta)")
        .items(&["Confirmar", "Cancelar"])
        .default(1)
        .clear(true)
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
    println!("{}", style("Checkpoints").bold());
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
