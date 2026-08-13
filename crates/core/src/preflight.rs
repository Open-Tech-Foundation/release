//! Strict preflight gate — all-or-nothing, runs before any prompt or mutation.
//!
//! For every non-private package, state is derived from its last git tag matching the configured
//! tag format. Missing changelog notes on packages with only ignorable path changes are downgraded
//! to warnings; hard violations still collect *all* failures and exit non-zero before any
//! `release/*` branch is created or any file is written. See `docs/preflight.md`.

use std::path::{Path, PathBuf};

use anyhow::Result;
use glob::Pattern;

use crate::adapter::Pkg;
use crate::changelog;
use crate::git::RepoState;

/// A single preflight failure, e.g. "3 commits since core@1.2.0 but [Unreleased] is empty".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub package: String,
    pub message: String,
}

/// A non-fatal preflight condition that should be surfaced before continuing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub package: String,
    pub message: String,
}

/// The full result of a preflight check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub violations: Vec<Violation>,
    pub warnings: Vec<Warning>,
}

/// Preflight behavior switches supplied by the caller.
#[derive(Debug, Clone)]
pub struct CheckOptions {
    /// Configured git tag formats used to find prior releases, per package.
    pub tag_formats: crate::config::TagFormats,
    /// Per-package glob patterns that should be ignored when classifying path-scoped changes.
    pub ignore_paths: std::collections::HashMap<String, Vec<String>>,
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self {
            tag_formats: crate::config::TagFormats::global(crate::config::DEFAULT_TAG_FORMAT),
            ignore_paths: std::collections::HashMap::new(),
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
    Ok(check_with_options(repo, packages, selected, CheckOptions::default())?.violations)
}

/// Run the gate with explicit behavior switches.
pub fn check_with_options(
    repo: &dyn RepoState,
    packages: &[Pkg],
    selected: &[String],
    opts: CheckOptions,
) -> Result<Report> {
    let mut report = Report::default();

    for pkg in packages {
        // Private packages may carry commits and need no changelog.
        if !pkg.publishable {
            continue;
        }

        let empty = unreleased_is_empty(&pkg.changelog_path)?;
        let selected_for_bump = selected.iter().any(|name| name == &pkg.name);
        let pkg_dir = pkg.manifest_path.parent().unwrap_or_else(|| Path::new("."));

        let history = opts.tag_formats.history_for(&pkg.name);
        let last_tag = repo.last_tag(&pkg.name, &history)?;
        if last_tag.is_none() {
            if let Some(orphan) = orphaned_history(repo, &pkg.name, &history)? {
                report.warnings.push(Warning {
                    package: pkg.name.clone(),
                    message: format!(
                        "no release history under the configured tag format, but `{orphan}` \
                         exists. Changing `tag_format` hides every tag written under the old one: \
                         this package now looks unreleased, so notes are generated from the whole \
                         history and nothing is diffed against its real last release. Add \
                         `legacy_tag_formats = [\"{}\"]` to keep reading them.",
                        orphan_format(&orphan, &pkg.name)
                    ),
                });
            }
        }

        let violation = match last_tag {
            None if empty => Some("first release but [Unreleased] is empty".to_string()),
            None => None,
            Some(tag) => {
                let count = repo.commit_count_since(&tag, pkg_dir)?;
                if empty && count > 0 {
                    let changed_files = repo.changed_files_since(&tag, pkg_dir)?;
                    let ignored = only_ignored_changes(
                        &changed_files,
                        opts.ignore_paths
                            .get(&pkg.name)
                            .map(Vec::as_slice)
                            .unwrap_or(&[]),
                    )?;
                    if ignored {
                        report.warnings.push(Warning {
                            package: pkg.name.clone(),
                            message: format!(
                                "{count} commit(s) since {tag} but [Unreleased] is empty; only ignored paths changed"
                            ),
                        });
                        None
                    } else {
                        Some(format!(
                            "{count} commit(s) since {tag} but [Unreleased] is empty"
                        ))
                    }
                } else if empty && selected_for_bump {
                    Some("selected for bump but [Unreleased] is empty".to_string())
                } else {
                    None
                }
            }
        };

        if let Some(message) = violation {
            report.violations.push(Violation {
                package: pkg.name.clone(),
                message,
            });
        }
    }

    Ok(report)
}

/// Render violations as the CLI abort block.
pub fn format_violations(violations: &[Violation]) -> String {
    let mut out = String::from("release aborted — preflight violations:\n");
    for v in violations {
        out.push_str(&format!("\n  {}: {}", v.package, v.message));
    }
    out
}

/// A tag this package plainly has, written under a format the config no longer reads.
///
/// Only consulted when the configured formats find nothing: a package with history is not
/// interesting, and a genuine first release has no tag under any format either. Checking the
/// built-in formats is enough to catch the case that actually happens — someone edits
/// `tag_format` and every prior release drops out of view with no error and no output.
fn orphaned_history(
    repo: &dyn RepoState,
    pkg_name: &str,
    configured: &[String],
) -> Result<Option<String>> {
    for candidate in crate::config::COMMON_TAG_FORMATS {
        if configured.iter().any(|f| f == candidate) {
            continue;
        }
        if let Some(tag) = repo.last_tag(pkg_name, &[(*candidate).to_string()])? {
            return Ok(Some(tag));
        }
    }
    Ok(None)
}

/// Recover which built-in format produced `tag`, for the fix text.
fn orphan_format(tag: &str, pkg_name: &str) -> String {
    crate::config::COMMON_TAG_FORMATS
        .iter()
        .find(|format| crate::git::version_from_tag(tag, format, pkg_name).is_some())
        .map(|format| (*format).to_string())
        .unwrap_or_default()
}

/// A missing changelog counts as empty (the "empty/missing" rule), not an error.
fn unreleased_is_empty(changelog_path: &Path) -> Result<bool> {
    if !changelog_path.exists() {
        return Ok(true);
    }
    Ok(changelog::parse_unreleased(changelog_path)?.is_empty())
}

/// Whether every file this package changed since `tag` matches one of its `ignore_paths`.
///
/// Shared with the generated-changelog path in `version`, so "this change does not warrant a
/// release" is decided by one rule rather than two that can disagree.
pub fn changes_are_all_ignored(
    repo: &dyn RepoState,
    tag: &str,
    pkg_dir: &Path,
    ignore_paths: &[String],
) -> Result<bool> {
    let changed = repo.changed_files_since(tag, pkg_dir)?;
    only_ignored_changes(&changed, ignore_paths)
}

fn only_ignored_changes(changed_files: &[PathBuf], ignore_paths: &[String]) -> Result<bool> {
    if changed_files.is_empty() {
        return Ok(false);
    }
    if ignore_paths.is_empty() {
        return Ok(false);
    }

    let patterns: Result<Vec<Pattern>, glob::PatternError> =
        ignore_paths.iter().map(|glob| Pattern::new(glob)).collect();
    let patterns = patterns?;

    Ok(changed_files.iter().all(|path| {
        let candidate = path.to_string_lossy();
        patterns.iter().any(|pattern| pattern.matches(&candidate))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;

    struct FakeRepo {
        tags: HashMap<String, String>,
        counts: HashMap<String, usize>,
        files: HashMap<String, Vec<PathBuf>>,
    }

    impl RepoState for FakeRepo {
        fn last_tag(&self, pkg_name: &str, _: &[String]) -> Result<Option<String>> {
            Ok(self.tags.get(pkg_name).cloned())
        }
        fn commit_count_since(&self, tag: &str, _pkg_dir: &Path) -> Result<usize> {
            Ok(self.counts.get(tag).copied().unwrap_or(0))
        }
        fn changed_files_since(&self, tag: &str, _pkg_dir: &Path) -> Result<Vec<PathBuf>> {
            Ok(self.files.get(tag).cloned().unwrap_or_default())
        }
        fn commits_since(&self, _: Option<&str>, _: &Path) -> Result<String> {
            Ok(String::new())
        }
    }

    /// A repo holding real tag strings, matched through the same parser the production code uses.
    /// The other fake keys off the package name alone, which cannot model the thing under test:
    /// whether a *format* finds a tag.
    struct TaggedRepo {
        tags: Vec<String>,
    }

    impl RepoState for TaggedRepo {
        fn last_tag(&self, pkg_name: &str, tag_formats: &[String]) -> Result<Option<String>> {
            Ok(self
                .tags
                .iter()
                .find(|tag| {
                    tag_formats
                        .iter()
                        .any(|f| crate::git::version_from_tag(tag, f, pkg_name).is_some())
                })
                .cloned())
        }
        fn commit_count_since(&self, _: &str, _: &Path) -> Result<usize> {
            Ok(0)
        }
        fn changed_files_since(&self, _: &str, _: &Path) -> Result<Vec<PathBuf>> {
            Ok(Vec::new())
        }
        fn commits_since(&self, _: Option<&str>, _: &Path) -> Result<String> {
            Ok(String::new())
        }
    }

    fn options(format: &str) -> CheckOptions {
        CheckOptions {
            tag_formats: crate::config::TagFormats::global(format),
            ignore_paths: HashMap::new(),
        }
    }

    /// Editing `tag_format` silently orphans every tag written under the old one: `last_tag`
    /// stops matching, the package reads as never released, and notes are generated from the whole
    /// history with nothing to diff against. Nothing errors, so the only way to notice is to be
    /// told.
    #[test]
    fn changing_the_tag_format_warns_that_the_old_tags_are_no_longer_read() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = TaggedRepo {
            tags: vec!["v0.23.0".to_string()],
        };
        let packages = vec![pkg(tmp.path(), "es-runtime-cli", true, Some(WITH_NOTES))];

        let report =
            check_with_options(&repo, &packages, &[], options("{name}@{version}")).unwrap();

        assert!(report.violations.is_empty(), "{report:?}");
        let warning = report
            .warnings
            .iter()
            .find(|w| w.package == "es-runtime-cli")
            .expect("orphaned history is reported");
        assert!(warning.message.contains("v0.23.0"), "{}", warning.message);
        assert!(
            warning
                .message
                .contains(r#"legacy_tag_formats = ["v{version}"]"#),
            "the fix must name the exact line to add: {}",
            warning.message
        );
    }

    /// Declaring the old format is the fix, so declaring it must silence the warning — and the
    /// history must actually be read again, not merely stop complaining.
    #[test]
    fn declaring_the_old_format_as_legacy_restores_the_history() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = TaggedRepo {
            tags: vec!["v0.23.0".to_string()],
        };
        let packages = vec![pkg(tmp.path(), "es-runtime-cli", true, Some(WITH_NOTES))];

        let mut opts = options("{name}@{version}");
        opts.tag_formats = opts.tag_formats.with_legacy(vec!["v{version}".to_string()]);

        let report = check_with_options(&repo, &packages, &[], opts).unwrap();
        assert!(report.warnings.is_empty(), "{report:?}");
    }

    /// A genuine first release has no tag under any format. Warning there would fire on every new
    /// package in the repo, which is exactly the noise that gets a warning ignored.
    #[test]
    fn a_package_that_never_shipped_is_not_reported_as_orphaned() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = TaggedRepo { tags: Vec::new() };
        let packages = vec![pkg(tmp.path(), "brand-new", true, Some(WITH_NOTES))];

        let report = check_with_options(&repo, &packages, &[], options("v{version}")).unwrap();
        assert!(report.warnings.is_empty(), "{report:?}");
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

    fn messages<T>(
        items: &[T],
        package: fn(&T) -> &String,
        message: fn(&T) -> &String,
    ) -> HashMap<String, String> {
        items
            .iter()
            .map(|v| (package(v).clone(), message(v).clone()))
            .collect()
    }

    #[test]
    fn collects_hard_violations_and_downgrades_ignored_only_changes_to_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        let packages = vec![
            pkg(d, "core", true, Some(EMPTY)), // tag + commits + empty -> ignored-only warning
            pkg(d, "engine", true, Some(EMPTY)), // tag + commits + empty -> hard violation
            pkg(d, "utils", true, Some(WITH_NOTES)), // tag + commits + notes -> ok
            pkg(d, "sdk", true, Some(EMPTY)),  // tag, no commits, empty, selected -> selected
            pkg(d, "new", true, Some(EMPTY)), // no tag + empty notes -> first-release notes violation
            pkg(d, "newgood", true, Some(WITH_NOTES)), // no tag + notes -> ok
            pkg(d, "miss", true, None), // no tag + missing notes -> first-release notes violation
            pkg(d, "app", false, Some(EMPTY)), // private -> skipped
        ];

        let repo = FakeRepo {
            tags: HashMap::from([
                ("core".into(), "core@1.2.0".into()),
                ("engine".into(), "engine@1.2.0".into()),
                ("utils".into(), "utils@0.5.0".into()),
                ("sdk".into(), "sdk@2.0.0".into()),
            ]),
            counts: HashMap::from([
                ("core@1.2.0".into(), 3),
                ("engine@1.2.0".into(), 2),
                ("utils@0.5.0".into(), 2),
                ("sdk@2.0.0".into(), 0),
            ]),
            files: HashMap::from([
                (
                    "core@1.2.0".into(),
                    vec![
                        PathBuf::from("docs/guide.md"),
                        PathBuf::from("src/lib.test.ts"),
                    ],
                ),
                ("engine@1.2.0".into(), vec![PathBuf::from("src/lib.rs")]),
            ]),
        };

        let selected = vec!["sdk".to_string()];
        let report = check_with_options(
            &repo,
            &packages,
            &selected,
            CheckOptions {
                tag_formats: crate::config::TagFormats::global(crate::config::DEFAULT_TAG_FORMAT),
                ignore_paths: HashMap::from([(
                    "core".into(),
                    vec!["docs/**".into(), "**/*.test.ts".into()],
                )]),
            },
        )
        .unwrap();
        let msgs = messages(&report.violations, |v| &v.package, |v| &v.message);
        let warns = messages(&report.warnings, |w| &w.package, |w| &w.message);

        assert_eq!(report.violations.len(), 4, "got: {msgs:?}");
        assert_eq!(report.warnings.len(), 1, "got: {warns:?}");
        assert_eq!(
            warns.get("core").unwrap(),
            "3 commit(s) since core@1.2.0 but [Unreleased] is empty; only ignored paths changed"
        );
        assert_eq!(
            msgs.get("engine").unwrap(),
            "2 commit(s) since engine@1.2.0 but [Unreleased] is empty"
        );
        assert_eq!(
            msgs.get("sdk").unwrap(),
            "selected for bump but [Unreleased] is empty"
        );
        assert_eq!(
            msgs.get("new").unwrap(),
            "first release but [Unreleased] is empty"
        );
        assert!(!msgs.contains_key("newgood"));
        assert_eq!(
            msgs.get("miss").unwrap(),
            "first release but [Unreleased] is empty"
        );
        assert!(!msgs.contains_key("core"));
        assert!(!msgs.contains_key("utils"));
        assert!(!msgs.contains_key("app"));
    }

    #[test]
    fn first_release_requires_notes_without_an_explicit_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        let packages = vec![
            pkg(d, "new", true, Some(WITH_NOTES)),
            pkg(d, "miss", true, None),
        ];
        let repo = FakeRepo {
            tags: HashMap::new(),
            counts: HashMap::new(),
            files: HashMap::new(),
        };

        let violations = check(&repo, &packages, &[]).unwrap();
        let msgs = messages(&violations, |v| &v.package, |v| &v.message);

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
            files: HashMap::from([("core@1.0.0".into(), vec![PathBuf::from("src/lib.rs")])]),
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
