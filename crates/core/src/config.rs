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

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The committed config file name, at the workspace root.
pub const CONFIG_FILE: &str = "release.toml";

/// An enabled ecosystem. Serialized by its registry name (`npm`, `crates.io`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ecosystem {
    #[serde(rename = "npm")]
    Npm,
    #[serde(rename = "crates.io")]
    Cargo,
}

impl Ecosystem {
    /// All ecosystems offered by `init`, in menu order.
    pub const ALL: [Ecosystem; 2] = [Ecosystem::Npm, Ecosystem::Cargo];

    /// The human/registry label shown in prompts and written to the file.
    pub fn label(self) -> &'static str {
        match self {
            Ecosystem::Npm => "npm",
            Ecosystem::Cargo => "crates.io",
        }
    }
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

/// A package that needs a build step before it is published or released.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageEntry {
    /// The package name (as the adapter discovers it).
    pub name: String,
    /// Which enabled ecosystem this package belongs to.
    pub adapter: Ecosystem,
    /// Publish to a registry, or build-only (artifacts -> GitHub Release).
    pub mode: Mode,
    /// Build across a target matrix (multiple platforms).
    #[serde(default)]
    pub matrix: bool,
    /// Cross-compile target triples (only when `matrix`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    /// The build command run in CI.
    pub command: String,
    /// A glob of artifacts to stage for publish / attach to the release.
    pub artifacts: String,
}

/// The whole `release.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReleaseConfig {
    /// Ecosystems enabled for this repo.
    pub adapters: Vec<Ecosystem>,
    /// Packages with an explicit build step. Packages absent here are published as-is by their
    /// adapter (no build), in `publish` mode.
    #[serde(default, rename = "package")]
    pub packages: Vec<PackageEntry>,
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
            adapters: vec![Ecosystem::Npm, Ecosystem::Cargo],
            packages: vec![
                PackageEntry {
                    name: "web-compiler".into(),
                    adapter: Ecosystem::Cargo,
                    mode: Mode::BuildOnly,
                    matrix: true,
                    targets: vec!["x86_64-unknown-linux-gnu".into()],
                    command: "cargo build --release -p otfw_cli".into(),
                    artifacts: "target/*/release/otfwc*".into(),
                },
                PackageEntry {
                    name: "docs-site".into(),
                    adapter: Ecosystem::Npm,
                    mode: Mode::Publish,
                    matrix: false,
                    targets: vec![],
                    command: "npm run build".into(),
                    artifacts: "dist/**".into(),
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

        let back: ReleaseConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.adapters, cfg.adapters);
        assert_eq!(back.packages.len(), 2);
        assert_eq!(back.build_only_names(), vec!["web-compiler".to_string()]);
    }

    #[test]
    fn save_and_load_via_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = ReleaseConfig {
            adapters: vec![Ecosystem::Cargo],
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
