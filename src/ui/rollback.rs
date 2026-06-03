use crate::ui::term::{clear_screen, header, line, tree_branch, truncate_for_terminal};
use console::style;
use std::path::Path;
use std::time::Instant;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Clone, Copy)]
enum MemberStatus {
    Pending,
    Running,
    Done,
}

/// Painel ao vivo do rollback. Diferente do boot sync, o passo pesado
/// (`btrfs subvolume snapshot`) não emite saída — bloqueia mudo no kernel. Então
/// a linha viva aqui não é um feed do processo, e sim o comando em curso + um
/// spinner/cronômetro, pra a fase pesada deixar de ser uma tela parada.
pub(crate) struct RollbackPanel {
    title: String,
    subtitle: String,
    members: Vec<(String, MemberStatus)>,
    summary: String,
    live: String,
    spin: usize,
    started: Option<Instant>,
}

impl RollbackPanel {
    pub(crate) fn new(title: &str, subtitle: String, configs: &[String]) -> Self {
        let members = configs
            .iter()
            .map(|c| (c.clone(), MemberStatus::Pending))
            .collect();
        Self {
            title: title.to_string(),
            subtitle,
            members,
            summary: String::from("preparando cópias graváveis"),
            live: String::from("montando ambiente"),
            spin: 0,
            started: None,
        }
    }

    /// Marca o membro como em preparação e fixa a linha viva no comando que vai
    /// rodar. `command` é o `btrfs subvolume snapshot ...` literal.
    pub(crate) fn start_prepare(&mut self, config: &str, command: String) {
        self.mark(config, MemberStatus::Running);
        self.summary = format!("preparando {config}");
        self.live = command;
        self.started = Some(Instant::now());
        self.render();
    }

    /// Avança o spinner. Passado como `on_tick` ao `btrfs::create_snapshot`.
    pub(crate) fn tick(&mut self) {
        self.spin = (self.spin + 1) % SPINNER.len();
        self.render();
    }

    pub(crate) fn finish_prepare(&mut self, config: &str) {
        self.mark(config, MemberStatus::Done);
        self.render();
    }

    /// Fase 2: renomeações atômicas (instantâneas, sem spinner). Só anuncia.
    pub(crate) fn start_commit(&mut self) {
        self.summary = String::from("aplicando — renomeações atômicas");
        self.live = String::from("trocando subvolumes ativos");
        self.started = None;
        self.spin = 0;
        self.render();
    }

    fn mark(&mut self, config: &str, status: MemberStatus) {
        for (name, st) in &mut self.members {
            if name == config {
                *st = status;
            }
        }
    }

    fn render(&mut self) {
        clear_screen();
        header(&self.title);
        println!();
        line(format_args!("{}", self.subtitle));
        println!();
        line(format_args!("{}", self.summary));
        let total = self.members.len();
        for (i, (name, st)) in self.members.iter().enumerate() {
            let last = i + 1 == total;
            let label = format!("{name:<12}");
            match st {
                MemberStatus::Pending => println!(
                    "{} {} {}",
                    tree_branch(last),
                    style(label).dim(),
                    style("aguardando").dim()
                ),
                MemberStatus::Running => println!(
                    "{} {} {}",
                    tree_branch(last),
                    style(label).bold(),
                    style("preparando").bold()
                ),
                MemberStatus::Done => {
                    println!("{} {} pronto", tree_branch(last), label)
                }
            }
        }
        println!();
        line(format_args!("executando"));
        let elapsed = self
            .started
            .map(|t| format!("  ({}s)", t.elapsed().as_secs()))
            .unwrap_or_default();
        let body = format!("{} {}{elapsed}", SPINNER[self.spin], self.live);
        line(format_args!("{}", truncate_for_terminal(&body, 0)));
    }
}

pub(crate) fn print_deleted_regret(name: &str) {
    println!("  regret anterior deletado: {name}");
}

pub(crate) fn print_discard_delete_failed(config: &str, discard: &Path, error: &anyhow::Error) {
    eprintln!(
        "{} revert {}: backup restaurado mas subvol descartado não foi deletado: {error:#}",
        style("⚠").yellow().bold(),
        config
    );
    eprintln!(
        "   limpe manualmente: sudo btrfs subvolume delete {}",
        discard.display()
    );
}
