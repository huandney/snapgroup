use clap::{Parser, Subcommand};
use std::path::PathBuf;

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
    /// Renomeia a descrição de um checkpoint snapg
    Rename {
        /// ID do checkpoint. Sem ID, abre seleção interativa.
        id: Option<i64>,
        /// Novo nome/descrição. Sem nome, pergunta interativamente.
        #[arg(num_args = 0.., trailing_var_arg = true)]
        description: Vec<String>,
    },
    /// Move checkpoints para a lixeira (purga após TRASH_PRUNE_DAYS)
    Delete {
        /// Sem TUI: opera no grupo mais recente
        #[arg(short = 'y', long)]
        yes: bool,
        /// Apaga permanentemente, ignorando a lixeira (irreversível)
        #[arg(long)]
        purge: bool,
    },
    /// Diagnostica o boot e oferece correção assistida quando necessário
    Doctor {
        /// Root do sistema alvo (ex: /mnt em recuperação por Live-USB)
        #[arg(long)]
        root: Option<PathBuf>,
        /// Boot do sistema alvo (ex: /mnt/boot em recuperação por Live-USB)
        #[arg(long)]
        boot: Option<PathBuf>,
        /// Aplica a correção sugerida sem perguntar novamente
        #[arg(long)]
        apply: bool,
    },
    /// Uso interno: limpa discards e se desarma do systemd. Não invoque manualmente.
    #[command(hide = true)]
    BootClean,
}
