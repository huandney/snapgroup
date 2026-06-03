use anyhow::{Context, Result};
use dialoguer::theme::Theme;
use indicatif::{ProgressBar, ProgressStyle};
use std::fmt;
use std::time::Duration;

/// Spinner ao vivo, atualizado no lugar (sem limpar a tela) — robusto em
/// qualquer terminal, ao contrário do redraw com `clear_screen`. O `indicatif`
/// anima numa thread própria (steady tick), então gira mesmo durante uma fase
/// muda (ex.: compressão do zstd no mkinitcpio, ou o `btrfs snapshot` que
/// bloqueia sem emitir nada). Use `set_message` pra alimentar a linha viva e
/// `finish_and_clear` ao terminar. Indentado pra casar com o `CONTENT_INDENT`.
pub fn spinner(message: String) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    // Spinner na cor de marca dos cabeçalhos (trigo, HEADER_COLOR), pra casar
    // com o `▪ Sincronização de boot` no topo.
    let template = format!("   {{spinner:.{HEADER_COLOR}}} {{wide_msg}}");
    pb.set_style(
        ProgressStyle::with_template(&template)
            .unwrap()
            // Braille girando (suave, sentido horário). O último frame é o
            // "concluído" do indicatif — irrelevante aqui porque finalizamos com
            // finish_and_clear, mas é exigido pela API.
            .tick_strings(&[
                "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "⠿",
            ]),
    );
    pb.enable_steady_tick(Duration::from_millis(120));
    pb.set_message(message);
    pb
}

pub struct SnapgTheme;

impl Theme for SnapgTheme {
    fn format_prompt(&self, f: &mut dyn fmt::Write, prompt: &str) -> fmt::Result {
        write!(f, "{CONTENT_INDENT}{prompt}")
    }

    fn format_select_prompt_item(
        &self,
        f: &mut dyn fmt::Write,
        text: &str,
        active: bool,
    ) -> fmt::Result {
        write!(f, "{}{} {}", CONTENT_INDENT, if active { ">" } else { " " }, text)
    }

    fn format_multi_select_prompt_item(
        &self,
        f: &mut dyn fmt::Write,
        text: &str,
        checked: bool,
        active: bool,
    ) -> fmt::Result {
        write!(
            f,
            "{}{} {}",
            CONTENT_INDENT,
            match (checked, active) {
                (true, true) => "> [x]",
                (true, false) => "  [x]",
                (false, true) => "> [ ]",
                (false, false) => "  [ ]",
            },
            text
        )
    }
}

pub static THEME: SnapgTheme = SnapgTheme;

/// Tom terroso (trigo, 256-color) dos títulos de seção. Trocar aqui muda toda a
/// identidade visual dos cabeçalhos.
const HEADER_COLOR: u8 = 173;
/// Tom verde dessaturado para destacar Regret sem competir com sucesso/erro.
const REGRET_COLOR: u8 = 109;
/// Cor de marca: mais forte que os cabeçalhos, mas sem usar verde/vermelho de status.
const BRAND_COLOR: u8 = 214;
/// Azul dessaturado para destacar paths (/boot, /, /@) sem aspas — diferencia o
/// token do texto sem poluir o visual.
const PATH_COLOR: u8 = 110;
/// Versão exibida na UI. O manifesto fica plain (X.Y.Z) pelo esquema de release;
/// o sufixo de pré-lançamento mora só aqui.
pub const DISPLAY_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-beta");
/// Conteúdo fica alinhado ao texto do cabeçalho, não ao marcador.
pub const PAGE_INDENT: &str = " ";
pub const CONTENT_INDENT: &str = "   ";

/// Largura do prefixo que cada tema injeta entre o CONTENT_INDENT e o texto do
/// item. Select escreve "> " (marcador 1 + espaço 1 = 2); MultiSelect escreve
/// "> [x] " (marcador 5 + espaço 1 = 6). O CONTENT_INDENT é somado dentro de
/// `truncate_for_terminal`, então o caller passa só um destes.
pub const SELECT_MARKER: usize = 2;
pub const MULTI_MARKER: usize = 6;

/// Dica canônica pros MultiSelect (marcação múltipla).
pub const HINT_MULTI: &str = "(espaço marca · enter confirma · esc volta)";
/// Dica canônica pros Select que voltam um passo com Esc.
pub const HINT_BACK: &str = "(esc volta)";

pub fn clear_screen() {
    let _ = console::Term::stdout().clear_screen();
}

/// Guard RAII do alternate screen buffer. Entra no buffer alternativo ao criar
/// e volta ao normal no drop — inclusive em early return, erro ou panic. Mantém
/// a fase interativa fora do scrollback; o resultado é impresso após o drop, no
/// terminal normal. No-op quando stdout não é um terminal.
pub struct AltScreen {
    active: bool,
}

impl AltScreen {
    pub fn enter() -> Self {
        let term = console::Term::stdout();
        let active = term.is_term();
        if active {
            let _ = term.write_str("\x1b[?1049h");
        }
        AltScreen { active }
    }
}

impl Drop for AltScreen {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let term = console::Term::stdout();
        let _ = term.write_str("\x1b[?1049l");
        let _ = term.show_cursor();
    }
}

/// Token de título de seção em trigo + bold. Use como primeira parte de
/// cabeçalhos compostos: `title("Checkpoint")`.
pub fn title(s: &str) -> console::StyledObject<&str> {
    console::style(s).color256(HEADER_COLOR).bold()
}

/// Cabeçalho de seção de linha única (trigo + bold).
pub fn app_header() {
    println!(
        "{PAGE_INDENT}{} {}",
        console::style("SnapGroup").color256(BRAND_COLOR).bold(),
        console::style(DISPLAY_VERSION).dim()
    );
}

pub fn section_header(marker: &str, s: &str) {
    println!("{PAGE_INDENT}{} {}", title(marker), title(s));
}

pub fn header(s: &str) {
    app_header();
    section_header("▪", s);
}

pub fn line(args: fmt::Arguments<'_>) {
    println!("{CONTENT_INDENT}{args}");
}

/// Conector de árvore com o indent de conteúdo (CONTENT_INDENT = "   ") embutido
/// como literal — `&'static str` pra evitar um `format!` por linha.
pub fn tree_branch(last: bool) -> &'static str {
    if last { "   └─" } else { "   ├─" }
}

pub fn tree_stem(last: bool) -> &'static str {
    if last { "      " } else { "   │  " }
}

/// Token de Regret em cor própria para diferenciar o estado anterior sem usar
/// o verde de sucesso.
pub fn regret_title(s: &str) -> console::StyledObject<&str> {
    console::style(s).color256(REGRET_COLOR).bold()
}

/// Path destacado em cor própria (sem aspas), para diferenciar tokens como
/// /boot, / e /@ do texto ao redor.
pub fn path(s: &str) -> console::StyledObject<&str> {
    console::style(s).color256(PATH_COLOR)
}

/// Pergunta de confirmação em negrito. Reservado pras decisões consequentes
/// (sim/não), não pra prompts de seleção — bold em tudo dilui o sinal.
pub fn prompt_bold(question: &str) -> String {
    console::style(question).bold().to_string()
}

/// Prompt de seleção com dica de navegação: pergunta normal + hint em dim.
pub fn prompt_hint(question: &str, hint: &str) -> String {
    format!("{}  {}", question, console::style(hint).dim())
}

/// Confirmação consequente com dica: pergunta em negrito + hint em dim. Estiliza
/// os dois separados pra que o negrito da pergunta não vaze pro hint.
pub fn prompt_bold_hint(question: &str, hint: &str) -> String {
    format!("{}  {}", console::style(question).bold(), console::style(hint).dim())
}

/// Conector de árvore pro último irmão (`└─`) ou intermediário (`├─`).
pub fn branch(last: bool) -> &'static str {
    if last { "└─" } else { "├─" }
}

/// Encurta um timestamp pra `YYYY-MM-DD HH:MM`, descartando segundos e timezone.
/// Robusto a formatos diferentes (snapper, `btrfs subvolume show`) e a fallbacks
/// não-data: se os dois primeiros tokens não parecerem data+hora, devolve igual.
pub fn short_datetime(s: &str) -> String {
    let mut it = s.split_whitespace();
    let (Some(date), Some(time)) = (it.next(), it.next()) else {
        return s.to_string();
    };
    if !date.contains('-') || !time.contains(':') {
        return s.to_string();
    }
    let hm: String = time.chars().take(5).collect();
    format!("{date} {hm}")
}

/// Trunca texto pra caber numa linha de item interativo sem wrap. `marker_len` é
/// só a largura do marcador do tema (`SELECT_MARKER` ou `MULTI_MARKER`); o
/// CONTENT_INDENT, constante em todo item, é somado aqui pra que nenhum caller
/// precise lembrar dele. Previne o bug visual do dialoguer (linhas "comendo" o
/// conteúdo acima) quando um item estoura a largura e quebra.
pub fn truncate_for_terminal(text: &str, marker_len: usize) -> String {
    let width = console::Term::stdout().size().1 as usize;
    let max = width.saturating_sub(CONTENT_INDENT.chars().count() + marker_len);
    if text.chars().count() <= max {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{truncated}…")
}

/// Confirmação Sim/Não navegável por setas. Default destacado em "Não"
/// (cauteloso); Esc também equivale a "Não".
pub fn confirm(prompt: &str) -> Result<bool> {
    let choice = dialoguer::Select::with_theme(&THEME)
        .with_prompt(prompt_bold(prompt))
        .items(&["Sim", "Não"])
        .default(1)
        .clear(true)
        .report(false)
        .interact_opt()
        .context("seleção cancelada")?;
    Ok(matches!(choice, Some(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_template_is_valid() {
        // O template é parseado em runtime; sem este teste, uma cor 256 ou
        // placeholder inválido viraria panic no `unwrap` só durante um restore.
        let template = format!("   {{spinner:.{HEADER_COLOR}}} {{wide_msg}}");
        assert!(ProgressStyle::with_template(&template).is_ok());
    }
}
