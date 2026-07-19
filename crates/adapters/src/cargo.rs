//! The cargo adapter — the Rust / crates.io ecosystem.
//!
//! Mirrors the npm adapter over `Cargo.toml`, using `toml_edit` for format-preserving edits.
//! Cargo differs from npm in ways the roadmap calls out (see `docs/roadmap.md`):
//!
//! - **No peerDep concept** → every internal dependent takes a `Patch` (`dependent_bump`).
//! - **`version.workspace = true`** (inherited versions) is handled as **lockstep** versioning:
//!   a crate that inherits its version is bumped by writing the shared `[workspace.package]`
//!   version in the root manifest, so every inheriting crate moves together. Such crates also
//!   share a single root `CHANGELOG.md`. Crates with a concrete `[package] version` are still
//!   versioned independently.
//! - **`[workspace.dependencies]` internal pins** — an internal crate pinned there (with a `path`
//!   and a `version`, referenced by members via `{ workspace = true }`) has its pin bumped in
//!   lockstep with the workspace version, and a member's inherited `{ workspace = true }` entry is
//!   never given a conflicting `version` key.
//! - **`cargo publish` needs a concrete `version` on path dependencies** → `resolve_workspace_links`
//!   injects them before publishing.
//! - **crates.io is source-only** → `publish` ignores any staged binary `staged_assets`
//!   (binaries are distributed out-of-band, e.g. via GitHub Releases).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use glob::glob;
use toml_edit::{value, DocumentMut, Item};

use otf_release_core::adapter::{Adapter, Bump, DepKind, InternalDep, Pkg};

pub use crate::command::{CommandOutput, CommandRunner, SystemRunner};

/// The dependency sections cargo recognizes, in a stable order.
const DEP_SECTIONS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

/// A declared dependency with the section it came from.
struct CargoDep {
    section: &'static str,
    name: String,
    version_req: Option<String>,
}

/// A `Cargo.toml` held as a `toml_edit` document so edits preserve formatting.
struct CargoManifest {
    path: PathBuf,
    doc: DocumentMut,
}

impl CargoManifest {
    fn read(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading manifest {}", path.display()))?;
        Self::from_str(path.to_path_buf(), &content)
    }

    fn from_str(path: PathBuf, content: &str) -> Result<Self> {
        let doc = content
            .parse::<DocumentMut>()
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(Self { path, doc })
    }

    fn save(&self) -> Result<()> {
        std::fs::write(&self.path, self.doc.to_string())
            .with_context(|| format!("writing {}", self.path.display()))
    }

    fn package_name(&self) -> Option<String> {
        self.doc
            .get("package")?
            .get("name")?
            .as_str()
            .map(str::to_string)
    }

    /// The `[package] version` if it is a concrete string (not `{ workspace = true }`).
    fn concrete_version(&self) -> Option<String> {
        self.doc
            .get("package")?
            .get("version")?
            .as_str()
            .map(str::to_string)
    }

    fn version_is_inherited(&self) -> bool {
        self.doc
            .get("package")
            .and_then(|p| p.get("version"))
            .and_then(|v| v.as_table_like())
            .and_then(|t| t.get("workspace"))
            .and_then(|w| w.as_bool())
            == Some(true)
    }

    /// `publish = false` (or `publish = []`) marks a crate as not publishable.
    fn is_publishable(&self) -> bool {
        match self.doc.get("package").and_then(|p| p.get("publish")) {
            Some(item) if item.as_bool() == Some(false) => false,
            Some(item) if item.as_array().is_some_and(|a| a.is_empty()) => false,
            _ => true,
        }
    }

    fn workspace_members(&self) -> Vec<String> {
        self.doc
            .get("workspace")
            .and_then(|w| w.get("members"))
            .and_then(Item::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn workspace_version(&self) -> Option<String> {
        self.doc
            .get("workspace")?
            .get("package")?
            .get("version")?
            .as_str()
            .map(str::to_string)
    }

    fn deps(&self) -> Vec<CargoDep> {
        let mut out = Vec::new();
        for section in DEP_SECTIONS {
            if let Some(table) = self.doc.get(section).and_then(Item::as_table_like) {
                for (name, item) in table.iter() {
                    let version_req = if let Some(s) = item.as_str() {
                        Some(s.to_string())
                    } else {
                        item.as_table_like()
                            .and_then(|t| t.get("version"))
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    };
                    out.push(CargoDep {
                        section,
                        name: name.to_string(),
                        version_req,
                    });
                }
            }
        }
        out
    }

    fn set_package_version(&mut self, new: &str) -> Result<()> {
        let pkg = self
            .doc
            .get_mut("package")
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| anyhow!("{}: no [package] table", self.path.display()))?;
        pkg.insert("version", value(new));
        Ok(())
    }

    /// Write the shared `[workspace.package] version` (lockstep bump for inheriting crates).
    fn set_workspace_version(&mut self, new: &str) -> Result<()> {
        let ws = self
            .doc
            .get_mut("workspace")
            .and_then(|w| w.get_mut("package"))
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| {
                anyhow!(
                    "{}: no [workspace.package] table to write a lockstep version",
                    self.path.display()
                )
            })?;
        ws.insert("version", value(new));
        Ok(())
    }

    /// Set the version requirement for `name` in `section`. Works for both string-form
    /// (`dep = "1.2"`) and table-form (`dep = { path = "..", version = ".." }`) entries. A
    /// workspace-inherited entry (`dep = { workspace = true }`) is left untouched: its version lives
    /// in the root `[workspace.dependencies]` pin (bumped separately), and adding a `version` key
    /// beside `workspace = true` is invalid.
    fn set_dep_version(&mut self, section: &str, name: &str, new: &str) -> bool {
        let Some(item) = self.doc.get_mut(section).and_then(|s| s.get_mut(name)) else {
            return false;
        };
        set_item_version(item, new)
    }

    /// The `[workspace.dependencies]` table, if present.
    fn workspace_deps_mut(&mut self) -> Option<&mut dyn toml_edit::TableLike> {
        self.doc
            .get_mut("workspace")
            .and_then(|w| w.get_mut("dependencies"))
            .and_then(Item::as_table_like_mut)
    }

    /// Set the version pin for one internal crate `name` in `[workspace.dependencies]`. Members that
    /// reference it via `{ workspace = true }` inherit this pin, so bumping it here is what keeps a
    /// dependent buildable/publishable after the dependency bumps.
    fn set_workspace_dep_version(&mut self, name: &str, new: &str) -> bool {
        let Some(table) = self.workspace_deps_mut() else {
            return false;
        };
        let Some(item) = table.get_mut(name) else {
            return false;
        };
        set_item_version(item, new)
    }

    /// Bump every internal (path) dependency pinned in `[workspace.dependencies]` to `new` — the
    /// lockstep counterpart to `set_workspace_version`. Only path-carrying entries are touched, so
    /// external pins (`serde = { version = "1" }`) are left alone. Returns whether anything changed.
    fn bump_internal_workspace_dep_pins(&mut self, new: &str) -> bool {
        let Some(table) = self.workspace_deps_mut() else {
            return false;
        };
        let mut changed = false;
        for (_name, item) in table.iter_mut() {
            if let Some(dep) = item.as_table_like_mut() {
                if dep.contains_key("path") && dep.contains_key("version") {
                    dep.insert("version", value(new));
                    changed = true;
                }
            }
        }
        changed
    }
}

/// Set a dependency item's version, string- or table-form, skipping a `{ workspace = true }` entry
/// (its version is inherited from `[workspace.dependencies]`). Returns whether it changed.
fn set_item_version(item: &mut Item, new: &str) -> bool {
    if item.is_str() {
        *item = value(new);
        true
    } else if let Some(table) = item.as_table_like_mut() {
        if table.get("workspace").and_then(|w| w.as_bool()) == Some(true) {
            return false;
        }
        table.insert("version", value(new));
        true
    } else {
        false
    }
}

/// cargo-backed adapter, rooted at the workspace directory.
pub struct CargoAdapter {
    pub root: PathBuf,
    runner: Box<dyn CommandRunner>,
}

impl CargoAdapter {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            runner: Box::new(SystemRunner),
        }
    }

    pub fn with_runner(root: impl Into<PathBuf>, runner: Box<dyn CommandRunner>) -> Self {
        Self {
            root: root.into(),
            runner,
        }
    }

    fn member_dirs(&self) -> Result<Vec<PathBuf>> {
        let root = CargoManifest::read(&self.root.join("Cargo.toml"))?;
        let mut dirs = BTreeSet::new();
        for pattern in root.workspace_members() {
            let joined = self.root.join(&pattern).join("Cargo.toml");
            let glob_str = joined
                .to_str()
                .ok_or_else(|| anyhow!("non-UTF-8 path in workspace member: {pattern}"))?;
            for entry in
                glob(glob_str).with_context(|| format!("invalid member glob: {pattern}"))?
            {
                if let Some(dir) = entry?.parent() {
                    dirs.insert(dir.to_path_buf());
                }
            }
        }
        Ok(dirs.into_iter().collect())
    }
}

impl Adapter for CargoAdapter {
    fn discover_packages(&self) -> Result<Vec<Pkg>> {
        let root = CargoManifest::read(&self.root.join("Cargo.toml"))?;
        let workspace_version = root.workspace_version();
        // A workspace that declares a shared `[workspace.package] version` versions its
        // inheriting crates in lockstep against a single root CHANGELOG.md.
        let lockstep = workspace_version.is_some();

        let mut members: Vec<(PathBuf, CargoManifest)> = Vec::new();
        for dir in self.member_dirs()? {
            let manifest = CargoManifest::read(&dir.join("Cargo.toml"))?;
            members.push((dir, manifest));
        }

        let internal_names: HashSet<String> = members
            .iter()
            .filter_map(|(_, m)| m.package_name())
            .collect();

        let mut packages = Vec::with_capacity(members.len());
        for (dir, manifest) in &members {
            let name = manifest
                .package_name()
                .ok_or_else(|| anyhow!("{}: no [package] name", dir.display()))?;
            let version = manifest
                .concrete_version()
                .or_else(|| workspace_version.clone())
                .ok_or_else(|| anyhow!("{name}: cannot determine version"))?;
            let internal_deps = manifest
                .deps()
                .into_iter()
                .filter(|d| internal_names.contains(&d.name))
                .map(|d| InternalDep {
                    name: d.name,
                    kind: kind_of(d.section),
                    range: d.version_req.unwrap_or_default(),
                })
                .collect();
            // Inheriting crates in a lockstep workspace share the root CHANGELOG.md; crates with
            // their own concrete version keep a per-crate changelog.
            let changelog_path = if lockstep && manifest.version_is_inherited() {
                self.root.join("CHANGELOG.md")
            } else {
                dir.join("CHANGELOG.md")
            };
            packages.push(Pkg {
                name,
                version,
                manifest_path: dir.join("Cargo.toml"),
                changelog_path,
                publishable: manifest.is_publishable(),
                internal_deps,
            });
        }

        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(packages)
    }

    fn write_version(&self, pkg: &Pkg, new: &str) -> Result<()> {
        let manifest = CargoManifest::read(&pkg.manifest_path)?;
        if manifest.version_is_inherited() {
            // Lockstep: the crate inherits `version.workspace = true`, so bump the shared
            // `[workspace.package] version` in the root manifest. Every inheriting crate follows.
            // Internal path deps pinned in `[workspace.dependencies]` share that version, so bump
            // their pins in lockstep too — otherwise `cargo update`/publish can't resolve a
            // dependent whose pin still points at the previous version.
            let mut root = CargoManifest::read(&self.root.join("Cargo.toml"))?;
            root.set_workspace_version(new)?;
            root.bump_internal_workspace_dep_pins(new);
            root.save()
        } else {
            let mut manifest = manifest;
            manifest.set_package_version(new)?;
            manifest.save()
        }
    }

    fn update_dep_range(&self, pkg: &Pkg, dep: &str, new_dep_version: &str) -> Result<()> {
        let mut manifest = CargoManifest::read(&pkg.manifest_path)?;
        let mut changed = false;
        for section in DEP_SECTIONS {
            if manifest.set_dep_version(section, dep, new_dep_version) {
                changed = true;
            }
        }
        if changed {
            manifest.save()?;
        }
        // The dependent may inherit `dep` from the root `[workspace.dependencies]` pin
        // (`dep = { workspace = true }`); update that shared pin too. Read the root fresh (so a
        // just-saved member edit isn't clobbered), and no-op when there is no such pin. Idempotent,
        // and it covers the independent-version case the lockstep `write_version` bump does not.
        let mut root = CargoManifest::read(&self.root.join("Cargo.toml"))?;
        if root.set_workspace_dep_version(dep, new_dep_version) {
            root.save()?;
        }
        Ok(())
    }

    fn format_range(&self, version: &str) -> String {
        // A bare version in Cargo.toml means `^version`.
        version.to_string()
    }

    fn resolve_workspace_links(&self, pkg: &Pkg) -> Result<()> {
        let versions: HashMap<String, String> = self
            .discover_packages()?
            .into_iter()
            .map(|p| (p.name, p.version))
            .collect();

        let mut manifest = CargoManifest::read(&pkg.manifest_path)?;
        let mut changed = false;
        for dep in manifest.deps() {
            if let Some(version) = versions.get(&dep.name) {
                // Ensure every internal (path) dep carries a concrete version for publish.
                if manifest.set_dep_version(dep.section, &dep.name, version) {
                    changed = true;
                }
            }
        }
        if changed {
            manifest.save()?;
        }
        Ok(())
    }

    fn update_lockfile(&self, root: &Path) -> Result<()> {
        let out = self.runner.run("cargo", &["update", "--workspace"], root)?;
        if !out.success {
            bail!("`cargo update --workspace` failed:\n{}", out.stderr);
        }
        Ok(())
    }

    fn dependent_bump(&self, _dep_bump: Bump, _kind: &DepKind) -> Bump {
        // Cargo has no peerDep concept: an internal dependent only needs to pick up the new
        // version range, which is a patch.
        Bump::Patch
    }

    fn version_groups(&self) -> Result<Vec<Vec<String>>> {
        let root = CargoManifest::read(&self.root.join("Cargo.toml"))?;
        // Without a shared `[workspace.package] version` nothing is locked together.
        if root.workspace_version().is_none() {
            return Ok(Vec::new());
        }
        // Every crate that inherits `version.workspace = true` shares the one workspace version,
        // so they form a single lockstep group.
        let mut group = Vec::new();
        for dir in self.member_dirs()? {
            let manifest = CargoManifest::read(&dir.join("Cargo.toml"))?;
            if manifest.version_is_inherited() {
                if let Some(name) = manifest.package_name() {
                    group.push(name);
                }
            }
        }
        group.sort();
        Ok(if group.is_empty() {
            Vec::new()
        } else {
            vec![group]
        })
    }

    fn is_published(&self, pkg: &Pkg, version: &str) -> Result<bool> {
        let spec = format!("{}@{}", pkg.name, version);
        let out = self.runner.run("cargo", &["info", &spec], &self.root)?;
        if out.success {
            return Ok(true);
        }
        let stderr = out.stderr.to_lowercase();
        // `cargo info` was only stabilized in cargo 1.82. On older toolchains it is an unknown
        // subcommand; surface an actionable error rather than the cryptic raw output.
        if stderr.contains("no such subcommand") || stderr.contains("unrecognized subcommand") {
            bail!(
                "`cargo info` is unavailable — it requires cargo 1.82 or newer. \
                 Upgrade your Rust toolchain to publish cargo crates.\n{}",
                out.stderr
            );
        }
        if stderr.contains("could not find") || stderr.contains("not found") {
            return Ok(false);
        }
        bail!("`cargo info {spec}` failed:\n{}", out.stderr);
    }

    fn publish(&self, pkg: &Pkg, _staged_assets: Option<&Path>) -> Result<()> {
        // crates.io is source-only; staged binaries are distributed out-of-band.
        //
        // `--allow-dirty` is deliberate: `publish` calls `resolve_workspace_links` immediately
        // before this to inject concrete `version`s onto internal path deps. In the normal flow
        // the release PR already wrote those, so the resolve is a no-op and the tree is clean; but
        // when it does edit a manifest (e.g. a path dep added without a version after the release
        // PR), the tree is dirty and a plain `cargo publish` would abort mid-run — after earlier
        // crates in the graph already shipped, with no rollback. Allowing the dirty tree lets the
        // intended resolve edits through instead of stranding a partial release.
        let out = self.runner.run(
            "cargo",
            &["publish", "-p", &pkg.name, "--allow-dirty"],
            &self.root,
        )?;
        if !out.success {
            bail!("`cargo publish -p {}` failed:\n{}", pkg.name, out.stderr);
        }
        Ok(())
    }
}

fn kind_of(section: &str) -> DepKind {
    match section {
        "dev-dependencies" => DepKind::DevDep,
        _ => DepKind::Dep, // dependencies + build-dependencies
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Arc, Mutex};

    type Calls = Arc<Mutex<Vec<(Vec<String>, PathBuf)>>>;

    #[derive(Clone)]
    struct FakeRunner {
        out: CommandOutput,
        calls: Calls,
    }
    impl FakeRunner {
        fn new(success: bool, stdout: &str, stderr: &str) -> Self {
            Self {
                out: CommandOutput {
                    success,
                    stdout: stdout.into(),
                    stderr: stderr.into(),
                },
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }
    impl CommandRunner for FakeRunner {
        fn run(&self, _program: &str, args: &[&str], cwd: &Path) -> Result<CommandOutput> {
            self.calls.lock().unwrap().push((
                args.iter().map(|s| s.to_string()).collect(),
                cwd.to_path_buf(),
            ));
            Ok(self.out.clone())
        }
    }

    fn write(path: PathBuf, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn workspace() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nversion = \"9.9.9\"\n",
        );
        write(
            root.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"1.0.0\"\n",
        );
        write(
            root.join("crates/b/Cargo.toml"),
            "[package]\nname = \"b\"\nversion = \"2.0.0\"\n\n[dependencies]\na = { path = \"../a\", version = \"1.0.0\" }\nleft-pad = \"1\"\n",
        );
        write(
            root.join("crates/c/Cargo.toml"),
            "[package]\nname = \"c\"\nversion = \"0.1.0\"\npublish = false\n\n[dependencies]\na = { path = \"../a\" }\n",
        );
        tmp
    }

    fn dummy_pkg(name: &str, manifest_path: &Path) -> Pkg {
        Pkg {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            manifest_path: manifest_path.to_path_buf(),
            changelog_path: manifest_path.with_file_name("CHANGELOG.md"),
            publishable: true,
            internal_deps: vec![],
        }
    }

    #[test]
    fn discovers_crates_and_only_internal_deps() {
        let tmp = workspace();
        let adapter = CargoAdapter::new(tmp.path());
        let pkgs = adapter.discover_packages().unwrap();

        let names: Vec<_> = pkgs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);

        let b = pkgs.iter().find(|p| p.name == "b").unwrap();
        assert!(b.publishable);
        assert_eq!(b.internal_deps.len(), 1, "left-pad must be excluded");
        assert_eq!(b.internal_deps[0].name, "a");
        assert_eq!(b.internal_deps[0].kind, DepKind::Dep);

        let c = pkgs.iter().find(|p| p.name == "c").unwrap();
        assert!(!c.publishable, "publish = false => not publishable");
    }

    #[test]
    fn write_version_and_update_dep_range_preserve_formatting() {
        let tmp = workspace();
        let b_path = tmp.path().join("crates/b/Cargo.toml");
        let adapter = CargoAdapter::new(tmp.path());
        let pkg = dummy_pkg("b", &b_path);

        adapter.write_version(&pkg, "2.1.0").unwrap();
        adapter.update_dep_range(&pkg, "a", "1.1.0").unwrap();

        let after = fs::read_to_string(&b_path).unwrap();
        assert!(after.contains("version = \"2.1.0\""));
        assert!(after.contains("a = { path = \"../a\", version = \"1.1.0\" }"));
        assert!(
            after.contains("left-pad = \"1\""),
            "siblings preserved: {after}"
        );
    }

    #[test]
    fn resolve_workspace_links_injects_version_on_path_only_dep() {
        let tmp = workspace();
        let c_path = tmp.path().join("crates/c/Cargo.toml");
        let adapter = CargoAdapter::new(tmp.path());
        let pkg = dummy_pkg("c", &c_path);

        adapter.resolve_workspace_links(&pkg).unwrap();
        let after = fs::read_to_string(&c_path).unwrap();
        assert!(after.contains("version = \"1.0.0\""), "got: {after}");
    }

    #[test]
    fn write_version_lockstep_bumps_workspace_and_shares_root_changelog() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nversion = \"1.2.3\"\n",
        );
        write(
            root.join("crates/cli/Cargo.toml"),
            "[package]\nname = \"tool\"\nversion.workspace = true\n",
        );
        let adapter = CargoAdapter::new(root);

        // Discovery resolves the inherited version and points the changelog at the root.
        let pkgs = adapter.discover_packages().unwrap();
        let tool = pkgs.iter().find(|p| p.name == "tool").unwrap();
        assert_eq!(tool.version, "1.2.3");
        assert_eq!(tool.changelog_path, root.join("CHANGELOG.md"));

        // Writing the version bumps the shared [workspace.package] version, not the crate.
        adapter.write_version(tool, "1.3.0").unwrap();
        let root_toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            root_toml.contains("version = \"1.3.0\""),
            "got: {root_toml}"
        );
        let crate_toml = fs::read_to_string(root.join("crates/cli/Cargo.toml")).unwrap();
        assert!(
            crate_toml.contains("version.workspace = true"),
            "crate manifest must stay inherited: {crate_toml}"
        );
    }

    #[test]
    fn lockstep_bumps_internal_workspace_dependency_pins() {
        // The esrun layout: a virtual workspace whose members inherit both the version and their
        // internal deps from the root, which pins those internal path deps in
        // `[workspace.dependencies]`. A bump must move the pins in lockstep with the version.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n\n\
             [workspace.package]\nversion = \"0.9.0\"\n\n\
             [workspace.dependencies]\n\
             es-runtime-core = { path = \"crates/core\", version = \"0.9.0\" }\n\
             serde = \"1\"\n",
        );
        write(
            root.join("crates/core/Cargo.toml"),
            "[package]\nname = \"es-runtime-core\"\nversion.workspace = true\n",
        );
        write(
            root.join("crates/cli/Cargo.toml"),
            "[package]\nname = \"es-runtime-cli\"\nversion.workspace = true\n\n\
             [dependencies]\nes-runtime-core = { workspace = true }\nserde = { workspace = true }\n",
        );
        let adapter = CargoAdapter::new(root);

        // The internal edge is discovered even though the member inherits the dep.
        let pkgs = adapter.discover_packages().unwrap();
        let cli = pkgs.iter().find(|p| p.name == "es-runtime-cli").unwrap();
        assert!(cli
            .internal_deps
            .iter()
            .any(|d| d.name == "es-runtime-core"));

        // A lockstep write bumps the workspace version AND the internal pin (but not `serde`).
        adapter.write_version(cli, "0.10.0").unwrap();
        let root_toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            root_toml
                .contains("es-runtime-core = { path = \"crates/core\", version = \"0.10.0\" }"),
            "internal pin must track the workspace version: {root_toml}"
        );
        assert!(
            root_toml.contains("serde = \"1\""),
            "external pins must be left alone: {root_toml}"
        );

        // The inheriting member is never given a conflicting `version` beside `workspace = true`.
        adapter
            .update_dep_range(cli, "es-runtime-core", "0.10.0")
            .unwrap();
        let cli_toml = fs::read_to_string(root.join("crates/cli/Cargo.toml")).unwrap();
        assert!(
            cli_toml.contains("es-runtime-core = { workspace = true }"),
            "member dep must stay inherited, uncorrupted: {cli_toml}"
        );
        assert!(
            !cli_toml.contains("version ="),
            "no version injected: {cli_toml}"
        );
    }

    #[test]
    fn version_groups_locks_only_inherited_crates_together() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nversion = \"1.2.3\"\n",
        );
        // Two crates inherit the workspace version; one pins its own.
        write(
            root.join("crates/a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion.workspace = true\n",
        );
        write(
            root.join("crates/b/Cargo.toml"),
            "[package]\nname = \"b\"\nversion.workspace = true\n",
        );
        write(
            root.join("crates/c/Cargo.toml"),
            "[package]\nname = \"c\"\nversion = \"0.4.0\"\n",
        );

        let groups = CargoAdapter::new(root).version_groups().unwrap();
        assert_eq!(groups, vec![vec!["a".to_string(), "b".to_string()]]);
    }

    #[test]
    fn version_groups_empty_without_shared_workspace_version() {
        // The default `workspace()` fixture pins concrete versions and has no inheriting crate.
        let tmp = workspace();
        assert!(CargoAdapter::new(tmp.path())
            .version_groups()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn dependent_bump_is_always_patch_and_range_is_bare() {
        let adapter = CargoAdapter::new(".");
        assert_eq!(
            adapter.dependent_bump(Bump::Major, &DepKind::Dep),
            Bump::Patch
        );
        assert_eq!(adapter.format_range("1.2.3"), "1.2.3");
    }

    #[test]
    fn is_published_true_false_and_publish_uses_package_flag() {
        let found = FakeRunner::new(true, "a v1.0.0", "");
        let adapter = CargoAdapter::with_runner("/repo", Box::new(found.clone()));
        let pkg = dummy_pkg("a", Path::new("/repo/crates/a/Cargo.toml"));
        assert!(adapter.is_published(&pkg, "1.0.0").unwrap());

        let missing = FakeRunner::new(false, "", "error: could not find `a` in registry");
        let adapter = CargoAdapter::with_runner("/repo", Box::new(missing));
        assert!(!adapter.is_published(&pkg, "9.9.9").unwrap());

        let pubrunner = FakeRunner::new(true, "", "");
        let adapter = CargoAdapter::with_runner("/repo", Box::new(pubrunner.clone()));
        adapter.publish(&pkg, None).unwrap();
        let calls = pubrunner.calls.lock().unwrap();
        assert_eq!(calls[0].0, ["publish", "-p", "a", "--allow-dirty"]);
        assert_eq!(calls[0].1, PathBuf::from("/repo"));
    }
}
