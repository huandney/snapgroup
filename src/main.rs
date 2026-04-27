mod btrfs;
mod cli;
mod commands;
mod group;
mod rollback;
mod snapper;
mod sudo;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    sudo::ensure_root()?;
    match cli.command {
        cli::Command::Save { description } => commands::save(description),
        cli::Command::Undo { yes } => commands::undo(yes),
        cli::Command::Redo { yes } => commands::redo(yes),
        cli::Command::List => commands::list(),
        cli::Command::Delete { yes } => commands::delete(yes),
        cli::Command::Gc { yes } => commands::gc(yes),
        cli::Command::BootClean => commands::boot_clean(),
    }
}
