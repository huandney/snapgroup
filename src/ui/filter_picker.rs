//! Seletor do restore com recortes: Tab alterna Todos · Fixos · Saves ·
//! Pacman. O `Select` do dialoguer não aceita outra tecla além das dele, por
//! isso este é próprio; fora o Tab, ele se comporta igual (setas, enter, esc).

use crate::config::RestoreFilter;
use crate::ui::term::{CONTENT_INDENT, clear_screen, content_width, header, line};
use anyhow::{Context, Result};
use console::{Key, style};

pub(crate) struct Item {
    /// Linha pronta, já cortada para a largura do terminal.
    pub(crate) text: String,
    pub(crate) pacman: bool,
    /// Fora da retenção (nome escolhido, renomeado ou vindo do Regret): só
    /// sai pelo `delete`.
    pub(crate) fixed: bool,
    /// Resumo mostrado embaixo da lista quando o cursor está no item.
    pub(crate) detail: Vec<String>,
}

/// O que o seletor lembra entre uma abertura e outra no mesmo restore: voltar
/// da prévia com Esc reabre no mesmo filtro e no mesmo checkpoint.
pub(crate) struct PickState {
    pub(crate) filter: RestoreFilter,
    pub(crate) chosen: Option<usize>,
}

/// Linhas acima da lista (cabeçalho de 2, pergunta, filtro, colunas) e abaixo
/// (branco, dica, cursor), fora o resumo do item.
const CHROME_LINES: usize = 8;

/// Índice do item escolhido em `items`, ou `None` com Esc. Quando nenhum
/// recorte separa nada (a lista toda cabe num só dos três), a linha do filtro
/// e o Tab somem.
pub(crate) fn pick(
    title: &str,
    prompt: &str,
    columns_header: &str,
    items: &[Item],
    state: &mut PickState,
) -> Result<Option<usize>> {
    let has_split = splits(items);
    state.filter = opening_filter(items, state.filter);
    let mut cursor = state
        .chosen
        .and_then(|chosen| visible(items, state.filter).iter().position(|&i| i == chosen))
        .unwrap_or(0);
    let mut offset = 0usize;
    let term = console::Term::stdout();
    let _ = term.hide_cursor();
    let picked = loop {
        let filter = state.filter;
        let visible = visible(items, filter);
        cursor = cursor.min(visible.len().saturating_sub(1));
        // O espaço do resumo é fixo dentro do recorte: a lista não pula quando
        // o cursor passa de um checkpoint com resumo (do pacman) para um sem,
        // e some quando nada à vista tem resumo (só checkpoints fora do pacman).
        let detail_lines = visible.iter().map(|&i| items[i].detail.len()).max().unwrap_or(0);
        let detail_chrome = if detail_lines > 0 { detail_lines + 1 } else { 0 };
        let page = (term.size().0 as usize).saturating_sub(CHROME_LINES + detail_chrome).max(1);
        offset = scroll(offset, cursor, page);

        clear_screen();
        header(title);
        line(format_args!("{}", style(prompt).bold()));
        if has_split {
            line(format_args!("{}", filter_line(items, filter)));
        }
        line(format_args!("  {}", style(columns_header).bold()));
        if visible.is_empty() {
            line(format_args!("  {}", style("nenhum checkpoint neste filtro").dim()));
        }
        for (row, &index) in visible.iter().enumerate().skip(offset).take(page) {
            let marker = if row == cursor { ">" } else { " " };
            println!("{CONTENT_INDENT}{marker} {}", items[index].text);
        }
        if detail_lines > 0 {
            println!();
            let detail = visible.get(cursor).map(|&i| items[i].detail.as_slice()).unwrap_or_default();
            for row in 0..detail_lines {
                match detail.get(row) {
                    // Na largura de agora: o resumo foi montado na da abertura.
                    Some(text) => line(format_args!("{}", console::truncate_str(text, content_width(), "…"))),
                    None => println!(),
                }
            }
        }
        println!();
        let tab = if has_split { " · tab filtra" } else { "" };
        line(format_args!("{}", style(format!("↑/↓ move{tab} · enter confirma")).dim()));

        // As teclas do Select do dialoguer, que este substitui, mais o Tab.
        match term.read_key().context("aguardar tecla")? {
            Key::ArrowUp | Key::Char('k') => cursor = cursor.checked_sub(1).unwrap_or(visible.len().saturating_sub(1)),
            Key::ArrowDown | Key::Char('j') => cursor = if cursor + 1 >= visible.len() { 0 } else { cursor + 1 },
            Key::PageUp | Key::ArrowLeft | Key::Char('h') => cursor = cursor.saturating_sub(page),
            Key::PageDown | Key::ArrowRight | Key::Char('l') => cursor = (cursor + page).min(visible.len().saturating_sub(1)),
            Key::Home => cursor = 0,
            Key::End => cursor = visible.len().saturating_sub(1),
            Key::Tab if has_split => {
                state.filter = next_filter(items, filter);
                cursor = 0;
                offset = 0;
            }
            Key::Enter if !visible.is_empty() => break Some(visible[cursor]),
            Key::Escape | Key::Char('q') => break None,
            _ => {}
        }
    };
    let _ = term.show_cursor();
    state.chosen = picked.or(state.chosen);
    Ok(picked)
}

/// Índices dos itens que o recorte mostra, na ordem original (data).
fn visible(items: &[Item], filter: RestoreFilter) -> Vec<usize> {
    (0..items.len()).filter(|&i| filter.shows(items[i].pacman, items[i].fixed)).collect()
}

/// Recorte com que o seletor abre: o pedido, ou Todos quando ele não separa
/// nada ou não mostraria nada (`RESTORE_FILTER=pacman` sem checkpoint do
/// pacman abriria numa lista vazia).
fn opening_filter(items: &[Item], wanted: RestoreFilter) -> RestoreFilter {
    if !splits(items) || visible(items, wanted).is_empty() {
        return RestoreFilter::All;
    }
    wanted
}

/// Próximo recorte que mostra algo: Todos nunca está vazio quando há o que
/// separar, então o laço sempre para.
fn next_filter(items: &[Item], current: RestoreFilter) -> RestoreFilter {
    let mut next = current.next();
    while visible(items, next).is_empty() {
        next = next.next();
    }
    next
}

/// Algum recorte mostra só parte da lista? Como Fixos, Saves e Pacman não se
/// sobrepõem, isso é a lista ocupar pelo menos dois deles.
fn splits(items: &[Item]) -> bool {
    let occupied = RestoreFilter::ORDER[1..]
        .iter()
        .filter(|f| items.iter().any(|item| f.shows(item.pacman, item.fixed)))
        .count();
    occupied >= 2
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

/// `Mostrar  Todos · [Fixos] · Saves · Pacman`, com o recorte ativo em destaque.
/// Recorte vazio fica fora da linha, como fica fora do Tab.
fn filter_line(items: &[Item], active: RestoreFilter) -> String {
    let options: Vec<String> = RestoreFilter::ORDER
        .iter()
        .filter(|f| !visible(items, **f).is_empty())
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
    use super::{Item, next_filter, opening_filter, scroll, splits, visible};
    use crate::config::RestoreFilter;

    /// `(pacman, fixo)` de cada item.
    fn items(kinds: &[(bool, bool)]) -> Vec<Item> {
        kinds
            .iter()
            .map(|&(pacman, fixed)| Item { text: String::new(), pacman, fixed, detail: Vec::new() })
            .collect()
    }

    #[test]
    fn filters_keep_the_date_order() {
        // Do pacman não fixo, manual fixo, do pacman renomeado, manual não fixo.
        let list = items(&[(true, false), (false, true), (true, true), (false, false)]);
        assert_eq!(visible(&list, RestoreFilter::All), vec![0, 1, 2, 3]);
        assert_eq!(visible(&list, RestoreFilter::Fixed), vec![1, 2]);
        assert_eq!(visible(&list, RestoreFilter::Saves), vec![3]);
        assert_eq!(visible(&list, RestoreFilter::Pacman), vec![0]);
    }

    #[test]
    fn the_three_filters_add_up_to_all() {
        let list = items(&[(true, false), (false, true), (true, true), (false, false), (true, false)]);
        let mut shown: Vec<usize> = RestoreFilter::ORDER[1..].iter().flat_map(|f| visible(&list, *f)).collect();
        shown.sort();
        assert_eq!(shown, visible(&list, RestoreFilter::All));
    }

    #[test]
    fn the_filter_shows_only_when_it_separates_something() {
        assert!(!splits(&items(&[])));
        assert!(!splits(&items(&[(false, true), (false, true)])));
        assert!(!splits(&items(&[(true, false), (true, false)])));
        // Fixos de origens diferentes caem todos em Fixos.
        assert!(!splits(&items(&[(false, true), (true, true)])));
        assert!(splits(&items(&[(false, true), (false, false)])));
        assert!(splits(&items(&[(false, false), (true, false)])));
        assert!(splits(&items(&[(true, true), (true, false)])));
    }

    #[test]
    fn an_empty_starting_filter_opens_on_all() {
        let no_pacman = items(&[(false, true), (false, false)]);
        assert_eq!(opening_filter(&no_pacman, RestoreFilter::Pacman), RestoreFilter::All);
        assert_eq!(opening_filter(&no_pacman, RestoreFilter::Fixed), RestoreFilter::Fixed);
        let fixed_pacman = items(&[(false, false), (true, true)]);
        assert_eq!(opening_filter(&fixed_pacman, RestoreFilter::Pacman), RestoreFilter::All);
        assert_eq!(opening_filter(&fixed_pacman, RestoreFilter::Saves), RestoreFilter::Saves);
    }

    #[test]
    fn tab_skips_empty_filters() {
        // Saves e um pacman renomeado: Pacman está vazio.
        let list = items(&[(false, false), (true, true)]);
        assert_eq!(next_filter(&list, RestoreFilter::Fixed), RestoreFilter::Saves);
        assert_eq!(next_filter(&list, RestoreFilter::Saves), RestoreFilter::All);
        let full = items(&[(false, false), (true, true), (true, false)]);
        assert_eq!(next_filter(&full, RestoreFilter::Saves), RestoreFilter::Pacman);
    }

    #[test]
    fn tab_cycles_through_the_four() {
        assert_eq!(RestoreFilter::All.next(), RestoreFilter::Fixed);
        assert_eq!(RestoreFilter::Fixed.next(), RestoreFilter::Saves);
        assert_eq!(RestoreFilter::Saves.next(), RestoreFilter::Pacman);
        assert_eq!(RestoreFilter::Pacman.next(), RestoreFilter::All);
        for filter in RestoreFilter::ORDER {
            assert_eq!(filter.next().prev(), filter);
        }
    }

    #[test]
    fn the_window_follows_the_cursor() {
        assert_eq!(scroll(0, 3, 5), 0);
        assert_eq!(scroll(0, 7, 5), 3);
        assert_eq!(scroll(3, 1, 5), 1);
    }
}
