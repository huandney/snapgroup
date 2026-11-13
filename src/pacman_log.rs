//! Leitura do `/var/log/pacman.log` a partir da posição que o checkpoint do
//! pacman gravou. O log fica no subvolume `@log`, fora do root: sobrevive aos
//! restores e guarda inclusive as transações que um restore desfez.
// Consumido pelas telas de detalhe e de prévia do restore (próximo passo).
#![allow(dead_code)]

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

const LOG_PATH: &str = "/var/log/pacman.log";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Upgraded,
    Downgraded,
    Installed,
    Removed,
    Reinstalled,
}

/// Uma linha de pacote do log. `old` é a versão antes da transação (`None`:
/// não estava instalado) e `new` a de depois (`None`: saiu).
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub op: Op,
    pub name: String,
    pub old: Option<String>,
    pub new: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum State {
    /// Nenhum `transaction started`: abortada antes de mexer em pacotes.
    NotStarted,
    /// Começou e não terminou no log (Ctrl-C, queda de energia, ou em curso).
    Interrupted,
    Completed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Transaction {
    pub state: State,
    pub changes: Vec<Change>,
    /// `.pacnew`/`.pacsave` que a transação deixou.
    pub config_files: Vec<String>,
}

/// O que mudou entre o ponto do checkpoint e o fim do log, por pacote. É o
/// que um restore para esse ponto desfaz: `now` volta a ser `before`.
#[derive(Clone, Debug, PartialEq)]
pub struct NetChange {
    pub name: String,
    pub before: Option<String>,
    pub now: Option<String>,
}

enum Event {
    Running,
    Started,
    Completed,
    Change(Change),
    ConfigFile(String),
}

/// Transações do log a partir de `offset` até o fim. A primeira é a do
/// checkpoint: o hook roda depois do `Running` dela e antes do `started`.
/// Log menor que `offset` foi truncado ou trocado: não dá para ancorar.
pub fn read_since(offset: u64) -> Result<Vec<Transaction>> {
    let mut file = File::open(LOG_PATH).with_context(|| format!("abrir {LOG_PATH}"))?;
    let len = file.metadata().with_context(|| format!("ler {LOG_PATH}"))?.len();
    if len < offset {
        bail!("{LOG_PATH} tem {len} bytes, menos que a posição gravada ({offset}): o log foi truncado");
    }
    file.seek(SeekFrom::Start(offset)).with_context(|| format!("posicionar em {LOG_PATH}"))?;
    let mut raw = Vec::new();
    file.read_to_end(&mut raw).with_context(|| format!("ler {LOG_PATH}"))?;
    Ok(parse(&String::from_utf8_lossy(&raw)))
}

pub fn parse(text: &str) -> Vec<Transaction> {
    let mut out = Vec::new();
    let mut current = empty_transaction();
    let mut started = false;
    for line in text.lines() {
        let Some(event) = parse_line(line) else {
            continue;
        };
        match event {
            Event::Running => {
                // A primeira entra mesmo vazia: é a do checkpoint, e vazia
                // quer dizer que abortou antes de começar. As outras vazias
                // são comandos sem transação (`pacman -Sy`).
                if out.is_empty() || started || !current.changes.is_empty() {
                    out.push(close(current, started));
                }
                current = empty_transaction();
                started = false;
            }
            Event::Started => started = true,
            Event::Completed => {
                current.state = State::Completed;
                out.push(current);
                current = empty_transaction();
                started = false;
            }
            Event::Change(change) => current.changes.push(change),
            Event::ConfigFile(path) => current.config_files.push(path),
        }
    }
    if started || !current.changes.is_empty() {
        out.push(close(current, started));
    }
    out
}

fn empty_transaction() -> Transaction {
    Transaction { state: State::NotStarted, changes: Vec::new(), config_files: Vec::new() }
}

/// Fecha uma transação sem `completed`.
fn close(mut transaction: Transaction, started: bool) -> Transaction {
    if transaction.state != State::Completed && started {
        transaction.state = State::Interrupted;
    }
    transaction
}

/// Soma as transações: para cada pacote, a versão antes da primeira mudança e
/// a de agora. Pacote que voltou ao que era (instalado e removido, ou só
/// reinstalado) não aparece.
pub fn net_changes(transactions: &[Transaction]) -> Vec<NetChange> {
    let mut by_name: BTreeMap<&str, NetChange> = BTreeMap::new();
    for change in transactions.iter().flat_map(|t| &t.changes) {
        by_name
            .entry(&change.name)
            .and_modify(|net| net.now = change.new.clone())
            .or_insert_with(|| NetChange {
                name: change.name.clone(),
                before: change.old.clone(),
                now: change.new.clone(),
            });
    }
    by_name.into_values().filter(|net| net.before != net.now).collect()
}

/// Transações que mexeram em pacotes (as abortadas antes de começar não contam).
pub fn started_count(transactions: &[Transaction]) -> usize {
    transactions.iter().filter(|t| t.state != State::NotStarted).count()
}

/// `[2026-11-13T12:59:24-0400] [ALPM] upgraded clang (23.1.4-2 -> 23.1.5-2)`.
/// Só as linhas `[ALPM]` e o `Running` do `[PACMAN]` interessam; a saída dos
/// scriptlets (`[ALPM-SCRIPTLET]`) pode conter qualquer texto.
fn parse_line(line: &str) -> Option<Event> {
    let (_, rest) = line.split_once("] ")?;
    if rest.starts_with("[PACMAN] Running ") {
        return Some(Event::Running);
    }
    let message = rest.strip_prefix("[ALPM] ")?;
    match message {
        "transaction started" => return Some(Event::Started),
        "transaction completed" => return Some(Event::Completed),
        _ => {}
    }
    if let Some(warning) = message.strip_prefix("warning: ") {
        return config_file(warning).map(Event::ConfigFile);
    }
    let (verb, rest) = message.split_once(' ')?;
    let (name, versions) = rest.split_once(" (")?;
    let versions = versions.strip_suffix(')')?;
    let (op, old, new) = match verb {
        "upgraded" | "downgraded" => {
            let (old, new) = versions.split_once(" -> ")?;
            let op = if verb == "upgraded" { Op::Upgraded } else { Op::Downgraded };
            (op, Some(old), Some(new))
        }
        "installed" => (Op::Installed, None, Some(versions)),
        "removed" => (Op::Removed, Some(versions), None),
        "reinstalled" => (Op::Reinstalled, Some(versions), Some(versions)),
        _ => return None,
    };
    Some(Event::Change(Change {
        op,
        name: name.to_string(),
        old: old.map(str::to_string),
        new: new.map(str::to_string),
    }))
}

/// `/etc/x installed as /etc/x.pacnew` e `/etc/x saved as /etc/x.pacsave`.
/// Os outros avisos (`permissions differ`, ...) não deixam arquivo.
fn config_file(warning: &str) -> Option<String> {
    let path = warning
        .split_once(" installed as ")
        .or_else(|| warning.split_once(" saved as "))?
        .1;
    (path.ends_with(".pacnew") || path.ends_with(".pacsave")).then(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::{Change, NetChange, Op, State, net_changes, parse, started_count};

    /// A troca do kernel de 08/11, do jeito que o log a guarda: o hook do
    /// snapg grava a posição depois do `Running` e antes do `started`.
    const KERNEL_UPGRADE: &str = "\
[2026-11-08T10:59:21-0400] [ALPM] running '05-snapgroup-pre.hook'...
[2026-11-08T10:59:21-0400] [ALPM-SCRIPTLET]    pacman -Syu   / /home /root · #1794149973
[2026-11-08T10:59:22-0400] [ALPM] running '71-dkms-remove.hook'...
[2026-11-08T10:59:22-0400] [ALPM-SCRIPTLET] ==> dkms remove --no-depmod nvidia/580.196.07 -k 7.3.1-1-cachyos
[2026-11-08T10:59:23-0400] [ALPM] transaction started
[2026-11-08T10:59:23-0400] [ALPM] upgraded lz4 (1:1.10.0-3 -> 1:1.11.0-1)
[2026-11-08T10:59:24-0400] [ALPM] upgraded cachyos-settings (1:1.4.1-1 -> 1:1.4.2-1)
[2026-11-08T10:59:24-0400] [ALPM-SCRIPTLET] restarting services...
[2026-11-08T10:59:25-0400] [ALPM] upgraded linux-cachyos (7.3.1-1 -> 7.3.3-1)
[2026-11-08T10:59:26-0400] [ALPM] transaction completed
[2026-11-08T10:59:26-0400] [ALPM] running '21-systemd-tmpfiles.hook'...
";

    fn change(op: Op, name: &str, old: Option<&str>, new: Option<&str>) -> Change {
        Change { op, name: name.into(), old: old.map(Into::into), new: new.map(Into::into) }
    }

    #[test]
    fn reads_the_transaction_after_the_anchor() {
        let txs = parse(KERNEL_UPGRADE);
        assert_eq!(txs.len(), 1);
        assert_eq!(txs[0].state, State::Completed);
        assert_eq!(txs[0].changes.len(), 3);
        assert_eq!(
            txs[0].changes[2],
            change(Op::Upgraded, "linux-cachyos", Some("7.3.1-1"), Some("7.3.3-1"))
        );
    }

    #[test]
    fn parses_every_operation_and_config_file() {
        let log = "\
[t] [ALPM] transaction started
[t] [ALPM] installed nibbler (1.2-1)
[t] [ALPM] removed fish-pkg (0.3-2)
[t] [ALPM] reinstalled snapgroup (0.12.0-1)
[t] [ALPM] downgraded dbus (1.16.4-1.1 -> 1.16.2-2.1)
[t] [ALPM] warning: /etc/paru.conf installed as /etc/paru.conf.pacnew
[t] [ALPM] warning: /etc/snap-pac.ini saved as /etc/snap-pac.ini.pacsave
[t] [ALPM] warning: directory permissions differ on /srv/ftp/
[t] [ALPM-SCRIPTLET] upgraded fake (1 -> 2)
[t] [ALPM] transaction completed
";
        let tx = &parse(log)[0];
        assert_eq!(
            tx.changes,
            vec![
                change(Op::Installed, "nibbler", None, Some("1.2-1")),
                change(Op::Removed, "fish-pkg", Some("0.3-2"), None),
                change(Op::Reinstalled, "snapgroup", Some("0.12.0-1"), Some("0.12.0-1")),
                change(Op::Downgraded, "dbus", Some("1.16.4-1.1"), Some("1.16.2-2.1")),
            ]
        );
        assert_eq!(tx.config_files, vec!["/etc/paru.conf.pacnew", "/etc/snap-pac.ini.pacsave"]);
    }

    #[test]
    fn aborted_and_interrupted_transactions_are_marked() {
        // A do checkpoint abortou antes de começar; a seguinte caiu no meio.
        let log = "\
[t] [ALPM] running '10-limine-snapper-lock.hook'...
[t] [PACMAN] Running 'pacman -Sy'
[t] [PACMAN] Running 'pacman -S nibbler'
[t] [ALPM] transaction started
[t] [ALPM] installed nibbler (1.2-1)
";
        let txs = parse(log);
        assert_eq!(txs.len(), 2);
        assert_eq!(txs[0].state, State::NotStarted);
        assert!(txs[0].changes.is_empty());
        assert_eq!(txs[1].state, State::Interrupted);
        assert_eq!(started_count(&txs), 1);
    }

    #[test]
    fn net_changes_keep_first_old_and_last_new() {
        let log = format!(
            "{KERNEL_UPGRADE}\
[t] [PACMAN] Running 'pacman -Syu'
[t] [ALPM] transaction started
[t] [ALPM] upgraded linux-cachyos (7.3.3-1 -> 7.3.4-1)
[t] [ALPM] installed temp (1-1)
[t] [ALPM] reinstalled lz4 (1:1.11.0-1)
[t] [ALPM] transaction completed
[t] [PACMAN] Running 'pacman -R temp cachyos-settings'
[t] [ALPM] transaction started
[t] [ALPM] removed temp (1-1)
[t] [ALPM] removed cachyos-settings (1:1.4.2-1)
[t] [ALPM] transaction completed
[t] [PACMAN] Running 'pacman -U snapgroup.pkg.tar.zst'
[t] [ALPM] transaction started
[t] [ALPM] reinstalled snapgroup (0.12.0-1)
[t] [ALPM] transaction completed
"
        );
        let txs = parse(&log);
        assert_eq!(started_count(&txs), 4);
        let net = |name: &str, before: Option<&str>, now: Option<&str>| NetChange {
            name: name.into(),
            before: before.map(Into::into),
            now: now.map(Into::into),
        };
        // `temp` entrou e saiu, `snapgroup` só foi reinstalado: somem.
        assert_eq!(
            net_changes(&txs),
            vec![
                net("cachyos-settings", Some("1:1.4.1-1"), None),
                net("linux-cachyos", Some("7.3.1-1"), Some("7.3.4-1")),
                net("lz4", Some("1:1.10.0-3"), Some("1:1.11.0-1")),
            ]
        );
    }

    /// Lê o log real da máquina inteiro: toda transação começada tem de virar
    /// uma `Transaction` começada, e toda concluída, uma concluída.
    #[test]
    #[ignore]
    fn real_log_matches_the_transaction_markers() {
        let text = String::from_utf8_lossy(&std::fs::read(super::LOG_PATH).unwrap()).into_owned();
        let txs = parse(&text);
        let started = text.lines().filter(|l| l.ends_with("] [ALPM] transaction started")).count();
        let completed = text.lines().filter(|l| l.ends_with("] [ALPM] transaction completed")).count();
        assert_eq!(started_count(&txs), started);
        assert_eq!(txs.iter().filter(|t| t.state == State::Completed).count(), completed);
        let changes: usize = txs.iter().map(|t| t.changes.len()).sum();
        println!("{} transações, {started} começadas, {completed} concluídas, {changes} linhas de pacote", txs.len());

        // SNAPG_LOG_OFFSET=<bytes>: o que um checkpoint ancorado ali enxerga.
        let Ok(offset) = std::env::var("SNAPG_LOG_OFFSET") else {
            return;
        };
        let txs = super::read_since(offset.parse().unwrap()).unwrap();
        println!(
            "desde {offset}: 1ª {:?} com {} pacotes · {} começadas · {} pacotes líquidos",
            txs[0].state,
            txs[0].changes.len(),
            started_count(&txs),
            net_changes(&txs).len()
        );
    }
}
