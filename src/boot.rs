use anyhow::{bail, Context, Result};
use std::collections::HashMap;
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

/// Sincroniza kernel e initramfs em /boot (FAT32) com o subvolume restaurado.
///
/// Para cada kernel ativo em /boot:
///   - localiza /usr/lib/modules/<kver>/ no snapshot cujo `pkgbase` casa
///   - copia o vmlinuz daquele kver para /boot
///   - regenera o initramfs com `mkinitcpio -k <kver> -r <restored_root>`
///
/// No final, recalcula hashes BLAKE2B em /boot/limine.conf. Sempre roda
/// quando /boot é FAT32 — o running kernel reportado por `uname -r` é só
/// o que carregou no boot atual, não reflete o estado escrito em /boot
/// (um `pacman -Syu` sem reboot deixa kernel novo no FAT32 e antigo
/// rodando; pular sync nesse caso quebra o boot seguinte).
pub fn sync_fat32(restored_root: &Path) -> Result<()> {
    if !is_fat32() {
        return Ok(());
    }

    let groups = discover_kernel_groups()?;
    if groups.is_empty() {
        bail!("nenhum vmlinuz/initramfs ativo encontrado em /boot");
    }

    let critical = critical_boot_files(&groups);
    backup_boot_files(&critical)?;

    let result = sync_inner(restored_root, &groups);
    if let Err(e) = result {
        eprintln!("  boot sync: falhou, restaurando backup de /boot");
        if let Err(re) = restore_backup() {
            eprintln!("  boot sync: restauração do backup falhou: {re:#}");
        }
        return Err(e);
    }

    let _ = fs::remove_dir_all(boot_backup_dir());
    println!("  boot sync: kernel, initramfs e limine.conf sincronizados");
    Ok(())
}

fn sync_inner(restored_root: &Path, groups: &[KernelGroup]) -> Result<()> {
    let modules_root = restored_root.join("usr/lib/modules");
    if !modules_root.exists() {
        bail!(
            "/usr/lib/modules não existe no snapshot: {}",
            modules_root.display()
        );
    }
    let config = restored_root.join("etc/mkinitcpio.conf");
    if !config.exists() {
        bail!("mkinitcpio.conf não encontrado em {}", config.display());
    }
    let pkgbase_map = read_pkgbase_map(&modules_root)?;

    for group in groups {
        let kver = pkgbase_map.get(&group.kernel_name).with_context(|| {
            format!(
                "snapshot não contém módulos para o kernel '{}' (procurado em {})",
                group.kernel_name,
                modules_root.display()
            )
        })?;
        let snap_vmlinuz = modules_root.join(kver).join("vmlinuz");
        if !snap_vmlinuz.exists() {
            bail!(
                "vmlinuz não encontrado para {kver}: {}",
                snap_vmlinuz.display()
            );
        }

        for dest in &group.vmlinuz_paths {
            fs::copy(&snap_vmlinuz, dest).with_context(|| {
                format!(
                    "copiar vmlinuz {} → {}",
                    snap_vmlinuz.display(),
                    dest.display()
                )
            })?;
            println!("  boot sync: vmlinuz ({kver}) → {}", dest.display());
        }

        for dest in &group.initramfs_paths {
            regen_initramfs(&config, kver, restored_root, dest)?;
            println!(
                "  boot sync: initramfs regenerado ({kver}) → {}",
                dest.display()
            );
        }
    }

    refresh_limine_boot_hashes().context("atualizar hashes do limine.conf")?;
    Ok(())
}

fn regen_initramfs(config: &Path, kver: &str, restored_root: &Path, out: &Path) -> Result<()> {
    let res = Command::new("mkinitcpio")
        .args(["--nopost", "-c"])
        .arg(config)
        .args(["-k", kver, "-r"])
        .arg(restored_root)
        .arg("-g")
        .arg(out)
        .output()
        .context("mkinitcpio falhou")?;
    if !res.status.success() {
        bail!(
            "mkinitcpio {kver} → {}: {}",
            out.display(),
            String::from_utf8_lossy(&res.stderr)
        );
    }
    Ok(())
}

/// Mapa pkgbase → kver. Lê /usr/lib/modules/<kver>/pkgbase do snapshot e
/// inverte para "linux-cachyos" → "7.0.1-1-cachyos". O snapshot precisa
/// ter `pkgbase` em cada dir de módulos (padrão Arch desde 2021); kver dirs
/// sem pkgbase são ignorados.
fn read_pkgbase_map(modules_root: &Path) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for entry in
        fs::read_dir(modules_root).with_context(|| format!("ler {}", modules_root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let pkgbase_file = entry.path().join("pkgbase");
        if !pkgbase_file.exists() {
            continue;
        }
        let pkgbase = fs::read_to_string(&pkgbase_file)
            .with_context(|| format!("ler {}", pkgbase_file.display()))?
            .trim()
            .to_string();
        let kver = entry.file_name().to_string_lossy().into_owned();
        map.insert(pkgbase, kver);
    }
    Ok(map)
}

/// Grupo de artefatos em /boot pertencentes ao mesmo kernel.
struct KernelGroup {
    kernel_name: String,
    vmlinuz_paths: Vec<PathBuf>,
    initramfs_paths: Vec<PathBuf>,
}

/// Varre /boot recursivamente e agrupa vmlinuz-*/initramfs-* por kernel_name
/// extraído do nome do arquivo (padrão Arch: vmlinuz-linux-cachyos,
/// initramfs-linux-cachyos[.img]). Layouts BLS e flat saem agrupados juntos
/// sob o mesmo kernel_name.
fn discover_kernel_groups() -> Result<Vec<KernelGroup>> {
    let mut by_name: HashMap<String, KernelGroup> = HashMap::new();
    scan_boot_dir(Path::new("/boot"), &mut by_name)?;
    let mut groups: Vec<KernelGroup> = by_name.into_values().collect();
    groups.sort_by(|a, b| a.kernel_name.cmp(&b.kernel_name));
    Ok(groups)
}

fn scan_boot_dir(dir: &Path, out: &mut HashMap<String, KernelGroup>) -> Result<()> {
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
        let Some((kernel_name, is_vmlinuz)) = classify_boot_file(&name) else {
            continue;
        };
        let group = out
            .entry(kernel_name.to_string())
            .or_insert_with(|| KernelGroup {
                kernel_name: kernel_name.to_string(),
                vmlinuz_paths: Vec::new(),
                initramfs_paths: Vec::new(),
            });
        if is_vmlinuz {
            group.vmlinuz_paths.push(path);
        } else {
            group.initramfs_paths.push(path);
        }
    }
    Ok(())
}

fn is_ignored_boot_dir(name: &str) -> bool {
    matches!(name, "limine_history" | ".snapg_boot_backup")
}

/// Classifica um arquivo em /boot como `(kernel_name, is_vmlinuz)`.
/// - `vmlinuz-linux-cachyos`       → `("linux-cachyos", true)`
/// - `initramfs-linux-cachyos.img` → `("linux-cachyos", false)`
///
/// Fallback initramfs (`initramfs-*-fallback*`) é ignorado: sem preset não
/// dá pra reconstruir um fallback corretamente, e o do backup vai cobrir a
/// recuperação se algo falhar.
fn classify_boot_file(name: &str) -> Option<(&str, bool)> {
    if let Some(rest) = name.strip_prefix("vmlinuz-") {
        return Some((strip_img_ext(rest), true));
    }
    if let Some(rest) = name.strip_prefix("initramfs-") {
        let stripped = strip_img_ext(rest);
        if stripped.contains("fallback") {
            return None;
        }
        return Some((stripped, false));
    }
    None
}

fn strip_img_ext(s: &str) -> &str {
    s.strip_suffix(".img").unwrap_or(s)
}

/// Conjunto de arquivos críticos para backup: vmlinuz/initramfs ativos de
/// todos os kernels descobertos + limine.conf (e .old, se existir).
fn critical_boot_files(groups: &[KernelGroup]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for g in groups {
        files.extend(g.vmlinuz_paths.iter().cloned());
        files.extend(g.initramfs_paths.iter().cloned());
    }
    for extra in ["/boot/limine.conf", "/boot/limine.conf.old"] {
        let p = Path::new(extra);
        if p.exists() {
            files.push(p.to_path_buf());
        }
    }
    files
}

fn boot_backup_dir() -> PathBuf {
    PathBuf::from("/boot/.snapg_boot_backup")
}

fn backup_boot_files(files: &[PathBuf]) -> Result<()> {
    let backup = boot_backup_dir();
    if backup.exists() {
        let _ = fs::remove_dir_all(&backup);
    }
    fs::create_dir_all(&backup).context("criar diretório de backup do boot")?;

    for src in files {
        let rel = src
            .strip_prefix("/boot")
            .with_context(|| format!("{} não está dentro de /boot", src.display()))?;
        let dest = backup.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).with_context(|| format!("criar {}", parent.display()))?;
        }
        fs::copy(src, &dest)
            .with_context(|| format!("backup {} → {}", src.display(), dest.display()))?;
    }
    println!("  boot sync: backup em {}", backup.display());
    Ok(())
}

/// Restaura os ficheiros de boot do backup (recovery do próprio sync_fat32).
pub fn restore_backup() -> Result<()> {
    if !is_fat32() {
        return Ok(());
    }
    let backup = boot_backup_dir();
    if !backup.exists() {
        return Ok(());
    }
    restore_backup_dir(&backup, Path::new("/boot"))?;
    let _ = fs::remove_dir_all(&backup);
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
    use super::{classify_boot_file, limine_boot_path_from_line};
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

    #[test]
    fn classifies_vmlinuz() {
        assert_eq!(
            classify_boot_file("vmlinuz-linux-cachyos"),
            Some(("linux-cachyos", true))
        );
    }

    #[test]
    fn classifies_initramfs_plain() {
        assert_eq!(
            classify_boot_file("initramfs-linux-cachyos"),
            Some(("linux-cachyos", false))
        );
    }

    #[test]
    fn classifies_initramfs_with_img_ext() {
        assert_eq!(
            classify_boot_file("initramfs-linux-cachyos.img"),
            Some(("linux-cachyos", false))
        );
    }

    #[test]
    fn skips_fallback_initramfs() {
        assert_eq!(
            classify_boot_file("initramfs-linux-cachyos-fallback.img"),
            None
        );
    }

    #[test]
    fn ignores_unrelated_files() {
        assert_eq!(classify_boot_file("intel-ucode.img"), None);
        assert_eq!(classify_boot_file("limine.conf"), None);
        assert_eq!(classify_boot_file("limine-splash.png"), None);
    }
}
