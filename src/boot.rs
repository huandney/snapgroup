use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// True se /boot está montado em FAT32 (vfat).
pub fn is_fat32() -> bool {
    Command::new("findmnt")
        .args(["-no", "FSTYPE", "/boot"])
        .output()
        .map(|o| {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .eq_ignore_ascii_case("vfat")
        })
        .unwrap_or(false)
}

/// Sincroniza o kernel e initramfs em /boot (FAT32) com o subvolume restaurado.
///
/// Fluxo:
///  1. Descobre a versão do kernel no snapshot restaurado (via /usr/lib/modules/)
///  2. Faz backup dos ficheiros atuais em /boot
///  3. Copia o vmlinuz do snapshot para /boot
///  4. Regenera o initramfs usando configuração/módulos do root restaurado
///  5. Atualiza hashes BLAKE2B do Limine para os novos artefatos
///
/// `restored_root` é o path absoluto do novo @ no toplevel (ex: /run/snapgroup/<uuid>/@).
pub fn sync_fat32(restored_root: &Path) -> Result<()> {
    if !is_fat32() {
        return Ok(());
    }

    let modules_dir = restored_root.join("usr/lib/modules");
    let snap_kver = discover_kernel_version(&modules_dir)?;
    let current_kver = current_kernel_version()?;

    if snap_kver == current_kver {
        println!("  boot sync: kernel idêntico ({snap_kver}), nada a fazer");
        return Ok(());
    }

    println!("  boot sync: kernel mudou {current_kver} → {snap_kver}");

    let boot_files = discover_boot_files()?;

    // 1. Backup dos ficheiros atuais
    backup_boot_files(&boot_files)?;

    let result = sync_fat32_inner(restored_root, &modules_dir, &snap_kver, &boot_files);
    if let Err(e) = result {
        eprintln!("  boot sync: falhou, restaurando backup de /boot");
        if let Err(re) = restore_backup() {
            eprintln!("  boot sync: restauração do backup falhou: {re:#}");
        }
        return Err(e);
    }

    let _ = fs::remove_dir_all(boot_backup_dir());
    println!("  boot sync: kernel e initramfs sincronizados para {snap_kver}");
    Ok(())
}

fn sync_fat32_inner(
    restored_root: &Path,
    modules_dir: &Path,
    snap_kver: &str,
    boot_files: &[BootFile],
) -> Result<()> {
    // 2. Copiar vmlinuz do snapshot para /boot
    let snap_vmlinuz = modules_dir.join(snap_kver).join("vmlinuz");
    if !snap_vmlinuz.exists() {
        bail!(
            "vmlinuz não encontrado no snapshot: {}",
            snap_vmlinuz.display()
        );
    }
    for bf in boot_files {
        if !bf.is_vmlinuz {
            continue;
        }
        fs::copy(&snap_vmlinuz, &bf.path).with_context(|| {
            format!(
                "copiar vmlinuz {} → {}",
                snap_vmlinuz.display(),
                bf.path.display()
            )
        })?;
        println!("  boot sync: vmlinuz copiado → {}", bf.path.display());
    }

    // 3. Regenerar initramfs com os módulos/configuração do snapshot.
    regen_initramfs(snap_kver, restored_root, boot_files)?;

    // 4. Se o Limine tinha hashes para os artefatos antigos, eles não valem mais.
    refresh_limine_boot_hashes().context("atualizar hashes do limine.conf")?;
    Ok(())
}

/// Restaura os ficheiros de boot do backup (usado em caso de rollback do rollback).
#[allow(dead_code)]
pub fn restore_backup() -> Result<()> {
    if !is_fat32() {
        return Ok(());
    }
    let backup_dir = boot_backup_dir();
    if !backup_dir.exists() {
        return Ok(());
    }

    restore_backup_dir(&backup_dir, Path::new("/boot"))?;
    let _ = fs::remove_dir_all(&backup_dir);
    println!("  boot sync: ficheiros de boot restaurados do backup");
    Ok(())
}

fn restore_backup_dir(src: &Path, dest: &Path) -> Result<()> {
    for entry in fs::read_dir(src).with_context(|| format!("ler backup {}", src.display()))? {
        let entry = entry?;
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&dest_path)
                .with_context(|| format!("criar {}", dest_path.display()))?;
            restore_backup_dir(&src_path, &dest_path)?;
        } else {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("criar {}", parent.display()))?;
            }
            fs::copy(&src_path, &dest_path).with_context(|| {
                format!(
                    "restaurar backup {} → {}",
                    src_path.display(),
                    dest_path.display()
                )
            })?;
        }
    }
    Ok(())
}

/// Descobre a versão do kernel dentro de /usr/lib/modules/ de um subvolume.
/// Retorna a primeira diretoria que contém um ficheiro `vmlinuz`.
fn discover_kernel_version(modules_dir: &Path) -> Result<String> {
    if !modules_dir.exists() {
        bail!("diretório de módulos não existe: {}", modules_dir.display());
    }

    let mut entries: Vec<_> = fs::read_dir(modules_dir)
        .with_context(|| format!("ler {}", modules_dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().map(|ft| ft.is_dir()).unwrap_or(false)
                && e.path().join("vmlinuz").exists()
        })
        .collect();

    if entries.is_empty() {
        bail!(
            "nenhuma versão de kernel encontrada em {}",
            modules_dir.display()
        );
    }

    // Se houver múltiplas versões, pegar a mais recente por mtime.
    entries.sort_by(|a, b| {
        let ma = a.metadata().and_then(|m| m.modified()).ok();
        let mb = b.metadata().and_then(|m| m.modified()).ok();
        mb.cmp(&ma)
    });

    Ok(entries[0].file_name().to_string_lossy().into_owned())
}

/// Versão do kernel atual (rodando).
fn current_kernel_version() -> Result<String> {
    let out = Command::new("uname")
        .arg("-r")
        .output()
        .context("uname -r falhou")?;
    if !out.status.success() {
        bail!("uname -r: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

struct BootFile {
    path: PathBuf,
    is_vmlinuz: bool,
}

/// Descobre vmlinuz e initramfs em /boot (BLS ou flat layout).
fn discover_boot_files() -> Result<Vec<BootFile>> {
    let mut found = Vec::new();

    // BLS layout: /boot/<machine-id>/<kernel-name>/{vmlinuz,initramfs}*
    let machine_id = fs::read_to_string("/etc/machine-id")
        .context("ler /etc/machine-id")?
        .trim()
        .to_string();

    let bls_dir = Path::new("/boot").join(&machine_id);
    if bls_dir.exists() {
        scan_boot_dir(&bls_dir, &mut found)?;
    }

    // Flat layout: /boot/vmlinuz-*, /boot/initramfs-*
    if found.is_empty() {
        scan_boot_dir(Path::new("/boot"), &mut found)?;
    }

    if found.is_empty() {
        bail!("nenhum vmlinuz ou initramfs encontrado em /boot");
    }
    Ok(found)
}

/// Varre diretoria à procura de vmlinuz-* e initramfs-* ativos.
/// Diretórios de histórico/backup do bootloader nunca são destinos ativos.
fn scan_boot_dir(dir: &Path, out: &mut Vec<BootFile>) -> Result<()> {
    let walk = fs::read_dir(dir).with_context(|| format!("ler {}", dir.display()))?;
    for entry in walk {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if path.is_dir() {
            if is_ignored_boot_dir(&name) {
                continue;
            }
            scan_boot_dir(&path, out)?;
            continue;
        }
        if name.starts_with("vmlinuz") {
            out.push(BootFile {
                path,
                is_vmlinuz: true,
            });
        } else if name.starts_with("initramfs") {
            out.push(BootFile {
                path,
                is_vmlinuz: false,
            });
        }
    }
    Ok(())
}

fn is_ignored_boot_dir(name: &str) -> bool {
    matches!(name, "limine_history" | ".snapg_boot_backup")
}

fn boot_backup_dir() -> PathBuf {
    PathBuf::from("/boot/.snapg_boot_backup")
}

fn backup_boot_files(files: &[BootFile]) -> Result<()> {
    let backup = boot_backup_dir();
    if backup.exists() {
        let _ = fs::remove_dir_all(&backup);
    }
    fs::create_dir_all(&backup).context("criar diretório de backup do boot")?;

    for bf in files {
        let rel = bf
            .path
            .strip_prefix("/boot")
            .with_context(|| format!("{} não está dentro de /boot", bf.path.display()))?;
        let dest = backup.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).with_context(|| format!("criar {}", parent.display()))?;
        }
        fs::copy(&bf.path, &dest)
            .with_context(|| format!("backup {} → {}", bf.path.display(), dest.display()))?;
    }
    println!(
        "  boot sync: backup dos ficheiros atuais em {}",
        backup.display()
    );
    Ok(())
}

/// Regenera o initramfs para a versão de kernel do snapshot.
///
/// Usa `-r <restored_root>` para buscar os módulos no snapshot restaurado e
/// `-c <restored_root>/etc/mkinitcpio.conf` para evitar ler configuração do
/// root vivo, que pode estar numa linha do tempo diferente.
fn regen_initramfs(snap_kver: &str, restored_root: &Path, boot_files: &[BootFile]) -> Result<()> {
    let preset = find_mkinitcpio_preset(restored_root)?;
    let config = restored_root.join("etc/mkinitcpio.conf");
    if !config.exists() {
        bail!("mkinitcpio.conf não encontrado em {}", config.display());
    }

    let initramfs_files: Vec<_> = boot_files.iter().filter(|bf| !bf.is_vmlinuz).collect();
    if initramfs_files.is_empty() {
        bail!("nenhum initramfs ativo encontrado em /boot");
    }

    for bf in initramfs_files {
        let out = Command::new("mkinitcpio")
            .args(["--nopost", "-c"])
            .arg(&config)
            .args(["-k", snap_kver, "-r"])
            .arg(restored_root)
            .arg("-g")
            .arg(&bf.path)
            .output()
            .context("mkinitcpio falhou")?;
        if !out.status.success() {
            bail!(
                "mkinitcpio ({preset}) -> {}: {}",
                bf.path.display(),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        println!("  boot sync: initramfs regenerado → {}", bf.path.display());
    }
    Ok(())
}

/// Descobre o preset do mkinitcpio (ex: "linux-cachyos").
fn find_mkinitcpio_preset(restored_root: &Path) -> Result<String> {
    let preset_dir = restored_root.join("etc/mkinitcpio.d");
    if !preset_dir.exists() {
        bail!(
            "diretório de presets não existe em {}",
            preset_dir.display()
        );
    }
    for entry in
        fs::read_dir(&preset_dir).with_context(|| format!("ler {}", preset_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(preset) = name.strip_suffix(".preset") {
            return Ok(preset.to_string());
        }
    }
    bail!(
        "nenhum preset .preset encontrado em {}",
        preset_dir.display()
    )
}

fn refresh_limine_boot_hashes() -> Result<()> {
    let path = Path::new("/boot/limine.conf");
    if !path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(path).context("ler /boot/limine.conf")?;
    let had_trailing_newline = content.ends_with('\n');
    let mut changed = false;
    let mut lines = Vec::new();

    for line in content.lines() {
        let refreshed = refresh_limine_hash_for_line(line)?;
        if refreshed != line {
            changed = true;
        }
        lines.push(refreshed);
    }

    if !changed {
        return Ok(());
    }

    let mut updated = lines.join("\n");
    if had_trailing_newline {
        updated.push('\n');
    }
    let tmp = path.with_extension("conf.snapg_tmp");
    fs::write(&tmp, updated).context("escrever limine.conf temporário")?;
    fs::rename(&tmp, path).context("substituir /boot/limine.conf com hashes atualizados")?;
    println!("  boot sync: hashes BLAKE2B atualizados em /boot/limine.conf");
    Ok(())
}

fn refresh_limine_hash_for_line(line: &str) -> Result<String> {
    let Some(boot_path) = limine_boot_path_from_line(line) else {
        return Ok(line.to_string());
    };
    if !boot_path.exists() {
        return Ok(line.to_string());
    }

    let hash = blake2b_hex(&boot_path)?;
    let Some(hash_pos) = line.find('#') else {
        return Ok(format!("{}#{hash}", line.trim_end()));
    };
    Ok(format!("{}#{hash}", line[..hash_pos].trim_end()))
}

fn limine_boot_path_from_line(line: &str) -> Option<PathBuf> {
    let trimmed = line.trim_start();
    let (key, value) = trimmed.split_once(':')?;
    let key = key.trim();
    if !matches!(key, "path" | "kernel_path" | "module_path" | "image_path") {
        return None;
    }

    let uri = value.trim();
    let uri_without_hash = uri.split_once('#').map(|(uri, _)| uri).unwrap_or(uri);
    let boot_relative = uri_without_hash.strip_prefix("boot():/")?;
    if boot_relative.contains(char::is_whitespace) {
        return None;
    }
    Some(Path::new("/boot").join(boot_relative))
}

fn blake2b_hex(path: &Path) -> Result<String> {
    let out = Command::new("b2sum")
        .arg(path)
        .output()
        .with_context(|| format!("calcular BLAKE2B de {}", path.display()))?;
    if !out.status.success() {
        bail!(
            "b2sum {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .split_whitespace()
        .next()
        .map(String::from)
        .context("b2sum não retornou hash")
}

#[cfg(test)]
mod tests {
    use super::limine_boot_path_from_line;
    use std::path::Path;

    #[test]
    fn parses_limine_kernel_path() {
        let line = "  path: boot():/linux-cachyos/vmlinuz-linux-cachyos#deadbeef";
        assert_eq!(
            limine_boot_path_from_line(line).as_deref(),
            Some(Path::new("/boot/linux-cachyos/vmlinuz-linux-cachyos"))
        );
    }

    #[test]
    fn parses_limine_module_path() {
        let line = "  module_path: boot():/linux-cachyos/initramfs-linux-cachyos#cafebabe";
        assert_eq!(
            limine_boot_path_from_line(line).as_deref(),
            Some(Path::new("/boot/linux-cachyos/initramfs-linux-cachyos"))
        );
    }

    #[test]
    fn keeps_non_boot_path_lines_unchanged() {
        let line = "  cmdline: quiet rw rootflags=subvol=/@";
        assert_eq!(limine_boot_path_from_line(line), None);
    }
}
