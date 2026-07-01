//! Strict preflight gate — all-or-nothing, runs before any prompt or mutation.
//!
//! For every non-private package, state is derived from its last git tag matching the configured
//! tag format.
//! A single violation collects *all* violations, prints them, and exits non-zero before
//! any `release/*` branch is created or any file is written. See `docs/preflight.md`.

use std::path::Path;

use anyhow::Result;

use crate::adapter::Pkg;
use crate::changelog;
use crate::git::RepoState;

/// A single preflight failure, e.g. "3 commits since core@1.2.0 but [Unreleased] is empty".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub package: String,
    pub message: String,
}

/// Preflight behavior switches supplied by the caller.
#[derive(Debug, Clone)]
pub struct CheckOptions {
    /// Permit publishable packages with no prior matching release tag.
    pub allow_first_release: bool,
    /// Configured git tag formats used to find prior releases.
    pub tag_formats: Vec<String>,
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self {
            allow_first_release: false,
            tag_formats: vec![crate::config::DEFAULT_TAG_FORMAT.to_string()],
        }
    }
}

/// Run the gate. `selected` is the set of package names the user chose to bump (empty when
/// preflight runs before the prompt). Returns every violation found; an empty vec means pass.
pub fn check(
    repo: &dyn RepoState,
    packages: &[Pkg],
    selected: &[String],
) -> Result<Vec<Violation>> {
    check_with_options(repo, packages, selected, CheckOptions::default())
}

/// Run the gate with explicit behavior switches.
pub fn check_with_options(
    repo: &dyn RepoState,
    packages: &[Pkg],
    selected: &[String],
    opts: CheckOptions,
) -> Result<Vec<Violation>> {
    let mut violations = Vec::new();

    for pkg in packages {
        // Private packages may carry commits and need no changelog.
        if !pkg.publishable {
            continue;
        }

        let empty = unreleased_is_empty(&pkg.changelog_path)?;
        let selected_for_bump = selected.iter().any(|name| name == &pkg.name);
        let pkg_dir = pkg.manifest_path.parent().unwrap_or_else(|| Path::new("."));

        let violation = match repo.last_tag(&pkg.name, &opts.tag_formats)? {
            // First releases are explicit so accidentally untagged packages do not slip through.
            None if !opts.allow_first_release => {
                Some("first release requires --first-release".to_string())
            }
            None if empty => Some("first release but [Unreleased] is empty".to_string()),
            None => None,
            Some(tag) => {
                let count = repo.commit_count_since(&tag, pkg_dir)?;
                if empty && count > 0 {
                    Some(format!(
                        "{count} commit(s) since {tag} but [Unreleased] is empty"
                    ))
                } else if empty && selected_for_bump {
                    Some("selected for bump but [Unreleased] is empty".to_string())
                } else {
                    None
                }
            }
        };

        if let Some(message) = violation {
            violations.push(Violation {
                package: pkg.name.clone(),
                message,
            });
        }
    }

    Ok(violations)
}

/// Render violations as the CLI abort block.
pub fn format_violations(violations: &[Violation]) -> String {
    let mut out = String::from("release aborted — preflight violations:\n");
    for v in violations {
        out.push_str(&format!("\n  {}: {}", v.package, v.message));
    }
    out
}

/// A missing changelog counts as empty (the "empty/missing" rule), not an error.
fn unreleased_is_empty(changelog_path: &Path) -> Result<bool> {
    if !changelog_path.exists() {
        return Ok(true);
    }
    Ok(changelog::parse_unreleased(changelog_path)?.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;

    struct FakeRepo {
        tags: HashMap<String, String>,
        counts: HashMap<String, usize>,
    }

    impl RepoState for FakeRepo {
        fn last_tag(&self, pkg_name: &str, _: &[String]) -> Result<Option<String>> {
            Ok(self.tags.get(pkg_name).cloned())
        }
        fn commit_count_since(&self, tag: &str, _pkg_dir: &Path) -> Result<usize> {
            Ok(self.counts.get(tag).copied().unwrap_or(0))
        }
        fn commits_since(&self, _: Option<&str>, _: &Path) -> Result<String> {
            Ok(String::new())
        }
    }

    const EMPTY: &str = "# Changelog\n\n## [Unreleased]\n\n## [1.0.0] - 2024-01-01\n- x\n";
    const WITH_NOTES: &str =
        "# Changelog\n\n## [Unreleased]\n\n### Added\n- y\n\n## [1.0.0] - 2024-01-01\n- x\n";

    fn pkg(dir: &Path, name: &str, publishable: bool, changelog: Option<&str>) -> Pkg {
        let pkg_dir = dir.join(name.replace('/', "_"));
        fs::create_dir_all(&pkg_dir).unwrap();
        let changelog_path = pkg_dir.join("CHANGELOG.md");
        if let Some(content) = changelog {
            fs::write(&changelog_path, content).unwrap();
        }
        Pkg {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            manifest_path: pkg_dir.join("package.json"),
            changelog_path,
            publishable,
            internal_deps: vec![],
        }
    }

    fn messages(violations: &[Violation]) -> HashMap<String, String> {
        violations
            .iter()
            .map(|v| (v.package.clone(), v.message.clone()))
            .collect()
    }

    #[test]
    fn collects_all_violation_kinds_and_skips_clean_and_private() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        let packages = vec![
            pkg(d, "core", true, Some(EMPTY)), // tag + commits + empty -> commits violation
            pkg(d, "utils", true, Some(WITH_NOTES)), // tag + commits + notes -> ok
            pkg(d, "sdk", true, Some(EMPTY)),  // tag, no commits, empty, selected -> selected
            pkg(d, "new", true, Some(EMPTY)),  // no tag -> explicit first-release violation
            pkg(d, "newgood", true, Some(WITH_NOTES)), // no tag -> explicit first-release violation
            pkg(d, "miss", true, None),        // no tag -> explicit first-release violation
            pkg(d, "app", false, Some(EMPTY)), // private -> skipped
        ];

        let repo = FakeRepo {
            tags: HashMap::from([
                ("core".into(), "core@1.2.0".into()),
                ("utils".into(), "utils@0.5.0".into()),
                ("sdk".into(), "sdk@2.0.0".into()),
            ]),
            counts: HashMap::from([
                ("core@1.2.0".into(), 3),
                ("utils@0.5.0".into(), 2),
                ("sdk@2.0.0".into(), 0),
            ]),
        };

        let selected = vec!["sdk".to_string()];
        let violations = check(&repo, &packages, &selected).unwrap();
        let msgs = messages(&violations);

        assert_eq!(violations.len(), 5, "got: {msgs:?}");
        assert_eq!(
            msgs.get("core").unwrap(),
            "3 commit(s) since core@1.2.0 but [Unreleased] is empty"
        );
        assert_eq!(
            msgs.get("sdk").unwrap(),
            "selected for bump but [Unreleased] is empty"
        );
        assert_eq!(
            msgs.get("new").unwrap(),
            "first release requires --first-release"
        );
        assert_eq!(
            msgs.get("newgood").unwrap(),
            "first release requires --first-release"
        );
        assert_eq!(
            msgs.get("miss").unwrap(),
            "first release requires --first-release"
        );
        assert!(!msgs.contains_key("utils"));
        assert!(!msgs.contains_key("app"));
    }

    #[test]
    fn first_release_flag_allows_untagged_packages_but_still_requires_notes() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        let packages = vec![
            pkg(d, "new", true, Some(WITH_NOTES)),
            pkg(d, "miss", true, None),
        ];
        let repo = FakeRepo {
            tags: HashMap::new(),
            counts: HashMap::new(),
        };

        let violations = check_with_options(
            &repo,
            &packages,
            &[],
            CheckOptions {
                allow_first_release: true,
                ..Default::default()
            },
        )
        .unwrap();
        let msgs = messages(&violations);

        assert_eq!(violations.len(), 1, "got: {msgs:?}");
        assert_eq!(
            msgs.get("miss").unwrap(),
            "first release but [Unreleased] is empty"
        );
        assert!(!msgs.contains_key("new"));
    }

    #[test]
    fn clean_repo_has_no_violations() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        let packages = vec![pkg(d, "core", true, Some(WITH_NOTES))];
        let repo = FakeRepo {
            tags: HashMap::from([("core".into(), "core@1.0.0".into())]),
            counts: HashMap::from([("core@1.0.0".into(), 1)]),
        };
        assert!(check(&repo, &packages, &[]).unwrap().is_empty());
    }

    #[test]
    fn format_violations_lists_each_package() {
        let v = vec![
            Violation {
                package: "core".into(),
                message: "boom".into(),
            },
            Violation {
                package: "cli".into(),
                message: "bang".into(),
            },
        ];
        let out = format_violations(&v);
        assert!(out.contains("preflight violations"));
        assert!(out.contains("  core: boom"));
        assert!(out.contains("  cli: bang"));
    }
}
