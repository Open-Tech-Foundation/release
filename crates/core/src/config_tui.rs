//! `release config` — a full-screen settings editor.
//!
//! The old editor was a chain of `inquire` prompts: a menu asked which area, another asked which
//! setting, a third asked for the value. You could not see what anything was currently set to
//! without opening it, and a nine-item menu rendered in a seven-row window, so half the options
//! were behind scroll arrows.
//!
//! This screen shows every setting and its current value at once. Navigation never leaves the
//! screen; editing opens a modal over it and closes back to the same row.
//!
//! Structure follows [`crate::review`]: [`build`] turns the config into a list of entries and is
//! pure, so the whole model is testable without a terminal; [`run`] is a thin event loop.

use std::path::{Path, PathBuf};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::config::{
    format_tag, is_env_name, ArchiveFormat, ChangelogScope, ChangelogStrategy, Ecosystem,
    GithubReleaseNotes, JobKind, Mode, PackageEntry, ReleaseConfig, Setup, SetupSteps, Target,
    COMMON_TAG_FORMATS, CONFIG_FILE, DEFAULT_VERSION_FIELD, TARGET_REGISTRY,
};
use crate::init::{
    adopt_package, sync_package_blocks, unconfigured_packages, AdapterFactory, UnconfiguredPackage,
};
use crate::ui::ACCENT_RGB as ACCENT;

/// Width of the label column, so values line up in one column the eye can run down.
const LABEL_WIDTH: usize = 26;

/// Which setting a row edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    ToolVersion,
    NpmSecret,
    CargoSecret,
    DiscoveryNpm,
    OpenIgnorePaths,
    IgnorePaths(String),
    AddIgnorePaths,
    AddPackage,
    OpenTargets,
    OpenTarget(usize),
    AddTarget,
    RemoveTarget(usize),
    Target(usize, TargetPart),
    RestoreSetup,
    PkgName,
    PkgAdapter,
    PkgMatrix,
    PkgBinName,
    PkgCompress,
    PkgArchive,
    PkgInclude,
    PkgEnv,
    PkgExecutable,
    PkgLegacyTags,
    Provider,
    DefaultBranch,
    TagFormat,
    LegacyTagFormats,
    SnapshotTag,
    ChangelogScope,
    ChangelogStrategy,
    GithubReleaseNotes,
    Ecosystems,
    SkipPublish,
    Hook(HookStage),
    /// Open the detail view for one step of a setup list.
    OpenSetupStep(SetupScope, usize),
    /// Append a blank step to a setup list and open it.
    SetupAdd(SetupScope),
    /// One field of the step the detail view is showing.
    Setup(SetupScope, usize, SetupPart),
    /// Drop the step the detail view is showing.
    SetupRemove(SetupScope, usize),
    /// Open the detail view for a configured package.
    OpenPackage(String),
    /// Decide whether a package the repo has but `release.toml` does not is released or skipped.
    AdoptPackage(String),
    PkgMode,
    PkgCommand,
    PkgArtifacts,
    PkgTargets,
    PkgChecksums,
    PkgAttest,
    PkgProvenance,
    PkgTagFormat,
    PkgChangelog,
    PkgManifest,
    PkgVersionField,
    PkgPublishCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetPart {
    Name,
    Arch,
    Triple,
    Runner,
    StageAs,
    Ext,
    Cross,
    Vm,
}

impl TargetPart {
    const ALL: [Self; 8] = [
        Self::Name,
        Self::Arch,
        Self::Triple,
        Self::Runner,
        Self::StageAs,
        Self::Ext,
        Self::Cross,
        Self::Vm,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Name => "OS name",
            Self::Arch => "Architecture",
            Self::Triple => "Rust triple",
            Self::Runner => "Runner",
            Self::StageAs => "Stage directory",
            Self::Ext => "Extension",
            Self::Cross => "Cross compile",
            Self::Vm => "Build in VM",
        }
    }
}

/// Which setup list a row edits. The package's name travels with it, so a step's rows resolve
/// without consulting the current view — the step detail view is not the package view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupScope {
    Repo,
    Package(String),
}

impl SetupScope {
    /// The heading the step detail view opens under, naming the list the step belongs to.
    fn heading(&self) -> String {
        match self {
            SetupScope::Repo => "Build setup".to_string(),
            SetupScope::Package(name) => format!("{name} · build setup"),
        }
    }
}

/// Which part of one setup step a row edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupPart {
    Uses,
    With,
    Run,
    Targets,
    Jobs,
}

impl SetupPart {
    fn label(self) -> &'static str {
        match self {
            SetupPart::Uses => "Action",
            SetupPart::With => "Action inputs",
            SetupPart::Run => "Script",
            SetupPart::Targets => "Targets",
            SetupPart::Jobs => "Jobs",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            SetupPart::Uses => "an action run before the build, e.g. ./.github/actions/setup-tsr",
            SetupPart::With => "inputs for the action above, as key=value pairs",
            SetupPart::Run => "shell commands run as one step, one command per entry",
            SetupPart::Targets => {
                "triples this step is for, comma-separated; blank runs it on every matrix row"
            }
            SetupPart::Jobs => {
                "kinds of job this step runs in, e.g. build; none picked runs it in every job"
            }
        }
    }

    const ALL: [SetupPart; 5] = [
        SetupPart::Uses,
        SetupPart::With,
        SetupPart::Run,
        SetupPart::Targets,
        SetupPart::Jobs,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookStage {
    PreVersion,
    PostVersion,
    PrePublish,
    PostPublish,
}

impl HookStage {
    fn label(self) -> &'static str {
        match self {
            HookStage::PreVersion => "pre_version",
            HookStage::PostVersion => "post_version",
            HookStage::PrePublish => "pre_publish",
            HookStage::PostPublish => "post_publish",
        }
    }

    const ALL: [HookStage; 4] = [
        HookStage::PreVersion,
        HookStage::PostVersion,
        HookStage::PrePublish,
        HookStage::PostPublish,
    ];
}

/// One selectable line: what it is called, what it is set to now, and what Enter opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    pub value: String,
    pub field: Field,
    /// Shown in the footer while this row is focused — the thing the old UI had nowhere to put.
    pub hint: &'static str,
}

/// A rendered line of the screen. Headers are not selectable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Header(String),
    Row(Row),
}

/// Which view the screen is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    Settings,
    IgnorePaths,
    Targets(String),
    Target(String, usize),
    /// A package's own fields, by name — names survive the re-sort that adopting a package causes.
    Package(String),
    /// One setup step's fields. A step has four of them, which is more than a settings screen
    /// should spend on each entry of a list that can grow — so a step gets a view, the way a
    /// package does, and the list above it stays one line per step.
    SetupStep(SetupScope, usize),
}

impl View {
    /// Where Esc goes from here. A step opened from a package's screen returns to it, not to the
    /// top, so backing out retraces the way in.
    fn parent(&self) -> Option<View> {
        match self {
            View::Settings => None,
            View::IgnorePaths => Some(View::Settings),
            View::Targets(name) => Some(View::Package(name.clone())),
            View::Target(name, _) => Some(View::Targets(name.clone())),
            View::Package(_) => Some(View::Settings),
            View::SetupStep(SetupScope::Repo, _) => Some(View::Settings),
            View::SetupStep(SetupScope::Package(name), _) => Some(View::Package(name.clone())),
        }
    }
}

fn row(label: &str, value: String, field: Field, hint: &'static str) -> Entry {
    Entry::Row(Row {
        label: label.to_string(),
        value,
        field,
        hint,
    })
}

fn or_inherit(value: Option<&String>, inherited: &str) -> String {
    match value {
        Some(v) => v.clone(),
        None => format!("(repo default: {inherited})"),
    }
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "(none)".to_string()
    } else {
        items.join(", ")
    }
}

fn ecosystem_label(eco: Ecosystem) -> &'static str {
    match eco {
        Ecosystem::Npm => "npm",
        Ecosystem::Cargo => "crates.io",
        Ecosystem::Jsr => "jsr",
        Ecosystem::Generic => "generic",
    }
}

/// Turn the config into everything the screen shows. Pure — no terminal, no I/O.
pub fn build(config: &ReleaseConfig, view: &View, new_packages: &[String]) -> Vec<Entry> {
    match view {
        View::Settings => settings_entries(config, new_packages),
        View::IgnorePaths => ignore_paths_entries(config),
        View::Targets(name) => targets_entries(config, name),
        View::Target(name, index) => target_entries(config, name, *index),
        View::Package(name) => package_entries(config, name),
        View::SetupStep(scope, index) => setup_step_entries(config, scope, *index),
    }
}

fn settings_entries(config: &ReleaseConfig, new_packages: &[String]) -> Vec<Entry> {
    let mut out = vec![Entry::Header("Repository".into())];
    out.push(row(
        "Provider",
        config.provider.clone(),
        Field::Provider,
        "the git host releases are cut on",
    ));
    out.push(row(
        "Default branch",
        config.default_branch.clone(),
        Field::DefaultBranch,
        "the branch a release is cut from and returned to",
    ));
    out.push(row(
        "Tag format",
        config.tag_format.clone(),
        Field::TagFormat,
        "how release tags are named; needs {name} when packages version independently",
    ));
    out.push(row(
        "Legacy tag formats",
        list_or_none(&config.legacy_tag_formats),
        Field::LegacyTagFormats,
        "older formats still read as release history; new tags never use them",
    ));
    out.push(row(
        "Snapshot tag",
        config
            .snapshot_tag
            .clone()
            .unwrap_or_else(|| "(none)".into()),
        Field::SnapshotTag,
        "prerelease channel for per-commit snapshot publishes",
    ));

    out.push(row(
        "Tool version",
        config
            .otf_release_version
            .clone()
            .unwrap_or_else(|| "(generating version)".into()),
        Field::ToolVersion,
        "version of release installed by generated workflows; blank uses the generating version",
    ));
    out.push(Entry::Header("Registry secrets".into()));
    out.push(row(
        "npm token secret",
        config.secrets.npm.clone(),
        Field::NpmSecret,
        "repository secret name; enter the name, not the token",
    ));
    out.push(row(
        "Cargo token secret",
        config.secrets.cargo.clone(),
        Field::CargoSecret,
        "repository secret name; enter the name, not the token",
    ));
    out.push(Entry::Header("Changelog".into()));
    out.push(row(
        "Scope",
        match config.changelog_scope {
            ChangelogScope::Root => "root".into(),
            ChangelogScope::Package => "package".into(),
        },
        Field::ChangelogScope,
        "one CHANGELOG.md at the root, or one per package",
    ));
    out.push(row(
        "Strategy",
        match config.changelog_strategy {
            ChangelogStrategy::Curated => "curated".into(),
            ChangelogStrategy::Generated => "generated".into(),
        },
        Field::ChangelogStrategy,
        "curated: you write [Unreleased]. generated: built from commit subjects",
    ));
    out.push(row(
        "GitHub Release notes",
        match config.github_release_notes {
            GithubReleaseNotes::AutoGenerate => "auto-generate".into(),
            GithubReleaseNotes::CuratedChangelog => "curated-changelog".into(),
            GithubReleaseNotes::SemanticCommits => "semantic-commits".into(),
        },
        Field::GithubReleaseNotes,
        "where a build-only package's release body comes from",
    ));

    out.push(Entry::Header("Ecosystems".into()));
    out.push(row(
        "Enabled",
        if config.adapters.is_empty() {
            "(none)".into()
        } else {
            config
                .adapters
                .iter()
                .map(|e| ecosystem_label(*e))
                .collect::<Vec<_>>()
                .join(", ")
        },
        Field::Ecosystems,
        "which adapters discover and publish packages here",
    ));
    out.push(row(
        "Never publish",
        list_or_none(&config.skip_publish),
        Field::SkipPublish,
        "packages this repo must never version or publish",
    ));

    out.push(row("npm package directories", list_or_none(&config.discovery.npm), Field::DiscoveryNpm, "directory globs for npm packages in repos without a native npm workspace; empty uses native discovery"));
    out.push(row(
        "Publish ignore paths",
        format!("{} package(s)", config.publish.ignore_paths.len()),
        Field::OpenIgnorePaths,
        "path globs ignored when checking for commits without release notes",
    ));
    out.push(Entry::Header("Hooks".into()));
    for stage in HookStage::ALL {
        let commands = hook_commands(config, stage);
        out.push(row(
            stage.label(),
            list_or_none(commands),
            Field::Hook(stage),
            "shell commands run around the release, one command per entry",
        ));
    }

    out.push(Entry::Header("Build setup".into()));
    setup_rows(&config.setup, SetupScope::Repo, false, &mut out);

    out.push(Entry::Header(format!(
        "Packages ({})",
        config.packages.len() + new_packages.len()
    )));
    for pkg in &config.packages {
        let mode = match pkg.mode {
            Mode::Publish => "publish",
            Mode::BuildOnly => "build-only",
        };
        let matrix = if pkg.matrix {
            format!(", matrix ×{}", pkg.targets.len())
        } else {
            String::new()
        };
        out.push(row(
            &pkg.name,
            format!("{} · {mode}{matrix}", ecosystem_label(pkg.adapter)),
            Field::OpenPackage(pkg.name.clone()),
            "enter to edit this package's build and release identity",
        ));
    }
    for name in new_packages {
        out.push(row(
            name,
            "[new] in this repo, not in release.toml".into(),
            Field::AdoptPackage(name.clone()),
            "enter to release it or skip it for good — nothing is written until you choose",
        ));
    }

    out.push(row("Add package", String::new(), Field::AddPackage, "create a package block for a project discovery cannot find; choose its adapter in the package screen"));
    out
}

fn ignore_paths_entries(config: &ReleaseConfig) -> Vec<Entry> {
    let mut out = vec![Entry::Header("Publish ignore paths".into())];
    let mut names = config
        .publish
        .ignore_paths
        .keys()
        .cloned()
        .chain(config.packages.iter().map(|p| p.name.clone()))
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    for name in names {
        out.push(row(
            &name,
            list_or_none(config.publish_ignore_paths_for(&name)),
            Field::IgnorePaths(name.clone()),
            "one path glob per entry; empty removes this package's ignore policy",
        ));
    }
    out.push(row(
        "Add policy",
        String::new(),
        Field::AddIgnorePaths,
        "add an ignore policy for another package name",
    ));
    out
}

fn targets_entries(config: &ReleaseConfig, name: &str) -> Vec<Entry> {
    let mut out = vec![Entry::Header(format!("{name} · target details"))];
    if let Some(pkg) = config.package(name) {
        for (index, target) in pkg.targets.iter().enumerate() {
            out.push(row(
                &target_label(&target.name, &target.arch),
                format!("{} · {}", target.triple(), target.runner()),
                Field::OpenTarget(index),
                "edit this target's triple, runner, staging, extension, and build flags",
            ));
        }
    }
    out.push(row(
        "Add target",
        String::new(),
        Field::AddTarget,
        "create a custom target definition",
    ));
    out
}

fn target_entries(config: &ReleaseConfig, name: &str, index: usize) -> Vec<Entry> {
    let mut out = vec![Entry::Header(format!("{name} · target {}", index + 1))];
    if let Some(target) = config.package(name).and_then(|p| p.targets.get(index)) {
        for part in TargetPart::ALL {
            let value = match part {
                TargetPart::Name => target.name.clone(),
                TargetPart::Arch => target.arch.clone(),
                TargetPart::Triple => target.triple.clone(),
                TargetPart::Runner => target.runner.clone(),
                TargetPart::StageAs => target.stage_as.clone(),
                TargetPart::Ext => target.ext.clone(),
                TargetPart::Cross => {
                    if target.cross {
                        "yes".into()
                    } else {
                        format!("(registry default: {})", yes_no(target.is_cross()))
                    }
                }
                TargetPart::Vm => {
                    if target.vm {
                        "yes".into()
                    } else {
                        format!("(registry default: {})", yes_no(target.is_vm()))
                    }
                }
            };
            out.push(row(
                part.label(),
                value,
                Field::Target(index, part),
                "edit this target only; empty optional values use registry defaults",
            ));
        }
        out.push(row(
            "Remove target",
            String::new(),
            Field::RemoveTarget(index),
            "remove this target; removing all targets disables the matrix",
        ));
    }
    out
}

fn view_package(view: &View) -> Option<&str> {
    match view {
        View::Package(name) | View::Targets(name) | View::Target(name, _) => Some(name),
        _ => None,
    }
}

fn hook_commands(config: &ReleaseConfig, stage: HookStage) -> &Vec<String> {
    match stage {
        HookStage::PreVersion => &config.hooks.pre_version,
        HookStage::PostVersion => &config.hooks.post_version,
        HookStage::PrePublish => &config.hooks.pre_publish,
        HookStage::PostPublish => &config.hooks.post_publish,
    }
}

fn set_hook_commands(config: &mut ReleaseConfig, stage: HookStage, commands: Vec<String>) {
    match stage {
        HookStage::PreVersion => config.hooks.pre_version = commands,
        HookStage::PostVersion => config.hooks.post_version = commands,
        HookStage::PrePublish => config.hooks.pre_publish = commands,
        HookStage::PostPublish => config.hooks.post_publish = commands,
    }
}

fn package_entries(config: &ReleaseConfig, name: &str) -> Vec<Entry> {
    let Some(pkg) = config.package(name) else {
        return vec![Entry::Header(format!("{name} — no longer configured"))];
    };

    let mut out = vec![Entry::Header(format!("{name}  ·  build"))];
    out.push(row(
        "Name",
        pkg.name.clone(),
        Field::PkgName,
        "package name used by discovery, commands, tags, and publish policy",
    ));
    out.push(row(
        "Adapter",
        ecosystem_label(pkg.adapter).into(),
        Field::PkgAdapter,
        "ecosystem that discovers and releases this package",
    ));

    out.push(row(
        "Mode",
        match pkg.mode {
            Mode::Publish => "publish".into(),
            Mode::BuildOnly => "build-only".into(),
        },
        Field::PkgMode,
        "publish: push to the registry. build-only: attach assets to a GitHub Release",
    ));
    out.push(row(
        "Build command",
        if pkg.command.is_empty() {
            "(none)".into()
        } else {
            pkg.command.clone()
        },
        Field::PkgCommand,
        "run before publishing; {triple}/{bin}/{ext} expand per matrix target",
    ));
    out.push(row(
        "Artifacts",
        if pkg.artifacts.is_empty() {
            "(none)".into()
        } else {
            pkg.artifacts.clone()
        },
        Field::PkgArtifacts,
        "glob for what the build produces",
    ));
    out.push(row(
        "Build targets",
        if pkg.targets.is_empty() {
            "(not a matrix build)".into()
        } else {
            pkg.targets
                .iter()
                .map(|t| format!("{}-{}", t.name, t.arch))
                .collect::<Vec<_>>()
                .join(", ")
        },
        Field::PkgTargets,
        "platforms to build for; selecting none turns the matrix off",
    ));

    out.push(row(
        "Matrix build",
        yes_no(pkg.matrix),
        Field::PkgMatrix,
        "build this package across its target list",
    ));
    out.push(row(
        "Binary name",
        pkg.bin_name.clone().unwrap_or_else(|| "(none)".into()),
        Field::PkgBinName,
        "compiled binary basename; required for matrix staging",
    ));
    out.push(row(
        "Compression",
        pkg.compress.clone().unwrap_or_else(|| "none".into()),
        Field::PkgCompress,
        "optional brotli compression applied to each staged binary",
    ));
    out.push(row(
        "Target details",
        format!("{} target(s)", pkg.targets.len()),
        Field::OpenTargets,
        "edit custom target definitions without replacing existing overrides",
    ));
    out.push(row(
        "Manifest",
        pkg.manifest.clone().unwrap_or_else(|| "(none)".into()),
        Field::PkgManifest,
        "repo-relative manifest path; also determines the npm build working directory",
    ));
    out.push(row(
        "Build environment",
        list_or_none(&env_entries(pkg)),
        Field::PkgEnv,
        "KEY=value variables set on the build, in CI and by a local release build",
    ));
    out.push(Entry::Header("Build setup".into()));
    // A package with no list of its own shows what it inherits, labelled as inherited so the rows
    // cannot be read as settings this package has made.
    setup_rows(
        effective_setup(config, pkg),
        SetupScope::Package(pkg.name.clone()),
        pkg.setup.is_none(),
        &mut out,
    );

    if pkg.setup.is_some() {
        out.push(row(
            "Use repository setup",
            String::new(),
            Field::RestoreSetup,
            "remove this package's setup override and inherit the repository's steps again",
        ));
    }
    if pkg.adapter == Ecosystem::Npm && pkg.is_publish() {
        out.push(Entry::Header("npm".into()));
        out.push(row(
            "Provenance",
            yes_no(pkg.provenance),
            Field::PkgProvenance,
            "publish with --provenance; signs the tarball with the workflow's OIDC identity",
        ));
    }

    if pkg.is_build_only() {
        out.push(Entry::Header("Release assets".into()));
        out.push(row(
            "Archive format",
            pkg.archive
                .map(|a| match a {
                    ArchiveFormat::Auto => "auto",
                    ArchiveFormat::TarGz => "tar.gz",
                    ArchiveFormat::Zip => "zip",
                })
                .unwrap_or("(default: auto)")
                .into(),
            Field::PkgArchive,
            "archive format for each release asset; auto uses zip on Windows and tar.gz elsewhere",
        ));
        out.push(row(
            "Included files",
            list_or_none(&pkg.include),
            Field::PkgInclude,
            "repo-relative paths or globs bundled into each archive",
        ));
        out.push(row(
            "Executable",
            pkg.executable.map(yes_no).unwrap_or_else(|| "auto".into()),
            Field::PkgExecutable,
            "override whether the archived artifact is executable",
        ));
        out.push(row(
            "Checksums",
            yes_no(pkg.checksums),
            Field::PkgChecksums,
            "attach one checksums.txt covering every asset",
        ));
        out.push(row(
            "Build provenance",
            yes_no(pkg.attest),
            Field::PkgAttest,
            "sign assets with the workflow's identity; needs `upgrade` to add the step",
        ));
    }

    out.push(Entry::Header("Release identity".into()));
    out.push(row(
        "Tag format",
        or_inherit(pkg.tag_format.as_ref(), &config.tag_format),
        Field::PkgTagFormat,
        "this package's own tag line, when it must not share the repo's",
    ));
    out.push(row(
        "Changelog",
        or_inherit(
            pkg.changelog.as_ref(),
            match config.changelog_scope {
                ChangelogScope::Root => "root scope",
                ChangelogScope::Package => "package scope",
            },
        ),
        Field::PkgChangelog,
        "path to this package's changelog, relative to the repo root",
    ));

    out.push(row(
        "Legacy tag formats",
        list_or_none(&pkg.legacy_tag_formats),
        Field::PkgLegacyTags,
        "older tag formats belonging to this package; one format per entry",
    ));
    out.push(row(
        "Publish ignore paths",
        list_or_none(config.publish_ignore_paths_for(name)),
        Field::IgnorePaths(name.into()),
        "one path glob per entry used by the release-note checks",
    ));
    if pkg.adapter == Ecosystem::Generic {
        out.push(Entry::Header("Generic adapter".into()));
        out.push(row(
            "Version field",
            pkg.version_field
                .clone()
                .unwrap_or_else(|| DEFAULT_VERSION_FIELD.to_string()),
            Field::PkgVersionField,
            "the key inside the manifest holding the version",
        ));
        out.push(row(
            "Publish command",
            pkg.publish.clone().unwrap_or_else(|| "(none)".into()),
            Field::PkgPublishCommand,
            "how this package reaches its registry",
        ));
    }

    out
}

/// The setup this package's jobs actually run — its own list, or the repo-wide one.
fn effective_setup<'a>(config: &'a ReleaseConfig, pkg: &'a PackageEntry) -> &'a SetupSteps {
    pkg.setup.as_ref().unwrap_or(&config.setup)
}

/// One row per **step** in a setup list, plus the row that appends another.
///
/// A step has four fields, and spending four rows on each turned a two-step list into eight
/// near-identical lines that read as noise rather than as an ordered list. So the list shows one
/// line per step — what it runs, and whether it is filtered — and the fields live in a view of
/// their own, exactly as a package's do.
///
/// `inherited` labels the values as belonging to a list this view does not own, so a package that
/// has not declared one cannot be misread as having set what it merely receives.
fn setup_rows(setup: &SetupSteps, scope: SetupScope, inherited: bool, out: &mut Vec<Entry>) {
    let show = |value: String| {
        if inherited {
            format!("(repo default: {value})")
        } else {
            value
        }
    };

    let steps = setup.steps();
    if steps.is_empty() {
        out.push(row(
            "Steps",
            show("none".into()),
            Field::SetupAdd(scope),
            "no setup step runs here; press enter to add one",
        ));
        return;
    }

    for (i, step) in steps.iter().enumerate() {
        out.push(row(
            &format!("Step {}", i + 1),
            show(step_summary(step)),
            Field::OpenSetupStep(scope.clone(), i),
            "enter opens this step; steps run in the order listed",
        ));
    }

    out.push(row(
        "Add step",
        String::new(),
        Field::SetupAdd(scope),
        "append a step to the end of the list",
    ));
}

/// One step on one line: what it runs, then what confines it.
///
/// The action reference is the identifying part and leads; a script-only step is counted rather
/// than quoted, since a `curl … | bash` line is longer than the column. The `targets` count is the
/// only other thing that changes what the step *does*, so it is the only thing appended.
fn step_summary(step: &Setup) -> String {
    let mut parts = Vec::new();
    if let Some(uses) = &step.uses {
        parts.push(uses.clone());
    }
    if !step.run.is_empty() {
        parts.push(match step.run.len() {
            1 => "1 command".to_string(),
            n => format!("{n} commands"),
        });
    }
    if parts.is_empty() {
        return "empty".to_string();
    }
    let mut summary = parts.join(" + ");
    if !step.targets.is_empty() {
        summary.push_str(&match step.targets.len() {
            1 => " · 1 target".to_string(),
            n => format!(" · {n} targets"),
        });
    }
    if !step.jobs.is_empty() {
        summary.push_str(&format!(" · {} only", job_names(&step.jobs).join("/")));
    }
    summary
}

/// One setup step's own screen: its four fields, and the row that deletes it.
fn setup_step_entries(config: &ReleaseConfig, scope: &SetupScope, index: usize) -> Vec<Entry> {
    let mut out = vec![Entry::Header(format!(
        "{} · step {}",
        scope.heading(),
        index + 1
    ))];

    let Some(step) = scoped_setup(config, scope).and_then(|list| list.steps().get(index)) else {
        return out;
    };

    for part in SetupPart::ALL {
        let value = match part {
            SetupPart::Uses => step.uses.clone(),
            SetupPart::With => Some(step.format_with()).filter(|w| !w.is_empty()),
            SetupPart::Run => Some(step.run.join(", ")).filter(|r| !r.is_empty()),
            // An unfiltered step runs everywhere, which is "all", not "nothing set".
            SetupPart::Targets => Some(if step.targets.is_empty() {
                "all".to_string()
            } else {
                step.targets.join(", ")
            }),
            SetupPart::Jobs => Some(if step.jobs.is_empty() {
                "all".to_string()
            } else {
                job_names(&step.jobs).join(", ")
            }),
        };
        out.push(row(
            part.label(),
            value.unwrap_or_else(|| "(none)".to_string()),
            Field::Setup(scope.clone(), index, part),
            part.hint(),
        ));
    }

    out.push(row(
        "Remove step",
        String::new(),
        Field::SetupRemove(scope.clone(), index),
        "delete this step and go back to the list",
    ));
    out
}

/// The list a scope names, read out of a config rather than an app — the pure row builders need it
/// without a running screen.
fn scoped_setup<'a>(config: &'a ReleaseConfig, scope: &SetupScope) -> Option<&'a SetupSteps> {
    match scope {
        SetupScope::Repo => Some(&config.setup),
        SetupScope::Package(name) => config.package(name).map(|pkg| effective_setup(config, pkg)),
    }
}

fn yes_no(on: bool) -> String {
    if on { "yes" } else { "no" }.to_string()
}

/// The selectable rows, in screen order.
pub fn rows(entries: &[Entry]) -> Vec<&Row> {
    entries
        .iter()
        .filter_map(|e| match e {
            Entry::Row(r) => Some(r),
            Entry::Header(_) => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// modals
// ---------------------------------------------------------------------------

/// An editor open over the screen. Every value change goes through one of these three.
#[derive(Debug, Clone)]
enum Modal {
    /// Pick exactly one.
    Choice {
        title: String,
        options: Vec<String>,
        cursor: usize,
        field: Field,
    },
    /// Check any number.
    Check {
        title: String,
        options: Vec<String>,
        checked: Vec<bool>,
        cursor: usize,
        field: Field,
    },
    List {
        title: String,
        items: Vec<String>,
        cursor: usize,
        editing: Option<String>,
        field: Field,
    },
    /// Type a value.
    Text {
        title: String,
        buffer: String,
        field: Field,
    },
}

impl Modal {
    fn title(&self) -> &str {
        match self {
            Modal::Choice { title, .. }
            | Modal::Check { title, .. }
            | Modal::Text { title, .. }
            | Modal::List { title, .. } => title,
        }
    }
}

fn choice(title: &str, options: Vec<String>, current: &str, field: Field) -> Modal {
    let cursor = options.iter().position(|o| o == current).unwrap_or(0);
    Modal::Choice {
        title: title.to_string(),
        options,
        cursor,
        field,
    }
}

fn check(title: &str, options: Vec<String>, on: &[String], field: Field) -> Modal {
    let checked = options.iter().map(|o| on.contains(o)).collect();
    Modal::Check {
        title: title.to_string(),
        options,
        checked,
        cursor: 0,
        field,
    }
}

fn list(title: &str, items: &[String], field: Field) -> Modal {
    Modal::List {
        title: title.into(),
        items: items.to_vec(),
        cursor: 0,
        editing: None,
        field,
    }
}

fn text(title: &str, current: &str, field: Field) -> Modal {
    Modal::Text {
        title: title.to_string(),
        buffer: current.to_string(),
        field,
    }
}

// ---------------------------------------------------------------------------
// app
// ---------------------------------------------------------------------------

struct App<'a> {
    root: PathBuf,
    factory: &'a dyn AdapterFactory,
    config: ReleaseConfig,
    view: View,
    cursor: usize,
    scroll: u16,
    modal: Option<Modal>,
    status: Option<String>,
    /// Packages the repo has that `release.toml` does not, refreshed when the config changes.
    new_packages: Vec<UnconfiguredPackage>,
}

impl App<'_> {
    fn new_names(&self) -> Vec<String> {
        self.new_packages
            .iter()
            .map(|p| p.pkg.name.clone())
            .collect()
    }

    fn entries(&self) -> Vec<Entry> {
        build(&self.config, &self.view, &self.new_names())
    }

    /// Move to another view, landing at the top of it. Every navigation resets both the cursor and
    /// the scroll: the row counts differ per view, so carrying either across lands on whatever
    /// happens to sit at that offset.
    fn goto(&mut self, view: View) {
        self.view = view;
        self.cursor = 0;
        self.scroll = 0;
    }

    fn refresh_new_packages(&mut self) {
        self.new_packages = unconfigured_packages(&self.config, self.factory).unwrap_or_default();
    }

    fn save(&mut self) -> Result<()> {
        self.config.save_preserving(&self.root)?;
        self.refresh_new_packages();
        self.status = Some(format!("Saved {CONFIG_FILE}"));
        Ok(())
    }
}

/// Show the config screen. Returns when the user leaves it.
pub fn run(root: &Path, factory: &dyn AdapterFactory) -> Result<()> {
    require_terminal()?;
    let config = ReleaseConfig::load(root)?;
    let mut app = App {
        root: root.to_path_buf(),
        factory,
        config,
        view: View::Settings,
        cursor: 0,
        scroll: 0,
        modal: None,
        status: None,
        new_packages: Vec::new(),
    };
    app.refresh_new_packages();

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    result
}

/// Fail with an explanation before entering raw mode, rather than panicking inside it.
///
/// A full-screen editor needs a real terminal on both ends: stdin to read keys, stdout to draw. In
/// CI or behind a pipe there is neither, and `ratatui::init` panics — which in a release pipeline
/// surfaces as a backtrace instead of a sentence saying what to do. Point at the file, since
/// `release.toml` is the actual interface for anything automated.
fn require_terminal() -> Result<()> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        return Ok(());
    }
    anyhow::bail!(
        "`config` is an interactive screen and needs a terminal on stdin and stdout.\n\
         Nothing here is exclusive to it: `{CONFIG_FILE}` is plain, committed TOML — edit it \
         directly, then run `release doctor` to check the result."
    )
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        let entries = app.entries();
        let count = rows(&entries).len();
        if count > 0 && app.cursor >= count {
            app.cursor = count - 1;
        }
        terminal.draw(|f| draw(f, app, &entries))?;

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(());
        }
        if app.modal.is_some() {
            handle_modal_key(app, key)?;
        } else if !handle_screen_key(app, key, count)? {
            return Ok(());
        }
    }
}

/// Returns false when the screen should close.
fn handle_screen_key(app: &mut App, key: KeyEvent, count: usize) -> Result<bool> {
    match key.code {
        KeyCode::Char('q') => return Ok(false),
        // Esc is "back" everywhere else in this tool; at the top there is nowhere to go.
        KeyCode::Esc => match app.view.parent() {
            Some(parent) => app.goto(parent),
            None => return Ok(false),
        },
        KeyCode::Down | KeyCode::Char('j') if count > 0 => app.cursor = (app.cursor + 1) % count,
        KeyCode::Up | KeyCode::Char('k') if count > 0 => {
            app.cursor = (app.cursor + count - 1) % count
        }
        KeyCode::Home => app.cursor = 0,
        KeyCode::End => app.cursor = count.saturating_sub(1),
        KeyCode::Enter | KeyCode::Char(' ') => open_editor(app)?,
        _ => {}
    }
    Ok(true)
}

fn open_editor(app: &mut App) -> Result<()> {
    let entries = app.entries();
    let Some(row) = rows(&entries).get(app.cursor).map(|r| (*r).clone()) else {
        return Ok(());
    };
    app.status = None;

    let config = &app.config;
    let modal = match &row.field {
        Field::OpenIgnorePaths => {
            app.goto(View::IgnorePaths);
            return Ok(());
        }
        Field::IgnorePaths(name) => list(
            "Publish ignore globs",
            config.publish_ignore_paths_for(name),
            row.field.clone(),
        ),
        Field::AddIgnorePaths => text("Package name for ignore policy", "", row.field.clone()),
        Field::ToolVersion => text(
            "Tool version (blank uses the generating version)",
            config.otf_release_version.as_deref().unwrap_or(""),
            row.field.clone(),
        ),
        Field::NpmSecret => text(
            "npm token secret name",
            &config.secrets.npm,
            row.field.clone(),
        ),
        Field::CargoSecret => text(
            "Cargo token secret name",
            &config.secrets.cargo,
            row.field.clone(),
        ),
        Field::DiscoveryNpm => list(
            "npm package directory globs",
            &config.discovery.npm,
            row.field.clone(),
        ),
        Field::AddPackage => text("New package name", "", row.field.clone()),
        Field::OpenTargets => {
            let name = view_package(&app.view).unwrap().to_string();
            app.goto(View::Targets(name));
            return Ok(());
        }
        Field::OpenTarget(index) => {
            let name = view_package(&app.view).unwrap().to_string();
            app.goto(View::Target(name, *index));
            return Ok(());
        }
        Field::AddTarget => {
            let name = view_package(&app.view).unwrap().to_string();
            let pkg = app
                .config
                .packages
                .iter_mut()
                .find(|p| p.name == name)
                .unwrap();
            let mut suffix = pkg.targets.len() + 1;
            while pkg
                .targets
                .iter()
                .any(|t| t.name == format!("custom-{suffix}"))
            {
                suffix += 1;
            }
            pkg.targets.push(Target {
                name: format!("custom-{suffix}"),
                arch: "x86_64".into(),
                ..Target::default()
            });
            pkg.matrix = true;
            let index = pkg.targets.len() - 1;
            app.save()?;
            app.goto(View::Target(name, index));
            return Ok(());
        }
        Field::RemoveTarget(index) => {
            let name = view_package(&app.view).unwrap().to_string();
            let pkg = app
                .config
                .packages
                .iter_mut()
                .find(|p| p.name == name)
                .unwrap();
            if *index < pkg.targets.len() {
                pkg.targets.remove(*index);
            }
            if pkg.targets.is_empty() {
                pkg.matrix = false;
            }
            app.save()?;
            app.goto(View::Targets(name));
            return Ok(());
        }
        Field::Target(index, part) => {
            let target = config
                .package(view_package(&app.view).unwrap())
                .unwrap()
                .targets
                .get(*index)
                .unwrap();
            match part {
                TargetPart::Cross | TargetPart::Vm => choice(
                    part.label(),
                    vec!["default".into(), "yes".into()],
                    if if *part == TargetPart::Cross {
                        target.cross
                    } else {
                        target.vm
                    } {
                        "yes"
                    } else {
                        "default"
                    },
                    row.field.clone(),
                ),
                _ => {
                    let value = match part {
                        TargetPart::Name => &target.name,
                        TargetPart::Arch => &target.arch,
                        TargetPart::Triple => &target.triple,
                        TargetPart::Runner => &target.runner,
                        TargetPart::StageAs => &target.stage_as,
                        TargetPart::Ext => &target.ext,
                        _ => unreachable!(),
                    };
                    text(part.label(), value, row.field.clone())
                }
            }
        }
        Field::RestoreSetup => {
            let name = view_package(&app.view).unwrap().to_string();
            app.config
                .packages
                .iter_mut()
                .find(|p| p.name == name)
                .unwrap()
                .setup = None;
            app.save()?;
            return Ok(());
        }

        Field::OpenPackage(name) => {
            app.goto(View::Package(name.clone()));
            return Ok(());
        }
        Field::OpenSetupStep(scope, index) => {
            app.goto(View::SetupStep(scope.clone(), *index));
            return Ok(());
        }
        Field::AdoptPackage(name) => choice(
            &format!("{name} is not in release.toml yet"),
            vec![
                "Release it — write its [[package]] block".into(),
                "Skip it — never version or publish it".into(),
            ],
            "",
            Field::AdoptPackage(name.clone()),
        ),
        Field::Provider => text("Git hosting provider", &config.provider, Field::Provider),
        Field::DefaultBranch => text(
            "Default branch",
            &config.default_branch,
            Field::DefaultBranch,
        ),
        Field::TagFormat => {
            let mut options: Vec<String> = COMMON_TAG_FORMATS
                .iter()
                .map(|f| (*f).to_string())
                .collect();
            if !options.contains(&config.tag_format) {
                options.push(config.tag_format.clone());
            }
            options.push("Custom…".into());
            choice("Tag format", options, &config.tag_format, Field::TagFormat)
        }
        Field::LegacyTagFormats => list(
            "Legacy tag formats",
            &config.legacy_tag_formats,
            Field::LegacyTagFormats,
        ),
        Field::SnapshotTag => text(
            "Snapshot tag (blank for none)",
            config.snapshot_tag.as_deref().unwrap_or(""),
            Field::SnapshotTag,
        ),
        Field::ChangelogScope => choice(
            "Changelog scope",
            vec!["root".into(), "package".into()],
            match config.changelog_scope {
                ChangelogScope::Root => "root",
                ChangelogScope::Package => "package",
            },
            Field::ChangelogScope,
        ),
        Field::ChangelogStrategy => choice(
            "Changelog strategy",
            vec!["curated".into(), "generated".into()],
            match config.changelog_strategy {
                ChangelogStrategy::Curated => "curated",
                ChangelogStrategy::Generated => "generated",
            },
            Field::ChangelogStrategy,
        ),
        Field::GithubReleaseNotes => choice(
            "GitHub Release notes",
            vec![
                "auto-generate".into(),
                "curated-changelog".into(),
                "semantic-commits".into(),
            ],
            match config.github_release_notes {
                GithubReleaseNotes::AutoGenerate => "auto-generate",
                GithubReleaseNotes::CuratedChangelog => "curated-changelog",
                GithubReleaseNotes::SemanticCommits => "semantic-commits",
            },
            Field::GithubReleaseNotes,
        ),
        Field::Ecosystems => {
            let on: Vec<String> = config
                .adapters
                .iter()
                .map(|e| ecosystem_label(*e).to_string())
                .collect();
            check(
                "Enabled ecosystems",
                Ecosystem::ALL
                    .iter()
                    .map(|e| ecosystem_label(*e).to_string())
                    .collect(),
                &on,
                Field::Ecosystems,
            )
        }
        Field::SkipPublish => {
            let names = known_package_names(config, app.factory)?;
            check(
                "Packages this repo must never publish",
                names,
                &config.skip_publish,
                Field::SkipPublish,
            )
        }
        Field::Hook(stage) => list(
            stage.label(),
            hook_commands(config, *stage),
            Field::Hook(*stage),
        ),
        Field::SetupAdd(scope) => {
            // Not a modal: the row appends a blank step and opens it, so adding one lands where it
            // is filled in rather than back on a list with a new empty line on it. Nothing is
            // written yet — a step with neither an action nor a script emits nothing, so there is
            // nothing to save until one of its fields is edited.
            let scope = scope.clone();
            let Some(list) = setup_list_mut(app, &scope) else {
                return Ok(());
            };
            list.steps_mut().push(Setup::default());
            let index = list.steps().len() - 1;
            app.goto(View::SetupStep(scope, index));
            return Ok(());
        }
        Field::SetupRemove(scope, index) => {
            let (scope, index) = (scope.clone(), *index);
            if let Some(list) = setup_list_mut(app, &scope) {
                let steps = list.steps_mut();
                if index < steps.len() {
                    steps.remove(index);
                }
            }
            // Back to the list: the view this row belongs to is about a step that no longer exists.
            if let Some(parent) = View::SetupStep(scope, index).parent() {
                app.goto(parent);
            }
            app.save()?;
            return Ok(());
        }
        Field::Setup(scope, index, part) => {
            let Some(step) = setup_list(app, scope).and_then(|l| l.steps().get(*index)) else {
                return Ok(());
            };
            let field = Field::Setup(scope.clone(), *index, *part);
            match part {
                // Targets are picked, not typed: the triples are already declared in
                // `[[package.targets]]`, and a filter only does anything when it matches one
                // exactly. Typing them from memory is the one way to write a filter that silently
                // never runs — which is `doctor`'s `setup-targets-unknown`, avoided here entirely.
                SetupPart::Targets => {
                    let on = step.targets.clone();
                    check(
                        "Targets this step runs on (none checked = all of them)",
                        selectable_triples(config, scope, &on),
                        &on,
                        field,
                    )
                }
                SetupPart::Uses => text(
                    "Setup action (blank for none)",
                    &step.uses.clone().unwrap_or_default(),
                    field,
                ),
                SetupPart::With => list(
                    "Action inputs (one key=value per entry)",
                    &step
                        .with
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>(),
                    field,
                ),
                SetupPart::Run => list("Setup commands", &step.run, field),
                // Picked, like targets: a typo here would be an unknown job kind.
                SetupPart::Jobs => {
                    let on = job_names(&step.jobs);
                    check(
                        "Jobs this step runs in (none checked = all of them)",
                        selectable_jobs(scope),
                        &on,
                        field,
                    )
                }
            }
        }
        other => package_editor(app, other.clone())?,
    };
    app.modal = Some(modal);
    Ok(())
}

fn package_editor(app: &App, field: Field) -> Result<Modal> {
    let View::Package(name) = &app.view else {
        anyhow::bail!("package field outside a package view");
    };
    let pkg = app
        .config
        .package(name)
        .ok_or_else(|| anyhow::anyhow!("{name} is no longer configured"))?;

    Ok(match field {
        Field::PkgName => text("Package name", &pkg.name, field),
        Field::PkgAdapter => choice(
            "Package adapter",
            Ecosystem::ALL
                .iter()
                .map(|e| ecosystem_label(*e).into())
                .collect(),
            ecosystem_label(pkg.adapter),
            field,
        ),
        Field::PkgMatrix => choice(
            "Build across a target matrix?",
            vec!["yes".into(), "no".into()],
            &yes_no(pkg.matrix),
            field,
        ),
        Field::PkgBinName => text(
            "Binary name (without extension)",
            pkg.bin_name.as_deref().unwrap_or(""),
            field,
        ),
        Field::PkgCompress => choice(
            "Binary compression",
            vec!["none".into(), "brotli".into()],
            pkg.compress.as_deref().unwrap_or("none"),
            field,
        ),
        Field::PkgArchive => choice(
            "Archive format",
            vec![
                "default".into(),
                "auto".into(),
                "tar.gz".into(),
                "zip".into(),
            ],
            pkg.archive
                .map(|a| match a {
                    ArchiveFormat::Auto => "auto",
                    ArchiveFormat::TarGz => "tar.gz",
                    ArchiveFormat::Zip => "zip",
                })
                .unwrap_or("default"),
            field,
        ),
        Field::PkgExecutable => choice(
            "Archived executable permission",
            vec!["auto".into(), "yes".into(), "no".into()],
            &pkg.executable.map(yes_no).unwrap_or_else(|| "auto".into()),
            field,
        ),
        Field::PkgInclude => list("Included paths and globs", &pkg.include, field),
        Field::PkgEnv => list(
            "Build environment (one KEY=value per entry)",
            &env_entries(pkg),
            field,
        ),
        Field::PkgLegacyTags => list("Package legacy tag formats", &pkg.legacy_tag_formats, field),

        Field::PkgMode => choice(
            "Package mode",
            vec!["publish".into(), "build-only".into()],
            match pkg.mode {
                Mode::Publish => "publish",
                Mode::BuildOnly => "build-only",
            },
            Field::PkgMode,
        ),
        Field::PkgCommand => text("Build command", &pkg.command, Field::PkgCommand),
        Field::PkgArtifacts => text("Artifacts glob", &pkg.artifacts, Field::PkgArtifacts),
        Field::PkgTargets => {
            let on: Vec<String> = pkg
                .targets
                .iter()
                .map(|t| target_label(&t.name, &t.arch))
                .collect();
            let mut options: Vec<String> = TARGET_REGISTRY
                .iter()
                .map(|t| target_label(t.name, t.arch))
                .collect();
            // A hand-written target the registry does not know stays on the list rather than being
            // silently dropped the first time someone opens this.
            for extra in &on {
                if !options.contains(extra) {
                    options.push(extra.clone());
                }
            }
            check("Build targets", options, &on, Field::PkgTargets)
        }
        Field::PkgChecksums => choice(
            "Attach a checksums.txt?",
            vec!["yes".into(), "no".into()],
            &yes_no(pkg.checksums),
            Field::PkgChecksums,
        ),
        Field::PkgAttest => choice(
            "Generate signed build provenance?",
            vec!["yes".into(), "no".into()],
            &yes_no(pkg.attest),
            Field::PkgAttest,
        ),
        Field::PkgProvenance => choice(
            "Publish with npm provenance?",
            vec!["yes".into(), "no".into()],
            &yes_no(pkg.provenance),
            Field::PkgProvenance,
        ),
        Field::PkgTagFormat => {
            let mut options = vec![format!("(repo default: {})", app.config.tag_format)];
            options.extend(
                COMMON_TAG_FORMATS
                    .iter()
                    .filter(|f| **f != app.config.tag_format)
                    .map(|f| (*f).to_string()),
            );
            if let Some(current) = &pkg.tag_format {
                if !options.contains(current) {
                    options.push(current.clone());
                }
            }
            let current = pkg.tag_format.clone().unwrap_or_else(|| options[0].clone());
            options.push("Custom…".into());
            choice("Tag format for this package", options, &current, field)
        }
        Field::PkgChangelog => text(
            "Changelog path (blank inherits the repo's scope)",
            pkg.changelog.as_deref().unwrap_or(""),
            Field::PkgChangelog,
        ),
        Field::PkgManifest => text(
            "Generic manifest",
            pkg.manifest.as_deref().unwrap_or(""),
            Field::PkgManifest,
        ),
        Field::PkgVersionField => text(
            "Generic version field",
            pkg.version_field
                .as_deref()
                .unwrap_or(DEFAULT_VERSION_FIELD),
            Field::PkgVersionField,
        ),
        Field::PkgPublishCommand => text(
            "Generic publish command",
            pkg.publish.as_deref().unwrap_or(""),
            Field::PkgPublishCommand,
        ),
        other => anyhow::bail!("{other:?} is not a package field"),
    })
}

/// Every package name this repo knows about: the blocks it configures, the packages its adapters
/// discover, and whatever is already skipped.
///
/// The union matters — a name already in `skip_publish` is invisible to discovery (that is the
/// point of skipping it), so a list built from discovery alone would show every existing entry as
/// absent and wipe them the moment the checklist was confirmed.
fn known_package_names(
    config: &ReleaseConfig,
    factory: &dyn AdapterFactory,
) -> Result<Vec<String>> {
    let mut names: Vec<String> = config.skip_publish.clone();
    names.extend(config.packages.iter().map(|entry| entry.name.clone()));
    for eco in config
        .adapters
        .iter()
        .copied()
        .filter(|eco| *eco != Ecosystem::Generic)
    {
        let adapter = factory.make_with_discovery(eco, &config.discovery);
        names.extend(adapter.discover_packages()?.into_iter().map(|pkg| pkg.name));
    }
    names.sort();
    names.dedup();
    Ok(names)
}

fn target_label(name: &str, arch: &str) -> String {
    format!("{name}-{arch}")
}

/// The triples a setup step can be filtered to: what the packages in its scope actually build.
///
/// A repo-wide step reaches every package that has not replaced the list, so its options are the
/// union of what those build. Anything already on the step stays on the list even if no package
/// declares it any more — dropping it silently would edit the config just by opening the row, and
/// `doctor`'s `setup-targets-unknown` is what reports it.
/// The job kinds a scope's steps can reach. The repo-wide list runs in the gate and the catch-all
/// publish; a package's list runs in its own jobs, which never include the gate.
fn selectable_jobs(scope: &SetupScope) -> Vec<String> {
    let kinds: &[JobKind] = match scope {
        SetupScope::Repo => &[JobKind::CheckRelease, JobKind::Publish],
        SetupScope::Package(_) => &[
            JobKind::Matrix,
            JobKind::Build,
            JobKind::Publish,
            JobKind::GithubRelease,
        ],
    };
    job_names(kinds)
}

fn job_names(kinds: &[JobKind]) -> Vec<String> {
    kinds.iter().map(|kind| kind.as_str().to_string()).collect()
}

/// A package's `env` as the `KEY=value` lines its editor shows and reads back.
fn env_entries(pkg: &PackageEntry) -> Vec<String> {
    pkg.env.iter().map(|(k, v)| format!("{k}={v}")).collect()
}

fn selectable_triples(
    config: &ReleaseConfig,
    scope: &SetupScope,
    current: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = config
        .packages
        .iter()
        .filter(|pkg| match scope {
            SetupScope::Repo => pkg.setup.is_none(),
            SetupScope::Package(name) => pkg.name == *name,
        })
        .filter(|pkg| pkg.matrix)
        .flat_map(|pkg| pkg.targets.iter())
        .map(|target| target.triple.clone())
        .collect();
    out.sort();
    out.dedup();
    for extra in current {
        if !out.contains(extra) {
            out.push(extra.clone());
        }
    }
    out
}

fn handle_modal_key(app: &mut App, key: KeyEvent) -> Result<()> {
    let Some(modal) = app.modal.as_mut() else {
        return Ok(());
    };
    match modal {
        Modal::List {
            items,
            cursor,
            editing,
            ..
        } => {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
                if let Some(value) = editing.take() {
                    if !value.is_empty() {
                        if *cursor < items.len() {
                            items[*cursor] = value;
                        } else {
                            items.push(value);
                        }
                    }
                }
                let modal = app.modal.take().unwrap();
                apply(app, modal)?;
            } else if let Some(buffer) = editing.as_mut() {
                match key.code {
                    KeyCode::Esc => {
                        *editing = None;
                        *cursor = (*cursor).min(items.len().saturating_sub(1));
                    }
                    KeyCode::Backspace => {
                        buffer.pop();
                    }
                    KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                        buffer.push('\n')
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        buffer.clear()
                    }
                    KeyCode::Enter => {
                        let value = editing.take().unwrap();
                        if !value.is_empty() {
                            if *cursor < items.len() {
                                items[*cursor] = value;
                            } else {
                                items.push(value);
                            }
                        }
                    }
                    KeyCode::Char(c) => buffer.push(c),
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Esc => app.modal = None,
                    KeyCode::Enter => {
                        *editing = Some(items.get(*cursor).cloned().unwrap_or_default())
                    }
                    KeyCode::Char('a') => {
                        *cursor = items.len();
                        *editing = Some(String::new());
                    }
                    KeyCode::Char('d') if *cursor < items.len() => {
                        items.remove(*cursor);
                        *cursor = cursor.saturating_sub(1);
                    }
                    KeyCode::Up
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && *cursor > 0
                            && *cursor < items.len() =>
                    {
                        items.swap(*cursor, *cursor - 1);
                        *cursor -= 1;
                    }
                    KeyCode::Down
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && *cursor + 1 < items.len() =>
                    {
                        items.swap(*cursor, *cursor + 1);
                        *cursor += 1;
                    }
                    KeyCode::Down | KeyCode::Char('j') if !items.is_empty() => {
                        *cursor = (*cursor + 1) % items.len()
                    }
                    KeyCode::Up | KeyCode::Char('k') if !items.is_empty() => {
                        *cursor = (*cursor + items.len() - 1) % items.len()
                    }
                    _ => {}
                }
            }
        }

        Modal::Choice {
            options, cursor, ..
        } => match key.code {
            KeyCode::Esc => app.modal = None,
            KeyCode::Down | KeyCode::Char('j') => *cursor = (*cursor + 1) % options.len(),
            KeyCode::Up | KeyCode::Char('k') => {
                *cursor = (*cursor + options.len() - 1) % options.len()
            }
            KeyCode::Enter => {
                let modal = app.modal.take().expect("modal present");
                apply(app, modal)?;
            }
            _ => {}
        },
        Modal::Check {
            options,
            checked,
            cursor,
            ..
        } => match key.code {
            KeyCode::Esc => app.modal = None,
            KeyCode::Down | KeyCode::Char('j') => *cursor = (*cursor + 1) % options.len().max(1),
            KeyCode::Up | KeyCode::Char('k') => {
                *cursor = (*cursor + options.len().max(1) - 1) % options.len().max(1)
            }
            KeyCode::Char(' ') => {
                if let Some(slot) = checked.get_mut(*cursor) {
                    *slot = !*slot;
                }
            }
            KeyCode::Enter => {
                let modal = app.modal.take().expect("modal present");
                apply(app, modal)?;
            }
            _ => {}
        },
        Modal::Text { buffer, .. } => match key.code {
            KeyCode::Esc => app.modal = None,
            KeyCode::Backspace => {
                buffer.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => buffer.clear(),
            KeyCode::Char(c) => buffer.push(c),
            KeyCode::Enter => {
                let modal = app.modal.take().expect("modal present");
                apply(app, modal)?;
            }
            _ => {}
        },
    }
    Ok(())
}

fn optional(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Write a confirmed edit into the config and persist it.
///
/// Validation failures set the status line instead of unwinding: an invalid tag format is a typo to
/// correct, not a reason to lose the session.
fn apply(app: &mut App, modal: Modal) -> Result<()> {
    match modal {
        Modal::List { items, field, .. } => apply_list(app, field, items)?,
        Modal::Choice {
            options,
            cursor,
            field,
            ..
        } => {
            let picked = options[cursor].clone();
            apply_choice(app, field, picked)?;
        }
        Modal::Check {
            options,
            checked,
            field,
            ..
        } => {
            let picked: Vec<String> = options
                .into_iter()
                .zip(checked)
                .filter_map(|(o, on)| on.then_some(o))
                .collect();
            apply_check(app, field, picked)?;
        }
        Modal::Text { buffer, field, .. } => apply_text(app, field, buffer)?,
    }
    Ok(())
}

fn apply_list(app: &mut App, field: Field, items: Vec<String>) -> Result<()> {
    match field {
        Field::DiscoveryNpm | Field::IgnorePaths(_) | Field::PkgInclude => {
            for pattern in &items {
                let pattern = pattern.strip_prefix('!').unwrap_or(pattern);
                if pattern.is_empty() || glob::Pattern::new(pattern).is_err() {
                    app.status = Some("Not saved: invalid path glob".into());
                    return Ok(());
                }
            }
            match field {
                Field::DiscoveryNpm => app.config.discovery.npm = items,
                Field::IgnorePaths(name) => {
                    if items.is_empty() {
                        app.config.publish.ignore_paths.remove(&name);
                    } else {
                        app.config.publish.ignore_paths.insert(name, items);
                    }
                }
                Field::PkgInclude => {
                    let name = view_package(&app.view).unwrap().to_string();
                    app.config
                        .packages
                        .iter_mut()
                        .find(|p| p.name == name)
                        .unwrap()
                        .include = items;
                }
                _ => unreachable!(),
            }
        }
        Field::LegacyTagFormats | Field::PkgLegacyTags => {
            for format in &items {
                if let Err(err) = format_tag(format, "package", "1.2.3") {
                    app.status = Some(format!("Not saved: {err}"));
                    return Ok(());
                }
            }
            if field == Field::LegacyTagFormats {
                app.config.legacy_tag_formats = items;
            } else {
                let name = view_package(&app.view).unwrap().to_string();
                app.config
                    .packages
                    .iter_mut()
                    .find(|p| p.name == name)
                    .unwrap()
                    .legacy_tag_formats = items;
            }
        }

        Field::Hook(stage) => set_hook_commands(&mut app.config, stage, items),
        Field::PkgEnv => {
            let mut env = std::collections::BTreeMap::new();
            for item in &items {
                let Some((key, value)) = item.split_once('=') else {
                    app.status = Some("Not saved: each variable must be KEY=value".into());
                    return Ok(());
                };
                let key = key.trim();
                if !is_env_name(key) || env.insert(key.to_string(), value.to_string()).is_some() {
                    app.status = Some(format!(
                        "Not saved: `{key}` is not a unique environment variable name"
                    ));
                    return Ok(());
                }
            }
            let name = view_package(&app.view).unwrap().to_string();
            app.config
                .packages
                .iter_mut()
                .find(|p| p.name == name)
                .unwrap()
                .env = env;
        }
        Field::Setup(scope, index, part) => {
            let mut inputs = std::collections::BTreeMap::new();
            if part == SetupPart::With {
                for item in &items {
                    let Some((key, value)) = item.split_once('=') else {
                        app.status = Some("Not saved: each input must be key=value".into());
                        return Ok(());
                    };
                    if key.trim().is_empty()
                        || inputs
                            .insert(key.trim().to_string(), value.to_string())
                            .is_some()
                    {
                        app.status =
                            Some("Not saved: input names must be nonempty and unique".into());
                        return Ok(());
                    }
                }
            }
            let Some(steps) = setup_list_mut(app, &scope) else {
                return Ok(());
            };
            let backup = steps.clone();
            let Some(step) = steps.steps_mut().get_mut(index) else {
                return Ok(());
            };
            match part {
                SetupPart::Run => step.run = items,
                SetupPart::With => step.with = inputs,
                _ => return Ok(()),
            }
            if let Err(err) = steps.validate("setup") {
                *steps = backup;
                app.status = Some(format!("Not saved: {err}"));
                return Ok(());
            }
        }
        _ => return Ok(()),
    }
    app.save()
}

fn apply_choice(app: &mut App, field: Field, picked: String) -> Result<()> {
    match field {
        Field::TagFormat | Field::PkgTagFormat if picked == "Custom…" => {
            let current = if field == Field::TagFormat {
                app.config.tag_format.clone()
            } else {
                app.config
                    .package(view_package(&app.view).unwrap())
                    .unwrap()
                    .tag_format
                    .clone()
                    .unwrap_or_default()
            };
            app.modal = Some(text(
                "Custom tag format (must contain {version})",
                &current,
                field,
            ));
            return Ok(());
        }
        Field::Target(index, part) => {
            let name = view_package(&app.view).unwrap().to_string();
            let target = &mut app
                .config
                .packages
                .iter_mut()
                .find(|p| p.name == name)
                .unwrap()
                .targets[index];
            match part {
                TargetPart::Cross => target.cross = picked == "yes",
                TargetPart::Vm => target.vm = picked == "yes",
                _ => return Ok(()),
            }
        }

        Field::AdoptPackage(name) => {
            if picked.starts_with("Release") {
                let Some(new) = app
                    .new_packages
                    .iter()
                    .find(|p| p.pkg.name == name)
                    .cloned()
                else {
                    return Ok(());
                };
                let stripped = adopt_package(&mut app.config, app.factory, &app.root, &new)?;
                app.save()?;
                app.status = Some(match stripped.len() {
                    0 => format!("Added a [[package]] block for {name}"),
                    n => format!("Added {name}; stripped {n} npm lifecycle hook(s)"),
                });
            } else {
                app.config.skip_publish.push(name.clone());
                app.config.skip_publish.sort();
                app.config.skip_publish.dedup();
                app.save()?;
                app.status = Some(format!("{name} moved into skip_publish"));
            }
            return Ok(());
        }
        Field::Provider => app.config.provider = picked,
        Field::TagFormat => {
            if let Err(err) = format_tag(&picked, "package", "1.2.3") {
                app.status = Some(format!("Not saved: {err}"));
                return Ok(());
            }
            app.config.tag_format = picked;
        }
        Field::ChangelogScope => {
            app.config.changelog_scope = if picked == "root" {
                ChangelogScope::Root
            } else {
                ChangelogScope::Package
            }
        }
        Field::ChangelogStrategy => {
            app.config.changelog_strategy = if picked == "generated" {
                ChangelogStrategy::Generated
            } else {
                ChangelogStrategy::Curated
            }
        }
        Field::GithubReleaseNotes => {
            app.config.github_release_notes = match picked.as_str() {
                "curated-changelog" => GithubReleaseNotes::CuratedChangelog,
                "semantic-commits" => GithubReleaseNotes::SemanticCommits,
                _ => GithubReleaseNotes::AutoGenerate,
            }
        }
        Field::PkgAdapter
        | Field::PkgMatrix
        | Field::PkgCompress
        | Field::PkgArchive
        | Field::PkgExecutable
        | Field::PkgMode
        | Field::PkgChecksums
        | Field::PkgAttest
        | Field::PkgProvenance
        | Field::PkgTagFormat => return apply_package_choice(app, field, picked),
        _ => return Ok(()),
    }
    app.save()
}

fn apply_package_choice(app: &mut App, field: Field, picked: String) -> Result<()> {
    let View::Package(name) = app.view.clone() else {
        return Ok(());
    };
    // Validate before borrowing the entry mutably, so a bad value never half-applies.
    if field == Field::PkgTagFormat && !picked.starts_with("(repo default") {
        if let Err(err) = format_tag(&picked, &name, "1.2.3") {
            app.status = Some(format!("Not saved: {err}"));
            return Ok(());
        }
    }
    let Some(pkg) = app.config.packages.iter_mut().find(|p| p.name == name) else {
        return Ok(());
    };
    match field {
        Field::PkgAdapter => {
            if let Some(adapter) = Ecosystem::ALL
                .iter()
                .find(|e| ecosystem_label(**e) == picked)
            {
                pkg.adapter = *adapter;
            }
        }
        Field::PkgMatrix => pkg.matrix = picked == "yes",
        Field::PkgCompress => pkg.compress = (picked == "brotli").then_some(picked),
        Field::PkgArchive => {
            pkg.archive = match picked.as_str() {
                "auto" => Some(ArchiveFormat::Auto),
                "tar.gz" => Some(ArchiveFormat::TarGz),
                "zip" => Some(ArchiveFormat::Zip),
                _ => None,
            }
        }
        Field::PkgExecutable => {
            pkg.executable = match picked.as_str() {
                "yes" => Some(true),
                "no" => Some(false),
                _ => None,
            }
        }

        Field::PkgMode => {
            pkg.mode = if picked == "publish" {
                Mode::Publish
            } else {
                Mode::BuildOnly
            }
        }
        Field::PkgChecksums => pkg.checksums = picked == "yes",
        Field::PkgAttest => pkg.attest = picked == "yes",
        Field::PkgProvenance => pkg.provenance = picked == "yes",
        Field::PkgTagFormat => {
            pkg.tag_format = (!picked.starts_with("(repo default")).then_some(picked);
        }
        _ => return Ok(()),
    }
    let reminder = match field {
        Field::PkgAttest if pkg.attest => Some("Run `release upgrade` to add the signing step"),
        Field::PkgProvenance if pkg.provenance => {
            Some("Run `release upgrade` to add the OIDC permissions")
        }
        _ => None,
    };
    app.save()?;
    if let Some(reminder) = reminder {
        app.status = Some(reminder.into());
    }
    Ok(())
}

fn apply_check(app: &mut App, field: Field, picked: Vec<String>) -> Result<()> {
    match field {
        Field::LegacyTagFormats => {
            for format in &picked {
                if let Err(err) = format_tag(format, "package", "1.2.3") {
                    app.status = Some(format!("Not saved: {err}"));
                    return Ok(());
                }
            }
            app.config.legacy_tag_formats = picked;
        }
        Field::Setup(scope, index, SetupPart::Jobs) => {
            // As with targets, every kind checked means the same as none: drop the scope.
            let picked = if picked.len() == selectable_jobs(&scope).len() {
                Vec::new()
            } else {
                picked
                    .iter()
                    .filter_map(|name| JobKind::parse(name))
                    .collect()
            };
            let Some(list) = setup_list_mut(app, &scope) else {
                return Ok(());
            };
            let Some(step) = list.steps_mut().get_mut(index) else {
                return Ok(());
            };
            step.jobs = picked;
        }
        Field::Setup(scope, index, SetupPart::Targets) => {
            // Checking every option and checking none both mean "every row", so the filter is
            // dropped rather than written out in full — `doctor` would call that one redundant.
            let all = selectable_triples(&app.config, &scope, &picked);
            let picked = if picked.len() == all.len() {
                Vec::new()
            } else {
                picked
            };
            let Some(list) = setup_list_mut(app, &scope) else {
                return Ok(());
            };
            let Some(step) = list.steps_mut().get_mut(index) else {
                return Ok(());
            };
            step.targets = picked;
        }
        Field::SkipPublish => {
            app.config.skip_publish = picked;
            let sync = sync_package_blocks(&mut app.config, app.factory, &app.root)?;
            if !sync.is_empty() {
                app.status = Some(format!(
                    "{} block(s) added, {} removed",
                    sync.added.len(),
                    sync.removed.len()
                ));
            }
        }
        Field::Ecosystems => {
            app.config.adapters = Ecosystem::ALL
                .iter()
                .copied()
                .filter(|e| picked.iter().any(|p| p == ecosystem_label(*e)))
                .collect();
            let sync = sync_package_blocks(&mut app.config, app.factory, &app.root)?;
            if !sync.is_empty() {
                app.status = Some(format!(
                    "{} block(s) added, {} removed",
                    sync.added.len(),
                    sync.removed.len()
                ));
            }
        }
        Field::PkgTargets => {
            let View::Package(name) = app.view.clone() else {
                return Ok(());
            };
            let Some(pkg) = app.config.packages.iter_mut().find(|p| p.name == name) else {
                return Ok(());
            };
            let targets = picked
                .iter()
                .filter_map(|label| {
                    pkg.targets
                        .iter()
                        .find(|t| target_label(&t.name, &t.arch) == *label)
                        .cloned()
                        .or_else(|| {
                            TARGET_REGISTRY
                                .iter()
                                .find(|t| target_label(t.name, t.arch) == *label)
                                .map(|t| Target::resolved(t.name, t.arch))
                        })
                })
                .collect::<Vec<_>>();
            pkg.matrix = !targets.is_empty();
            pkg.targets = targets;
        }
        _ => return Ok(()),
    }
    app.save()
}

fn apply_text(app: &mut App, field: Field, buffer: String) -> Result<()> {
    match field {
        Field::Provider => {
            if let Some(provider) = optional(&buffer) {
                app.config.provider = provider;
            } else {
                app.status = Some("Not saved: provider cannot be blank".into());
                return Ok(());
            }
        }
        Field::ToolVersion => app.config.otf_release_version = optional(&buffer),
        Field::NpmSecret | Field::CargoSecret => {
            let name = buffer.trim();
            if name.is_empty()
                || name.to_ascii_uppercase().starts_with("GITHUB_")
                || name.chars().next().unwrap().is_ascii_digit()
                || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                app.status = Some(
                    "Not saved: use a secret name with letters, digits, and underscores".into(),
                );
                return Ok(());
            }
            if field == Field::NpmSecret {
                app.config.secrets.npm = name.into();
            } else {
                app.config.secrets.cargo = name.into();
            }
        }
        Field::AddIgnorePaths => {
            let Some(name) = optional(&buffer) else {
                app.status = Some("Not saved: package name cannot be blank".into());
                return Ok(());
            };
            app.modal = Some(list(
                "Publish ignore globs",
                app.config.publish_ignore_paths_for(&name),
                Field::IgnorePaths(name),
            ));
            return Ok(());
        }
        Field::AddPackage => {
            let Some(name) = optional(&buffer) else {
                app.status = Some("Not saved: package name cannot be blank".into());
                return Ok(());
            };
            if app.config.package(&name).is_some() {
                app.status = Some("Not saved: this package already exists".into());
                return Ok(());
            }
            let entry: PackageEntry = toml::from_str(&format!(
                "name = {}\nadapter = \"generic\"\nmode = \"build-only\"\n",
                serde_json::to_string(&name).unwrap()
            ))?;
            app.config.packages.push(entry);
            app.goto(View::Package(name));
        }
        Field::TagFormat => return apply_choice(app, field, buffer),
        Field::PkgTagFormat => return apply_package_choice(app, field, buffer),
        Field::Target(index, part) => {
            let name = view_package(&app.view).unwrap().to_string();
            let pkg = app
                .config
                .packages
                .iter_mut()
                .find(|p| p.name == name)
                .unwrap();
            let value = buffer.trim().to_string();
            let invalid_identity = matches!(part, TargetPart::Name | TargetPart::Arch)
                && (value.is_empty()
                    || pkg.targets.iter().enumerate().any(|(i, target)| {
                        i != index
                            && if part == TargetPart::Name {
                                target.name == value && target.arch == pkg.targets[index].arch
                            } else {
                                target.name == pkg.targets[index].name && target.arch == value
                            }
                    }));
            if invalid_identity {
                app.status = Some(
                    "Not saved: target name and architecture must be nonempty and unique".into(),
                );
                return Ok(());
            }
            let target = &mut pkg.targets[index];
            match part {
                TargetPart::Name => target.name = value,
                TargetPart::Arch => target.arch = value,
                TargetPart::Triple => target.triple = value,
                TargetPart::Runner => target.runner = value,
                TargetPart::StageAs => target.stage_as = value,
                TargetPart::Ext => target.ext = value,
                _ => return Ok(()),
            }
        }
        Field::PkgName => {
            let old = view_package(&app.view).unwrap().to_string();
            let Some(name) = optional(&buffer) else {
                app.status = Some("Not saved: package name cannot be blank".into());
                return Ok(());
            };
            if name != old && app.config.package(&name).is_some() {
                app.status = Some("Not saved: this package name already exists".into());
                return Ok(());
            }
            app.config
                .packages
                .iter_mut()
                .find(|p| p.name == old)
                .unwrap()
                .name = name.clone();
            if let Some(paths) = app.config.publish.ignore_paths.remove(&old) {
                app.config.publish.ignore_paths.insert(name.clone(), paths);
            }
            for skipped in &mut app.config.skip_publish {
                if *skipped == old {
                    *skipped = name.clone();
                }
            }
            app.goto(View::Package(name));
        }

        Field::DefaultBranch => match optional(&buffer) {
            Some(branch) => app.config.default_branch = branch,
            None => {
                app.status = Some("Not saved: the default branch cannot be blank".into());
                return Ok(());
            }
        },
        Field::SnapshotTag => app.config.snapshot_tag = optional(&buffer),
        Field::Hook(stage) => set_hook_commands(
            &mut app.config,
            stage,
            if buffer.is_empty() {
                vec![]
            } else {
                vec![buffer]
            },
        ),
        Field::Setup(scope, index, part) => {
            return apply_setup_text(app, scope, index, part, buffer)
        }
        Field::PkgBinName
        | Field::PkgCommand
        | Field::PkgArtifacts
        | Field::PkgChangelog
        | Field::PkgManifest
        | Field::PkgVersionField
        | Field::PkgPublishCommand => return apply_package_text(app, field, buffer),
        _ => return Ok(()),
    }
    app.save()
}

/// The setup list a row edits, read-only.
fn setup_list<'a>(app: &'a App, scope: &SetupScope) -> Option<&'a SetupSteps> {
    scoped_setup(&app.config, scope)
}

/// The setup list a row edits, mutably.
///
/// A package's first setup edit materialises its own list from whatever the repo-wide one was
/// already giving it, so changing one field of an inherited list keeps the rest instead of
/// silently dropping what the package was getting.
fn setup_list_mut<'a>(app: &'a mut App, scope: &SetupScope) -> Option<&'a mut SetupSteps> {
    match scope {
        SetupScope::Repo => Some(&mut app.config.setup),
        SetupScope::Package(name) => {
            // Cloned before the packages are borrowed mutably.
            let repo_setup = app.config.setup.clone();
            let pkg = app.config.packages.iter_mut().find(|p| p.name == *name)?;
            Some(pkg.setup.get_or_insert(repo_setup))
        }
    }
}

fn apply_setup_text(
    app: &mut App,
    scope: SetupScope,
    index: usize,
    part: SetupPart,
    buffer: String,
) -> Result<()> {
    // Parsed before the list is borrowed, so a malformed `key=value` reports through `status`
    // instead of being written.
    let with = match part {
        SetupPart::With => match Setup::parse_with(&buffer) {
            Ok(with) => Some(with),
            Err(err) => {
                app.status = Some(format!("Not saved: {err}"));
                return Ok(());
            }
        },
        _ => None,
    };

    let Some(list) = setup_list_mut(app, &scope) else {
        return Ok(());
    };
    let restore = list.clone();
    let steps = list.steps_mut();
    let Some(step) = steps.get_mut(index) else {
        return Ok(());
    };
    match part {
        SetupPart::Uses => step.uses = optional(&buffer),
        SetupPart::With => step.with = with.unwrap_or_default(),
        SetupPart::Run => {
            step.run = if buffer.is_empty() {
                vec![]
            } else {
                vec![buffer]
            }
        }
        SetupPart::Targets => {
            step.targets = buffer
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        }
        SetupPart::Jobs => {
            step.jobs = buffer
                .split([',', '\n'])
                .filter_map(|name| JobKind::parse(name.trim()))
                .collect()
        }
    }
    // A step blanked of both its action and its script is dropped rather than left as a hole in an
    // ordered list. For a package this can empty the list, which is exactly how it opts out.
    let dropped = step.is_empty();
    if dropped {
        steps.remove(index);
    }

    if let Err(err) = list.validate("setup") {
        *list = restore;
        app.status = Some(format!("Not saved: {err}"));
        return Ok(());
    }
    // The step this view was showing is gone, so there is nothing left here to look at.
    if dropped {
        if let Some(parent) = View::SetupStep(scope, index).parent() {
            app.goto(parent);
        }
    }
    app.save()
}

fn apply_package_text(app: &mut App, field: Field, buffer: String) -> Result<()> {
    let View::Package(name) = app.view.clone() else {
        return Ok(());
    };
    let Some(pkg) = app.config.packages.iter_mut().find(|p| p.name == name) else {
        return Ok(());
    };
    match field {
        Field::PkgBinName => pkg.bin_name = optional(&buffer),
        Field::PkgCommand => pkg.command = buffer.trim().to_string(),
        Field::PkgArtifacts => pkg.artifacts = buffer.trim().to_string(),
        Field::PkgChangelog => {
            let previous = pkg.changelog.clone();
            pkg.changelog = optional(&buffer);
            if let Err(err) = pkg.validate_release_identity() {
                pkg.changelog = previous;
                app.status = Some(format!("Not saved: {err}"));
                return Ok(());
            }
        }
        Field::PkgManifest => pkg.manifest = optional(&buffer),
        Field::PkgVersionField => pkg.version_field = optional(&buffer),
        Field::PkgPublishCommand => pkg.publish = optional(&buffer),
        _ => return Ok(()),
    }
    app.save()
}

// ---------------------------------------------------------------------------
// drawing
// ---------------------------------------------------------------------------

fn draw(f: &mut Frame, app: &mut App, entries: &[Entry]) {
    let (_, row_lines) = screen_lines(entries, app.cursor);

    // Keep the focused row on screen without jumping: scroll only when it leaves the window.
    let visible = f.area().height.saturating_sub(5).max(1);
    if let Some(line) = row_lines.get(app.cursor).copied() {
        let line = line as u16;
        if line < app.scroll {
            app.scroll = line;
        } else if line >= app.scroll + visible {
            app.scroll = line - visible + 1;
        }
    }

    render_frame(
        f,
        entries,
        app.cursor,
        app.scroll,
        &format!(" {CONFIG_FILE} · {} ", app.config.provider),
        footer_lines(app, entries),
    );

    if let Some(modal) = &app.modal {
        draw_modal(f, modal, f.area());
    }
}

/// Draw the body and footer. The modal layer is the caller's business, so a test can snapshot the
/// screen underneath it.
fn render_frame(
    f: &mut Frame,
    entries: &[Entry],
    cursor: usize,
    scroll: u16,
    title: &str,
    footer: Vec<Line<'static>>,
) {
    let [body, footer_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(f.area());
    let (lines, _) = screen_lines(entries, cursor);

    f.render_widget(
        Paragraph::new(lines)
            .block(
                Block::bordered()
                    .title(Span::styled(
                        title.to_string(),
                        Style::new()
                            .fg(Color::Black)
                            .bg(ACCENT)
                            .add_modifier(Modifier::BOLD),
                    ))
                    .border_style(Style::new().fg(ACCENT)),
            )
            .scroll((scroll, 0)),
        body,
    );

    let dim = Style::new().fg(Color::DarkGray);
    f.render_widget(
        Paragraph::new(footer)
            .block(Block::bordered().border_style(dim))
            .wrap(Wrap { trim: true }),
        footer_area,
    );
}

fn footer_lines(app: &App, entries: &[Entry]) -> Vec<Line<'static>> {
    let dim = Style::new().fg(Color::DarkGray);
    let line = if let Some(status) = &app.status {
        Line::styled(status.clone(), Style::new().fg(Color::Green))
    } else if let Some(hint) = rows(entries).get(app.cursor).map(|r| r.hint) {
        Line::styled(hint.to_string(), dim)
    } else {
        Line::raw("")
    };

    let keys = match (&app.view, &app.modal) {
        (_, Some(Modal::List { .. })) => {
            "[enter] edit entry  [a/d] add/delete  [ctrl+s] save  [esc] cancel"
        }
        (_, Some(Modal::Check { .. })) => "[space] toggle  [enter] confirm  [esc] cancel",
        (_, Some(_)) => "[enter] confirm  [esc] cancel",
        (View::Settings, None) => "[↑↓/jk] move  [enter] edit  [q] quit",
        (_, None) => "[↑↓/jk] move  [enter] edit  [esc] back  [q] quit",
    };

    vec![line, Line::styled(keys.to_string(), dim)]
}

/// Render the entries, returning the line index of each selectable row so the caller can scroll.
fn screen_lines(entries: &[Entry], cursor: usize) -> (Vec<Line<'static>>, Vec<usize>) {
    let dim = Style::new().fg(Color::DarkGray);
    let mut lines = Vec::new();
    let mut row_lines = Vec::new();
    let mut index = 0usize;

    for entry in entries {
        match entry {
            Entry::Header(title) => {
                if !lines.is_empty() {
                    lines.push(Line::raw(""));
                }
                lines.push(Line::styled(
                    title.to_uppercase(),
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                ));
            }
            Entry::Row(r) => {
                let focused = index == cursor;
                row_lines.push(lines.len());
                let marker = if focused { "❯ " } else { "  " };
                let label = format!("{:<width$}", r.label, width = LABEL_WIDTH);
                let label_style = if focused {
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
                } else {
                    Style::new()
                };
                let value_style = if r.value.starts_with('(') {
                    dim
                } else if focused {
                    Style::new().add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(Color::Gray)
                };
                lines.push(Line::from(vec![
                    Span::styled(marker, label_style),
                    Span::styled(label, label_style),
                    Span::styled(r.value.clone(), value_style),
                ]));
                index += 1;
            }
        }
    }
    (lines, row_lines)
}

fn input_tail(buffer: &str, width: usize) -> String {
    buffer
        .replace('\n', "↵")
        .chars()
        .rev()
        .take(width)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn draw_modal(f: &mut Frame, modal: &Modal, area: Rect) {
    let body: Vec<Line<'static>> = match modal {
        Modal::List {
            items,
            cursor,
            editing,
            ..
        } => {
            let capacity = area.height.saturating_sub(10).max(1) as usize;
            let start = cursor.saturating_sub(capacity.saturating_sub(1));
            let mut lines = items
                .iter()
                .enumerate()
                .skip(start)
                .take(capacity)
                .map(|(i, value)| choice_line(&value.replace('\n', " ↵ "), i == *cursor))
                .collect::<Vec<_>>();
            if let Some(buffer) = editing {
                lines.push(Line::raw(format!(
                    "Edit: {}▏",
                    input_tail(buffer, area.width.saturating_sub(17).min(65) as usize)
                )));
                lines.push(Line::raw(
                    "Enter: accept · Alt+Enter: newline · Ctrl+u: clear · Esc: cancel",
                ));
            } else {
                if items.is_empty() {
                    lines.push(Line::raw("(empty)"));
                }
                lines.push(Line::raw(
                    "Enter: edit · a: add · d: delete · Ctrl+↑/↓: reorder",
                ));
                lines.push(Line::raw("Ctrl+s: save list · Esc: cancel"));
            }
            lines
        }
        Modal::Choice {
            options, cursor, ..
        } => options
            .iter()
            .enumerate()
            .map(|(i, o)| choice_line(o, i == *cursor))
            .collect(),
        Modal::Check {
            options,
            checked,
            cursor,
            ..
        } => options
            .iter()
            .enumerate()
            .map(|(i, o)| check_line(o, checked.get(i).copied().unwrap_or(false), i == *cursor))
            .collect(),
        Modal::Text { buffer, .. } => vec![
            Line::raw(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    input_tail(buffer, area.width.saturating_sub(13).min(70) as usize),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::styled("▏", Style::new().fg(ACCENT)),
            ]),
        ],
    };

    let height = (body.len() as u16 + 2).clamp(3, area.height.saturating_sub(4).max(3));
    let width = area.width.saturating_sub(8).clamp(20, 76);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    f.render_widget(Clear, rect);
    let focused = match modal {
        Modal::Choice { cursor, .. } | Modal::Check { cursor, .. } => *cursor,
        Modal::List { .. } => body.len().saturating_sub(1),
        Modal::Text { .. } => 0,
    };
    let scroll = focused.saturating_sub(rect.height.saturating_sub(3) as usize) as u16;
    f.render_widget(
        Paragraph::new(body).scroll((scroll, 0)).block(
            Block::bordered()
                .title(Span::styled(
                    format!(" {} ", modal.title()),
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                ))
                .border_style(Style::new().fg(ACCENT)),
        ),
        rect,
    );
}

fn choice_line(option: &str, focused: bool) -> Line<'static> {
    let marker = if focused { "❯ " } else { "  " };
    let style = if focused {
        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::new()
    };
    Line::from(vec![
        Span::styled(marker, style),
        Span::styled(option.to_string(), style),
    ])
}

fn check_line(option: &str, checked: bool, focused: bool) -> Line<'static> {
    let marker = if focused { "❯ " } else { "  " };
    let box_span = if checked {
        Span::styled("◉ ", Style::new().fg(Color::Green))
    } else {
        Span::styled("◯ ", Style::new().fg(Color::DarkGray))
    };
    let style = if focused {
        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::new()
    };
    Line::from(vec![
        Span::styled(marker, style),
        box_span,
        Span::styled(option.to_string(), style),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PackageEntry, PublishConfig};

    fn pkg(name: &str, adapter: Ecosystem, mode: Mode) -> PackageEntry {
        PackageEntry {
            name: name.to_string(),
            adapter,
            mode,
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
            provenance: false,
            executable: None,
            include: Vec::new(),
            tag_format: None,
            legacy_tag_formats: Vec::new(),
            changelog: None,
            setup: None,
            env: Default::default(),
        }
    }

    fn config() -> ReleaseConfig {
        ReleaseConfig {
            adapters: vec![Ecosystem::Npm, Ecosystem::Cargo],
            tag_format: "v{version}".into(),
            skip_publish: vec!["internal".into()],
            publish: PublishConfig::default(),
            secrets: Default::default(),
            packages: vec![pkg("@x/sdk", Ecosystem::Npm, Mode::Publish)],
            ..ReleaseConfig::default()
        }
    }

    fn value_of(entries: &[Entry], label: &str) -> String {
        rows(entries)
            .iter()
            .find(|r| r.label == label)
            .unwrap_or_else(|| panic!("no row labelled {label} in {entries:#?}"))
            .value
            .clone()
    }

    struct EmptyDiscovery;

    impl AdapterFactory for EmptyDiscovery {
        fn make(&self, _: Ecosystem) -> Box<dyn crate::adapter::Adapter> {
            Box::new(Self)
        }
    }

    impl crate::adapter::Adapter for EmptyDiscovery {
        fn discover_packages(&self) -> Result<Vec<crate::adapter::Pkg>> {
            Ok(Vec::new())
        }
        fn write_version(&self, _: &crate::adapter::Pkg, _: &str) -> Result<()> {
            unreachable!()
        }
        fn update_dep_range(&self, _: &crate::adapter::Pkg, _: &str, _: &str) -> Result<()> {
            unreachable!()
        }
        fn format_range(&self, _: &str) -> String {
            unreachable!()
        }
        fn resolve_workspace_links(&self, _: &crate::adapter::Pkg) -> Result<()> {
            unreachable!()
        }
        fn update_lockfile(&self, _: &Path) -> Result<()> {
            unreachable!()
        }
        fn dependent_bump(
            &self,
            _: crate::adapter::Bump,
            _: &crate::adapter::DepKind,
        ) -> crate::adapter::Bump {
            unreachable!()
        }
        fn is_published(&self, _: &crate::adapter::Pkg, _: &str) -> Result<bool> {
            unreachable!()
        }
        fn publish(&self, _: &crate::adapter::Pkg, _: Option<&Path>) -> Result<()> {
            unreachable!()
        }
    }

    fn test_app(root: &Path, config: ReleaseConfig) -> App<'static> {
        App {
            root: root.to_path_buf(),
            factory: &EmptyDiscovery,
            config,
            view: View::Package("@x/sdk".into()),
            cursor: 0,
            scroll: 0,
            modal: None,
            status: None,
            new_packages: Vec::new(),
        }
    }

    #[test]
    fn target_picker_preserves_overrides_and_resolves_musl_names() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config();
        let mut custom = Target::resolved("linux", "x86_64");
        custom.runner = "self-hosted".into();
        custom.triple = "custom-linux-triple".into();
        custom.cross = true;
        cfg.packages[0].targets = vec![custom.clone()];
        let mut app = test_app(root.path(), cfg);
        apply_check(
            &mut app,
            Field::PkgTargets,
            vec![
                target_label("linux", "x86_64"),
                target_label("linux-musl", "x86_64"),
            ],
        )
        .unwrap();
        let saved = ReleaseConfig::load(root.path()).unwrap();
        assert_eq!(
            saved.packages[0].targets,
            vec![custom, Target::resolved("linux-musl", "x86_64")]
        );
    }

    #[test]
    fn command_list_keyboard_edit_preserves_commas_and_can_cancel() {
        let root = tempfile::tempdir().unwrap();
        let mut app = test_app(root.path(), config());
        app.modal = Some(list("pre_publish", &[], Field::Hook(HookStage::PrePublish)));
        handle_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        )
        .unwrap();
        let command = "node -e 'console.log(1,2)'";
        for c in command.chars() {
            handle_modal_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            )
            .unwrap();
        }
        handle_modal_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).unwrap();
        handle_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        )
        .unwrap();
        assert_eq!(
            ReleaseConfig::load(root.path()).unwrap().hooks.pre_publish,
            vec![command]
        );
        app.modal = Some(list(
            "pre_publish",
            &[command.into()],
            Field::Hook(HookStage::PrePublish),
        ));
        handle_modal_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        )
        .unwrap();
        handle_modal_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).unwrap();
        assert_eq!(
            ReleaseConfig::load(root.path()).unwrap().hooks.pre_publish,
            vec![command]
        );
    }

    #[test]
    fn setup_list_preserves_shell_commands_and_input_values() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config();
        cfg.setup = Setup {
            uses: Some("example/action@v1".into()),
            ..Setup::default()
        }
        .into();
        let mut app = test_app(root.path(), cfg);
        let command = "printf '%s,%s' one two";
        apply_list(
            &mut app,
            Field::Setup(SetupScope::Repo, 0, SetupPart::Run),
            vec![command.into()],
        )
        .unwrap();
        apply_list(
            &mut app,
            Field::Setup(SetupScope::Repo, 0, SetupPart::With),
            vec!["values=one,two=three".into()],
        )
        .unwrap();
        let saved = ReleaseConfig::load(root.path()).unwrap();
        assert_eq!(saved.setup.steps()[0].run, vec![command]);
        assert_eq!(saved.setup.steps()[0].with["values"], "one,two=three");
    }

    /// `jobs` is picked from the kinds the scope can reach, and picking all of them is the same as
    /// picking none, so it is not written out.
    #[test]
    fn setup_jobs_are_picked_and_saved() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config();
        cfg.packages[0].setup = Some(
            Setup {
                uses: Some("Swatinem/rust-cache@v2".into()),
                ..Setup::default()
            }
            .into(),
        );
        let mut app = test_app(root.path(), cfg);
        let scope = SetupScope::Package("@x/sdk".into());
        let field = Field::Setup(scope.clone(), 0, SetupPart::Jobs);
        apply_check(&mut app, field.clone(), vec!["build".into()]).unwrap();
        let saved = ReleaseConfig::load(root.path()).unwrap();
        let step = &saved.package("@x/sdk").unwrap().setup.as_ref().unwrap().steps()[0];
        assert_eq!(step.jobs, vec![JobKind::Build]);
        assert_eq!(step_summary(step), "Swatinem/rust-cache@v2 · build only");

        apply_check(&mut app, field, selectable_jobs(&scope)).unwrap();
        let saved = ReleaseConfig::load(root.path()).unwrap();
        let step = &saved.package("@x/sdk").unwrap().setup.as_ref().unwrap().steps()[0];
        assert!(step.jobs.is_empty());
    }

    #[test]
    fn build_env_is_edited_as_key_value_entries() {
        let root = tempfile::tempdir().unwrap();
        let mut app = test_app(root.path(), config());
        apply_list(
            &mut app,
            Field::PkgEnv,
            vec!["ES_RUNTIME_INSPECTOR=1".into(), "FLAGS=a=b".into()],
        )
        .unwrap();
        let saved = ReleaseConfig::load(root.path()).unwrap();
        let env = &saved.package("@x/sdk").unwrap().env;
        assert_eq!(env["ES_RUNTIME_INSPECTOR"], "1");
        assert_eq!(env["FLAGS"], "a=b");

        // A name no shell accepts is refused, and the saved value is left alone.
        apply_list(&mut app, Field::PkgEnv, vec!["9LIVES=1".into()]).unwrap();
        assert!(app.status.as_deref().unwrap().starts_with("Not saved"));
        assert_eq!(ReleaseConfig::load(root.path()).unwrap().packages, saved.packages);
    }

    #[test]
    fn provenance_toggle_saves_and_upgrade_adds_or_removes_oidc_permission() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config();
        cfg.adapters = vec![Ecosystem::Npm];
        let mut app = App {
            root: root.path().to_path_buf(),
            factory: &EmptyDiscovery,
            config: cfg,
            view: View::Package("@x/sdk".into()),
            cursor: 0,
            scroll: 0,
            modal: None,
            status: None,
            new_packages: Vec::new(),
        };
        let entries = app.entries();
        assert_eq!(value_of(&entries, "Provenance"), "no");
        app.cursor = rows(&entries)
            .iter()
            .position(|row| row.field == Field::PkgProvenance)
            .unwrap();

        for enabled in [true, false] {
            open_editor(&mut app).unwrap();
            // The current value is focused; switching once selects the other value.
            handle_modal_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).unwrap();
            handle_modal_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).unwrap();
            assert!(app.modal.is_none());
            assert_eq!(
                value_of(&app.entries(), "Provenance"),
                if enabled { "yes" } else { "no" }
            );
            let saved = ReleaseConfig::load(root.path()).unwrap();
            assert_eq!(saved.package("@x/sdk").unwrap().provenance, enabled);
            if enabled {
                assert!(app.status.as_ref().unwrap().contains("release upgrade"));
            }
            crate::upgrade::orchestrate(
                root.path(),
                &crate::upgrade::UpgradeOptions { force: true },
            )
            .unwrap();
            let yaml =
                std::fs::read_to_string(root.path().join(".github/workflows/release.yml")).unwrap();
            assert_eq!(yaml.contains("id-token: write"), enabled, "{yaml}");
        }
    }

    #[test]
    fn provenance_toggle_is_only_shown_for_npm_publish_packages() {
        for (adapter, mode, visible) in [
            (Ecosystem::Npm, Mode::Publish, true),
            (Ecosystem::Npm, Mode::BuildOnly, false),
            (Ecosystem::Cargo, Mode::Publish, false),
            (Ecosystem::Generic, Mode::BuildOnly, false),
        ] {
            let mut cfg = config();
            cfg.packages = vec![pkg("app", adapter, mode)];
            let entries = build(&cfg, &View::Package("app".into()), &[]);
            assert_eq!(
                rows(&entries)
                    .iter()
                    .any(|row| row.field == Field::PkgProvenance),
                visible
            );
        }
    }

    /// The whole point of the screen: every setting shows what it is currently set to, without
    /// opening it. The old menu showed only names, so the value was one prompt away at all times.
    #[test]
    fn repository_controls_save_and_change_generated_workflow() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config();
        cfg.packages
            .push(pkg("rust-lib", Ecosystem::Cargo, Mode::Publish));
        let mut app = test_app(root.path(), cfg);
        app.goto(View::Settings);
        apply_text(&mut app, Field::ToolVersion, "v0.32.0".into()).unwrap();
        apply_text(&mut app, Field::NpmSecret, "ORG_NPM_TOKEN".into()).unwrap();
        apply_text(&mut app, Field::CargoSecret, "ORG_CARGO_TOKEN".into()).unwrap();
        apply_list(
            &mut app,
            Field::DiscoveryNpm,
            vec!["packages/*".into(), "!packages/private".into()],
        )
        .unwrap();
        apply_list(
            &mut app,
            Field::IgnorePaths("@x/sdk".into()),
            vec!["**/*.md".into()],
        )
        .unwrap();
        apply_choice(&mut app, Field::TagFormat, "Custom…".into()).unwrap();
        assert!(matches!(app.modal, Some(Modal::Text { .. })));
        apply_text(&mut app, Field::TagFormat, "sdk-{version}".into()).unwrap();
        apply_list(
            &mut app,
            Field::LegacyTagFormats,
            vec!["old-sdk-{version}".into()],
        )
        .unwrap();
        let saved = ReleaseConfig::load(root.path()).unwrap();
        assert_eq!(saved.secrets.cargo, "ORG_CARGO_TOKEN");
        assert_eq!(saved.discovery.npm, vec!["packages/*", "!packages/private"]);
        assert_eq!(saved.publish_ignore_paths_for("@x/sdk"), ["**/*.md"]);
        let workflow = crate::init::render_workflow_for_root(&saved, root.path());
        assert!(workflow.contains("secrets.ORG_NPM_TOKEN"), "{workflow}");
        assert!(workflow.contains("secrets.ORG_CARGO_TOKEN"), "{workflow}");
        assert!(workflow.contains("/v0.32.0/install.sh"), "{workflow}");
        assert_eq!(saved.tag_format, "sdk-{version}");
        assert_eq!(saved.legacy_tag_formats, vec!["old-sdk-{version}"]);
    }

    #[test]
    fn package_controls_round_trip_and_restore_inherited_setup() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config();
        cfg.packages[0].adapter = Ecosystem::Generic;
        cfg.packages[0].mode = Mode::BuildOnly;
        cfg.setup = Setup {
            uses: Some("shared/action@v1".into()),
            ..Setup::default()
        }
        .into();
        cfg.packages[0].setup = Some(SetupSteps::default());
        let mut app = test_app(root.path(), cfg);
        apply_text(&mut app, Field::PkgBinName, "sdk-cli".into()).unwrap();
        apply_text(&mut app, Field::PkgManifest, "package.json".into()).unwrap();
        apply_text(&mut app, Field::PkgVersionField, "metadata.version".into()).unwrap();
        apply_choice(&mut app, Field::PkgCompress, "brotli".into()).unwrap();
        apply_choice(&mut app, Field::PkgArchive, "zip".into()).unwrap();
        apply_choice(&mut app, Field::PkgExecutable, "no".into()).unwrap();
        apply_list(
            &mut app,
            Field::PkgInclude,
            vec!["LICENSE".into(), "types/*.d.ts".into()],
        )
        .unwrap();
        apply_list(
            &mut app,
            Field::PkgLegacyTags,
            vec!["sdk-old-{version}".into()],
        )
        .unwrap();
        apply_text(&mut app, Field::PkgTagFormat, "sdk-new-{version}".into()).unwrap();
        let saved = ReleaseConfig::load(root.path()).unwrap();
        let pkg = saved.package("@x/sdk").unwrap();
        assert_eq!(pkg.bin_name.as_deref(), Some("sdk-cli"));
        assert_eq!(pkg.compress.as_deref(), Some("brotli"));
        assert_eq!(pkg.archive, Some(ArchiveFormat::Zip));
        assert_eq!(pkg.executable, Some(false));
        assert_eq!(pkg.include, vec!["LICENSE", "types/*.d.ts"]);
        assert_eq!(pkg.legacy_tag_formats, vec!["sdk-old-{version}"]);
        assert_eq!(pkg.manifest.as_deref(), Some("package.json"));
        assert_eq!(pkg.version_field.as_deref(), Some("metadata.version"));
        let entries = app.entries();
        app.cursor = rows(&entries)
            .iter()
            .position(|r| r.field == Field::RestoreSetup)
            .unwrap();
        open_editor(&mut app).unwrap();
        let saved = ReleaseConfig::load(root.path()).unwrap();
        assert!(saved.packages[0].setup.is_none());
        assert_eq!(
            effective_setup(&saved, &saved.packages[0]).steps()[0]
                .uses
                .as_deref(),
            Some("shared/action@v1")
        );
    }

    #[test]
    fn target_detail_edit_and_custom_package_creation_persist() {
        let root = tempfile::tempdir().unwrap();
        let mut app = test_app(root.path(), config());
        apply_text(&mut app, Field::AddPackage, "custom-app".into()).unwrap();
        assert_eq!(app.view, View::Package("custom-app".into()));
        apply_choice(&mut app, Field::PkgAdapter, "generic".into()).unwrap();
        apply_text(&mut app, Field::PkgManifest, "version.json".into()).unwrap();
        app.goto(View::Targets("custom-app".into()));
        app.cursor = 0;
        open_editor(&mut app).unwrap();
        for (part, value) in [
            (TargetPart::Name, "custom-os"),
            (TargetPart::Arch, "arm64"),
            (TargetPart::Triple, "aarch64-unknown-linux-musl"),
            (TargetPart::Runner, "self-hosted"),
            (TargetPart::StageAs, "custom-arm64"),
            (TargetPart::Ext, ".bin"),
        ] {
            apply_text(&mut app, Field::Target(0, part), value.into()).unwrap();
        }
        apply_choice(&mut app, Field::Target(0, TargetPart::Cross), "yes".into()).unwrap();
        apply_choice(&mut app, Field::Target(0, TargetPart::Vm), "yes".into()).unwrap();
        let saved = ReleaseConfig::load(root.path()).unwrap();
        let pkg = saved.package("custom-app").unwrap();
        assert!(pkg.matrix);
        assert_eq!(
            pkg.targets[0],
            Target {
                name: "custom-os".into(),
                arch: "arm64".into(),
                triple: "aarch64-unknown-linux-musl".into(),
                runner: "self-hosted".into(),
                stage_as: "custom-arm64".into(),
                ext: ".bin".into(),
                cross: true,
                vm: true,
            }
        );
    }

    #[test]
    fn invalid_values_do_not_replace_saved_config() {
        let root = tempfile::tempdir().unwrap();
        let mut app = test_app(root.path(), config());
        app.save().unwrap();
        let before = std::fs::read_to_string(root.path().join(CONFIG_FILE)).unwrap();
        apply_text(&mut app, Field::NpmSecret, "invalid secret".into()).unwrap();
        apply_list(
            &mut app,
            Field::IgnorePaths("@x/sdk".into()),
            vec!["[bad".into()],
        )
        .unwrap();
        apply_list(
            &mut app,
            Field::PkgLegacyTags,
            vec!["no-version-placeholder".into()],
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join(CONFIG_FILE)).unwrap(),
            before
        );
    }

    /// The inventory must be updated when a configuration field is added. Row/editor tests
    /// separately verify the declared controls actually open and save their values.
    #[test]
    fn schema_inventory_covers_every_configuration_field() {
        let schema = include_str!("config.rs");
        for (structure, expected) in [
            ("ReleaseConfig", "adapters otf_release_version skip_publish hooks setup publish secrets discovery packages snapshot_tag tag_format legacy_tag_formats provider default_branch changelog_strategy changelog_scope github_release_notes"),
            ("PackageEntry", "name adapter mode matrix targets command artifacts bin_name compress manifest version_field publish archive attest provenance checksums include executable tag_format legacy_tag_formats changelog setup env"),
            ("Target", "name arch triple runner stage_as ext cross vm"),
            ("Setup", "uses with run targets jobs"),
            ("Hooks", "pre_version post_version pre_publish post_publish"),
            ("Secrets", "npm cargo"), ("PublishConfig", "ignore_paths"), ("Discovery", "npm"),
        ] {
            let declaration = schema.split(&format!("pub struct {structure} {{")).nth(1).unwrap().split("\n}").next().unwrap();
            let mut actual = declaration.lines().filter_map(|line| line.trim().strip_prefix("pub ").and_then(|line| line.split_once(':').map(|(name,_)| name.to_string()))).collect::<Vec<_>>();
            let mut expected = expected.split_whitespace().map(str::to_string).collect::<Vec<_>>();
            actual.sort(); expected.sort();
            assert_eq!(actual, expected, "update the TUI coverage inventory for {structure}");
        }
    }

    #[test]
    fn long_list_editor_shows_focused_entry_and_save_controls() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        let modal = Modal::List {
            title: "Commands".into(),
            items: (0..40).map(|i| format!("echo command-{i}")).collect(),
            cursor: 39,
            editing: None,
            field: Field::Hook(HookStage::PreVersion),
        };
        terminal
            .draw(|frame| draw_modal(frame, &modal, frame.area()))
            .unwrap();
        let screen = buffer_text(terminal.backend()).join("\n");
        assert!(screen.contains("❯ echo command-39"), "{screen}");
        assert!(screen.contains("Ctrl+s: save list"), "{screen}");
    }

    #[test]
    fn matrix_npm_package_shows_effective_publish_controls() {
        let mut cfg = config();
        cfg.packages[0].mode = Mode::BuildOnly;
        cfg.packages[0].matrix = true;
        let entries = build(&cfg, &View::Package("@x/sdk".into()), &[]);
        assert!(rows(&entries)
            .iter()
            .any(|row| row.field == Field::PkgProvenance));
        assert!(!rows(&entries)
            .iter()
            .any(|row| row.field == Field::PkgArchive));
    }

    #[test]
    fn missing_controls_are_reachable_in_their_screens() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config();
        cfg.packages[0].adapter = Ecosystem::Generic;
        cfg.packages[0].mode = Mode::BuildOnly;
        cfg.packages[0].targets = vec![Target::resolved("linux-musl", "x86_64")];
        let mut app = test_app(root.path(), cfg);
        for (view, fields) in [
            (
                View::Settings,
                vec![
                    Field::ToolVersion,
                    Field::NpmSecret,
                    Field::CargoSecret,
                    Field::DiscoveryNpm,
                ],
            ),
            (
                View::Package("@x/sdk".into()),
                vec![
                    Field::PkgName,
                    Field::PkgAdapter,
                    Field::PkgMatrix,
                    Field::PkgBinName,
                    Field::PkgCompress,
                    Field::PkgArchive,
                    Field::PkgInclude,
                    Field::PkgExecutable,
                    Field::PkgLegacyTags,
                    Field::PkgManifest,
                    Field::PkgVersionField,
                    Field::PkgPublishCommand,
                ],
            ),
            (
                View::Target("@x/sdk".into(), 0),
                TargetPart::ALL
                    .iter()
                    .map(|part| Field::Target(0, *part))
                    .collect(),
            ),
        ] {
            app.goto(view);
            for field in fields {
                let entries = app.entries();
                app.cursor = rows(&entries)
                    .iter()
                    .position(|row| row.field == field)
                    .unwrap_or_else(|| panic!("missing row {field:?}"));
                open_editor(&mut app).unwrap();
                assert!(app.modal.is_some(), "missing editor {field:?}");
                app.modal = None;
            }
        }
    }

    #[test]
    fn every_setting_row_carries_its_current_value() {
        let entries = build(&config(), &View::Settings, &[]);

        assert_eq!(value_of(&entries, "Tag format"), "v{version}");
        assert_eq!(value_of(&entries, "Enabled"), "npm, crates.io");
        assert_eq!(value_of(&entries, "Never publish"), "internal");
        assert_eq!(value_of(&entries, "Scope"), "package");
        assert_eq!(value_of(&entries, "Strategy"), "curated");
        // A package's row summarises it rather than making you open it to find out.
        assert_eq!(value_of(&entries, "@x/sdk"), "npm · publish");
    }

    #[test]
    fn a_setup_list_is_one_row_per_step() {
        let mut cfg = config();
        cfg.setup = vec![
            Setup {
                uses: Some("./.github/actions/setup-tsr".into()),
                with: Setup::parse_with("esdev=true").unwrap(),
                ..Setup::default()
            },
            Setup {
                uses: Some("./.github/actions/setup-esdev".into()),
                targets: vec!["x86_64-unknown-linux-gnu".into()],
                ..Setup::default()
            },
        ]
        .into();
        let entries = build(&cfg, &View::Settings, &[]);

        // One line per step, not one per field of each step.
        assert_eq!(value_of(&entries, "Step 1"), "./.github/actions/setup-tsr");
        assert_eq!(
            value_of(&entries, "Step 2"),
            "./.github/actions/setup-esdev · 1 target"
        );
        assert!(rows(&entries).iter().any(|r| r.label == "Add step"));
        assert!(
            !rows(&entries).iter().any(|r| r.label.contains("action")),
            "the fields belong to the step's own view: {entries:#?}"
        );
    }

    /// A script-only step is summarised by how much it runs — a `curl … | bash` line is longer
    /// than the column it would have to fit in.
    #[test]
    fn a_script_step_is_summarised_by_its_command_count() {
        let mut cfg = config();
        cfg.setup = Setup {
            run: vec![
                "curl -fsSL https://example.com/i.sh | bash".into(),
                "hash -r".into(),
            ],
            ..Setup::default()
        }
        .into();
        let entries = build(&cfg, &View::Settings, &[]);

        assert_eq!(value_of(&entries, "Step 1"), "2 commands");
    }

    /// Opening a step is where its four fields live, under a heading naming the list it is in.
    #[test]
    fn a_step_view_shows_its_fields_and_a_way_to_delete_it() {
        let mut cfg = config();
        cfg.setup = Setup {
            uses: Some("./.github/actions/setup-tsr".into()),
            with: Setup::parse_with("esdev=true").unwrap(),
            ..Setup::default()
        }
        .into();
        let entries = build(&cfg, &View::SetupStep(SetupScope::Repo, 0), &[]);

        assert!(
            matches!(&entries[0], Entry::Header(h) if h == "Build setup · step 1"),
            "{entries:#?}"
        );
        assert_eq!(value_of(&entries, "Action"), "./.github/actions/setup-tsr");
        assert_eq!(value_of(&entries, "Action inputs"), "esdev=true");
        assert_eq!(value_of(&entries, "Script"), "(none)");
        // An unfiltered step runs everywhere, which is "all", not "nothing set".
        assert_eq!(value_of(&entries, "Targets"), "all");
        assert!(rows(&entries).iter().any(|r| r.label == "Remove step"));
    }

    /// Esc retraces the way in: a step opened from a package returns to that package, not to the
    /// top of the settings screen.
    #[test]
    fn a_step_view_goes_back_where_it_was_opened_from() {
        assert_eq!(
            View::SetupStep(SetupScope::Repo, 0).parent(),
            Some(View::Settings)
        );
        assert_eq!(
            View::SetupStep(SetupScope::Package("@x/sdk".into()), 1).parent(),
            Some(View::Package("@x/sdk".into()))
        );
    }

    /// The triples on offer are the ones the packages in scope actually build, so a filter cannot
    /// be written against a triple that will never match.
    #[test]
    fn target_options_come_from_what_the_packages_in_scope_build() {
        let mut cfg = config();
        cfg.packages.push(PackageEntry {
            matrix: true,
            targets: vec![
                Target::resolved("linux", "x86_64"),
                Target::resolved("windows", "x86_64"),
            ],
            ..pkg("cli", Ecosystem::Cargo, Mode::BuildOnly)
        });

        assert_eq!(
            selectable_triples(&cfg, &SetupScope::Repo, &[]),
            vec!["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"]
        );
        // An npm package builds no triple, so its own steps have nothing to be filtered to.
        assert!(selectable_triples(&cfg, &SetupScope::Package("@x/sdk".into()), &[]).is_empty());
        // A triple already on the step survives even when no package declares it any more.
        assert_eq!(
            selectable_triples(
                &cfg,
                &SetupScope::Package("@x/sdk".into()),
                &["gone".into()]
            ),
            vec!["gone"]
        );
    }

    /// A package with no list of its own shows what it inherits, labelled, so the rows cannot be
    /// misread as settings the package has made.
    #[test]
    fn a_package_without_its_own_list_shows_the_repo_default() {
        let mut cfg = config();
        cfg.setup = Setup {
            uses: Some("./.github/actions/setup-tsr".into()),
            ..Setup::default()
        }
        .into();
        let entries = build(&cfg, &View::Package("@x/sdk".into()), &[]);

        assert_eq!(
            value_of(&entries, "Step 1"),
            "(repo default: ./.github/actions/setup-tsr)"
        );
    }

    /// A package that has opted out says so, rather than showing the repo-wide list it is not
    /// running.
    #[test]
    fn a_package_that_opted_out_shows_no_steps() {
        let mut cfg = config();
        cfg.setup = Setup {
            uses: Some("./.github/actions/setup-tsr".into()),
            ..Setup::default()
        }
        .into();
        cfg.packages[0].setup = Some(SetupSteps::default());
        let entries = build(&cfg, &View::Package("@x/sdk".into()), &[]);

        assert_eq!(value_of(&entries, "Steps"), "none");
    }

    /// Unset values read as what the repo will actually do, not as blanks — the parenthesised form
    /// is also what the renderer dims.
    #[test]
    fn unset_values_name_the_fallback_instead_of_showing_nothing() {
        let entries = build(&config(), &View::Settings, &[]);
        assert_eq!(value_of(&entries, "Legacy tag formats"), "(none)");
        assert_eq!(value_of(&entries, "Snapshot tag"), "(none)");
        assert_eq!(value_of(&entries, "pre_version"), "(none)");

        let entries = build(&config(), &View::Package("@x/sdk".into()), &[]);
        assert_eq!(
            value_of(&entries, "Tag format"),
            "(repo default: v{version})"
        );
        assert_eq!(
            value_of(&entries, "Changelog"),
            "(repo default: package scope)"
        );
    }

    /// A matrix package's target count is the thing you actually want to see at a glance.
    #[test]
    fn a_matrix_package_row_shows_how_many_targets_it_builds() {
        let mut cfg = config();
        cfg.packages.push(PackageEntry {
            matrix: true,
            targets: vec![
                Target::resolved("linux", "x86_64"),
                Target::resolved("macos", "aarch64"),
            ],
            ..pkg("cli", Ecosystem::Cargo, Mode::BuildOnly)
        });
        let entries = build(&cfg, &View::Settings, &[]);
        assert_eq!(
            value_of(&entries, "cli"),
            "crates.io · build-only, matrix ×2"
        );
    }

    /// Packages the repo has but release.toml does not appear in the same list, marked — and they
    /// are rows you act on, not decoration.
    #[test]
    fn unconfigured_packages_are_listed_as_new() {
        let entries = build(&config(), &View::Settings, &["es-runtime-lsp".to_string()]);
        let row = rows(&entries)
            .into_iter()
            .find(|r| r.label == "es-runtime-lsp")
            .expect("new package listed");
        assert!(row.value.contains("[new]"), "{}", row.value);
        assert_eq!(row.field, Field::AdoptPackage("es-runtime-lsp".into()));
    }

    /// Build-only settings only exist for build-only packages, so the screen must not offer them
    /// on a package that publishes to a registry.
    #[test]
    fn asset_rows_appear_only_for_build_only_packages() {
        let mut cfg = config();
        cfg.packages
            .push(pkg("cli", Ecosystem::Cargo, Mode::BuildOnly));

        let publish_rows = build(&cfg, &View::Package("@x/sdk".into()), &[]);
        assert!(!rows(&publish_rows).iter().any(|r| r.label == "Checksums"));

        let build_rows = build(&cfg, &View::Package("cli".into()), &[]);
        assert!(rows(&build_rows).iter().any(|r| r.label == "Checksums"));
        assert!(rows(&build_rows)
            .iter()
            .any(|r| r.label == "Build provenance"));
    }

    /// Generic packages carry three fields no other adapter has; they must not clutter the others.
    #[test]
    fn generic_only_rows_are_scoped_to_generic_packages() {
        let mut cfg = config();
        cfg.packages
            .push(pkg("deno-lib", Ecosystem::Generic, Mode::Publish));

        let generic = build(&cfg, &View::Package("deno-lib".into()), &[]);
        assert!(rows(&generic).iter().any(|r| r.label == "Publish command"));

        let npm = build(&cfg, &View::Package("@x/sdk".into()), &[]);
        assert!(!rows(&npm).iter().any(|r| r.label == "Publish command"));
    }

    /// The cursor indexes selectable rows, but scrolling works in screen lines. A wrong mapping
    /// scrolls to the wrong place as soon as a section header is above the cursor.
    #[test]
    fn row_line_indices_account_for_headers_and_blank_lines() {
        let entries = build(&config(), &View::Settings, &[]);
        let (lines, row_lines) = screen_lines(&entries, 0);

        assert_eq!(row_lines.len(), rows(&entries).len());
        // First header, then the first row directly under it.
        assert_eq!(row_lines[0], 1);
        for (i, line) in row_lines.iter().enumerate() {
            let rendered = &lines[*line];
            let text: String = rendered.spans.iter().map(|s| s.content.clone()).collect();
            assert!(
                text.contains(rows(&entries)[i].label.as_str()),
                "row {i} maps to the wrong line: {text}"
            );
        }
    }

    /// Render for real, into a fixed-size buffer. The model tests above prove the right values are
    /// computed; this proves they survive layout — that the value column is not pushed off a
    /// narrow terminal, and that a modal actually covers the rows underneath it.
    #[test]
    fn the_screen_renders_labels_and_values_on_one_line() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let entries = build(&config(), &View::Settings, &[]);
        let (lines, _) = screen_lines(&entries, 0);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| {
                f.render_widget(Paragraph::new(lines.clone()), f.area());
            })
            .unwrap();

        let rendered = buffer_text(terminal.backend());
        assert!(
            rendered
                .iter()
                .any(|l| l.contains("Tag format") && l.contains("v{version}")),
            "label and value must share a line: {rendered:#?}"
        );
        assert!(rendered.iter().any(|l| l.contains("REPOSITORY")));
        // The focused row is marked, so the cursor is visible without relying on colour alone.
        assert!(rendered.iter().any(|l| l.trim_start().starts_with('❯')));
    }

    #[test]
    fn a_modal_covers_the_rows_underneath_it() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let entries = build(&config(), &View::Settings, &[]);
        let (lines, _) = screen_lines(&entries, 0);
        let modal = check(
            "Enabled ecosystems",
            vec!["npm".into(), "crates.io".into()],
            &["npm".to_string()],
            Field::Ecosystems,
        );

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| {
                f.render_widget(Paragraph::new(lines.clone()), f.area());
                draw_modal(f, &modal, f.area());
            })
            .unwrap();

        let rendered = buffer_text(terminal.backend());
        assert!(rendered.iter().any(|l| l.contains("Enabled ecosystems")));
        // Checked and unchecked states are distinguishable in the buffer, not just by colour.
        assert!(rendered
            .iter()
            .any(|l| l.contains("◉") && l.contains("npm")));
        assert!(rendered
            .iter()
            .any(|l| l.contains("◯") && l.contains("crates.io")));
    }

    fn buffer_text(backend: &ratatui::backend::TestBackend) -> Vec<String> {
        let buffer = backend.buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    /// A snapshot of the finished layout. The other render tests check individual guarantees; this
    /// one catches the whole thing shifting — a column that stops lining up, a header that loses
    /// its blank line, a footer that eats a row of settings.
    #[test]
    fn the_finished_screen_lays_out_as_expected() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let entries = build(&config(), &View::Settings, &["new-pkg".to_string()]);
        let mut terminal = Terminal::new(TestBackend::new(64, 30)).unwrap();
        terminal
            .draw(|f| {
                render_frame(
                    f,
                    &entries,
                    2,
                    0,
                    " release.toml · github ",
                    vec![Line::raw("how release tags are named")],
                )
            })
            .unwrap();
        let screen = buffer_text(terminal.backend());

        let trimmed: Vec<&str> = screen.iter().map(|l| l.trim_end()).collect();
        assert_eq!(
            trimmed[1],
            "│REPOSITORY                                                    │"
        );
        assert_eq!(
            trimmed[2],
            "│  Provider                  github                            │"
        );
        // Row 2 is focused, so it carries the marker and nothing else does.
        assert_eq!(
            trimmed[4],
            "│❯ Tag format                v{version}                        │"
        );
        assert_eq!(trimmed.iter().filter(|l| l.contains('❯')).count(), 1);
        // A blank line separates each section even as new settings are added.
        let changelog = trimmed
            .iter()
            .position(|line| line.contains("CHANGELOG"))
            .unwrap();
        assert_eq!(
            trimmed[changelog - 1],
            "│                                                              │"
        );
        assert!(trimmed.iter().any(|line| line.contains("REGISTRY SECRETS")));
        // The focused row's hint occupies the footer.
        assert!(
            screen
                .iter()
                .any(|l| l.contains("how release tags are named")),
            "{screen:#?}"
        );
    }

    /// A package whose block was removed while its view was open must not panic the screen.
    #[test]
    fn a_package_view_for_a_missing_package_degrades_to_a_message() {
        let entries = build(&config(), &View::Package("gone".into()), &[]);
        assert!(rows(&entries).is_empty());
        assert!(matches!(&entries[0], Entry::Header(h) if h.contains("no longer configured")));
    }
}
