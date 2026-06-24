//! Confirmation / dry-run rendering shown before any files are written.
//!
//! Renders the three blocks from `docs/commands/version.md`: packages to publish
//! (selected), auto-bumped dependents, and internal range updates (including private
//! apps, which are flagged "range updated, NOT published").

use crate::adapter::{Bump, Pkg};

/// A single planned change for one package.
#[derive(Debug, Clone)]
pub struct PlannedChange {
    pub name: String,
    pub old_version: String,
    pub new_version: String,
    pub bump: Bump,
    /// True when the user explicitly selected this package; false when reached via cascade.
    pub selected: bool,
    /// True for private apps: ranges are updated but the package is never published.
    pub private: bool,
}

/// The full plan for a `version` run.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub changes: Vec<PlannedChange>,
}

/// Render the human-readable confirmation summary.
pub fn render(plan: &Plan, packages: &[Pkg]) -> String {
    todo!("format the 'Packages to publish / Auto-bumped / Internal range updates' blocks")
}
