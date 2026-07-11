use crate::group::{self, Group, GroupId};
use crate::ui::term::{
    CONTENT_INDENT, clear_screen, content_width, header, line, short_datetime,
};
use anyhow::{Context, Result};
use console::style;
use std::collections::HashMap;

pub(crate) const KERNEL_HEADER: &str = "Kernel";
pub(crate) const NAME_HEADER: &str = "Nome";
const DATE_HEADER: &str = "Data";
const MEMBERS_HEADER: &str = "Membros";
const ID_HEADER: &str = "ID";
const DATE_WIDTH: usize = 16;
const MEMBERS_WIDTH: usize = 7;

pub(crate) struct CheckpointColumns {
    pub(crate) name: usize,
    pub(crate) kernel: usize,
    id: usize,
}

#[derive(Clone, Copy)]
pub(crate) enum PickerTail {
    MembersAndId,
    None,
}

impl CheckpointColumns {
    pub(crate) fn new(
        groups: &[Group],
        kernel_labels: &HashMap<GroupId, String>,
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
        Self { name, kernel, id }
    }

    pub(crate) fn fit_to_terminal(
        mut self,
        marker_width: usize,
        status_width: Option<usize>,
        tail: PickerTail,
    ) -> Self {
        let terminal_width = console::Term::stdout().size().1 as usize;
        self.fit_name_to_width(terminal_width, marker_width, status_width, tail);
        self
    }

    fn fit_name_to_width(
        &mut self,
        terminal_width: usize,
        marker_width: usize,
        status_width: Option<usize>,
        tail: PickerTail,
    ) {
        let mut fixed = self.kernel + DATE_WIDTH + 6;
        if let Some(status_width) = status_width {
            fixed += 3 + status_width;
        }
        if matches!(tail, PickerTail::MembersAndId) {
            fixed += 3 + MEMBERS_WIDTH + 3 + self.id;
        }
        let available = terminal_width
            .saturating_sub(CONTENT_INDENT.chars().count())
            .saturating_sub(marker_width)
            .saturating_sub(fixed)
            .max(NAME_HEADER.len());
        self.name = self.name.min(available);
    }
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
    columns: &CheckpointColumns,
    status: Option<(&str, usize)>,
    tail: PickerTail,
) -> String {
    let name = name_cell(group, columns.name);
    let kernel = kernel_label(kernel_labels, group.id);
    let status = status
        .map(|(value, width)| format!("   {value:<width$}"))
        .unwrap_or_default();
    let tail = match tail {
        PickerTail::MembersAndId => format!(
            "   {:<MEMBERS_WIDTH$}   {:<id_col$}",
            group.members.len(),
            format!("#{}", group.id),
            id_col = columns.id,
        ),
        PickerTail::None => String::new(),
    };
    format!(
        "{name}   {kernel:<kernel_col$}   {:<DATE_WIDTH$}{status}{tail}",
        short_datetime(group::date(group)),
        kernel_col = columns.kernel,
    )
}

pub(crate) fn picker_header(
    columns: &CheckpointColumns,
    status: Option<(&str, usize)>,
    tail: PickerTail,
) -> String {
    let status = status
        .map(|(label, width)| format!("   {label:<width$}"))
        .unwrap_or_default();
    let tail = match tail {
        PickerTail::MembersAndId => {
            format!("   {MEMBERS_HEADER:<MEMBERS_WIDTH$}   {ID_HEADER:<id_col$}", id_col = columns.id)
        }
        PickerTail::None => String::new(),
    };
    format!(
        "{NAME_HEADER:<name_col$}   {KERNEL_HEADER:<kernel_col$}   {DATE_HEADER:<DATE_WIDTH$}{status}{tail}",
        name_col = columns.name,
        kernel_col = columns.kernel,
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

pub(crate) fn review_irreversible(
    targets: &[&Group],
    kernel_labels: &HashMap<GroupId, String>,
    title: &str,
    prompt: &str,
    hint: &str,
) -> Result<ReviewDecision> {
    let mut yes = false;
    let mut page = 0usize;
    loop {
        let page_size = review_page_size();
        let pages = targets.len().div_ceil(page_size).max(1);
        page = page.min(pages - 1);

        render_review(
            targets,
            kernel_labels,
            title,
            prompt,
            hint,
            yes,
            (page, page_size, pages),
        );
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
    targets: &[&Group],
    kernel_labels: &HashMap<GroupId, String>,
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
    let end = (start + page_size).min(targets.len());
    for group in &targets[start..end] {
        print_review_card(group, kernel_label(kernel_labels, group.id));
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

fn print_review_card(group: &Group, kernel: &str) {
    let description = group::description(group).trim();
    let description = if description.is_empty() {
        format!("#{}", group.id)
    } else {
        description.to_string()
    };
    let description = truncate_chars(&description, content_width().min(80));
    println!("{CONTENT_INDENT}{description}");
    println!(
        "{}{}",
        CONTENT_INDENT,
        style(format!(
            "└─ {}  ·  kernel {}  ·  {} membros  ·  #{}",
            short_datetime(group::date(group)),
            kernel,
            group.members.len(),
            group.id
        ))
        .dim()
    );
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
        CheckpointColumns, PickerTail, kernel_label, name_cell, picker_header, picker_prompt,
        picker_row,
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
        let columns = CheckpointColumns::new(&groups, &labels, 4, 10, 6);

        assert_eq!(columns.name, 10);
        assert_eq!(columns.kernel, 16);
        assert_eq!(name_cell(&groups[0], columns.name), "nome muit…");
    }

    #[test]
    fn picker_row_keeps_common_field_order_and_optional_status() {
        let group = group(7, "Teste");
        let labels = HashMap::from([(7, "7.0.12-1-cachyos".to_string())]);
        let columns =
            CheckpointColumns::new(std::slice::from_ref(&group), &labels, 4, 36, 6);

        assert_eq!(
            picker_row(
                &group,
                &labels,
                &columns,
                Some(("em ~2d", 6)),
                PickerTail::MembersAndId,
            ),
            "Teste   7.0.12-1-cachyos   2026-07-10 20:50   em ~2d   1         #7"
        );
    }

    #[test]
    fn picker_header_aligns_with_row_and_marker() {
        let group = group(7, "Teste");
        let labels = HashMap::from([(7, "7.0.12-1-cachyos".to_string())]);
        let columns =
            CheckpointColumns::new(std::slice::from_ref(&group), &labels, 4, 36, 6);
        let header = picker_header(&columns, None, PickerTail::MembersAndId);
        let row = picker_row(
            &group,
            &labels,
            &columns,
            None,
            PickerTail::MembersAndId,
        );

        assert_eq!(header.find("Kernel"), row.find("7.0.12-1-cachyos"));
        assert_eq!(header.find("Data"), row.find("2026-07-10 20:50"));
        assert_eq!(header.find("Membros"), row.find("1         #7"));
        assert_eq!(header.find("ID"), row.find("#7"));
        assert!(picker_prompt("Escolha", &header, 6).contains("\n         Nome"));

        let compact_header = picker_header(&columns, Some(("Purga", 7)), PickerTail::None);
        let compact_row = picker_row(
            &group,
            &labels,
            &columns,
            Some(("em ~2d", 7)),
            PickerTail::None,
        );
        assert!(!compact_header.contains("Membros"));
        assert!(!compact_header.contains("ID"));
        assert!(!compact_row.contains("#7"));
    }

    #[test]
    fn name_column_uses_only_the_terminal_space_left_by_fixed_fields() {
        let groups = vec![group(7, "um nome de checkpoint deliberadamente muito comprido")];
        let labels = HashMap::from([(7, "7.0.12-1-cachyos".to_string())]);

        let mut narrow = CheckpointColumns::new(&groups, &labels, 4, 36, 6);
        narrow.fit_name_to_width(80, 6, None, PickerTail::MembersAndId);
        assert_eq!(narrow.name, 18);
        assert_eq!(picker_header(&narrow, None, PickerTail::MembersAndId).len() + 9, 80);

        let mut medium = CheckpointColumns::new(&groups, &labels, 4, 36, 6);
        medium.fit_name_to_width(100, 6, None, PickerTail::MembersAndId);
        assert_eq!(medium.name, 36);

        let mut wide = CheckpointColumns::new(&groups, &labels, 4, 36, 6);
        wide.fit_name_to_width(120, 6, None, PickerTail::MembersAndId);
        assert_eq!(wide.name, 36);

        let mut compact = CheckpointColumns::new(&groups, &labels, 4, 36, 6);
        compact.fit_name_to_width(80, 6, Some(7), PickerTail::None);
        assert_eq!(compact.name, 23);
        assert_eq!(
            picker_header(&compact, Some(("Purga", 7)), PickerTail::None).len() + 9,
            80
        );
    }
}
