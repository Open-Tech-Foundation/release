//! Keep a Changelog parser/rewriter.
//!
//! The hand-written `[Unreleased]` section is the **source of truth** for release notes —
//! never inferred from commits. On apply, `[Unreleased]` is moved to a dated
//! `## [x.y.z] - YYYY-MM-DD` section and a fresh empty `[Unreleased]` is left behind.

use std::path::Path;

use anyhow::Result;

/// The parsed `[Unreleased]` section of a changelog.
#[derive(Debug, Clone, Default)]
pub struct Unreleased {
    /// Raw markdown body between the `## [Unreleased]` heading and the next heading.
    pub body: String,
}

impl Unreleased {
    /// True when the section has no meaningful content (whitespace/comments only).
    pub fn is_empty(&self) -> bool {
        self.body.trim().is_empty()
    }
}

/// Parse the `[Unreleased]` section out of a changelog file.
pub fn parse_unreleased(changelog_path: &Path) -> Result<Unreleased> {
    todo!("locate `## [Unreleased]` and capture up to the next `## ` heading")
}

/// Move `[Unreleased]` into a dated release section for `version`, leaving a fresh empty
/// `[Unreleased]`. Auto-bumped-only packages receive the `_Dependency updates._` stub.
pub fn release_unreleased(
    changelog_path: &Path,
    version: &str,
    date: &str,
    stub_if_empty: bool,
) -> Result<()> {
    todo!("rewrite the changelog in place")
}
