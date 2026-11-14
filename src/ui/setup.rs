//! `snapg setup`: os ajustes do snapgroup numa tela só. Uma linha por ajuste,
//! a explicação do item sob o cursor no rodapé, e cada mudança gravada na hora
//! (no `/etc/snapgroup.conf`, ou nos hooks do pacman para o snapg/snap-pac).

use crate::config::{self, Config, RestoreFilter};
use crate::pacman_hooks::{self, Dirs};
use crate::ui::term::{AltScreen, CONTENT_INDENT, clear_screen, header, input_line, line, result, result_section};
use anyhow::{Context, Result};
use console::{Key, style};

#[derive(Clone, Copy, PartialEq)]
enum Item {
    PacmanHook,
    KeepGroups,
    KeepPacman,
    TrashDays,
    RestoreFilter,
}

const ITEMS: [Item; 5] = [Item::PacmanHook, Item::KeepGroups, Item::KeepPacman, Item::TrashDays, Item::RestoreFilter];
/// Valores que ←/→ percorrem; enter digita qualquer outro.
const KEEP_STEPS: &[u64] = &[0, 5, 10, 20, 50];
const DAY_STEPS: &[u64] = &[3, 7, 15, 30, 60, 90];
const NAME_COL: usize = 28;

impl Item {
    fn name(self) -> &'static str {
        match self {
            Item::PacmanHook => "Checkpoints do pacman",
            Item::KeepGroups => "Automáticos mantidos",
            Item::KeepPacman => "Do pacman mantidos",
            Item::TrashDays => "Dias na lixeira",
            Item::RestoreFilter => "Filtro do restore",
        }
    }

    fn help(self) -> &'static str {
        match self {
            Item::PacmanHook => "quem cria o checkpoint antes de cada transação: snapg (um grupo) ou snap-pac",
            Item::KeepGroups => "checkpoints salvos com o nome padrão; os além dos N mais novos vão para a lixeira",
            Item::KeepPacman => "fila própria dos checkpoints do pacman; renomear um deles o fixa",
            Item::TrashDays => "depois disso o grupo é apagado de vez, no próximo save ou delete",
            Item::RestoreFilter => "o que o seletor do restore mostra ao abrir; Tab troca na tela",
        }
    }

    /// Onde o ajuste fica gravado.
    fn source(self) -> String {
        match self {
            Item::PacmanHook => "links em /etc/pacman.d/hooks · o mesmo que snapg pacman on/off".to_string(),
            _ => format!("arquivo: {} · {}", config::PATH, self.key().unwrap_or("")),
        }
    }

    fn key(self) -> Option<&'static str> {
        match self {
            Item::PacmanHook => None,
            Item::KeepGroups => Some("KEEP_GROUPS"),
            Item::KeepPacman => Some("KEEP_PACMAN"),
            Item::TrashDays => Some("TRASH_PRUNE_DAYS"),
            Item::RestoreFilter => Some("RESTORE_FILTER"),
        }
    }

    fn steps(self) -> Option<&'static [u64]> {
        match self {
            Item::KeepGroups | Item::KeepPacman => Some(KEEP_STEPS),
            Item::TrashDays => Some(DAY_STEPS),
            _ => None,
        }
    }
}

/// O que a tela mostra, relido depois de cada mudança.
struct Values {
    config: Config,
    snapg_hook: bool,
}

impl Values {
    fn read() -> Self {
        let status = pacman_hooks::status(&Dirs::system_default());
        Values { config: config::load(), snapg_hook: status.snapg_runs() && !status.snap_pac_runs() }
    }

    fn number(&self, item: Item) -> u64 {
        match item {
            Item::KeepGroups => self.config.keep_groups as u64,
            Item::KeepPacman => self.config.keep_pacman as u64,
            Item::TrashDays => self.config.trash_prune_days,
            _ => 0,
        }
    }

    fn text(&self, item: Item) -> String {
        match item {
            Item::PacmanHook => if self.snapg_hook { "snapg" } else { "snap-pac" }.to_string(),
            Item::KeepGroups | Item::KeepPacman => keep_text(self.number(item)),
            Item::TrashDays => self.number(item).to_string(),
            Item::RestoreFilter => self.config.restore_filter.label().to_string(),
        }
    }
}

fn keep_text(n: u64) -> String {
    if n == 0 { "sem limite".to_string() } else { n.to_string() }
}

/// O próximo valor comum depois de `current` (ou antes, com `forward` falso).
/// Fora da lista, vai para o vizinho mais próximo nessa direção.
fn step(steps: &[u64], current: u64, forward: bool) -> u64 {
    if forward {
        return steps.iter().copied().find(|&v| v > current).unwrap_or(current);
    }
    steps.iter().rev().copied().find(|&v| v < current).unwrap_or(current)
}

fn filter_value(filter: RestoreFilter) -> &'static str {
    match filter {
        RestoreFilter::All => "todos",
        RestoreFilter::Manual => "manuais",
        RestoreFilter::Pacman => "pacman",
    }
}

fn previous_filter(filter: RestoreFilter) -> RestoreFilter {
    filter.next().next()
}

/// Uma mudança feita, para o resumo ao sair: nome, antes e agora.
struct Change {
    item: Item,
    before: String,
    after: String,
}

pub(crate) fn run() -> Result<()> {
    let changes = {
        let _alt = AltScreen::enter();
        edit_loop()?
    };
    print_summary(&changes);
    Ok(())
}

fn edit_loop() -> Result<Vec<Change>> {
    let term = console::Term::stdout();
    let mut cursor = 0usize;
    let mut status: Option<Result<String, String>> = None;
    let mut changes: Vec<Change> = Vec::new();
    let _ = term.hide_cursor();
    loop {
        let values = Values::read();
        render(&values, cursor, status.as_ref());
        let item = ITEMS[cursor];
        let before = values.text(item);
        let applied = match term.read_key().context("aguardar tecla")? {
            Key::ArrowUp | Key::Char('k') => {
                cursor = cursor.checked_sub(1).unwrap_or(ITEMS.len() - 1);
                None
            }
            Key::ArrowDown | Key::Char('j') => {
                cursor = (cursor + 1) % ITEMS.len();
                None
            }
            Key::ArrowRight | Key::Char('l') => Some(change(item, &values, true)),
            Key::ArrowLeft | Key::Char('h') => Some(change(item, &values, false)),
            Key::Enter if item.steps().is_some() => type_value(item, &values)?,
            Key::Enter => Some(change(item, &values, true)),
            Key::Escape | Key::Char('q') => break,
            _ => None,
        };
        let Some(applied) = applied else {
            continue;
        };
        status = Some(applied.map_err(|e| format!("{e:#}")));
        let after = Values::read().text(item);
        if after != before {
            record(&mut changes, item, before, after);
        }
    }
    let _ = term.show_cursor();
    Ok(changes)
}

/// Mudanças seguidas no mesmo item viram uma: o primeiro "antes", o último
/// "agora"; voltar ao valor de partida some com ela.
fn record(changes: &mut Vec<Change>, item: Item, before: String, after: String) {
    if let Some(i) = changes.iter().position(|c| c.item == item) {
        changes[i].after = after;
        if changes[i].after == changes[i].before {
            changes.remove(i);
        }
        return;
    }
    changes.push(Change { item, before, after });
}

/// Aplica ←/→ (ou enter nos que não são número). Devolve a linha de status.
fn change(item: Item, values: &Values, forward: bool) -> Result<String> {
    match item {
        Item::PacmanHook => switch_pacman(!values.snapg_hook),
        Item::RestoreFilter => {
            let current = values.config.restore_filter;
            let next = if forward { current.next() } else { previous_filter(current) };
            save(item, filter_value(next))
        }
        _ => {
            let steps = item.steps().unwrap_or(&[]);
            save(item, &step(steps, values.number(item), forward).to_string())
        }
    }
}

fn switch_pacman(on: bool) -> Result<String> {
    let dirs = Dirs::system_default();
    let before = pacman_hooks::status(&dirs);
    if on {
        pacman_hooks::enable(&dirs, &before)?;
    } else {
        pacman_hooks::disable(&dirs, &before)?;
    }
    pacman_hooks::record_choice(on)?;
    Ok(if on { "gravado: o snapg faz os checkpoints" } else { "gravado: o snap-pac faz os checkpoints" }.to_string())
}

fn save(item: Item, value: &str) -> Result<String> {
    let key = item.key().unwrap_or_default();
    config::set(key, value)?;
    Ok(format!("gravado: {key}={value}"))
}

/// Enter num ajuste numérico: campo para digitar. Esc volta sem mudar.
fn type_value(item: Item, values: &Values) -> Result<Option<Result<String>>> {
    let current = values.number(item).to_string();
    let hint = match item {
        Item::TrashDays => "dias · enter confirma · esc volta",
        _ => "0 = sem limite · enter confirma · esc volta",
    };
    let typed = input_line(item.name(), &current, "", hint, || {
        clear_screen();
        header("Configurações");
        println!();
    })?;
    let Some(typed) = typed else {
        return Ok(None);
    };
    let Ok(number) = typed.trim().parse::<u64>() else {
        return Ok(Some(Err(anyhow::anyhow!("\"{typed}\" não é um número inteiro"))));
    };
    if item == Item::TrashDays && number > config::MAX_PRUNE_DAYS {
        return Ok(Some(Err(anyhow::anyhow!("o máximo é {} dias", config::MAX_PRUNE_DAYS))));
    }
    Ok(Some(save(item, &number.to_string())))
}

fn render(values: &Values, cursor: usize, status: Option<&Result<String, String>>) {
    clear_screen();
    header("Configurações");
    println!();
    for (i, item) in ITEMS.iter().enumerate() {
        let name = format!("{:<NAME_COL$}", item.name());
        let value = values.text(*item);
        if i != cursor {
            println!("{CONTENT_INDENT}  {name}{value}");
            continue;
        }
        println!(
            "{CONTENT_INDENT}> {}{}{}{}",
            style(name).bold(),
            style("◂ ").dim(),
            style(value).bold(),
            style(" ▸").dim()
        );
    }
    println!();
    let item = ITEMS[cursor];
    line(format_args!("{}", style(item.help()).dim()));
    line(format_args!("{}", style(item.source()).dim()));
    match status {
        Some(Ok(done)) => line(format_args!("{}", style(format!("✓ {done}")).green())),
        Some(Err(error)) => line(format_args!("{}", style(format!("! {error}")).yellow())),
        None => println!(),
    }
    println!();
    let enter = if item.steps().is_some() { "enter digita" } else { "enter muda" };
    line(format_args!("{}", style(format!("↑/↓ move · ←/→ muda · {enter} · esc sai")).dim()));
}

fn print_summary(changes: &[Change]) {
    if changes.is_empty() {
        crate::ui::term::note_apart(format_args!("nada mudou"));
        return;
    }
    result_section("Configurações");
    for c in changes {
        result(format_args!(
            "{:<NAME_COL$}{} → {}",
            c.item.name(),
            style(&c.before).dim(),
            style(&c.after).bold()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::{DAY_STEPS, Item, KEEP_STEPS, record, step};

    #[test]
    fn arrows_walk_the_common_values() {
        assert_eq!(step(KEEP_STEPS, 0, true), 5);
        assert_eq!(step(KEEP_STEPS, 20, true), 50);
        assert_eq!(step(KEEP_STEPS, 50, true), 50);
        assert_eq!(step(KEEP_STEPS, 0, false), 0);
        // Valor digitado fora da lista: vai para o vizinho na direção.
        assert_eq!(step(KEEP_STEPS, 12, true), 20);
        assert_eq!(step(KEEP_STEPS, 12, false), 10);
        assert_eq!(step(DAY_STEPS, 15, false), 7);
    }

    #[test]
    fn changes_to_one_item_collapse_and_vanish_when_undone() {
        let mut changes = Vec::new();
        record(&mut changes, Item::KeepGroups, "sem limite".into(), "5".into());
        record(&mut changes, Item::KeepGroups, "5".into(), "10".into());
        assert_eq!(changes.len(), 1);
        assert_eq!((changes[0].before.as_str(), changes[0].after.as_str()), ("sem limite", "10"));
        record(&mut changes, Item::KeepGroups, "10".into(), "sem limite".into());
        assert!(changes.is_empty());
    }
}
