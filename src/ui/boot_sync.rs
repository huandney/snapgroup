use std::path::Path;

pub(crate) fn print_already_synced() {
    println!("  boot sync: /boot já corresponde ao snapshot, nada a fazer");
}

pub(crate) fn print_restore_backup_after_failure() {
    eprintln!("  boot sync: falhou, restaurando backup de /boot");
}

pub(crate) fn print_backup_restore_failed(error: &anyhow::Error) {
    eprintln!("  boot sync: restauração do backup falhou: {error:#}");
}

pub(crate) fn print_synced() {
    println!("  boot sync: kernel, initramfs e limine.conf sincronizados");
}

pub(crate) fn print_vmlinuz_copied(kver: &str, dest: &Path) {
    println!("  boot sync: vmlinuz ({kver}) → {}", dest.display());
}

pub(crate) fn print_initramfs_regenerated(kver: &str, dest: &Path) {
    println!("  boot sync: initramfs regenerado ({kver}) → {}", dest.display());
}

pub(crate) fn print_backup_created(backup: &Path) {
    println!("  boot sync: backup em {}", backup.display());
}

pub(crate) fn print_backup_restored() {
    println!("  boot sync: ficheiros de boot restaurados do backup");
}

pub(crate) fn print_hashes_updated() {
    println!("  boot sync: hashes BLAKE2B atualizados em /boot/limine.conf");
}
