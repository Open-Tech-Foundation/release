//! Git access for preflight (and, later, the `version` branch/commit flow).
//!
//! Preflight derives each package's state from its last version tag `name@x.y.z`. That access
//! is behind the [`RepoState`] trait so the rule engine can be unit-tested with a fake, while
//! [`GitRepo`] provides the real `git`-backed implementation.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Read-only repository state preflight needs.
pub trait RepoState {
    /// The highest-versioned tag `name@x.y.z` for a package, or `None` if it has never shipped.
    fn last_tag(&self, pkg_name: &str) -> Result<Option<String>>;

    /// Number of commits since `tag` that touched `pkg_dir` (scoped to the package directory,
    /// so shared root files like the lockfile or CI config don't falsely dirty it).
    fn commit_count_since(&self, tag: &str, pkg_dir: &Path) -> Result<usize>;
}

/// The real `git`-backed implementation, rooted at the repository root.
pub struct GitRepo {
    root: PathBuf,
}

impl GitRepo {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

pub fn short_hash(root: &Path) -> Result<String> {
    let out = run_git(root, &["rev-parse", "--short", "HEAD"])?;
    Ok(out.trim().to_string())
}

impl RepoState for GitRepo {
    fn last_tag(&self, pkg_name: &str) -> Result<Option<String>> {
        let prefix = format!("{pkg_name}@");
        let stdout = run_git(&self.root, &["tag", "--list", &format!("{prefix}*")])?;
        let best = stdout
            .lines()
            .filter_map(|line| {
                let version = line.strip_prefix(&prefix)?;
                parse_semver(version).map(|sv| (sv, line.to_string()))
            })
            .max_by_key(|(sv, _)| *sv)
            .map(|(_, tag)| tag);
        Ok(best)
    }

    fn commit_count_since(&self, tag: &str, pkg_dir: &Path) -> Result<usize> {
        // Use a repo-relative pathspec so git accepts it regardless of the cwd.
        let rel = pkg_dir.strip_prefix(&self.root).unwrap_or(pkg_dir);
        let pathspec = rel
            .to_str()
            .with_context(|| format!("non-UTF-8 package path: {}", rel.display()))?;
        let range = format!("{tag}..HEAD");
        let stdout = run_git(&self.root, &["rev-list", "--count", &range, "--", pathspec])?;
        Ok(stdout.trim().parse().unwrap_or(0))
    }
}

/// Mutating git operations used by the `version` command's branch/commit/push flow.
pub trait GitOps {
    fn is_clean(&self) -> Result<bool>;
    fn current_branch(&self) -> Result<String>;
    fn create_branch(&self, name: &str) -> Result<()>;
    fn add_all(&self) -> Result<()>;
    fn commit(&self, message: &str) -> Result<()>;
    fn push_branch(&self, name: &str) -> Result<()>;
    fn create_tag(&self, name: &str) -> Result<()>;
    fn push_tag(&self, name: &str) -> Result<()>;
}

impl GitOps for GitRepo {
    fn is_clean(&self) -> Result<bool> {
        Ok(run_git(&self.root, &["status", "--porcelain"])?
            .trim()
            .is_empty())
    }

    fn current_branch(&self) -> Result<String> {
        Ok(run_git(&self.root, &["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim()
            .to_string())
    }

    fn create_branch(&self, name: &str) -> Result<()> {
        run_git(&self.root, &["checkout", "-b", name]).map(|_| ())
    }

    fn add_all(&self) -> Result<()> {
        run_git(&self.root, &["add", "-A"]).map(|_| ())
    }

    fn commit(&self, message: &str) -> Result<()> {
        run_git(&self.root, &["commit", "-q", "-m", message]).map(|_| ())
    }

    fn push_branch(&self, name: &str) -> Result<()> {
        run_git(&self.root, &["push", "--set-upstream", "origin", name]).map(|_| ())
    }

    fn create_tag(&self, name: &str) -> Result<()> {
        run_git(&self.root, &["tag", name]).map(|_| ())
    }

    fn push_tag(&self, name: &str) -> Result<()> {
        let refspec = format!("refs/tags/{name}");
        run_git(&self.root, &["push", "origin", &refspec]).map(|_| ())
    }
}

fn run_git(root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("failed to run `git {}`", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "`git {}` failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Parse `x.y.z` (ignoring any pre-release suffix, which v1 doesn't produce) for tag ordering.
fn parse_semver(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.split('-').next().unwrap_or(version);
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn commit_all(dir: &Path, msg: &str) {
        git(dir, &["add", "-A"]);
        git(
            dir,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=Test",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                msg,
            ],
        );
    }

    fn write(path: PathBuf, content: &str) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn last_tag_picks_highest_and_commit_count_is_path_scoped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init", "-q"]);

        let pkg_dir = root.join("packages/a");
        write(pkg_dir.join("package.json"), r#"{ "name": "a" }"#);
        commit_all(root, "init a");
        git(root, &["tag", "a@1.0.0"]);
        git(root, &["tag", "a@1.2.0"]);
        git(root, &["tag", "a@1.10.0"]); // numeric, not lexical, ordering

        let repo = GitRepo::new(root);
        assert_eq!(repo.last_tag("a").unwrap().as_deref(), Some("a@1.10.0"));
        assert_eq!(repo.last_tag("ghost").unwrap(), None);

        // A commit touching only a root file must NOT count against the package.
        write(root.join("pnpm-lock.yaml"), "lock: 1\n");
        commit_all(root, "touch root lockfile");
        assert_eq!(repo.commit_count_since("a@1.10.0", &pkg_dir).unwrap(), 0);

        // A commit touching the package dir does count.
        write(pkg_dir.join("index.js"), "// code\n");
        commit_all(root, "change package a");
        assert_eq!(repo.commit_count_since("a@1.10.0", &pkg_dir).unwrap(), 1);
    }
}
