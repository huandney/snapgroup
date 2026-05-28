use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Monta o subvolume top-level (subvolid=5) num path temporário.
/// Necessário pra criar/renomear subvolumes irmãos como @, @home — operações
/// que exigem acesso ao top-level, não às mounts individuais.
pub fn mount_toplevel(uuid: &str, target: &Path) -> Result<()> {
    fs::create_dir_all(target).context("criar diretório de mount")?;
    let out = Command::new("mount")
        .args(["-o", "subvolid=5", "-U", uuid])
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
