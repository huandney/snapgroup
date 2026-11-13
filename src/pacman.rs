//! Entrada do hook do pacman: de onde sai o nome do checkpoint e a âncora no
//! `pacman.log`. O hook roda com `NeedsTargets` (pacotes no stdin) e tem o
//! pacman como processo pai, então a linha de comando dele diz a operação.

use std::fs;
use std::io::{self, BufRead};

const LOG_PATH: &str = "/var/log/pacman.log";
/// Pacotes listados no nome antes do `+N`.
const NAMED_TARGETS: usize = 3;

/// Pacotes da transação, um por linha no stdin. Num terminal (rodado à mão)
/// não há o que ler, e esperar travaria.
pub fn read_targets() -> Vec<String> {
    if unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
        return Vec::new();
    }
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
    let raw = fs::read(format!("/proc/{}/cmdline", std::os::unix::process::parent_id())).ok()?;
    let args: Vec<String> = raw
        .split(|&b| b == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
    let program = args.first()?.rsplit('/').next()?;
    (program == "pacman").then_some(args)
}

/// Tamanho do log agora: a transação que este checkpoint precede é escrita a
/// partir daqui.
pub fn log_offset() -> Option<u64> {
    fs::metadata(LOG_PATH).ok().map(|m| m.len())
}

/// Nome do checkpoint: `pacman -Syu`, `pacman -U openai-codex-bin`,
/// `pacman -S a, b, c +2`. Sem a operação, `pacman: <pacotes>`.
pub fn checkpoint_name(cmdline: Option<&[String]>, targets: &[String]) -> String {
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

/// Operação normalizada: `-Syu`, `-S`, `-U` ou `-R`. Aceita flags curtas
/// combinadas (`-Syu`, `-Rns`) e longas (`--sync --sysupgrade`, como o paru
/// chama). O `u` só é sysupgrade junto do `-S` (no `-R` é `--unneeded`).
fn operation(args: &[String]) -> Option<&'static str> {
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
    use super::checkpoint_name;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    fn pkgs(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn name(line: &str, targets: &[&str]) -> String {
        checkpoint_name(Some(&args(line)), &pkgs(targets))
    }

    #[test]
    fn sysupgrade_hides_the_package_list() {
        assert_eq!(name("pacman --color always -Syu", &["llvm", "clang"]), "pacman -Syu");
        // Como o paru chama.
        assert_eq!(name("/usr/bin/pacman --sync --color=always -y -u --", &["a"]), "pacman -Syu");
        assert_eq!(name("pacman -Syu nibbler", &["nibbler"]), "pacman -Syu");
    }

    #[test]
    fn install_upgrade_and_remove_list_the_packages() {
        assert_eq!(name("pacman -S nibbler", &["nibbler"]), "pacman -S nibbler");
        // O caminho do .pkg.tar.zst fica de fora: o nome vem do stdin.
        assert_eq!(
            name(
                "pacman --upgrade --color=always --noconfirm -- /home/u/.cache/paru/clone/openai-codex-bin/openai-codex-bin-0.169.0-1-x86_64.pkg.tar.zst",
                &["openai-codex-bin"],
            ),
            "pacman -U openai-codex-bin"
        );
        assert_eq!(name("pacman -U --noconfirm snapgroup-0.12.0-1-x86_64.pkg.tar.zst", &["snapgroup"]), "pacman -U snapgroup");
        // No -R, o `u` é --unneeded, não sysupgrade.
        assert_eq!(name("pacman -Rnsu fish-pkg", &["fish-pkg"]), "pacman -R fish-pkg");
    }

    #[test]
    fn long_lists_are_cut_after_three() {
        assert_eq!(name("pacman -S a b c d e", &["a", "b", "c", "d", "e"]), "pacman -S a, b, c +2");
        assert_eq!(name("pacman -S a b c", &["a", "b", "c"]), "pacman -S a, b, c");
    }

    #[test]
    fn unknown_operation_falls_back_to_the_targets() {
        assert_eq!(checkpoint_name(None, &pkgs(&["dkms", "linux"])), "pacman: dkms, linux");
        assert_eq!(checkpoint_name(None, &[]), "pacman");
        assert_eq!(name("pacman -S", &[]), "pacman -S");
        // `-D`/`-Q` não são transações que o hook veria, mas não podem quebrar.
        assert_eq!(name("pacman -D --asdeps x", &["x"]), "pacman: x");
    }
}
