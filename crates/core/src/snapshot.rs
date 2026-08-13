use anyhow::{Context, Result};
use std::path::Path;

use crate::adapter::Adapter;
use crate::config::ReleaseConfig;
use crate::git;
use crate::graph::Graph;
use crate::publish;
use crate::ui;

/// Options for a snapshot run.
#[derive(Debug, Clone, Default)]
pub struct SnapshotOptions {
    /// Print the versions this run would publish and stop, writing nothing.
    ///
    /// Snapshot rewrites every manifest and the lockfile in place with no restore. On an ephemeral
    /// runner that is harmless; run once in a working tree to see what it does and it leaves the
    /// tree rewritten. This is the way to look first.
    pub dry_run: bool,
}

pub fn run(
    adapter: &dyn Adapter,
    root: &Path,
    config: &ReleaseConfig,
    opts: &SnapshotOptions,
) -> Result<()> {
    let tag = config.snapshot_tag.as_deref().unwrap_or("snapshot");

    // 1. Get the current short git hash
    let hash = git::short_hash(root).context("failed to get short git hash for snapshot")?;

    // 2. Discover packages
    let packages = adapter.discover_packages()?;
    let graph = Graph::build(&packages)?;
    let order = graph.topo_order()?;

    // 3. Compute new snapshot versions
    // For each publishable package, if it is a pre-release already (e.g. 1.0.0-beta.1), we might strip it or just append.
    // The simplest format is: x.y.z-{tag}.{hash}
    let mut new_versions = std::collections::HashMap::new();
    for pkg in &order {
        if !pkg.publishable {
            continue;
        }
        let core = pkg.version.split('-').next().unwrap();
        let new_ver = format!("{}-{}.{}", core, tag, hash);
        new_versions.insert(pkg.name.clone(), new_ver);
    }

    if new_versions.is_empty() {
        ui::warn("No publishable packages found for snapshot.");
        return Ok(());
    }

    if opts.dry_run {
        ui::info(&format!(
            "Would publish {} snapshot version(s):",
            new_versions.len()
        ));
        let mut names: Vec<&String> = new_versions.keys().collect();
        names.sort();
        for name in names {
            ui::detail(&format!("{name}@{}", new_versions[name]));
        }
        ui::detail("no git tag and no GitHub Release: snapshots ship to the registry only");
        return Ok(());
    }

    // 4. Run pre_version hooks
    use crate::hooks::{HookRunner, ShHookRunner};
    let runner = ShHookRunner;
    runner.run_hooks(root, &config.hooks.pre_version)?;

    // 5. Write versions and update inter-dependencies
    for pkg in &order {
        if let Some(new_ver) = new_versions.get(&pkg.name) {
            adapter.write_version(pkg, new_ver)?;
        }
        for dep in &pkg.internal_deps {
            if let Some(dep_ver) = new_versions.get(&dep.name) {
                adapter.update_dep_range(pkg, &dep.name, dep_ver)?;
            }
        }
        adapter.resolve_workspace_links(pkg)?;
    }

    // 6. Update lockfile
    adapter.update_lockfile(root)?;

    // 7. Run post_version hooks
    runner.run_hooks(root, &config.hooks.post_version)?;

    // 8. Hand off to the standard publish flow for the actual build and registry push
    let skip = config.build_only_names();
    publish::run(
        adapter,
        root,
        &publish::PublishOptions {
            package: None,
            exclude_packages: Vec::new(),
            artifacts_dir: None,
            dry_run: false,
            tags: config.tag_formats(),
            skip,
            // The snapshot flow has no build-matrix stage, so it stages no binaries to require.
            require_staged: Vec::new(),
            changelog: config.changelog_layout(),
            // A snapshot ships to the registry only. See `PublishOptions::tag_releases`: tagging
            // one version per commit would bury — and outrank — the release tags `last_tag` reads.
            tag_releases: false,
        },
        &config.hooks,
    )?;

    Ok(())
}
