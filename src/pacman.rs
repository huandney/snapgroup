//! Entrada do hook do pacman: de onde saem o nome do checkpoint e a execução
//! a que ele pertence. O hook roda com `NeedsTargets` (pacotes no stdin) e
//! tem o pacman como processo pai, então a linha de comando dele diz a
//! operação; acima dele pode estar um ajudante (paru, makepkg).

use std::fs;
use std::io::{self, BufRead};
use std::path::Path;

/// Pacotes listados no nome antes do `+N`.
const NAMED_TARGETS: usize = 3;
/// Ajudantes que fazem várias transações numa execução: o paru faz o `-Syu` e
/// um `-U` por pacote do AUR; o `makepkg -si` instala dependências, o pacote e
/// as remove. Uma execução deles vira um checkpoint só.
const HELPERS: &[&str] = &["paru", "yay", "pikaur", "trizen", "aura", "makepkg"];
/// Quantos processos subir procurando o ajudante (pacman ← sudo ← paru, com
/// folga para um sudo com pty ou um shell no meio).
const HELPER_DEPTH: usize = 6;

/// Pacotes da transação, um por linha no stdin (`NeedsTargets`). Só chamar
/// com o pacman como pai: ele fecha o stdin depois de escrever a lista.
pub fn read_targets() -> Vec<String> {
    io::stdin()
        .lock()
        .lines()
        .map_while(Result::ok)
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

/// Argumentos do processo pai, se ele for o pacman.
pub fn parent_cmdline() -> Option<Vec<String>> {
    let args = cmdline_of(std::os::unix::process::parent_id())?;
    let program = args.first()?.rsplit('/').next()?;
    (program == "pacman").then_some(args)
}

fn cmdline_of(pid: u32) -> Option<Vec<String>> {
    let raw = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(
        raw.split(|&b| b == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect(),
    )
}

/// Um ajudante acima do pacman. `session` (`paru:<pid>-<início>`) identifica a
/// execução: um PID reaproveitado depois tem outro início.
pub struct Helper {
    pub name: String,
    /// Argumentos depois do programa (`-S google-chrome`).
    pub args: Vec<String>,
    pub session: String,
    /// Diretório de trabalho: no makepkg, o nome do pacote sendo compilado.
    pub workdir: Option<String>,
}

/// O ajudante acima do pacman que chamou o hook, se houver.
pub fn helper_above_parent() -> Option<Helper> {
    let mut pid = stat_of(std::os::unix::process::parent_id())?.ppid;
    for _ in 0..HELPER_DEPTH {
        if pid <= 1 {
            return None;
        }
        let stat = stat_of(pid)?;
        if HELPERS.contains(&stat.comm.as_str()) {
            return Some(helper(pid, &stat));
        }
        pid = stat.ppid;
    }
    None
}

fn helper(pid: u32, stat: &Stat) -> Helper {
    let argv = cmdline_of(pid).unwrap_or_default();
    // O makepkg é um script: o argv começa pelo bash.
    let program = argv
        .iter()
        .position(|arg| arg.rsplit('/').next() == Some(stat.comm.as_str()))
        .unwrap_or(0);
    let workdir = fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .and_then(|dir| dir.file_name().map(|name| name.to_string_lossy().into_owned()));
    Helper {
        name: stat.comm.clone(),
        args: argv.into_iter().skip(program + 1).collect(),
        session: format!("{}:{pid}-{}", stat.comm, stat.start),
        workdir,
    }
}

struct Stat {
    comm: String,
    ppid: u32,
    start: u64,
}

/// `/proc/<pid>/stat`: `pid (comm) estado ppid ... início(22º campo)`. O comm
/// pode ter espaços e parênteses, então os campos contam do último `)`.
fn stat_of(pid: u32) -> Option<Stat> {
    let raw = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_stat(&raw)
}

fn parse_stat(raw: &str) -> Option<Stat> {
    let open = raw.find('(')?;
    let close = raw.rfind(')')?;
    let fields: Vec<&str> = raw.get(close + 1..)?.split_whitespace().collect();
    Some(Stat {
        comm: raw.get(open + 1..close)?.to_string(),
        ppid: fields.get(1)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
    })
}

/// O pacman está mostrando cores? O hook escreve num pipe do pacman, então o
/// próprio terminal do snapg não diz: vale o `--color` do comando, senão a
/// opção `Color` do pacman.conf com o pacman num terminal.
pub fn pacman_shows_color(cmdline: &[String]) -> bool {
    let mut args = cmdline.iter();
    while let Some(arg) = args.next() {
        let value = match arg.strip_prefix("--color") {
            Some("") => args.next().map(String::as_str),
            Some(rest) => rest.strip_prefix('='),
            None => continue,
        };
        match value {
            Some("always") => return true,
            Some("never") => return false,
            _ => break,
        }
    }
    let on_terminal = fs::read_link(format!("/proc/{}/fd/1", std::os::unix::process::parent_id()))
        .is_ok_and(|out| out.starts_with("/dev/pts") || out.starts_with("/dev/tty"));
    on_terminal && pacman_conf_has_color(Path::new("/etc/pacman.conf"))
}

fn pacman_conf_has_color(path: &Path) -> bool {
    fs::read_to_string(path).is_ok_and(|conf| conf.lines().any(|line| line.trim() == "Color"))
}

/// Nome do checkpoint, dito pelo que vem depois dele: `Antes de paru -Syu`,
/// `Antes de pacman -U openai-codex-bin`, `Antes de makepkg -si (snapgroup)`.
/// O snapshot é tirado antes da transação: o nome sozinho parecia dizer o que
/// já estava dentro dele.
pub fn checkpoint_name(cmdline: Option<&[String]>, targets: &[String], helper: Option<&Helper>) -> String {
    let what = match helper {
        Some(helper) => helper_label(helper),
        None => pacman_label(cmdline, targets),
    };
    format!("Antes de {what}")
}

fn pacman_label(cmdline: Option<&[String]>, targets: &[String]) -> String {
    let op = cmdline.and_then(operation);
    if op == Some("-Syu") {
        return "pacman -Syu".to_string();
    }
    let listed = target_list(targets);
    match (op, listed.is_empty()) {
        (Some(op), true) => format!("pacman {op}"),
        (Some(op), false) => format!("pacman {op} {listed}"),
        (None, true) => "pacman".to_string(),
        (None, false) => format!("pacman: {listed}"),
    }
}

/// `paru` sem argumentos é um `-Syu`; `paru -S x` lista os pacotes; o
/// makepkg leva as flags e a pasta do pacote.
fn helper_label(helper: &Helper) -> String {
    let name = &helper.name;
    if name == "makepkg" {
        let flags = helper.args.join(" ");
        let flags = if flags.is_empty() { String::new() } else { format!(" {flags}") };
        return match &helper.workdir {
            Some(dir) => format!("makepkg{flags} ({dir})"),
            None => format!("makepkg{flags}"),
        };
    }
    let argv: Vec<String> = std::iter::once(name.clone()).chain(helper.args.iter().cloned()).collect();
    let packages: Vec<String> = helper
        .args
        .iter()
        .filter(|arg| !arg.starts_with('-'))
        .map(|arg| package_of_arg(arg))
        .collect();
    match (operation(&argv), packages.is_empty()) {
        // Só `paru` sem nada é um -Syu: `paru -c` (limpa órfãos) é remoção.
        (Some("-Syu"), _) => format!("{name} -Syu"),
        (None, true) if helper.args.is_empty() => format!("{name} -Syu"),
        (None, true) => format!("{name} {}", helper.args.join(" ")),
        (Some(op), true) => format!("{name} {op}"),
        (Some(op), false) => format!("{name} {op} {}", target_list(&packages)),
        (None, false) => format!("{name} {}", target_list(&packages)),
    }
}

/// Operação normalizada: `-Syu`, `-S`, `-U` ou `-R`. Aceita flags curtas
/// combinadas (`-Syu`, `-Rns`) e longas (`--sync --sysupgrade`, como o paru
/// chama). O `u` só é sysupgrade junto do `-S` (no `-R` é `--unneeded`).
pub fn operation(args: &[String]) -> Option<&'static str> {
    let mut op = None;
    let mut sysupgrade = false;
    for arg in args.iter().skip(1) {
        if arg == "--" {
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            match long {
                "sync" => op = Some('S'),
                "upgrade" => op = Some('U'),
                "remove" => op = Some('R'),
                "sysupgrade" => sysupgrade = true,
                _ => {}
            }
            continue;
        }
        let Some(short) = arg.strip_prefix('-') else {
            continue;
        };
        for flag in short.chars() {
            match flag {
                'S' | 'U' | 'R' => op = Some(flag),
                'u' => sysupgrade = true,
                _ => {}
            }
        }
    }
    match op? {
        'S' if sysupgrade => Some("-Syu"),
        'S' => Some("-S"),
        'U' => Some("-U"),
        'R' => Some("-R"),
        _ => None,
    }
}

/// `./x/snapgroup-0.12.0-1-x86_64.pkg.tar.zst` → `snapgroup`; um nome de
/// pacote passa como está. O arquivo leva `<nome>-<versão>-<release>-<arch>`,
/// e versão, release e arch não têm hífen.
fn package_of_arg(arg: &str) -> String {
    let file = arg.rsplit('/').next().unwrap_or(arg);
    let Some((stem, _)) = file.split_once(".pkg.tar") else {
        return file.to_string();
    };
    let mut parts = stem.rsplitn(4, '-');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(_arch), Some(_rel), Some(_ver), Some(name)) => name.to_string(),
        _ => stem.to_string(),
    }
}

fn target_list(targets: &[String]) -> String {
    let named = targets[..targets.len().min(NAMED_TARGETS)].join(", ");
    let rest = targets.len().saturating_sub(NAMED_TARGETS);
    if rest == 0 {
        return named;
    }
    format!("{named} +{rest}")
}

#[cfg(test)]
mod tests {
    use super::{Helper, checkpoint_name, pacman_shows_color, parse_stat};

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    fn pkgs(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn name(line: &str, targets: &[&str]) -> String {
        checkpoint_name(Some(&args(line)), &pkgs(targets), None)
    }

    #[test]
    fn sysupgrade_hides_the_package_list() {
        assert_eq!(name("pacman --color always -Syu", &["llvm", "clang"]), "Antes de pacman -Syu");
        // Como o paru chama.
        assert_eq!(name("/usr/bin/pacman --sync --color=always -y -u --", &["a"]), "Antes de pacman -Syu");
        assert_eq!(name("pacman -Syu nibbler", &["nibbler"]), "Antes de pacman -Syu");
    }

    #[test]
    fn install_upgrade_and_remove_list_the_packages() {
        assert_eq!(name("pacman -S nibbler", &["nibbler"]), "Antes de pacman -S nibbler");
        // O caminho do .pkg.tar.zst fica de fora: o nome vem do stdin.
        assert_eq!(
            name(
                "pacman --upgrade --color=always --noconfirm -- /home/u/.cache/paru/clone/openai-codex-bin/openai-codex-bin-0.169.0-1-x86_64.pkg.tar.zst",
                &["openai-codex-bin"],
            ),
            "Antes de pacman -U openai-codex-bin"
        );
        assert_eq!(name("pacman -U --noconfirm snapgroup-0.12.0-1-x86_64.pkg.tar.zst", &["snapgroup"]), "Antes de pacman -U snapgroup");
        // No -R, o `u` é --unneeded, não sysupgrade.
        assert_eq!(name("pacman -Rnsu fish-pkg", &["fish-pkg"]), "Antes de pacman -R fish-pkg");
    }

    #[test]
    fn long_lists_are_cut_after_three() {
        assert_eq!(name("pacman -S a b c d e", &["a", "b", "c", "d", "e"]), "Antes de pacman -S a, b, c +2");
        assert_eq!(name("pacman -S a b c", &["a", "b", "c"]), "Antes de pacman -S a, b, c");
    }

    #[test]
    fn unknown_operation_falls_back_to_the_targets() {
        assert_eq!(checkpoint_name(None, &pkgs(&["dkms", "linux"]), None), "Antes de pacman: dkms, linux");
        assert_eq!(checkpoint_name(None, &[], None), "Antes de pacman");
        assert_eq!(name("pacman -S", &[]), "Antes de pacman -S");
        // `-D`/`-Q` não são transações que o hook veria, mas não podem quebrar.
        assert_eq!(name("pacman -D --asdeps x", &["x"]), "Antes de pacman: x");
    }

    fn helper(name: &str, args: &str, workdir: Option<&str>) -> Helper {
        Helper {
            name: name.into(),
            args: args.split_whitespace().map(str::to_string).collect(),
            session: "1-1".into(),
            workdir: workdir.map(Into::into),
        }
    }

    #[test]
    fn helpers_name_the_whole_run() {
        let named = |h: Helper| checkpoint_name(Some(&args("pacman -U x.pkg.tar.zst")), &pkgs(&["x"]), Some(&h));
        assert_eq!(named(helper("paru", "", None)), "Antes de paru -Syu");
        assert_eq!(named(helper("paru", "-Syu", None)), "Antes de paru -Syu");
        assert_eq!(named(helper("paru", "-S google-chrome", None)), "Antes de paru -S google-chrome");
        assert_eq!(named(helper("yay", "-Sua", None)), "Antes de yay -Syu");
        assert_eq!(named(helper("paru", "chrome", None)), "Antes de paru chrome");
        assert_eq!(named(helper("paru", "-c", None)), "Antes de paru -c");
        assert_eq!(named(helper("yay", "-Yc", None)), "Antes de yay -Yc");
        assert_eq!(named(helper("makepkg", "-si", Some("snapgroup"))), "Antes de makepkg -si (snapgroup)");
        // Arquivo de pacote no argumento: só o nome do pacote.
        assert_eq!(
            named(helper("paru", "-U /tmp/out/linux-cachyos-headers-7.3.3-1-x86_64.pkg.tar.zst", None)),
            "Antes de paru -U linux-cachyos-headers"
        );
        assert_eq!(named(helper("paru", "/tmp/out/snapgroup-0.12.0-1-x86_64.pkg.tar.zst", None)), "Antes de paru snapgroup");
    }

    #[test]
    fn stat_fields_count_from_the_last_paren() {
        let raw = "4242 (my (odd) prog) S 4100 4242 4242 0 -1 4194560 1 0 0 0 0 0 0 0 20 0 1 0 987654 0 0";
        let stat = parse_stat(raw).unwrap();
        assert_eq!(stat.comm, "my (odd) prog");
        assert_eq!(stat.ppid, 4100);
        assert_eq!(stat.start, 987654);
    }

    #[test]
    fn color_follows_the_pacman_flag() {
        assert!(pacman_shows_color(&args("pacman --color always -Syu")));
        assert!(pacman_shows_color(&args("pacman --sync --color=always -y -u --")));
        assert!(!pacman_shows_color(&args("pacman --color never -Syu")));
        assert!(!pacman_shows_color(&args("pacman --color=never -S x")));
    }
}
