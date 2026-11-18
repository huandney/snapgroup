//! Pacotes instalados num root, pelo banco local do pacman
//! (`var/lib/pacman/local/<nome>-<versão>-<release>/`). O snapshot do root
//! carrega o banco do instante em que foi feito: comparado com o do sistema
//! atual, diz exatamente o que um restore troca, inclusive depois de outros
//! restores (o pacman.log sobrevive a eles e não diz o estado atual).

use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

const LOCAL_DB: &str = "var/lib/pacman/local";

/// Um pacote cuja versão o restore troca: `now` (sistema atual) volta a ser
/// `before` (o snapshot). `None` de um lado: não está instalado ali.
#[derive(Clone, Debug, PartialEq)]
pub struct NetChange {
    pub name: String,
    pub before: Option<String>,
    pub now: Option<String>,
    /// Compilado nesta máquina (AUR, makepkg): o pacote não tem assinatura.
    pub built_here: bool,
}

/// O que muda nos pacotes se o root voltar para `snapshot_root`.
pub fn changes_since(snapshot_root: &Path) -> Result<Vec<NetChange>> {
    let mut nets = diff(&installed(snapshot_root)?, &installed(Path::new("/"))?);
    for net in &mut nets {
        // A versão instalada agora decide; um pacote que só o snapshot tem, a de lá.
        let (root, version) = match (&net.now, &net.before) {
            (Some(now), _) => (Path::new("/"), now),
            (None, Some(before)) => (snapshot_root, before),
            (None, None) => continue,
        };
        let desc = root.join(LOCAL_DB).join(format!("{}-{version}", net.name)).join("desc");
        net.built_here = fs::read_to_string(desc).is_ok_and(|text| unsigned(&text));
    }
    Ok(nets)
}

/// O `desc` do banco local diz como o pacman validou o pacote: `pgp` ou
/// `sha256` quando veio de um repositório, `none` quando foi um `-U` de um
/// pacote sem assinatura, que é o que o makepkg gera.
fn unsigned(desc: &str) -> bool {
    let mut lines = desc.lines();
    lines.by_ref().find(|line| *line == "%VALIDATION%").is_some()
        && lines.take_while(|line| !line.is_empty()).any(|line| line == "none")
}

/// Nome → versão (`1:1.11.0-1`, com epoch e release) dos pacotes em `root`.
fn installed(root: &Path) -> Result<BTreeMap<String, String>> {
    let dir = root.join(LOCAL_DB);
    let mut packages = BTreeMap::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("ler {}", dir.display()))? {
        let entry = entry.with_context(|| format!("ler {}", dir.display()))?;
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue; // ALPM_DB_VERSION
        }
        let name = entry.file_name();
        if let Some((package, version)) = split_entry(&name.to_string_lossy()) {
            packages.insert(package.to_string(), version.to_string());
        }
    }
    Ok(packages)
}

/// `linux-cachyos-headers-7.3.3-1` → (`linux-cachyos-headers`, `7.3.3-1`). O
/// pacman proíbe hífen em pkgver e pkgrel, então os dois últimos separam.
fn split_entry(entry: &str) -> Option<(&str, &str)> {
    let (rest, _release) = entry.rsplit_once('-')?;
    let (name, _version) = rest.rsplit_once('-')?;
    Some((name, &entry[name.len() + 1..]))
}

fn diff(before: &BTreeMap<String, String>, now: &BTreeMap<String, String>) -> Vec<NetChange> {
    let names: BTreeSet<&String> = before.keys().chain(now.keys()).collect();
    names
        .into_iter()
        .filter(|name| before.get(*name) != now.get(*name))
        .map(|name| NetChange {
            name: name.clone(),
            before: before.get(name).cloned(),
            now: now.get(name).cloned(),
            built_here: false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{NetChange, diff, split_entry, unsigned};
    use std::collections::BTreeMap;

    #[test]
    fn entries_split_on_the_last_two_hyphens() {
        assert_eq!(split_entry("linux-cachyos-headers-7.3.3-1"), Some(("linux-cachyos-headers", "7.3.3-1")));
        assert_eq!(split_entry("lz4-1:1.11.0-1"), Some(("lz4", "1:1.11.0-1")));
        assert_eq!(split_entry("antigravity-cli-1.5.7_6190022064472064-1"), Some(("antigravity-cli", "1.5.7_6190022064472064-1")));
        assert_eq!(split_entry("ALPM_DB_VERSION"), None);
    }

    #[test]
    fn diff_lists_only_what_changes() {
        let map = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect()
        };
        let snapshot = map(&[("linux", "7.3.1-1"), ("same", "1-1"), ("snap-pac", "3.0.1-3")]);
        let live = map(&[("linux", "7.3.3-1"), ("same", "1-1"), ("nibbler", "1.2-1")]);
        let net = |name: &str, before: Option<&str>, now: Option<&str>| NetChange {
            name: name.into(),
            before: before.map(Into::into),
            now: now.map(Into::into),
            built_here: false,
        };
        assert_eq!(
            diff(&snapshot, &live),
            vec![
                net("linux", Some("7.3.1-1"), Some("7.3.3-1")),
                net("nibbler", None, Some("1.2-1")),
                net("snap-pac", Some("3.0.1-3"), None),
            ]
        );
    }

    #[test]
    fn unsigned_packages_were_built_here() {
        let desc = |validation: &str| format!("%NAME%\nlimine\n\n%VALIDATION%\n{validation}\n\n%DEPENDS%\nnone\n");
        assert!(unsigned(&desc("none")));
        assert!(!unsigned(&desc("pgp")));
        assert!(!unsigned(&desc("sha256")));
        // O `none` de outro campo não conta.
        assert!(!unsigned("%NAME%\nx\n\n%DEPENDS%\nnone\n"));
    }
}
