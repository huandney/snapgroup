use crate::group::{self, Group, GroupId};
use crate::snapper;
use crate::ui::term::{
    AltScreen, CONTENT_INDENT, HINT_MULTI, PAGE_INDENT, THEME, app_header, clear_screen,
    confirm, content_width, ellipsize, header, input_line, line, prompt_hint,
    section_header, short_datetime, truncate_for_terminal,
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
/// são detalhe interno — nenhum outro comando os pede. PAGE_INDENT alinha a
/// linha com os section headers.
pub(crate) fn print_save_created(id: i64, desc: &str, mountpoints: &mut [String]) {
    println!(
        "{PAGE_INDENT}{} {} salvo  {}  {}  {}  {}",
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
pub(crate) fn select_delete_plan(groups: &[Group], purge: bool) -> Result<Option<Vec<usize>>> {
    let _alt = AltScreen::enter();
    // Lixeira é reversível (carência de TRASH_PRUNE_DAYS) → sem tela de
    // confirmação. Purge é permanente → mantém a confirmação.
    if !purge {
        return select_delete_targets(groups, false);
    }
    loop {
        let Some(indices) = select_delete_targets(groups, true)? else {
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

fn select_delete_targets(groups: &[Group], purge: bool) -> Result<Option<Vec<usize>>> {
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
            Some(1) => match select_all_delete_targets(groups, purge)? {
                Some(targets) => return Ok(Some(targets)),
                None => continue,
            },
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

fn select_all_delete_targets(groups: &[Group], purge: bool) -> Result<Option<Vec<usize>>> {
    // Trash de todos é reversível → sem confirmação.
    if !purge {
        return Ok(Some((0..groups.len()).collect()));
    }
    let targets: Vec<&Group> = groups.iter().collect();
    match confirm_delete_targets_with_prompt(&targets, "Apagar permanentemente?", Some("ignora a lixeira"))? {
        DeleteFlow::Proceed => {
            clear_screen();
            header("Apagar checkpoints");
            if confirm("Tem certeza que deseja apagar TODOS permanentemente?")? {
                Ok(Some((0..groups.len()).collect()))
            } else {
                Ok(None)
            }
        }
        DeleteFlow::Back | DeleteFlow::Cancel => Ok(None),
    }
}

/// Decisão na tela de confirmação de exclusão. `Back` = Esc (volta pra seleção,
/// um passo); `Cancel` = "Cancelar" explícito (encerra).
enum DeleteFlow {
    Proceed,
    Back,
    Cancel,
}

fn confirm_delete_targets(targets: &[&Group]) -> Result<DeleteFlow> {
    confirm_delete_targets_with_prompt(targets, "Apagar permanentemente?", Some("ignora a lixeira"))
}

fn confirm_delete_targets_with_prompt(
    targets: &[&Group],
    prompt: &str,
    dim_hint: Option<&str>,
) -> Result<DeleteFlow> {
    let mut yes = false;
    let mut page = 0usize;
    loop {
        let page_size = delete_confirm_page_size();
        let pages = targets.len().div_ceil(page_size).max(1);
        page = page.min(pages - 1);

        render_delete_confirmation(targets, prompt, dim_hint, yes, page, page_size, pages);
        match console::Term::stdout()
            .read_key()
            .context("aguardar confirmação")?
        {
            console::Key::ArrowUp | console::Key::ArrowDown => yes = !yes,
            console::Key::ArrowLeft => page = page.saturating_sub(1),
            console::Key::ArrowRight => page = (page + 1).min(pages - 1),
            console::Key::Enter => {
                return Ok(if yes {
                    DeleteFlow::Proceed
                } else {
                    DeleteFlow::Cancel
                });
            }
            console::Key::Escape => return Ok(DeleteFlow::Back),
            _ => {}
        }
    }
}

fn render_delete_confirmation(
    targets: &[&Group],
    prompt: &str,
    dim_hint: Option<&str>,
    yes: bool,
    page: usize,
    page_size: usize,
    pages: usize,
) {
    clear_screen();
    header("Apagar checkpoints");
    println!();
    match dim_hint {
        Some(hint) => line(format_args!("{} {}", style(prompt).bold(), style(format!("({hint})")).dim())),
        None => line(format_args!("{}", style(prompt).bold())),
    }
    line(format_args!("{} Sim", if yes { ">" } else { " " }));
    line(format_args!("{} Não", if yes { " " } else { ">" }));
    println!();

    let start = page * page_size;
    let end = (start + page_size).min(targets.len());
    for g in &targets[start..end] {
        print_delete_target_card(g);
    }
    if pages > 1 {
        line(format_args!(
            "{}",
            style(format!("página {}/{} · ←/→ navega", page + 1, pages)).dim()
        ));
    }
}

fn delete_confirm_page_size() -> usize {
    let rows = console::Term::stdout().size().0 as usize;
    // Header + prompt/options + paginação ocupam ~8 linhas. Cada item usa
    // 3 linhas (nome, metadados, respiro).
    rows.saturating_sub(8).max(3) / 3
}

fn print_delete_target_card(g: &Group) {
    let desc_width = content_width().min(80);
    let desc = group::description(g).trim();
    let desc = if desc.is_empty() {
        format!("#{}", g.id)
    } else {
        desc.to_string()
    };
    let desc = truncate_chars(&desc, desc_width);
    println!("{CONTENT_INDENT}{desc}");
    println!(
        "{}{} {}",
        CONTENT_INDENT,
        style(format!(
            "└─ {} · {} membros ·",
            short_datetime(group::date(g)),
            g.members.len()
        ))
        .dim(),
        style(g.id).dim()
    );
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
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
        "{} {n} checkpoint(s) movido(s) para a lixeira",
        style("✓").green().bold(),
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
        "{} cleanup: {n} grupo(s) antigo(s) movido(s) para a lixeira",
        style("✓").green().bold(),
    );
}

pub(crate) fn print_cleanup_failed(e: &anyhow::Error) {
    eprintln!("{} cleanup falhou: {e:#}", style("!").yellow().bold());
}

pub(crate) fn print_purge_done(n: usize) {
    println!(
        "{} lixeira: {n} grupo(s) expirado(s) apagado(s)",
        style("✓").green().bold(),
    );
}

pub(crate) fn print_purge_failed(e: &anyhow::Error) {
    eprintln!("{} purge da lixeira falhou: {e:#}", style("!").yellow().bold());
}

pub(crate) fn print_purge_member_failed(id: GroupId, e: &anyhow::Error) {
    eprintln!("{} falha ao purgar grupo {id}: {e:#}", style("!").yellow().bold());
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

pub(crate) fn print_pending_restore_status() {
    app_header();
    section_header("⏳", "Restauração pendente de reboot");
    line(format_args!(
        "Rode 'snapg restore' para concluir (reiniciar) ou cancelar a restauração."
    ));
    println!();
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
