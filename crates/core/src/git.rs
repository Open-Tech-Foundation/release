//! Git access for preflight (and, later, the `version` branch/commit flow).
//!
//! Preflight derives each package's state from its last version tag matching the configured tag
//! format. That access is behind the [`RepoState`] trait so the rule engine can be unit-tested
//! with a fake, while [`GitRepo`] provides the real `git`-backed implementation.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Read-only repository state preflight needs.
pub trait RepoState {
    /// The highest-versioned tag matching any configured tag format, or `None` if it has never
    /// shipped.
    fn last_tag(&self, pkg_name: &str, tag_formats: &[String]) -> Result<Option<String>>;

    /// Number of commits since `tag` that touched `pkg_dir` (scoped to the package directory,
    /// so shared root files like the lockfile or CI config don't falsely dirty it).
    fn commit_count_since(&self, tag: &str, pkg_dir: &Path) -> Result<usize>;

    /// Get the formatted commits touching `pkg_dir` since `tag` (or all if None).
    fn commits_since(&self, tag: Option<&str>, pkg_dir: &Path) -> Result<String>;
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
    fn last_tag(&self, pkg_name: &str, tag_formats: &[String]) -> Result<Option<String>> {
        let stdout = run_git(&self.root, &["tag", "--list"])?;
        let best = stdout
            .lines()
            .flat_map(|line| {
                tag_formats.iter().filter_map(move |format| {
                    let version = version_from_tag(line, format, pkg_name)?;
                    parse_semver(version).map(|sv| (sv, line.to_string()))
                })
            })
            .max_by_key(|(sv, _)| *sv)
            .map(|(_, tag)| tag);
        Ok(best)
    }

    fn commit_count_since(&self, tag: &str, pkg_dir: &Path) -> Result<usize> {
        // Use a repo-relative pathspec so git accepts it regardless of the cwd.
        let pathspec = repo_pathspec(&self.root, pkg_dir)?;
        let range = format!("{tag}..HEAD");
        let stdout = run_git(&self.root, &["rev-list", "--count", &range, "--", pathspec])?;
        Ok(stdout.trim().parse().unwrap_or(0))
    }

    fn commits_since(&self, tag: Option<&str>, pkg_dir: &Path) -> Result<String> {
        let pathspec = repo_pathspec(&self.root, pkg_dir)?;
        let range = match tag {
            Some(t) => format!("{t}..HEAD"),
            None => "HEAD".to_string(),
        };
        let stdout = run_git(
            &self.root,
            &["log", &range, "--pretty=format:* %s", "--", pathspec],
        )?;
        Ok(stdout.trim().to_string())
    }
}

fn repo_pathspec<'a>(root: &Path, path: &'a Path) -> Result<&'a str> {
    let rel = path.strip_prefix(root).unwrap_or(path);
    if rel.as_os_str().is_empty() {
        return Ok(".");
    }
    rel.to_str()
        .with_context(|| format!("non-UTF-8 package path: {}", rel.display()))
}

/// Mutating git operations used by the `version` command's branch/commit/push flow.
pub trait GitOps {
    fn is_clean(&self) -> Result<bool>;
    fn current_branch(&self) -> Result<String>;
    fn create_branch(&self, name: &str) -> Result<()>;
    fn checkout_branch(&self, name: &str) -> Result<()>;
    fn diff_stat(&self) -> Result<String>;
    fn reset_hard(&self) -> Result<()>;
    fn add_all(&self) -> Result<()>;
    fn commit(&self, message: &str) -> Result<()>;
    fn push_branch(&self, name: &str) -> Result<()>;
    fn create_tag(&self, name: &str) -> Result<()>;
    fn push_tag(&self, name: &str) -> Result<()>;
    /// Whether a tag with this exact name already exists locally. Used to keep `publish`'s
    /// tagging step idempotent so a forward-resume never tries to recreate an existing tag.
    fn tag_exists(&self, name: &str) -> Result<bool>;
    /// Switch back to the release's default branch (`branch`) and update it from its upstream.
    fn return_to_default_branch(&self, branch: &str) -> Result<()>;
    /// Delete a local release branch after it has been pushed.
    fn delete_local_branch(&self, name: &str) -> Result<()>;
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
        run_git(&self.root, &["checkout", "-B", name]).map(|_| ())
    }

    fn checkout_branch(&self, name: &str) -> Result<()> {
        run_git(&self.root, &["checkout", name]).map(|_| ())
    }

    fn diff_stat(&self) -> Result<String> {
        run_git(&self.root, &["diff", "--stat"])
    }

    fn reset_hard(&self) -> Result<()> {
        run_git(&self.root, &["reset", "--hard"]).map(|_| ())
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

    fn tag_exists(&self, name: &str) -> Result<bool> {
        Ok(!run_git(&self.root, &["tag", "--list", name])?
            .trim()
            .is_empty())
    }

    fn return_to_default_branch(&self, branch: &str) -> Result<()> {
        run_git(&self.root, &["switch", branch])?;
        run_git(&self.root, &["pull", "--tags"])?;
        Ok(())
    }

    fn delete_local_branch(&self, name: &str) -> Result<()> {
        run_git(&self.root, &["branch", "-D", name]).map(|_| ())
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

fn version_from_tag<'a>(tag: &'a str, tag_format: &str, pkg_name: &str) -> Option<&'a str> {
    let (before_version, after_version) = tag_format.split_once("{version}")?;
    let prefix = before_version.replace("{name}", pkg_name);
    let suffix = after_version.replace("{name}", pkg_name);
    tag.strip_prefix(&prefix)?.strip_suffix(&suffix)
}

/// Parse the `x.y.z` core of a version (ignoring any pre-release suffix) into a comparable tuple.
pub fn parse_semver(version: &str) -> Option<(u64, u64, u64)> {
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
        assert_eq!(
            repo.last_tag("a", &["{name}@{version}".to_string()])
                .unwrap()
                .as_deref(),
            Some("a@1.10.0")
        );
        assert_eq!(
            repo.last_tag("ghost", &["{name}@{version}".to_string()])
                .unwrap(),
            None
        );
        git(root, &["tag", "v2.0.0"]);
        git(root, &["tag", "v1.9.0"]);
        assert_eq!(
            repo.last_tag("a", &["v{version}".to_string()])
                .unwrap()
                .as_deref(),
            Some("v2.0.0")
        );
        assert_eq!(
            repo.last_tag(
                "a",
                &["v{version}".to_string(), "{name}@{version}".to_string()]
            )
            .unwrap()
            .as_deref(),
            Some("v2.0.0")
        );

        // A commit touching only a root file must NOT count against the package.
        write(root.join("pnpm-lock.yaml"), "lock: 1\n");
        commit_all(root, "touch root lockfile");
        assert_eq!(repo.commit_count_since("a@1.10.0", &pkg_dir).unwrap(), 0);
        assert_eq!(repo.commit_count_since("v2.0.0", root).unwrap(), 1);
        assert!(repo
            .commits_since(Some("v2.0.0"), root)
            .unwrap()
            .contains("touch root lockfile"));

        // A commit touching the package dir does count.
        write(pkg_dir.join("index.js"), "// code\n");
        commit_all(root, "change package a");
        assert_eq!(repo.commit_count_since("a@1.10.0", &pkg_dir).unwrap(), 1);
    }
}
