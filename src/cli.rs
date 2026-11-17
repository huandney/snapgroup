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
        /// Uso interno: checkpoint do hook do pacman (pacotes no stdin).
        #[arg(long, hide = true, conflicts_with = "description")]
        pacman: bool,
    },
    /// Restauração interativa: seleciona checkpoint ou regret via TUI
    Restore,
    /// Lista grupos existentes
    List,
    /// Mostra o que cada checkpoint do pacman instalou, atualizou ou removeu
    Log,
    /// Ajustes do snapgroup numa tela: retenção, lixeira, restore e o hook do pacman
    Setup,
    /// Quem faz os checkpoints do pacman: sem argumento mostra; on = snapg, off = snap-pac
    Pacman {
        #[command(subcommand)]
        action: Option<PacmanAction>,
    },
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
    /// Gerencia a lixeira: restaura grupos ou apaga de vez
    Trash,
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
    /// Uso interno: falha se há restauração pendente. Chamado pelo hook do pacman.
    #[command(hide = true)]
    PendingCheck,
    /// Diagnóstico: mede a estimativa exata de espaço liberado.
    #[command(hide = true)]
    SpaceProbe,
}

#[derive(Subcommand)]
pub enum PacmanAction {
    /// O snapg faz um checkpoint agrupado por transação; o snap-pac é desligado
    On,
    /// Volta ao snap-pac; o hook do snapg é desligado
    Off,
}
