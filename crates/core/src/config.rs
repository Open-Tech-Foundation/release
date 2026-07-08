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

use std::collections::HashMap;
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

/// The default branch a release is cut from and returned to.
pub const DEFAULT_BRANCH: &str = "main";

/// Common git tag formats shown by interactive prompts before falling back to custom input.
pub const COMMON_TAG_FORMATS: &[&str] = &[
    "v{version}",
    "{version}",
    "{name}@{version}",
    "{name}@v{version}",
];

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

/// A build target, reconciling the three naming systems that describe one physical binary. The
/// same artifact is known by a **Rust target triple** (to cargo), a **CI runner OS** (to GitHub
/// Actions), and a **`process.platform-process.arch` directory** (to the Node `extract.js`
/// resolver). The tool is the only place that sees all three, so a `Target` carries all three —
/// keeping them in sync is what prevents a "published, but no install can find the binary" bug.
///
/// `name`/`arch` are always present; the rest default to empty/false and fall back to the built-in
/// [`TARGET_REGISTRY`] via the accessor methods, so a hand-written `release.toml` can list just
/// `name`/`arch` while `init` writes every field explicitly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// Generic OS name (e.g. "linux", "macos", "windows").
    pub name: String,
    /// Generic architecture (e.g. "x86_64", "aarch64", "x86").
    pub arch: String,
    /// Rust target triple, e.g. `aarch64-unknown-linux-gnu`. Empty ⇒ look up by (name, arch).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub triple: String,
    /// GitHub-hosted runner that builds this target, e.g. `ubuntu-latest`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub runner: String,
    /// The staged directory inside the package. **MUST** equal Node's
    /// `process.platform`-`process.arch` (e.g. `linux-arm64`, `darwin-x64`, `win32-x64`) so the
    /// package's install-time resolver finds the binary.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stage_as: String,
    /// Executable extension for this target (`""` or `.exe`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ext: String,
    /// Whether this target needs cross-compile prep (a non-host linker) on its runner.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cross: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A built-in fact row reconciling a `(name, arch)` pair to its triple, runner, Node stage dir,
/// exe extension, and whether cross-compile prep is required.
pub struct TargetInfo {
    pub label: &'static str,
    pub name: &'static str,
    pub arch: &'static str,
    pub triple: &'static str,
    pub runner: &'static str,
    pub stage_as: &'static str,
    pub ext: &'static str,
    pub cross: bool,
    /// Whether `init` selects this target by default. Only the widely-supported platforms (the set
    /// an npm package's `extract.js` resolver typically handles) are on by default; niche targets
    /// (`win32-arm64`, 32-bit) stay in the registry for explicit opt-in.
    pub default_on: bool,
}

/// The single source of truth mapping `(name, arch)` to the three naming systems. `stage_as` is the
/// Node `process.platform`-`process.arch` directory the package resolver reads — getting it wrong
/// is the one mistake that publishes a working-looking package no install can use.
#[rustfmt::skip]
pub const TARGET_REGISTRY: &[TargetInfo] = &[
    TargetInfo { label: "Linux x64",          name: "linux",   arch: "x86_64",  triple: "x86_64-unknown-linux-gnu",  runner: "ubuntu-latest",  stage_as: "linux-x64",   ext: "",     cross: false, default_on: true },
    TargetInfo { label: "Linux ARM64",        name: "linux",   arch: "aarch64", triple: "aarch64-unknown-linux-gnu", runner: "ubuntu-latest",  stage_as: "linux-arm64", ext: "",     cross: true,  default_on: true },
    TargetInfo { label: "Linux x86 (32-bit)", name: "linux",   arch: "x86",     triple: "i686-unknown-linux-gnu",    runner: "ubuntu-latest",  stage_as: "linux-ia32",  ext: "",     cross: true,  default_on: false },
    TargetInfo { label: "macOS ARM64",        name: "macos",   arch: "aarch64", triple: "aarch64-apple-darwin",      runner: "macos-latest",   stage_as: "darwin-arm64",ext: "",     cross: false, default_on: true },
    TargetInfo { label: "macOS x64",          name: "macos",   arch: "x86_64",  triple: "x86_64-apple-darwin",       runner: "macos-latest",   stage_as: "darwin-x64",  ext: "",     cross: false, default_on: true },
    TargetInfo { label: "Windows x64",        name: "windows", arch: "x86_64",  triple: "x86_64-pc-windows-msvc",    runner: "windows-latest", stage_as: "win32-x64",   ext: ".exe", cross: false, default_on: true },
    // win32-arm64 is rarely in a package's resolver SUPPORTED set and cross-links arm64 on an x64
    // Windows runner; offered but off by default.
    TargetInfo { label: "Windows ARM64",      name: "windows", arch: "aarch64", triple: "aarch64-pc-windows-msvc",   runner: "windows-latest", stage_as: "win32-arm64", ext: ".exe", cross: false, default_on: false },
    TargetInfo { label: "Windows x86 (32-bit)", name: "windows", arch: "x86",   triple: "i686-pc-windows-msvc",      runner: "windows-latest", stage_as: "win32-ia32",  ext: ".exe", cross: false, default_on: false },
];

/// Look up the built-in facts for a `(name, arch)` pair.
pub fn lookup_target(name: &str, arch: &str) -> Option<&'static TargetInfo> {
    TARGET_REGISTRY
        .iter()
        .find(|t| t.name == name && t.arch == arch)
}

impl Target {
    fn info(&self) -> Option<&'static TargetInfo> {
        lookup_target(&self.name, &self.arch)
    }

    /// The Rust target triple — the explicit field if set, else the registry value.
    pub fn triple(&self) -> String {
        non_empty(&self.triple).unwrap_or_else(|| {
            self.info()
                .map(|i| i.triple.to_string())
                .unwrap_or_default()
        })
    }

    /// The GitHub runner OS — the explicit field if set, else the registry value.
    pub fn runner(&self) -> String {
        non_empty(&self.runner).unwrap_or_else(|| {
            self.info()
                .map(|i| i.runner.to_string())
                .unwrap_or_default()
        })
    }

    /// The Node `process.platform-process.arch` stage dir — explicit field if set, else registry.
    pub fn stage_as(&self) -> String {
        non_empty(&self.stage_as).unwrap_or_else(|| {
            self.info()
                .map(|i| i.stage_as.to_string())
                .unwrap_or_default()
        })
    }

    /// The executable extension — explicit field if set, else the registry value.
    pub fn ext(&self) -> String {
        non_empty(&self.ext)
            .unwrap_or_else(|| self.info().map(|i| i.ext.to_string()).unwrap_or_default())
    }

    /// Whether cross-compile prep is needed — true if explicitly set, else the registry value.
    pub fn is_cross(&self) -> bool {
        self.cross || self.info().map(|i| i.cross).unwrap_or(false)
    }

    /// Expand the per-target placeholders in a command/artifacts template: `{triple}`, `{ext}`,
    /// `{stage_as}`, `{bin}`, `{arch}`, `{name}` (the OS name). `bin` is the package's binary name.
    pub fn render(&self, template: &str, bin: &str) -> String {
        template
            .replace("{triple}", &self.triple())
            .replace("{stage_as}", &self.stage_as())
            .replace("{ext}", &self.ext())
            .replace("{arch}", &self.arch)
            .replace("{name}", &self.name)
            .replace("{bin}", bin)
    }

    /// Build a fully-populated `Target` for a `(name, arch)` pair from the registry, so `init`
    /// writes every reconciling field into `release.toml` rather than leaving them implicit.
    pub fn resolved(name: &str, arch: &str) -> Self {
        match lookup_target(name, arch) {
            Some(i) => Self {
                name: i.name.to_string(),
                arch: i.arch.to_string(),
                triple: i.triple.to_string(),
                runner: i.runner.to_string(),
                stage_as: i.stage_as.to_string(),
                ext: i.ext.to_string(),
                cross: i.cross,
            },
            None => Self {
                name: name.to_string(),
                arch: arch.to_string(),
                ..Self::default()
            },
        }
    }
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
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
    /// A glob of artifacts to stage for publish / attach to the release (may be empty). For matrix
    /// builds it is templated per target with `{triple}`, `{ext}`, `{stage_as}`, `{bin}`.
    #[serde(default)]
    pub artifacts: String,
    /// The compiled binary's base name (no extension), used to template `{bin}` and to name the
    /// staged file `bin/{stage_as}/{bin}{ext}`. Matrix builds only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin_name: Option<String>,
    /// Compression applied to each staged binary, e.g. `brotli` (writes `…{ext}.br`). The package's
    /// install-time resolver decompresses it. Matrix builds only; `None` stages the raw binary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compress: Option<String>,

    // --- manifest fields (generic uses these to version; npm may use `manifest` for workflow reads) ---
    /// Manifest file holding the version. Required for a generic package; for npm packages `init`
    /// may persist the discovered `package.json` path so generated workflows can read it directly.
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

impl PackageEntry {
    /// Whether this package ships its artifacts via a GitHub Release instead of a registry.
    ///
    /// `build-only` means "standalone binaries attached to a GitHub Release" — correct for a cargo
    /// or generic CLI. It is **meaningless for an npm matrix package**, whose per-platform binaries
    /// ship *inside the npm tarball* under `bin/<stage_as>/`, not as Release assets. So an
    /// npm + matrix package is always treated as `publish` regardless of its stored mode, which is
    /// what keeps its binaries flowing to `npm publish` instead of a cosmetic GitHub Release.
    pub fn is_build_only(&self) -> bool {
        self.mode == Mode::BuildOnly && !(self.adapter == Ecosystem::Npm && self.matrix)
    }

    /// The inverse of [`is_build_only`]: the package is published to its registry.
    pub fn is_publish(&self) -> bool {
        !self.is_build_only()
    }
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

/// Publish policy knobs that affect release gating.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PublishConfig {
    /// Per-package path globs that publish flow checks should ignore when deciding whether path-scoped
    /// commits without changelog notes deserve only a warning.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub ignore_paths: HashMap<String, Vec<String>>,
}

/// The whole `release.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseConfig {
    /// Ecosystems enabled for this repo.
    pub adapters: Vec<Ecosystem>,
    /// Publishable packages that this tool must not version or publish.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_publish: Vec<String>,
    /// Global lifecycle hooks.
    #[serde(default)]
    pub hooks: Hooks,
    /// Publish path-ignore policy keyed by package name.
    #[serde(default)]
    pub publish: PublishConfig,
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
    /// Older tag formats to read as release history while writing new tags with `tag_format`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub legacy_tag_formats: Vec<String>,
    /// Git hosting provider (e.g. "github", "gitlab").
    #[serde(default = "default_provider")]
    pub provider: String,
    /// The branch a release is started from and returned to (e.g. `main`, `master`, `trunk`).
    #[serde(default = "default_branch")]
    pub default_branch: String,
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

fn default_branch() -> String {
    DEFAULT_BRANCH.to_string()
}

fn default_tag_format() -> String {
    DEFAULT_TAG_FORMAT.to_string()
}

impl Default for ReleaseConfig {
    fn default() -> Self {
        Self {
            adapters: Vec::new(),
            skip_publish: Vec::new(),
            hooks: Hooks::default(),
            publish: PublishConfig::default(),
            packages: Vec::new(),
            snapshot_tag: None,
            tag_format: default_tag_format(),
            legacy_tag_formats: Vec::new(),
            provider: default_provider(),
            default_branch: default_branch(),
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
    /// Tag formats used to find prior releases. New tags are still written only with `tag_format`.
    pub fn history_tag_formats(&self) -> Vec<String> {
        std::iter::once(self.tag_format.clone())
            .chain(self.legacy_tag_formats.iter().cloned())
            .collect()
    }

    /// The configured publish ignore globs for this package name.
    pub fn publish_ignore_paths_for(&self, pkg_name: &str) -> &[String] {
        self.publish
            .ignore_paths
            .get(pkg_name)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Mark configured packages as non-publishable so version/preflight/publish treat them like
    /// private apps without requiring package manifests to set `private: true`.
    pub fn apply_publish_skips(&self, packages: &mut [crate::adapter::Pkg]) {
        for pkg in packages {
            if self.skip_publish.iter().any(|name| name == &pkg.name) {
                pkg.publishable = false;
            }
        }
    }

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
        let mut names: Vec<String> = self
            .packages
            .iter()
            .filter(|p| p.is_build_only())
            .map(|p| p.name.clone())
            .collect();
        names.extend(self.skip_publish.iter().cloned());
        names
    }

    /// Names of `matrix` publish-mode packages — those that must have their per-platform binaries
    /// staged before `publish` is allowed to push them (see `PublishOptions::require_staged`).
    pub fn matrix_publish_names(&self) -> Vec<String> {
        self.packages
            .iter()
            .filter(|p| p.matrix && p.is_publish())
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
            legacy_tag_formats: Vec::new(),
            skip_publish: vec!["private-tool".to_string()],
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            adapters: vec![Ecosystem::Npm, Ecosystem::Cargo],
            hooks: Hooks::default(),
            publish: PublishConfig {
                ignore_paths: HashMap::from([(
                    "docs-site".into(),
                    vec!["docs/**".into(), "**/*.test.ts".into()],
                )]),
            },
            packages: vec![
                PackageEntry {
                    name: "web-compiler".into(),
                    adapter: Ecosystem::Cargo,
                    mode: Mode::BuildOnly,
                    matrix: true,
                    targets: vec![Target::resolved("linux", "x86_64")],
                    command: "cargo build --release -p otfw_cli".into(),
                    artifacts: "target/*/release/otfwc*".into(),
                    bin_name: Some("otfwc".into()),
                    compress: None,
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
                    bin_name: None,
                    compress: None,
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
        assert!(text.contains("skip_publish = [\"private-tool\"]"));
        assert!(text.contains("[publish.ignore_paths]"));

        let back: ReleaseConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.adapters, cfg.adapters);
        assert_eq!(back.github_release_notes, GithubReleaseNotes::AutoGenerate);
        assert_eq!(back.skip_publish, vec!["private-tool"]);
        assert_eq!(back.changelog_scope, ChangelogScope::Package);
        assert_eq!(
            back.publish_ignore_paths_for("docs-site"),
            ["docs/**", "**/*.test.ts"]
        );
        assert_eq!(back.packages.len(), 2);
        assert_eq!(
            back.build_only_names(),
            vec!["web-compiler".to_string(), "private-tool".to_string()]
        );
    }

    #[test]
    fn save_and_load_via_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = ReleaseConfig {
            snapshot_tag: None,
            tag_format: DEFAULT_TAG_FORMAT.to_string(),
            legacy_tag_formats: Vec::new(),
            skip_publish: Vec::new(),
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            adapters: vec![Ecosystem::Cargo],
            hooks: Hooks::default(),
            publish: PublishConfig::default(),
            packages: vec![],
        };
        cfg.save(tmp.path()).unwrap();
        assert!(ReleaseConfig::exists(tmp.path()));
        let back = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(back.adapters, vec![Ecosystem::Cargo]);
    }

    #[test]
    fn skip_publish_marks_packages_non_publishable_and_publish_skips_them() {
        let cfg = ReleaseConfig {
            skip_publish: vec!["@scope/manual".to_string()],
            ..ReleaseConfig::default()
        };
        let mut packages = vec![
            crate::adapter::Pkg {
                name: "@scope/manual".to_string(),
                version: "1.0.0".to_string(),
                manifest_path: "packages/manual/package.json".into(),
                changelog_path: "packages/manual/CHANGELOG.md".into(),
                publishable: true,
                internal_deps: vec![],
            },
            crate::adapter::Pkg {
                name: "@scope/managed".to_string(),
                version: "1.0.0".to_string(),
                manifest_path: "packages/managed/package.json".into(),
                changelog_path: "packages/managed/CHANGELOG.md".into(),
                publishable: true,
                internal_deps: vec![],
            },
        ];

        cfg.apply_publish_skips(&mut packages);

        assert!(!packages[0].publishable);
        assert!(packages[1].publishable);
        assert_eq!(cfg.build_only_names(), vec!["@scope/manual"]);
    }

    #[test]
    fn publish_ignore_paths_default_to_empty() {
        let cfg = ReleaseConfig::default();
        assert!(cfg.publish_ignore_paths_for("missing").is_empty());
    }

    #[test]
    fn default_branch_defaults_to_main_and_round_trips_a_custom_value() {
        // Absent from the file → defaults to main.
        let cfg: ReleaseConfig = toml::from_str("adapters = [\"npm\"]\n").unwrap();
        assert_eq!(cfg.default_branch, "main");

        // Explicit value survives a save/load round-trip.
        let custom: ReleaseConfig =
            toml::from_str("adapters = [\"npm\"]\ndefault_branch = \"trunk\"\n").unwrap();
        assert_eq!(custom.default_branch, "trunk");
        let text = toml::to_string_pretty(&custom).unwrap();
        assert!(text.contains("default_branch = \"trunk\""));
    }

    #[test]
    fn load_missing_is_a_helpful_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = ReleaseConfig::load(tmp.path()).unwrap_err().to_string();
        assert!(err.contains("otf-release init"));
    }
}
