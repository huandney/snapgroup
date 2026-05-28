use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "snapg",
    version,
    about = "Wrapper Snapper com snapshots agrupados por subvolume"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Cria snapshot em todas as configs snapper, agrupados por id
    Save {
        /// Descrição opcional do snapshot
        description: Option<String>,
    },
    /// Restauração interativa: seleciona checkpoint ou regret via TUI
    Restore,
    /// Lista grupos existentes
    List,
    /// Apaga o grupo mais recente criado por save
    Delete {
        /// Pula confirmação interativa
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Uso interno: limpa discards e se desarma do systemd. Não invoque manualmente.
    #[command(hide = true)]
    BootClean,
}
