//! Opening the release pull request. Behind a trait so the `version` flow is testable
//! without invoking `gh`.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};

/// A code-hosting forge that can open a pull request for a pushed branch.
pub trait Forge {
    fn open_pr(&self, branch: &str, title: &str, body: &str) -> Result<()>;
}

/// GitHub via the `gh` CLI.
pub struct GhForge {
    root: PathBuf,
}

impl GhForge {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl Forge for GhForge {
    fn open_pr(&self, branch: &str, title: &str, body: &str) -> Result<()> {
        let out = Command::new("gh")
            .args(["pr", "create", "--head", branch, "--title", title, "--body", body])
            .current_dir(&self.root)
            .output()
            .context("failed to run `gh pr create`")?;
        if !out.status.success() {
            bail!(
                "`gh pr create` failed:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(())
    }
}
