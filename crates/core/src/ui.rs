//! Shared terminal styling. One place so every `inquire` prompt across `init`, `config`, and
//! `version` looks consistent instead of using the library defaults, and so the lines a command
//! prints *between* prompts read as part of the same interface rather than as debug output.
//!
//! Colour comes from [`anstyle`] and goes out through [`anstream`], which strips escape sequences
//! when stdout is not a terminal. A pipe or a CI log therefore gets clean text, not `\x1b[32m`.
//!
//! Colors are named, never RGB: they resolve against whatever palette the terminal is set to, so
//! this stays legible on a light background instead of assuming a dark one.

use anstyle::{AnsiColor, Effects, Style};
use inquire::ui::{Attributes, Color, RenderConfig, StyleSheet, Styled};

/// The accent color used for `inquire` prompts and selections.
pub const ACCENT: Color = Color::LightCyan;

/// The same accent as a `ratatui` color, for the interactive review screen.
pub const ACCENT_RGB: ratatui::style::Color = ratatui::style::Color::LightCyan;

/// How many rows a list prompt shows before it scrolls. `inquire` defaults to 7, which turns an
/// ordinary monorepo's package list into a peephole; 12 fits a typical terminal without pushing
/// the question itself off the top.
pub const PAGE_SIZE: usize = 12;

/// Install the global `inquire` render config. Call once at startup, before any prompt. Safe for
/// non-interactive commands too — it only affects how prompts draw if they run.
#[allow(clippy::field_reassign_with_default)] // RenderConfig has many fields; per-field is clearest
pub fn install_render_config() {
    let dim = StyleSheet::new().with_fg(Color::DarkGrey);

    let mut rc = RenderConfig::default();
    // A prompt in progress is a filled marker, an answered one a check: scrolling back through a
    // long wizard, what is still open and what is settled reads at a glance.
    rc.prompt_prefix = Styled::new("◆").with_fg(ACCENT);
    rc.answered_prompt_prefix = Styled::new("✓").with_fg(Color::LightGreen);
    rc.prompt = StyleSheet::new().with_attr(Attributes::BOLD);

    // Everything that is context rather than content recedes: the help line, the pre-filled
    // default, the placeholder.
    rc.help_message = dim;
    rc.default_value = dim;
    rc.placeholder = dim;

    rc.highlighted_option_prefix = Styled::new("❯").with_fg(ACCENT);
    rc.scroll_up_prefix = Styled::new("↑").with_fg(Color::DarkGrey);
    rc.scroll_down_prefix = Styled::new("↓").with_fg(Color::DarkGrey);
    rc.selected_checkbox = Styled::new("◉").with_fg(Color::LightGreen);
    rc.unselected_checkbox = Styled::new("◯").with_fg(Color::DarkGrey);
    rc.selected_option = Some(
        StyleSheet::new()
            .with_fg(ACCENT)
            .with_attr(Attributes::BOLD),
    );

    rc.answer = StyleSheet::new()
        .with_fg(ACCENT)
        .with_attr(Attributes::BOLD);
    // Esc is "go back", not "you broke something" — the default `<canceled>` reads like a failure.
    rc.canceled_prompt_indicator = Styled::new("back").with_fg(Color::DarkGrey);

    rc.error_message = rc
        .error_message
        .with_prefix(Styled::new("✖").with_fg(Color::LightRed))
        .with_message(StyleSheet::new().with_fg(Color::LightRed));

    inquire::set_global_render_config(rc);
}

/// The palette every command paints with. Exposed as [`anstyle::Style`] values, not as printers,
/// so a command that assembles a report into a `String` (`doctor`) uses exactly the same colours as
/// one that prints a line at a time.
///
/// Anything built with these must go out through `anstream`, which decides at write time whether
/// the terminal gets the escape sequences or the pipe gets plain text.
pub const OK: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::BrightGreen)));
pub const INFO: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::BrightCyan)));
pub const WARN: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::BrightYellow)));
pub const DANGER: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::BrightRed)));
pub const DIM: Style = Style::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::BrightBlack)));
pub const BOLD: Style = Style::new().effects(Effects::BOLD);

/// The marker each severity carries, so `doctor`'s report and a command's status lines agree about
/// what a check, a bang and a cross mean.
pub const OK_MARK: &str = "✓";
pub const INFO_MARK: &str = "›";
pub const WARN_MARK: &str = "!";
pub const DANGER_MARK: &str = "✖";

/// Wrap `text` in `style`, for output assembled into a `String` before printing.
pub fn paint(style: Style, text: &str) -> String {
    format!("{}{text}{}", style.render(), style.render_reset())
}

fn line(style: Style, marker: &str, message: &str) {
    anstream::println!("{} {message}", paint(style, marker));
}

/// Something was written or completed.
pub fn ok(message: &str) {
    line(OK, OK_MARK, message);
}

/// A fact worth stating that is not a result — what a prompt found, what a list contains.
pub fn info(message: &str) {
    line(INFO, INFO_MARK, message);
}

/// Something the user should act on, but which is not an error.
pub fn warn(message: &str) {
    line(WARN, WARN_MARK, message);
}

/// Something failed or will fail. Goes to stdout with the surrounding narrative; a command that
/// aborts reports through `anyhow` and the CLI's error printer instead.
pub fn danger(message: &str) {
    line(DANGER, DANGER_MARK, message);
}

/// A step about to run, or one that just did: the running commentary of a long operation.
pub fn step(message: &str) {
    anstream::println!("{} {message}", paint(DIM, "·"));
}

/// A continuation line under a status line, indented to line up under the message.
pub fn detail(message: &str) {
    anstream::println!("  {}", paint(DIM, message));
}

/// A section break: a blank line, then the title in bold.
pub fn heading(title: &str) {
    anstream::println!("\n{}", paint(BOLD, title));
}
