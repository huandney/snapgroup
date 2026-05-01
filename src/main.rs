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
        cli::Command::Restore => commands::restore(),
        cli::Command::List => commands::list(),
        cli::Command::Delete { yes } => commands::delete(yes),
        cli::Command::BootClean => commands::boot_clean(),
    }
}
