//! Strict preflight gate — all-or-nothing, runs before any prompt or mutation.
//!
//! For every non-private package, state is derived from its last git tag `name@x.y.z`.
//! A single violation collects *all* violations, prints them, and exits non-zero before
//! any `release/*` branch is created or any file is written.

use anyhow::Result;

use crate::adapter::Pkg;

/// A single preflight failure, e.g. "3 commits since core@1.2.0 but [Unreleased] is empty".
#[derive(Debug, Clone)]
pub struct Violation {
    pub package: String,
    pub message: String,
}

/// Run the gate. `selected` is the set of packages the user chose to bump (may be empty when
/// preflight runs before the prompt; the selection check is layered in by `version`).
pub fn check(packages: &[Pkg], selected: &[String]) -> Result<Vec<Violation>> {
    // Rules (see docs/preflight.md):
    //  - commits since last tag (scoped to pkg path) but [Unreleased] empty/missing -> violation
    //  - selected for bump but [Unreleased] empty -> violation
    //  - no last tag + publishable -> first-release requires [Unreleased]
    //  - private packages: commits allowed, no changelog demanded
    todo!("derive per-package state from git tags and changelog, collect all violations")
}
