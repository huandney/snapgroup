mod boot;
mod btrfs;
mod cli;
mod commands;
mod group;
mod lock;
mod rollback;
mod snapper;
mod sudo;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    sudo::ensure_root()?;
    match cli.command {
        // Comandos mutantes: lock global exclusivo enquanto rodam. List é
        // read-only e boot-clean é pós-boot (tratado depois com regra própria).
        cli::Command::Save { description } => {
            let _lock = lock::acquire()?;
            commands::save(description)
        }
        cli::Command::Restore => {
            let _lock = lock::acquire()?;
            commands::restore()
        }
        cli::Command::Delete { yes } => {
            let _lock = lock::acquire()?;
            commands::delete(yes)
        }
        cli::Command::List => commands::list(),
        cli::Command::BootClean => commands::boot_clean(),
    }
}
