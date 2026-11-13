//! Seletor do restore com recorte por origem: Tab alterna Todos · Manuais ·
//! Pacman. O `Select` do dialoguer não aceita outra tecla além das dele, por
//! isso este é próprio; fora o Tab, ele se comporta igual (setas, enter, esc).

use crate::config::RestoreFilter;
use crate::ui::term::{CONTENT_INDENT, clear_screen, header, line};
use anyhow::{Context, Result};
use console::{Key, style};

pub(crate) struct Item {
    /// Linha pronta, já cortada para a largura do terminal.
    pub(crate) text: String,
    pub(crate) pacman: bool,
}

/// Linhas acima da lista (cabeçalho de 2, pergunta, filtro, colunas) e abaixo
/// (branco, dica, cursor).
const CHROME_LINES: usize = 8;

/// Índice do item escolhido em `items`, ou `None` com Esc. Sem item do pacman
/// o recorte não tem o que separar: a linha do filtro e o Tab somem.
pub(crate) fn pick(
    title: &str,
    prompt: &str,
    columns_header: &str,
    items: &[Item],
    initial: RestoreFilter,
) -> Result<Option<usize>> {
    let has_pacman = items.iter().any(|item| item.pacman);
    let mut filter = if has_pacman { initial } else { RestoreFilter::All };
    let mut cursor = 0usize;
    let mut offset = 0usize;
    let term = console::Term::stdout();
    let _ = term.hide_cursor();
    loop {
        let visible = visible(items, filter);
        cursor = cursor.min(visible.len().saturating_sub(1));
        let page = (term.size().0 as usize).saturating_sub(CHROME_LINES).max(1);
        offset = scroll(offset, cursor, page);

        clear_screen();
        header(title);
        line(format_args!("{}", style(prompt).bold()));
        if has_pacman {
            line(format_args!("{}", filter_line(filter)));
        }
        line(format_args!("  {}", style(columns_header).bold()));
        if visible.is_empty() {
            line(format_args!("  {}", style("nenhum checkpoint neste filtro").dim()));
        }
        for (row, &index) in visible.iter().enumerate().skip(offset).take(page) {
            let marker = if row == cursor { ">" } else { " " };
            println!("{CONTENT_INDENT}{marker} {}", items[index].text);
        }
        println!();
        let tab = if has_pacman { " · tab filtra" } else { "" };
        line(format_args!("{}", style(format!("↑/↓ move{tab} · enter confirma")).dim()));

        match term.read_key().context("aguardar tecla")? {
            Key::ArrowUp => cursor = cursor.checked_sub(1).unwrap_or(visible.len().saturating_sub(1)),
            Key::ArrowDown => cursor = if cursor + 1 >= visible.len() { 0 } else { cursor + 1 },
            Key::PageUp => cursor = cursor.saturating_sub(page),
            Key::PageDown => cursor = (cursor + page).min(visible.len().saturating_sub(1)),
            Key::Home => cursor = 0,
            Key::End => cursor = visible.len().saturating_sub(1),
            Key::Tab if has_pacman => {
                filter = filter.next();
                cursor = 0;
                offset = 0;
            }
            Key::Enter if !visible.is_empty() => return Ok(Some(visible[cursor])),
            Key::Escape => return Ok(None),
            _ => {}
        }
    }
}

/// Índices dos itens que o recorte mostra, na ordem original (data).
fn visible(items: &[Item], filter: RestoreFilter) -> Vec<usize> {
    (0..items.len()).filter(|&i| filter.shows(items[i].pacman)).collect()
}

/// Primeira linha da janela, mantendo o cursor dentro dela.
fn scroll(offset: usize, cursor: usize, page: usize) -> usize {
    if cursor < offset {
        return cursor;
    }
    if cursor >= offset + page {
        return cursor + 1 - page;
    }
    offset
}

/// `Mostrar  Todos · [Manuais] · Pacman`, com o recorte ativo em destaque.
fn filter_line(active: RestoreFilter) -> String {
    let options: Vec<String> = RestoreFilter::ORDER
        .iter()
        .map(|f| {
            if *f == active {
                return style(format!("[{}]", f.label())).bold().to_string();
            }
            style(f.label()).dim().to_string()
        })
        .collect();
    format!("{}{}", style("Mostrar  ").dim(), options.join(&style(" · ").dim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::{Item, scroll, visible};
    use crate::config::RestoreFilter;

    fn items(kinds: &[bool]) -> Vec<Item> {
        kinds.iter().map(|&pacman| Item { text: String::new(), pacman }).collect()
    }

    #[test]
    fn filters_keep_the_date_order() {
        let list = items(&[true, false, true, false]);
        assert_eq!(visible(&list, RestoreFilter::All), vec![0, 1, 2, 3]);
        assert_eq!(visible(&list, RestoreFilter::Manual), vec![1, 3]);
        assert_eq!(visible(&list, RestoreFilter::Pacman), vec![0, 2]);
    }

    #[test]
    fn tab_cycles_through_the_three() {
        assert_eq!(RestoreFilter::All.next(), RestoreFilter::Manual);
        assert_eq!(RestoreFilter::Manual.next(), RestoreFilter::Pacman);
        assert_eq!(RestoreFilter::Pacman.next(), RestoreFilter::All);
    }

    #[test]
    fn the_window_follows_the_cursor() {
        assert_eq!(scroll(0, 3, 5), 0);
        assert_eq!(scroll(0, 7, 5), 3);
        assert_eq!(scroll(3, 1, 5), 1);
    }
}
