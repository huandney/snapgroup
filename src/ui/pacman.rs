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
    AltScreen, SELECT_MARKER, THEME, clear_screen, header, hook_ok, line, note, path,
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
/// Branco e rodapé abaixo da lista, mais o cursor.
const LIST_FOOTER_LINES: usize = 3;
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

/// Saída do hook na transação do pacman: uma linha sob a do hook, com a marca
/// abrindo para o checkpoint se destacar entre as linhas dos outros hooks.
pub(crate) fn print_hook_saved(id: i64, desc: &str, mountpoints: &mut [String]) {
    hook_ok(format_args!(
        "{}{}",
        style(desc).bold(),
        style(format!(" · {} · #{id}", member_badges(mountpoints))).dim()
    ));
}

/// Transação seguinte da mesma execução do paru/cachy-update: nada novo é salvo.
pub(crate) fn print_hook_covered(group: &Group) {
    hook_ok(format_args!("{}", style(format!("já salvo em #{} · {}", group.id, group::description(group))).dim()));
}

/// Erro que encerra o hook (a trava de outro snapg, uma config que falta):
/// primeira linha como aviso, o resto embaixo dela.
pub(crate) fn print_hook_failed(error: &anyhow::Error) {
    let text = format!("{error:#}");
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if let Some(first) = lines.next() {
        warn(format_args!("{first}"));
    }
    for rest in lines {
        crate::ui::term::warn_detail(format_args!("{rest}"));
    }
}

/// `snapg boot-check`, no fim da transação: o `limine.conf` confere com o
/// que está no /boot? Um hash que não bate faz o Limine recusar a entrada no
/// próximo boot.
pub(crate) fn print_boot_check(report: Option<&crate::boot::HashReport>) {
    let Some(report) = report else {
        hook_ok(format_args!("{}", style("sem limine.conf: nada a conferir").dim()));
        return;
    };
    let Some(first) = report.mismatched.first() else {
        let files = count_text(report.checked, "arquivo", "arquivos");
        hook_ok(format_args!("{}", style(format!("boot conferido · {files} do limine.conf, hashes ok")).dim()));
        return;
    };
    let more = match report.mismatched.len() {
        1 => String::new(),
        n => format!(" (e mais {})", n - 1),
    };
    warn(format_args!("{}", style(format!("o Limine vai recusar {first}{more}: o hash não confere")).yellow()));
    crate::ui::term::warn_detail(format_args!("rode snapg doctor antes de reiniciar"));
}

/// Aviso do hook quando o snap-pac também está ligado.
pub(crate) fn print_snap_pac_active() {
    warn(format_args!("snap-pac ligado: checkpoint não criado"));
    crate::ui::term::warn_detail(format_args!("para o snapg assumir: snapg pacman on"));
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
    let (head, body) = source_sections(group, txs);
    page_lines(&intro, intro_lines, &head, &body)
}

/// Blocos "Repositório" e "Compilados", cada um com título e as linhas dos
/// pacotes, com as colunas alinhadas entre os dois. Devolve o cabeçalho das
/// colunas à parte: ele se repete em toda página.
fn source_sections(group: &Group, txs: &[Transaction]) -> (String, Vec<String>) {
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
    let head = head_line(widths, ("Antes", "Depois"));
    let mut body = Vec::new();
    for (i, section_rows) in &rows {
        let (name, section_txs) = &sections[*i];
        if section_txs.is_empty() {
            continue;
        }
        if !body.is_empty() {
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
    (head, body)
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
                let sections = restore_sections(&nets);
                paged(&intro, 4, &|room| section_pages(&sections, room))?;
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
/// Uma seção da lista do `d`: título, cabeçalho das colunas e os grupos, cada
/// um com a linha dele e os nomes que quebram embaixo.
struct Section {
    title: String,
    head: String,
    units: Vec<Vec<String>>,
}

/// Lista do `d` na prévia do restore: "Repositório" e "Compilados", cada um
/// com o cabeçalho e a largura das colunas dele (uma versão longa do AUR não
/// alarga o resto). Pacotes com a mesma troca de versão viram uma linha com o
/// primeiro nome e `+N`, e os outros nomes quebram embaixo, apagados.
fn restore_sections(nets: &[NetChange]) -> Vec<Section> {
    let (local, repo): (Vec<&NetChange>, Vec<&NetChange>) = nets.iter().partition(|n| n.built_here);
    let names_width = crate::ui::term::content_width().saturating_sub(4).min(GROUP_NAMES_MAX);
    let mut sections = Vec::new();
    for (name, section) in [("Repositório", repo), ("Compilados", local)] {
        if section.is_empty() {
            continue;
        }
        let packages = count_text(section.len(), "pacote", "pacotes");
        let units = same_version_change(&section);
        let name_col = units.iter().map(|u| unit_label(u).chars().count()).max().unwrap_or(0).clamp("Pacote".len(), NAME_COL_MAX);
        let left_col = units.iter().map(|u| version(&u[0].now).chars().count()).max().unwrap_or(0).max("Agora".len());
        let lines = units
            .iter()
            .map(|unit| {
                let mut lines = vec![unit_line(unit, name_col, left_col)];
                if unit.len() > 2 {
                    let others: Vec<&str> = unit[1..].iter().map(|n| n.name.as_str()).collect();
                    let wrapped = crate::ui::term::wrap_text(&others.join(" "), names_width);
                    lines.extend(wrapped.into_iter().map(|t| format!("    {}", style(t).dim())));
                }
                lines
            })
            .collect();
        sections.push(Section {
            title: format!("{}{}", title(name), style(format!("  {packages}")).dim()),
            head: head_line((name_col, left_col), ("Agora", "Depois do restore")),
            units: lines,
        });
    }
    sections
}

/// Páginas de até `room` linhas. Uma seção que continua na página seguinte
/// repete o título e o cabeçalho; um grupo só se parte entre páginas quando
/// não cabe numa página inteira, e um título não fica sozinho no pé da página.
fn section_pages(sections: &[Section], room: usize) -> Vec<Vec<String>> {
    let mut pages = Vec::new();
    let mut page: Vec<String> = Vec::new();
    for section in sections {
        let mut opened = false;
        // Um grupo que não cabe nem numa página vazia segue linha a linha.
        let pieces = section.units.iter().flat_map(|unit| -> Vec<&[String]> {
            if 2 + unit.len() <= room { vec![unit.as_slice()] } else { unit.chunks(1).collect() }
        });
        for unit in pieces {
            let gap = usize::from(!opened && !page.is_empty());
            let header = if opened { 0 } else { 2 };
            if !page.is_empty() && page.len() + gap + header + unit.len() > room {
                pages.push(std::mem::take(&mut page));
                opened = false;
            }
            if !opened {
                if !page.is_empty() {
                    page.push(String::new());
                }
                page.push(section.title.clone());
                page.push(section.head.clone());
                opened = true;
            }
            page.extend(unit.iter().cloned());
        }
    }
    if !page.is_empty() {
        pages.push(page);
    }
    pages
}

/// Largura máxima dos nomes que quebram embaixo de um grupo.
const GROUP_NAMES_MAX: usize = 64;

/// Itens com a mesma troca (`key`) juntos, cada grupo em ordem de nome. Ordem
/// dos grupos: os de boot, os maiores, e o resto pelo nome. Serve à lista do
/// `d` (bancos de pacotes) e ao resumo do seletor (pacman.log).
fn same_change<'a, T, K: Ord>(items: &[&'a T], key: impl Fn(&'a T) -> K, name: impl Fn(&T) -> &str) -> Vec<Vec<&'a T>> {
    let mut by_key: std::collections::BTreeMap<K, Vec<&'a T>> = std::collections::BTreeMap::new();
    for &item in items {
        by_key.entry(key(item)).or_default().push(item);
    }
    let mut units: Vec<Vec<&'a T>> = by_key.into_values().collect();
    for unit in &mut units {
        unit.sort_by(|a, b| name(a).cmp(name(b)));
    }
    let boot = |u: &Vec<&T>| u.iter().any(|x| pacman_log::is_boot_package(name(x)));
    units.sort_by(|a, b| boot(b).cmp(&boot(a)).then(b.len().cmp(&a.len())).then(name(a[0]).cmp(name(b[0]))));
    units
}

fn same_version_change<'a>(nets: &[&'a NetChange]) -> Vec<Vec<&'a NetChange>> {
    same_change(nets, |n| (n.before.as_deref(), n.now.as_deref()), |n| &n.name)
}

/// `poppler, poppler-glib` para dois; `kcolorscheme +10` a partir de três.
fn unit_label(unit: &[&NetChange]) -> String {
    match unit {
        [only] => only.name.clone(),
        [a, b] => format!("{}, {}", a.name, b.name),
        [first, rest @ ..] => format!("{} +{}", first.name, rest.len()),
        [] => String::new(),
    }
}

/// Linha de um grupo. Nas versões, o que as duas têm em comum fica apagado:
/// sobra à vista o trecho que muda (`6.32.0` → `6.31.0`).
fn unit_line(unit: &[&NetChange], name_col: usize, left_col: usize) -> String {
    let net = unit[0];
    let marker = match (&net.before, &net.now) {
        _ if unit.iter().any(|n| pacman_log::is_boot_package(&n.name)) => style("●").yellow().to_string(),
        (None, _) => style("−").red().to_string(),
        (_, None) => style("+").green().to_string(),
        _ => style("↺").dim().to_string(),
    };
    let name = crate::ui::term::ellipsize(&unit_label(unit), name_col);
    let now = version(&net.now);
    let before = version(&net.before);
    let pad = " ".repeat(left_col.saturating_sub(now.chars().count()));
    let (left, right) = match (&net.now, &net.before) {
        (Some(now), Some(before)) => {
            let (pre, now_mid, before_mid, post) = version_diff(now, before);
            (
                format!("{}{}{}", style(pre).dim(), now_mid, style(post).dim()),
                format!("{}{}{}", style(pre).dim(), style(before_mid).bold(), style(post).dim()),
            )
        }
        _ => (style(now).dim().to_string(), before),
    };
    format!("{marker} {name:<name_col$}   {left}{pad}   {right}")
}

/// Corta duas versões em começo comum, o trecho de cada uma que muda e fim
/// comum. Os cortes caem num separador (`.`, `-`, `_`, `:`), para o trecho
/// ser um número inteiro: `6.32.0-1.1` × `6.31.0-1.1` → `6.`, `32`, `31`, `.0-1.1`.
fn version_diff<'a>(a: &'a str, b: &'a str) -> (&'a str, &'a str, &'a str, &'a str) {
    let sep = |c: u8| matches!(c, b'.' | b'-' | b'_' | b':');
    let (x, y) = (a.as_bytes(), b.as_bytes());
    let mut pre = x.iter().zip(y).take_while(|(p, q)| p == q).count();
    while pre > 0 && !sep(x[pre - 1]) {
        pre -= 1;
    }
    let room = (x.len() - pre).min(y.len() - pre);
    let mut post = x.iter().rev().zip(y.iter().rev()).take(room).take_while(|(p, q)| p == q).count();
    while post > 0 && !sep(x[x.len() - post]) {
        post -= 1;
    }
    // Versões do pacman são ASCII; um corte fora da fronteira de um caractere
    // devolve as duas inteiras.
    let cut = || Some((a.get(..pre)?, a.get(pre..a.len() - post)?, b.get(pre..b.len() - post)?, a.get(a.len() - post..)?));
    cut().unwrap_or(("", a, b, ""))
}

/// Resumo do checkpoint sob o cursor no seletor do restore, pelo que as
/// transações dele fizeram: uma linha por origem (`Repositório 43`, `AUR 2`),
/// com os de boot primeiro, os grupos com a mesma troca como `nome +N` e o que
/// não couber em `+N`. Vazio para quem não veio do pacman.
pub(crate) fn picker_summary(group: &Group, txs: &[Transaction], width: usize) -> Vec<String> {
    let (local, repo): (Vec<&Transaction>, Vec<&Transaction>) = txs.iter().partition(|t| t.op == Some("-U"));
    let compiled = match group::pacman_helper(group) {
        Some("makepkg") => "makepkg",
        Some(_) => "AUR",
        None => "Arquivo",
    };
    let sections: Vec<(String, Vec<Change>)> = [("Repositório", repo), (compiled, local)]
        .into_iter()
        .map(|(name, txs)| (name, txs.iter().flat_map(|t| t.changes.clone()).collect::<Vec<Change>>()))
        .filter(|(_, changes)| !changes.is_empty())
        .map(|(name, changes)| (format!("{name} {}", changes.len()), changes))
        .collect();
    let label_col = sections.iter().map(|(label, _)| label.chars().count()).max().unwrap_or(0);
    sections
        .iter()
        .map(|(label, changes)| {
            let room = width.saturating_sub(label_col + 3);
            format!("{}   {}", style(format!("{label:<label_col$}")).dim(), summary_items(changes, room))
        })
        .collect()
}

/// Os itens do resumo que cabem em `room`, com `+N` para o resto.
fn summary_items(changes: &[Change], room: usize) -> String {
    let refs: Vec<&Change> = changes.iter().collect();
    let units = same_change(&refs, |c| (c.op as u8, c.old.as_deref(), c.new.as_deref()), |c| &c.name);
    let boot = |u: &Vec<&Change>| u.iter().any(|c| pacman_log::is_boot_package(&c.name));

    let sep = " · ";
    let total = changes.len();
    let mut shown = 0;
    let mut used = 0;
    let mut parts: Vec<String> = Vec::new();
    for unit in &units {
        let (plain, styled) = summary_item(unit, boot(unit));
        let left_after = total - shown - unit.len();
        let tail = if left_after > 0 { sep.chars().count() + format!("+{left_after}").len() } else { 0 };
        let gap = if parts.is_empty() { 0 } else { sep.chars().count() };
        if used + gap + plain.chars().count() + tail > room {
            break;
        }
        used += gap + plain.chars().count();
        shown += unit.len();
        parts.push(styled);
    }
    if shown < total {
        parts.push(style(format!("+{}", total - shown)).dim().to_string());
    }
    parts.join(&style(sep).dim().to_string())
}

/// Um item do resumo, em texto puro (para medir) e pintado.
fn summary_item(unit: &[&Change], boot: bool) -> (String, String) {
    let change = unit[0];
    let text = match (unit.len(), change.op) {
        (1, Op::Installed) => format!("+{}", change.name),
        (1, Op::Removed) => format!("−{}", change.name),
        (1, Op::Upgraded | Op::Downgraded) => {
            let (old, new) = short_versions(change.old.as_deref().unwrap_or("?"), change.new.as_deref().unwrap_or("?"));
            format!("{} {old} → {new}", change.name)
        }
        (1, Op::Reinstalled) => change.name.clone(),
        (n, Op::Installed) => format!("novos: {} +{}", change.name, n - 1),
        (n, Op::Removed) => format!("removidos: {} +{}", change.name, n - 1),
        (n, _) => format!("{} +{}", change.name, n - 1),
    };
    if !boot {
        return (text.clone(), style(text).dim().to_string());
    }
    (format!("● {text}"), format!("{} {}", style("●").yellow(), style(text).dim()))
}

/// Sem o pkgrel quando a versão em si muda (`12.9.6-1` → `12.9.6`); numa
/// recompilação (`1.28.9-1.1` → `1.28.9-2`) o pkgrel é a diferença e fica.
fn short_versions<'a>(old: &'a str, new: &'a str) -> (&'a str, &'a str) {
    let base = |v: &'a str| v.rsplit_once('-').map_or(v, |(base, _)| base);
    if base(old) == base(new) {
        return (old, new);
    }
    (base(old), base(new))
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
fn page_lines(intro: &dyn Fn(), intro_lines: usize, head: &str, rest: &[String]) -> Result<()> {
    paged(intro, intro_lines, &|room| {
        if rest.is_empty() {
            return vec![vec![head.to_string(), style("  nenhum pacote").dim().to_string()]];
        }
        let rows = room.saturating_sub(1).max(1);
        rest.chunks(rows).map(|chunk| std::iter::once(head.to_string()).chain(chunk.iter().cloned()).collect()).collect()
    })
}

/// Mostra as páginas que `pages_for` monta para `room` linhas livres, que
/// dependem da altura do terminal (recalculada a cada tecla).
fn paged(intro: &dyn Fn(), intro_lines: usize, pages_for: &dyn Fn(usize) -> Vec<Vec<String>>) -> Result<()> {
    let mut page = 0usize;
    loop {
        let rows_on_screen = console::Term::stdout().size().0 as usize;
        let room = rows_on_screen.saturating_sub(intro_lines + LIST_FOOTER_LINES).max(4);
        let pages = pages_for(room);
        let count = pages.len().max(1);
        page = page.min(count - 1);

        clear_screen();
        intro();
        // Cortada na largura de agora: uma linha que quebrasse em duas
        // desmontaria a conta das páginas.
        let width = crate::ui::term::content_width();
        for text in pages.get(page).into_iter().flatten() {
            line(format_args!("{}", console::truncate_str(text, width, "…")));
        }
        println!();
        let back = "esc volta";
        let footer = if count > 1 {
            format!("página {}/{} · ←/→ navega · {back}", page + 1, count)
        } else {
            back.to_string()
        };
        line(format_args!("{}", style(footer).dim()));

        match console::Term::stdout().read_key().context("aguardar tecla")? {
            Key::ArrowLeft => page = page.saturating_sub(1),
            Key::ArrowRight => page = (page + 1).min(count - 1),
            Key::Escape | Key::Enter => return Ok(()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Section, boot_rows_that_fit, same_version_change, section_pages, short_versions, shorten_after, summary_items,
        unit_label, version_diff,
    };
    use crate::pacman_db::NetChange;
    use crate::pacman_log::{Change, Op};

    fn net(name: &str, before: &str, now: &str) -> NetChange {
        NetChange { name: name.into(), before: Some(before.into()), now: Some(now.into()), built_here: false }
    }

    fn up(name: &str, old: &str, new: &str) -> Change {
        Change { op: Op::Upgraded, name: name.into(), old: Some(old.into()), new: Some(new.into()) }
    }

    #[test]
    fn version_cuts_fall_on_separators() {
        assert_eq!(version_diff("6.32.0-1.1", "6.31.0-1.1"), ("6.", "32", "31", ".0-1.1"));
        assert_eq!(version_diff("1.28.9-2", "1.28.9-1.1"), ("1.28.9-", "2", "1.1", ""));
        assert_eq!(version_diff("12.10.0-1", "12.9.6-1"), ("12.", "10.0", "9.6", "-1"));
        assert_eq!(version_diff("2:9.0.2-5", "2:9.0.2-3"), ("2:9.0.2-", "5", "3", ""));
        // Uma é o começo da outra: o trecho a mais aparece inteiro.
        assert_eq!(version_diff("4.1.1-1", "4.1-1"), ("4.", "1.1", "1", "-1"));
        assert_eq!(version_diff("a", "b"), ("", "a", "b", ""));
    }

    #[test]
    fn pkgrel_stays_only_when_it_is_the_change() {
        assert_eq!(short_versions("12.9.6-1", "12.10.0-1"), ("12.9.6", "12.10.0"));
        assert_eq!(short_versions("1:48.4-2.1", "1:48.5-1.1"), ("1:48.4", "1:48.5"));
        assert_eq!(short_versions("1.28.9-1.1", "1.28.9-2"), ("1.28.9-1.1", "1.28.9-2"));
    }

    #[test]
    fn same_change_groups_lead_with_boot_then_size() {
        let nets = [
            net("kconfig", "6.31.0-1.1", "6.32.0-1.1"),
            net("btop", "1.4.7-1.1", "1.4.8-1.1"),
            net("kcrash", "6.31.0-1.1", "6.32.0-1.1"),
            net("ki18n", "6.31.0-1.1", "6.32.0-1.1"),
            net("limine", "12.9.6-1", "12.10.0-1"),
            net("poppler", "26.10.0-2.1", "26.11.0-1"),
            net("poppler-glib", "26.10.0-2.1", "26.11.0-1"),
        ];
        let refs: Vec<&NetChange> = nets.iter().collect();
        let labels: Vec<String> = same_version_change(&refs).iter().map(|u| unit_label(u)).collect();
        assert_eq!(labels, ["limine", "kconfig +2", "poppler, poppler-glib", "btop"]);
    }

    #[test]
    fn summary_cuts_with_the_count_of_what_is_left() {
        console::set_colors_enabled(false);
        let changes = [
            up("kconfig", "6.31.0-1.1", "6.32.0-1.1"),
            up("kcrash", "6.31.0-1.1", "6.32.0-1.1"),
            up("ki18n", "6.31.0-1.1", "6.32.0-1.1"),
            up("limine", "12.9.6-1", "12.10.0-1"),
            up("btop", "1.4.7-1.1", "1.4.8-1.1"),
            up("mpv", "1:0.41.0-7.1", "1:0.42.0-2.1"),
        ];
        assert_eq!(
            summary_items(&changes, 200),
            "● limine 12.9.6 → 12.10.0 · kconfig +2 · btop 1.4.7 → 1.4.8 · mpv 1:0.41.0 → 1:0.42.0"
        );
        // 45 colunas: o btop não cabe com o "+1" depois dele.
        assert_eq!(summary_items(&changes, 45), "● limine 12.9.6 → 12.10.0 · kconfig +2 · +2");
        assert_eq!(summary_items(&changes, 5), "+6");
    }

    fn section(title: &str, units: &[usize]) -> Section {
        Section {
            title: title.into(),
            head: "cols".into(),
            units: units.iter().map(|&n| (0..n).map(|i| format!("{title}{i}")).collect()).collect(),
        }
    }

    #[test]
    fn pages_repeat_the_header_and_keep_groups_whole() {
        let sections = [section("R", &[1, 3, 1]), section("C", &[1])];
        let pages = section_pages(&sections, 6);
        assert_eq!(pages, [
            vec!["R", "cols", "R0", "R0", "R1", "R2"],
            // O "C" com cabeçalho e uma linha não cabe nas 3 que sobram.
            vec!["R", "cols", "R0"],
            vec!["C", "cols", "C0"],
        ].map(|p| p.into_iter().map(String::from).collect::<Vec<_>>()));
    }

    #[test]
    fn a_group_taller_than_a_page_goes_on_with_the_header() {
        let sections = [section("R", &[1, 6])];
        let pages = section_pages(&sections, 5);
        assert_eq!(pages, [
            vec!["R", "cols", "R0", "R0", "R1"],
            vec!["R", "cols", "R2", "R3", "R4"],
            vec!["R", "cols", "R5"],
        ].map(|p| p.into_iter().map(String::from).collect::<Vec<_>>()));
    }

    #[test]
    fn groups_of_installs_say_so() {
        console::set_colors_enabled(false);
        let new = |name: &str| Change { op: Op::Installed, name: name.into(), old: None, new: Some("1-1".into()) };
        let changes = [new("a"), new("b"), new("c"), up("d", "1-1", "2-1"), up("e", "1-1", "2-1")];
        assert_eq!(summary_items(&changes, 200), "novos: a +2 · d +1");
    }

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
