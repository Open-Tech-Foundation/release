//! The [`Adapter`] trait and the ecosystem-agnostic domain types the core operates on.
//!
//! Core never reads a `package.json` (or any manifest) directly — every ecosystem-specific
//! decision (range syntax, cascade rule, publish mechanics, registry lookups) lives behind
//! this trait. v1 ships exactly one implementation: the npm adapter in `opentf-release-adapters`.

use std::path::{Path, PathBuf};

use anyhow::Result;

/// A semantic-version bump level.
///
/// Variants are ordered `Patch < Minor < Major` so that `max(...)` over a set of bumps
/// (used when a package is reached by several cascade paths) yields the strongest bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bump {
    Prerelease,
    Patch,
    Minor,
    Major,
}

impl std::fmt::Display for Bump {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Bump::Prerelease => "prerelease",
            Bump::Patch => "patch",
            Bump::Minor => "minor",
            Bump::Major => "major",
        })
    }
}

/// The relationship kind of an internal dependency. The concrete set is adapter-specific;
/// this is the npm-flavored set used in v1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepKind {
    Dep,
    PeerDep,
    DevDep,
}

/// A dependency on another package *within the same monorepo*.
#[derive(Debug, Clone)]
pub struct InternalDep {
    pub name: String,
    pub kind: DepKind,
    /// The currently declared range, in the ecosystem's syntax (e.g. `^1.2.0`).
    pub range: String,
}

/// A discovered workspace package, normalized into ecosystem-agnostic terms.
#[derive(Debug, Clone)]
pub struct Pkg {
    pub name: String,
    pub version: String,
    pub manifest_path: PathBuf,
    pub changelog_path: PathBuf,
    /// `false` => private app: a graph leaf that is never versioned or published,
    /// but whose internal ranges are still kept up to date so it stays buildable.
    pub publishable: bool,
    pub internal_deps: Vec<InternalDep>,
}

/// The seam between core orchestration and a specific registry/ecosystem.
///
/// Implementations are expected to be stateless with respect to the release run:
/// all state is derived from disk and the registry, never from a persisted config file.
pub trait Adapter {
    /// Discover all workspace packages and their internal dependency edges.
    fn discover_packages(&self) -> Result<Vec<Pkg>>;

    /// Write `new` as the package's version into its manifest.
    fn write_version(&self, pkg: &Pkg, new: &str) -> Result<()>;

    /// Update `pkg`'s declared range for internal dependency `dep` to track `new_dep_version`.
    fn update_dep_range(&self, pkg: &Pkg, dep: &str, new_dep_version: &str) -> Result<()>;

    /// Render a version into the ecosystem's range syntax (e.g. `1.2.3` -> `^1.2.3`).
    fn format_range(&self, version: &str) -> String;

    /// Replace workspace links (e.g. `workspace:*`) with concrete published versions
    /// immediately before publishing. Some ecosystems (npm) do not do this automatically.
    fn resolve_workspace_links(&self, pkg: &Pkg) -> Result<()>;

    /// Refresh the lockfile after version writes so a CI install does not drift.
    fn update_lockfile(&self, root: &Path) -> Result<()>;

    /// The cascade rule, owned by the adapter rather than shared config:
    /// given a dependency's bump and the edge kind, what bump does the dependent take?
    fn dependent_bump(&self, dep_bump: Bump, kind: &DepKind) -> Bump;

    /// Registry check: is `version` of `pkg` already published? Used to make publish idempotent.
    fn is_published(&self, pkg: &Pkg, version: &str) -> Result<bool>;

    /// Publish `pkg`. `staged_assets` points at a prebuilt-artifact directory if the workflow
    /// staged binaries for this package, otherwise `None` (registry-only publish).
    fn publish(&self, pkg: &Pkg, staged_assets: Option<&Path>) -> Result<()>;
}
