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
    pb.set_message(message);
    pb.tick();
    pb.enable_steady_tick(Duration::from_millis(120));
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
/// Convenção de hints de teclas — não inventar um terceiro formato:
/// - selects do dialoguer: inline na pergunta, entre parênteses, via
///   `prompt_hint`/`prompt_bold_hint` (rodapé abaixo da lista é impossível —
///   o redraw do dialoguer engole qualquer linha impressa depois);
/// - widgets custom full-screen (prompt de pending, `input_line`): rodapé em
///   dim, sem parênteses, mesmos tokens;
/// - tokens canônicos: "espaço marca", "enter confirma", "esc volta"/"esc sai",
///   separados por " · ";
/// - esc só é anunciado quando VOLTA um passo. Primeira página de um fluxo não
///   ganha hint de esc, salvo gates operacionais onde "sair sem resolver" é
///   uma escolha relevante e deve aparecer explicitamente como "esc sai".
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

pub fn content_width() -> usize {
    let width = console::Term::stdout().size().1 as usize;
    width.saturating_sub(CONTENT_INDENT.chars().count()).max(20)
}

pub fn wrap_text(s: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        let sep = usize::from(!line.is_empty());
        if !line.is_empty() && line.chars().count() + sep + word.chars().count() > width {
            lines.push(line);
            line = String::new();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
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

/// Editor de linha mínimo com Esc-para-sair/voltar. O `dialoguer::Input` não
/// expõe Esc nem usa o `CONTENT_INDENT` (o tema só cobre `format_prompt`/select).
/// Redesenha a tela inteira a cada tecla via `render_chrome` (mesmo padrão do
/// prompt de pending), com o hint num rodapé dim — fora da linha editável.
/// Edição só no fim da linha (append/backspace) — suficiente para nomes curtos.
/// `Ok(None)` = Esc. Enter com buffer vazio é ignorado: nome vazio não é aceito.
/// `initial` entra no buffer (editável — o rename pré-carrega o nome atual);
/// `placeholder` é texto-fantasma em dim quando o buffer está vazio: Enter o
/// aceita, qualquer tecla digitada o substitui, e ele volta se o buffer
/// esvaziar. Com placeholder vazio, Enter sem texto é ignorado (nome vazio
/// não é aceito).
pub fn input_line(
    prompt: &str,
    initial: &str,
    placeholder: &str,
    footer: &str,
    render_chrome: impl Fn(),
) -> Result<Option<String>> {
    let term = console::Term::stdout();
    if !term.is_term() {
        anyhow::bail!("entrada de texto requer um terminal interativo");
    }
    let mut buf = initial.to_string();
    loop {
        render_chrome();
        println!("{CONTENT_INDENT}{}", console::style(prompt).bold());
        let ghost = buf.is_empty() && !placeholder.is_empty();
        let text = if ghost {
            console::style(placeholder).dim()
        } else {
            console::style(buf.as_str())
        };
        println!(
            "{CONTENT_INDENT}{} {}{}",
            console::style("❯").bold(),
            text,
            console::style("▏").dim()
        );
        println!();
        println!("{CONTENT_INDENT}{}", console::style(footer).dim());
        match term.read_key().context("ler tecla")? {
            console::Key::Enter => {
                let value = buf.trim().to_string();
                if !value.is_empty() {
                    return Ok(Some(value));
                }
                if !placeholder.is_empty() {
                    return Ok(Some(placeholder.to_string()));
                }
            }
            console::Key::Escape => return Ok(None),
            console::Key::Backspace => {
                buf.pop();
            }
            console::Key::Char(c) if !c.is_control() => buf.push(c),
            _ => {}
        }
    }
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
    let normalized = s.replace('T', " ");
    let mut it = normalized.split_whitespace();
    let (Some(date), Some(time)) = (it.next(), it.next()) else {
        return normalized;
    };
    if !date.contains('-') || !time.contains(':') {
        return normalized;
    }
    let hm: String = time.chars().take(5).collect();
    format!("{date} {hm}")
}

/// True se stdout é um terminal interativo. Callers usam para decidir entre
/// wizard e comportamento não-interativo (ex.: save sem nome em script).
pub fn stdout_is_tty() -> bool {
    console::Term::stdout().is_term()
}

/// Corta `s` em `max` chars com reticências — o mesmo corte das colunas de
/// nome (list e pickers), para texto cujo limite é fixo e não a largura do
/// terminal (esse caso é o `truncate_for_terminal`).
pub fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
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

    #[test]
    fn short_datetime_accepts_t_separator() {
        assert_eq!(
            short_datetime("2026-06-17T14:34:12-0400"),
            "2026-06-17 14:34"
        );
    }
}
