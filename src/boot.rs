use anyhow::{Context, Result, bail};
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
///  4. Regenera o initramfs via mkinitcpio -k <versão>
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

    // 2. Copiar vmlinuz do snapshot para /boot
    let snap_vmlinuz = modules_dir.join(&snap_kver).join("vmlinuz");
    if !snap_vmlinuz.exists() {
        bail!(
            "vmlinuz não encontrado no snapshot: {}",
            snap_vmlinuz.display()
        );
    }
    for bf in &boot_files {
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

    // 3. Regenerar initramfs com os módulos do snapshot.
    //    Bind-mount temporário: monta /usr/lib/modules/<snap_kver> do snapshot
    //    sobre /usr/lib/modules/<snap_kver> do sistema vivo, para que o
    //    mkinitcpio encontre os módulos corretos.
    regen_initramfs(&snap_kver, &modules_dir)?;

    println!("  boot sync: kernel e initramfs sincronizados para {snap_kver}");
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

    let entries = fs::read_dir(&backup_dir).context("ler diretório de backup do boot")?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let dest = Path::new("/boot").join(&name);
        fs::copy(entry.path(), &dest).with_context(|| {
            format!("restaurar backup {} → {}", entry.path().display(), dest.display())
        })?;
    }
    let _ = fs::remove_dir_all(&backup_dir);
    println!("  boot sync: ficheiros de boot restaurados do backup");
    Ok(())
}

/// Descobre a versão do kernel dentro de /usr/lib/modules/ de um subvolume.
/// Retorna a primeira diretoria que contém um ficheiro `vmlinuz`.
fn discover_kernel_version(modules_dir: &Path) -> Result<String> {
    if !modules_dir.exists() {
        bail!(
            "diretório de módulos não existe: {}",
            modules_dir.display()
        );
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

/// Varre diretoria (recursivamente 1 nível) à procura de vmlinuz-* e initramfs-*.
fn scan_boot_dir(dir: &Path, out: &mut Vec<BootFile>) -> Result<()> {
    let walk = fs::read_dir(dir).with_context(|| format!("ler {}", dir.display()))?;
    for entry in walk {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            // Um nível de recursão (BLS: /boot/<mid>/<kernel-name>/)
            scan_boot_dir(&path, out)?;
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("vmlinuz") {
            out.push(BootFile { path, is_vmlinuz: true });
        } else if name.starts_with("initramfs") {
            out.push(BootFile { path, is_vmlinuz: false });
        }
    }
    Ok(())
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
        let name = bf.path.file_name().context("nome de ficheiro inválido")?;
        let dest = backup.join(name);
        fs::copy(&bf.path, &dest).with_context(|| {
            format!(
                "backup {} → {}",
                bf.path.display(),
                dest.display()
            )
        })?;
    }
    println!("  boot sync: backup dos ficheiros atuais em {}", backup.display());
    Ok(())
}

/// Regenera o initramfs para a versão de kernel do snapshot.
///
/// Cria um bind mount temporário dos módulos do snapshot sobre /usr/lib/modules/<kver>
/// para que o mkinitcpio encontre os módulos corretos, depois executa:
///   mkinitcpio -k <kver> -g /boot/.../initramfs-<kernel-name>
fn regen_initramfs(snap_kver: &str, snap_modules_dir: &Path) -> Result<()> {
    let snap_mod_src = snap_modules_dir.join(snap_kver);
    let live_mod_target = Path::new("/usr/lib/modules").join(snap_kver);

    // Se a diretoria de destino já existe (mesmo kernel, módulos diferentes),
    // fazemos bind mount por cima. Se não existe, criamos e montamos.
    let needs_mount = live_mod_target.exists()
        && snap_mod_src.canonicalize().ok() != live_mod_target.canonicalize().ok();
    let created_dir = !live_mod_target.exists();

    if created_dir {
        fs::create_dir_all(&live_mod_target)
            .context("criar diretório de módulos temporário")?;
    }

    if needs_mount || created_dir {
        let mount_out = Command::new("mount")
            .args(["--bind"])
            .arg(&snap_mod_src)
            .arg(&live_mod_target)
            .output()
            .context("bind mount de módulos falhou")?;
        if !mount_out.status.success() {
            bail!(
                "bind mount {} → {}: {}",
                snap_mod_src.display(),
                live_mod_target.display(),
                String::from_utf8_lossy(&mount_out.stderr)
            );
        }
    }

    // Encontra o preset correcto para o mkinitcpio.
    let preset = find_mkinitcpio_preset()?;

    let result = Command::new("mkinitcpio")
        .args(["-p", &preset])
        .output()
        .context("mkinitcpio falhou");

    // Cleanup: desmonta bind mount se fizemos.
    if needs_mount || created_dir {
        let _ = Command::new("umount").arg(&live_mod_target).output();
    }
    if created_dir {
        let _ = fs::remove_dir(&live_mod_target);
    }

    let out = result?;
    if !out.status.success() {
        bail!(
            "mkinitcpio -p {preset}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    println!("  boot sync: initramfs regenerado via mkinitcpio -p {preset}");
    Ok(())
}

/// Descobre o preset do mkinitcpio (ex: "linux-cachyos").
fn find_mkinitcpio_preset() -> Result<String> {
    let preset_dir = Path::new("/etc/mkinitcpio.d");
    if !preset_dir.exists() {
        bail!("diretório de presets /etc/mkinitcpio.d não existe");
    }
    for entry in fs::read_dir(preset_dir).context("ler /etc/mkinitcpio.d")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(preset) = name.strip_suffix(".preset") {
            return Ok(preset.to_string());
        }
    }
    bail!("nenhum preset .preset encontrado em /etc/mkinitcpio.d")
}
