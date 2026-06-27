//! The `upgrade` command — regenerates the GitHub workflow from the existing config.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::config::ReleaseConfig;
use crate::init::render_workflow;
use crate::prompt::Prompt;

/// Options for an `upgrade` run.
#[derive(Debug, Clone, Default)]
pub struct UpgradeOptions {
    /// Overwrite existing files (`release.yml`) without prompting.
    pub force: bool,
}

/// Load the config and regenerate the workflow.
pub fn orchestrate(root: &Path, opts: &UpgradeOptions, prompt: &dyn Prompt) -> Result<()> {
    let config = ReleaseConfig::load(root)
        .context("Could not load release.toml. Are you in an initialized repo?")?;
    let yaml = render_workflow(&config);
    let yml_path = root.join(".github/workflows/release.yml");

    if yml_path.exists()
        && !opts.force
        && !prompt.confirm(&format!("Overwrite {}? ", yml_path.display()))?
    {
        return Ok(());
    }

    fs::create_dir_all(yml_path.parent().unwrap())
        .with_context(|| format!("creating {}", yml_path.parent().unwrap().display()))?;
    fs::write(&yml_path, yaml).with_context(|| format!("writing {}", yml_path.display()))?;
    println!("Upgraded {}", yml_path.display());

    Ok(())
}
