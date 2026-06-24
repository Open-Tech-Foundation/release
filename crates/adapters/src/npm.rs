//! The npm adapter — the only implemented adapter in v1.
//!
//! Baked-in rules & gotchas (see `docs/adapters/npm.md`), already battle-tested:
//!
//! - `dependent_bump`: `PeerDep => mirror(dep_bump)`; everything else => `Patch`.
//! - `is_published`: `npm view <name>@<version> version` succeeds => already published => skip.
//! - `publish`: `npm publish --access public --no-workspaces`
//!     - `--access public` is required for a scoped package's first publish.
//!     - `--no-workspaces` is required because the repo root is a private workspace; without it
//!       npm runs in workspace mode and skips the package even from its own directory.
//! - `resolve_workspace_links`: rewrite `workspace:*` / linked internal deps to the concrete
//!   published version before publish (npm does not do this automatically).
//! - No `private:true` guard hack — asset packages are normal publishable packages.

use std::path::Path;

use anyhow::Result;
use opentf_release_core::adapter::{Adapter, Bump, DepKind, Pkg};

/// npm-backed adapter. Rooted at the workspace directory.
pub struct NpmAdapter {
    pub root: std::path::PathBuf,
}

impl NpmAdapter {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl Adapter for NpmAdapter {
    fn discover_packages(&self) -> Result<Vec<Pkg>> {
        todo!("read workspace globs from root package.json, parse each package.json")
    }

    fn write_version(&self, pkg: &Pkg, new: &str) -> Result<()> {
        todo!("rewrite the `version` field in package.json, preserving formatting")
    }

    fn update_dep_range(&self, pkg: &Pkg, dep: &str, new_dep_version: &str) -> Result<()> {
        todo!("rewrite the dependency range across deps/peerDeps/devDeps")
    }

    fn format_range(&self, version: &str) -> String {
        format!("^{version}")
    }

    fn resolve_workspace_links(&self, pkg: &Pkg) -> Result<()> {
        todo!("replace workspace:* / linked ranges with concrete versions before publish")
    }

    fn update_lockfile(&self, root: &Path) -> Result<()> {
        todo!("run `npm install --package-lock-only` (or equivalent) to refresh the lockfile")
    }

    fn dependent_bump(&self, dep_bump: Bump, kind: &DepKind) -> Bump {
        match kind {
            DepKind::PeerDep => dep_bump, // mirror
            _ => Bump::Patch,
        }
    }

    fn is_published(&self, pkg: &Pkg, version: &str) -> Result<bool> {
        todo!("`npm view <name>@<version> version`; Ok(true) on success, Ok(false) on 404")
    }

    fn publish(&self, pkg: &Pkg, staged_assets: Option<&Path>) -> Result<()> {
        todo!("npm publish --access public --no-workspaces, attaching staged_assets if present")
    }
}
