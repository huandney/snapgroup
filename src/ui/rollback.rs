use console::style;
use std::path::Path;

pub(crate) fn print_deleted_regret(name: &str) {
    println!("  regret anterior deletado: {name}");
}

pub(crate) fn print_aside_delete_failed(aside_subvol: &str, aside_path: &Path, error: &anyhow::Error) {
    eprintln!(
        "{} regret anterior (aside {}) não foi deletado: {error:#}",
        style("⚠").yellow().bold(),
        aside_subvol
    );
    eprintln!(
        "   limpe manualmente: sudo btrfs subvolume delete {}",
        aside_path.display()
    );
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
