use crate::boot::{BootDiagnosis, BootHealth, BootIssue, RescueContext};
use crate::doctor::DoctorTarget;
use crate::ui::term::{
    SELECT_MARKER, THEME, clear_screen, header, line, path, title, tree_branch, tree_stem,
    truncate_for_terminal,
};
use anyhow::{Context, Result};
use console::style;
use std::path::Path;

pub(crate) enum UndoDoneAction {
    Exit,
    ShowDiagnosis,
}

pub(crate) fn select_target(targets: &[DoctorTarget]) -> Result<usize> {
    let labels: Vec<&str> = targets.iter().map(|target| target.label.as_str()).collect();
    clear_screen();
    header("Diagnóstico de boot");
    dialoguer::Select::with_theme(&THEME)
        .with_prompt("Escolha o sistema para diagnosticar")
        .items(&labels)
        .default(0)
        .clear(true)
        .report(false)
        .interact()
        .context("selecionar sistema")
}

/// Espera enter/esc após uma ação terminal cujo status já está na tela (relatório
/// do doctor, "nada a fazer", "correção não aplicada"). Enter sai; Esc pede o
/// diagnóstico do sistema atual. Só imprime a dica de teclas — não limpa nem
/// reescreve o status acima.
pub(crate) fn wait_done_action() -> Result<UndoDoneAction> {
    println!();
    line(format_args!("{}", style("enter sai · esc mostra diagnóstico").dim()));
    loop {
        match console::Term::stdout()
            .read_key()
            .context("aguardar confirmação")?
        {
            console::Key::Enter => return Ok(UndoDoneAction::Exit),
            console::Key::Escape => return Ok(UndoDoneAction::ShowDiagnosis),
            _ => {}
        }
    }
}

pub(crate) fn print_boot_sync_failure(error: &anyhow::Error, recovery: &str) {
    eprintln!();
    eprintln!(
        "{} sincronização do /boot (FAT32) falhou: {error:#}",
        style("✗").red().bold()
    );
    eprintln!("  O rollback BTRFS foi aplicado, mas o kernel/initramfs em /boot");
    eprintln!("  NÃO corresponde aos módulos do snapshot restaurado.");
    eprintln!("  REINICIAR AGORA CAI EM EMERGENCY MODE — reboot bloqueado.");
    eprintln!();
    eprintln!("  Recuperação alternativa: {recovery}");
    eprintln!();
}

/// Menu de resolução no resgate. As duas pontas do desync são `"/boot"` e `"/"`.
/// Cada item diz o que mexe. ESC retorna `None` (volta/sai). Índice 0 = ajustar
/// `"/boot"`; demais = abrir o restore.
pub(crate) fn select_rescue_action(
    ctx: &RescueContext,
    current_kernel: &str,
    default_kernel: &str,
    boot_kernel: &str,
) -> Result<Option<usize>> {
    clear_screen();
    header("Diagnóstico de boot");
    line(format_args!(
        "{} os kernels de {} e do {} não estão sincronizados",
        style("!").yellow().bold(),
        path("/boot"),
        path("/")
    ));
    println!(
        "{} {:<COL$} {}  kernel {}  (resgate)",
        tree_branch(false),
        "montado",
        path(&ctx.current_subvol),
        current_kernel
    );
    println!(
        "{} {:<COL$} {}  kernel {}",
        tree_branch(false),
        "boota",
        path(&ctx.default_subvol),
        default_kernel
    );
    println!(
        "{} {:<COL$} {} {}  kernel {} (difere do {})",
        tree_branch(true),
        "boot",
        style("✗").red().bold(),
        path("/boot"),
        boot_kernel,
        path("/")
    );
    println!();
    // Itens em texto plano e truncados à largura do terminal. Cor/ANSI dentro de
    // um item do Select quebra a medição de largura, e item que dá wrap faz o
    // dialoguer "comer" as linhas acima a cada seta. A cor dos paths fica nas
    // linhas de diagnóstico acima, impressas direto.
    let choices = [
        format!(
            "Manter o root atual (kernel {default_kernel}) — ajusta o /boot · mantém root e home"
        ),
        "Restaurar só o / — escolher kernel · mantém home e root".to_string(),
        "Mudar o que boota — restore completo · outro snapshot ou desfazer".to_string(),
    ];
    let items: Vec<String> = choices
    .iter()
    .map(|t| truncate_for_terminal(t, SELECT_MARKER))
    .collect();
    dialoguer::Select::with_theme(&THEME)
        .with_prompt("Como resolver?")
        .items(&items)
        .default(0)
        .clear(true)
        .report(true)
        .interact_opt()
        .context("seleção cancelada")
}

pub(crate) fn print_rescue_mount_failed(error: &anyhow::Error) {
    eprintln!(
        "  {} não foi possível montar o root padrão para resolver automaticamente: {error:#}",
        style("!").yellow().bold()
    );
    eprintln!("  seguindo com instrução manual:");
    println!();
}

pub(crate) fn print_rescue_boot(ctx: &RescueContext) {
    clear_screen();
    header("Diagnóstico de boot");
    line(format_args!(
        "{} você está num snapshot de resgate",
        style("!").yellow().bold()
    ));
    println!("{} {:<COL$} {}", tree_branch(false), "montado em /", ctx.current_subvol);
    println!(
        "{} {:<COL$} {} (conforme fstab)",
        tree_branch(false),
        "boota",
        ctx.default_subvol
    );
    println!(
        "{} {:<COL$} sincronizar /boot daqui miraria o root errado",
        tree_branch(true),
        "risco"
    );
    println!();
    println!("  monte o root padrão e rode o doctor contra ele:");
    println!(
        "    {}",
        style(format!(
            "mount -o subvol=/{} {} /mnt/at",
            ctx.default_subvol, ctx.device
        ))
        .bold()
    );
    println!(
        "    {}",
        style("snapg doctor --root /mnt/at --boot /boot --apply").bold()
    );
}

pub(crate) fn print_report(target: &DoctorTarget, diagnosis: &BootDiagnosis) {
    clear_screen();
    header("Diagnóstico de boot");
    print_target(target, diagnosis);
    print_diagnosis_inner(&target.root, diagnosis);
}

fn print_target(target: &DoctorTarget, diagnosis: &BootDiagnosis) {
    line(format_args!(
        "{}  {}  {}",
        title("Alvo"),
        style("·").dim(),
        target.label
    ));
    println!(
        "{} {:<COL$} {:<PATH_COL$} {}",
        tree_branch(false),
        "root",
        path(&target.root_display),
        style(format!("kernel {}", diagnosis.root_kernel)).dim()
    );
    println!(
        "{} {:<COL$} {:<PATH_COL$} {}",
        tree_branch(false),
        "boot",
        path(&target.boot.display().to_string()),
        style(format!("kernel {}", diagnosis.boot_kernel)).dim()
    );
}

pub(crate) fn print_no_action_needed() {
    line(format_args!("{} nada a fazer", style("✓").green().bold()));
}

pub(crate) fn print_suggested_sync(target: &DoctorTarget) {
    println!();
    line(format_args!(
        "ação sugerida: sincronizar {} com {}",
        path(&target.boot.display().to_string()),
        path(&target.root_display)
    ));
}

pub(crate) fn print_correction_skipped() {
    line(format_args!("correção não aplicada"));
}

pub(crate) fn print_spacer() {
    println!();
}

pub(crate) fn print_diagnosis(root: &Path, diagnosis: &BootDiagnosis) {
    header("Diagnóstico de boot");
    print_diagnosis_inner(root, diagnosis);
}

/// Largura da coluna de rótulos do relatório (o maior é "kernel groups" = 13).
const COL: usize = 14;
const PATH_COL: usize = 8;

fn print_diagnosis_inner(root: &Path, diagnosis: &BootDiagnosis) {
    println!("{} {:<COL$} {}", tree_branch(false), "filesystem", diagnosis.fstype);
    println!("{} {:<COL$} {}", tree_branch(false), "kernel groups", diagnosis.kernel_groups);
    println!("{} {:<COL$} {}", tree_branch(false), "initramfs", diagnosis.initramfs_files);
    match diagnosis.health {
        BootHealth::NativeBoot => {
            println!(
                "{} {:<COL$} nativo (/boot não é FAT32 separado)",
                tree_branch(true),
                "estado"
            );
            if root != Path::new("/") {
                println!(
                    "{}nota: se este sistema usa /boot FAT32 separado, monte-o em {} \
                     ou rode com --boot explícito",
                    tree_stem(true),
                    root.join("boot").display()
                );
            }
        }
        BootHealth::Synced => {
            println!(
                "{} {:<COL$} coerente com o root alvo",
                tree_branch(true),
                "estado"
            );
        }
        BootHealth::NeedsSync(BootIssue::KernelMismatch) => {
            println!(
                "{} {:<COL$} {} dessincronizado com o root alvo",
                tree_branch(true),
                "estado",
                style("✗").red().bold()
            );
        }
        BootHealth::NeedsSync(BootIssue::InterruptedSync) => {
            println!(
                "{} {:<COL$} {} incompleta: kernels casam, mas há backup remanescente",
                tree_branch(true),
                "estado",
                style("✗").red().bold()
            );
        }
        BootHealth::NeedsSync(BootIssue::HashMismatch) => {
            println!(
                "{} {:<COL$} {} kernels casam, mas os hashes do limine.conf divergem (Limine recusa o boot)",
                tree_branch(true),
                "estado",
                style("✗").red().bold()
            );
        }
        BootHealth::Unmounted => print_unmounted(root),
    }
}

/// /boot deveria ser FAT32 (fstab) mas não está montado. Não dá pra diagnosticar
/// nem sincronizar daqui; orienta montar ou recuperar por Live ISO.
fn print_unmounted(root: &Path) {
    let boot = root.join("boot");
    println!(
        "{} {:<COL$} {} /boot não está montado (fstab declara FAT32)",
        tree_branch(true),
        "estado",
        style("✗").red().bold()
    );
    println!();
    println!("  monte-o e rode de novo:  {}", style(format!("mount {}", boot.display())).bold());
    println!();
    println!("  se o kernel atual não montar vfat (emergency shell), use uma Live ISO:");
    println!("    1. boot pela Live ISO");
    println!("    2. monte o root BTRFS em /mnt");
    println!("    3. monte a partição FAT32 em /mnt/boot");
    println!(
        "    4. {}",
        style("snapg doctor --root /mnt --boot /mnt/boot --apply").bold()
    );
}
