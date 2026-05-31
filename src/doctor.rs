use crate::boot::{self, BootHealth};
use anyhow::{bail, Context, Result};
use dialoguer::{Confirm, Select};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn run(root: Option<PathBuf>, boot: Option<PathBuf>, apply: bool) -> Result<()> {
    let target = select_target(root, boot)?;
    diagnose_and_apply(&target, apply)
}

pub fn handle_boot_sync_failure(
    root: &Path,
    boot: &Path,
    error: anyhow::Error,
    recovery: &str,
) -> Result<()> {
    eprintln!();
    eprintln!("✗ sincronização do /boot (FAT32) falhou: {error:#}");
    eprintln!("  O rollback BTRFS foi aplicado, mas o kernel/initramfs em /boot");
    eprintln!("  NÃO corresponde aos módulos do snapshot restaurado.");
    eprintln!("  REINICIAR AGORA CAI EM EMERGENCY MODE — reboot bloqueado.");
    eprintln!();
    eprintln!("  Recuperação alternativa: {recovery}");
    eprintln!();

    print_diagnosis(root, boot)?;

    if !Confirm::new()
        .with_prompt("Tentar sincronizar /boot novamente agora?")
        .default(false)
        .interact()
        .context("ler confirmação")?
    {
        bail!("restauração incompleta: /boot dessincronizado, reboot bloqueado");
    }

    boot::sync_fat32_paths(root, boot)?;
    print_diagnosis(root, boot)?;
    Ok(())
}

fn select_target(root: Option<PathBuf>, boot: Option<PathBuf>) -> Result<DoctorTarget> {
    if let Some(root) = root {
        let boot = boot.unwrap_or_else(|| root.join("boot"));
        return Ok(DoctorTarget::new(
            format!("root={} boot={}", root.display(), boot.display()),
            root,
            boot,
        ));
    }
    if let Some(boot) = boot {
        let root = PathBuf::from("/");
        return Ok(DoctorTarget::new(
            format!("root={} boot={}", root.display(), boot.display()),
            root,
            boot,
        ));
    }

    if let Some(target) = current_system_target()? {
        return Ok(target);
    }

    let targets = mounted_system_targets()?;
    if targets.is_empty() {
        bail!(
            "nenhum sistema Linux instalado foi detectado. \
             Monte o sistema alvo e rode: snapg doctor --root /mnt --boot /mnt/boot"
        );
    }

    let labels: Vec<String> = targets.iter().map(|target| target.label.clone()).collect();
    let idx = Select::new()
        .with_prompt("Escolha o sistema para diagnosticar")
        .items(&labels)
        .default(0)
        .interact()
        .context("selecionar sistema")?;
    Ok(targets.into_iter().nth(idx).expect("índice selecionado existe"))
}

fn current_system_target() -> Result<Option<DoctorTarget>> {
    let root = Path::new("/");
    if !root.join("usr/lib/modules").exists() {
        return Ok(None);
    }

    let fstype = boot::boot_fstype(root)?;
    if matches!(
        fstype.as_str(),
        "overlay" | "squashfs" | "iso9660" | "tmpfs"
    ) {
        return Ok(None);
    }

    Ok(Some(DoctorTarget::new(
        "sistema atual".to_string(),
        PathBuf::from("/"),
        PathBuf::from("/boot"),
    )))
}

fn diagnose_and_apply(target: &DoctorTarget, apply: bool) -> Result<()> {
    println!("Alvo: {}", target.label);
    println!("  root: {}", target.root.display());
    println!("  boot: {}", target.boot.display());
    println!();

    let needs_sync = print_diagnosis(&target.root, &target.boot)?;
    if !needs_sync {
        println!("Nenhuma ação necessária.");
        return Ok(());
    }

    println!();
    println!("Ação sugerida: sincronizar {} com {}", target.boot.display(), target.root.display());
    let should_apply = apply
        || Confirm::new()
            .with_prompt("Aplicar correção agora?")
            .default(false)
            .interact()
            .context("ler confirmação")?;
    if !should_apply {
        println!("Correção não aplicada.");
        return Ok(());
    }

    boot::sync_fat32_paths(&target.root, &target.boot)?;
    println!();
    print_diagnosis(&target.root, &target.boot)?;
    Ok(())
}

fn print_diagnosis(root: &Path, boot_path: &Path) -> Result<bool> {
    validate_target(root, boot_path)?;
    let diagnosis = boot::diagnose_boot(root, boot_path)?;
    println!("Diagnóstico de boot:");
    println!("  filesystem de boot: {}", diagnosis.fstype);
    match diagnosis.health {
        BootHealth::NativeBoot => {
            println!("  ✓ /boot não é FAT32 separado; nenhuma sincronização é necessária");
            if root != Path::new("/") {
                println!(
                    "  nota: se este sistema usa /boot FAT32 separado, monte-o em {} \
                     ou rode com --boot explícito",
                    root.join("boot").display()
                );
            }
            Ok(false)
        }
        BootHealth::Synced => {
            println!(
                "  ✓ /boot está coerente com o root alvo ({} kernel groups, {} initramfs)",
                diagnosis.kernel_groups, diagnosis.initramfs_files
            );
            Ok(false)
        }
        BootHealth::NeedsSync => {
            println!(
                "  ✗ /boot está dessincronizado com o root alvo ({} kernel groups, {} initramfs)",
                diagnosis.kernel_groups, diagnosis.initramfs_files
            );
            Ok(true)
        }
    }
}

fn validate_target(root: &Path, boot: &Path) -> Result<()> {
    if !root.join("usr/lib/modules").exists() {
        bail!(
            "{} não parece ser um root Linux Arch válido: /usr/lib/modules ausente",
            root.display()
        );
    }
    if !boot.exists() {
        bail!("{} não existe ou não está montado", boot.display());
    }
    Ok(())
}

struct DoctorTarget {
    label: String,
    root: PathBuf,
    boot: PathBuf,
}

impl DoctorTarget {
    fn new(label: String, root: PathBuf, boot: PathBuf) -> Self {
        Self { label, root, boot }
    }
}

fn mounted_system_targets() -> Result<Vec<DoctorTarget>> {
    let out = Command::new("findmnt")
        .args(["-rn", "-t", "btrfs", "-o", "TARGET"])
        .output()
        .context("findmnt btrfs falhou")?;
    if !out.status.success() {
        bail!("findmnt btrfs: {}", String::from_utf8_lossy(&out.stderr));
    }

    let mut targets = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let path = PathBuf::from(line.trim());
        if path.join("usr/lib/modules").exists() {
            targets.push(DoctorTarget::new(
                format!("root montado em {}", path.display()),
                path.clone(),
                path.join("boot"),
            ));
        }
    }
    Ok(targets)
}
