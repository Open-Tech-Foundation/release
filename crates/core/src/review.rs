//! The interactive release-review screen for `version` — a real full-screen TUI (raw mode +
//! alternate screen + event loop), replacing the old static boxed text + line confirm.
//!
//! [`review_lines`] is the pure, testable styling of the plan; [`run`] drives the event loop. The
//! `version` flow reaches this only through the `Prompt::confirm` trait, so tests use a fake and
//! never enter raw mode.

use std::collections::BTreeMap;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::summary::{dep_section, Plan, RangeUpdate, VersionChange};
use crate::ui::ACCENT_RGB as ACCENT;

/// Show the review full-screen and return whether the user confirmed (create the release PR).
pub fn run(
    plan: &Plan,
    diff_stat: &str,
    skip_pr: bool,
    release_branch: &str,
    commit_title: &str,
) -> Result<bool> {
    let lines = review_lines(plan, diff_stat, skip_pr, release_branch, commit_title);
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &lines);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, lines: &[Line<'static>]) -> Result<bool> {
    let mut scroll: u16 = 0;
    loop {
        terminal.draw(|f| draw(f, lines, scroll))?;
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => return Ok(true),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') | KeyCode::Esc => {
                    return Ok(false)
                }
                KeyCode::Down | KeyCode::Char('j') => scroll = scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                KeyCode::PageDown | KeyCode::Char(' ') => scroll = scroll.saturating_add(10),
                KeyCode::PageUp => scroll = scroll.saturating_sub(10),
                KeyCode::Home => scroll = 0,
                _ => {}
            }
        }
    }
}

fn draw(f: &mut Frame, lines: &[Line<'static>], scroll: u16) {
    let [body, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(f.area());

    let title = Span::styled(
        " Release Review ",
        Style::new()
            .fg(Color::Black)
            .bg(ACCENT)
            .add_modifier(Modifier::BOLD),
    );
    let body_widget = Paragraph::new(lines.to_vec())
        .block(
            Block::bordered()
                .title(title)
                .border_style(Style::new().fg(ACCENT)),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(body_widget, body);

    let footer_line = Line::from(vec![
        key_hint("Yes", "y/enter", Color::Green),
        Span::styled("  |  ", Style::new().fg(Color::DarkGray)),
        key_hint("No", "n/esc", Color::Red),
        Span::raw("    "),
        key_hint("↑↓ jk", "scroll", ACCENT),
    ]);
    let footer_widget = Paragraph::new(footer_line)
        .block(Block::bordered().border_style(Style::new().fg(Color::DarkGray)))
        .centered();
    f.render_widget(footer_widget, footer);
}

fn key_hint(keys: &'static str, desc: &'static str, color: Color) -> Span<'static> {
    Span::styled(
        format!("[{keys}] {desc}"),
        Style::new().fg(color).add_modifier(Modifier::BOLD),
    )
}

/// Build the styled body of the review screen. Pure: no terminal, fully testable.
pub fn review_lines(
    plan: &Plan,
    diff_stat: &str,
    skip_pr: bool,
    release_branch: &str,
    commit_title: &str,
) -> Vec<Line<'static>> {
    let dim = Style::new().fg(Color::DarkGray);
    let mut lines = Vec::new();

    lines.push(section("Packages"));
    let selected: Vec<&VersionChange> = plan.changes.iter().filter(|c| c.selected).collect();
    let automatic: Vec<&VersionChange> = plan.changes.iter().filter(|c| !c.selected).collect();
    push_package_group(&mut lines, "Selected by you", &selected, false);
    push_package_group(&mut lines, "Added by dependency rules", &automatic, true);
    lines.push(Line::raw(""));

    if !plan.range_updates.is_empty() {
        lines.push(section("Dependency Range Updates"));
        push_range_updates(&mut lines, &plan.range_updates);
        lines.push(Line::raw(""));
    }

    lines.push(section("Changed files"));
    if diff_stat.trim().is_empty() {
        lines.push(Line::styled("  No file changes.", dim));
    } else {
        for l in diff_stat.trim_end().lines() {
            lines.push(Line::raw(format!("  {l}")));
        }
    }

    if skip_pr {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "⚠  gh CLI unavailable — the PR will be skipped after push.",
            Style::new().fg(Color::Yellow),
        ));
    }

    lines.push(Line::raw(""));
    lines.push(section("After Confirm"));
    lines.push(Line::from(vec![
        Span::raw("  Branch: "),
        Span::styled(release_branch.to_string(), Style::new().fg(ACCENT)),
    ]));
    lines.push(Line::from(vec![
        Span::raw("  Commit: "),
        Span::styled(commit_title.to_string(), Style::new().fg(Color::Green)),
    ]));
    lines.push(Line::styled(
        "  Publish: not now; publishing happens after the PR is merged and CI runs.",
        dim,
    ));

    lines
}

fn push_package_group(
    lines: &mut Vec<Line<'static>>,
    title: &'static str,
    changes: &[&VersionChange],
    show_reason: bool,
) {
    let dim = Style::new().fg(Color::DarkGray);
    lines.push(Line::styled(
        format!("  {title}"),
        Style::new().add_modifier(Modifier::BOLD),
    ));
    if changes.is_empty() {
        lines.push(Line::styled("    (none)", dim));
        return;
    }
    let name_width = changes
        .iter()
        .map(|c| c.name.chars().count())
        .max()
        .unwrap_or(0);
    for c in changes {
        let version = format!("{} → {}", c.old_version, c.new_version);
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(
                format!("{:<name_width$}", c.name),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(version, Style::new().fg(Color::Green)),
            Span::raw("  "),
            Span::styled(bump_label(&c.note), dim),
        ]));
        if show_reason {
            lines.push(Line::styled(
                format!("      Reason: {}", friendly_reason(&c.note)),
                dim,
            ));
        }
    }
}

fn push_range_updates(lines: &mut Vec<Line<'static>>, updates: &[RangeUpdate]) {
    let mut by_consumer: BTreeMap<&str, Vec<&RangeUpdate>> = BTreeMap::new();
    for update in updates {
        by_consumer
            .entry(update.consumer.as_str())
            .or_default()
            .push(update);
    }

    for (consumer, updates) in by_consumer {
        let private = updates.iter().any(|r| r.consumer_private);
        let suffix = if private {
            "  (range only, not published)"
        } else {
            ""
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                consumer.to_string(),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::styled(suffix, Style::new().fg(Color::Yellow)),
        ]));

        let mut by_section: BTreeMap<&str, Vec<&RangeUpdate>> = BTreeMap::new();
        for update in updates {
            by_section
                .entry(dep_section(&update.kind))
                .or_default()
                .push(update);
        }

        for (section, updates) in by_section {
            lines.push(Line::styled(
                format!("    {section}"),
                Style::new().fg(Color::DarkGray),
            ));
            let dep_width = updates
                .iter()
                .map(|r| r.dep.chars().count())
                .max()
                .unwrap_or(0);
            for update in updates {
                lines.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled(
                        format!("{:<dep_width$}", update.dep),
                        Style::new().fg(ACCENT),
                    ),
                    Span::raw("  "),
                    Span::styled(update.old_range.clone(), Style::new().fg(Color::DarkGray)),
                    Span::raw(" → "),
                    Span::styled(update.new_range.clone(), Style::new().fg(Color::Green)),
                ]));
            }
        }
    }
}

fn bump_label(note: &str) -> String {
    note.split([',', '—'])
        .next()
        .unwrap_or(note)
        .trim()
        .replace("mirror ", "")
}

fn friendly_reason(note: &str) -> String {
    if let Some(dep) = note.strip_prefix("mirror ") {
        if let Some((bump, dep)) = dep.split_once(" — peerDep on ") {
            return format!("peer dependency {dep} was bumped; mirroring {bump}");
        }
    }
    if let Some((bump, dep)) = note.split_once(" — depends on ") {
        return format!("dependency {dep} was bumped; applying {bump}");
    }
    note.to_string()
}

fn section(title: &'static str) -> Line<'static> {
    Line::styled(
        title,
        Style::new()
            .fg(ACCENT)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::DepKind;
    use crate::summary::{RangeUpdate, VersionChange};

    fn plan() -> Plan {
        Plan {
            changes: vec![
                VersionChange {
                    name: "@x/core".into(),
                    old_version: "1.0.0".into(),
                    new_version: "2.0.0".into(),
                    selected: true,
                    note: "major, selected".into(),
                },
                VersionChange {
                    name: "@x/sdk".into(),
                    old_version: "1.0.0".into(),
                    new_version: "1.0.1".into(),
                    selected: false,
                    note: "patch — depends on @x/core".into(),
                },
            ],
            range_updates: vec![RangeUpdate {
                consumer: "@x/app".into(),
                dep: "@x/core".into(),
                kind: DepKind::PeerDep,
                old_range: "^1.0.0".into(),
                new_range: "^2.0.0".into(),
                consumer_private: true,
            }],
        }
    }

    /// Flatten the styled lines back to plain text for content assertions.
    fn text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_all_sections_and_the_version_transition() {
        let out = text(&review_lines(
            &plan(),
            " packages/core/package.json | 2 +-\n",
            false,
            "release/2026-07-02",
            "chore(release): @x/core@2.0.0",
        ));
        assert!(out.contains("Packages"));
        assert!(out.contains("Selected by you"));
        assert!(out.contains("Added by dependency rules"));
        assert!(out.contains("@x/core"));
        assert!(out.contains("1.0.0 → 2.0.0"));
        assert!(out.contains("Dependency Range Updates"));
        assert!(out.contains("peerDependencies"));
        assert!(out.contains("dependency @x/core was bumped; applying patch"));
        assert!(out.contains("(range only, not published)"));
        assert!(out.contains("Changed files"));
        assert!(out.contains("packages/core/package.json"));
        assert!(out.contains("After Confirm"));
        assert!(out.contains("release/2026-07-02"));
        assert!(out.contains("chore(release): @x/core@2.0.0"));
    }

    #[test]
    fn skip_pr_adds_warning_and_empty_diff_is_handled() {
        let out = text(&review_lines(
            &plan(),
            "   ",
            true,
            "release/2026-07-02",
            "chore(release): @x/core@2.0.0",
        ));
        assert!(out.contains("No file changes."));
        assert!(out.contains("gh CLI unavailable"));
    }
}
