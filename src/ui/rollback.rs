use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::Path;
use std::time::Duration;

/// Spinner pra envolver a fase de rollback. O passo pesado
/// (`btrfs subvolume snapshot`) bloqueia mudo no kernel; o `indicatif` anima numa
/// thread própria (steady tick), então gira mesmo sem saída do processo. Em
/// rollback instantâneo, aparece e some — sem tela parada. Finalize com
/// `ProgressBar::finish_and_clear` ao terminar.
pub(crate) fn spinner(message: String) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::with_template("   {spinner} {msg}").unwrap());
    pb.enable_steady_tick(Duration::from_millis(120));
    pb.set_message(message);
    pb
}

pub(crate) fn print_deleted_regret(name: &str) {
    println!("  regret anterior deletado: {name}");
}

pub(crate) fn print_discard_delete_failed(config: &str, discard: &Path, error: &anyhow::Error) {
    eprintln!(
        "{} revert {}: backup restaurado mas subvol descartado não foi deletado: {error:#}",
        style("⚠").yellow().bold(),
        config
    );
    eprintln!(
        "   limpe manualmente: sudo btrfs subvolume delete {}",
        discard.display()
    );
}
