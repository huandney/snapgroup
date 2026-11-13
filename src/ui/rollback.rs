use crate::ui::term::{note, warn, warn_detail};
use std::path::Path;

pub(crate) fn print_deleted_regret(name: &str) {
    note(format_args!("regret anterior deletado: {name}"));
}

pub(crate) fn print_nested_displaced(dest: &Path, aside: &Path) {
    warn(format_args!(
        "{} já tinha conteúdo; o subvolume aninhado ficou no lugar e o conteúdo anterior foi preservado em {}",
        dest.display(),
        aside.display()
    ));
}

pub(crate) fn print_discard_delete_failed(config: &str, discard: &Path, error: &anyhow::Error) {
    warn(format_args!(
        "revert {config}: backup restaurado mas subvol descartado não foi deletado: {error:#}"
    ));
    warn_detail(format_args!("limpe manualmente: sudo btrfs subvolume delete {}", discard.display()));
}

pub(crate) fn print_stashed_regret_delete_failed(
    config: &str,
    stashed: &Path,
    error: &anyhow::Error,
) {
    warn(format_args!(
        "restore {config}: Regret anterior substituído, mas o subvol temporário não foi deletado: {error:#}"
    ));
    warn_detail(format_args!("limpe manualmente: sudo btrfs subvolume delete {}", stashed.display()));
}
