use crate::group::{self, Group, GroupId};
use crate::ui::term::{
    CONTENT_INDENT, clear_screen, content_width, header, line, short_datetime,
};
use anyhow::{Context, Result};
use console::style;
use std::collections::HashMap;

pub(crate) const KERNEL_HEADER: &str = "Kernel";
pub(crate) const NAME_HEADER: &str = "Nome";
pub(crate) const ORIGIN_HEADER: &str = "Origem";
/// Abaixo disso o Nome deixa de dizer algo: a coluna Origem sai antes.
pub(crate) const MIN_NAME_WITH_ORIGIN: usize = 10;
const DATE_HEADER: &str = "Data";
const MEMBERS_HEADER: &str = "Membros";
const ID_HEADER: &str = "ID";
const DATE_WIDTH: usize = 16;
const MEMBERS_WIDTH: usize = 7;

pub(crate) struct CheckpointColumns {
    pub(crate) name: usize,
    /// Largura da Origem; `0` = coluna escondida.
    pub(crate) origin: usize,
    pub(crate) kernel: usize,
    id: usize,
}

impl CheckpointColumns {
    pub(crate) fn new(
        groups: &[Group],
        kernel_labels: &HashMap<GroupId, String>,
        origins: &HashMap<GroupId, String>,
        minimum_name: usize,
        maximum_name: usize,
        minimum_kernel: usize,
    ) -> Self {
        let name = groups
            .iter()
            .map(|group| group::description(group).chars().count())
            .max()
            .unwrap_or(minimum_name)
            .max(minimum_name)
            .min(maximum_name);
        let kernel = groups
            .iter()
            .filter_map(|group| kernel_labels.get(&group.id))
            .map(|kernel| kernel.chars().count())
            .max()
            .unwrap_or(minimum_kernel)
            .max(minimum_kernel);
        let id = groups
            .iter()
            .map(|group| format!("#{}", group.id).chars().count())
            .max()
            .unwrap_or(ID_HEADER.len())
            .max(ID_HEADER.len());
        Self { name, origin: origin_width(origins), kernel, id }
    }

    pub(crate) fn fit_to_terminal(mut self, marker_width: usize) -> Self {
        let terminal_width = console::Term::stdout().size().1 as usize;
        self.fit_name_to_width(terminal_width, marker_width);
        self
    }

    fn fit_name_to_width(&mut self, terminal_width: usize, marker_width: usize) {
        let mut available = self.available_name(terminal_width, marker_width);
        if self.origin > 0 && available < self.name.min(MIN_NAME_WITH_ORIGIN) {
            self.origin = 0;
            available = self.available_name(terminal_width, marker_width);
        }
        self.name = self.name.min(available);
    }

    fn available_name(&self, terminal_width: usize, marker_width: usize) -> usize {
        let origin = if self.origin > 0 { self.origin + 3 } else { 0 };
        let fixed = self.kernel + DATE_WIDTH + 6 + 3 + MEMBERS_WIDTH + 3 + self.id + origin;
        terminal_width
            .saturating_sub(CONTENT_INDENT.chars().count())
            .saturating_sub(marker_width)
            .saturating_sub(fixed)
            .max(NAME_HEADER.len())
    }
}

/// Largura da coluna Origem: o maior rótulo, ou `0` sem grupos do pacman.
pub(crate) fn origin_width(origins: &HashMap<GroupId, String>) -> usize {
    let Some(widest) = origins.values().map(|label| label.chars().count()).max() else {
        return 0;
    };
    widest.max(ORIGIN_HEADER.len())
}

/// Célula de Origem com o separador da coluna seguinte, ou vazio se escondida.
pub(crate) fn origin_cell(origins: &HashMap<GroupId, String>, id: GroupId, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let label = origins.get(&id).map(String::as_str).unwrap_or("");
    format!("{label:<width$}   ")
}

pub(crate) fn kernel_label(
    kernel_labels: &HashMap<GroupId, String>,
    group_id: GroupId,
) -> &str {
    kernel_labels.get(&group_id).map(String::as_str).unwrap_or("?")
}

pub(crate) fn name_cell(group: &Group, width: usize) -> String {
    let description = group::description(group);
    if description.chars().count() > width {
        let cut: String = description.chars().take(width.saturating_sub(1)).collect();
        return format!("{cut}…");
    }
    format!("{description:<width$}")
}

pub(crate) fn picker_row(
    group: &Group,
    kernel_labels: &HashMap<GroupId, String>,
    origins: &HashMap<GroupId, String>,
    columns: &CheckpointColumns,
) -> String {
    let name = name_cell(group, columns.name);
    let origin = origin_cell(origins, group.id, columns.origin);
    let kernel = kernel_label(kernel_labels, group.id);
    format!(
        "{name}   {origin}{kernel:<kernel_col$}   {:<DATE_WIDTH$}   {:<MEMBERS_WIDTH$}   {:<id_col$}",
        short_datetime(group::date(group)),
        group.members.len(),
        format!("#{}", group.id),
        kernel_col = columns.kernel,
        id_col = columns.id,
    )
}

pub(crate) fn picker_header(columns: &CheckpointColumns) -> String {
    let origin = if columns.origin > 0 {
        format!("{ORIGIN_HEADER:<width$}   ", width = columns.origin)
    } else {
        String::new()
    };
    format!(
        "{NAME_HEADER:<name_col$}   {origin}{KERNEL_HEADER:<kernel_col$}   {DATE_HEADER:<DATE_WIDTH$}   \
         {MEMBERS_HEADER:<MEMBERS_WIDTH$}   {ID_HEADER:<id_col$}",
        name_col = columns.name,
        kernel_col = columns.kernel,
        id_col = columns.id,
    )
}

pub(crate) fn picker_prompt(prompt: &str, header: &str, marker_width: usize) -> String {
    let indent = " ".repeat(CONTENT_INDENT.chars().count() + marker_width);
    format!("{prompt}\n{indent}{}", style(header).bold())
}

pub(crate) enum ReviewDecision {
    Proceed,
    Back,
    Cancel,
}

/// Cartão da revisão irreversível: uma linha de título e uma de detalhe.
pub(crate) struct ReviewCard {
    pub(crate) title: String,
    pub(crate) detail: String,
}

pub(crate) fn review_cards(
    cards: &[ReviewCard],
    title: &str,
    prompt: &str,
    hint: &str,
) -> Result<ReviewDecision> {
    let mut yes = false;
    let mut page = 0usize;
    loop {
        let page_size = review_page_size();
        let pages = cards.len().div_ceil(page_size).max(1);
        page = page.min(pages - 1);

        render_review(cards, title, prompt, hint, yes, (page, page_size, pages));
        match console::Term::stdout()
            .read_key()
            .context("aguardar confirmação")?
        {
            console::Key::ArrowUp | console::Key::ArrowDown => yes = !yes,
            console::Key::ArrowLeft => page = page.saturating_sub(1),
            console::Key::ArrowRight => page = (page + 1).min(pages - 1),
            console::Key::Enter => {
                return Ok(if yes {
                    ReviewDecision::Proceed
                } else {
                    ReviewDecision::Cancel
                });
            }
            console::Key::Escape => return Ok(ReviewDecision::Back),
            _ => {}
        }
    }
}

fn render_review(
    cards: &[ReviewCard],
    title: &str,
    prompt: &str,
    hint: &str,
    yes: bool,
    pagination: (usize, usize, usize),
) {
    let (page, page_size, pages) = pagination;
    clear_screen();
    header(title);
    println!();
    line(format_args!("{} {}", style(prompt).bold(), style(format!("({hint})")).dim()));
    line(format_args!("{} Sim", if yes { ">" } else { " " }));
    line(format_args!("{} Não", if yes { " " } else { ">" }));
    println!();

    let start = page * page_size;
    let end = (start + page_size).min(cards.len());
    for card in &cards[start..end] {
        print_review_card(card);
    }
    if pages > 1 {
        line(format_args!(
            "{}",
            style(format!("página {}/{} · ←/→ navega", page + 1, pages)).dim()
        ));
    }
}

fn review_page_size() -> usize {
    let rows = console::Term::stdout().size().0 as usize;
    rows.saturating_sub(8).max(3) / 3
}

fn print_review_card(card: &ReviewCard) {
    let title = truncate_chars(&card.title, content_width().min(80));
    let detail = truncate_chars(&card.detail, content_width().saturating_sub(3));
    println!("{CONTENT_INDENT}{title}");
    println!("{}{}", CONTENT_INDENT, style(format!("└─ {detail}")).dim());
}

fn truncate_chars(value: &str, maximum: usize) -> String {
    if value.chars().count() <= maximum {
        return value.to_string();
    }
    format!(
        "{}…",
        value.chars().take(maximum.saturating_sub(1)).collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::{
        CheckpointColumns, kernel_label, name_cell, picker_header, picker_prompt, picker_row,
    };
    use crate::group::{Group, Member};
    use crate::snapper::Snapshot;
    use std::collections::HashMap;

    fn group(id: i64, description: &str) -> Group {
        Group {
            id,
            members: vec![Member {
                config: "root".to_string(),
                snapshot: Snapshot {
                    number: 1,
                    kind: "single".to_string(),
                    date: "2026-07-10 20:50:00".to_string(),
                    user: "root".to_string(),
                    description: description.to_string(),
                    cleanup: String::new(),
                    userdata: None,
                },
            }],
        }
    }

    #[test]
    fn kernel_label_falls_back_to_question_mark() {
        assert_eq!(kernel_label(&HashMap::new(), 7), "?");
    }

    #[test]
    fn columns_respect_limits_and_kernel_width() {
        let groups = vec![group(7, "nome muito comprido")];
        let labels = HashMap::from([(7, "7.0.12-1-cachyos".to_string())]);
        let columns = CheckpointColumns::new(&groups, &labels, &HashMap::new(), 4, 10, 6);

        assert_eq!(columns.name, 10);
        assert_eq!(columns.kernel, 16);
        assert_eq!(name_cell(&groups[0], columns.name), "nome muit…");
    }

    #[test]
    fn picker_row_keeps_common_field_order() {
        let group = group(7, "Teste");
        let labels = HashMap::from([(7, "7.0.12-1-cachyos".to_string())]);
        let columns =
            CheckpointColumns::new(std::slice::from_ref(&group), &labels, &HashMap::new(), 4, 36, 6);

        assert_eq!(
            picker_row(&group, &labels, &HashMap::new(), &columns),
            "Teste   7.0.12-1-cachyos   2026-07-10 20:50   1         #7"
        );
    }

    #[test]
    fn picker_header_aligns_with_row_and_marker() {
        let group = group(7, "Teste");
        let labels = HashMap::from([(7, "7.0.12-1-cachyos".to_string())]);
        let columns =
            CheckpointColumns::new(std::slice::from_ref(&group), &labels, &HashMap::new(), 4, 36, 6);
        let header = picker_header(&columns);
        let row = picker_row(&group, &labels, &HashMap::new(), &columns);

        assert_eq!(header.find("Kernel"), row.find("7.0.12-1-cachyos"));
        assert_eq!(header.find("Data"), row.find("2026-07-10 20:50"));
        assert_eq!(header.find("Membros"), row.find("1         #7"));
        assert_eq!(header.find("ID"), row.find("#7"));
        assert!(picker_prompt("Escolha", &header, 6).contains("\n         Nome"));
    }

    #[test]
    fn name_column_uses_only_the_terminal_space_left_by_fixed_fields() {
        let groups = vec![group(7, "um nome de checkpoint deliberadamente muito comprido")];
        let labels = HashMap::from([(7, "7.0.12-1-cachyos".to_string())]);

        let mut narrow = CheckpointColumns::new(&groups, &labels, &HashMap::new(), 4, 36, 6);
        narrow.fit_name_to_width(80, 6);
        assert_eq!(narrow.name, 18);
        assert_eq!(picker_header(&narrow).len() + 9, 80);

        let mut medium = CheckpointColumns::new(&groups, &labels, &HashMap::new(), 4, 36, 6);
        medium.fit_name_to_width(100, 6);
        assert_eq!(medium.name, 36);

        let mut wide = CheckpointColumns::new(&groups, &labels, &HashMap::new(), 4, 36, 6);
        wide.fit_name_to_width(120, 6);
        assert_eq!(wide.name, 36);
    }

    #[test]
    fn origin_column_sits_after_the_name_and_hides_when_the_name_would_starve() {
        let group = group(7, "pacman -Syu");
        let labels = HashMap::from([(7, "7.0.12-1-cachyos".to_string())]);
        let origins = HashMap::from([(7, "pacman ↑14".to_string())]);

        let mut wide = CheckpointColumns::new(std::slice::from_ref(&group), &labels, &origins, 4, 36, 6);
        wide.fit_name_to_width(120, 2);
        assert_eq!(wide.origin, "pacman ↑14".chars().count());
        let header = picker_header(&wide);
        let row = picker_row(&group, &labels, &origins, &wide);
        assert_eq!(header.find("Origem"), row.find("pacman ↑14"));
        assert_eq!(
            header.chars().position(|c| c == 'K'),
            row.chars().position(|c| c == '7')
        );

        // Em 80 colunas o Nome ficaria com menos de 10: a Origem sai.
        let mut narrow = CheckpointColumns::new(std::slice::from_ref(&group), &labels, &origins, 4, 36, 6);
        narrow.fit_name_to_width(80, 2);
        assert_eq!(narrow.origin, 0);
        assert!(!picker_header(&narrow).contains("Origem"));
    }

    #[test]
    fn origin_column_needs_a_pacman_group() {
        let groups = vec![group(7, "Teste")];
        let columns = CheckpointColumns::new(&groups, &HashMap::new(), &HashMap::new(), 4, 36, 6);
        assert_eq!(columns.origin, 0);
    }
}
