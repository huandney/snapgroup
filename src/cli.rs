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
    /// Reverte o grupo mais recente criado por save (exige reboot)
    Undo {
        /// Pula confirmação interativa
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Desfaz o último undo restaurando os backups (botão de pânico, exige reboot)
    Redo {
        /// Pula confirmação interativa
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Lista grupos existentes
    List,
    /// Apaga o grupo mais recente criado por save
    Delete {
        /// Pula confirmação interativa
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Apaga TODOS os subvolumes de backup deixados por undos antigos.
    /// AVISO: depois disso, `snapg redo` não consegue mais restaurar esses pontos no tempo.
    Gc {
        /// Pula confirmação interativa
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Uso interno: limpa redo-discards e se desarma do systemd. Não invoque manualmente.
    #[command(hide = true)]
    BootClean,
}
