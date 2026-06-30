//! Shared terminal styling. One place so every `inquire` prompt across `init`, `config`, and
//! `version` looks consistent instead of using the library defaults.

use inquire::ui::{Attributes, Color, RenderConfig, StyleSheet, Styled};

/// The accent color used for `inquire` prompts and selections.
pub const ACCENT: Color = Color::LightCyan;

/// The same accent as a `ratatui` color, for the interactive review screen.
pub const ACCENT_RGB: ratatui::style::Color = ratatui::style::Color::LightCyan;

/// Install the global `inquire` render config. Call once at startup, before any prompt. Safe for
/// non-interactive commands too — it only affects how prompts draw if they run.
#[allow(clippy::field_reassign_with_default)] // RenderConfig has many fields; per-field is clearest
pub fn install_render_config() {
    let mut rc = RenderConfig::default();
    rc.prompt_prefix = Styled::new("?").with_fg(ACCENT);
    rc.answered_prompt_prefix = Styled::new("✓").with_fg(Color::LightGreen);
    rc.highlighted_option_prefix = Styled::new("›").with_fg(ACCENT);
    rc.selected_option = Some(
        StyleSheet::new()
            .with_fg(ACCENT)
            .with_attr(Attributes::BOLD),
    );
    rc.selected_checkbox = Styled::new("◉").with_fg(Color::LightGreen);
    rc.unselected_checkbox = Styled::new("◯").with_fg(Color::DarkGrey);
    rc.help_message = StyleSheet::new().with_fg(Color::DarkGrey);
    rc.answer = StyleSheet::new()
        .with_fg(ACCENT)
        .with_attr(Attributes::BOLD);
    inquire::set_global_render_config(rc);
}
