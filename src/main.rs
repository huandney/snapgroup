mod boot;
mod btrfs;
mod cli;
mod commands;
mod doctor;
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
        // Comandos mutantes usam lock global. List é read-only; boot-clean
        // pula limpo se houver contenção e tenta de novo no próximo boot.
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
        cli::Command::Doctor { root, boot, apply } => doctor::run(root, boot, apply),
        cli::Command::BootClean => match lock::try_acquire()? {
            Some(_lock) => commands::boot_clean(),
            None => {
                println!("snapg boot-clean: outro snapg está rodando; cleanup adiado para o próximo boot");
                Ok(())
            }
        },
    }
}
