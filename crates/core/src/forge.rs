//! Opening the release pull request. Behind a trait so the `version` flow is testable
//! without invoking `gh`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// How a GitHub Release body is sourced when creating it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseNotes {
    /// A concrete, tool-computed body (curated changelog section or commit list).
    Body(String),
    /// Let GitHub auto-generate the notes (`gh --generate-notes`).
    Generate,
}

/// A code-hosting forge that can open a pull request and publish a release.
pub trait Forge {
    fn open_pr(&self, branch: &str, title: &str, body: &str) -> Result<()>;
    fn create_release(&self, tag: &str, title: &str, notes: &str) -> Result<()>;
    /// Whether a release for this tag already exists. Keeps `publish`'s release step idempotent
    /// so a forward-resume doesn't fail trying to recreate a release that already shipped.
    fn release_exists(&self, tag: &str) -> Result<bool>;

    /// Create a release for a `build-only` package: an optional `--target` ref, a notes source
    /// (curated body or GitHub-generated), and any number of asset files to attach. This is what
    /// `otf-release github-release` calls so the workflow never hand-rolls `gh release create` in
    /// inline bash. The default delegates to [`create_release`](Self::create_release) so existing
    /// test doubles keep compiling; [`GhForge`] overrides it with the full `gh` invocation.
    fn create_release_with_assets(
        &self,
        tag: &str,
        title: &str,
        notes: &ReleaseNotes,
        _target: Option<&str>,
        _assets: &[PathBuf],
    ) -> Result<()> {
        match notes {
            ReleaseNotes::Body(body) => self.create_release(tag, title, body),
            ReleaseNotes::Generate => self.create_release(tag, title, ""),
        }
    }
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
            .args([
                "pr", "create", "--head", branch, "--title", title, "--body", body,
            ])
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

    fn create_release(&self, tag: &str, title: &str, notes: &str) -> Result<()> {
        let out = Command::new("gh")
            .args(["release", "create", tag, "--title", title, "--notes", notes])
            .current_dir(&self.root)
            .output()
            .context("failed to run `gh release create`")?;
        if !out.status.success() {
            bail!(
                "`gh release create` failed:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(())
    }

    fn release_exists(&self, tag: &str) -> Result<bool> {
        // `gh release view <tag>` exits 0 when the release exists, non-zero otherwise. Any
        // failure (including auth/network) is treated as "not present"; the subsequent
        // `create_release` call will surface a real error if one is genuinely wrong.
        let out = Command::new("gh")
            .args(["release", "view", tag])
            .current_dir(&self.root)
            .output()
            .context("failed to run `gh release view`")?;
        Ok(out.status.success())
    }

    fn create_release_with_assets(
        &self,
        tag: &str,
        title: &str,
        notes: &ReleaseNotes,
        target: Option<&str>,
        assets: &[PathBuf],
    ) -> Result<()> {
        let mut args: Vec<String> = vec![
            "release".into(),
            "create".into(),
            tag.into(),
            "--title".into(),
            title.into(),
        ];
        if let Some(target) = target {
            args.push("--target".into());
            args.push(target.into());
        }
        match notes {
            ReleaseNotes::Body(body) => {
                args.push("--notes".into());
                args.push(body.clone());
            }
            ReleaseNotes::Generate => args.push("--generate-notes".into()),
        }
        for asset in assets {
            args.push(path_arg(asset)?);
        }
        let out = Command::new("gh")
            .args(&args)
            .current_dir(&self.root)
            .output()
            .context("failed to run `gh release create`")?;
        if !out.status.success() {
            bail!(
                "`gh release create` failed:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(())
    }
}

/// Render a path as a `gh` argument, rejecting non-UTF-8 paths with a clear error rather than a
/// lossy conversion that could point `gh` at the wrong file.
fn path_arg(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .with_context(|| format!("non-UTF-8 asset path: {}", path.display()))
}
