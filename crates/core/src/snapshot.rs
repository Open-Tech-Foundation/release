use anyhow::{Context, Result};
use std::path::Path;

use crate::adapter::Adapter;
use crate::config::ReleaseConfig;
use crate::git;
use crate::graph::Graph;
use crate::publish;

pub fn run(adapter: &dyn Adapter, root: &Path, config: &ReleaseConfig) -> Result<()> {
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
        println!("No publishable packages found for snapshot.");
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
            artifacts_dir: None,
            dry_run: false,
            skip,
        },
        &config.hooks,
    )?;

    Ok(())
}
