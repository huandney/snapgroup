use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Monta o subvolume top-level (subvolid=5) num path temporário.
/// Necessário pra criar/renomear subvolumes irmãos como @, @home — operações
/// que exigem acesso ao top-level, não às mounts individuais.
pub fn mount_toplevel(uuid: &str, target: &Path) -> Result<()> {
    create_private_mount_dir(target).context("criar diretório de mount")?;
    // nosuid/nodev: o top-level expõe regret/undo/discard, cópias antigas do
    // root com binários setuid que não devem ser executáveis por este mount.
    let out = Command::new("mount")
        .args(["-o", "subvolid=5,nosuid,nodev", "-U", uuid])
        .arg(target)
        .output()
        .context("mount falhou")?;
    if !out.status.success() {
        bail!(
            "mount toplevel UUID={uuid} -> {}: {}",
            target.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// Cria o mountpoint com o diretório pai (`/run/snapgroup`) em 0700. Os mounts
/// do snapg dão acesso a subvolumes fora da proteção 0750 do `.snapshots`, e
/// ficam de pé enquanto uma TUI espera — ou para sempre, se o processo morrer.
/// Re-aplica o modo a cada chamada: o pai pode ter sido criado 0755 por uma
/// versão anterior neste mesmo boot.
pub fn create_private_mount_dir(target: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(target).with_context(|| format!("criar {}", target.display()))?;
    let parent = target
        .parent()
        .with_context(|| format!("mountpoint sem diretório pai: {}", target.display()))?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("restringir {}", parent.display()))
}

pub fn umount_toplevel(target: &Path) -> Result<()> {
    let out = Command::new("umount")
        .arg(target)
        .output()
        .context("umount falhou")?;
    if !out.status.success() {
        bail!(
            "umount {}: {}",
            target.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = fs::remove_dir(target);
    Ok(())
}

pub fn create_snapshot(source: &Path, dest: &Path) -> Result<()> {
    let out = Command::new("btrfs")
        .args(["subvolume", "snapshot"])
        .arg(source)
        .arg(dest)
        .output()
        .context("btrfs subvolume snapshot falhou")?;
    if !out.status.success() {
        bail!(
            "snapshot {} -> {}: {}",
            source.display(),
            dest.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

pub fn create_readonly_snapshot(source: &Path, dest: &Path) -> Result<()> {
    let out = Command::new("btrfs")
        .args(["subvolume", "snapshot", "-r"])
        .arg(source)
        .arg(dest)
        .output()
        .context("btrfs subvolume snapshot -r falhou")?;
    if !out.status.success() {
        bail!(
            "snapshot -r {} -> {}: {}",
            source.display(),
            dest.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

pub fn delete_subvolume(path: &Path) -> Result<()> {
    let out = Command::new("btrfs")
        .args(["subvolume", "delete"])
        .arg(path)
        .output()
        .context("btrfs subvolume delete falhou")?;
    if !out.status.success() {
        bail!(
            "delete {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// Retorna o path top-level-relativo de um subvolume dado qualquer caminho dentro dele.
/// Ex: "/home" -> "@home"; "/home/.snapshots/22/snapshot" -> "@home_snapshots/22/snapshot".
/// A primeira linha de `btrfs subvolume show` é exatamente essa string.
pub fn subvol_relative_path(any_path: &Path) -> Result<String> {
    let out = Command::new("btrfs")
        .args(["subvolume", "show"])
        .arg(any_path)
        .output()
        .context("btrfs subvolume show falhou")?;
    if !out.status.success() {
        bail!(
            "show {}: {}",
            any_path.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines()
        .next()
        .map(|l| l.trim().to_string())
        .context("output vazio de btrfs subvolume show")
}

/// Subvolumes aninhados diretamente dentro de `subvol` (nome relativo ao
/// top-level montado em `toplevel`), como paths relativos à raiz dele.
/// Ex: para "@" → [".snapshots", "var/lib/machines"]. Só os filhos diretos: o
/// que vive dentro de um aninhado acompanha o pai num rename.
pub fn nested_subvolumes(toplevel: &Path, subvol: &str) -> Result<Vec<std::path::PathBuf>> {
    let path = toplevel.join(subvol);
    let out = Command::new("btrfs")
        .args(["subvolume", "list", "-o"])
        .arg(&path)
        .output()
        .context("btrfs subvolume list falhou")?;
    if !out.status.success() {
        bail!(
            "list -o {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    parse_nested_subvolumes(&String::from_utf8_lossy(&out.stdout), subvol)
}

/// Cada linha é `ID 257 gen 9 top level 256 path @/var/lib/machines`, com o
/// path relativo ao top-level do filesystem.
fn parse_nested_subvolumes(listing: &str, subvol: &str) -> Result<Vec<std::path::PathBuf>> {
    let prefix = format!("{subvol}/");
    let mut nested = Vec::new();
    for line in listing.lines() {
        let Some((_, full)) = line.split_once(" path ") else {
            continue;
        };
        let rel = full
            .strip_prefix(&prefix)
            .with_context(|| format!("subvolume '{full}' fora de '{subvol}' na listagem"))?;
        nested.push(std::path::PathBuf::from(rel));
    }
    Ok(nested)
}

/// True se o path é um subvolume btrfs (não só diretório).
pub fn is_subvolume(path: &Path) -> bool {
    Command::new("btrfs")
        .args(["subvolume", "show"])
        .arg(path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Timestamp local formatado pra naming de backup.
/// Ex: "2026-04-26_19:57:24". Usa `date` pra evitar dep de chrono.
pub fn now_local_label() -> Result<String> {
    let out = Command::new("date")
        .arg("+%Y-%m-%d_%H:%M:%S")
        .output()
        .context("date falhou")?;
    if !out.status.success() {
        bail!("date: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Extrai o campo `Creation time` da saída de `btrfs subvolume show`.
/// Retorna a string bruta (ex: "2026-04-30 17:32:41 -0400").
pub fn subvol_creation_time(path: &Path) -> Result<String> {
    let out = Command::new("btrfs")
        .args(["subvolume", "show"])
        .arg(path)
        .output()
        .context("btrfs subvolume show falhou")?;
    if !out.status.success() {
        bail!(
            "show {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let s = String::from_utf8_lossy(&out.stdout);
    for line in s.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Creation time:") {
            return Ok(rest.trim().to_string());
        }
    }
    bail!(
        "Creation time não encontrado em btrfs subvolume show {}",
        path.display()
    )
}

pub fn fs_uuid(mountpoint: &str) -> Result<String> {
    let out = Command::new("findmnt")
        .args(["-no", "UUID", mountpoint])
        .output()
        .context("findmnt falhou")?;
    if !out.status.success() {
        bail!(
            "findmnt UUID {mountpoint}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        bail!("UUID vazio pra {mountpoint}");
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::parse_nested_subvolumes;
    use std::path::PathBuf;

    #[test]
    fn parses_nested_paths_relative_to_subvol() {
        let listing = "ID 257 gen 9 top level 256 path @/.snapshots\n\
                       ID 258 gen 10 top level 256 path @/var/lib/my dir/sub one\n";
        assert_eq!(
            parse_nested_subvolumes(listing, "@").unwrap(),
            vec![PathBuf::from(".snapshots"), PathBuf::from("var/lib/my dir/sub one")]
        );
        assert!(parse_nested_subvolumes("", "@").unwrap().is_empty());
        // Path de outro subvolume: listagem inesperada vira erro, não é ignorada.
        assert!(parse_nested_subvolumes("ID 1 gen 1 top level 5 path @home/x\n", "@").is_err());
    }
}
