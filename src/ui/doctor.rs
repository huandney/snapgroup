use crate::boot::{BootDiagnosis, BootHealth};
use crate::doctor::DoctorTarget;
use crate::ui::term::{THEME, branch, clear_screen, header, stem, title};
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

pub(crate) fn print_target(target: &DoctorTarget) {
    println!(
        "{} {} {}",
        title("Alvo"),
        style("·").dim(),
        target.label
    );
    println!("{} root  {}", branch(false), target.root.display());
    println!("{} boot  {}", branch(true), target.boot.display());
    println!();
}

pub(crate) fn print_no_action_needed() {
    println!("{} nada a fazer", style("✓").green().bold());
}

pub(crate) fn print_suggested_sync(target: &DoctorTarget) {
    println!();
    println!(
        "ação sugerida: sincronizar {} com {}",
        target.boot.display(),
        target.root.display()
    );
}

pub(crate) fn print_correction_skipped() {
    println!("correção não aplicada");
}

pub(crate) fn print_spacer() {
    println!();
}

pub(crate) fn print_diagnosis(root: &Path, diagnosis: &BootDiagnosis) {
    header("Diagnóstico de boot");
    println!("{} filesystem  {}", branch(false), diagnosis.fstype);
    match diagnosis.health {
        BootHealth::NativeBoot => {
            println!(
                "{} /boot não é FAT32 separado; nenhuma sincronização é necessária",
                branch(true)
            );
            if root != Path::new("/") {
                println!(
                    "{}nota: se este sistema usa /boot FAT32 separado, monte-o em {} \
                     ou rode com --boot explícito",
                    stem(true),
                    root.join("boot").display()
                );
            }
        }
        BootHealth::Synced => {
            println!(
                "{} /boot está coerente com o root alvo ({} kernel groups, {} initramfs)",
                branch(true),
                diagnosis.kernel_groups,
                diagnosis.initramfs_files
            );
        }
        BootHealth::NeedsSync => {
            println!(
                "{} {} /boot está dessincronizado com o root alvo ({} kernel groups, {} initramfs)",
                branch(true),
                style("✗").red().bold(),
                diagnosis.kernel_groups,
                diagnosis.initramfs_files
            );
        }
    }
}
