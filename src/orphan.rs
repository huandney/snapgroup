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

/// Backups criados juntos (o mesmo restore) em uma ou mais bases.
pub struct OrphanBackup {
    /// Rótulo do primeiro subvolume (o texto depois de `_backup_`).
    pub label: String,
    /// Nomes no top-level, do mais antigo ao mais novo.
    pub subvols: Vec<String>,
    /// Bases presentes, sem repetição, em ordem alfabética.
    pub bases: Vec<String>,
    /// Instante do backup do primeiro membro, hora local (ex: "2026-04-30 17:32:41").
    pub created: String,
}

/// Um restore cria um backup por subvolume, com segundos de diferença (o do
/// Limine grava cada um com o próprio instante). Até este intervalo, é o mesmo.
const SAME_RESTORE_SECS: i64 = 60;

struct Candidate {
    base: String,
    name: String,
    label: String,
    created: String,
    epoch: Option<i64>,
}

/// Órfãos das `bases` (subvols das configs do Snapper) sob o top-level montado
/// em `toplevel`, mais antigo primeiro. Ficam de fora o default, os montados,
/// os citados pelo bootloader e os que guardam subvolume aninhado com dados.
pub fn find(toplevel: &Path, bases: &[String]) -> Result<Vec<OrphanBackup>> {
    let entries = toplevel_entries(toplevel)?;
    let default = default_subvol(toplevel)?;
    let mounted = mounted_btrfs_roots()?;
    let refs = bootloader_references(Path::new("/boot"));

    let mut kept = Vec::new();
    for (base, name, label) in candidates(&entries, bases) {
        if is_protected(&name, default.as_deref(), &mounted, &refs)
            || !btrfs::is_subvolume(&toplevel.join(&name))
            || btrfs::nested_with_data(toplevel, &name)?.is_some()
        {
            continue;
        }
        // O backup é o subvolume antigo renomeado: a data de criação do btrfs é
        // a de quando ele nasceu, não a do backup. O nome guarda o instante real.
        let epoch = label_epoch(&label).or_else(|| {
            let created = btrfs::subvol_creation_time(&toplevel.join(&name)).ok()?;
            creation_epoch(&created)
        });
        let created = epoch.map(local_datetime).unwrap_or_default();
        kept.push(Candidate { base, name, label, created, epoch });
    }
    Ok(group_by_time(kept, bases))
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

/// `(base, nome, rótulo)` de cada `<base>_backup_<rótulo>`. Prefixo exato:
/// `@home_backup_x` não casa com a base `@`.
fn candidates(entries: &[String], bases: &[String]) -> Vec<(String, String, String)> {
    let mut found = Vec::new();
    for base in bases {
        let prefix = format!("{base}{BACKUP_INFIX}");
        for name in entries.iter().filter(|e| e.starts_with(&prefix)) {
            let label = &name[prefix.len()..];
            if !label.is_empty() {
                found.push((base.clone(), name.clone(), label.to_string()));
            }
        }
    }
    found
}

/// Junta em uma linha os backups criados a até `SAME_RESTORE_SECS` do anterior,
/// mais antigo primeiro. Sem data legível, o backup fica sozinho, no fim.
fn group_by_time(mut items: Vec<Candidate>, bases: &[String]) -> Vec<OrphanBackup> {
    items.sort_by(|a, b| a.epoch.cmp(&b.epoch).then_with(|| a.name.cmp(&b.name)));
    items.sort_by_key(|c| c.epoch.is_none());
    let mut groups: Vec<(Option<i64>, Vec<Candidate>)> = Vec::new();
    for item in items {
        let joins = match (groups.last(), item.epoch) {
            (Some((Some(last), _)), Some(epoch)) => epoch - last <= SAME_RESTORE_SECS,
            _ => false,
        };
        if joins {
            let (last, members) = groups.last_mut().expect("grupo existe");
            *last = item.epoch;
            members.push(item);
            continue;
        }
        groups.push((item.epoch, vec![item]));
    }
    groups
        .into_iter()
        .map(|(_, members)| {
            // Ordem alfabética põe `@` (o root) antes de `@home` e `@root`.
            let mut present: Vec<String> = bases
                .iter()
                .filter(|b| members.iter().any(|m| &m.base == *b))
                .cloned()
                .collect();
            present.sort();
            OrphanBackup {
                label: members[0].label.clone(),
                created: members[0].created.clone(),
                subvols: members.into_iter().map(|m| m.name).collect(),
                bases: present,
            }
        })
        .collect()
}

/// Instante gravado no rótulo: ISO em UTC (`2026-11-08T03:03:12.448Z`, do
/// limine-snapper-restore) ou hora local (`2026-04-26_20:58:44`).
fn label_epoch(label: &str) -> Option<i64> {
    if let Some(utc) = label.strip_suffix('Z') {
        let (date, time) = utc.split_once('T')?;
        let time = time.split('.').next()?;
        let (y, m, d) = parse_date(date)?;
        let (hh, mm, ss) = parse_time(time)?;
        return Some(days_from_civil(y, m, d) * 86_400 + hh * 3600 + mm * 60 + ss);
    }
    let (date, time) = label.split_once('_')?;
    let (y, m, d) = parse_date(date)?;
    let (hh, mm, ss) = parse_time(time)?;
    local_epoch(y, m, d, hh, mm, ss)
}

fn parse_date(date: &str) -> Option<(i64, i64, i64)> {
    let mut d = date.split('-').map(|v| v.parse::<i64>().ok());
    let parsed = (d.next()??, d.next()??, d.next()??);
    d.next().is_none().then_some(parsed)
}

fn parse_time(time: &str) -> Option<(i64, i64, i64)> {
    let mut t = time.split(':').map(|v| v.parse::<i64>().ok());
    let parsed = (t.next()??, t.next()??, t.next()??);
    t.next().is_none().then_some(parsed)
}

/// Hora local em epoch, pelo fuso do sistema (`mktime`).
fn local_epoch(y: i64, m: i64, d: i64, hh: i64, mm: i64, ss: i64) -> Option<i64> {
    // SAFETY: tm é POD; zerado e preenchido campo a campo.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = (y - 1900) as i32;
    tm.tm_mon = (m - 1) as i32;
    tm.tm_mday = d as i32;
    tm.tm_hour = hh as i32;
    tm.tm_min = mm as i32;
    tm.tm_sec = ss as i32;
    tm.tm_isdst = -1;
    // SAFETY: tm válido; mktime só lê e normaliza.
    let epoch = unsafe { libc::mktime(&mut tm) };
    (epoch != -1).then_some(epoch)
}

/// Epoch como `YYYY-MM-DD HH:MM:SS` na hora local.
fn local_datetime(epoch: i64) -> String {
    // SAFETY: localtime_r preenche tm; zerado antes por segurança.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let time: libc::time_t = epoch;
    // SAFETY: ponteiros válidos durante a chamada.
    if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
        return String::new();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// `2026-04-30 17:32:41 -0400` (formato do `btrfs subvolume show`) em epoch.
fn creation_epoch(created: &str) -> Option<i64> {
    let mut parts = created.split_whitespace();
    let (date, time, zone) = (parts.next()?, parts.next()?, parts.next()?);
    let (y, m, day) = parse_date(date)?;
    let (hh, mm, ss) = parse_time(time)?;
    if zone.len() != 5 {
        return None;
    }
    let sign = if zone.starts_with('-') { -1 } else { 1 };
    let offset = sign * (zone[1..3].parse::<i64>().ok()? * 3600 + zone[3..5].parse::<i64>().ok()? * 60);
    Some(days_from_civil(y, m, day) * 86_400 + hh * 3600 + mm * 60 + ss - offset)
}

/// Dias desde 1970-01-01 no calendário gregoriano (algoritmo de H. Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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
    use super::{
        candidates, creation_epoch, group_by_time, is_protected, label_epoch, parse_default_subvol,
        parse_mounted_btrfs_roots,
    };
    use std::collections::HashSet;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    fn candidate(base: &str, name: &str, epoch: Option<i64>) -> super::Candidate {
        super::Candidate {
            base: base.to_string(),
            name: name.to_string(),
            label: name.split("_backup_").nth(1).unwrap().to_string(),
            created: String::new(),
            epoch,
        }
    }

    #[test]
    fn candidates_need_an_exact_base_prefix_and_a_label() {
        let entries = strings(&[
            "@",
            "@home_backup_2026-04-26_20:58:44",
            "@_backup_2026-11-08T03:03:06.978Z",
            "@_snapg_regret",
            "@srv_backup_2026-01-01_00:00:00",
            "@_backup_",
        ]);
        let found = candidates(&entries, &strings(&["@", "@home"]));
        let names: Vec<&str> = found.iter().map(|(_, n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["@_backup_2026-11-08T03:03:06.978Z", "@home_backup_2026-04-26_20:58:44"]);
        assert!(candidates(&strings(&["@home_backup_x"]), &strings(&["@"])).is_empty());
    }

    #[test]
    fn backups_of_the_same_restore_share_a_row() {
        let bases = strings(&["@", "@home", "@root"]);
        let items = vec![
            candidate("@home", "@home_backup_b", Some(1006)),
            candidate("@", "@_backup_a", Some(1000)),
            candidate("@root", "@root_backup_c", Some(1050)),
            candidate("@", "@_backup_later", Some(1200)),
            candidate("@", "@_backup_undated", None),
        ];
        let groups = group_by_time(items, &bases);
        let rows: Vec<(Vec<String>, Vec<String>)> =
            groups.iter().map(|g| (g.subvols.clone(), g.bases.clone())).collect();
        assert_eq!(
            rows,
            vec![
                (strings(&["@_backup_a", "@home_backup_b", "@root_backup_c"]), bases.clone()),
                (strings(&["@_backup_later"]), strings(&["@"])),
                (strings(&["@_backup_undated"]), strings(&["@"])),
            ]
        );
        assert_eq!(groups[0].label, "a");
    }

    #[test]
    fn label_time_reads_both_formats() {
        assert_eq!(label_epoch("2026-04-30T21:32:41.795Z"), Some(1_777_584_761));
        assert_eq!(label_epoch("1970-01-01T00:00:00Z"), Some(0));
        assert!(label_epoch("2026-04-26_20:58:44").is_some());
        assert_eq!(label_epoch("x"), None);
        assert_eq!(label_epoch("2026-04-26_20:58"), None);
        assert_eq!(label_epoch("antes-do-update"), None);
    }

    #[test]
    fn creation_time_parses_to_utc_epoch() {
        assert_eq!(creation_epoch("1970-01-01 00:00:00 +0000"), Some(0));
        assert_eq!(creation_epoch("2026-04-30 17:32:41 -0400"), Some(1_777_584_761));
        assert_eq!(creation_epoch("2026-04-30 21:32:41 +0000"), Some(1_777_584_761));
        assert_eq!(creation_epoch(""), None);
        assert_eq!(creation_epoch("2026-04-30 17:32:41"), None);
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
        // Criados no mesmo segundo: uma linha só, como os de um restore.
        let mut offered: Vec<String> = found.iter().flat_map(|o| o.subvols.clone()).collect();
        offered.sort();
        assert_eq!(offered, strings(&["@_backup_a", "@_backup_e", "@home_backup_a"]));
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
