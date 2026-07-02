//! The [`Adapter`] trait and the ecosystem-agnostic domain types the core operates on.
//!
//! Core never reads a `package.json` (or any manifest) directly — every ecosystem-specific
//! decision (range syntax, cascade rule, publish mechanics, registry lookups) lives behind
//! this trait. Implementations live in `otf-release-adapters` (npm, cargo, and generic).

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::ChangelogScope;

/// A semantic-version bump level.
///
/// The derived `Ord` is used only to pick the stronger of two bumps *within the same channel*:
/// stable bumps order `Patch < Minor < Major`, and prerelease bumps of one channel order
/// `Prerelease < PrePatch < PreMinor < PreMajor`. It is **not** a meaningful total order across
/// channels — a stable `Patch` outranking a `PreMajor` by declaration order would silently drop
/// prerelease intent when a package is reached by several cascade paths. Cross-channel merges go
/// through [`Bump::merge`] instead, which keeps the prerelease and refuses genuinely ambiguous
/// mixes (two different prerelease channels).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bump {
    Graduate,
    Prerelease(String),
    PrePatch(String),
    PreMinor(String),
    PreMajor(String),
    Patch,
    Minor,
    Major,
}

impl Bump {
    /// The prerelease channel this bump targets (`beta`, `rc`, …), or `None` for a stable bump
    /// (`Patch`/`Minor`/`Major`) or a `Graduate` (which produces a stable version).
    fn prerelease_channel(&self) -> Option<&str> {
        match self {
            Bump::Prerelease(ch)
            | Bump::PrePatch(ch)
            | Bump::PreMinor(ch)
            | Bump::PreMajor(ch) => Some(ch),
            _ => None,
        }
    }

    /// Merge two bumps that reach the same package by different cascade (or lockstep) paths,
    /// returning the stronger one.
    ///
    /// Within a single channel this is the larger magnitude (`max` over the derived order). A
    /// prerelease bump **dominates** a stable one, because a stable release whose internal range
    /// points at a prerelease (e.g. a peer range of `^2.0.0-beta.0`) is a broken release — the
    /// prerelease intent must not be dropped. Two *different* prerelease channels (e.g. `beta`
    /// and `rc`) cannot be reconciled by magnitude and are a hard error; release them separately.
    pub fn merge(&self, other: &Bump) -> Result<Bump> {
        match (self.prerelease_channel(), other.prerelease_channel()) {
            (None, None) => Ok(self.max(other).clone()),
            (Some(_), None) => Ok(self.clone()),
            (None, Some(_)) => Ok(other.clone()),
            (Some(a), Some(b)) if a == b => Ok(self.max(other).clone()),
            (Some(a), Some(b)) => anyhow::bail!(
                "conflicting prerelease channels `{a}` and `{b}` reach the same package; \
                 release these channels in separate runs"
            ),
        }
    }
}

impl std::fmt::Display for Bump {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Bump::Graduate => "graduate",
            Bump::Prerelease(ch) => return write!(f, "prerelease ({ch})"),
            Bump::PrePatch(ch) => return write!(f, "prepatch ({ch})"),
            Bump::PreMinor(ch) => return write!(f, "preminor ({ch})"),
            Bump::PreMajor(ch) => return write!(f, "premajor ({ch})"),
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

/// Apply the configured changelog layout after adapter discovery.
pub fn apply_changelog_scope(root: &Path, scope: &ChangelogScope, packages: &mut [Pkg]) {
    if *scope == ChangelogScope::Root {
        let root_changelog = root.join("CHANGELOG.md");
        for pkg in packages {
            pkg.changelog_path = root_changelog.clone();
        }
    }
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

    /// Sets of packages that are versioned in lockstep and must always move to the **same**
    /// version together — e.g. cargo crates that inherit `version.workspace = true` and share
    /// one `[workspace.package] version`. Each inner vec is one such group, named by package.
    ///
    /// Default: no groups (every package is versioned independently). The `version` flow uses
    /// this to reconcile a group's members to a single bump, so they cannot diverge.
    fn version_groups(&self) -> Result<Vec<Vec<String>>> {
        Ok(Vec::new())
    }

    /// Registry check: is `version` of `pkg` already published? Used to make publish idempotent.
    fn is_published(&self, pkg: &Pkg, version: &str) -> Result<bool>;

    /// Publish `pkg`. `staged_assets` points at a prebuilt-artifact directory if the workflow
    /// staged binaries for this package, otherwise `None` (registry-only publish).
    fn publish(&self, pkg: &Pkg, staged_assets: Option<&Path>) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pre(ch: &str) -> Bump {
        Bump::PreMajor(ch.to_string())
    }

    #[test]
    fn merge_takes_max_within_the_stable_channel() {
        assert_eq!(Bump::Patch.merge(&Bump::Major).unwrap(), Bump::Major);
        assert_eq!(Bump::Minor.merge(&Bump::Patch).unwrap(), Bump::Minor);
    }

    #[test]
    fn merge_takes_max_within_one_prerelease_channel() {
        assert_eq!(
            Bump::Prerelease("beta".into())
                .merge(&Bump::PreMajor("beta".into()))
                .unwrap(),
            Bump::PreMajor("beta".into())
        );
    }

    #[test]
    fn merge_lets_a_prerelease_dominate_a_stable_bump() {
        // The bug this guards: a package reached by a PreMajor peer path and a Patch dep path must
        // stay a prerelease, not silently become a stable Patch pointing at a `-beta` peer range.
        assert_eq!(Bump::Patch.merge(&pre("beta")).unwrap(), pre("beta"));
        assert_eq!(pre("beta").merge(&Bump::Patch).unwrap(), pre("beta"));
        assert_eq!(Bump::Major.merge(&pre("beta")).unwrap(), pre("beta"));
    }

    #[test]
    fn merge_refuses_conflicting_prerelease_channels() {
        let err = pre("beta").merge(&pre("rc")).unwrap_err().to_string();
        assert!(err.contains("beta") && err.contains("rc"), "got: {err}");
    }
}
