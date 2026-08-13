//! Confirmation / dry-run rendering shown before any files are written.
//!
//! Renders the three blocks from `docs/commands/version.md`: packages to publish
//! (selected), auto-bumped dependents, and internal range updates (including private
//! apps, which are flagged "range updated, NOT published").

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Cell, Padding, Row, Table, Widget};

use crate::adapter::DepKind;

/// A version change for one publishable package.
#[derive(Debug, Clone)]
pub struct VersionChange {
    pub name: String,
    pub old_version: String,
    pub new_version: String,
    /// True when the user explicitly selected this package; false when reached via cascade.
    pub selected: bool,
    /// The parenthetical reason, e.g. `major, selected` or `patch — depends on @x/core`.
    pub note: String,
    /// The git tag this release will create, formatted through the package's `tag_format`.
    ///
    /// Shown in the review because `tag_format` is the setting with the widest blast radius and
    /// the least visible effect: it decides what `last_tag` reads back, and two packages whose
    /// format has no `{name}` collide on one tag and silently release only the first. Reviewing a
    /// release without seeing its tags means the one thing you cannot check is the one that fails
    /// quietly.
    pub tag: String,
}

/// An internal dependency-range update on one consumer.
#[derive(Debug, Clone)]
pub struct RangeUpdate {
    pub consumer: String,
    pub dep: String,
    pub kind: DepKind,
    pub old_range: String,
    pub new_range: String,
    /// True for private apps: the range is updated but the package is never published.
    pub consumer_private: bool,
}

/// The full plan for a `version` run.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub changes: Vec<VersionChange>,
    pub range_updates: Vec<RangeUpdate>,
    /// Preflight conditions that do not block the release but should change the answer.
    ///
    /// These travel *in the plan* rather than being printed before it. The review is a full-screen
    /// TUI: anything written to stdout beforehand is hidden the moment the alternate screen opens,
    /// so a warning printed there was invisible at exactly the point it had to be read.
    pub warnings: Vec<Warning>,
}

/// A non-blocking condition attached to one package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub package: String,
    pub message: String,
}

/// Render the human-readable confirmation summary (no trailing prompt).
pub fn render(plan: &Plan) -> String {
    let mut out = String::new();

    if !plan.changes.is_empty() {
        let rows: Vec<Vec<String>> = plan
            .changes
            .iter()
            .map(|c| {
                vec![
                    if c.selected {
                        "● direct".to_string()
                    } else {
                        "↳ cascade".to_string()
                    },
                    c.name.clone(),
                    c.old_version.clone(),
                    "→".to_string(),
                    c.new_version.clone(),
                    c.tag.clone(),
                    c.note.clone(),
                ]
            })
            .collect();

        out.push('\n');
        render_table(
            &mut out,
            "Version Bumps",
            &["Kind", "Package", "Old", "", "New", "Tag", "Reason"],
            &rows,
        );
        out.push('\n');
    }

    if !plan.warnings.is_empty() {
        out.push_str("\nWarnings\n");
        for warning in &plan.warnings {
            out.push_str(&format!("  {}: {}\n", warning.package, warning.message));
        }
    }

    if !plan.range_updates.is_empty() {
        let rows: Vec<Vec<String>> = plan
            .range_updates
            .iter()
            .map(|r| {
                vec![
                    r.consumer.clone(),
                    dep_section(&r.kind).to_string(),
                    r.dep.clone(),
                    r.old_range.clone(),
                    "→".to_string(),
                    r.new_range.clone(),
                    match (r.new_range == r.old_range, r.consumer_private) {
                        (true, _) => "pinned spec, left unchanged".to_string(),
                        (false, true) => "private app, not published".to_string(),
                        (false, false) => "published package".to_string(),
                    },
                ]
            })
            .collect();

        render_table(
            &mut out,
            "Internal Range Updates",
            &[
                "Consumer",
                "Section",
                "Dependency",
                "Old",
                "",
                "New",
                "Notes",
            ],
            &rows,
        );
        out.push('\n');
    }

    out
}

pub fn dep_section(kind: &DepKind) -> &'static str {
    match kind {
        DepKind::Dep => "dependencies",
        DepKind::PeerDep => "peerDependencies",
        DepKind::DevDep => "devDependencies",
    }
}

fn render_table(out: &mut String, title: &str, headers: &[&str], rows: &[Vec<String>]) {
    let widths = column_widths(headers, rows);
    let constraints: Vec<Constraint> = widths
        .iter()
        .map(|width| Constraint::Length(*width as u16))
        .collect();
    let table_rows = rows
        .iter()
        .map(|row| Row::new(row.iter().map(|cell| Cell::from(cell.clone()))).bottom_margin(1));
    let header = Row::new(headers.iter().copied().map(Cell::from))
        .style(Style::new().add_modifier(Modifier::BOLD))
        .bottom_margin(1);
    let table = Table::new(table_rows, constraints)
        .header(header)
        .block(
            Block::bordered()
                .title(title)
                .padding(Padding::new(2, 2, 1, 1)),
        )
        .column_spacing(4);
    let width = table_width(&widths, headers.len());
    let height = table_height(rows.len());
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    table.render(area, &mut buffer);
    out.push_str(&buffer_to_string(&buffer));
}

fn column_widths(headers: &[&str], rows: &[Vec<String>]) -> Vec<usize> {
    headers
        .iter()
        .enumerate()
        .map(|(idx, header)| {
            rows.iter()
                .filter_map(|row| row.get(idx))
                .map(|cell| cell.chars().count())
                .max()
                .unwrap_or(0)
                .max(header.chars().count())
        })
        .collect()
}

fn table_width(widths: &[usize], columns: usize) -> u16 {
    let content_width = widths.iter().sum::<usize>();
    let spacing_width = columns.saturating_sub(1) * 4;
    let border_width = 2;
    let horizontal_padding = 4;
    (content_width + spacing_width + border_width + horizontal_padding) as u16
}

fn table_height(row_count: usize) -> u16 {
    let border_height = 2;
    let vertical_padding = 2;
    let header_height = 2;
    let row_height = row_count * 2;
    (border_height + vertical_padding + header_height + row_height) as u16
}

fn buffer_to_string(buffer: &Buffer) -> String {
    let area = *buffer.area();
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        let mut line = String::new();
        for x in area.left()..area.right() {
            line.push_str(buffer[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_all_three_blocks() {
        let plan = Plan {
            changes: vec![
                VersionChange {
                    name: "@opentf/core".into(),
                    old_version: "1.2.0".into(),
                    new_version: "2.0.0".into(),
                    selected: true,
                    note: "major, selected".into(),
                    tag: "@opentf/core@2.0.0".into(),
                },
                VersionChange {
                    name: "@opentf/sdk".into(),
                    old_version: "1.0.0".into(),
                    new_version: "2.0.0".into(),
                    selected: false,
                    note: "mirror major — peerDep on @opentf/core".into(),
                    tag: "@opentf/sdk@2.0.0".into(),
                },
            ],
            warnings: Vec::new(),
            range_updates: vec![RangeUpdate {
                consumer: "playground".into(),
                dep: "@opentf/core".into(),
                kind: DepKind::Dep,
                old_range: "^1.2.0".into(),
                new_range: "^2.0.0".into(),
                consumer_private: true,
            }],
        };

        let out = render(&plan);
        assert!(out.contains("Version Bumps"));
        assert!(out.contains("● direct"));
        assert!(out.contains("@opentf/core"));
        assert!(out.contains("1.2.0"));
        assert!(out.contains("2.0.0"));
        assert!(out.contains("major, selected"));
        assert!(out.contains("↳ cascade"));
        assert!(out.contains("mirror major — peerDep on @opentf/core"));
        assert!(out.contains("Internal Range Updates"));
        assert!(out.contains("private app, not published"));
    }

    #[test]
    fn omits_empty_blocks() {
        let plan = Plan {
            changes: vec![VersionChange {
                name: "a".into(),
                old_version: "1.0.0".into(),
                new_version: "1.0.1".into(),
                selected: true,
                note: "patch, selected".into(),
                tag: "v1.0.1".into(),
            }],
            range_updates: vec![],
            warnings: Vec::new(),
        };
        let out = render(&plan);
        assert!(out.contains("Version Bumps"));
        assert!(!out.contains("Internal Range Updates"));
    }
}
