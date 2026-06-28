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
    /// Package names to skip — `build-only` packages from `release.toml`. They ship via the
    /// GitHub Release the workflow creates, never through a registry, so `publish` leaves them
    /// alone even though their manifests look publishable.
    pub skip: Vec<String>,
}

/// Wire up the real git/forge and run the flow.
pub fn run(
    adapter: &dyn Adapter,
    root: &Path,
    opts: &PublishOptions,
    hooks: &crate::config::Hooks,
) -> Result<()> {
    run_many(&[adapter], root, opts, hooks)
}

/// Wire up the real git/forge and run the flow across every enabled adapter.
pub fn run_many(
    adapters: &[&dyn Adapter],
    root: &Path,
    opts: &PublishOptions,
    hooks: &crate::config::Hooks,
) -> Result<()> {
    let repo = GitRepo::new(root);
    let forge = GhForge::new(root);
    let hook_runner = crate::hooks::ShHookRunner;
    orchestrate_many(adapters, &repo, &forge, root, opts, hooks, &hook_runner)
}

/// The testable core of the publish flow:
/// discover → filter (publishable & not already published) → topo sort →
/// for each: resolve links → publish → tag → optional release. Halt on first failure.
pub fn orchestrate(
    adapter: &dyn Adapter,
    git: &dyn GitOps,
    forge: &dyn Forge,
    root: &Path,
    opts: &PublishOptions,
    hooks: &crate::config::Hooks,
    hook_runner: &dyn crate::hooks::HookRunner,
) -> Result<()> {
    orchestrate_many(&[adapter], git, forge, root, opts, hooks, hook_runner)
}

struct AdapterPublish<'a> {
    adapter: &'a dyn Adapter,
    to_publish: Vec<crate::adapter::Pkg>,
}

/// The testable multi-adapter publish flow. Hooks wrap the whole command once, while each adapter
/// still publishes its own package graph in dependency order.
pub fn orchestrate_many(
    adapters: &[&dyn Adapter],
    git: &dyn GitOps,
    forge: &dyn Forge,
    root: &Path,
    opts: &PublishOptions,
    hooks: &crate::config::Hooks,
    hook_runner: &dyn crate::hooks::HookRunner,
) -> Result<()> {
    let mut plans = Vec::with_capacity(adapters.len());

    for adapter in adapters {
        let packages = adapter.discover_packages()?;
        let graph = Graph::build(&packages)?;

        // Dependencies before dependents; keep only publishable, not-already-published packages.
        let mut to_publish = Vec::new();
        for pkg in graph.topo_order()? {
            if !pkg.publishable {
                continue; // private apps are never published
            }
            if opts.skip.iter().any(|n| n == &pkg.name) {
                continue; // build-only: ships via GitHub Release, not a registry
            }
            if adapter.is_published(pkg, &pkg.version)? {
                continue; // already shipped → idempotent / resumable
            }
            to_publish.push(pkg.clone());
        }

        plans.push(AdapterPublish {
            adapter: *adapter,
            to_publish,
        });
    }

    let has_publish_work = plans.iter().any(|plan| !plan.to_publish.is_empty());

    if !has_publish_work {
        println!("Nothing to publish: every package is already at its published version.");
        return Ok(());
    }

    if opts.dry_run {
        println!("Would publish (in dependency order):");
        for plan in &plans {
            for pkg in &plan.to_publish {
                println!("  {}@{}", pkg.name, pkg.version);
            }
        }
        return Ok(());
    }

    if !hooks.pre_publish.is_empty() {
        hook_runner.run_hooks(root, &hooks.pre_publish)?;
    }

    for plan in plans {
        for pkg in plan.to_publish {
            plan.adapter.resolve_workspace_links(&pkg)?;

            // Attach staged binaries only if their directory actually exists on disk.
            let staged = opts
                .artifacts_dir
                .as_ref()
                .map(|dir| dir.join(&pkg.name))
                .filter(|path| path.exists());
            plan.adapter.publish(&pkg, staged.as_deref())?; // halt on failure (no rollback)

            let tag = format!("{}@{}", pkg.name, pkg.version);
            git.create_tag(&tag)?;
            git.push_tag(&tag)?;

            if let Some(notes) = changelog::dated_section_notes(&pkg.changelog_path, &pkg.version)?
            {
                forge.create_release(&tag, &tag, &notes)?;
            }

            println!("Published {tag}");
        }
    }

    if !hooks.post_publish.is_empty() {
        hook_runner.run_hooks(root, &hooks.post_publish)?;
    }

    Ok(())
}
