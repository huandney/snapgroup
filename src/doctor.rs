use crate::boot::{self, BootHealth};
use crate::ui::doctor as doctor_ui;
use crate::ui::term::confirm;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn run(root: Option<PathBuf>, boot: Option<PathBuf>, apply: bool) -> Result<()> {
    // Caminho implícito (sem --root/--boot): se `/` é um snapshot de resgate, e
    // não o subvol que boota por padrão, sincronizar `/boot` daqui miraria o
    // root errado. Recusa e instrui. Args explícitos fazem bypass: quem passa
    // --root sabe o alvo.
    if root.is_none()
        && boot.is_none()
        && let Some(ctx) = boot::detect_rescue_boot()?
    {
        return resolve_rescue(ctx, apply);
    }
    let target = select_target(root, boot)?;
    diagnose_and_apply(&target, apply)?;
    Ok(())
}

/// Resolve um boot de resgate. Monta o subvol padrão (`/@`) read-only e oferece
/// o menu: (A) sincronizar `/boot` para o `/@` que boota — não-destrutivo,
/// default; (B) mudar o que boota, via handoff para o `restore` (que carrega o
/// Regret). Se o mount falhar, cai no piso da Fase 1: instruir o comando manual.
fn resolve_rescue(ctx: boot::RescueContext, apply: bool) -> Result<()> {
    // Loop para que ESC dentro do restore (opção "outras") volte a este menu em
    // vez de encerrar o doctor. ESC no próprio menu (None) é que sai.
    loop {
        let mount = match mount_default_subvol_ro(&ctx) {
            Ok(m) => m,
            Err(e) => {
                doctor_ui::print_rescue_mount_failed(&e);
                doctor_ui::print_rescue_boot(&ctx);
                return Ok(());
            }
        };

        let current_kernel = boot::kernel_label(Path::new("/"));
        let default_kernel = boot::kernel_label(&mount.path);
        let boot_kernel = boot::boot_kernel_label(Path::new("/boot"));
        let can_undo_pending_restore = crate::commands::has_pending_restore()?;

        let Some(choice) = doctor_ui::select_rescue_action(
            &ctx,
            &current_kernel,
            &default_kernel,
            &boot_kernel,
            can_undo_pending_restore,
        )?
        else {
            return Ok(()); // ESC no menu do doctor: sai
        };

        if can_undo_pending_restore && choice == 0 {
            drop(mount);
            {
                let _lock = crate::lock::acquire()?;
                crate::commands::undo_pending_restore()?;
            }
            match doctor_ui::select_undo_done_action()? {
                doctor_ui::UndoDoneAction::Exit => return Ok(()),
                doctor_ui::UndoDoneAction::ShowDiagnosis => {
                    let target = DoctorTarget::new(
                        "sistema atual".to_string(),
                        PathBuf::from("/"),
                        PathBuf::from("/boot"),
                    );
                    let _ = diagnose_and_apply(&target, false)?;
                    return Ok(());
                }
            }
        }
        let choice = if can_undo_pending_restore { choice - 1 } else { choice };

        match choice {
            // A: ajusta o "/boot" contra o /@ montado. Terminal — o `apply`
            // original ainda governa o confirm final. Aplicado, declinado ou
            // "nada a fazer", o resultado já está na tela; em vez de recair no
            // menu, fecha com enter sai / esc mostra o diagnóstico do `/`.
            0 => {
                let target = DoctorTarget::new(
                    format!("root padrão (subvol /{})", ctx.default_subvol),
                    mount.path.clone(),
                    PathBuf::from("/boot"),
                )
                .with_root_display(format!("/{}", ctx.default_subvol));
                diagnose_and_apply(&target, apply)?;
                drop(mount);
                match doctor_ui::wait_done_action()? {
                    doctor_ui::UndoDoneAction::Exit => return Ok(()),
                    doctor_ui::UndoDoneAction::ShowDiagnosis => {
                        let target = DoctorTarget::new(
                            "sistema atual".to_string(),
                            PathBuf::from("/"),
                            PathBuf::from("/boot"),
                        );
                        diagnose_and_apply(&target, false)?;
                        return Ok(());
                    }
                }
            }
            // C: ajusta a outra ponta — restaura SÓ o membro root para um
            // snapshot escolhido (kernel anotado), mantendo home e root_home.
            1 => {
                drop(mount);
                {
                    let _lock = crate::lock::acquire()?;
                    crate::commands::restore_root_only()?;
                }
            }
            // Outras: restore completo. Desmonta o /@ (o restore monta o toplevel)
            // e abre o picker. Ao voltar — concluído ou cancelado com ESC — o loop
            // reexibe o menu, então ESC no restore não encerra o doctor.
            _ => {
                drop(mount);
                {
                    let _lock = crate::lock::acquire()?;
                    crate::commands::restore()?;
                }
            }
        }
    }
}

/// Mount read-only do subvol padrão num temp efêmero. Read-only é a segurança:
/// o sync só lê módulos/config de lá; `/boot` é o único alvo de escrita.
struct RescueMount {
    path: PathBuf,
}

impl Drop for RescueMount {
    fn drop(&mut self) {
        let unmounted = Command::new("umount")
            .arg(&self.path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        // EBUSY transitório logo após o uso deixaria o /@ montado. Detach lazy
        // garante a limpeza — recovery tool não pode vazar mount do root.
        if !unmounted {
            let _ = Command::new("umount").arg("-l").arg(&self.path).status();
        }
        let _ = std::fs::remove_dir(&self.path);
    }
}

fn mount_default_subvol_ro(ctx: &boot::RescueContext) -> Result<RescueMount> {
    let path = PathBuf::from(format!("/run/snapgroup/doctor-at-{}", std::process::id()));
    std::fs::create_dir_all(&path).context("criar mountpoint do root padrão")?;
    let out = Command::new("mount")
        .args(["-o", &format!("ro,subvol=/{}", ctx.default_subvol)])
        .arg(&ctx.device)
        .arg(&path)
        .output()
        .context("mount do subvol padrão falhou")?;
    if !out.status.success() {
        let _ = std::fs::remove_dir(&path);
        bail!(
            "mount -o ro,subvol=/{} {} -> {}: {}",
            ctx.default_subvol,
            ctx.device,
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(RescueMount { path })
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

/// `Ok(true)` quando o caso foi resolvido/terminal (sync aplicado, nada a fazer,
/// ou não montado); `Ok(false)` quando o usuário **declinou** a correção — o
/// caller de resgate usa isso para voltar ao menu em vez de encerrar.
fn diagnose_and_apply(target: &DoctorTarget, apply: bool) -> Result<bool> {
    let diagnosis = diagnosis_for(&target.root, &target.boot)?;
    doctor_ui::print_report(target, &diagnosis);
    match diagnosis.health {
        // /boot não montado: o relatório já mostrou a recuperação. Não há o que
        // sincronizar até montá-lo, e não é "nada a fazer".
        BootHealth::Unmounted => return Ok(true),
        BootHealth::NeedsSync(_) => {}
        BootHealth::NativeBoot | BootHealth::Synced => {
            doctor_ui::print_no_action_needed();
            return Ok(true);
        }
    }

    doctor_ui::print_suggested_sync(target);
    let should_apply = apply || confirm("Aplicar correção agora?")?;
    if !should_apply {
        doctor_ui::print_correction_skipped();
        return Ok(false);
    }

    // Escreve no /boot — serializa com restore/delete/save pelo lock global. Pego
    // só aqui (não no main, nem antes do confirm): o Doctor inteiro sob lock daria
    // re-entrância com as opções de resgate que já pegam lock, e travar durante o
    // confirm interativo prenderia o lock no tempo de decisão do usuário.
    let _lock = crate::lock::acquire()?;
    boot::sync_fat32_paths(&target.root, &target.boot)?;
    doctor_ui::print_spacer();
    let diagnosis = diagnosis_for(&target.root, &target.boot)?;
    doctor_ui::print_report(target, &diagnosis);
    Ok(true)
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
    /// Como o root aparece na UI. Por padrão é o path; no resgate vira "/@" para
    /// não mostrar o mountpoint temporário (/run/snapgroup/doctor-at-...).
    pub(crate) root_display: String,
}

impl DoctorTarget {
    fn new(label: String, root: PathBuf, boot: PathBuf) -> Self {
        let root_display = root.display().to_string();
        Self {
            label,
            root,
            boot,
            root_display,
        }
    }

    fn with_root_display(mut self, display: String) -> Self {
        self.root_display = display;
        self
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
