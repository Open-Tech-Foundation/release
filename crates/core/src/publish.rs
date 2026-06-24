//! The `publish` command — non-interactive, run in **CI**, stateless.
//!
//! Publishes changed packages in dependency order, idempotent and resumable, halting on the
//! first failure (publishing is irreversible — forward-resume only). See
//! `docs/commands/publish.md`.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::adapter::Adapter;
use crate::changelog;
use crate::forge::{Forge, GhForge};
use crate::git::{GitOps, GitRepo};
use crate::graph::Graph;

/// Options for a `publish` run.
#[derive(Debug, Clone, Default)]
pub struct PublishOptions {
    /// Root of the staged-artifact tree (`.artifacts/`), if the workflow staged binaries.
    pub artifacts_dir: Option<PathBuf>,
    /// Resolve the plan and print it, but do not publish or push tags.
    pub dry_run: bool,
}

/// Wire up the real git/forge and run the flow.
pub fn run(adapter: &dyn Adapter, root: &Path, opts: &PublishOptions) -> Result<()> {
    let repo = GitRepo::new(root);
    let forge = GhForge::new(root);
    orchestrate(adapter, &repo, &forge, opts)
}

/// The testable core of the publish flow:
/// discover → filter (publishable & not already published) → topo sort →
/// for each: resolve links → publish → tag → optional release. Halt on first failure.
pub fn orchestrate(
    adapter: &dyn Adapter,
    git: &dyn GitOps,
    forge: &dyn Forge,
    opts: &PublishOptions,
) -> Result<()> {
    let packages = adapter.discover_packages()?;
    let graph = Graph::build(&packages)?;

    // Dependencies before dependents; keep only publishable, not-already-published packages.
    let mut to_publish = Vec::new();
    for pkg in graph.topo_order()? {
        if !pkg.publishable {
            continue; // private apps are never published
        }
        if adapter.is_published(pkg, &pkg.version)? {
            continue; // already shipped → idempotent / resumable
        }
        to_publish.push(pkg);
    }

    if to_publish.is_empty() {
        println!("Nothing to publish: every package is already at its published version.");
        return Ok(());
    }

    if opts.dry_run {
        println!("Would publish (in dependency order):");
        for pkg in &to_publish {
            println!("  {}@{}", pkg.name, pkg.version);
        }
        return Ok(());
    }

    for pkg in to_publish {
        adapter.resolve_workspace_links(pkg)?;

        // Attach staged binaries only if their directory actually exists on disk.
        let staged = opts
            .artifacts_dir
            .as_ref()
            .map(|dir| dir.join(&pkg.name))
            .filter(|path| path.exists());
        adapter.publish(pkg, staged.as_deref())?; // halt on failure (no rollback)

        let tag = format!("{}@{}", pkg.name, pkg.version);
        git.create_tag(&tag)?;
        git.push_tag(&tag)?;

        if let Some(notes) = changelog::dated_section_notes(&pkg.changelog_path, &pkg.version)? {
            forge.create_release(&tag, &tag, &notes)?;
        }

        println!("Published {tag}");
    }

    Ok(())
}
