use anyhow::{Context, Result};
use dialoguer::theme::Theme;
use std::fmt;

pub struct SnapgTheme;

impl Theme for SnapgTheme {
    fn format_prompt(&self, f: &mut dyn fmt::Write, prompt: &str) -> fmt::Result {
        write!(f, "{prompt}")
    }
}

pub static THEME: SnapgTheme = SnapgTheme;

/// Tom terroso (trigo, 256-color) dos títulos de seção. Trocar aqui muda toda a
/// identidade visual dos cabeçalhos.
const HEADER_COLOR: u8 = 173;

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
pub fn header(s: &str) {
    println!("{}", title(s));
}

/// Conector de árvore pro último irmão (`└─`) ou intermediário (`├─`).
pub fn branch(last: bool) -> &'static str {
    if last { "└─" } else { "├─" }
}

/// Prefixo de continuação pros filhos de um nó: vazio se o pai é o último
/// irmão, barra vertical caso contrário.
pub fn stem(last: bool) -> &'static str {
    if last { "   " } else { "│  " }
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

/// Trunca texto pra caber na largura do terminal.
/// Previne wrapping que causa bug visual no dialoguer (linhas "comendo" o conteúdo acima).
pub fn truncate_for_terminal(text: &str, prefix_len: usize) -> String {
    let width = console::Term::stdout().size().1 as usize;
    let max = width.saturating_sub(prefix_len);
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
        .with_prompt(prompt)
        .items(&["Sim", "Não"])
        .default(1)
        .clear(true)
        .report(false)
        .interact_opt()
        .context("seleção cancelada")?;
    Ok(matches!(choice, Some(0)))
}
