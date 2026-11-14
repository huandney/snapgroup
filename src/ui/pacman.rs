//! Telas do que o pacman fez: o detalhe de uma transação (`snapg log`, pelo
//! pacman.log) e a prévia do restore (pelos bancos de pacotes do snapshot e
//! do sistema atual).

use crate::group::{self, Group};
use crate::pacman_db::{self, NetChange};
use crate::pacman_log::{self, Change, Op, State, Transaction};
use crate::{rollback, snapper};
use std::path::Path;
use crate::ui::checkpoints::picker_prompt;
use crate::ui::snapshots::{count_text, member_badges};
use crate::pacman_hooks::Status;
use crate::ui::term::{
    AltScreen, SELECT_MARKER, THEME, app_header, clear_screen, header, line, note, path,
    picker_page_len, result, result_section, short_datetime, title, truncate_for_terminal, warn,
};
use anyhow::{Context, Result};
use console::{Key, style};

/// Uma linha das listas de pacotes. `marker` já vem colorido.
struct Row {
    marker: String,
    name: String,
    left: String,
    right: String,
}

pub(crate) enum PreviewFlow {
    Continue,
    Back,
}

/// Linhas acima da lista no detalhe sem avisos nem arquivos: cabeçalho de 2,
/// nome, data, branco, contagens, branco.
const DETAIL_INTRO_LINES: usize = 7;
/// Cabeçalho da lista, branco e rodapé abaixo dela, mais o cursor.
const LIST_CHROME_LINES: usize = 4;
const NAME_COL_MAX: usize = 32;

/// `snapg log`: escolhe um checkpoint do pacman e mostra a transação dele.
/// Esc no detalhe volta para a lista; Esc na lista sai.
pub(crate) fn browse(groups: &[Group]) -> Result<()> {
    let _alt = AltScreen::enter();
    let transactions: Vec<Option<Vec<Transaction>>> = groups.iter().map(pacman_log::of_group).collect();

    let name_col = groups
        .iter()
        .map(|g| group::description(g).chars().count())
        .max()
        .unwrap_or(4)
        .clamp(4, NAME_COL_MAX);
    let header_row = format!("{:<name_col$}   {:<7}   Data", "Nome", "Pacotes");
    let items: Vec<String> = groups
        .iter()
        .zip(&transactions)
        .map(|(g, tx)| {
            let count = tx
                .as_ref()
                .map(|txs| format!("↑{}", txs.iter().map(|t| t.changes.len()).sum::<usize>()))
                .unwrap_or("?".into());
            let name = crate::ui::term::ellipsize(group::description(g), name_col);
            let text = format!("{name:<name_col$}   {count:<7}   {}", short_datetime(group::date(g)));
            truncate_for_terminal(&text, SELECT_MARKER)
        })
        .collect();

    let mut selected = 0;
    loop {
        clear_screen();
        header("Checkpoints do pacman");
        let Some(choice) = dialoguer::Select::with_theme(&THEME)
            .with_prompt(picker_prompt("Escolha a transação", &header_row, SELECT_MARKER))
            .items(&items)
            .max_length(picker_page_len(4))
            .default(selected)
            .clear(true)
            .report(false)
            .interact_opt()
            .context("escolher transação")?
        else {
            return Ok(());
        };
        selected = choice;
        show_transaction(&groups[choice], transactions[choice].as_deref())?;
    }
}

/// `snapg pacman`: quem faz os checkpoints agora.
pub(crate) fn print_hook_status(status: &Status) {
    header("Checkpoints do pacman");
    match (status.snapg_runs(), status.snap_pac_runs()) {
        (true, false) => {
            line(format_args!("o snapg faz os checkpoints: um grupo por transação"));
            note(format_args!("para voltar ao snap-pac: snapg pacman off"));
        }
        (false, true) => {
            line(format_args!("o snap-pac faz os checkpoints (padrão do sistema)"));
            note(format_args!("para o snapg fazê-los: snapg pacman on"));
        }
        (true, true) => {
            warn(format_args!("snapg e snap-pac ligados: o snapg recua para não dobrar os snapshots"));
            note(format_args!("escolha um: snapg pacman on (snapg) ou off (snap-pac)"));
        }
        (false, false) => {
            warn(format_args!("nenhum dos dois faz checkpoints nas transações do pacman"));
            note(format_args!("para o snapg fazê-los: snapg pacman on"));
        }
    }
    if status.snap_pac.is_empty() {
        note(format_args!("snap-pac não instalado"));
    }
}

/// O resultado vem do estado depois da troca, não do que foi pedido.
pub(crate) fn print_hook_switched(on: bool, status: &Status) {
    result_section("Pacman");
    let snap_pac = match (status.snap_pac.is_empty(), status.snap_pac_runs()) {
        (true, _) => "snap-pac não instalado",
        (false, true) => "snap-pac ligado",
        (false, false) => "snap-pac desligado",
    };
    let snapg = if status.snapg_runs() { "snapg ligado" } else { "snapg desligado" };
    result(format_args!("{} · {}", style(snapg).bold(), style(snap_pac).dim()));
    if on && !status.snap_pac.is_empty() {
        note(format_args!(
            "os pre/post antigos do snap-pac não saem sozinhos: veja \"Migração do snap-pac\" no README"
        ));
    }
    if !on && status.snap_pac.is_empty() {
        warn(format_args!("sem snap-pac, as transações do pacman ficam sem checkpoint"));
    }
}

/// Saída do hook na transação do pacman: a marca e a versão abrem o bloco,
/// para o checkpoint se destacar entre as linhas dos outros hooks.
pub(crate) fn print_hook_saved(id: i64, desc: &str, mountpoints: &mut [String]) {
    app_header();
    result_section("Salvo");
    result(format_args!(
        "{}   {}",
        style(desc).bold(),
        style(format!("{} · #{id}", member_badges(mountpoints))).dim()
    ));
}

/// Transação seguinte da mesma execução do paru/makepkg: nada novo é salvo.
pub(crate) fn print_hook_covered(group: &Group, helper: &str) {
    app_header();
    result_section("Salvo");
    result(format_args!(
        "{}   {}",
        style(group::description(group)).bold(),
        style(format!("#{} · já cobre esta execução do {helper}", group.id)).dim()
    ));
}

/// Aviso do hook quando o snap-pac também está ligado.
pub(crate) fn print_snap_pac_active() {
    warn(format_args!(
        "snap-pac ligado: checkpoint do snapg não criado. Para o snapg assumir: snapg pacman on"
    ));
}

pub(crate) fn print_no_pacman_groups() {
    note(format_args!("nenhum checkpoint do pacman: eles aparecem a partir da próxima transação"));
}

/// Detalhe de um checkpoint do pacman: as transações que ele cobre (uma, ou
/// as de uma execução do paru), em blocos por fonte. O que vem pronto do
/// repositório fica separado do que foi compilado (o `-U` de um arquivo).
fn show_transaction(group: &Group, transactions: Option<&[Transaction]>) -> Result<()> {
    let members = members_of(group)?;
    let txs = transactions.unwrap_or_default();
    let notes = txs.iter().filter(|t| t.state != State::Completed).count();
    let config_files: Vec<&String> = txs.iter().flat_map(|t| &t.config_files).collect();
    let intro_lines = DETAIL_INTRO_LINES + notes + config_files.len();
    let title_text = if txs.len() > 1 { "Atualização" } else { "Transação do pacman" };
    let intro = || {
        header(title_text);
        let name = crate::ui::term::ellipsize(group::description(group), crate::ui::term::content_width());
        line(format_args!("{}", style(name).bold()));
        line(format_args!(
            "{}{}",
            style(format!("{} · checkpoint {} · ", short_datetime(group::date(group)), group.id)).dim(),
            path(&members)
        ));
        println!();
        if transactions.is_none() {
            line(format_args!("{}", style("transação não encontrada no pacman.log").dim()));
            println!();
            return;
        }
        let changes: Vec<Change> = txs.iter().flat_map(|t| t.changes.clone()).collect();
        let runs = if txs.len() > 1 { format!(" · {} transações", txs.len()) } else { String::new() };
        line(format_args!("{}{}", counts_line(&changes), style(runs).dim()));
        for tx in txs {
            match tx.state {
                State::Completed => {}
                State::Interrupted => line(format_args!(
                    "{}",
                    style("transação interrompida: o log não registra o fim").yellow()
                )),
                State::NotStarted => line(format_args!(
                    "{}",
                    style("transação abortada antes de mexer em pacotes").dim()
                )),
            }
        }
        for file in &config_files {
            line(format_args!("{} {}", style("arquivo novo").dim(), path(file)));
        }
        println!();
    };
    let body = source_sections(group, txs);
    page_lines(&intro, intro_lines, &body)
}

/// Blocos "Repositório" e "Compilados", cada um com título e as linhas dos
/// pacotes, com as colunas alinhadas entre os dois.
fn source_sections(group: &Group, txs: &[Transaction]) -> Vec<String> {
    let (local, repo): (Vec<&Transaction>, Vec<&Transaction>) = txs.iter().partition(|t| t.op == Some("-U"));
    let compiled = match group::pacman_helper(group) {
        Some("makepkg") => "Compilados (makepkg)",
        Some(_) => "Compilados (AUR)",
        None => "Instalados de arquivo",
    };
    let sections = [("Repositório", repo), (compiled, local)];
    let rows: Vec<(usize, Vec<Row>)> = sections
        .iter()
        .enumerate()
        .map(|(i, (_, txs))| (i, change_rows(&txs.iter().flat_map(|t| t.changes.clone()).collect::<Vec<_>>())))
        .collect();
    let widths = column_widths(rows.iter().flat_map(|(_, r)| r), "Antes");
    let mut body = vec![head_line(widths, ("Antes", "Depois"))];
    for (i, section_rows) in &rows {
        let (name, section_txs) = &sections[*i];
        if section_txs.is_empty() {
            continue;
        }
        if body.len() > 1 {
            body.push(String::new());
        }
        let packages = count_text(section_rows.len(), "pacote", "pacotes");
        let summary = match section_txs.as_slice() {
            [only] => match only.op {
                Some(op) => format!("pacman {op} · {packages}"),
                None => packages,
            },
            many => format!("{} transações · {packages}", many.len()),
        };
        body.push(format!("{}{}", title(name), style(format!("  {summary}")).dim()));
        body.extend(section_rows.iter().map(|row| row_line(row, widths)));
    }
    body
}

/// Prévia 3A, entre o seletor e a escolha de membros: compara o banco de
/// pacotes do snapshot do root com o do sistema atual. Vale para qualquer
/// checkpoint com o root, inclusive os anteriores ao hook. Sem o root, sem
/// banco legível ou sem diferença, não há o que mostrar e o fluxo segue.
pub(crate) fn review_package_restore(group: &Group, kernel: &str, toplevel: &Path) -> Result<PreviewFlow> {
    let Some(root) = group::root_member(group)? else {
        return Ok(PreviewFlow::Continue);
    };
    let Ok(snapshot) = rollback::member_snapshot_path(root, toplevel) else {
        return Ok(PreviewFlow::Continue);
    };
    let Ok(nets) = pacman_db::changes_since(&snapshot) else {
        return Ok(PreviewFlow::Continue);
    };
    if nets.is_empty() {
        return Ok(PreviewFlow::Continue);
    }
    loop {
        render_preview(group, kernel, &nets);
        match console::Term::stdout().read_key().context("aguardar tecla")? {
            Key::Char('d') => {
                let intro = || {
                    header("Pacotes que o restore troca");
                    line(format_args!(
                        "{}",
                        style(format!("checkpoint {} · {}", group.id, short_datetime(group::date(group)))).dim()
                    ));
                    println!();
                };
                let rows = net_rows(&nets);
                let widths = column_widths(rows.iter(), "Agora");
                let mut body = vec![head_line(widths, ("Agora", "Depois do restore"))];
                body.extend(rows.iter().map(|row| row_line(row, widths)));
                page_lines(&intro, 4, &body)?;
            }
            Key::Enter => return Ok(PreviewFlow::Continue),
            Key::Escape => return Ok(PreviewFlow::Back),
            _ => {}
        }
    }
}

fn render_preview(group: &Group, kernel: &str, nets: &[NetChange]) {
    clear_screen();
    header("Restaurar checkpoint");
    line(format_args!(
        "{}",
        style(format!("checkpoint {} · {}", group.id, short_datetime(group::date(group)))).dim()
    ));
    line(format_args!(
        "{}{}",
        style(group::description(group)).bold(),
        style(format!(" · kernel {kernel}")).dim()
    ));
    println!();
    let changed = count_text(nets.len(), "pacote", "pacotes");
    line(format_args!("{}", style(format!("Restaurar o / muda {changed} em relação ao sistema atual")).bold()));
    line(format_args!("{}", style(net_summary(nets)).dim()));

    let boot: Vec<&NetChange> = nets
        .iter()
        .filter(|n| pacman_log::is_boot_package(&n.name) && n.name != "snapgroup")
        .collect();
    let snapg = nets.iter().find(|n| n.name == "snapgroup");
    if !boot.is_empty() || snapg.is_some() {
        println!();
        line(format_args!("{}", title("Boot e sistema")));
        let grouped = group_by_versions(&boot);
        let name_col = grouped
            .iter()
            .map(|(label, _)| label.chars().count())
            .chain(snapg.map(|_| "snapgroup".len()))
            .max()
            .unwrap_or(0)
            .min(NAME_COL_MAX);
        // A seção não empurra o rodapé (as teclas) para fora da tela: o que
        // não cabe fica para a lista do `d`.
        let snapg_lines = if snapg.is_some() { 2 } else { 0 };
        let rows_on_screen = console::Term::stdout().size().0 as usize;
        let fit = boot_rows_that_fit(rows_on_screen, snapg_lines, grouped.len());
        for (label, net) in grouped.iter().take(fit) {
            line(format_args!(
                "  {:<name_col$}   {} → {}",
                crate::ui::term::ellipsize(label, name_col),
                style(version(&net.now)).dim(),
                version(&net.before)
            ));
        }
        if fit < grouped.len() {
            let hidden = count_text(grouped.len() - fit, "linha", "linhas");
            line(format_args!("  {}", style(format!("+{hidden} de boot na lista do d")).dim()));
        }
        if let Some(net) = snapg {
            line(format_args!(
                "{} {}   {} → {}",
                style("!").yellow().bold(),
                style(format!("{:<name_col$}", "snapgroup")).yellow(),
                style(version(&net.now)).dim(),
                style(version(&net.before)).yellow()
            ));
            let effect = if net.before.is_none() {
                "o snapg sai do sistema após o reboot"
            } else {
                "o próprio snapg volta de versão após o reboot"
            };
            line(format_args!("  {}", style(effect).dim()));
        }
    }
    println!();
    line(format_args!(
        "{}",
        style(format!("d lista os {} · enter continua · esc volta", nets.len())).dim()
    ));
}

/// Linhas fixas da prévia fora da seção de boot: cabeçalho (2), checkpoint,
/// nome, branco, título, resumo, branco, "Boot e sistema", branco, rodapé e o
/// cursor.
const PREVIEW_FIXED_LINES: usize = 12;

/// Quantas linhas de boot cabem; se nem todas, uma fica para o "+N".
fn boot_rows_that_fit(rows_on_screen: usize, snapg_lines: usize, boot_rows: usize) -> usize {
    let room = rows_on_screen.saturating_sub(PREVIEW_FIXED_LINES + snapg_lines).max(2);
    if boot_rows <= room {
        return boot_rows;
    }
    room - 1
}

/// "153 pacotes voltam de versão · 2 instalados depois saem".
fn net_summary(nets: &[NetChange]) -> String {
    let back = nets.iter().filter(|n| n.before.is_some() && n.now.is_some()).count();
    let leave = nets.iter().filter(|n| n.before.is_none()).count();
    let restored = nets.iter().filter(|n| n.now.is_none()).count();
    let mut parts = Vec::new();
    if back > 0 {
        parts.push(count_text(back, "pacote volta de versão", "pacotes voltam de versão"));
    }
    if leave > 0 {
        parts.push(count_text(leave, "instalado depois sai", "instalados depois saem"));
    }
    if restored > 0 {
        parts.push(count_text(restored, "removido depois volta", "removidos depois voltam"));
    }
    parts.join(" · ")
}

/// Pacotes com a mesma troca de versão numa linha: `systemd, systemd-libs +3`.
fn group_by_versions<'a>(nets: &[&'a NetChange]) -> Vec<(String, &'a NetChange)> {
    let mut groups: Vec<(Vec<&str>, &NetChange)> = Vec::new();
    for net in nets {
        let same = groups
            .iter_mut()
            .find(|(_, first)| first.before == net.before && first.now == net.now);
        match same {
            Some((names, _)) => names.push(&net.name),
            None => groups.push((vec![&net.name], net)),
        }
    }
    groups
        .into_iter()
        .map(|(names, net)| {
            let shown = names[..names.len().min(2)]
                .iter()
                .enumerate()
                .map(|(i, name)| if i == 0 { name.to_string() } else { shorten_after(names[0], name) })
                .collect::<Vec<_>>()
                .join(", ");
            let label = match names.len() {
                0..=2 => shown,
                n => format!("{shown} +{}", n - 2),
            };
            (label, net)
        })
        .collect()
}

/// O que `name` repete de `first` até um hífen sai: `linux-cachyos-headers`
/// depois de `linux-cachyos` vira `-headers`; `nvidia-580xx-utils` depois de
/// `nvidia-580xx-dkms`, `-utils`.
fn shorten_after(first: &str, name: &str) -> String {
    let common = first.bytes().zip(name.bytes()).take_while(|(a, b)| a == b).count();
    if common == first.len() && name[common..].starts_with('-') {
        return name[common..].to_string();
    }
    match name[..common].rfind('-') {
        Some(cut) if cut > 0 => name[cut..].to_string(),
        _ => name.to_string(),
    }
}


fn counts_line(changes: &[Change]) -> String {
    let count = |op: Op| changes.iter().filter(|c| c.op == op).count();
    let upgraded = count_text(count(Op::Upgraded), "atualizado", "atualizados");
    let mut rest = vec![
        count_text(count(Op::Installed), "instalado", "instalados"),
        count_text(count(Op::Removed), "removido", "removidos"),
    ];
    if count(Op::Downgraded) > 0 {
        rest.push(count_text(count(Op::Downgraded), "rebaixado", "rebaixados"));
    }
    if count(Op::Reinstalled) > 0 {
        rest.push(count_text(count(Op::Reinstalled), "reinstalado", "reinstalados"));
    }
    format!("{}{}", style(upgraded).green(), style(format!(" · {}", rest.join(" · "))).dim())
}

fn change_rows(changes: &[Change]) -> Vec<Row> {
    let mut sorted: Vec<&Change> = changes.iter().collect();
    sorted.sort_by(|a, b| boot_first(&a.name, &b.name));
    sorted
        .into_iter()
        .map(|c| Row {
            marker: change_marker(c),
            name: c.name.clone(),
            left: version(&c.old),
            right: version(&c.new),
        })
        .collect()
}

/// Na prévia do restore a leitura é "agora → depois do restore".
fn net_rows(nets: &[NetChange]) -> Vec<Row> {
    let mut sorted: Vec<&NetChange> = nets.iter().collect();
    sorted.sort_by(|a, b| boot_first(&a.name, &b.name));
    sorted
        .into_iter()
        .map(|n| {
            let marker = match (&n.before, &n.now) {
                _ if pacman_log::is_boot_package(&n.name) => style("●").yellow().to_string(),
                (None, _) => style("−").red().to_string(),
                (_, None) => style("+").green().to_string(),
                _ => style("↺").dim().to_string(),
            };
            Row { marker, name: n.name.clone(), left: version(&n.now), right: version(&n.before) }
        })
        .collect()
}

fn boot_first(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |name: &str| !pacman_log::is_boot_package(name);
    key(a).cmp(&key(b)).then_with(|| a.cmp(b))
}

fn change_marker(change: &Change) -> String {
    if pacman_log::is_boot_package(&change.name) {
        return style("●").yellow().to_string();
    }
    match change.op {
        Op::Upgraded => style("↑").green().to_string(),
        Op::Downgraded => style("↓").yellow().to_string(),
        Op::Installed => style("+").green().to_string(),
        Op::Removed => style("−").red().to_string(),
        Op::Reinstalled => style("↻").dim().to_string(),
    }
}

fn version(v: &Option<String>) -> String {
    v.clone().unwrap_or_else(|| "–".to_string())
}

fn members_of(group: &Group) -> Result<String> {
    let mut mountpoints = Vec::with_capacity(group.members.len());
    for m in &group.members {
        mountpoints.push(snapper::config_subvolume(&m.config)?);
    }
    Ok(member_badges(&mut mountpoints))
}

/// Larguras das colunas Pacote e da versão da esquerda.
fn column_widths<'a>(rows: impl Iterator<Item = &'a Row> + Clone, left_head: &str) -> (usize, usize) {
    let name = rows
        .clone()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0)
        .clamp("Pacote".len(), NAME_COL_MAX);
    let left = rows.map(|r| r.left.chars().count()).max().unwrap_or(0).max(left_head.len());
    (name, left)
}

fn head_line((name_col, left_col): (usize, usize), heads: (&str, &str)) -> String {
    style(format!("  {:<name_col$}   {:<left_col$}   {}", "Pacote", heads.0, heads.1)).bold().to_string()
}

/// Versão da esquerda apagada: o olho vai para a que fica.
fn row_line(row: &Row, (name_col, left_col): (usize, usize)) -> String {
    let name = crate::ui::term::ellipsize(&row.name, name_col);
    format!(
        "{} {name:<name_col$}   {}   {}",
        row.marker,
        style(format!("{:<left_col$}", row.left)).dim(),
        row.right
    )
}

/// Lista paginada com ←/→; Esc ou Enter volta. A primeira linha do corpo é o
/// cabeçalho das colunas e se repete em toda página.
fn page_lines(intro: &dyn Fn(), intro_lines: usize, body: &[String]) -> Result<()> {
    let (head, rest) = body.split_first().map(|(h, r)| (h.as_str(), r)).unwrap_or(("", &[]));
    let mut page = 0usize;
    loop {
        let rows_on_screen = console::Term::stdout().size().0 as usize;
        let page_size = rows_on_screen.saturating_sub(intro_lines + LIST_CHROME_LINES).max(3);
        let pages = rest.len().div_ceil(page_size).max(1);
        page = page.min(pages - 1);

        clear_screen();
        intro();
        line(format_args!("{head}"));
        if rest.is_empty() {
            line(format_args!("{}", style("  nenhum pacote").dim()));
        }
        for text in rest.iter().skip(page * page_size).take(page_size) {
            line(format_args!("{text}"));
        }
        println!();
        let back = "esc volta";
        let footer = if pages > 1 {
            format!("página {}/{} · ←/→ navega · {back}", page + 1, pages)
        } else {
            back.to_string()
        };
        line(format_args!("{}", style(footer).dim()));

        match console::Term::stdout().read_key().context("aguardar tecla")? {
            Key::ArrowLeft => page = page.saturating_sub(1),
            Key::ArrowRight => page = (page + 1).min(pages - 1),
            Key::Escape | Key::Enter => return Ok(()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{boot_rows_that_fit, shorten_after};

    #[test]
    fn boot_section_leaves_room_for_the_footer() {
        // 30 linhas: 12 fixas + 2 do aviso do snapg deixam 16.
        assert_eq!(boot_rows_that_fit(30, 2, 13), 13);
        assert_eq!(boot_rows_that_fit(30, 2, 40), 15);
        // Terminal minúsculo: ainda mostra uma linha e o "+N".
        assert_eq!(boot_rows_that_fit(10, 0, 5), 1);
    }

    #[test]
    fn repeated_prefixes_are_cut_at_a_hyphen() {
        assert_eq!(shorten_after("linux-cachyos", "linux-cachyos-headers"), "-headers");
        assert_eq!(shorten_after("systemd", "systemd-libs"), "-libs");
        assert_eq!(shorten_after("nvidia-580xx-dkms", "nvidia-580xx-utils"), "-utils");
        assert_eq!(shorten_after("dkms", "glibc"), "glibc");
        assert_eq!(shorten_after("lib32-a", "lib32-b"), "-b");
    }
}
