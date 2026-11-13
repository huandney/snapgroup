//! Tela do `snapg delete`: checkpoints e backups órfãos numa lista só, com o
//! espaço que a seleção libera no topo, recalculado a cada marcação.

use crate::group::{self, Group, GroupId};
use crate::orphan::OrphanBackup;
use crate::ui::checkpoints::{ReviewCard, ReviewDecision, kernel_label, review_cards};
use crate::ui::term::{AltScreen, CONTENT_INDENT, DISPLAY_VERSION, PAGE_INDENT, short_datetime, title};
use anyhow::{Context, Result, bail};
use console::{Key, Term, style};
use std::collections::{BTreeSet, HashMap};

/// Índices em `groups` e em `orphans` escolhidos para apagar.
pub(crate) struct DeletePlan {
    pub(crate) groups: Vec<usize>,
    pub(crate) orphans: Vec<usize>,
}

/// Espaço que a seleção libera, separado por tipo na ordem em que é somado:
/// backups, depois automáticos, depois fixos. Cada parte é o que ela acrescenta
/// às anteriores (o que dois tipos dividem conta uma vez, no primeiro).
/// Backups saem na hora; checkpoints, no purge da lixeira (ou na hora com
/// `--purge`).
pub(crate) enum Estimate {
    Pending,
    Unavailable(String),
    /// Partes em bytes de dados, os que voltam ao livre do `df`. `metadata` é o
    /// da seleção inteira: sai das árvores, mas fica reservado no btrfs.
    Ready { orphans: u64, auto: u64, fixed: u64, metadata: u64, scanned: u64 },
}

/// Rosa dos checkpoints fixos: longe do amarelo ("agora") e do azul-claro
/// ("no purge") que marcam a seleção.
const FIXED_COLOR: u8 = 211;

/// O que a tela precisa saber de fora dela.
pub(crate) struct DeleteContext<'a> {
    pub(crate) groups: &'a [Group],
    pub(crate) kernel_labels: &'a HashMap<GroupId, String>,
    pub(crate) orphans: &'a [OrphanBackup],
    pub(crate) purge: bool,
    pub(crate) trash_days: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum Row {
    Group(usize),
    Orphan(usize),
}

struct Selection {
    groups: BTreeSet<usize>,
    orphans: BTreeSet<usize>,
    cursor: usize,
    offset: usize,
}

impl Selection {
    fn has(&self, row: Row) -> bool {
        match row {
            Row::Group(i) => self.groups.contains(&i),
            Row::Orphan(i) => self.orphans.contains(&i),
        }
    }

    fn toggle(&mut self, row: Row) {
        let (set, i) = match row {
            Row::Group(i) => (&mut self.groups, i),
            Row::Orphan(i) => (&mut self.orphans, i),
        };
        if !set.remove(&i) {
            set.insert(i);
        }
    }

    fn is_empty(&self) -> bool {
        self.groups.is_empty() && self.orphans.is_empty()
    }
}

/// Marca todos de `all`, ou desmarca todos se já estavam marcados.
fn toggle_all(set: &mut BTreeSet<usize>, all: impl Iterator<Item = usize>) {
    let all: Vec<usize> = all.collect();
    if all.iter().all(|i| set.contains(i)) {
        for i in &all {
            set.remove(i);
        }
        return;
    }
    set.extend(all);
}

/// `t`: marca tudo da lista, ou desmarca tudo se já estava tudo marcado.
fn toggle_everything(ctx: &DeleteContext<'_>, sel: &mut Selection) {
    let everything = sel.groups.len() == ctx.groups.len() && sel.orphans.len() == ctx.orphans.len();
    if everything {
        sel.groups.clear();
        sel.orphans.clear();
        return;
    }
    sel.groups.extend(0..ctx.groups.len());
    sel.orphans.extend(0..ctx.orphans.len());
}

/// Lista → revisão → plano. Esc na revisão volta para a lista com a seleção
/// intacta. `estimate` é chamado a cada redesenho e deve responder rápido.
pub(crate) fn select_delete_plan(
    ctx: &DeleteContext<'_>,
    estimate: &mut dyn FnMut(&BTreeSet<usize>, &BTreeSet<usize>) -> Estimate,
) -> Result<Option<DeletePlan>> {
    if !Term::stdout().is_term() || unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
        bail!("snapg delete precisa de um terminal (use -y para o checkpoint mais recente)");
    }
    let _alt = AltScreen::enter();
    let rows: Vec<Row> = (0..ctx.groups.len())
        .map(Row::Group)
        .chain((0..ctx.orphans.len()).map(Row::Orphan))
        .collect();
    let mut sel = Selection { groups: BTreeSet::new(), orphans: BTreeSet::new(), cursor: 0, offset: 0 };
    loop {
        if !pick(ctx, &rows, &mut sel, estimate)? {
            return Ok(None);
        }
        let summary = summary_text(ctx, &sel, &estimate(&sel.groups, &sel.orphans));
        match review_cards(&review(ctx, &sel), "Apagar", "Apagar?", &summary)? {
            ReviewDecision::Proceed => {
                return Ok(Some(DeletePlan {
                    groups: sel.groups.into_iter().collect(),
                    orphans: sel.orphans.into_iter().collect(),
                }));
            }
            ReviewDecision::Back => continue,
            ReviewDecision::Cancel => return Ok(None),
        }
    }
}

/// Laço de teclas da lista. `true` = enter com algo marcado; `false` = saiu.
fn pick(
    ctx: &DeleteContext<'_>,
    rows: &[Row],
    sel: &mut Selection,
    estimate: &mut dyn FnMut(&BTreeSet<usize>, &BTreeSet<usize>) -> Estimate,
) -> Result<bool> {
    let term = Term::stdout();
    let _input = InputMode::enter()?;
    let _ = term.hide_cursor();
    let mut drawn: Option<(bool, (u16, u16))> = None;
    let mut dirty = true;
    loop {
        let est = estimate(&sel.groups, &sel.orphans);
        let pending = matches!(est, Estimate::Pending);
        let size = term.size();
        if dirty || drawn != Some((pending, size)) {
            let frame = render(ctx, rows, sel, &est, size);
            term.write_str(&frame).context("desenhar a tela")?;
            drawn = Some((pending, size));
            dirty = false;
        }
        if !key_ready(150)? {
            continue;
        }
        let page = list_height(size.0).max(1);
        let last = rows.len().saturating_sub(1);
        match term.read_key_raw().context("ler tecla")? {
            Key::ArrowUp => sel.cursor = sel.cursor.saturating_sub(1),
            Key::ArrowDown => sel.cursor = (sel.cursor + 1).min(last),
            Key::PageUp => sel.cursor = sel.cursor.saturating_sub(page),
            Key::PageDown => sel.cursor = (sel.cursor + page).min(last),
            Key::Home => sel.cursor = 0,
            Key::End => sel.cursor = last,
            Key::Char(' ') => {
                if let Some(&row) = rows.get(sel.cursor) {
                    sel.toggle(row);
                }
            }
            Key::Char('o') => toggle_all(&mut sel.orphans, 0..ctx.orphans.len()),
            Key::Char('a') => toggle_all(
                &mut sel.groups,
                (0..ctx.groups.len()).filter(|&i| group::is_auto(&ctx.groups[i])),
            ),
            Key::Char('f') => toggle_all(
                &mut sel.groups,
                (0..ctx.groups.len()).filter(|&i| !group::is_auto(&ctx.groups[i])),
            ),
            Key::Char('t') => toggle_everything(ctx, sel),
            Key::Enter if !sel.is_empty() => {
                let _ = term.show_cursor();
                return Ok(true);
            }
            Key::Escape | Key::CtrlC | Key::Char('q') => {
                let _ = term.show_cursor();
                return Ok(false);
            }
            _ => continue,
        }
        dirty = true;
    }
}

/// Linhas fixas fora da lista: cabeçalho (2), espaço em branco, bloco Espaço
/// (4), branco, e no rodapé branco + dica.
const FIXED_LINES: usize = 10;

/// Linhas da lista: o que sobra da altura. Mínimo 1, para o quadro nunca
/// passar da tela a partir de `FIXED_LINES + 1` linhas.
fn list_height(rows: u16) -> usize {
    (rows as usize).saturating_sub(FIXED_LINES).max(1)
}

fn render(
    ctx: &DeleteContext<'_>,
    rows: &[Row],
    sel: &mut Selection,
    est: &Estimate,
    (height, width): (u16, u16),
) -> String {
    let width = width as usize;
    let mut out: Vec<String> = Vec::new();
    out.push(format!(
        "{PAGE_INDENT}{} {}",
        style("SnapGroup").color256(214).bold(),
        style(DISPLAY_VERSION).dim()
    ));
    out.push(format!("{PAGE_INDENT}{} {}", title("▪"), title("Apagar")));
    out.push(String::new());
    out.extend(space_block(ctx, sel, est, width));
    out.push(String::new());

    let (list, cursor_line) = list_lines(ctx, rows, sel, width);
    let height = list_height(height);
    if cursor_line < sel.offset + 2 {
        sel.offset = cursor_line.saturating_sub(2);
    }
    if cursor_line >= sel.offset + height {
        sel.offset = cursor_line + 1 - height;
    }
    sel.offset = sel.offset.min(list.len().saturating_sub(height));
    let visible = &list[sel.offset..(sel.offset + height).min(list.len())];
    out.extend(visible.iter().cloned());
    for _ in visible.len()..height {
        out.push(String::new());
    }

    out.push(String::new());
    let mut keys = vec!["espaço marca"];
    if !ctx.orphans.is_empty() {
        keys.push("o órfãos");
    }
    if ctx.groups.iter().any(group::is_auto) {
        keys.push("a automáticos");
    }
    if ctx.groups.iter().any(|g| !group::is_auto(g)) {
        keys.push("f fixos");
    }
    keys.extend(["t tudo", "enter revisa", "esc sai"]);
    let position = if list.len() > height {
        format!("   {}/{}", sel.cursor + 1, rows.len())
    } else {
        String::new()
    };
    out.push(format!("{CONTENT_INDENT}{}{}", style(keys.join(" · ")).dim(), style(position).dim()));

    // Reescreve no lugar (sem limpar a tela inteira): evita o piscar. A última
    // linha não ganha `\n`: com o quadro na altura exata do terminal, ele
    // rolaria a tela e o cabeçalho sairia pelo topo.
    let body: Vec<String> = out.into_iter().map(|line| format!("{line}\x1b[K")).collect();
    format!("\x1b[H{}\x1b[J", body.join("\n"))
}

fn space_block(ctx: &DeleteContext<'_>, sel: &Selection, est: &Estimate, width: usize) -> Vec<String> {
    let cells = width.saturating_sub(CONTENT_INDENT.len() + 2).clamp(10, 60);
    let heading = match est {
        Estimate::Pending => format!("{PAGE_INDENT}{} {}", title("▪ Espaço"), style("calculando…").dim()),
        _ => format!("{PAGE_INDENT}{}", title("▪ Espaço")),
    };
    let (bar, text) = match est {
        Estimate::Pending => (
            style("░".repeat(cells)).dim().to_string(),
            style("lendo o disco para calcular o espaço que a seleção libera").dim().to_string(),
        ),
        Estimate::Unavailable(reason) => (
            style("░".repeat(cells)).dim().to_string(),
            style(format!("espaço indisponível: {reason}")).dim().to_string(),
        ),
        Estimate::Ready { orphans, auto, fixed, metadata, scanned } => (
            space_bar(&[(*orphans, Paint::Now), (*auto, Paint::Purge), (*fixed, Paint::Fixed)], *scanned, cells),
            freed_text(ctx, sel, *orphans, *auto, *fixed) + &metadata_note(sel, *metadata),
        ),
    };
    vec![
        heading,
        format!("{CONTENT_INDENT}{bar}"),
        format!("{CONTENT_INDENT}{text}"),
        format!("{CONTENT_INDENT}{}", style(counts_text(ctx, sel)).dim()),
    ]
}

/// " · +248 MiB de metadados ficam reservados", apagado: não entra no `df`.
fn metadata_note(sel: &Selection, metadata: u64) -> String {
    if sel.is_empty() || metadata == 0 {
        return String::new();
    }
    style(format!(" · +{} de metadados ficam reservados", human_bytes(metadata))).dim().to_string()
}

#[derive(Clone, Copy)]
enum Paint {
    Now,
    Purge,
    Fixed,
}

fn paint(text: String, paint: Paint) -> String {
    match paint {
        Paint::Now => style(text).yellow().to_string(),
        Paint::Purge => style(text).cyan().to_string(),
        Paint::Fixed => style(text).color256(FIXED_COLOR).to_string(),
    }
}

/// "libera 8.6 GiB agora · 2.1 GiB automáticos + 1.1 GiB fixos no purge da
/// lixeira", na cor de cada parte da barra. Com `--purge`, tudo sai agora.
fn freed_text(ctx: &DeleteContext<'_>, sel: &Selection, orphans: u64, auto: u64, fixed: u64) -> String {
    if sel.is_empty() {
        return style("marque itens para ver quanto espaço sai").dim().to_string();
    }
    let dot = style(" · ").dim().to_string();
    let plus = style(" + ").dim().to_string();
    let mut checkpoints = Vec::new();
    if auto > 0 {
        checkpoints.push(paint(format!("{} automáticos", human_bytes(auto)), Paint::Purge));
    }
    if fixed > 0 {
        checkpoints.push(paint(format!("{} fixos", human_bytes(fixed)), Paint::Fixed));
    }
    let checkpoints = checkpoints.join(&plus);
    let backups = paint(format!("{} backups", human_bytes(orphans)), Paint::Now);
    if ctx.purge {
        let mut all = Vec::new();
        if orphans > 0 {
            all.push(backups);
        }
        if !checkpoints.is_empty() {
            all.push(checkpoints);
        }
        if all.is_empty() {
            return style("libera 0 MiB").dim().to_string();
        }
        return format!("{} {}{}", style("libera").dim(), all.join(&plus), style(" agora").dim());
    }
    let mut parts = Vec::new();
    if !sel.orphans.is_empty() {
        parts.push(format!("{} {}", style("libera").dim(), paint(format!("{} agora", human_bytes(orphans)), Paint::Now)));
    }
    if !checkpoints.is_empty() {
        parts.push(format!("{checkpoints}{}", style(" no purge da lixeira").dim()));
    }
    if parts.is_empty() {
        return style("a seleção não libera espaço sozinha").dim().to_string();
    }
    parts.join(&dot)
}

/// Barra na escala de tudo que o disco guarda (dados + metadados lidos), uma
/// cor por tipo, na ordem dada.
fn space_bar(parts: &[(u64, Paint)], scanned: u64, cells: usize) -> String {
    let mut bar = String::new();
    let mut used = 0;
    for &(bytes, kind) in parts {
        if bytes == 0 || scanned == 0 {
            continue;
        }
        let eighths = ((bytes as f64 / scanned as f64 * (cells * 8) as f64).round() as usize).max(1);
        used += eighths.div_ceil(8);
        bar.push_str(&paint(blocks(eighths), kind));
    }
    bar.push_str(&style("░".repeat(cells.saturating_sub(used))).dim().to_string());
    bar
}

fn blocks(eighths: usize) -> String {
    const PARTIAL: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    format!("{}{}", "█".repeat(eighths / 8), PARTIAL[eighths % 8])
}

fn counts_text(ctx: &DeleteContext<'_>, sel: &Selection) -> String {
    let mut parts = Vec::new();
    if !ctx.groups.is_empty() {
        let dest = if ctx.purge {
            "apagados de vez".to_string()
        } else {
            format!("lixeira ({} dias)", ctx.trash_days)
        };
        parts.push(format!("{} de {} checkpoints → {dest}", sel.groups.len(), ctx.groups.len()));
    }
    if !ctx.orphans.is_empty() {
        let subvols: usize = sel.orphans.iter().map(|&i| ctx.orphans[i].subvols.len()).sum();
        let extra = if subvols > 0 { format!(" ({subvols} subvolumes)") } else { String::new() };
        parts.push(format!("{} de {} backups → agora{extra}", sel.orphans.len(), ctx.orphans.len()));
    }
    parts.join(" · ")
}

const NAME_MAX: usize = 36;
const DATE_WIDTH: usize = 16;
const FIX_WIDTH: usize = 4;

/// Linhas da lista (seções, cabeçalhos de coluna e itens) e a linha do cursor.
fn list_lines(ctx: &DeleteContext<'_>, rows: &[Row], sel: &Selection, width: usize) -> (Vec<String>, usize) {
    let mut lines = Vec::new();
    let mut cursor_line = 0;
    // "› ■ " antes de cada item.
    let prefix = CONTENT_INDENT.len() + 4;

    if !ctx.groups.is_empty() {
        let kernel_w = ctx
            .groups
            .iter()
            .map(|g| kernel_label(ctx.kernel_labels, g.id).chars().count())
            .max()
            .unwrap_or(0)
            .max("Kernel".len());
        let fixed = prefix + FIX_WIDTH + 2 + 3 + kernel_w + 3 + DATE_WIDTH;
        let name_w = ctx
            .groups
            .iter()
            .map(|g| group::description(g).chars().count())
            .max()
            .unwrap_or(0)
            .clamp("Nome".len(), NAME_MAX)
            .min(width.saturating_sub(fixed).max("Nome".len()));
        lines.push(format!("{PAGE_INDENT}{}", title("▪ Checkpoints")));
        lines.push(style(format!(
            "{}{:FIX_WIDTH$}  {:<name_w$}   {:<kernel_w$}   Data",
            " ".repeat(prefix),
            "",
            "Nome",
            "Kernel"
        ))
        .dim()
        .to_string());
        for (i, g) in ctx.groups.iter().enumerate() {
            let row = Row::Group(i);
            let at_cursor = rows.get(sel.cursor) == Some(&row);
            if at_cursor {
                cursor_line = lines.len();
            }
            let fix = if group::is_auto(g) { "" } else { "fixo" };
            let name = fit(group::description(g), name_w);
            let rest = format!(
                "   {:<kernel_w$}   {}",
                kernel_label(ctx.kernel_labels, g.id),
                short_datetime(group::date(g))
            );
            let kind = if fix.is_empty() { Paint::Purge } else { Paint::Fixed };
            let fix = format!("{fix:<FIX_WIDTH$}  ");
            lines.push(item_line(at_cursor, sel.has(row), kind, &fix, &name, &rest));
        }
    }

    if !ctx.orphans.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        let members_w = ctx
            .orphans
            .iter()
            .map(|o| members_text(o).chars().count())
            .max()
            .unwrap_or(0)
            .max("Subvolumes".len());
        lines.push(format!(
            "{PAGE_INDENT}{} {}",
            title("▪ Backups órfãos"),
            style("restos de restores fora do Snapper, sem lixeira").dim()
        ));
        lines.push(style(format!(
            "{}{:FIX_WIDTH$}  {:<DATE_WIDTH$}   {:<members_w$}   Origem",
            " ".repeat(prefix),
            "",
            "Criado",
            "Subvolumes"
        ))
        .dim()
        .to_string());
        for (i, o) in ctx.orphans.iter().enumerate() {
            let row = Row::Orphan(i);
            let at_cursor = rows.get(sel.cursor) == Some(&row);
            if at_cursor {
                cursor_line = lines.len();
            }
            let name = format!("{:<DATE_WIDTH$}", short_datetime(&o.created));
            let rest = format!("   {:<members_w$}   {}", members_text(o), style(origin(o)).dim());
            lines.push(item_line(at_cursor, sel.has(row), Paint::Now, &" ".repeat(FIX_WIDTH + 2), &name, &rest));
        }
    }
    (lines, cursor_line)
}

/// Marcado: `■` e o nome na cor do tipo (a mesma da barra), o resto da linha
/// em negrito. O rótulo "fixo" diz o tipo, sempre apagado.
fn item_line(at_cursor: bool, marked: bool, kind: Paint, fix: &str, name: &str, rest: &str) -> String {
    let cursor = if at_cursor { style("›").bold().to_string() } else { " ".to_string() };
    let fix = style(fix).dim();
    if !marked {
        return format!("{CONTENT_INDENT}{cursor} {} {fix}{name}{rest}", style("□").dim());
    }
    let mark = paint("■".to_string(), kind);
    let name = style(paint(name.to_string(), kind)).bold();
    format!("{CONTENT_INDENT}{cursor} {mark} {fix}{name}{}", style(rest).bold())
}

/// `@ @home @root`, com a contagem quando uma base aparece mais de uma vez.
fn members_text(o: &OrphanBackup) -> String {
    let bases = o.bases.join(" ");
    if o.subvols.len() > o.bases.len() {
        return format!("{bases} ({})", o.subvols.len());
    }
    bases
}

/// O `limine-snapper-restore` grava o rótulo em ISO UTC (`…T03:03:12.448Z`);
/// `YYYY-MM-DD_HH:MM:SS` é o formato do snapg antigo e do btrfs-assistant.
fn origin(o: &OrphanBackup) -> &'static str {
    if o.label.contains('T') && o.label.ends_with('Z') {
        return "restore do Limine";
    }
    "snapg antigo ou btrfs-assistant"
}

fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return format!("{text:<width$}");
    }
    let cut: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{cut}…")
}

fn review(ctx: &DeleteContext<'_>, sel: &Selection) -> Vec<ReviewCard> {
    let dest = if ctx.purge { "apagado de vez" } else { "lixeira" };
    let mut cards = Vec::new();
    for &i in &sel.groups {
        let g = &ctx.groups[i];
        let name = group::description(g).trim();
        cards.push(ReviewCard {
            title: if name.is_empty() { format!("#{}", g.id) } else { name.to_string() },
            detail: format!(
                "{}  ·  kernel {}  ·  {} membros  ·  → {dest}",
                short_datetime(group::date(g)),
                kernel_label(ctx.kernel_labels, g.id),
                g.members.len()
            ),
        });
    }
    for &i in &sel.orphans {
        let o = &ctx.orphans[i];
        cards.push(ReviewCard {
            title: format!("backup {}  ·  {}", short_datetime(&o.created), members_text(o)),
            detail: format!("{}  ·  → apagado agora", o.subvols.join("  ")),
        });
    }
    cards
}

fn summary_text(ctx: &DeleteContext<'_>, sel: &Selection, est: &Estimate) -> String {
    let mut parts = Vec::new();
    if !sel.groups.is_empty() {
        let dest = if ctx.purge { "apagados de vez" } else { "para a lixeira" };
        parts.push(format!("{} checkpoints {dest}", sel.groups.len()));
    }
    if !sel.orphans.is_empty() {
        parts.push(format!("{} backups apagados agora, sem lixeira", sel.orphans.len()));
    }
    if let Estimate::Ready { orphans, auto, fixed, .. } = est {
        let now = if ctx.purge { orphans + auto + fixed } else { *orphans };
        if now > 0 {
            parts.push(format!("libera {} agora", human_bytes(now)));
        }
    }
    parts.join(" · ")
}

pub(crate) fn human_bytes(bytes: u64) -> String {
    const GIB: f64 = (1u64 << 30) as f64;
    const MIB: f64 = (1u64 << 20) as f64;
    if bytes as f64 >= GIB {
        return format!("{:.1} GiB", bytes as f64 / GIB);
    }
    format!("{:.0} MiB", bytes as f64 / MIB)
}

/// Terminal sem modo canônico, eco e sinais enquanto a lista está aberta: as
/// teclas chegam uma a uma e o `poll` vê cada uma, e o laço pode redesenhar
/// quando a leitura do disco termina sem esperar tecla. Ctrl+C chega como
/// tecla e cancela. A saída continua processada (`\n` vira nova linha).
struct InputMode {
    original: libc::termios,
}

impl InputMode {
    fn enter() -> Result<Self> {
        let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: tcgetattr preenche o termios quando devolve 0.
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, termios.as_mut_ptr()) } != 0 {
            bail!("tcgetattr: {}", std::io::Error::last_os_error());
        }
        // SAFETY: inicializado pelo tcgetattr acima.
        let original = unsafe { termios.assume_init() };
        let mut mode = original;
        mode.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
        mode.c_cc[libc::VMIN] = 1;
        mode.c_cc[libc::VTIME] = 0;
        // SAFETY: termios válido, fd do terminal.
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &mode) } != 0 {
            bail!("tcsetattr: {}", std::io::Error::last_os_error());
        }
        Ok(Self { original })
    }
}

impl Drop for InputMode {
    fn drop(&mut self) {
        // SAFETY: restaura o termios lido no enter.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original) };
    }
}

fn key_ready(timeout_ms: i32) -> Result<bool> {
    let mut fd = libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 };
    // SAFETY: um pollfd válido.
    let rc = unsafe { libc::poll(&mut fd, 1, timeout_ms) };
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        bail!("poll: {err}");
    }
    Ok(rc > 0 && fd.revents & libc::POLLIN != 0)
}

#[cfg(test)]
mod tests {
    use super::{BTreeSet, DeleteContext, Estimate, Row, Selection, blocks, human_bytes, render, toggle_all};
    use crate::orphan::OrphanBackup;
    use std::collections::HashMap;

    #[test]
    fn frame_fills_the_terminal_without_scrolling_it() {
        let orphans: Vec<OrphanBackup> = (0..40)
            .map(|i| OrphanBackup {
                label: format!("2026-05-0{}T00:00:00Z", i % 9 + 1),
                subvols: vec![format!("@_backup_{i}")],
                bases: vec!["@".to_string()],
                created: "2026-05-01 00:00:00".to_string(),
            })
            .collect();
        let labels = HashMap::new();
        let ctx = DeleteContext { groups: &[], kernel_labels: &labels, orphans: &orphans, purge: false, trash_days: 15 };
        let rows: Vec<Row> = (0..orphans.len()).map(Row::Orphan).collect();
        let mut sel = Selection { groups: BTreeSet::new(), orphans: BTreeSet::new(), cursor: 39, offset: 0 };
        for height in [12u16, 24, 40] {
            let frame = render(&ctx, &rows, &mut sel, &Estimate::Pending, (height, 100));
            assert_eq!(frame.matches('\n').count() + 1, height as usize, "altura {height}");
            assert!(!frame.ends_with('\n'));
            assert!(frame.contains("SnapGroup"));
        }
    }

    #[test]
    fn freed_text_names_each_kind_and_its_destination() {
        let labels = HashMap::new();
        let orphans = vec![OrphanBackup {
            label: "x".to_string(),
            subvols: vec!["@_backup_x".to_string()],
            bases: vec!["@".to_string()],
            created: String::new(),
        }];
        let mut ctx = DeleteContext { groups: &[], kernel_labels: &labels, orphans: &orphans, purge: false, trash_days: 15 };
        let sel = Selection {
            groups: [0, 1].into_iter().collect(),
            orphans: [0].into_iter().collect(),
            cursor: 0,
            offset: 0,
        };
        let gib = 1u64 << 30;
        assert_eq!(
            super::freed_text(&ctx, &sel, 8 * gib, 2 * gib, gib),
            "libera 8.0 GiB agora · 2.0 GiB automáticos + 1.0 GiB fixos no purge da lixeira"
        );
        assert_eq!(
            super::freed_text(&ctx, &sel, 8 * gib, 0, gib),
            "libera 8.0 GiB agora · 1.0 GiB fixos no purge da lixeira"
        );
        ctx.purge = true;
        assert_eq!(
            super::freed_text(&ctx, &sel, 8 * gib, 2 * gib, gib),
            "libera 8.0 GiB backups + 2.0 GiB automáticos + 1.0 GiB fixos agora"
        );
    }

    #[test]
    fn toggle_all_marks_then_clears() {
        let mut set: BTreeSet<usize> = [1].into_iter().collect();
        toggle_all(&mut set, 0..3);
        assert_eq!(set, [0, 1, 2].into_iter().collect());
        toggle_all(&mut set, 0..3);
        assert!(set.is_empty());
    }

    #[test]
    fn bar_blocks_use_eighths() {
        assert_eq!(blocks(0), "");
        assert_eq!(blocks(9), "█▏");
        assert_eq!(blocks(16), "██");
    }

    #[test]
    fn bytes_read_as_gib_or_mib() {
        assert_eq!(human_bytes(17 << 30), "17.0 GiB");
        assert_eq!(human_bytes(300 << 20), "300 MiB");
    }
}
