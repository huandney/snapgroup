//! Quem faz os checkpoints do pacman: o snapg ou o snap-pac. Um arquivo em
//! `/etc/pacman.d/hooks` com o nome de um hook de `/usr/share/libalpm/hooks`
//! toma o lugar dele, e um link para `/dev/null` o desliga (alpm-hooks(5)).
//! Assim os dois pacotes ficam instalados e a troca é instantânea.

use anyhow::{Context, Result, bail};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

/// O hook do snapg, instalado pelo pacote em `/usr/share/libalpm/hooks`.
pub const OWN_HOOK: &str = "05-snapgroup-pre.hook";
const SYSTEM_DIR: &str = "/usr/share/libalpm/hooks";
const OVERRIDE_DIR: &str = "/etc/pacman.d/hooks";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HookState {
    /// Não instalado.
    Absent,
    Active,
    /// Desligado por um link para `/dev/null`.
    Masked,
    /// Trocado por um arquivo do usuário em `/etc`: roda, mas não é nosso.
    Replaced,
}

pub struct Dirs {
    pub system: PathBuf,
    pub overrides: PathBuf,
}

impl Dirs {
    pub fn system_default() -> Self {
        Dirs { system: PathBuf::from(SYSTEM_DIR), overrides: PathBuf::from(OVERRIDE_DIR) }
    }
}

pub struct Status {
    pub snapg: HookState,
    pub snap_pac: Vec<(String, HookState)>,
}

impl Status {
    pub fn snapg_runs(&self) -> bool {
        runs(self.snapg)
    }

    pub fn snap_pac_runs(&self) -> bool {
        self.snap_pac.iter().any(|(_, state)| runs(*state))
    }
}

fn runs(state: HookState) -> bool {
    matches!(state, HookState::Active | HookState::Replaced)
}

pub fn status(dirs: &Dirs) -> Status {
    let snap_pac = snap_pac_hooks()
        .into_iter()
        .map(|name| {
            let state = state(dirs, &name);
            (name, state)
        })
        .collect();
    Status { snapg: state(dirs, OWN_HOOK), snap_pac }
}

/// Hooks do pacote snap-pac, pelos arquivos que o pacman diz que são dele (um
/// rename numa atualização do snap-pac continua coberto). Vazio sem snap-pac.
/// `pacman -Q` só lê o banco: roda inclusive de dentro de um hook.
fn snap_pac_hooks() -> Vec<String> {
    let Ok(out) = Command::new("pacman").args(["-Ql", "snap-pac"]).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    hook_names(&String::from_utf8_lossy(&out.stdout))
}

/// Linhas `<pacote> <caminho>` do `pacman -Ql`. O dono tem de ser o próprio
/// snap-pac: o `-Q` também responde por quem só declara `provides=snap-pac`.
fn hook_names(file_list: &str) -> Vec<String> {
    file_list
        .lines()
        .filter_map(|line| line.strip_prefix("snap-pac "))
        .filter_map(|path| path.strip_prefix(SYSTEM_DIR)?.strip_prefix('/'))
        .filter(|name| name.ends_with(".hook") && !name.contains('/'))
        .map(str::to_string)
        .collect()
}

fn state(dirs: &Dirs, name: &str) -> HookState {
    let override_path = dirs.overrides.join(name);
    if is_null_link(&override_path) {
        return HookState::Masked;
    }
    if fs::symlink_metadata(&override_path).is_ok() {
        return HookState::Replaced;
    }
    if dirs.system.join(name).exists() {
        return HookState::Active;
    }
    HookState::Absent
}

fn is_null_link(path: &Path) -> bool {
    fs::read_link(path).is_ok_and(|target| target == Path::new("/dev/null"))
}

/// `snapg pacman on`: desliga os hooks do snap-pac e liga o do snapg.
pub fn enable(dirs: &Dirs, status: &Status) -> Result<()> {
    let snap_pac: Vec<&str> = status.snap_pac.iter().map(|(name, _)| name.as_str()).collect();
    refuse_replaced(dirs, &snap_pac)?;
    for name in &snap_pac {
        mask(dirs, name)?;
    }
    unmask(dirs, OWN_HOOK)
}

/// `snapg pacman off`: religa os hooks do snap-pac e desliga o do snapg.
pub fn disable(dirs: &Dirs, status: &Status) -> Result<()> {
    refuse_replaced(dirs, &[OWN_HOOK])?;
    for (name, _) in &status.snap_pac {
        unmask(dirs, name)?;
    }
    mask(dirs, OWN_HOOK)
}

/// Um hook trocado pelo usuário em `/etc` não é sobrescrito. Checa todos antes
/// de mexer em qualquer um: a troca não fica pela metade.
fn refuse_replaced(dirs: &Dirs, names: &[&str]) -> Result<()> {
    for name in names {
        if state(dirs, name) == HookState::Replaced {
            bail!(
                "{} é um arquivo seu, não um link para /dev/null: não vou sobrescrevê-lo",
                dirs.overrides.join(name).display()
            );
        }
    }
    Ok(())
}

fn mask(dirs: &Dirs, name: &str) -> Result<()> {
    let path = dirs.overrides.join(name);
    if is_null_link(&path) {
        return Ok(());
    }
    fs::create_dir_all(&dirs.overrides)
        .with_context(|| format!("criar {}", dirs.overrides.display()))?;
    symlink("/dev/null", &path).with_context(|| format!("desligar {}", path.display()))
}

/// Só remove o que é link para `/dev/null`; um arquivo do usuário fica.
fn unmask(dirs: &Dirs, name: &str) -> Result<()> {
    let path = dirs.overrides.join(name);
    if !is_null_link(&path) {
        return Ok(());
    }
    fs::remove_file(&path).with_context(|| format!("religar {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{Dirs, HookState, OWN_HOOK, Status, disable, enable, hook_names, state};
    use std::fs;
    use std::os::unix::fs::symlink;

    const PRE: &str = "05-snap-pac-pre.hook";
    const POST: &str = "zz-snap-pac-post.hook";

    fn dirs(tag: &str) -> Dirs {
        let root = std::env::temp_dir().join(format!("snapgroup-hooks-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dirs = Dirs { system: root.join("system"), overrides: root.join("etc") };
        fs::create_dir_all(&dirs.system).unwrap();
        for name in [OWN_HOOK, PRE, POST] {
            fs::write(dirs.system.join(name), "[Trigger]\n").unwrap();
        }
        dirs
    }

    fn status(dirs: &Dirs) -> Status {
        Status {
            snapg: state(dirs, OWN_HOOK),
            snap_pac: [PRE, POST].iter().map(|n| (n.to_string(), state(dirs, n))).collect(),
        }
    }

    #[test]
    fn on_and_off_swap_which_hook_runs() {
        let d = dirs("swap");
        // Recém-instalado: o pacote desliga o hook do snapg.
        fs::create_dir_all(&d.overrides).unwrap();
        symlink("/dev/null", d.overrides.join(OWN_HOOK)).unwrap();
        let before = status(&d);
        assert!(before.snap_pac_runs() && !before.snapg_runs());

        enable(&d, &before).unwrap();
        let on = status(&d);
        assert!(on.snapg_runs() && !on.snap_pac_runs());
        assert_eq!(state(&d, PRE), HookState::Masked);

        // Repetir não falha nem duplica.
        enable(&d, &on).unwrap();

        disable(&d, &status(&d)).unwrap();
        let off = status(&d);
        assert!(off.snap_pac_runs() && !off.snapg_runs());
        assert!(fs::symlink_metadata(d.overrides.join(PRE)).is_err());
        let _ = fs::remove_dir_all(d.system.parent().unwrap());
    }

    #[test]
    fn a_user_override_is_never_touched() {
        let d = dirs("user");
        fs::create_dir_all(&d.overrides).unwrap();
        fs::write(d.overrides.join(POST), "meu hook\n").unwrap();
        let st = status(&d);
        assert_eq!(state(&d, POST), HookState::Replaced);
        assert!(st.snap_pac_runs());

        assert!(enable(&d, &st).is_err());
        // Nada mudou: o PRE continua ativo, o arquivo do usuário intacto.
        assert_eq!(state(&d, PRE), HookState::Active);
        assert_eq!(fs::read_to_string(d.overrides.join(POST)).unwrap(), "meu hook\n");
        let _ = fs::remove_dir_all(d.system.parent().unwrap());
    }

    #[test]
    fn hook_names_come_from_the_package_file_list() {
        let list = "snap-pac /etc/snap-pac.ini\nsnap-pac /usr/share/libalpm/hooks/\n\
                    snap-pac /usr/share/libalpm/hooks/05-snap-pac-pre.hook\n\
                    snap-pac /usr/share/libalpm/hooks/zz-snap-pac-post.hook\n\
                    snap-pac /usr/share/libalpm/scripts/snap-pac\n";
        assert_eq!(hook_names(list), vec![PRE, POST]);
        // Quem só se declara snap-pac (provides) não conta.
        let provider = "snapgroup /usr/share/libalpm/hooks/05-snapgroup-pre.hook\n";
        assert!(hook_names(provider).is_empty());
    }
}
