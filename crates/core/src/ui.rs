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

fn styled(style: Style, marker: &str, message: &str) {
    anstream::println!(
        "{}{marker}{} {message}",
        style.render(),
        style.render_reset()
    );
}

/// Something was written or completed.
pub fn ok(message: &str) {
    styled(
        Style::new().fg_color(Some(AnsiColor::BrightGreen.into())),
        "✓",
        message,
    );
}

/// A fact worth stating that is not a result — what a prompt found, what a list contains.
pub fn info(message: &str) {
    styled(
        Style::new().fg_color(Some(AnsiColor::BrightCyan.into())),
        "›",
        message,
    );
}

/// Something the user should act on, but which is not an error.
pub fn warn(message: &str) {
    styled(
        Style::new().fg_color(Some(AnsiColor::BrightYellow.into())),
        "!",
        message,
    );
}

/// A continuation line under [`info`]/[`ok`], indented to line up under the message.
pub fn detail(message: &str) {
    let dim = Style::new().fg_color(Some(AnsiColor::BrightBlack.into()));
    anstream::println!("  {}{message}{}", dim.render(), dim.render_reset());
}

/// A section break: a blank line, then the title in bold.
pub fn heading(title: &str) {
    let bold = Style::new().effects(Effects::BOLD);
    anstream::println!("\n{}{title}{}", bold.render(), bold.render_reset());
}
