//! The interactive release-review screen for `version` — a real full-screen TUI (raw mode +
//! alternate screen + event loop), replacing the old static boxed text + line confirm.
//!
//! [`review_lines`] is the pure, testable styling of the plan; [`run`] drives the event loop. The
//! `version` flow reaches this only through the `Prompt::confirm` trait, so tests use a fake and
//! never enter raw mode.

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::summary::Plan;
use crate::ui::ACCENT_RGB as ACCENT;

/// Show the review full-screen and return whether the user confirmed (create the release PR).
pub fn run(plan: &Plan, diff_stat: &str, skip_pr: bool) -> Result<bool> {
    let lines = review_lines(plan, diff_stat, skip_pr);
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
pub fn review_lines(plan: &Plan, diff_stat: &str, skip_pr: bool) -> Vec<Line<'static>> {
    let dim = Style::new().fg(Color::DarkGray);
    let mut lines = Vec::new();

    lines.push(section("Packages to release"));
    if plan.changes.is_empty() {
        lines.push(Line::styled("  (none)", dim));
    } else {
        for c in &plan.changes {
            let (marker, marker_style) = if c.selected {
                (
                    "  ● ",
                    Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
                )
            } else {
                ("  ○ ", Style::new().fg(Color::Yellow))
            };
            lines.push(Line::from(vec![
                Span::styled(marker, marker_style),
                Span::styled(c.name.clone(), Style::new().add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(c.old_version.clone(), dim),
                Span::styled(" → ", Style::new().fg(ACCENT)),
                Span::styled(
                    c.new_version.clone(),
                    Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
                ),
            ]));
            if !c.note.is_empty() {
                lines.push(Line::styled(format!("      {}", c.note), dim));
            }
        }
    }
    lines.push(Line::raw(""));

    if !plan.range_updates.is_empty() {
        lines.push(section("Internal dependency ranges"));
        for r in &plan.range_updates {
            let consumer_style = if r.consumer_private {
                dim
            } else {
                Style::new()
            };
            let mut spans = vec![
                Span::raw("  "),
                Span::styled(r.consumer.clone(), consumer_style),
                Span::styled(" → ", dim),
                Span::styled(r.dep.clone(), Style::new().fg(ACCENT)),
                Span::raw("  "),
                Span::styled(r.old_range.clone(), dim),
                Span::raw(" ⇒ "),
                Span::styled(r.new_range.clone(), Style::new().fg(Color::Green)),
            ];
            if r.consumer_private {
                spans.push(Span::styled(
                    "  (range only, not published)",
                    Style::new().fg(Color::Yellow),
                ));
            }
            lines.push(Line::from(spans));
        }
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

    lines
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
        ));
        assert!(out.contains("Packages to release"));
        assert!(out.contains("@x/core"));
        assert!(out.contains("1.0.0 → 2.0.0"));
        assert!(out.contains("Internal dependency ranges"));
        assert!(out.contains("(range only, not published)"));
        assert!(out.contains("Changed files"));
        assert!(out.contains("packages/core/package.json"));
    }

    #[test]
    fn skip_pr_adds_warning_and_empty_diff_is_handled() {
        let out = text(&review_lines(&plan(), "   ", true));
        assert!(out.contains("No file changes."));
        assert!(out.contains("gh CLI unavailable"));
    }
}
