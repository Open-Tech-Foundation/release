//! `otf-release config` — a full-screen settings editor.
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
    format_tag, ChangelogScope, ChangelogStrategy, Ecosystem, GithubReleaseNotes, Mode,
    ReleaseConfig, Target, COMMON_TAG_FORMATS, CONFIG_FILE, DEFAULT_VERSION_FIELD, TARGET_REGISTRY,
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
    PkgTagFormat,
    PkgChangelog,
    PkgManifest,
    PkgVersionField,
    PkgPublishCommand,
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
    /// A package's own fields, by name — names survive the re-sort that adopting a package causes.
    Package(String),
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
        View::Package(name) => package_entries(config, name),
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

    out.push(Entry::Header("Hooks".into()));
    for stage in HookStage::ALL {
        let commands = hook_commands(config, stage);
        out.push(row(
            stage.label(),
            list_or_none(commands),
            Field::Hook(stage),
            "shell commands run around the release, comma-separated",
        ));
    }

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

    out
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

    if pkg.is_build_only() {
        out.push(Entry::Header("Release assets".into()));
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

    if pkg.adapter == Ecosystem::Generic {
        out.push(Entry::Header("Generic adapter".into()));
        out.push(row(
            "Manifest",
            pkg.manifest.clone().unwrap_or_else(|| "(none)".into()),
            Field::PkgManifest,
            "the file carrying this package's version",
        ));
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
            | Modal::Text { title, .. } => title,
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

    fn refresh_new_packages(&mut self) {
        self.new_packages = unconfigured_packages(&self.config, self.factory).unwrap_or_default();
    }

    fn save(&mut self) -> Result<()> {
        self.config.save(&self.root)?;
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
         directly, then run `otf-release doctor` to check the result."
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
        KeyCode::Esc => match &app.view {
            // Esc is "back" everywhere else in this tool; at the top there is nowhere to go.
            View::Package(_) => {
                app.view = View::Settings;
                app.cursor = 0;
                app.scroll = 0;
            }
            View::Settings => return Ok(false),
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
        Field::OpenPackage(name) => {
            app.view = View::Package(name.clone());
            app.cursor = 0;
            app.scroll = 0;
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
        Field::Provider => choice(
            "Git hosting provider",
            vec!["github".into()],
            &config.provider,
            Field::Provider,
        ),
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
            choice("Tag format", options, &config.tag_format, Field::TagFormat)
        }
        Field::LegacyTagFormats => {
            let mut options: Vec<String> = COMMON_TAG_FORMATS
                .iter()
                .map(|f| (*f).to_string())
                .collect();
            for extra in &config.legacy_tag_formats {
                if !options.contains(extra) {
                    options.push(extra.clone());
                }
            }
            options.retain(|f| *f != config.tag_format);
            check(
                "Tag formats read as release history",
                options,
                &config.legacy_tag_formats,
                Field::LegacyTagFormats,
            )
        }
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
        Field::Hook(stage) => text(
            &format!("{} commands (comma-separated)", stage.label()),
            &hook_commands(config, *stage).join(", "),
            Field::Hook(*stage),
        ),
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

fn handle_modal_key(app: &mut App, key: KeyEvent) -> Result<()> {
    let Some(modal) = app.modal.as_mut() else {
        return Ok(());
    };
    match modal {
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

fn parse_csv(text: &str) -> Vec<String> {
    text.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
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

fn apply_choice(app: &mut App, field: Field, picked: String) -> Result<()> {
    match field {
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
        Field::PkgMode | Field::PkgChecksums | Field::PkgAttest | Field::PkgTagFormat => {
            return apply_package_choice(app, field, picked)
        }
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
        Field::PkgMode => {
            pkg.mode = if picked == "publish" {
                Mode::Publish
            } else {
                Mode::BuildOnly
            }
        }
        Field::PkgChecksums => pkg.checksums = picked == "yes",
        Field::PkgAttest => {
            pkg.attest = picked == "yes";
            if pkg.attest {
                app.status = Some("Run `otf-release upgrade` to add the signing step".into());
            }
        }
        Field::PkgTagFormat => {
            pkg.tag_format = (!picked.starts_with("(repo default")).then_some(picked);
        }
        _ => return Ok(()),
    }
    app.save()
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
            let targets: Vec<Target> = picked
                .iter()
                .filter_map(|label| label.split_once('-'))
                .map(|(name, arch)| Target::resolved(name, arch))
                .collect();
            let Some(pkg) = app.config.packages.iter_mut().find(|p| p.name == name) else {
                return Ok(());
            };
            pkg.matrix = !targets.is_empty();
            pkg.targets = targets;
        }
        _ => return Ok(()),
    }
    app.save()
}

fn apply_text(app: &mut App, field: Field, buffer: String) -> Result<()> {
    match field {
        Field::DefaultBranch => match optional(&buffer) {
            Some(branch) => app.config.default_branch = branch,
            None => {
                app.status = Some("Not saved: the default branch cannot be blank".into());
                return Ok(());
            }
        },
        Field::SnapshotTag => app.config.snapshot_tag = optional(&buffer),
        Field::Hook(stage) => set_hook_commands(&mut app.config, stage, parse_csv(&buffer)),
        Field::PkgCommand
        | Field::PkgArtifacts
        | Field::PkgChangelog
        | Field::PkgManifest
        | Field::PkgVersionField
        | Field::PkgPublishCommand => return apply_package_text(app, field, buffer),
        _ => return Ok(()),
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
        Field::PkgCommand => pkg.command = buffer.trim().to_string(),
        Field::PkgArtifacts => pkg.artifacts = buffer.trim().to_string(),
        Field::PkgChangelog => {
            pkg.changelog = optional(&buffer);
            if let Err(err) = pkg.validate_release_identity() {
                pkg.changelog = None;
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
        (_, Some(Modal::Check { .. })) => "[space] toggle  [enter] confirm  [esc] cancel",
        (_, Some(_)) => "[enter] confirm  [esc] cancel",
        (View::Package(_), None) => "[↑↓/jk] move  [enter] edit  [esc] back  [q] quit",
        (View::Settings, None) => "[↑↓/jk] move  [enter] edit  [q] quit",
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

fn draw_modal(f: &mut Frame, modal: &Modal, area: Rect) {
    let body: Vec<Line<'static>> = match modal {
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
                Span::styled(buffer.clone(), Style::new().add_modifier(Modifier::BOLD)),
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
    f.render_widget(
        Paragraph::new(body).block(
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
            executable: None,
            include: Vec::new(),
            tag_format: None,
            changelog: None,
        }
    }

    fn config() -> ReleaseConfig {
        ReleaseConfig {
            adapters: vec![Ecosystem::Npm, Ecosystem::Cargo],
            tag_format: "v{version}".into(),
            skip_publish: vec!["internal".into()],
            publish: PublishConfig::default(),
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

    /// The whole point of the screen: every setting shows what it is currently set to, without
    /// opening it. The old menu showed only names, so the value was one prompt away at all times.
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
        // A blank line separates each section from the one above it.
        assert_eq!(
            trimmed[7],
            "│                                                              │"
        );
        assert!(trimmed[8].contains("CHANGELOG"));
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
