//! Leitura do `/var/log/pacman.log` a partir da posição que o checkpoint
//! gravou. O log fica no subvolume `@log`, fora do root: sobrevive aos
//! restores. Por isso ele conta o que cada transação fez (`snapg log`, coluna
//! Origem), mas não o estado atual: a prévia do restore compara os bancos de
//! pacotes (`pacman_db`).

use crate::group::{self, Group, GroupId};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;

const LOG_PATH: &str = "/var/log/pacman.log";
/// Pacotes que mexem no boot ou no próprio mecanismo de restore: aparecem
/// primeiro e destacados. Prefixos (`linux` cobre `linux-cachyos-headers`,
/// `systemd` cobre `systemd-libs`).
const BOOT_PREFIXES: &[&str] = &[
    "linux", "nvidia", "dkms", "mkinitcpio", "limine", "systemd", "glibc", "intel-ucode",
    "amd-ucode", "cryptsetup", "btrfs-progs", "snapper", "snapgroup",
];

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
    /// `-Syu`, `-S`, `-U`, `-R`, da linha `Running`: o `-U` de um arquivo é o
    /// que foi compilado (AUR, makepkg).
    pub op: Option<&'static str>,
    pub changes: Vec<Change>,
    /// `.pacnew`/`.pacsave` que a transação deixou.
    pub config_files: Vec<String>,
}

/// Onde a transação de um checkpoint começa: o tamanho do log no save e o
/// inode do arquivo. O inode pega o log trocado por outro (logrotate com
/// `create`); um truncamento no mesmo arquivo só é pego enquanto ele for menor
/// que `offset`. Grupos gravados antes do inode trazem `None`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub offset: u64,
    pub ino: Option<u64>,
}

enum Event {
    Running(String),
    /// `[ALPM] running '05-snapgroup-pre.hook'...`: o hook do snapg rodou para
    /// uma transação, ou seja, ela abre o checkpoint seguinte ou conta nele.
    OwnHook,
    Started,
    Completed,
    Change(Change),
    ConfigFile(String),
}

/// Âncora para um save feito agora: a transação seguinte é escrita a partir
/// do fim atual do log.
pub fn anchor_now() -> Option<Anchor> {
    let meta = fs::metadata(LOG_PATH).ok()?;
    Some(Anchor { offset: meta.len(), ino: Some(meta.ino()) })
}

/// A âncora vale neste log? Outro arquivo (inode diferente) ou um log menor
/// que a posição gravada não são o log em que ela foi gravada.
fn anchor_fits(anchor: Anchor, len: u64, ino: u64) -> bool {
    anchor.offset <= len && anchor.ino.is_none_or(|saved| saved == ino)
}

/// As transações que o checkpoint do pacman cobre: uma, ou as da execução do
/// paru/makepkg. Lê do arquivo só o trecho delas (posiciona na âncora e para
/// no fim), então um checkpoint antigo não custa reler meses de log. A
/// operação da primeira vem do userdata: o `Running` dela fica antes da âncora.
pub fn of_group(group: &Group) -> Option<Vec<Transaction>> {
    let anchor = group::pacman_log_anchor(group)?;
    let mut file = File::open(LOG_PATH).ok()?;
    let meta = file.metadata().ok()?;
    if !anchor_fits(anchor, meta.len(), meta.ino()) {
        return None;
    }
    file.seek(SeekFrom::Start(anchor.offset)).ok()?;
    let mut transactions = read_run(BufReader::new(file), group::pacman_transactions(group));
    if let Some(first) = transactions.first_mut()
        && first.op.is_none()
    {
        first.op = group::pacman_op(group).and_then(known_op);
    }
    Some(transactions)
}

/// Lê, a partir da âncora, as transações de um checkpoint que cobre `count`
/// delas. Para no que vier primeiro: `count` transações, ou o hook do snapg
/// rodando para uma transação que já não é deste checkpoint (a contagem sobe
/// no PreTransaction, e uma transação que aborta depois disso não deixa
/// `started` no log; sem este limite, a leitura invadiria a seguinte).
fn read_run(reader: impl BufRead, count: usize) -> Vec<Transaction> {
    let lines = reader.split(b'\n').map_while(Result::ok).map(|line| Cow::Owned(String::from_utf8_lossy(&line).into_owned()));
    parse_lines(lines, count, Some(count))
}

fn known_op(op: &str) -> Option<&'static str> {
    ["-Syu", "-S", "-U", "-R"].into_iter().find(|known| *known == op)
}


/// Rótulo da coluna Origem: `pacman ↑14` (pacotes da transação do grupo) ou
/// `manual`. Vazio quando nenhum grupo veio do pacman: a coluna não aparece.
pub fn origin_labels(groups: &[Group]) -> HashMap<GroupId, String> {
    if !groups.iter().any(group::is_pacman) {
        return HashMap::new();
    }
    groups.iter().map(|g| (g.id, origin_label(g))).collect()
}

fn origin_label(group: &Group) -> String {
    if !group::is_pacman(group) {
        return "manual".to_string();
    }
    let count = of_group(group).map(|txs| txs.iter().map(|tx| tx.changes.len()).sum::<usize>());
    match count {
        Some(n) => format!("pacman ↑{n}"),
        None => "pacman".to_string(),
    }
}

pub fn is_boot_package(name: &str) -> bool {
    BOOT_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

#[cfg(test)]
fn parse(text: &str) -> Vec<Transaction> {
    parse_lines(text.lines().map(Cow::Borrowed), usize::MAX, None)
}

/// Transações das linhas, até `limit` delas. Com `hook_runs`, para também na
/// transação em que o hook do snapg roda pela `hook_runs`-ésima vez depois da
/// âncora: ela é do checkpoint seguinte e fica de fora.
fn parse_lines<'a>(
    lines: impl Iterator<Item = Cow<'a, str>>,
    limit: usize,
    hook_runs: Option<usize>,
) -> Vec<Transaction> {
    let mut out = Vec::new();
    let mut current = empty_transaction();
    let mut started = false;
    let mut hooks_seen = 0;
    for line in lines {
        if out.len() >= limit {
            return out;
        }
        let Some(event) = parse_line(&line) else {
            continue;
        };
        match event {
            Event::OwnHook => {
                hooks_seen += 1;
                if hook_runs.is_some_and(|runs| hooks_seen >= runs) {
                    return out;
                }
            }
            Event::Running(command) => {
                // A primeira entra mesmo vazia: é a do checkpoint, e vazia
                // quer dizer que abortou antes de começar. As outras vazias
                // são comandos sem transação (`pacman -Sy`).
                if out.is_empty() || started || !current.changes.is_empty() {
                    out.push(close(current, started));
                }
                current = empty_transaction();
                current.op = operation_of(&command);
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
    // Mesma regra no fim do log: a do checkpoint entra mesmo abortada.
    if out.len() < limit && (out.is_empty() || started || !current.changes.is_empty()) {
        out.push(close(current, started));
    }
    out
}

fn empty_transaction() -> Transaction {
    Transaction { state: State::NotStarted, op: None, changes: Vec::new(), config_files: Vec::new() }
}

/// `pacman --upgrade --color=always --noconfirm -- /…/x.pkg.tar.zst` → `-U`.
fn operation_of(command: &str) -> Option<&'static str> {
    let args: Vec<String> = command.split_whitespace().map(str::to_string).collect();
    crate::pacman::operation(&args)
}

/// Fecha uma transação sem `completed`.
fn close(mut transaction: Transaction, started: bool) -> Transaction {
    if transaction.state != State::Completed && started {
        transaction.state = State::Interrupted;
    }
    transaction
}

/// `[2026-11-13T12:59:24-0400] [ALPM] upgraded clang (23.1.4-2 -> 23.1.5-2)`.
/// Só as linhas `[ALPM]` e o `Running` do `[PACMAN]` interessam; a saída dos
/// scriptlets (`[ALPM-SCRIPTLET]`) pode conter qualquer texto.
fn parse_line(line: &str) -> Option<Event> {
    let (_, rest) = line.split_once("] ")?;
    if let Some(command) = rest.strip_prefix("[PACMAN] Running ") {
        return Some(Event::Running(command.trim_matches('\'').to_string()));
    }
    let message = rest.strip_prefix("[ALPM] ")?;
    if message.strip_prefix("running '").is_some_and(|hook| hook.starts_with(crate::pacman_hooks::OWN_HOOK)) {
        return Some(Event::OwnHook);
    }
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
    use super::{Anchor, Change, Op, State, anchor_fits, parse, read_run};
    use std::io::Cursor;

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
    }

    #[test]
    fn checkpoint_aborted_at_the_end_of_the_log_is_still_reported() {
        let log = "[t] [ALPM] running '05-snapgroup-pre.hook'...\n[t] [ALPM] running '20-x.hook'...\n";
        let txs = parse(log);
        assert_eq!(txs.len(), 1);
        assert_eq!(txs[0].state, State::NotStarted);
    }

    /// O log a partir da âncora: o hook grava a posição depois da própria
    /// linha `running`, então o trecho começa na linha seguinte.
    fn after_anchor(text: &str) -> &str {
        text.split_once("05-snapgroup-pre.hook'...\n").map(|(_, rest)| rest).unwrap_or(text)
    }

    fn run(text: &str, count: usize) -> Vec<super::Transaction> {
        read_run(Cursor::new(after_anchor(text).as_bytes().to_vec()), count)
    }

    #[test]
    fn a_checkpoint_reads_only_its_transaction() {
        let text = format!("{KERNEL_UPGRADE}[t] [PACMAN] Running 'pacman -S x'\n[t] [ALPM] transaction started\n[t] [ALPM] installed x (1-1)\n[t] [ALPM] transaction completed\n");
        let txs = run(&text, 1);
        assert_eq!(txs.len(), 1);
        assert_eq!(txs[0].changes.len(), 3);
        assert_eq!(txs[0].changes[2].name, "linux-cachyos");
    }

    #[test]
    fn a_paru_run_reads_its_transactions_with_their_source() {
        let text = format!(
            "{KERNEL_UPGRADE}\
[t] [PACMAN] Running 'pacman --sync --color=always -y -u --'
[t] [PACMAN] Running 'pacman --upgrade --color=always --noconfirm -- /home/u/.cache/paru/clone/x/x-1-1-x86_64.pkg.tar.zst'
[t] [ALPM] running '05-snapgroup-pre.hook'...
[t] [ALPM] transaction started
[t] [ALPM] upgraded x (1-1 -> 2-1)
[t] [ALPM] transaction completed
[t] [PACMAN] Running 'pacman -S other'
[t] [ALPM] running '05-snapgroup-pre.hook'...
[t] [ALPM] transaction started
[t] [ALPM] installed other (1-1)
[t] [ALPM] transaction completed
"
        );
        let txs = run(&text, 2);
        assert_eq!(txs.len(), 2);
        // A primeira não tem `Running` depois da âncora; a do AUR tem.
        assert_eq!(txs[0].op, None);
        assert_eq!(txs[1].op, Some("-U"));
        assert_eq!(txs[1].changes[0].name, "x");
    }

    #[test]
    fn an_aborted_counted_transaction_does_not_pull_in_the_next_checkpoint() {
        // A 2ª transação da execução rodou o hook (a contagem subiu para 2) e
        // abortou; depois veio um `pacman -S foo` com checkpoint próprio.
        let text = format!(
            "{KERNEL_UPGRADE}\
[t] [PACMAN] Running 'pacman --upgrade --noconfirm -- /tmp/a-1-1-x86_64.pkg.tar.zst'
[t] [ALPM] running '05-snapgroup-pre.hook'...
[t] [ALPM] running '10-limine-snapper-lock.hook'...
[t] [PACMAN] Running 'pacman -S foo'
[t] [ALPM] running '05-snapgroup-pre.hook'...
[t] [ALPM] transaction started
[t] [ALPM] installed foo (1-1)
[t] [ALPM] transaction completed
"
        );
        let txs = run(&text, 2);
        assert_eq!(txs.len(), 1);
        assert!(txs.iter().all(|t| t.changes.iter().all(|c| c.name != "foo")));
    }

    #[test]
    fn the_anchor_only_fits_the_log_it_was_saved_in() {
        // Outro arquivo (inode diferente) ou log menor que a posição: não vale.
        assert!(!anchor_fits(Anchor { offset: 0, ino: Some(8) }, 100, 7));
        assert!(!anchor_fits(Anchor { offset: 101, ino: Some(7) }, 100, 7));
        assert!(anchor_fits(Anchor { offset: 100, ino: Some(7) }, 100, 7));
        // Grupo anterior ao inode: só a posição decide.
        assert!(anchor_fits(Anchor { offset: 0, ino: None }, 100, 7));
    }

    #[test]
    fn an_anchor_at_the_end_reads_a_transaction_not_written_yet() {
        assert_eq!(run("", 1)[0].state, State::NotStarted);
    }

    #[test]
    fn boot_packages_match_by_prefix() {
        assert!(super::is_boot_package("linux-cachyos-headers"));
        assert!(super::is_boot_package("systemd-libs"));
        assert!(super::is_boot_package("snapgroup"));
        assert!(!super::is_boot_package("lib32-systemd"));
        assert!(!super::is_boot_package("clang"));
    }

    /// Lê o log real da máquina inteiro: toda transação começada tem de virar
    /// uma `Transaction` começada, e toda concluída, uma concluída.
    #[test]
    #[ignore]
    fn real_log_matches_the_transaction_markers() {
        let raw = std::fs::read(super::LOG_PATH).unwrap();
        let text = String::from_utf8_lossy(&raw).into_owned();
        let txs = parse(&text);
        let started = text.lines().filter(|l| l.ends_with("] [ALPM] transaction started")).count();
        let completed = text.lines().filter(|l| l.ends_with("] [ALPM] transaction completed")).count();
        assert_eq!(txs.iter().filter(|t| t.state != State::NotStarted).count(), started);
        assert_eq!(txs.iter().filter(|t| t.state == State::Completed).count(), completed);
        let changes: usize = txs.iter().map(|t| t.changes.len()).sum();
        println!("{} transações, {started} começadas, {completed} concluídas, {changes} linhas de pacote", txs.len());
    }
}
