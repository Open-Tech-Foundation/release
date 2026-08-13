//! The `doctor` command — a read-only audit of a repo's release setup.
//!
//! Every other command acts at one moment: `check` gates a push, `version` cuts a release,
//! `publish` ships it. The failures that hurt most are the ones none of them can see at that
//! moment — a `release.toml` that parses, generates a workflow, and runs green while quietly
//! shipping nothing. Two packages formatting to the same tag do not error: `github-release` treats
//! an existing release as already shipped and attaches no assets. An npm package with a `build`
//! script but no build step in its block publishes an empty `dist/`. A member glob that matches no
//! directory drops a package from the release entirely.
//!
//! `doctor` looks for exactly those: the setup is inspected against what the adapters actually
//! discover on disk, and every finding names what breaks and how to fix it. It writes nothing.
//!
//! Severity is about consequence, not confidence:
//!
//! - [`Severity::Error`] — a release will silently ship the wrong thing, or not ship at all.
//! - [`Severity::Warning`] — a release will work, but something is off that will bite later.
//! - [`Severity::Suggestion`] — a practice worth adopting; nothing is broken.
//! - [`Severity::Info`] — the resolved facts, so the report is readable without the config open.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;

use crate::adapter::{Adapter, Pkg};
use crate::config::{Ecosystem, ReleaseConfig};
use crate::ui;

/// How much a finding costs if ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    Suggestion,
    Info,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Suggestion => "suggestion",
            Severity::Info => "info",
        }
    }
}

/// One thing `doctor` noticed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    /// Stable kebab-case identifier, so a finding can be talked about and grepped for.
    pub code: &'static str,
    /// The package it concerns, when it concerns one.
    pub package: Option<String>,
    /// What is wrong, and what it costs.
    pub message: String,
    /// The concrete edit that resolves it.
    pub fix: Option<String>,
}

impl Finding {
    fn new(severity: Severity, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity,
            code,
            package: None,
            message: message.into(),
            fix: None,
        }
    }

    fn about(mut self, package: impl Into<String>) -> Self {
        self.package = Some(package.into());
        self
    }

    fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

/// Everything `doctor` found, worst first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub findings: Vec<Finding>,
}

impl Report {
    /// A release built from this setup will misbehave.
    pub fn has_errors(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    pub fn count(&self, severity: Severity) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == severity)
            .count()
    }
}

/// A package as `doctor` sees it: what an adapter discovered, plus the ecosystem that found it.
#[derive(Debug, Clone)]
pub struct Discovered {
    pub ecosystem: Ecosystem,
    pub pkg: Pkg,
    /// The build command the adapter would run, when it detects one (npm's `scripts.build`).
    pub build_command: Option<String>,
}

/// Discover through every enabled adapter, then audit.
pub fn run(
    adapters: &[(Ecosystem, &dyn Adapter)],
    root: &Path,
    config: &ReleaseConfig,
) -> Result<Report> {
    let mut discovered = Vec::new();
    for (ecosystem, adapter) in adapters {
        for pkg in adapter.discover_packages()? {
            let build_command = adapter.build_command(&pkg)?;
            discovered.push(Discovered {
                ecosystem: *ecosystem,
                pkg,
                build_command,
            });
        }
    }
    Ok(audit(config, &discovered, root))
}

/// The whole audit as a pure function of the config, what was discovered, and the files on disk.
pub fn audit(config: &ReleaseConfig, discovered: &[Discovered], root: &Path) -> Report {
    let mut findings = Vec::new();

    // Packages this repo actually releases: discovered, publishable, not deliberately skipped.
    let released: Vec<&Discovered> = discovered
        .iter()
        .filter(|d| d.pkg.publishable && !config.skip_publish.contains(&d.pkg.name))
        .collect();

    tag_collisions(config, &released, &mut findings);
    missing_blocks(config, &released, &mut findings);
    stale_blocks(config, discovered, &mut findings);
    unbuilt_publishes(config, &released, &mut findings);
    empty_discovery_globs(config, discovered, &mut findings);
    changelog_files(config, &released, root, &mut findings);
    matrix_targets(config, &mut findings);
    tool_pin(config, &mut findings);
    supply_chain(config, &released, &mut findings);
    facts(config, &released, &mut findings);

    findings.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| a.code.cmp(b.code))
            .then_with(|| a.package.cmp(&b.package))
    });
    Report { findings }
}

/// Two packages that format to the same tag is the quietest failure this tool has: nothing errors,
/// and the second package's GitHub Release is skipped as "already shipped" with no assets attached.
fn tag_collisions(config: &ReleaseConfig, released: &[&Discovered], out: &mut Vec<Finding>) {
    let tags = config.tag_formats();

    // Same *format* without `{name}`: latent, and independent of what the versions happen to be
    // today. Two packages sharing such a format collide the moment their versions meet.
    let mut by_format: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for d in released {
        by_format
            .entry(tags.for_package(&d.pkg.name))
            .or_default()
            .push(d.pkg.name.as_str());
    }
    for (format, mut names) in by_format {
        if format.contains("{name}") || names.len() < 2 {
            continue;
        }
        names.sort();
        out.push(
            Finding::new(
                Severity::Error,
                "tag-collision",
                format!(
                    "{} packages share the tag format `{format}`, which has no `{{name}}`: {}. \
                     They format to the same tag whenever their versions match, and the second \
                     one's GitHub Release is then skipped as already-shipped — attaching no \
                     assets, with no error.",
                    names.len(),
                    names.join(", ")
                ),
            )
            .fix(
                "give all but one of them a scoped `tag_format = \"{name}@{version}\"` in its \
                 `[[package]]` block, or change the repo-wide `tag_format` to include `{name}`",
            ),
        );
    }

    // Same *tag string* right now: already broken, not merely latent.
    let mut by_tag: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for d in released {
        if let Ok(tag) = tags.tag_for(&d.pkg.name, &d.pkg.version) {
            by_tag.entry(tag).or_default().push(d.pkg.name.as_str());
        }
    }
    for (tag, mut names) in by_tag {
        if names.len() < 2 {
            continue;
        }
        names.sort();
        out.push(
            Finding::new(
                Severity::Error,
                "tag-collision-now",
                format!(
                    "{} packages resolve to the tag `{tag}` at their current versions: {}. \
                     Only the first to reach the forge will ship.",
                    names.len(),
                    names.join(", ")
                ),
            )
            .fix("scope `tag_format` per package so each one owns its own tag line"),
        );
    }
}

/// A released package with no block cannot be configured — no build step, no scoped tag format,
/// no scoped changelog.
fn missing_blocks(config: &ReleaseConfig, released: &[&Discovered], out: &mut Vec<Finding>) {
    for d in released {
        if config.package(&d.pkg.name).is_some() {
            continue;
        }
        out.push(
            Finding::new(
                Severity::Error,
                "missing-package-block",
                format!(
                    "`{}` is released but has no `[[package]]` block, so it has no build step and \
                     nowhere to scope its tag format or changelog.",
                    d.pkg.name
                ),
            )
            .about(&d.pkg.name)
            .fix("run `otf-release config` → Ecosystems and confirm, which writes the missing blocks"),
        );
    }
}

/// A block for a package no adapter finds generates jobs that release nothing.
fn stale_blocks(config: &ReleaseConfig, discovered: &[Discovered], out: &mut Vec<Finding>) {
    let known: HashSet<&str> = discovered.iter().map(|d| d.pkg.name.as_str()).collect();
    for entry in &config.packages {
        // Generic packages are declared by hand; no adapter discovers them.
        if entry.adapter == Ecosystem::Generic || known.contains(entry.name.as_str()) {
            continue;
        }
        let reason = if config.adapters.contains(&entry.adapter) {
            "no enabled adapter discovers it"
        } else {
            "its ecosystem is not enabled"
        };
        out.push(
            Finding::new(
                Severity::Error,
                "stale-package-block",
                format!(
                    "`{}` has a `[[package]]` block but {reason}, so its generated jobs release \
                     nothing.",
                    entry.name
                ),
            )
            .about(&entry.name)
            .fix("remove the block, or fix `[discovery]` / `adapters` so the package is found"),
        );
    }
}

/// The npm trap: a package whose manifest declares a build, configured with none. `npm publish`
/// then packs whatever is on disk — for a package whose `files` points at `dist/`, nothing.
fn unbuilt_publishes(config: &ReleaseConfig, released: &[&Discovered], out: &mut Vec<Finding>) {
    for d in released {
        let Some(command) = &d.build_command else {
            continue;
        };
        let Some(entry) = config.package(&d.pkg.name) else {
            continue; // already reported as a missing block
        };
        if !entry.command.trim().is_empty() {
            continue;
        }
        out.push(
            Finding::new(
                Severity::Error,
                "unbuilt-publish",
                format!(
                    "`{}` declares a build script but its block configures no build command, so it \
                     is published without ever being built.",
                    d.pkg.name
                ),
            )
            .about(&d.pkg.name)
            .fix(format!(
                "set `command = \"{command}\"` in its `[[package]]` block"
            )),
        );
    }
}

/// A declared member directory that matches nothing drops a package silently: `[discovery]` is the
/// member set, and a glob matching nothing is not an error anywhere else.
fn empty_discovery_globs(
    config: &ReleaseConfig,
    discovered: &[Discovered],
    out: &mut Vec<Finding>,
) {
    if config.discovery.npm.is_empty() {
        return;
    }
    let found: Vec<&Pkg> = discovered
        .iter()
        .filter(|d| d.ecosystem == Ecosystem::Npm)
        .map(|d| &d.pkg)
        .collect();
    for pattern in &config.discovery.npm {
        // A pattern contributes if some discovered manifest sits under a path it could name.
        let literal_dir = !pattern.contains('*') && !pattern.contains('?');
        let matched = found.iter().any(|pkg| {
            let manifest = pkg.manifest_path.to_string_lossy().replace('\\', "/");
            if literal_dir {
                manifest.contains(&format!("{pattern}/"))
            } else {
                // A glob's fixed prefix is enough to tell whether it contributed anything.
                let prefix = pattern.split(['*', '?']).next().unwrap_or("");
                prefix.is_empty() || manifest.contains(prefix)
            }
        });
        if !matched {
            out.push(
                Finding::new(
                    Severity::Error,
                    "discovery-matches-nothing",
                    format!(
                        "`[discovery] npm` lists `{pattern}`, which matches no package. Since the \
                         list *is* the member set, anything it was meant to name is not released \
                         at all — and a glob matching nothing raises no error anywhere else."
                    ),
                )
                .fix("correct the path, or drop the entry if the package moved"),
            );
        }
    }
}

/// Curated notes are the source of truth for what ships, so a released package whose changelog file
/// does not exist can never be released: its `[Unreleased]` is empty by definition.
fn changelog_files(
    config: &ReleaseConfig,
    released: &[&Discovered],
    root: &Path,
    out: &mut Vec<Finding>,
) {
    if config.changelog_strategy != crate::config::ChangelogStrategy::Curated {
        return;
    }
    let layout = config.changelog_layout();
    let mut sharing: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    // Collected, not reported one by one: the explanation is identical for every package, so N
    // packages produced N copies of the same paragraph.
    let mut missing: Vec<String> = Vec::new();

    for d in released {
        // In package scope the layout defers to the adapter, whose path is absolute in a real
        // run; resolve a relative one against the root so both shapes check the same file.
        let path = layout.path_for(root, &d.pkg.name).unwrap_or_else(|| {
            if d.pkg.changelog_path.is_absolute() {
                d.pkg.changelog_path.clone()
            } else {
                root.join(&d.pkg.changelog_path)
            }
        });
        if !path.exists() {
            missing.push(format!("{} ({})", d.pkg.name, rel(root, &path)));
        }
        sharing
            .entry(rel(root, &path))
            .or_default()
            .push(d.pkg.name.as_str());
    }

    if !missing.is_empty() {
        missing.sort();
        out.push(
            Finding::new(
                Severity::Warning,
                "missing-changelog",
                format!(
                    "{} package(s) have no changelog: {}. With the curated strategy a package with \
                     no `[Unreleased]` notes is never offered for release.",
                    missing.len(),
                    missing.join(", ")
                ),
            )
            .fix("create each file with a `## [Unreleased]` heading"),
        );
    }

    // Sharing one changelog is correct for a lockstep group and wrong for packages that version
    // independently — their notes end up interleaved in one file under one version heading.
    for (path, mut names) in sharing {
        if names.len() < 2 {
            continue;
        }
        names.sort();
        let versions: HashSet<&str> = released
            .iter()
            .filter(|d| names.contains(&d.pkg.name.as_str()))
            .map(|d| d.pkg.version.as_str())
            .collect();
        if versions.len() < 2 {
            continue; // one version across all of them: a lockstep group, which is the point
        }
        out.push(
            Finding::new(
                Severity::Warning,
                "shared-changelog",
                format!(
                    "{} packages at {} different versions all write release notes into `{path}`: \
                     {}. Independently versioned packages interleave their notes under one \
                     heading.",
                    names.len(),
                    versions.len(),
                    names.join(", ")
                ),
            )
            .fix(
                "set `changelog_scope = \"package\"`, or name a `changelog` in the `[[package]]` \
                 block of the ones that should keep their own",
            ),
        );
    }
}

fn matrix_targets(config: &ReleaseConfig, out: &mut Vec<Finding>) {
    for entry in &config.packages {
        if entry.matrix && entry.targets.is_empty() {
            out.push(
                Finding::new(
                    Severity::Warning,
                    "matrix-without-targets",
                    format!(
                        "`{}` is a matrix build with no `[[package.targets]]`, so its build fans \
                         out to nothing.",
                        entry.name
                    ),
                )
                .about(&entry.name)
                .fix("add targets, or set `matrix = false`"),
            );
        }
    }
}

/// The first release whose `install.sh` reads `OTF_RELEASE_VERSION`. Pin anything older and the
/// generated workflow fetches a script that hardcodes `releases/latest/download`, so CI silently
/// installs whatever shipped most recently instead of the version the repo pinned.
const PIN_HONOURED_FROM: (u64, u64, u64) = (0, 26, 0);

/// `otf_release_version` decides which tool builds every release, so a pin that has quietly stopped
/// meaning what it says is worth naming.
fn tool_pin(config: &ReleaseConfig, out: &mut Vec<Finding>) {
    let Some(pin) = &config.otf_release_version else {
        return; // unset means "the version that generated the workflow", which is fine
    };
    let Some(pinned) = crate::git::parse_semver(pin.trim_start_matches('v')) else {
        out.push(
            Finding::new(
                Severity::Warning,
                "unparseable-tool-pin",
                format!("`otf_release_version = \"{pin}\"` is not a version tag."),
            )
            .fix("use a released tag, e.g. `v0.32.0`"),
        );
        return;
    };

    if pinned < PIN_HONOURED_FROM {
        let (major, minor, patch) = PIN_HONOURED_FROM;
        out.push(
            Finding::new(
                Severity::Error,
                "inert-tool-pin",
                format!(
                    "`otf_release_version = \"{pin}\"` predates v{major}.{minor}.{patch}, whose \
                     installer was the first to read `OTF_RELEASE_VERSION`. The workflow fetches \
                     that older script, which always downloads the *latest* release — so CI does \
                     not build with the pinned version at all, and silently follows whatever ships \
                     next."
                ),
            )
            .fix("raise the pin to a released version, then run `otf-release upgrade --force`"),
        );
        return;
    }

    let running = env!("CARGO_PKG_VERSION");
    if let Some(current) = crate::git::parse_semver(running) {
        if pinned < current {
            out.push(
                Finding::new(
                    Severity::Suggestion,
                    "old-tool-pin",
                    format!(
                        "CI builds releases with `{pin}` while this binary is v{running}, so a \
                         release cut here uses a different tool than the one you tested with."
                    ),
                )
                .fix("raise `otf_release_version`, then run `otf-release upgrade --force`"),
            );
        }
    }
}

/// A downloaded binary is only trustworthy if its origin can be proved. Checksums show it arrived
/// intact; only attestation shows who built it.
fn supply_chain(config: &ReleaseConfig, released: &[&Discovered], out: &mut Vec<Finding>) {
    for entry in config.packages.iter().filter(|p| p.is_build_only()) {
        if !entry.checksums {
            out.push(
                Finding::new(
                    Severity::Suggestion,
                    "no-checksums",
                    format!(
                        "`{}` ships release assets without a `checksums.txt`, so a download \
                         cannot be verified as intact.",
                        entry.name
                    ),
                )
                .about(&entry.name)
                .fix("set `checksums = true` in its `[[package]]` block"),
            );
        }
        if !entry.attest {
            out.push(
                Finding::new(
                    Severity::Suggestion,
                    "no-attestation",
                    format!(
                        "`{}` ships release assets with no build provenance. A checksum can be \
                         replaced by whoever replaced the asset; a signed attestation cannot.",
                        entry.name
                    ),
                )
                .about(&entry.name)
                .fix(
                    "set `attest = true` in its `[[package]]` block, then run `otf-release upgrade \
                     --force` to add the workflow permissions",
                ),
            );
        }
    }

    // A repo-wide legacy format with no `{name}` matches every package that asks. In a repo with
    // one package that is exactly right; with several it silently hands one package's tag history
    // to all of them — including packages that never shipped, which then stop reading as first
    // releases and get bumped from a version they never published.
    if released.len() > 1 {
        let nameless: Vec<&str> = config
            .legacy_tag_formats
            .iter()
            .filter(|format| !format.contains("{name}"))
            .map(String::as_str)
            .collect();
        if !nameless.is_empty() {
            out.push(
                Finding::new(
                    Severity::Warning,
                    "shared-legacy-tag-format",
                    format!(
                        "`legacy_tag_formats` contains {} with no `{{name}}`, and this repo \
                         releases {} packages. Such a format matches any package's tag, so every \
                         package reads the same release history — a package that never shipped \
                         looks released, and is bumped from a version it never published.",
                        nameless
                            .iter()
                            .map(|f| format!("`{f}`"))
                            .collect::<Vec<_>>()
                            .join(", "),
                        released.len()
                    ),
                )
                .fix(
                    "move it into the `[[package]]` block of the package whose tags it actually \
                     wrote, as `legacy_tag_formats`, and drop it from the repo-wide list",
                ),
            );
        }
    }

    // One finding for all of them, not one per package: an empty list is the same mistake with the
    // same fix everywhere it appears, and a repo that has it usually has it for every package —
    // which turned the suggestions section into a wall of identical lines.
    let mut empty: Vec<&str> = config
        .publish
        .ignore_paths
        .iter()
        .filter(|(_, globs)| globs.is_empty())
        .map(|(name, _)| name.as_str())
        .collect();
    // `ignore_paths` is a HashMap, so without this the same config lists its packages in a
    // different order on every run.
    empty.sort_unstable();
    if !empty.is_empty() {
        out.push(
            Finding::new(
                Severity::Suggestion,
                "empty-ignore-paths",
                format!(
                    "`publish.ignore_paths` is an empty list for {} package(s), which does \
                     nothing: {}. A release is then blocked by a README-only or test-only change \
                     with no changelog notes.",
                    empty.len(),
                    empty.join(", ")
                ),
            )
            .fix(
                "list the globs that should not force changelog notes — `**/*.md` plus the \
                 ecosystem's test layout is what a new package now starts with — or remove the \
                 entries",
            ),
        );
    }
}

/// The resolved answers, so the report can be read without the config open beside it.
fn facts(config: &ReleaseConfig, released: &[&Discovered], out: &mut Vec<Finding>) {
    let adapters: Vec<&str> = config.adapters.iter().map(|e| e.label()).collect();
    out.push(Finding::new(
        Severity::Info,
        "setup",
        format!(
            "{} package(s) released across {}; tag format `{}`; {} changelog scope; {} changelog \
             strategy.",
            released.len(),
            if adapters.is_empty() {
                "no enabled ecosystems".to_string()
            } else {
                adapters.join(", ")
            },
            config.tag_format,
            match config.changelog_scope {
                crate::config::ChangelogScope::Root => "root",
                crate::config::ChangelogScope::Package => "package",
            },
            match config.changelog_strategy {
                crate::config::ChangelogStrategy::Curated => "curated",
                crate::config::ChangelogStrategy::Generated => "generated",
            },
        ),
    ));

    let tags = config.tag_formats();
    let mut lines: Vec<String> = released
        .iter()
        .map(|d| {
            let tag = tags
                .tag_for(&d.pkg.name, &d.pkg.version)
                .unwrap_or_else(|_| "<invalid tag format>".to_string());
            let mode = match config.package(&d.pkg.name) {
                Some(entry) if entry.is_build_only() => "build-only",
                Some(entry) if !entry.command.trim().is_empty() => "build + publish",
                Some(_) => "publish",
                None => "publish (no block)",
            };
            format!("{} {} → {tag} ({mode})", d.pkg.name, d.pkg.version)
        })
        .collect();
    lines.sort();
    for line in lines {
        out.push(Finding::new(Severity::Info, "package", line));
    }
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Render a report for the terminal. Grouped by severity, worst first, with a one-line tally.
///
/// Styling is baked into the returned string rather than printed here, so the caller can send it
/// through `anstream` — which strips the escape sequences when stdout is a pipe. A `doctor` run
/// redirected to a file or read by CI stays plain text.
pub fn render(report: &Report) -> String {
    let mut s = String::new();
    let groups = [
        (Severity::Error, "Errors", ui::DANGER, ui::DANGER_MARK),
        (Severity::Warning, "Warnings", ui::WARN, ui::WARN_MARK),
        (Severity::Suggestion, "Suggestions", ui::INFO, ui::INFO_MARK),
        (Severity::Info, "Info", ui::DIM, "·"),
    ];

    for (severity, heading, style, mark) in groups {
        let group: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.severity == severity)
            .collect();
        if group.is_empty() {
            continue;
        }
        // The heading carries the severity's colour, so a wall of findings is scannable by band
        // rather than by reading every line.
        s.push_str(&format!(
            "\n{}\n",
            ui::paint(style.bold(), &format!("{heading} ({})", group.len()))
        ));
        for finding in group {
            // The code is what you grep for and what the docs index; give it the weight.
            s.push_str(&format!(
                "  {} {} {}\n",
                ui::paint(style, mark),
                ui::paint(ui::BOLD, finding.code),
                finding.message
            ));
            if let Some(fix) = &finding.fix {
                s.push_str(&format!("      {} {fix}\n", ui::paint(ui::DIM, "fix:")));
            }
        }
    }

    let (errors, warnings, suggestions) = (
        report.count(Severity::Error),
        report.count(Severity::Warning),
        report.count(Severity::Suggestion),
    );
    // The tally takes the colour of the worst thing in it, so the last line of a long report is
    // the verdict and not just arithmetic.
    let verdict = if errors > 0 {
        ui::DANGER
    } else if warnings > 0 {
        ui::WARN
    } else {
        ui::DIM
    };
    s.push_str(&format!(
        "\n{}\n",
        ui::paint(
            verdict,
            &format!("{errors} error(s), {warnings} warning(s), {suggestions} suggestion(s).")
        )
    ));
    if errors == 0 && warnings == 0 {
        s.push_str(&format!(
            "{} {}\n",
            ui::paint(ui::OK, ui::OK_MARK),
            "Release setup looks healthy."
        ));
    }
    s
}

/// Counts by severity, for a caller that wants them without walking the findings.
pub fn tally(report: &Report) -> HashMap<Severity, usize> {
    let mut counts = HashMap::new();
    for finding in &report.findings {
        *counts.entry(finding.severity).or_insert(0) += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ChangelogScope, Discovery, Mode, PackageEntry, Target};
    use std::path::PathBuf;

    fn pkg(name: &str, version: &str, manifest: &str) -> Pkg {
        Pkg {
            name: name.to_string(),
            version: version.to_string(),
            manifest_path: PathBuf::from(manifest),
            changelog_path: PathBuf::from(manifest)
                .parent()
                .unwrap_or(Path::new("."))
                .join("CHANGELOG.md"),
            publishable: true,
            internal_deps: Vec::new(),
        }
    }

    fn found(ecosystem: Ecosystem, pkg: Pkg, build: Option<&str>) -> Discovered {
        Discovered {
            ecosystem,
            pkg,
            build_command: build.map(str::to_string),
        }
    }

    fn entry(name: &str, ecosystem: Ecosystem) -> PackageEntry {
        PackageEntry {
            name: name.to_string(),
            adapter: ecosystem,
            mode: Mode::Publish,
            matrix: false,
            targets: Vec::new(),
            command: String::new(),
            artifacts: String::new(),
            bin_name: None,
            compress: None,
            manifest: None,
            version_field: None,
            publish: None,
            archive: None,
            checksums: false,
            attest: false,
            include: Vec::new(),
            executable: None,
            tag_format: None,
            legacy_tag_formats: Vec::new(),
            changelog: None,
        }
    }

    fn codes(report: &Report, severity: Severity) -> Vec<&str> {
        report
            .findings
            .iter()
            .filter(|f| f.severity == severity)
            .map(|f| f.code)
            .collect()
    }

    /// The mistake this catches is one I walked a user into: `legacy_tag_formats = ["v{version}"]`
    /// looks like it restores one crate's history and silently gives it to every crate in the repo.
    #[test]
    fn flags_a_repo_wide_legacy_format_that_every_package_would_match() {
        let mut config = ReleaseConfig {
            tag_format: "{name}@{version}".to_string(),
            legacy_tag_formats: vec!["v{version}".to_string()],
            ..ReleaseConfig::default()
        };
        config.packages = vec![
            entry("cli", Ecosystem::Cargo),
            entry("dev-cli", Ecosystem::Cargo),
        ];
        let discovered = vec![
            found(
                Ecosystem::Cargo,
                pkg("cli", "0.23.0", "crates/cli/Cargo.toml"),
                None,
            ),
            found(
                Ecosystem::Cargo,
                pkg("dev-cli", "0.1.0", "crates/dev-cli/Cargo.toml"),
                None,
            ),
        ];

        let report = audit(&config, &discovered, Path::new("/repo"));
        assert!(
            codes(&report, Severity::Warning).contains(&"shared-legacy-tag-format"),
            "{report:?}"
        );

        // Scoping it to the package that owned those tags is the fix, and silences it.
        config.legacy_tag_formats.clear();
        config.packages[0].legacy_tag_formats = vec!["v{version}".to_string()];
        let report = audit(&config, &discovered, Path::new("/repo"));
        assert!(
            !codes(&report, Severity::Warning).contains(&"shared-legacy-tag-format"),
            "{report:?}"
        );
    }

    /// A single-package repo is exactly where a nameless legacy format is correct.
    #[test]
    fn a_single_package_repo_may_use_a_nameless_legacy_format() {
        let mut config = ReleaseConfig {
            tag_format: "{name}@{version}".to_string(),
            legacy_tag_formats: vec!["v{version}".to_string()],
            ..ReleaseConfig::default()
        };
        config.packages = vec![entry("cli", Ecosystem::Cargo)];
        let discovered = vec![found(
            Ecosystem::Cargo,
            pkg("cli", "0.23.0", "crates/cli/Cargo.toml"),
            None,
        )];

        let report = audit(&config, &discovered, Path::new("/repo"));
        assert!(!codes(&report, Severity::Warning).contains(&"shared-legacy-tag-format"));
    }

    #[test]
    fn flags_packages_that_share_a_nameless_tag_line() {
        let config = ReleaseConfig {
            tag_format: "v{version}".to_string(),
            packages: vec![
                entry("esrun", Ecosystem::Cargo),
                entry("@x/driver", Ecosystem::Npm),
            ],
            ..ReleaseConfig::default()
        };
        let discovered = vec![
            found(Ecosystem::Cargo, pkg("esrun", "0.23.0", "Cargo.toml"), None),
            found(
                Ecosystem::Npm,
                pkg("@x/driver", "0.0.1", "packages/driver/package.json"),
                None,
            ),
        ];

        let report = audit(&config, &discovered, Path::new("/repo"));
        assert!(
            codes(&report, Severity::Error).contains(&"tag-collision"),
            "{report:?}"
        );

        // Scoping one of them off the shared line resolves it.
        let mut scoped = config;
        scoped.packages[1].tag_format = Some("{name}@{version}".to_string());
        let report = audit(&scoped, &discovered, Path::new("/repo"));
        assert!(
            !codes(&report, Severity::Error).contains(&"tag-collision"),
            "{report:?}"
        );
    }

    #[test]
    fn flags_two_packages_already_resolving_to_one_tag() {
        let config = ReleaseConfig {
            tag_format: "v{version}".to_string(),
            packages: vec![
                entry("esrun", Ecosystem::Cargo),
                entry("esdev", Ecosystem::Cargo),
            ],
            ..ReleaseConfig::default()
        };
        // Both inherit one lockstep workspace version — the second binary's release is skipped.
        let discovered = vec![
            found(
                Ecosystem::Cargo,
                pkg("esrun", "0.24.0", "crates/cli/Cargo.toml"),
                None,
            ),
            found(
                Ecosystem::Cargo,
                pkg("esdev", "0.24.0", "crates/dev/Cargo.toml"),
                None,
            ),
        ];

        let report = audit(&config, &discovered, Path::new("/repo"));
        let errors = codes(&report, Severity::Error);
        assert!(errors.contains(&"tag-collision-now"), "{report:?}");
    }

    #[test]
    fn flags_a_released_package_with_no_block_and_an_unbuilt_publish() {
        let config = ReleaseConfig {
            tag_format: "{name}@{version}".to_string(),
            packages: vec![PackageEntry {
                // Declares a build script, configured with no command.
                ..entry("@x/driver", Ecosystem::Npm)
            }],
            ..ReleaseConfig::default()
        };
        let discovered = vec![
            found(
                Ecosystem::Npm,
                pkg("@x/driver", "0.0.1", "packages/driver/package.json"),
                Some("npm run build"),
            ),
            found(
                Ecosystem::Npm,
                pkg("@x/types", "0.1.0", "packages/types/package.json"),
                None,
            ),
        ];

        let report = audit(&config, &discovered, Path::new("/repo"));
        let errors = codes(&report, Severity::Error);
        assert!(errors.contains(&"unbuilt-publish"), "{report:?}");
        assert!(errors.contains(&"missing-package-block"), "{report:?}");
        // The fix names the exact command the adapter detected.
        let unbuilt = report
            .findings
            .iter()
            .find(|f| f.code == "unbuilt-publish")
            .unwrap();
        assert_eq!(
            unbuilt.fix.as_deref(),
            Some("set `command = \"npm run build\"` in its `[[package]]` block")
        );
    }

    #[test]
    fn flags_a_declared_member_directory_that_matches_nothing() {
        let config = ReleaseConfig {
            tag_format: "{name}@{version}".to_string(),
            adapters: vec![Ecosystem::Npm],
            discovery: Discovery {
                npm: vec![
                    "packages/postgres".to_string(),
                    "packages/types".to_string(),
                ],
            },
            packages: vec![entry("@x/postgres", Ecosystem::Npm)],
            ..ReleaseConfig::default()
        };
        // `packages/types` was moved and the declaration was left behind.
        let discovered = vec![found(
            Ecosystem::Npm,
            pkg("@x/postgres", "0.0.1", "packages/postgres/package.json"),
            None,
        )];

        let report = audit(&config, &discovered, Path::new("/repo"));
        let finding = report
            .findings
            .iter()
            .find(|f| f.code == "discovery-matches-nothing")
            .expect("the stale glob must be reported");
        assert!(finding.message.contains("packages/types"), "{finding:?}");
    }

    #[test]
    fn flags_a_stale_block_and_a_matrix_with_no_targets() {
        let config = ReleaseConfig {
            tag_format: "{name}@{version}".to_string(),
            adapters: vec![Ecosystem::Cargo],
            packages: vec![
                PackageEntry {
                    matrix: true,
                    mode: Mode::BuildOnly,
                    ..entry("esrun", Ecosystem::Cargo)
                },
                entry("@x/gone", Ecosystem::Npm),
            ],
            ..ReleaseConfig::default()
        };
        let discovered = vec![found(
            Ecosystem::Cargo,
            pkg("esrun", "0.24.0", "crates/cli/Cargo.toml"),
            None,
        )];

        let report = audit(&config, &discovered, Path::new("/repo"));
        assert!(
            codes(&report, Severity::Error).contains(&"stale-package-block"),
            "{report:?}"
        );
        assert!(
            codes(&report, Severity::Warning).contains(&"matrix-without-targets"),
            "{report:?}"
        );
    }

    #[test]
    fn a_lockstep_group_sharing_one_changelog_is_not_a_warning() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("CHANGELOG.md"), "# Changelog\n").unwrap();

        let config = ReleaseConfig {
            tag_format: "v{version}".to_string(),
            changelog_scope: ChangelogScope::Root,
            packages: vec![entry("a", Ecosystem::Cargo), entry("b", Ecosystem::Cargo)],
            ..ReleaseConfig::default()
        };
        // One workspace version across both: sharing the root changelog is the whole point.
        let lockstep = vec![
            found(
                Ecosystem::Cargo,
                pkg("a", "0.24.0", "crates/a/Cargo.toml"),
                None,
            ),
            found(
                Ecosystem::Cargo,
                pkg("b", "0.24.0", "crates/b/Cargo.toml"),
                None,
            ),
        ];
        let report = audit(&config, &lockstep, tmp.path());
        assert!(
            !codes(&report, Severity::Warning).contains(&"shared-changelog"),
            "{report:?}"
        );

        // Different versions in one file is the case worth flagging.
        let independent = vec![
            found(
                Ecosystem::Cargo,
                pkg("a", "0.24.0", "crates/a/Cargo.toml"),
                None,
            ),
            found(
                Ecosystem::Npm,
                pkg("b", "0.0.1", "packages/b/package.json"),
                None,
            ),
        ];
        let report = audit(&config, &independent, tmp.path());
        assert!(
            codes(&report, Severity::Warning).contains(&"shared-changelog"),
            "{report:?}"
        );
    }

    #[test]
    fn suggests_supply_chain_settings_for_build_only_assets() {
        let config = ReleaseConfig {
            tag_format: "{name}@{version}".to_string(),
            packages: vec![PackageEntry {
                mode: Mode::BuildOnly,
                matrix: true,
                targets: vec![Target {
                    name: "linux".to_string(),
                    arch: "x86_64".to_string(),
                    ..Target::default()
                }],
                ..entry("esrun", Ecosystem::Cargo)
            }],
            ..ReleaseConfig::default()
        };
        let discovered = vec![found(
            Ecosystem::Cargo,
            pkg("esrun", "0.24.0", "crates/cli/Cargo.toml"),
            None,
        )];

        let report = audit(&config, &discovered, Path::new("/repo"));
        let suggestions = codes(&report, Severity::Suggestion);
        assert!(suggestions.contains(&"no-checksums"), "{report:?}");
        assert!(suggestions.contains(&"no-attestation"), "{report:?}");
    }

    #[test]
    fn flags_a_pin_too_old_to_be_honoured_by_its_own_installer() {
        let config = ReleaseConfig {
            tag_format: "{name}@{version}".to_string(),
            otf_release_version: Some("v0.25.0".to_string()),
            ..ReleaseConfig::default()
        };
        let report = audit(&config, &[], Path::new("/repo"));
        let finding = report
            .findings
            .iter()
            .find(|f| f.code == "inert-tool-pin")
            .expect("a pin the installer ignores must be an error");
        assert_eq!(finding.severity, Severity::Error);

        // A pin from the era that reads the env var is honoured; at most it is merely behind.
        let honoured = ReleaseConfig {
            otf_release_version: Some("v0.26.0".to_string()),
            ..config.clone()
        };
        let report = audit(&honoured, &[], Path::new("/repo"));
        assert!(
            !codes(&report, Severity::Error).contains(&"inert-tool-pin"),
            "{report:?}"
        );
        assert!(
            codes(&report, Severity::Suggestion).contains(&"old-tool-pin"),
            "{report:?}"
        );

        // No pin at all is the default and says nothing.
        let unpinned = ReleaseConfig {
            otf_release_version: None,
            ..config
        };
        let report = audit(&unpinned, &[], Path::new("/repo"));
        assert!(
            !report.findings.iter().any(|f| f.code.ends_with("tool-pin")),
            "{report:?}"
        );
    }

    #[test]
    fn a_healthy_setup_reports_facts_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("packages/driver")).unwrap();
        std::fs::write(
            tmp.path().join("packages/driver/CHANGELOG.md"),
            "# Changelog\n\n## [Unreleased]\n",
        )
        .unwrap();

        let config = ReleaseConfig {
            tag_format: "{name}@{version}".to_string(),
            adapters: vec![Ecosystem::Npm],
            changelog_scope: ChangelogScope::Package,
            packages: vec![PackageEntry {
                command: "npm run build".to_string(),
                ..entry("@x/driver", Ecosystem::Npm)
            }],
            ..ReleaseConfig::default()
        };
        let discovered = vec![found(
            Ecosystem::Npm,
            pkg("@x/driver", "0.0.1", "packages/driver/package.json"),
            Some("npm run build"),
        )];

        let report = audit(&config, &discovered, tmp.path());
        assert!(!report.has_errors(), "{report:?}");
        assert_eq!(report.count(Severity::Warning), 0, "{report:?}");
        let rendered = render(&report);
        assert!(
            rendered.contains("Release setup looks healthy."),
            "{rendered}"
        );
        assert!(
            rendered.contains("@x/driver 0.0.1 → @x/driver@0.0.1"),
            "{rendered}"
        );
    }
}
