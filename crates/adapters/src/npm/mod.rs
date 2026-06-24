//! The npm adapter — the only implemented adapter in v1.
//!
//! Baked-in rules & gotchas (see `docs/adapters/npm.md`), already battle-tested:
//!
//! - `dependent_bump`: `PeerDep => mirror(dep_bump)`; everything else => `Patch`.
//! - `is_published`: `npm view <name>@<version> version` succeeds => already published => skip.
//! - `publish`: `npm publish --access public --no-workspaces`
//!     - `--access public` is required for a scoped package's first publish.
//!     - `--no-workspaces` is required because the repo root is a private workspace; without it
//!       npm runs in workspace mode and skips the package even from its own directory.
//! - `resolve_workspace_links`: rewrite `workspace:*` / linked internal deps to the concrete
//!   published version before publish (npm does not do this automatically).
//! - No `private:true` guard hack — asset packages are normal publishable packages.

mod manifest;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use glob::glob;
use serde_json::Value;

use opentf_release_core::adapter::{Adapter, Bump, DepKind, InternalDep, Pkg};

// Re-exported so existing `opentf_release_adapters::npm::{CommandRunner, ...}` paths still work.
pub use crate::command::{CommandOutput, CommandRunner, SystemRunner};

use manifest::{Manifest, DEP_SECTIONS};

/// npm-backed adapter. Rooted at the workspace directory.
pub struct NpmAdapter {
    pub root: PathBuf,
    runner: Box<dyn CommandRunner>,
}

impl NpmAdapter {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            runner: Box::new(SystemRunner),
        }
    }

    /// Construct with a custom command runner (used in tests).
    pub fn with_runner(root: impl Into<PathBuf>, runner: Box<dyn CommandRunner>) -> Self {
        Self {
            root: root.into(),
            runner,
        }
    }

    /// Expand the root `package.json` `workspaces` field into member package directories.
    fn member_dirs(&self) -> Result<Vec<PathBuf>> {
        let root_json = Manifest::read(&self.root.join("package.json"))?.json_value()?;
        let patterns = workspace_patterns(&root_json);

        let mut dirs = BTreeSet::new();
        for pattern in patterns {
            let joined = self.root.join(&pattern).join("package.json");
            let glob_str = joined
                .to_str()
                .ok_or_else(|| anyhow!("non-UTF-8 path in workspace pattern: {pattern}"))?;
            for entry in
                glob(glob_str).with_context(|| format!("invalid workspace glob: {pattern}"))?
            {
                let manifest_path = entry?;
                if let Some(dir) = manifest_path.parent() {
                    dirs.insert(dir.to_path_buf());
                }
            }
        }
        Ok(dirs.into_iter().collect())
    }
}

impl Adapter for NpmAdapter {
    fn discover_packages(&self) -> Result<Vec<Pkg>> {
        // First pass: read every member manifest so we know the full set of internal names.
        let mut members: Vec<(PathBuf, Manifest)> = Vec::new();
        for dir in self.member_dirs()? {
            let manifest_path = dir.join("package.json");
            let manifest = Manifest::read(&manifest_path)?;
            members.push((dir, manifest));
        }

        let internal_names: HashSet<String> =
            members.iter().filter_map(|(_, m)| m.name().ok()).collect();

        // Second pass: build packages, keeping only edges that point at another member.
        let mut packages = Vec::with_capacity(members.len());
        for (dir, manifest) in &members {
            let internal_deps = manifest
                .deps()?
                .into_iter()
                .filter(|d| internal_names.contains(&d.name))
                .map(|d| InternalDep {
                    name: d.name,
                    kind: kind_of(d.section),
                    range: d.range,
                })
                .collect();

            packages.push(Pkg {
                name: manifest.name()?,
                version: manifest.version()?,
                manifest_path: dir.join("package.json"),
                changelog_path: dir.join("CHANGELOG.md"),
                publishable: !manifest.is_private(),
                internal_deps,
            });
        }

        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(packages)
    }

    fn write_version(&self, pkg: &Pkg, new: &str) -> Result<()> {
        let mut manifest = Manifest::read(&pkg.manifest_path)?;
        if !manifest.set_string(&["version"], new)? {
            bail!(
                "{}: no \"version\" field to write",
                pkg.manifest_path.display()
            );
        }
        manifest.save()
    }

    fn update_dep_range(&self, pkg: &Pkg, dep: &str, new_dep_version: &str) -> Result<()> {
        let mut manifest = Manifest::read(&pkg.manifest_path)?;
        let mut changed = false;
        for section in DEP_SECTIONS {
            if let Some(old) = manifest.get_string(&[section, dep]) {
                let new_range = reformat_range(&old, new_dep_version);
                if new_range != old && manifest.set_string(&[section, dep], &new_range)? {
                    changed = true;
                }
            }
        }
        if changed {
            manifest.save()?;
        }
        Ok(())
    }

    fn format_range(&self, version: &str) -> String {
        format!("^{version}")
    }

    fn resolve_workspace_links(&self, pkg: &Pkg) -> Result<()> {
        // Map every internal package to its current concrete version.
        let versions: HashMap<String, String> = self
            .discover_packages()?
            .into_iter()
            .map(|p| (p.name, p.version))
            .collect();

        let mut manifest = Manifest::read(&pkg.manifest_path)?;
        let mut changed = false;
        for dep in manifest.deps()? {
            if !dep.range.starts_with("workspace:") {
                continue;
            }
            let Some(version) = versions.get(&dep.name) else {
                continue; // not an internal package; leave it for npm to resolve
            };
            let concrete = resolve_workspace_range(&dep.range, version);
            if manifest.set_string(&[dep.section, &dep.name], &concrete)? {
                changed = true;
            }
        }
        if changed {
            manifest.save()?;
        }
        Ok(())
    }

    fn update_lockfile(&self, root: &Path) -> Result<()> {
        let out = self
            .runner
            .run("npm", &["install", "--package-lock-only"], root)?;
        if !out.success {
            bail!("`npm install --package-lock-only` failed:\n{}", out.stderr);
        }
        Ok(())
    }

    fn dependent_bump(&self, dep_bump: Bump, kind: &DepKind) -> Bump {
        match kind {
            DepKind::PeerDep => dep_bump, // mirror
            _ => Bump::Patch,
        }
    }

    fn is_published(&self, pkg: &Pkg, version: &str) -> Result<bool> {
        let spec = format!("{}@{}", pkg.name, version);
        let out = self
            .runner
            .run("npm", &["view", &spec, "version"], &self.root)?;
        if out.success {
            return Ok(!out.stdout.trim().is_empty());
        }
        // A missing version is the expected "not published" signal, not an error.
        if out.stderr.contains("E404") || out.stderr.contains("404") {
            return Ok(false);
        }
        bail!("`npm view {spec} version` failed:\n{}", out.stderr);
    }

    fn publish(&self, pkg: &Pkg, staged_assets: Option<&Path>) -> Result<()> {
        let pkg_dir = pkg.manifest_path.parent().ok_or_else(|| {
            anyhow!(
                "{}: manifest has no parent dir",
                pkg.manifest_path.display()
            )
        })?;

        // Attach staged binaries (if any) by copying them into the package before packing.
        if let Some(assets) = staged_assets {
            copy_dir_contents(assets, pkg_dir)
                .with_context(|| format!("staging assets for {}", pkg.name))?;
        }

        let out = self.runner.run(
            "npm",
            &["publish", "--access", "public", "--no-workspaces"],
            pkg_dir,
        )?;
        if !out.success {
            bail!("`npm publish` for {} failed:\n{}", pkg.name, out.stderr);
        }
        Ok(())
    }
}

/// `dependencies`/`optionalDependencies` -> `Dep`; `peerDependencies` -> `PeerDep`;
/// `devDependencies` -> `DevDep`.
fn kind_of(section: &str) -> DepKind {
    match section {
        "peerDependencies" => DepKind::PeerDep,
        "devDependencies" => DepKind::DevDep,
        _ => DepKind::Dep,
    }
}

/// Read the `workspaces` field, supporting both the array form and the
/// `{ "packages": [...] }` object form.
fn workspace_patterns(root_json: &Value) -> Vec<String> {
    let strings = |arr: &Vec<Value>| {
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect::<Vec<_>>()
    };
    match root_json.get("workspaces") {
        Some(Value::Array(arr)) => strings(arr),
        Some(Value::Object(obj)) => obj
            .get("packages")
            .and_then(Value::as_array)
            .map(strings)
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Rewrite a concrete range while preserving its leading operator (`^`, `~`, `>=`, exact, …).
/// `workspace:` ranges and pure tags (`*`, `latest`) are left untouched.
fn reformat_range(old: &str, new_version: &str) -> String {
    let trimmed = old.trim();
    if trimmed.starts_with("workspace:") {
        return old.to_string();
    }
    match trimmed.find(|c: char| c.is_ascii_digit()) {
        None => old.to_string(), // no version component (e.g. "*", "latest")
        Some(prefix_len) => format!("{}{new_version}", &trimmed[..prefix_len]),
    }
}

/// Resolve a `workspace:` protocol range against the dependency's concrete version:
/// `workspace:*`/`workspace:` -> exact, `workspace:^` -> `^v`, `workspace:~` -> `~v`,
/// `workspace:1.2.3` -> `1.2.3`.
fn resolve_workspace_range(range: &str, version: &str) -> String {
    let spec = range.strip_prefix("workspace:").unwrap_or(range);
    match spec {
        "*" | "" => version.to_string(),
        "^" => format!("^{version}"),
        "~" => format!("~{version}"),
        explicit => explicit.to_string(),
    }
}

/// Recursively copy the *contents* of `src` into `dst` (which must already exist).
fn copy_dir_contents(src: &Path, dst: &Path) -> Result<()> {
    for entry in fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&to)?;
            copy_dir_contents(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .with_context(|| format!("copying {} -> {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

// `Manifest::json` is private to the module; expose what discovery needs via a thin shim.
impl Manifest {
    fn json_value(&self) -> Result<Value> {
        serde_json::from_str(self.content())
            .with_context(|| format!("parsing {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type Calls = Arc<Mutex<Vec<(String, Vec<String>, PathBuf)>>>;

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
                    stdout: stdout.to_string(),
                    stderr: stderr.to_string(),
                },
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&str], cwd: &Path) -> Result<CommandOutput> {
            self.calls.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
                cwd.to_path_buf(),
            ));
            Ok(self.out.clone())
        }
    }

    fn dummy_pkg(name: &str, manifest_path: &str) -> Pkg {
        Pkg {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            manifest_path: PathBuf::from(manifest_path),
            changelog_path: PathBuf::from("CHANGELOG.md"),
            publishable: true,
            internal_deps: vec![],
        }
    }

    fn write(path: PathBuf, content: &str) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn discovers_packages_and_only_internal_edges() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root.join("package.json"),
            r#"{ "name": "root", "private": true, "workspaces": ["packages/*"] }"#,
        );
        write(
            root.join("packages/a/package.json"),
            r#"{ "name": "@x/a", "version": "1.0.0" }"#,
        );
        write(
            root.join("packages/b/package.json"),
            r#"{ "name": "@x/b", "version": "2.0.0",
                "peerDependencies": { "@x/a": "^1.0.0" },
                "dependencies": { "left-pad": "^1.0.0" } }"#,
        );
        write(
            root.join("packages/c/package.json"),
            r#"{ "name": "@x/c", "version": "0.0.0", "private": true,
                "dependencies": { "@x/a": "^1.0.0" } }"#,
        );

        let adapter = NpmAdapter::new(root);
        let pkgs = adapter.discover_packages().unwrap();

        let names: Vec<_> = pkgs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["@x/a", "@x/b", "@x/c"]);

        let b = pkgs.iter().find(|p| p.name == "@x/b").unwrap();
        assert!(b.publishable);
        assert_eq!(b.internal_deps.len(), 1, "left-pad must be excluded");
        assert_eq!(b.internal_deps[0].name, "@x/a");
        assert_eq!(b.internal_deps[0].kind, DepKind::PeerDep);

        let c = pkgs.iter().find(|p| p.name == "@x/c").unwrap();
        assert!(!c.publishable, "private app is not publishable");
    }

    #[test]
    fn write_version_and_update_range_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let mp = tmp.path().join("package.json");
        write(
            mp.clone(),
            "{\n  \"name\": \"@x/b\",\n  \"version\": \"1.0.0\",\n  \"dependencies\": { \"@x/a\": \"^1.0.0\" }\n}\n",
        );

        let adapter = NpmAdapter::new(tmp.path());
        let pkg = dummy_pkg("@x/b", mp.to_str().unwrap());

        adapter.write_version(&pkg, "1.1.0").unwrap();
        adapter.update_dep_range(&pkg, "@x/a", "2.0.0").unwrap();

        let after = fs::read_to_string(&mp).unwrap();
        assert_eq!(
            after,
            "{\n  \"name\": \"@x/b\",\n  \"version\": \"1.1.0\",\n  \"dependencies\": { \"@x/a\": \"^2.0.0\" }\n}\n"
        );
    }

    #[test]
    fn resolve_workspace_links_injects_concrete_versions() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root.join("package.json"),
            r#"{ "name": "root", "private": true, "workspaces": ["packages/*"] }"#,
        );
        write(
            root.join("packages/a/package.json"),
            r#"{ "name": "@x/a", "version": "1.4.2" }"#,
        );
        let b_path = root.join("packages/b/package.json");
        write(
            b_path.clone(),
            r#"{ "name": "@x/b", "version": "2.0.0", "dependencies": { "@x/a": "workspace:^" } }"#,
        );

        let adapter = NpmAdapter::new(root);
        let pkg = dummy_pkg("@x/b", b_path.to_str().unwrap());
        adapter.resolve_workspace_links(&pkg).unwrap();

        let after = fs::read_to_string(&b_path).unwrap();
        assert!(after.contains(r#""@x/a": "^1.4.2""#), "got: {after}");
    }

    #[test]
    fn is_published_true_on_success() {
        let fake = FakeRunner::new(true, "1.2.3\n", "");
        let adapter = NpmAdapter::with_runner("/repo", Box::new(fake.clone()));
        let pkg = dummy_pkg("@x/a", "/repo/packages/a/package.json");

        assert!(adapter.is_published(&pkg, "1.2.3").unwrap());
        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls[0].0, "npm");
        assert_eq!(calls[0].1, ["view", "@x/a@1.2.3", "version"]);
    }

    #[test]
    fn is_published_false_on_404() {
        let fake = FakeRunner::new(false, "", "npm error code E404\nnot found");
        let adapter = NpmAdapter::with_runner("/repo", Box::new(fake));
        let pkg = dummy_pkg("@x/a", "/repo/packages/a/package.json");
        assert!(!adapter.is_published(&pkg, "9.9.9").unwrap());
    }

    #[test]
    fn publish_uses_the_required_flags_in_the_package_dir() {
        let fake = FakeRunner::new(true, "", "");
        let adapter = NpmAdapter::with_runner("/repo", Box::new(fake.clone()));
        let pkg = dummy_pkg("@x/a", "/repo/packages/a/package.json");

        adapter.publish(&pkg, None).unwrap();
        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls[0].0, "npm");
        assert_eq!(
            calls[0].1,
            ["publish", "--access", "public", "--no-workspaces"]
        );
        assert_eq!(calls[0].2, PathBuf::from("/repo/packages/a"));
    }

    #[test]
    fn reformat_range_preserves_operator() {
        assert_eq!(reformat_range("^1.0.0", "2.0.0"), "^2.0.0");
        assert_eq!(reformat_range("~1.0.0", "2.0.0"), "~2.0.0");
        assert_eq!(reformat_range("1.0.0", "2.0.0"), "2.0.0");
        assert_eq!(reformat_range(">=1.0.0", "2.0.0"), ">=2.0.0");
        assert_eq!(reformat_range("*", "2.0.0"), "*");
        assert_eq!(reformat_range("workspace:^", "2.0.0"), "workspace:^");
    }

    #[test]
    fn resolve_workspace_range_mapping() {
        assert_eq!(resolve_workspace_range("workspace:*", "1.2.3"), "1.2.3");
        assert_eq!(resolve_workspace_range("workspace:^", "1.2.3"), "^1.2.3");
        assert_eq!(resolve_workspace_range("workspace:~", "1.2.3"), "~1.2.3");
        assert_eq!(resolve_workspace_range("workspace:1.0.0", "1.2.3"), "1.0.0");
    }
}
