use crate::ui::term::{clear_screen, header, line, tree_branch, truncate_for_terminal};
use console::style;
use std::path::Path;
use std::time::{Duration, Instant};

const STEP_COUNT: usize = 5;

/// Intervalo mínimo entre redesenhos durante o stream ao vivo. O mkinitcpio
/// dispara dezenas de linhas em rajada; sem isso, seriam dezenas de
/// `clear_screen` por segundo (flicker). 40ms ≈ 25fps, suave e responsivo.
const LIVE_THROTTLE: Duration = Duration::from_millis(40);

#[derive(Clone, Copy)]
enum StepStatus {
    Pending,
    Running,
    Done,
    Synced,
    Failed,
}

pub(crate) struct BootSyncPanel {
    steps: [StepStatus; STEP_COUNT],
    current_step: usize,
    summary: String,
    execution_label: &'static str,
    execution: String,
    /// Última linha real emitida pelo comando em curso (mkinitcpio etc.). Quando
    /// `Some`, sobrescreve `execution` na linha de baixo — é o feed ao vivo.
    /// Volta a `None` ao fim de cada etapa, restaurando a descrição estática.
    live: Option<String>,
    last_render: Option<Instant>,
}

impl BootSyncPanel {
    pub(crate) fn new() -> Self {
        Self {
            steps: [StepStatus::Pending; STEP_COUNT],
            current_step: 1,
            summary: String::from("preparando sincronização"),
            execution_label: "executando",
            execution: String::from("analisar /boot"),
            live: None,
            last_render: None,
        }
    }

    /// Feed ao vivo: registra a linha mais recente do comando e redesenha o
    /// painel, respeitando o throttle pra não floodar o terminal. Passada como
    /// callback ao `proc::run_streamed`.
    pub(crate) fn stream(&mut self, line: &str) {
        self.live = Some(line.trim_end().to_string());
        let ready = self
            .last_render
            .is_none_or(|t| t.elapsed() >= LIVE_THROTTLE);
        if ready {
            self.render();
        }
    }

    pub(crate) fn already_synced(&mut self) {
        clear_screen();
        header("Sincronização de boot");
        println!();
        line(format_args!("etapa 1 de 1 · /boot já corresponde ao snapshot"));
        println!(
            "{} {:<12} {}",
            tree_branch(true),
            "estado",
            style("sincronizado").green().bold()
        );
        println!();
        line(format_args!("executado"));
        line(format_args!("nenhuma sincronização necessária"));
    }

    pub(crate) fn start_backup(&mut self, boot: &Path, backup: &Path) {
        self.start(
            0,
            "criando backup",
            format!("backup {} -> {}", boot.display(), backup.display()),
        );
    }

    pub(crate) fn finish_backup(&mut self) {
        self.finish(0, "backup criado");
    }

    /// Resync de interrupção: o backup remanescente é preservado como rollback,
    /// não recriado. Marca a etapa como concluída sem refazer o backup.
    pub(crate) fn reuse_backup(&mut self, backup: &Path) {
        self.start(
            0,
            "reusando backup existente",
            format!("backup preservado em {}", backup.display()),
        );
        self.finish(0, "backup preservado");
    }

    pub(crate) fn start_vmlinuz(&mut self, kver: &str, dest: &Path) {
        self.start(
            1,
            "copiando vmlinuz",
            format!("copiar vmlinuz {kver} -> {}", short_path(dest)),
        );
    }

    pub(crate) fn finish_vmlinuz(&mut self) {
        self.finish(1, "vmlinuz copiado");
    }

    pub(crate) fn start_initramfs(&mut self, kver: &str, dest: &Path) {
        self.start(
            2,
            "regenerando initramfs",
            format!("mkinitcpio -k {kver} -g {}", short_path(dest)),
        );
    }

    pub(crate) fn finish_initramfs(&mut self) {
        self.finish(2, "initramfs gerado");
    }

    pub(crate) fn start_limine(&mut self, boot: &Path) {
        self.start(
            3,
            "atualizando limine.conf",
            format!("atualizar hashes BLAKE2B em {}", boot.join("limine.conf").display()),
        );
    }

    pub(crate) fn finish_limine(&mut self) {
        self.finish(3, "limine.conf atualizado");
    }

    pub(crate) fn start_verify(&mut self) {
        self.start(
            4,
            "verificando sincronização",
            String::from("comparar /boot com o snapshot restaurado"),
        );
    }

    pub(crate) fn finish_synced(&mut self) {
        self.current_step = STEP_COUNT;
        self.summary = String::from("sincronização concluída");
        self.execution_label = "executado";
        self.execution = String::from("kernel, initramfs e limine.conf sincronizados");
        self.steps[4] = StepStatus::Synced;
        self.render();
    }

    pub(crate) fn fail_current(&mut self, summary: &str) {
        let current = self.current_step.saturating_sub(1).min(STEP_COUNT - 1);
        let idx = match self.steps[current] {
            StepStatus::Done | StepStatus::Synced => STEP_COUNT - 1,
            StepStatus::Pending | StepStatus::Running | StepStatus::Failed => current,
        };
        self.summary = summary.to_string();
        self.execution_label = "falhou";
        if matches!(self.steps[current], StepStatus::Done | StepStatus::Synced) {
            self.current_step = STEP_COUNT;
            self.execution = String::from("ver detalhes do erro abaixo");
        }
        self.steps[idx] = StepStatus::Failed;
        self.render();
    }

    fn start(&mut self, idx: usize, summary: &str, execution: String) {
        self.current_step = idx + 1;
        self.summary = summary.to_string();
        self.execution_label = "executando";
        self.execution = execution;
        self.live = None;
        self.steps[idx] = StepStatus::Running;
        self.render();
    }

    fn finish(&mut self, idx: usize, summary: &str) {
        self.current_step = idx + 1;
        self.summary = summary.to_string();
        self.execution_label = "executado";
        self.live = None;
        self.steps[idx] = StepStatus::Done;
        self.render();
    }

    fn render(&mut self) {
        clear_screen();
        header("Sincronização de boot");
        println!();
        line(format_args!(
            "etapa {} de {} · {}",
            self.current_step, STEP_COUNT, self.summary
        ));
        for (idx, label) in ["backup", "vmlinuz", "initramfs", "limine.conf", "estado"]
            .iter()
            .enumerate()
        {
            let last = idx == STEP_COUNT - 1;
            let status = self.status_text(self.steps[idx]);
            let label = format!("{label:<12}");
            match self.steps[idx] {
                StepStatus::Pending => {
                    println!("{} {} {}", tree_branch(last), style(label).dim(), status);
                }
                StepStatus::Running => {
                    println!("{} {} {}", tree_branch(last), style(label).bold(), status);
                }
                StepStatus::Done | StepStatus::Synced | StepStatus::Failed => {
                    println!("{} {} {}", tree_branch(last), label, status);
                }
            }
        }
        println!();
        line(format_args!("{}", self.execution_label));
        // A linha viva pode ser longa (mkinitcpio); trunca à largura pra não dar
        // wrap e quebrar o alinhamento do painel.
        let exec = self.live.as_deref().unwrap_or(&self.execution);
        line(format_args!("{}", truncate_for_terminal(exec, 0)));
        self.last_render = Some(Instant::now());
    }

    fn status_text(&self, status: StepStatus) -> String {
        match status {
            StepStatus::Pending => style("aguardando").dim().to_string(),
            StepStatus::Running => style("em execução").bold().to_string(),
            StepStatus::Done => String::from("concluído"),
            StepStatus::Synced => style("sincronizado").green().bold().to_string(),
            StepStatus::Failed => style("falhou").red().bold().to_string(),
        }
    }
}

pub(crate) fn print_restore_backup_after_failure() {
    eprintln!("  restaurando backup de /boot");
}

pub(crate) fn print_backup_restore_failed(error: &anyhow::Error) {
    eprintln!("  restauração do backup falhou: {error:#}");
}

pub(crate) fn print_backup_restored() {
    println!("  ficheiros de boot restaurados do backup");
}

pub(crate) fn print_backup_cleanup_failed(error: &std::io::Error) {
    eprintln!(
        "  {} /boot sincronizado, mas a limpeza do backup falhou: {error}",
        style("!").yellow().bold()
    );
    eprintln!("  o sistema está bootável; rode 'snapg doctor' para remover o backup");
}

fn short_path(path: &Path) -> String {
    let Some(file_name) = path.file_name() else {
        return path.display().to_string();
    };
    let Some(parent_name) = path.parent().and_then(Path::file_name) else {
        return path.display().to_string();
    };
    format!("{}/{}", parent_name.to_string_lossy(), file_name.to_string_lossy())
}
