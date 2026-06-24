//! The `init` command — interactive `release.yml` generator.
//!
//! Generates exactly one `.github/workflows/release.yml`. There is **no persisted config** —
//! the generated YAML is the single source of truth. See `docs/commands/init.md`.

use anyhow::Result;

use crate::adapter::Adapter;

/// Options for an `init` run.
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    /// Overwrite an existing `release.yml` without prompting.
    pub force: bool,
}

/// Run the interactive workflow generator:
/// detect ecosystems -> ask which publishable packages need binary artifacts ->
/// prompt target triples -> emit `release.yml` (build-matrix + publish jobs).
pub fn run(adapter: &dyn Adapter, opts: &InitOptions) -> Result<()> {
    todo!("generate .github/workflows/release.yml (see docs/commands/init.md)")
}
