//! The `publish` command — non-interactive, run in **CI**, stateless.
//!
//! Publishes changed packages in dependency order, idempotent and resumable, halting on the
//! first failure (publishing is irreversible — forward-resume only). See
//! `docs/commands/publish.md`.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::adapter::Adapter;
use crate::changelog;
use crate::config::{format_tag, DEFAULT_TAG_FORMAT};
use crate::forge::{Forge, GhForge};
use crate::git::{GitOps, GitRepo};
use crate::graph::Graph;

/// Options for a `publish` run.
#[derive(Debug, Clone)]
pub struct PublishOptions {
    /// Root of the staged-artifact tree (`.artifacts/`), if the workflow staged binaries.
    pub artifacts_dir: Option<PathBuf>,
    /// Resolve the plan and print it, but do not publish or push tags.
    pub dry_run: bool,
    /// Git tag format for releases.
    pub tag_format: String,
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

impl Default for PublishOptions {
    fn default() -> Self {
        Self {
            artifacts_dir: None,
            dry_run: false,
            tag_format: DEFAULT_TAG_FORMAT.to_string(),
            skip: Vec::new(),
        }
    }
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
    pending: Vec<Pending>,
}

/// One package still needing work: it is either not yet on the registry, or it published on a
/// previous run but its tag/GitHub Release was never created (a partial-failure resume).
struct Pending {
    pkg: crate::adapter::Pkg,
    tag: String,
    /// `false` when the registry already has this version — only the tag/release are missing, so
    /// the registry publish is skipped to avoid a "version already published" error on resume.
    needs_publish: bool,
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

        // Dependencies before dependents; keep publishable packages that still need any work.
        let mut pending = Vec::new();
        for pkg in graph.topo_order()? {
            if !pkg.publishable {
                continue; // private apps are never published
            }
            if opts.skip.iter().any(|n| n == &pkg.name) {
                continue; // build-only: ships via GitHub Release, not a registry
            }
            let tag = format_tag(&opts.tag_format, &pkg.name, &pkg.version)?;
            let published = adapter.is_published(pkg, &pkg.version)?;
            let tagged = git.tag_exists(&tag)?;
            // A package is fully shipped only when the registry has it AND its tag exists. If it
            // published on an earlier run but the tag/release step failed, it is reprocessed so
            // the missing tag and GitHub Release get created — without re-publishing the version.
            if published && tagged {
                continue;
            }
            pending.push(Pending {
                pkg: pkg.clone(),
                tag,
                needs_publish: !published,
            });
        }

        plans.push(AdapterPublish {
            adapter: *adapter,
            pending,
        });
    }

    let has_work = plans.iter().any(|plan| !plan.pending.is_empty());

    if !has_work {
        println!("Nothing to publish: every package is already published and tagged.");
        return Ok(());
    }

    if opts.dry_run {
        println!("Would publish (in dependency order):");
        for plan in &plans {
            for p in &plan.pending {
                if p.needs_publish {
                    println!("  {}@{}", p.pkg.name, p.pkg.version);
                } else {
                    println!(
                        "  {}@{} (already published — tag/release only)",
                        p.pkg.name, p.pkg.version
                    );
                }
            }
        }
        return Ok(());
    }

    if !hooks.pre_publish.is_empty() {
        hook_runner.run_hooks(root, &hooks.pre_publish)?;
    }

    for plan in plans {
        for p in plan.pending {
            if p.needs_publish {
                plan.adapter.resolve_workspace_links(&p.pkg)?;

                // Attach staged binaries only if their directory actually exists on disk.
                let staged = opts
                    .artifacts_dir
                    .as_ref()
                    .map(|dir| dir.join(&p.pkg.name))
                    .filter(|path| path.exists());
                plan.adapter.publish(&p.pkg, staged.as_deref())?; // halt on failure (no rollback)
            }

            // Tag + release are idempotent so a forward-resume after a mid-package failure fills
            // in whatever is missing instead of skipping the package and stranding the release.
            if !git.tag_exists(&p.tag)? {
                git.create_tag(&p.tag)?;
            }
            git.push_tag(&p.tag)?;

            if let Some(notes) =
                changelog::dated_section_notes(&p.pkg.changelog_path, &p.pkg.version)?
            {
                if !forge.release_exists(&p.tag)? {
                    forge.create_release(&p.tag, &p.tag, &notes)?;
                }
            }

            if p.needs_publish {
                println!("Published {}", p.tag);
            } else {
                println!("Tagged {} (already published)", p.tag);
            }
        }
    }

    if !hooks.post_publish.is_empty() {
        hook_runner.run_hooks(root, &hooks.post_publish)?;
    }

    Ok(())
}
