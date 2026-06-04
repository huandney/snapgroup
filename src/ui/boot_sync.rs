use crate::ui::term::{clear_screen, header, line, tree_branch};
use console::style;

const STEP_COUNT: usize = 5;

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
}

impl BootSyncPanel {
    pub(crate) fn new() -> Self {
        Self {
            steps: [StepStatus::Pending; STEP_COUNT],
            current_step: 1,
            summary: String::from("preparando sincronização"),
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
    }

    pub(crate) fn start_backup(&mut self) {
        self.start(0, "criando backup");
    }

    pub(crate) fn finish_backup(&mut self) {
        self.finish(0, "backup criado");
    }

    /// Resync de interrupção: o backup remanescente é preservado como rollback,
    /// não recriado. Marca a etapa como concluída sem refazer o backup.
    pub(crate) fn reuse_backup(&mut self) {
        self.start(0, "reusando backup existente");
        self.finish(0, "backup preservado");
    }

    pub(crate) fn start_vmlinuz(&mut self) {
        self.start(1, "copiando vmlinuz");
    }

    pub(crate) fn finish_vmlinuz(&mut self) {
        self.finish(1, "vmlinuz copiado");
    }

    pub(crate) fn start_initramfs(&mut self) {
        self.start(2, "regenerando initramfs");
    }

    pub(crate) fn finish_initramfs(&mut self) {
        self.finish(2, "initramfs gerado");
    }

    pub(crate) fn start_limine(&mut self) {
        self.start(3, "atualizando limine.conf");
    }

    pub(crate) fn finish_limine(&mut self) {
        self.finish(3, "limine.conf atualizado");
    }

    pub(crate) fn start_verify(&mut self) {
        self.start(4, "verificando sincronização");
    }

    pub(crate) fn finish_synced(&mut self) {
        self.current_step = STEP_COUNT;
        self.summary = String::from("sincronização concluída");
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
        if matches!(self.steps[current], StepStatus::Done | StepStatus::Synced) {
            self.current_step = STEP_COUNT;
        }
        self.steps[idx] = StepStatus::Failed;
        self.render();
    }

    fn start(&mut self, idx: usize, summary: &str) {
        self.current_step = idx + 1;
        self.summary = summary.to_string();
        self.steps[idx] = StepStatus::Running;
        self.render();
    }

    fn finish(&mut self, idx: usize, summary: &str) {
        self.current_step = idx + 1;
        self.summary = summary.to_string();
        self.steps[idx] = StepStatus::Done;
        self.render();
    }

    fn render(&self) {
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
        // Sem bloco "executando/comando": a linha de etapa já anuncia a ação, e
        // o spinner ao vivo (durante o initramfs) é desenhado logo abaixo daqui
        // pelo caller. O kernel aparece no próprio stream ("Starting build: …").
        println!();
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
