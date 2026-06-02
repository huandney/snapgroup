use crate::boot::{self, BootHealth};
use crate::ui::doctor as doctor_ui;
use crate::ui::term::confirm;
use anyhow::{bail, Context, Result};
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
    doctor_ui::print_boot_sync_failure(&error, recovery);

    print_diagnosis(root, boot)?;

    if !confirm("Tentar sincronizar /boot novamente agora?")? {
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

    let idx = doctor_ui::select_target(&targets)?;
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
    let diagnosis = diagnosis_for(&target.root, &target.boot)?;
    doctor_ui::print_report(target, &diagnosis);
    match diagnosis.health {
        // /boot não montado: o relatório já mostrou a recuperação. Não há o que
        // sincronizar até montá-lo, e não é "nada a fazer".
        BootHealth::Unmounted => return Ok(()),
        BootHealth::NeedsSync(_) => {}
        BootHealth::NativeBoot | BootHealth::Synced => {
            doctor_ui::print_no_action_needed();
            return Ok(());
        }
    }

    doctor_ui::print_suggested_sync(target);
    let should_apply = apply || confirm("Aplicar correção agora?")?;
    if !should_apply {
        doctor_ui::print_correction_skipped();
        return Ok(());
    }

    boot::sync_fat32_paths(&target.root, &target.boot)?;
    doctor_ui::print_spacer();
    let diagnosis = diagnosis_for(&target.root, &target.boot)?;
    doctor_ui::print_report(target, &diagnosis);
    Ok(())
}

fn print_diagnosis(root: &Path, boot_path: &Path) -> Result<bool> {
    let diagnosis = diagnosis_for(root, boot_path)?;
    let needs_sync = matches!(diagnosis.health, BootHealth::NeedsSync(_));
    doctor_ui::print_diagnosis(root, &diagnosis);
    Ok(needs_sync)
}

fn diagnosis_for(root: &Path, boot_path: &Path) -> Result<boot::BootDiagnosis> {
    validate_target(root, boot_path)?;
    boot::diagnose_boot(root, boot_path)
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

pub(crate) struct DoctorTarget {
    pub(crate) label: String,
    pub(crate) root: PathBuf,
    pub(crate) boot: PathBuf,
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
