//! The `version` command — interactive, run **locally**.
//!
//! Produces a release PR; never publishes, never writes to `main`. See
//! `docs/commands/version.md` for the full step sequence.

use anyhow::Result;

use crate::adapter::Adapter;

/// Options for a `version` run (wired up by the CLI crate).
#[derive(Debug, Clone, Default)]
pub struct VersionOptions {
    /// Compute and print the plan, but write nothing.
    pub dry_run: bool,
    /// Allow first-release of packages that have no prior tag.
    pub first_release: bool,
}

/// Run the interactive version flow:
/// discover -> preflight -> prompt -> cascade -> summary/confirm -> branch -> apply ->
/// lockfile -> commit -> push -> open PR.
pub fn run(adapter: &dyn Adapter, opts: &VersionOptions) -> Result<()> {
    todo!("orchestrate the version flow (see docs/commands/version.md)")
}
