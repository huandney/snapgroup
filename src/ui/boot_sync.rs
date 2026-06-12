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

#[derive(Clone, Copy)]
enum PanelKind {
    BootOnly,
    Restore,
    Cancel,
}

pub(crate) struct BootSyncPanel {
    steps: [StepStatus; STEP_COUNT],
    kind: PanelKind,
    btrfs: Option<StepStatus>,
    current_boot_step: usize,
    current_step: usize,
    summary: String,
}

impl BootSyncPanel {
    pub(crate) fn new() -> Self {
        Self {
            steps: [StepStatus::Pending; STEP_COUNT],
            kind: PanelKind::BootOnly,
            btrfs: None,
            current_boot_step: 0,
            current_step: 1,
            summary: String::from("preparando sincronização"),
        }
    }

    pub(crate) fn restore_after_btrfs() -> Self {
        Self {
            steps: [StepStatus::Pending; STEP_COUNT],
            kind: PanelKind::Restore,
            btrfs: Some(StepStatus::Done),
            current_boot_step: 0,
            current_step: 2,
            summary: String::from("preparando sincronização"),
        }
    }

    pub(crate) fn cancel_before_btrfs() -> Self {
        Self {
            steps: [StepStatus::Pending; STEP_COUNT],
            kind: PanelKind::Cancel,
            btrfs: Some(StepStatus::Pending),
            current_boot_step: 0,
            current_step: 1,
            summary: String::from("preparando sincronização"),
        }
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
        self.current_step = self.display_step(4);
        self.summary = String::from("sincronização concluída");
        self.steps[4] = StepStatus::Synced;
        self.render();
    }

    pub(crate) fn finish_btrfs(&mut self) {
        self.btrfs = Some(StepStatus::Done);
        self.current_step = self.btrfs_step().unwrap_or(self.current_step);
        self.summary = String::from("Btrfs concluído");
        self.render();
    }

    pub(crate) fn fail_current(&mut self, summary: &str) {
        let current = self.current_boot_step.min(STEP_COUNT - 1);
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
        self.current_boot_step = idx;
        self.current_step = self.display_step(idx);
        self.summary = summary.to_string();
        self.steps[idx] = StepStatus::Running;
        self.render();
    }

    fn finish(&mut self, idx: usize, summary: &str) {
        self.current_boot_step = idx;
        self.current_step = self.display_step(idx);
        self.summary = summary.to_string();
        self.steps[idx] = StepStatus::Done;
        self.render();
    }

    fn render(&self) {
        clear_screen();
        header(self.title());
        println!();
        line(format_args!(
            "etapa {} de {} · {}",
            self.current_step,
            self.total_steps(),
            self.summary
        ));
        for (idx, (label, status)) in self.render_steps().iter().enumerate() {
            let last = idx == self.total_steps() - 1;
            let status_text = self.status_text(*status);
            let label = format!("{label:<12}");
            match *status {
                StepStatus::Pending => {
                    println!("{} {} {}", tree_branch(last), style(label).dim(), status_text);
                }
                StepStatus::Running => {
                    println!("{} {} {}", tree_branch(last), style(label).bold(), status_text);
                }
                StepStatus::Done | StepStatus::Synced | StepStatus::Failed => {
                    println!("{} {} {}", tree_branch(last), label, status_text);
                }
            }
        }
        // Sem bloco "executando/comando": a linha de etapa já anuncia a ação, e
        // o spinner ao vivo (durante o initramfs) é desenhado logo abaixo daqui
        // pelo caller. O kernel aparece no próprio stream ("Starting build: …").
    }

    fn title(&self) -> &'static str {
        match self.kind {
            PanelKind::BootOnly => "Sincronização de boot",
            PanelKind::Restore => "Restauração",
            PanelKind::Cancel => "Cancelamento",
        }
    }

    fn total_steps(&self) -> usize {
        STEP_COUNT
            + usize::from(self.btrfs.is_some())
            + usize::from(matches!(self.kind, PanelKind::Restore))
    }

    fn display_step(&self, boot_idx: usize) -> usize {
        match self.kind {
            PanelKind::BootOnly | PanelKind::Cancel => boot_idx + 1,
            PanelKind::Restore => boot_idx + 2,
        }
    }

    fn btrfs_step(&self) -> Option<usize> {
        self.btrfs?;
        Some(match self.kind {
            PanelKind::Restore => 1,
            PanelKind::Cancel => STEP_COUNT + 1,
            PanelKind::BootOnly => return None,
        })
    }

    fn render_steps(&self) -> Vec<(&'static str, StepStatus)> {
        let boot = [
            ("backup /boot", self.steps[0]),
            ("vmlinuz", self.steps[1]),
            ("initramfs", self.steps[2]),
            ("limine.conf", self.steps[3]),
            ("verificação", self.steps[4]),
        ];
        let mut rows = Vec::with_capacity(self.total_steps());
        if matches!(self.kind, PanelKind::Restore)
            && let Some(status) = self.btrfs
        {
            rows.push(("Btrfs", status));
        }
        rows.extend(boot);
        if matches!(self.kind, PanelKind::Cancel)
            && let Some(status) = self.btrfs
        {
            rows.push(("Btrfs", status));
        }
        if matches!(self.kind, PanelKind::Restore) {
            rows.push(("reboot", StepStatus::Pending));
        }
        rows
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
