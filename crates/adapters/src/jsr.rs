use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use glob::glob;
use serde_json::Value;

use crate::command::{CommandRunner, SystemRunner};
use crate::npm::manifest::{strip_jsonc_comments, Manifest};
use otf_release_core::adapter::{Adapter, Bump, DepKind, InternalDep, Pkg};

pub struct JsrAdapter {
    pub root: PathBuf,
    runner: Box<dyn CommandRunner>,
}

/// The shape of a Deno/JSR repo, read off the root manifest (`deno.json`, `deno.jsonc`, or
/// `jsr.json`). Settled before anything looks for members, so a single-package repo is never a
/// workspace whose globs happened to match nothing — the two want different answers, and deciding
/// by "did we find any members?" cannot tell them apart.
enum Layout {
    /// No root manifest at all: nothing to discover. `init` scaffolds one in this case, so this
    /// is an empty result rather than an error.
    Absent,
    /// A root manifest with no `workspace` array: the root is the only package — and only when it
    /// carries both a `name` and a `version`. A bare deno config (tasks, imports, lint rules) is
    /// configuration, not a package.
    Single { root_is_package: bool },
    /// A root manifest with a `workspace` array: members come from its globs, plus the root when
    /// it is a package in its own right (`name` + `version`, e.g. a root package that also hosts
    /// members) rather than a pure workspace config.
    Workspace {
        patterns: Vec<String>,
        root_is_member: bool,
    },
}

/// Classify the repo from its root manifest.
fn layout_of(root: &Path) -> Result<Layout> {
    let Some((_, json)) = read_manifest(root)? else {
        return Ok(Layout::Absent);
    };
    let is_package = json.get("name").is_some() && json.get("version").is_some();
    match json.get("workspace").and_then(Value::as_array) {
        Some(arr) => Ok(Layout::Workspace {
            patterns: arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            root_is_member: is_package,
        }),
        None => Ok(Layout::Single {
            root_is_package: is_package,
        }),
    }
}

impl JsrAdapter {
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

    /// Every package directory in the repo, by [`Layout`]: the root alone for a single-package
    /// repo, the `workspace` globs (plus the root, when it is a package too) for a workspace.
    fn member_dirs(&self) -> Result<Vec<PathBuf>> {
        let (patterns, root_is_member) = match layout_of(&self.root)? {
            Layout::Absent => return Ok(Vec::new()),
            Layout::Single { root_is_package } => {
                return Ok(if root_is_package {
                    vec![self.root.clone()]
                } else {
                    Vec::new()
                })
            }
            Layout::Workspace {
                patterns,
                root_is_member,
            } => (patterns, root_is_member),
        };

        let mut dirs = BTreeSet::new();
        if root_is_member {
            dirs.insert(self.root.clone());
        }
        for pat in patterns {
            let joined = self.root.join(&pat);
            let glob_str = joined
                .to_str()
                .ok_or_else(|| anyhow!("non-UTF-8 path in workspace pattern: {pat}"))?;
            for entry in glob(glob_str).with_context(|| format!("invalid workspace glob: {pat}"))? {
                let path = entry?;
                // A member glob names directories; only those carrying a manifest are packages.
                if path.is_dir() && read_manifest(&path)?.is_some() {
                    dirs.insert(path);
                }
            }
        }
        Ok(dirs.into_iter().collect())
    }
}

impl Adapter for JsrAdapter {
    fn discover_packages(&self) -> Result<Vec<Pkg>> {
        let mut members = Vec::new();
        for dir in self.member_dirs()? {
            if let Some((manifest_path, json)) = read_manifest(&dir)? {
                members.push((dir, manifest_path, json));
            }
        }

        let internal_names: HashSet<String> = members
            .iter()
            .filter_map(|(_, _, json)| json.get("name").and_then(Value::as_str).map(String::from))
            .collect();

        let mut packages = Vec::with_capacity(members.len());
        for (dir, manifest_path, json) in &members {
            let name = json
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("{}: missing name field", manifest_path.display()))?;
            let version = json
                .get("version")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("{}: missing version field", manifest_path.display()))?;

            let publishable = match json.get("publish") {
                Some(Value::Bool(b)) => *b,
                _ => true,
            };

            let mut internal_deps = Vec::new();
            if let Some(imports) = json.get("imports").and_then(Value::as_object) {
                for (key, val) in imports {
                    if let Some(val_str) = val.as_str() {
                        let mut matched_dep = None;
                        for internal_name in &internal_names {
                            if val_str.starts_with(&format!("jsr:{}@", internal_name))
                                || val_str == format!("jsr:{}", internal_name)
                                || val_str.starts_with(&format!("npm:{}@", internal_name))
                                || val_str == format!("npm:{}", internal_name)
                                || (val_str.starts_with("workspace:") && key == internal_name)
                            {
                                matched_dep =
                                    Some((internal_name.clone(), val_str.to_string(), key.clone()));
                                break;
                            }
                        }

                        if let Some((dep_name, specifier, _alias)) = matched_dep {
                            let range = if specifier.starts_with("workspace:") {
                                specifier.strip_prefix("workspace:").unwrap().to_string()
                            } else {
                                let prefix_jsr = format!("jsr:{}@", dep_name);
                                let prefix_npm = format!("npm:{}@", dep_name);
                                if let Some(r) = specifier.strip_prefix(&prefix_jsr) {
                                    r.to_string()
                                } else if let Some(r) = specifier.strip_prefix(&prefix_npm) {
                                    r.to_string()
                                } else {
                                    "*".to_string()
                                }
                            };

                            internal_deps.push(InternalDep {
                                name: dep_name,
                                kind: DepKind::Dep,
                                range,
                            });
                        }
                    }
                }
            }

            packages.push(Pkg {
                name: name.to_string(),
                version: version.to_string(),
                manifest_path: manifest_path.clone(),
                changelog_path: dir.join("CHANGELOG.md"),
                publishable,
                internal_deps,
            });
        }

        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(packages)
    }

    fn write_version(&self, pkg: &Pkg, new: &str) -> Result<()> {
        let mut manifest = Manifest::read(&pkg.manifest_path)?;
        manifest.set_string(&["version"], new)?;
        manifest.save()?;
        Ok(())
    }

    fn update_dep_range(&self, pkg: &Pkg, dep: &str, new_dep_version: &str) -> Result<()> {
        let mut manifest = Manifest::read(&pkg.manifest_path)?;
        let json = manifest.json()?;
        let mut changed = false;

        if let Some(imports) = json.get("imports").and_then(Value::as_object) {
            for (key, val) in imports {
                if let Some(val_str) = val.as_str() {
                    let prefix_jsr = format!("jsr:{}@", dep);
                    let prefix_npm = format!("npm:{}@", dep);
                    let is_workspace = val_str.starts_with("workspace:") && key == dep;

                    if val_str.starts_with(&prefix_jsr)
                        || val_str.starts_with(&prefix_npm)
                        || is_workspace
                    {
                        let new_val_str = if is_workspace {
                            let old_range = val_str.strip_prefix("workspace:").unwrap();
                            let new_range = reformat_range(old_range, new_dep_version);
                            format!("workspace:{}", new_range)
                        } else {
                            let prefix = if val_str.starts_with(&prefix_jsr) {
                                &prefix_jsr
                            } else {
                                &prefix_npm
                            };
                            let old_range = val_str.strip_prefix(prefix).unwrap();
                            let new_range = reformat_range(old_range, new_dep_version);
                            format!("{}{}", prefix, new_range)
                        };

                        if val_str != new_val_str {
                            manifest.set_string(&["imports", key], &new_val_str)?;
                            changed = true;
                        }
                    }
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
        let versions: HashMap<String, String> = self
            .discover_packages()?
            .into_iter()
            .map(|p| (p.name, p.version))
            .collect();

        let mut manifest = Manifest::read(&pkg.manifest_path)?;
        let json = manifest.json()?;
        let mut changed = false;

        if let Some(imports) = json.get("imports").and_then(Value::as_object) {
            for (key, val) in imports {
                if let Some(val_str) = val.as_str() {
                    let mut matched = None;
                    for dep_name in versions.keys() {
                        let prefix_jsr_ws = format!("jsr:{}@workspace:", dep_name);
                        let prefix_npm_ws = format!("npm:{}@workspace:", dep_name);
                        if val_str.starts_with(&prefix_jsr_ws) {
                            matched = Some((
                                dep_name.clone(),
                                val_str
                                    .strip_prefix(&format!("jsr:{}@", dep_name))
                                    .unwrap()
                                    .to_string(),
                                true,
                            ));
                            break;
                        } else if val_str.starts_with(&prefix_npm_ws) {
                            matched = Some((
                                dep_name.clone(),
                                val_str
                                    .strip_prefix(&format!("npm:{}@", dep_name))
                                    .unwrap()
                                    .to_string(),
                                false,
                            ));
                            break;
                        } else if val_str.starts_with("workspace:") && key == dep_name {
                            matched = Some((dep_name.clone(), val_str.to_string(), true));
                            break;
                        }
                    }

                    if let Some((dep_name, ws_spec, is_jsr)) = matched {
                        let version = versions.get(&dep_name).unwrap();
                        let resolved_range = resolve_workspace_range(&ws_spec, version);
                        let scheme = if is_jsr { "jsr" } else { "npm" };
                        let new_val_str = format!("{}:{}@{}", scheme, dep_name, resolved_range);
                        if val_str != new_val_str {
                            manifest.set_string(&["imports", key], &new_val_str)?;
                            changed = true;
                        }
                    }
                }
            }
        }

        if changed {
            manifest.save()?;
        }
        Ok(())
    }

    fn update_lockfile(&self, root: &Path) -> Result<()> {
        if root.join("deno.lock").exists() {
            let out = self.runner.run("deno", &["install"], root)?;
            if !out.success {
                let _ = self.runner.run("deno", &["cache", "deno.json"], root);
            }
        } else if root.join("bun.lock").exists() || root.join("bun.lockb").exists() {
            let out = self
                .runner
                .run("bun", &["install", "--lockfile-only"], root)?;
            if !out.success {
                bail!("`bun install --lockfile-only` failed:\n{}", out.stderr);
            }
        } else if root.join("package-lock.json").exists() {
            let out = self
                .runner
                .run("npm", &["install", "--package-lock-only"], root)?;
            if !out.success {
                bail!("`npm install --package-lock-only` failed:\n{}", out.stderr);
            }
        }
        Ok(())
    }

    fn dependent_bump(&self, _dep_bump: Bump, _kind: &DepKind) -> Bump {
        Bump::Patch
    }

    fn is_published(&self, pkg: &Pkg, version: &str) -> Result<bool> {
        let url = format!("https://jsr.io/{}/meta.json", pkg.name);
        match ureq::get(&url).call() {
            Ok(resp) => {
                let meta: serde_json::Value = resp.into_json()?;
                if let Some(versions) = meta.get("versions").and_then(|v| v.as_object()) {
                    Ok(versions.contains_key(version))
                } else {
                    Ok(false)
                }
            }
            Err(ureq::Error::Status(404, _)) => Ok(false),
            Err(e) => bail!("failed to query JSR registry: {}", e),
        }
    }

    fn publish(&self, pkg: &Pkg, staged_assets: Option<&Path>) -> Result<()> {
        let pkg_dir = pkg.manifest_path.parent().ok_or_else(|| {
            anyhow!(
                "{}: manifest has no parent dir",
                pkg.manifest_path.display()
            )
        })?;

        if let Some(assets) = staged_assets {
            copy_dir_contents(assets, pkg_dir)
                .with_context(|| format!("staging assets for {}", pkg.name))?;
        }

        let mut program = "deno";
        let mut args = vec!["publish"];

        if pkg_dir.join("jsr.json").exists() {
            program = "bunx";
            args = vec!["jsr", "publish"];
        } else if !pkg_dir.join("deno.json").exists() && !pkg_dir.join("deno.jsonc").exists() {
            program = "deno";
            args = vec!["publish"];
        }

        let out = self.runner.run(program, &args, pkg_dir)?;
        if !out.success {
            bail!(
                "`{} {}` for {} failed:\n{}",
                program,
                args.join(" "),
                pkg.name,
                out.stderr
            );
        }
        Ok(())
    }

    fn build_command(&self, pkg: &Pkg) -> Result<Option<String>> {
        let manifest = Manifest::read(&pkg.manifest_path)?;
        let json = manifest.json()?;
        let has_build_task = json
            .get("tasks")
            .and_then(Value::as_object)
            .and_then(|t| t.get("build"))
            .is_some();
        let has_build_script = json
            .get("scripts")
            .and_then(Value::as_object)
            .and_then(|t| t.get("build"))
            .is_some();
        if has_build_task || has_build_script {
            Ok(Some("deno task build".to_string()))
        } else {
            Ok(None)
        }
    }
}

fn read_manifest(dir: &Path) -> Result<Option<(PathBuf, Value)>> {
    let names = ["deno.json", "deno.jsonc", "jsr.json"];
    for name in names {
        let path = dir.join(name);
        if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("reading manifest {}", path.display()))?;
            let cleaned = if name.ends_with(".jsonc") {
                strip_jsonc_comments(&content)
            } else {
                content
            };
            let json: Value = serde_json::from_str(&cleaned)
                .with_context(|| format!("parsing manifest {}", path.display()))?;
            return Ok(Some((path, json)));
        }
    }
    Ok(None)
}

fn reformat_range(old: &str, new_version: &str) -> String {
    let trimmed = old.trim();
    if trimmed.starts_with("workspace:") {
        return old.to_string();
    }
    match trimmed.find(|c: char| c.is_ascii_digit()) {
        None => old.to_string(),
        Some(prefix_len) => format!("{}{new_version}", &trimmed[..prefix_len]),
    }
}

fn resolve_workspace_range(range: &str, version: &str) -> String {
    let spec = range.strip_prefix("workspace:").unwrap_or(range);
    match spec {
        "*" | "" => version.to_string(),
        "^" => format!("^{version}"),
        "~" => format!("~{version}"),
        explicit => explicit.to_string(),
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandOutput;
    use std::sync::{Arc, Mutex};

    type Calls = Arc<Mutex<Vec<(String, Vec<String>, PathBuf)>>>;

    #[derive(Clone)]
    struct FakeRunner {
        success: bool,
        stdout: String,
        stderr: String,
        calls: Calls,
    }

    impl FakeRunner {
        fn new(success: bool, stdout: &str, stderr: &str) -> Self {
            Self {
                success,
                stdout: stdout.to_string(),
                stderr: stderr.to_string(),
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
            Ok(CommandOutput {
                success: self.success,
                stdout: self.stdout.clone(),
                stderr: self.stderr.clone(),
            })
        }
    }

    fn write_file(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn test_discover_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            &root.join("deno.json"),
            r#"{
            "workspace": ["packages/*"]
        }"#,
        );

        write_file(
            &root.join("packages/a/deno.json"),
            r#"{
            "name": "@scope/a",
            "version": "1.0.0",
            "imports": {
                "@scope/b": "jsr:@scope/b@^2.0.0"
            }
        }"#,
        );

        write_file(
            &root.join("packages/b/deno.jsonc"),
            r#"{
            // Comment test
            "name": "@scope/b",
            "version": "2.0.0",
            "publish": false
        }"#,
        );

        let adapter = JsrAdapter::new(root);
        let pkgs = adapter.discover_packages().unwrap();

        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0].name, "@scope/a");
        assert_eq!(pkgs[0].version, "1.0.0");
        assert!(pkgs[0].publishable);
        assert_eq!(pkgs[0].internal_deps.len(), 1);
        assert_eq!(pkgs[0].internal_deps[0].name, "@scope/b");
        assert_eq!(pkgs[0].internal_deps[0].range, "^2.0.0");

        assert_eq!(pkgs[1].name, "@scope/b");
        assert_eq!(pkgs[1].version, "2.0.0");
        assert!(!pkgs[1].publishable);
    }

    #[test]
    fn discovers_the_root_package_of_a_single_package_repo() {
        // No `workspace` array: the root manifest is the one package.
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            &tmp.path().join("deno.json"),
            r#"{ "name": "@scope/solo", "version": "1.2.3" }"#,
        );

        let pkgs = JsrAdapter::new(tmp.path()).discover_packages().unwrap();
        assert_eq!(pkgs.len(), 1, "got: {pkgs:?}");
        assert_eq!(pkgs[0].name, "@scope/solo");
        assert_eq!(pkgs[0].version, "1.2.3");
    }

    #[test]
    fn bare_deno_config_is_not_a_package() {
        // A root manifest that only configures Deno — no name/version — is not releasable, and
        // must not be reported as one just because no members were found.
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            &tmp.path().join("deno.json"),
            r#"{ "tasks": { "dev": "deno run -A main.ts" } }"#,
        );

        let pkgs = JsrAdapter::new(tmp.path()).discover_packages().unwrap();
        assert!(pkgs.is_empty(), "got: {pkgs:?}");
    }

    #[test]
    fn workspace_with_no_matching_members_is_empty_not_the_root() {
        // The case the old `if member_dirs.is_empty()` fallback got wrong: a workspace whose
        // globs match nothing is an empty workspace, not a single-package repo. Its root is a
        // package here, so the fallback would have published it as one.
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            &tmp.path().join("deno.json"),
            r#"{ "name": "@scope/root", "version": "1.0.0", "workspace": ["packages/*"] }"#,
        );

        let pkgs = JsrAdapter::new(tmp.path()).discover_packages().unwrap();
        let names: Vec<_> = pkgs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["@scope/root"],
            "the root is a member on its own merits"
        );
    }

    #[test]
    fn workspace_root_that_is_itself_a_package_is_a_member() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(
            &root.join("deno.json"),
            r#"{ "name": "@scope/top", "version": "3.0.0", "workspace": ["packages/*"] }"#,
        );
        write_file(
            &root.join("packages/a/deno.json"),
            r#"{ "name": "@scope/a", "version": "1.0.0" }"#,
        );

        let pkgs = JsrAdapter::new(root).discover_packages().unwrap();
        let names: Vec<_> = pkgs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["@scope/a", "@scope/top"]);
    }

    #[test]
    fn no_root_manifest_discovers_nothing() {
        // `init` calls discovery before scaffolding a jsr.json — an empty result, not an error.
        let tmp = tempfile::tempdir().unwrap();
        assert!(JsrAdapter::new(tmp.path())
            .discover_packages()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_publish() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        write_file(
            &root.join("deno.json"),
            r#"{
            "name": "@scope/pkg",
            "version": "1.0.0"
        }"#,
        );

        let runner = FakeRunner::new(true, "published successfully", "");
        let adapter = JsrAdapter::with_runner(root, Box::new(runner.clone()));
        let pkg = Pkg {
            name: "@scope/pkg".to_string(),
            version: "1.0.0".to_string(),
            publishable: true,
            manifest_path: root.join("deno.json"),
            changelog_path: std::path::PathBuf::new(),
            internal_deps: Vec::new(),
        };

        adapter.publish(&pkg, None).unwrap();

        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "deno");
        assert_eq!(calls[0].1, vec!["publish".to_string()]);
    }
}
