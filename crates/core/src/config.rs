//! `release.toml` — the persisted, committed source of truth.
//!
//! `init` writes it (which ecosystems are enabled, and the per-package build steps); every
//! other command reads it instead of taking an `--adapter` flag. The file is hand-editable —
//! it is plain TOML with a stable, documented shape, not a tool-managed blob.
//!
//! ```toml
//! adapters = ["npm", "crates.io"]
//!
//! [[package]]
//! name      = "web-compiler"
//! adapter   = "crates.io"
//! mode      = "build-only"          # artifacts -> GitHub Release, no registry push
//! matrix    = true
//! targets   = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]
//! command   = "cargo build --release -p otfw_cli"
//! artifacts = "target/*/release/otfwc*"
//!
//! [[package]]
//! name      = "docs-site"
//! adapter   = "npm"
//! mode      = "publish"             # build, then publish to the registry
//! command   = "npm run build"
//! artifacts = "dist/**"
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// The committed config file name, at the workspace root.
pub const CONFIG_FILE: &str = "release.toml";

/// An enabled ecosystem. Serialized by its registry name (`npm`, `crates.io`) or `generic`.
///
/// `Generic` is for registries the tool doesn't natively support (e.g. Deno's JSR): it versions a
/// project via a named manifest field and publishes through a user-supplied command. See
/// [`otf_release_adapters::generic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ecosystem {
    #[serde(rename = "npm")]
    Npm,
    #[serde(rename = "crates.io")]
    Cargo,
    #[serde(rename = "generic")]
    Generic,
}

impl Ecosystem {
    /// All ecosystems offered by `init`, in menu order.
    pub const ALL: [Ecosystem; 3] = [Ecosystem::Npm, Ecosystem::Cargo, Ecosystem::Generic];

    /// The human/registry label shown in prompts and written to the file.
    pub fn label(self) -> &'static str {
        match self {
            Ecosystem::Npm => "npm",
            Ecosystem::Cargo => "crates.io",
            Ecosystem::Generic => "generic (any registry, via your own commands)",
        }
    }
}

/// The default version field/key for a generic manifest.
pub const DEFAULT_VERSION_FIELD: &str = "version";

/// The default git tag format for releases.
pub const DEFAULT_TAG_FORMAT: &str = "v{version}";

/// How generated GitHub Releases should get their body text.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GithubReleaseNotes {
    /// Let GitHub generate the body from merged PRs and commits.
    #[default]
    AutoGenerate,
    /// Copy the dated section for the released version from `CHANGELOG.md`.
    CuratedChangelog,
    /// Build a commit-subject list from the previous matching configured tag.
    SemanticCommits,
}

/// What `publish`/CI does with a package after its build step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    /// Build, then publish to the ecosystem's registry (`otf-release publish`).
    #[serde(rename = "publish")]
    Publish,
    /// Build only — stage the artifacts and attach them to a GitHub Release. No registry push.
    #[serde(rename = "build-only")]
    BuildOnly,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Publish => "publish",
            Mode::BuildOnly => "build-only",
        }
    }
}

/// A build target defining generic properties of the OS and architecture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// Generic OS name (e.g. "linux", "macos", "windows")
    pub name: String,
    /// Generic architecture (e.g. "x86_64", "aarch64", "x86")
    pub arch: String,
}

/// A package that needs a build step before it is published or released.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageEntry {
    /// The package name (as the adapter discovers it, or the generic project name).
    pub name: String,
    /// Which enabled ecosystem this package belongs to.
    pub adapter: Ecosystem,
    /// Publish to a registry, or build-only (artifacts -> GitHub Release).
    pub mode: Mode,
    /// Build across a target matrix (multiple platforms).
    #[serde(default)]
    pub matrix: bool,
    /// Cross-compile targets (only when `matrix`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<Target>,
    /// The build command run in CI (may be empty for a publish-only generic package).
    #[serde(default)]
    pub command: String,
    /// A glob of artifacts to stage for publish / attach to the release (may be empty).
    #[serde(default)]
    pub artifacts: String,

    // --- generic-adapter fields (ignored by npm/cargo) ---
    /// `generic` only: the manifest file holding the version (e.g. `deno.json`). Required for a
    /// generic package — it is the source of the version, and thus the git tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
    /// `generic` only: the version field/key inside `manifest` (defaults to `version`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_field: Option<String>,
    /// `generic` only: the shell command that publishes to the (unsupported) registry, e.g.
    /// `npx jsr publish`. Omitted ⇒ the package is build-only (artifacts -> GitHub Release).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish: Option<String>,
}

/// Global lifecycle hook scripts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Hooks {
    /// Commands to run before computing the release (e.g. `npm run lint`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_version: Vec<String>,
    /// Commands to run after versions/manifests are updated but before committing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_version: Vec<String>,
    /// Commands to run before publishing starts in CI.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_publish: Vec<String>,
    /// Commands to run after a successful publish.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_publish: Vec<String>,
}

/// The whole `release.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseConfig {
    /// Ecosystems enabled for this repo.
    pub adapters: Vec<Ecosystem>,
    /// Global lifecycle hooks.
    #[serde(default)]
    pub hooks: Hooks,
    /// Packages with an explicit build step. Packages absent here are published as-is by their
    /// adapter (no build), in `publish` mode.
    #[serde(default, rename = "package")]
    pub packages: Vec<PackageEntry>,
    /// Tag used for automated snapshot releases (e.g. "snapshot", "canary").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_tag: Option<String>,
    /// Git tag format for releases. Supports `{version}` and optional `{name}` placeholders.
    #[serde(default = "default_tag_format")]
    pub tag_format: String,
    /// Git hosting provider (e.g. "github", "gitlab").
    #[serde(default = "default_provider")]
    pub provider: String,
    /// How the changelog is managed.
    #[serde(default)]
    pub changelog_strategy: ChangelogStrategy,
    /// Where curated changelog notes are maintained.
    #[serde(default)]
    pub changelog_scope: ChangelogScope,
    /// How GitHub Release bodies are generated in CI.
    #[serde(default)]
    pub github_release_notes: GithubReleaseNotes,
}

/// The strategy for managing changelogs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangelogStrategy {
    /// Read [Unreleased] sections from hand-written CHANGELOG.md files.
    #[default]
    Curated,
    /// Automatically generate from Git commits since the last tag.
    Generated,
}

/// Where release notes live in a repository.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangelogScope {
    /// A single root CHANGELOG.md is shared by every package.
    Root,
    /// Each package uses the changelog path discovered by its adapter.
    #[default]
    Package,
}

fn default_provider() -> String {
    "github".to_string()
}

fn default_tag_format() -> String {
    DEFAULT_TAG_FORMAT.to_string()
}

impl Default for ReleaseConfig {
    fn default() -> Self {
        Self {
            adapters: Vec::new(),
            hooks: Hooks::default(),
            packages: Vec::new(),
            snapshot_tag: None,
            tag_format: default_tag_format(),
            provider: default_provider(),
            changelog_strategy: ChangelogStrategy::default(),
            changelog_scope: ChangelogScope::default(),
            github_release_notes: GithubReleaseNotes::default(),
        }
    }
}

pub fn format_tag(format: &str, name: &str, version: &str) -> Result<String> {
    if !format.contains("{version}") {
        bail!("tag_format must contain `{{version}}`");
    }
    Ok(format.replace("{name}", name).replace("{version}", version))
}

impl ReleaseConfig {
    /// The path to `release.toml` under `root`.
    pub fn path(root: &Path) -> PathBuf {
        root.join(CONFIG_FILE)
    }

    /// Whether a `release.toml` exists under `root`.
    pub fn exists(root: &Path) -> bool {
        Self::path(root).exists()
    }

    /// Load and parse `release.toml`. The error names the file when it is missing.
    pub fn load(root: &Path) -> Result<Self> {
        let path = Self::path(root);
        let text = fs::read_to_string(&path).with_context(|| {
            format!(
                "reading {} — run `otf-release init` to create it",
                path.display()
            )
        })?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Serialize to `release.toml` under `root`.
    pub fn save(&self, root: &Path) -> Result<()> {
        let path = Self::path(root);
        let text = toml::to_string_pretty(self)
            .with_context(|| format!("serializing {}", path.display()))?;
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Names of all `build-only` packages — the set `publish` must skip (they ship via the
    /// GitHub Release the workflow creates, not through a registry).
    pub fn build_only_names(&self) -> Vec<String> {
        self.packages
            .iter()
            .filter(|p| p.mode == Mode::BuildOnly)
            .map(|p| p.name.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let cfg = ReleaseConfig {
            snapshot_tag: None,
            tag_format: DEFAULT_TAG_FORMAT.to_string(),
            provider: "github".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            adapters: vec![Ecosystem::Npm, Ecosystem::Cargo],
            hooks: Hooks::default(),
            packages: vec![
                PackageEntry {
                    name: "web-compiler".into(),
                    adapter: Ecosystem::Cargo,
                    mode: Mode::BuildOnly,
                    matrix: true,
                    targets: vec![Target {
                        name: "linux".into(),
                        arch: "x86_64".into(),
                    }],
                    command: "cargo build --release -p otfw_cli".into(),
                    artifacts: "target/*/release/otfwc*".into(),
                    manifest: None,
                    version_field: None,
                    publish: None,
                },
                PackageEntry {
                    name: "docs-site".into(),
                    adapter: Ecosystem::Npm,
                    mode: Mode::Publish,
                    matrix: false,
                    targets: vec![],
                    command: "npm run build".into(),
                    artifacts: "dist/**".into(),
                    manifest: None,
                    version_field: None,
                    publish: None,
                },
            ],
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        // Registry names, not Rust identifiers.
        assert!(text.contains("\"npm\""));
        assert!(text.contains("\"crates.io\""));
        assert!(text.contains("adapter = \"crates.io\""));
        assert!(text.contains("mode = \"build-only\""));
        assert!(text.contains("mode = \"publish\""));
        assert!(text.contains("github_release_notes = \"auto-generate\""));

        let back: ReleaseConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.adapters, cfg.adapters);
        assert_eq!(back.github_release_notes, GithubReleaseNotes::AutoGenerate);
        assert_eq!(back.changelog_scope, ChangelogScope::Package);
        assert_eq!(back.packages.len(), 2);
        assert_eq!(back.build_only_names(), vec!["web-compiler".to_string()]);
    }

    #[test]
    fn save_and_load_via_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = ReleaseConfig {
            snapshot_tag: None,
            tag_format: DEFAULT_TAG_FORMAT.to_string(),
            provider: "github".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            adapters: vec![Ecosystem::Cargo],
            hooks: Hooks::default(),
            packages: vec![],
        };
        cfg.save(tmp.path()).unwrap();
        assert!(ReleaseConfig::exists(tmp.path()));
        let back = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(back.adapters, vec![Ecosystem::Cargo]);
    }

    #[test]
    fn load_missing_is_a_helpful_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = ReleaseConfig::load(tmp.path()).unwrap_err().to_string();
        assert!(err.contains("otf-release init"));
    }
}
