//! The `publish` command — non-interactive, run in **CI**, stateless.
//!
//! Publishes changed packages in dependency order, idempotent and resumable, halting on the
//! first failure (publishing is irreversible — forward-resume only). See
//! `docs/commands/publish.md`.

use std::path::PathBuf;

use anyhow::Result;

use crate::adapter::Adapter;

/// Options for a `publish` run.
#[derive(Debug, Clone, Default)]
pub struct PublishOptions {
    /// Root of the staged-artifact tree (`.artifacts/`), if the workflow staged binaries.
    pub artifacts_dir: Option<PathBuf>,
    /// Resolve the plan and print it, but do not publish or push tags.
    pub dry_run: bool,
}

/// Run the publish flow:
/// discover -> filter (publishable & not already published) -> topo sort ->
/// for each: resolve links -> publish -> tag/release. Halt on first failure.
pub fn run(adapter: &dyn Adapter, opts: &PublishOptions) -> Result<()> {
    todo!("orchestrate the publish flow (see docs/commands/publish.md)")
}
