use crate::group::{self, Group, GroupId};
use crate::orphan::OrphanBackup;
use crate::snapper;
use crate::ui::checkpoints::{CheckpointColumns, KERNEL_HEADER, NAME_HEADER, kernel_label, name_cell};
use crate::ui::term::{
    AltScreen, app_header, clear_screen, ellipsize, header, input_line, line, note, result,
    result_section, section_header, short_datetime, warn,
};
use anyhow::Result;
use console::style;
use std::collections::HashMap;

/// Wizard do `snapg save` sem nome: membros e kernel como informação (a
/// seleção do que restaurar mora no restore — checkpoint completo é grátis no
/// CoW e parcial quebraria o sinal "grupo incompleto = suspeito" do doctor),
/// campo de nome no editor padrão. Primeira página do fluxo: Esc sai.
pub(crate) fn prompt_save_name(
    mountpoints: &mut [String],
    kernel: &str,
    placeholder: &str,
    keep_groups: usize,
) -> Result<Option<String>> {
    let _alt = AltScreen::enter();
    let badges = member_badges(mountpoints);
    let kernel = kernel.to_string();
    input_line("Nome", "", placeholder, "enter confirma · esc sai", move || {
        clear_screen();
        header("Salvar checkpoint");
        line(format_args!("{:<9} {}", "membros", badges));
        line(format_args!("{:<9} {}", "kernel", kernel));
        // Sem retenção (KEEP_GROUPS=0) a diferença não muda nada: sem aviso.
        if keep_groups > 0 {
            line(format_args!(
                "{:<9} {}",
                "retenção",
                style(format!(
                    "nome padrão entra nos {keep_groups} automáticos mantidos · nome digitado fixa"
                ))
                .dim()
            ));
        }
        println!();
    })
}

pub(crate) fn print_save_cancelled() {
    note(format_args!("save cancelado"));
}

/// Confirmação do save no vocabulário do `list`: nome primeiro, membros
/// (root primeiro) e ID em dim. Os números por-config do snapper são detalhe
/// interno — nenhum outro comando os pede.
pub(crate) fn print_save_created(id: i64, desc: &str, mountpoints: &mut [String]) {
    result_section("Salvo");
    result(format_args!(
        "{}   {}",
        style(ellipsize(desc, NAME_COL_MAX)).bold(),
        style(format!("{} · #{id}", member_badges(mountpoints))).dim()
    ));
}

pub(crate) fn print_orphan_deleted(orphan: &OrphanBackup) {
    result_section("Apagado");
    result(format_args!(
        "backup órfão {}   {}",
        style(short_datetime(&orphan.created)).bold(),
        style(orphan.bases.join(" ")).dim()
    ));
}

pub(crate) fn print_orphan_delete_failed(name: &str, e: &anyhow::Error) {
    warn(format_args!("falha ao apagar {name}: {e:#}"));
}

pub(crate) fn print_orphan_scan_failed(e: &anyhow::Error) {
    warn(format_args!("backups órfãos não listados: {e:#}"));
}

/// Espaço livre antes e depois do delete, ao lado do previsto pela tela.
/// `liberados` vem do statvfs, que só enxerga dados: compara com a parte de
/// dados da previsão; o metadado sai junto, mas fica reservado no btrfs.
pub(crate) fn print_space_freed(before: u64, after: u64, predicted: Option<crate::space::Freed>, synced: bool) {
    let human = crate::ui::delete::human_bytes;
    let predicted_data = predicted
        .map(|p| format!("   {}", style(format!("previsto {}", human(p.data))).dim()))
        .unwrap_or_default();
    result_section("Espaço");
    result(format_args!(
        "{:<10} {}{predicted_data}",
        "liberados",
        style(human(after.saturating_sub(before))).bold()
    ));
    result(format_args!(
        "{:<10} {}",
        "livre",
        style(free_range(before, after)).bold()
    ));
    if let Some(p) = predicted.filter(|p| p.metadata > 0) {
        note(format_args!("{:<10} {} ficam reservados no btrfs", "metadados", human(p.metadata)));
    }
    if !synced {
        note(format_args!("o btrfs ainda está liberando blocos; o livre pode crescer mais"));
    }
}

/// "107.4 → 111.2 GiB": com a mesma unidade dos dois lados, ela sai uma vez.
fn free_range(before: u64, after: u64) -> String {
    let human = crate::ui::delete::human_bytes_parts;
    let ((before, unit), (after, after_unit)) = (human(before), human(after));
    if unit == after_unit {
        return format!("{before} → {after} {unit}");
    }
    format!("{before} {unit} → {after} {after_unit}")
}

pub(crate) fn print_delete_cancelled() {
    note(format_args!("cancelado"));
}

pub(crate) fn print_delete_done(g: &Group) {
    result_section("Apagado");
    result(format_args!("grupo {} apagado ({} membros)", g.id, g.members.len()));
}

pub(crate) fn print_trash_done(n: usize) {
    result_section("Lixeira");
    result(format_args!(
        "{}",
        count_text(n, "checkpoint movido para a lixeira", "checkpoints movidos para a lixeira")
    ));
}

pub(crate) fn print_trash_member_failed(config: &str, number: u32, e: &anyhow::Error) {
    warn(format_args!("falha ao marcar {config} #{number} como lixeira: {e:#}"));
}

pub(crate) fn print_cleanup_done(n: usize) {
    result_section("Retenção");
    result(format_args!(
        "{}",
        count_text(n, "grupo antigo movido para a lixeira", "grupos antigos movidos para a lixeira")
    ));
}

pub(crate) fn print_cleanup_failed(e: &anyhow::Error) {
    warn(format_args!("cleanup falhou: {e:#}"));
}

pub(crate) fn print_purge_done(n: usize) {
    result_section("Retenção");
    result(format_args!(
        "{}",
        count_text(n, "grupo expirado apagado da lixeira", "grupos expirados apagados da lixeira")
    ));
}

pub(crate) fn print_purge_failed(e: &anyhow::Error) {
    warn(format_args!("purge da lixeira falhou: {e:#}"));
}

pub(crate) fn print_purge_member_failed(id: GroupId, e: &anyhow::Error) {
    warn(format_args!("falha ao purgar grupo {id}: {e:#}"));
}

pub(crate) fn print_trash_empty() {
    note(format_args!("lixeira vazia"));
}

pub(crate) fn print_trash_cancelled() {
    note(format_args!("cancelado"));
}

pub(crate) fn print_untrash_done(n: usize) {
    result_section("Restaurado");
    result(format_args!(
        "{}",
        count_text(n, "grupo restaurado da lixeira", "grupos restaurados da lixeira")
    ));
}

fn count_text(n: usize, singular: &str, plural: &str) -> String {
    let word = if n == 1 { singular } else { plural };
    format!("{n} {word}")
}

pub(crate) fn print_untrash_member_failed(config: &str, number: u32, e: &anyhow::Error) {
    warn(format_args!("falha ao restaurar {config} #{number} da lixeira: {e:#}"));
}

pub(crate) fn print_no_groups() {
    note(format_args!("nenhum grupo snapg save encontrado"));
}

const ID_COL: usize = 10;
const DATE_HEADER: &str = "Data";
const MEMBERS_HEADER: &str = "Membros";
const DATE_COL: usize = 16;
/// Limite da coluna de nome no `list`: respeita o maior nome comum, mas impede
/// uma descrição muito longa de empurrar kernel/data para fora da tela.
const NAME_COL_MAX: usize = 36;

pub(crate) fn print_groups(
    groups: &[Group],
    kernel_labels: &HashMap<GroupId, String>,
    show_app_header: bool,
) -> Result<()> {
    if show_app_header {
        header("Checkpoints");
    } else {
        section_header("▪", "Checkpoints");
    }
    let mut rows = Vec::with_capacity(groups.len());
    for g in groups {
        let mut mountpoints = Vec::with_capacity(g.members.len());
        for m in &g.members {
            mountpoints.push(snapper::config_subvolume(&m.config)?);
        }
        rows.push((g, member_badges(&mut mountpoints)));
    }

    let columns = CheckpointColumns::new(
        groups,
        kernel_labels,
        NAME_HEADER.len(),
        NAME_COL_MAX,
        KERNEL_HEADER.len(),
    );
    let kernel_col = columns.kernel;
    let members_col = rows
        .iter()
        .map(|(_, members)| members.chars().count())
        .max()
        .unwrap_or(MEMBERS_HEADER.len())
        .max(MEMBERS_HEADER.len());
    let name_col = columns.name.min(list_name_col_for_terminal(kernel_col, members_col));

    line(format_args!(
        "{}   {}   {}   {}   {}",
        style(format!("{:<ID_COL$}", "ID")).bold(),
        style(format!("{:<name_col$}", NAME_HEADER)).bold(),
        style(format!("{:<kernel_col$}", KERNEL_HEADER)).bold(),
        style(format!("{:<DATE_COL$}", DATE_HEADER)).bold(),
        style(MEMBERS_HEADER).bold()
    ));

    for (g, members) in rows {
        let desc_cell = name_cell(g, name_col);
        let kernel = kernel_label(kernel_labels, g.id);
        line(format_args!(
            "{}   {}   {:<kernel_col$}   {:<DATE_COL$}   {}",
            style(format!("{:>ID_COL$}", g.id)).dim(),
            desc_cell,
            style(kernel).dim(),
            style(short_datetime(group::date(g))).dim(),
            style(members).dim(),
        ));
    }
    Ok(())
}

fn list_name_col_for_terminal(kernel_col: usize, members_col: usize) -> usize {
    let width = console::Term::stdout().size().1 as usize;
    let fixed = ID_COL + kernel_col + DATE_COL + members_col + (4 * 3);
    width
        .saturating_sub(crate::ui::term::CONTENT_INDENT.chars().count())
        .saturating_sub(fixed)
        .max(NAME_HEADER.len())
}

fn member_badges(mountpoints: &mut [String]) -> String {
    mountpoints.sort_by(|a, b| match (a.as_str(), b.as_str()) {
        ("/", "/") => std::cmp::Ordering::Equal,
        ("/", _) => std::cmp::Ordering::Less,
        (_, "/") => std::cmp::Ordering::Greater,
        _ => a.cmp(b),
    });
    mountpoints
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn print_pending_restore_status() {
    app_header();
    section_header("⏳", "Restauração pendente de reboot");
    line(format_args!(
        "Rode 'snapg restore' para concluir (reiniciar) ou cancelar a restauração."
    ));
    println!();
}

pub(crate) fn print_regret_status(creation_time: &str, kernel: &str, saved: bool) {
    app_header();
    let title = if saved { "Regret ativo guardado" } else { "Regret ativo" };
    section_header("↺", title);
    let kernel_col = kernel.chars().count().max(KERNEL_HEADER.len());
    line(format_args!(
        "{}   {}   {}",
        style(format!("{:<kernel_col$}", KERNEL_HEADER)).bold(),
        style(format!("{:<DATE_COL$}", DATE_HEADER)).bold(),
        style("Ação").bold()
    ));
    line(format_args!(
        "{}   {}   {}",
        style(format!("{:<kernel_col$}", kernel)).dim(),
        style(format!("{:<DATE_COL$}", short_datetime(creation_time))).dim(),
        style("snapg restore").dim()
    ));
    println!();
}

#[cfg(test)]
mod tests {
    use super::free_range;

    const GIB: u64 = 1 << 30;
    const MIB: u64 = 1 << 20;

    #[test]
    fn free_range_prints_shared_unit_once() {
        assert_eq!(free_range(107 * GIB, 111 * GIB), "107.0 → 111.0 GiB");
    }

    #[test]
    fn free_range_keeps_both_units_when_they_differ() {
        assert_eq!(free_range(900 * MIB, 2 * GIB), "900 MiB → 2.0 GiB");
    }
}
