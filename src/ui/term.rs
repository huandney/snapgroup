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

pub fn clear_screen() {
    let _ = console::Term::stdout().clear_screen();
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
        .interact_opt()
        .context("seleção cancelada")?;
    Ok(matches!(choice, Some(0)))
}
