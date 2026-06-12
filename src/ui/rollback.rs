use console::style;
use std::path::Path;

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

pub(crate) fn print_stashed_regret_delete_failed(
    config: &str,
    stashed: &Path,
    error: &anyhow::Error,
) {
    eprintln!(
        "{} restore {}: Regret anterior substituído, mas o subvol temporário não foi deletado: {error:#}",
        style("⚠").yellow().bold(),
        config
    );
    eprintln!(
        "   limpe manualmente: sudo btrfs subvolume delete {}",
        stashed.display()
    );
}
