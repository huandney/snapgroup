use crate::boot::{BootDiagnosis, BootHealth, BootIssue, RescueContext};
use crate::doctor::DoctorTarget;
use crate::ui::term::{THEME, clear_screen, header, line, title, tree_branch, tree_stem};
use anyhow::{Context, Result};
use console::style;
use std::path::Path;

pub(crate) fn select_target(targets: &[DoctorTarget]) -> Result<usize> {
    let labels: Vec<&str> = targets.iter().map(|target| target.label.as_str()).collect();
    clear_screen();
    header("Diagnóstico de boot");
    dialoguer::Select::with_theme(&THEME)
        .with_prompt("Escolha o sistema para diagnosticar")
        .items(&labels)
        .default(0)
        .clear(true)
        .report(false)
        .interact()
        .context("selecionar sistema")
}

pub(crate) fn print_boot_sync_failure(error: &anyhow::Error, recovery: &str) {
    eprintln!();
    eprintln!(
        "{} sincronização do /boot (FAT32) falhou: {error:#}",
        style("✗").red().bold()
    );
    eprintln!("  O rollback BTRFS foi aplicado, mas o kernel/initramfs em /boot");
    eprintln!("  NÃO corresponde aos módulos do snapshot restaurado.");
    eprintln!("  REINICIAR AGORA CAI EM EMERGENCY MODE — reboot bloqueado.");
    eprintln!();
    eprintln!("  Recuperação alternativa: {recovery}");
    eprintln!();
}

pub(crate) fn print_rescue_boot(ctx: &RescueContext) {
    clear_screen();
    header("Diagnóstico de boot");
    line(format_args!(
        "{} você está num snapshot de resgate",
        style("!").yellow().bold()
    ));
    println!("{} {:<COL$} {}", tree_branch(false), "montado em /", ctx.current_subvol);
    println!(
        "{} {:<COL$} {} (conforme fstab)",
        tree_branch(false),
        "boota",
        ctx.default_subvol
    );
    println!(
        "{} {:<COL$} sincronizar /boot daqui miraria o root errado",
        tree_branch(true),
        "risco"
    );
    println!();
    println!("  monte o root padrão e rode o doctor contra ele:");
    println!(
        "    {}",
        style(format!(
            "mount -o subvol=/{} {} /mnt/at",
            ctx.default_subvol, ctx.device
        ))
        .bold()
    );
    println!(
        "    {}",
        style("snapg doctor --root /mnt/at --boot /boot --apply").bold()
    );
}

pub(crate) fn print_report(target: &DoctorTarget, diagnosis: &BootDiagnosis) {
    clear_screen();
    header("Diagnóstico de boot");
    print_target(target);
    print_diagnosis_inner(&target.root, diagnosis);
}

fn print_target(target: &DoctorTarget) {
    line(format_args!(
        "{} {} {}",
        title("Alvo"),
        style("·").dim(),
        target.label
    ));
    println!("{} root  {}", tree_branch(false), target.root.display());
    println!("{} boot  {}", tree_branch(false), target.boot.display());
}

pub(crate) fn print_no_action_needed() {
    line(format_args!("{} nada a fazer", style("✓").green().bold()));
}

pub(crate) fn print_suggested_sync(target: &DoctorTarget) {
    println!();
    line(format_args!(
        "ação sugerida: sincronizar {} com {}",
        target.boot.display(),
        target.root.display()
    ));
}

pub(crate) fn print_correction_skipped() {
    line(format_args!("correção não aplicada"));
}

pub(crate) fn print_spacer() {
    println!();
}

pub(crate) fn print_diagnosis(root: &Path, diagnosis: &BootDiagnosis) {
    header("Diagnóstico de boot");
    print_diagnosis_inner(root, diagnosis);
}

/// Largura da coluna de rótulos do relatório (o maior é "kernel groups" = 13).
const COL: usize = 14;

fn print_diagnosis_inner(root: &Path, diagnosis: &BootDiagnosis) {
    println!("{} {:<COL$} {}", tree_branch(false), "filesystem", diagnosis.fstype);
    println!("{} {:<COL$} {}", tree_branch(false), "kernel groups", diagnosis.kernel_groups);
    println!("{} {:<COL$} {}", tree_branch(false), "initramfs", diagnosis.initramfs_files);
    match diagnosis.health {
        BootHealth::NativeBoot => {
            println!(
                "{} {:<COL$} nativo (/boot não é FAT32 separado)",
                tree_branch(true),
                "estado"
            );
            if root != Path::new("/") {
                println!(
                    "{}nota: se este sistema usa /boot FAT32 separado, monte-o em {} \
                     ou rode com --boot explícito",
                    tree_stem(true),
                    root.join("boot").display()
                );
            }
        }
        BootHealth::Synced => {
            println!(
                "{} {:<COL$} coerente com o root alvo",
                tree_branch(true),
                "estado"
            );
        }
        BootHealth::NeedsSync(BootIssue::KernelMismatch) => {
            println!(
                "{} {:<COL$} {} dessincronizado com o root alvo",
                tree_branch(true),
                "estado",
                style("✗").red().bold()
            );
        }
        BootHealth::NeedsSync(BootIssue::InterruptedSync) => {
            println!(
                "{} {:<COL$} {} sincronização incompleta (backup de boot remanescente)",
                tree_branch(true),
                "estado",
                style("✗").red().bold()
            );
        }
        BootHealth::Unmounted => print_unmounted(root),
    }
}

/// /boot deveria ser FAT32 (fstab) mas não está montado. Não dá pra diagnosticar
/// nem sincronizar daqui; orienta montar ou recuperar por Live ISO.
fn print_unmounted(root: &Path) {
    let boot = root.join("boot");
    println!(
        "{} {:<COL$} {} /boot não está montado (fstab declara FAT32)",
        tree_branch(true),
        "estado",
        style("✗").red().bold()
    );
    println!();
    println!("  monte-o e rode de novo:  {}", style(format!("mount {}", boot.display())).bold());
    println!();
    println!("  se o kernel atual não montar vfat (emergency shell), use uma Live ISO:");
    println!("    1. boot pela Live ISO");
    println!("    2. monte o root BTRFS em /mnt");
    println!("    3. monte a partição FAT32 em /mnt/boot");
    println!(
        "    4. {}",
        style("snapg doctor --root /mnt --boot /mnt/boot --apply").bold()
    );
}
