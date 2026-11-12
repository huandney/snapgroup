//! Backups órfãos: `<base>_backup_<label>` que restores de outras ferramentas
//! (e versões antigas do snapg) deixam no top-level. Ficam fora do Snapper,
//! então nem `KEEP_GROUPS` nem a lixeira os alcançam; só o `delete` os apaga.

use crate::{boot, btrfs};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::process::Command;

const BACKUP_INFIX: &str = "_backup_";

/// Backups do mesmo restore (mesmo label) em uma ou mais bases.
pub struct OrphanBackup {
    pub label: String,
    /// Nomes no top-level, na ordem das bases.
    pub subvols: Vec<String>,
    pub bases: Vec<String>,
    /// `Creation time` do btrfs do primeiro membro (ex: "2026-04-30 17:32:41 -0400").
    pub created: String,
}

/// Órfãos das `bases` (subvols das configs do Snapper) sob o top-level montado
/// em `toplevel`, mais antigo primeiro. Ficam de fora o default, os montados,
/// os citados pelo bootloader e os que guardam subvolume aninhado com dados.
pub fn find(toplevel: &Path, bases: &[String]) -> Result<Vec<OrphanBackup>> {
    let entries = toplevel_entries(toplevel)?;
    let default = default_subvol(toplevel)?;
    let mounted = mounted_btrfs_roots()?;
    let refs = bootloader_references(Path::new("/boot"));

    let mut found = Vec::new();
    for (label, members) in group_candidates(&entries, bases) {
        let mut subvols = Vec::new();
        let mut kept_bases = Vec::new();
        for (base, name) in members {
            if is_protected(&name, default.as_deref(), &mounted, &refs)
                || !btrfs::is_subvolume(&toplevel.join(&name))
                || btrfs::nested_with_data(toplevel, &name)?.is_some()
            {
                continue;
            }
            subvols.push(name);
            kept_bases.push(base);
        }
        let Some(first) = subvols.first() else {
            continue;
        };
        let created = btrfs::subvol_creation_time(&toplevel.join(first)).unwrap_or_default();
        found.push(OrphanBackup {
            label,
            subvols,
            bases: kept_bases,
            created,
        });
    }
    found.sort_by(|a, b| a.created.cmp(&b.created));
    Ok(found)
}

fn toplevel_entries(toplevel: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(toplevel).with_context(|| format!("ler {}", toplevel.display()))? {
        if let Some(name) = entry?.file_name().to_str() {
            names.push(name.to_string());
        }
    }
    Ok(names)
}

/// Agrupa `<base>_backup_<label>` por label, membros na ordem de `bases`.
/// Prefixo exato: `@home_backup_x` não casa com a base `@`.
fn group_candidates(entries: &[String], bases: &[String]) -> Vec<(String, Vec<(String, String)>)> {
    let mut groups: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for base in bases {
        let prefix = format!("{base}{BACKUP_INFIX}");
        let mut names: Vec<&String> = entries.iter().filter(|e| e.starts_with(&prefix)).collect();
        names.sort();
        for name in names {
            let label = &name[prefix.len()..];
            if label.is_empty() {
                continue;
            }
            let member = (base.clone(), name.clone());
            match groups.iter_mut().find(|(l, _)| l == label) {
                Some((_, members)) => members.push(member),
                None => groups.push((label.to_string(), vec![member])),
            }
        }
    }
    groups
}

fn is_protected(name: &str, default: Option<&str>, mounted: &HashSet<String>, refs: &str) -> bool {
    default == Some(name) || mounted.contains(name) || refs.contains(name)
}

/// Subvolume default do filesystem, se não for o próprio top-level (ID 5).
fn default_subvol(toplevel: &Path) -> Result<Option<String>> {
    let out = Command::new("btrfs")
        .args(["subvolume", "get-default"])
        .arg(toplevel)
        .output()
        .context("btrfs subvolume get-default falhou")?;
    if !out.status.success() {
        anyhow::bail!(
            "get-default {}: {}",
            toplevel.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(parse_default_subvol(&String::from_utf8_lossy(&out.stdout)))
}

/// `ID 5 (FS_TREE)` → nenhum; `ID 256 gen 9 top level 5 path @` → `@`.
fn parse_default_subvol(output: &str) -> Option<String> {
    let (_, path) = output.trim().split_once(" path ")?;
    Some(path.trim().to_string())
}

fn mounted_btrfs_roots() -> Result<HashSet<String>> {
    let content =
        fs::read_to_string("/proc/self/mountinfo").context("ler /proc/self/mountinfo")?;
    Ok(parse_mounted_btrfs_roots(&content))
}

/// Subvolume de topo de cada montagem btrfs: o 1º componente do campo `root`
/// (`/@home` → `@home`). O top-level montado (`/`) não conta.
fn parse_mounted_btrfs_roots(mountinfo: &str) -> HashSet<String> {
    let mut roots = HashSet::new();
    for line in mountinfo.lines() {
        let Some((head, tail)) = line.split_once(" - ") else {
            continue;
        };
        if tail.split_whitespace().next() != Some("btrfs") {
            continue;
        }
        let Some(root) = head.split_whitespace().nth(3) else {
            continue;
        };
        let Some(top) = root.trim_start_matches('/').split('/').next() else {
            continue;
        };
        if !top.is_empty() {
            roots.insert(top.to_string());
        }
    }
    roots
}

/// Texto do `limine.conf` e dos `snapshots.json` do limine-snapper-sync: um
/// backup citado ali pode ser alvo de uma entrada de boot. Arquivo ilegível
/// só não protege nada; o resto das proteções continua valendo.
fn bootloader_references(boot: &Path) -> String {
    let mut text = fs::read_to_string(boot::limine_conf_path(boot)).unwrap_or_default();
    let Ok(dirs) = fs::read_dir(boot) else {
        return text;
    };
    for dir in dirs.flatten() {
        let history = dir.path().join("limine_history/snapshots.json");
        if let Ok(json) = fs::read_to_string(history) {
            text.push('\n');
            text.push_str(&json);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{group_candidates, is_protected, parse_default_subvol, parse_mounted_btrfs_roots};
    use std::collections::HashSet;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn groups_backups_by_label_in_base_order() {
        let entries = strings(&[
            "@",
            "@home",
            "@home_backup_2026-04-26_20:58:44",
            "@_backup_2026-04-26_20:58:44",
            "@root_backup_2026-04-26_20:58:44",
            "@_backup_2026-11-08T03:03:06.978Z",
            "@home_backup_2026-11-08T03:03:12.448Z",
            "@_snapg_regret",
            "@srv_backup_2026-01-01_00:00:00",
            "@_backup_",
        ]);
        let bases = strings(&["@", "@home", "@root"]);
        let groups = group_candidates(&entries, &bases);
        let labels: Vec<&str> = groups.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "2026-04-26_20:58:44",
                "2026-11-08T03:03:06.978Z",
                "2026-11-08T03:03:12.448Z",
            ]
        );
        let first: Vec<&str> = groups[0].1.iter().map(|(b, _)| b.as_str()).collect();
        assert_eq!(first, vec!["@", "@home", "@root"]);
        assert_eq!(groups[0].1[1].1, "@home_backup_2026-04-26_20:58:44");
    }

    #[test]
    fn base_prefix_is_exact() {
        let entries = strings(&["@home_backup_x"]);
        assert!(group_candidates(&entries, &strings(&["@"])).is_empty());
        assert_eq!(group_candidates(&entries, &strings(&["@home"])).len(), 1);
    }

    #[test]
    fn default_subvol_parses_both_shapes() {
        assert_eq!(parse_default_subvol("ID 5 (FS_TREE)\n"), None);
        assert_eq!(
            parse_default_subvol("ID 399 gen 12 top level 5 path @_backup_2026-04-26_20:58:44\n"),
            Some("@_backup_2026-04-26_20:58:44".to_string())
        );
    }

    #[test]
    fn mounted_roots_keep_only_btrfs_top_components() {
        let info = "\
42 1 0:35 /@ / rw,noatime shared:1 - btrfs /dev/nvme0n1p4 rw,subvol=/@
62 42 0:35 /@home /home rw shared:158 - btrfs /dev/nvme0n1p4 rw,subvol=/@home
70 42 0:35 /@_backup_x/var /mnt/x rw shared:9 - btrfs /dev/nvme0n1p4 rw
80 42 0:35 / /run/snapgroup/u rw shared:10 - btrfs /dev/nvme0n1p4 rw,subvolid=5
90 42 0:40 /@weird /tmp rw shared:11 - tmpfs tmpfs rw
";
        let roots = parse_mounted_btrfs_roots(info);
        let expected: HashSet<String> =
            ["@", "@home", "@_backup_x"].iter().map(|s| s.to_string()).collect();
        assert_eq!(roots, expected);
    }

    /// Descoberta e deleção num btrfs de verdade. Rodar como root, com um
    /// btrfs vazio montado pelo top-level:
    ///   cargo test --no-run && sudo SNAPG_TEST_BTRFS=/mnt/x <binário> --ignored
    #[test]
    #[ignore = "precisa de root e de um btrfs vazio montado em SNAPG_TEST_BTRFS"]
    fn finds_and_deletes_orphans_on_real_btrfs() {
        use std::path::PathBuf;
        use std::process::{Command, Stdio};
        let top = PathBuf::from(std::env::var("SNAPG_TEST_BTRFS").expect("SNAPG_TEST_BTRFS"));
        let run = |cmd: &str, args: &[&str]| {
            let status = Command::new(cmd)
                .args(args)
                .current_dir(&top)
                .stdout(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "{cmd} {args:?}");
        };
        for name in ["@", "@home", "@_backup_a", "@home_backup_a", "@_backup_m", "@_backup_n", "@_backup_e"] {
            run("btrfs", &["subvolume", "create", name]);
        }
        run("btrfs", &["subvolume", "create", "@_backup_n/aninhado"]);
        std::fs::write(top.join("@_backup_n/aninhado/dado"), "x").unwrap();
        run("btrfs", &["subvolume", "create", "@_backup_e/vazio"]);
        std::fs::create_dir(top.join("@_backup_dir")).unwrap();

        let out = Command::new("findmnt").args(["-no", "SOURCE"]).arg(&top).output().unwrap();
        let source = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let source = source.split('[').next().unwrap().to_string();
        let mnt = top.join("@/mnt_m");
        std::fs::create_dir(&mnt).unwrap();
        run("mount", &["-o", "subvol=@_backup_m", &source, mnt.to_str().unwrap()]);

        let found = super::find(&top, &strings(&["@", "@home"])).unwrap();
        run("umount", &[mnt.to_str().unwrap()]);
        let mut labels: Vec<(String, Vec<String>)> =
            found.iter().map(|o| (o.label.clone(), o.subvols.clone())).collect();
        labels.sort();
        assert_eq!(
            labels,
            vec![
                ("a".to_string(), strings(&["@_backup_a", "@home_backup_a"])),
                ("e".to_string(), strings(&["@_backup_e"])),
            ]
        );
        assert!(found.iter().all(|o| !o.created.is_empty()));

        crate::btrfs::delete_subvolume_tree(&top, "@_backup_e").unwrap();
        assert!(!top.join("@_backup_e").exists());
        assert!(crate::btrfs::delete_subvolume_tree(&top, "@_backup_n").is_err());
        assert!(top.join("@_backup_n/aninhado/dado").exists());

        std::fs::remove_file(top.join("@_backup_n/aninhado/dado")).unwrap();
        for name in ["@_backup_n", "@_backup_a", "@home_backup_a", "@_backup_m"] {
            crate::btrfs::delete_subvolume_tree(&top, name).unwrap();
        }
        std::fs::remove_dir(&mnt).unwrap();
        run("btrfs", &["subvolume", "delete", "@", "@home"]);
        std::fs::remove_dir(top.join("@_backup_dir")).unwrap();
    }

    #[test]
    fn protection_covers_default_mounted_and_bootloader_refs() {
        let mounted: HashSet<String> = ["@_backup_m".to_string()].into_iter().collect();
        let refs = "path=/@_backup_l/boot/vmlinuz";
        assert!(is_protected("@_backup_d", Some("@_backup_d"), &mounted, refs));
        assert!(is_protected("@_backup_m", None, &mounted, refs));
        assert!(is_protected("@_backup_l", None, &mounted, refs));
        assert!(!is_protected("@_backup_free", None, &mounted, refs));
    }
}
