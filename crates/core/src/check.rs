//! The `check` command — the CI release gate.
//!
//! `release.yml` runs on every push to `main`, but most pushes aren't releases. This command is the
//! single decision "does this commit release anything?": it prints `true` when at least one
//! configured package has a real version whose tag doesn't exist yet, else `false`. The generated
//! workflow's `check-release` job is just `should_release=$(otf-release check)`.
//!
//! It reuses the *same* primitives as [`crate::publish`] — `discover_packages` for the version,
//! `format_tag` for the tag, `git tag` for existence — so the gate can never drift from what
//! actually ships. Unlike `publish`, build-only packages count too: they ship via the GitHub
//! Release the same run creates, so a release where only a build-only package bumped must not skip.

use std::path::Path;

use anyhow::Result;

use crate::adapter::{Adapter, Pkg};
use crate::config::{format_tag, ReleaseConfig};
use crate::git::{GitOps, GitRepo};

/// The version a package carries before its first release — checking it out is not a release.
const UNRELEASED_VERSION: &str = "0.0.0";

/// Wire up the real git repo, discover every enabled adapter's packages, and decide.
pub fn run_many(adapters: &[&dyn Adapter], root: &Path, config: &ReleaseConfig) -> Result<bool> {
    run_many_for_package(adapters, root, config, None, &[])
}

/// Decide whether one package, or any package when omitted, has a pending release.
pub fn run_many_for_package(
    adapters: &[&dyn Adapter],
    root: &Path,
    config: &ReleaseConfig,
    package: Option<&str>,
    exclude_packages: &[String],
) -> Result<bool> {
    let repo = GitRepo::new(root);
    let mut packages = Vec::new();
    for adapter in adapters {
        let mut discovered = adapter.discover_packages()?;
        // Fold `skip_publish` into `publishable = false` so the decision below excludes both those
        // and private apps with one check — matching how `publish` treats them.
        config.apply_publish_skips(&mut discovered);
        if let Some(name) = package {
            discovered.retain(|pkg| pkg.name == name);
        }
        discovered.retain(|pkg| !exclude_packages.contains(&pkg.name));
        packages.extend(discovered);
    }
    any_pending(&packages, &config.tag_format, |tag| repo.tag_exists(tag))
}

/// The pure gate decision: `true` when at least one publishable package has a real version whose
/// `tag_format` tag is absent — i.e. a release is pending. Non-publishable packages (private apps
/// and `skip_publish`) and the `0.0.0` unreleased sentinel are ignored. `tag_exists` is injected so
/// this is unit-testable without a live repo, and so `run_many` owns the one git dependency.
pub fn any_pending(
    packages: &[Pkg],
    tag_format: &str,
    tag_exists: impl Fn(&str) -> Result<bool>,
) -> Result<bool> {
    for pkg in packages {
        if !pkg.publishable || pkg.version == UNRELEASED_VERSION {
            continue;
        }
        let tag = format_tag(tag_format, &pkg.name, &pkg.version)?;
        if !tag_exists(&tag)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn pkg(name: &str, version: &str, publishable: bool) -> Pkg {
        Pkg {
            name: name.to_string(),
            version: version.to_string(),
            manifest_path: PathBuf::from(format!("packages/{name}/manifest")),
            changelog_path: PathBuf::new(),
            publishable,
            internal_deps: Vec::new(),
        }
    }

    /// A tag-existence oracle over a fixed set of already-released tags.
    fn tags(existing: &'static [&'static str]) -> impl Fn(&str) -> Result<bool> {
        move |tag: &str| Ok(existing.contains(&tag))
    }

    #[test]
    fn true_when_a_bumped_package_has_no_tag_yet() {
        let pkgs = vec![
            pkg("@x/web", "0.7.0", true),          // bumped, tag missing
            pkg("@x/web-compiler", "0.2.0", true), // unchanged, tag exists
        ];
        assert!(any_pending(&pkgs, "{name}@{version}", tags(&["@x/web-compiler@0.2.0"])).unwrap());
    }

    #[test]
    fn false_when_every_package_is_already_tagged() {
        let pkgs = vec![
            pkg("@x/web", "0.7.0", true),
            pkg("@x/web-compiler", "0.2.0", true),
        ];
        let existing = tags(&["@x/web@0.7.0", "@x/web-compiler@0.2.0"]);
        assert!(!any_pending(&pkgs, "{name}@{version}", existing).unwrap());
    }

    #[test]
    fn ignores_unreleased_sentinel_and_non_publishable() {
        // `@x/manual` here stands for a `skip_publish` package: `run_many` folds those into
        // `publishable = false` via `apply_publish_skips` before this decision runs.
        let pkgs = vec![
            pkg("@x/create-web", "0.0.0", true),   // sentinel: never released
            pkg("@x/private-app", "9.9.9", false), // private / skip_publish
        ];
        assert!(!any_pending(&pkgs, "{name}@{version}", tags(&[])).unwrap());
    }

    #[test]
    fn build_only_package_still_counts() {
        // A cargo CLI shipped via GitHub Release is publishable in `Pkg` terms; `publish` skips it
        // but the gate must not — a release where only it bumped still needs to run.
        let pkgs = vec![pkg("otf-release", "0.15.0", true)];
        assert!(any_pending(&pkgs, "{name}@{version}", tags(&[])).unwrap());
    }
}
