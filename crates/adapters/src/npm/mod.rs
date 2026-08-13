//! The npm adapter — the npm / Node ecosystem.
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

pub(crate) mod manifest;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use glob::glob;
use serde_json::Value;

use otf_release_core::adapter::{Adapter, Bump, DepKind, InternalDep, Pkg};
use otf_release_core::discover::pnpm_workspace_patterns;

// Re-exported so existing `otf_release_adapters::npm::{CommandRunner, ...}` paths still work.
pub use crate::command::{CommandOutput, CommandRunner, SystemRunner};

use manifest::{Manifest, DEP_SECTIONS};

/// npm-backed adapter. Rooted at the workspace directory.
pub struct NpmAdapter {
    pub root: PathBuf,
    runner: Box<dyn CommandRunner>,
    /// Explicit member directory globs from `release.toml`'s `[discovery] npm`. Non-empty ⇒ they
    /// *are* the member set and the root `package.json` is never consulted for `workspaces`.
    packages: Vec<String>,
    /// Package names configured with `provenance = true`. Publishing these passes `--provenance`,
    /// which signs the tarball with the workflow's OIDC identity — the npm equivalent of the
    /// attestation build-only assets get. Carried here because it lives in `release.toml`, which
    /// is also what grants the workflow the `id-token: write` the signature needs.
    provenance: Vec<String>,
}

/// A workspace manifest that npm discovery intentionally ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedWorkspaceManifest {
    pub path: PathBuf,
    pub reason: String,
}

/// The shape of a Node repo, read off the root `package.json`. Which shape it is decides where the
/// member packages come from, so it is settled first — a single-package repo is not a workspace
/// whose `workspaces` globs happen to match nothing.
enum Layout {
    /// No `workspaces` field: the root manifest *is* the one package.
    Single,
    /// A `workspaces` field: members come from its globs. The root joins them only when it is a
    /// real package itself (a `name` and a `version`); a workspace root is usually a private
    /// container, and pulling it in regardless would report it as a skipped manifest on every run.
    Workspace {
        patterns: Vec<String>,
        root_is_member: bool,
    },
}

impl NpmAdapter {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            runner: Box::new(SystemRunner),
            packages: Vec::new(),
            provenance: Vec::new(),
        }
    }

    /// Construct with a custom command runner (used in tests).
    pub fn with_runner(root: impl Into<PathBuf>, runner: Box<dyn CommandRunner>) -> Self {
        Self {
            root: root.into(),
            runner,
            packages: Vec::new(),
            provenance: Vec::new(),
        }
    }

    /// Take the member directories from an explicit declaration (`[discovery] npm`) instead of the
    /// root `package.json`. For a repo whose root belongs to another ecosystem, this is the only
    /// way to name the JS packages without restructuring how the repo installs.
    pub fn with_packages(mut self, packages: Vec<String>) -> Self {
        self.packages = packages;
        self
    }

    /// Names of packages that publish with `--provenance`.
    pub fn with_provenance(mut self, provenance: Vec<String>) -> Self {
        self.provenance = provenance;
        self
    }

    /// Whether the repo root is itself a package (a `package.json` with a `name` and a `version`),
    /// as opposed to a private container for the members.
    fn root_is_package(&self) -> Result<bool> {
        let path = self.root.join("package.json");
        if !path.exists() {
            return Ok(false);
        }
        Ok(skip_reason(&Manifest::read(&path)?)?.is_none())
    }

    /// Directories of every `package.json` matched by `patterns` (each pattern names a directory).
    ///
    /// A pattern starting with `!` is an exclusion, which npm, pnpm, and bun all accept — it must
    /// remove a directory the other patterns matched rather than being globbed as a literal path,
    /// or a member the repo deliberately excluded would be released. Exclusions are resolved by
    /// globbing them the same way as the includes and subtracting: matching a pattern string
    /// against an already-globbed path instead would compare two different spellings of the same
    /// directory (`./packages/x` vs `packages/x`) whenever the root is relative.
    fn dirs_matching(&self, patterns: &[String]) -> Result<BTreeSet<PathBuf>> {
        let mut included = BTreeSet::new();
        let mut excluded = BTreeSet::new();
        for pattern in patterns {
            match pattern.strip_prefix('!') {
                Some(negated) => excluded.extend(self.glob_dirs(negated)?),
                None => included.extend(self.glob_dirs(pattern)?),
            }
        }
        Ok(included.difference(&excluded).cloned().collect())
    }

    /// Directories under `root` holding a `package.json` whose path matches `pattern`.
    fn glob_dirs(&self, pattern: &str) -> Result<BTreeSet<PathBuf>> {
        let joined = self.root.join(pattern).join("package.json");
        let glob_str = joined
            .to_str()
            .ok_or_else(|| anyhow!("non-UTF-8 path in workspace pattern: {pattern}"))?;
        let mut dirs = BTreeSet::new();
        for entry in glob(glob_str).with_context(|| format!("invalid workspace glob: {pattern}"))? {
            if let Some(dir) = entry?.parent() {
                dirs.insert(dir.to_path_buf());
            }
        }
        Ok(dirs)
    }

    /// Every package directory in the repo: the explicitly declared globs when `release.toml`
    /// carries them, otherwise by [`Layout`] — the root alone for a single-package repo, the
    /// `workspaces` globs (plus the root, when it is a package too) for a workspace.
    fn member_dirs(&self) -> Result<Vec<PathBuf>> {
        // An explicit declaration wins outright. It exists precisely for repos with no root
        // `package.json`, so falling back to reading one would defeat it.
        if !self.packages.is_empty() {
            return Ok(self.dirs_matching(&self.packages)?.into_iter().collect());
        }

        // pnpm keeps its member list in `pnpm-workspace.yaml`, not in package.json, and ignores a
        // `workspaces` field entirely — so when that file declares packages it is what actually
        // installs the repo, and it wins here for the same reason.
        if let Some(patterns) = pnpm_workspace_patterns(&self.root)? {
            let mut dirs = self.dirs_matching(&patterns)?;
            if self.root_is_package()? {
                dirs.insert(self.root.clone());
            }
            return Ok(dirs.into_iter().collect());
        }

        // No root manifest means this repo declares no npm packages here — not a failure. A
        // polyglot repo's root routinely belongs to another ecosystem, and erroring would take
        // `check`/`version`/`publish` down for every *other* enabled adapter along with it.
        let root_manifest_path = self.root.join("package.json");
        if !root_manifest_path.exists() {
            return Ok(Vec::new());
        }

        let root_manifest = Manifest::read(&root_manifest_path)?;
        let (patterns, root_is_member) = match layout_of(&root_manifest)? {
            Layout::Single => return Ok(vec![self.root.clone()]),
            Layout::Workspace {
                patterns,
                root_is_member,
            } => (patterns, root_is_member),
        };

        let mut dirs = self.dirs_matching(&patterns)?;
        if root_is_member {
            dirs.insert(self.root.clone());
        }
        Ok(dirs.into_iter().collect())
    }

    /// Workspace manifests that are valid JSON but are not release packages.
    pub fn skipped_workspace_manifests(&self) -> Result<Vec<SkippedWorkspaceManifest>> {
        let mut skipped = Vec::new();
        for dir in self.member_dirs()? {
            let manifest_path = dir.join("package.json");
            let manifest = Manifest::read(&manifest_path)?;
            if let Some(reason) = skip_reason(&manifest)? {
                skipped.push(SkippedWorkspaceManifest {
                    path: manifest_path,
                    reason,
                });
            }
        }
        Ok(skipped)
    }
}

impl Adapter for NpmAdapter {
    fn discover_packages(&self) -> Result<Vec<Pkg>> {
        // First pass: read every member manifest so we know the full set of internal names.
        let mut members: Vec<(PathBuf, Manifest)> = Vec::new();
        for dir in self.member_dirs()? {
            let manifest_path = dir.join("package.json");
            let manifest = Manifest::read(&manifest_path)?;
            if skip_reason(&manifest)?.is_some() {
                continue;
            }
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

    fn preview_range(&self, old_range: &str, new_version: &str) -> String {
        reformat_range(old_range, new_version)
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
        // Nothing at the root to refresh: no lockfile *and* no manifest to install from. That is
        // the explicitly-declared layout, where each package is its own install with its own
        // lockfile beside it — running a root install there fails outright.
        if !has_root_lockfile(root) && !root.join("package.json").exists() {
            return Ok(());
        }
        let (program, args) = lockfile_update_command(root);
        let out = self.runner.run(program, &args, root)?;
        if !out.success {
            bail!("`{} {}` failed:\n{}", program, args.join(" "), out.stderr);
        }
        Ok(())
    }

    fn dependent_bump(&self, dep_bump: Bump, kind: &DepKind) -> Bump {
        match kind {
            // A peerDep mirrors its dependency's bump — except `Initial`, which means "ship the
            // manifest version unchanged". That is only ever right for the package that has never
            // shipped; mirroring it onto a dependent that *has* would re-release a version it
            // already published, and collide with that version's existing tag.
            DepKind::PeerDep if dep_bump != Bump::Initial => dep_bump,
            _ => Bump::Patch,
        }
    }

    fn is_published(&self, pkg: &Pkg, version: &str) -> Result<bool> {
        let spec = format!("{}@{}", pkg.name, version);
        // Retried: a dropped connection here would otherwise abort a release before it published
        // anything. A 404 — the expected "not published yet" — is not transient and returns at once.
        let out = crate::command::run_probe(
            self.runner.as_ref(),
            "npm",
            &["view", &spec, "version"],
            &self.root,
        )?;
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

        // `--access` on the command line beats `publishConfig.access` in the manifest, so passing
        // a hardcoded `public` would publish a package that asked to stay restricted — once, and
        // irreversibly. Honour the manifest when it has an opinion; default to `public` only when
        // it does not, since that is what a scoped package's *first* publish needs to succeed.
        let access = Manifest::read(&pkg.manifest_path)
            .ok()
            .and_then(|manifest| manifest.publish_access())
            .unwrap_or_else(|| "public".to_string());

        // A prerelease version (e.g. a `1.2.3-dev.<hash>` snapshot) must publish under its own
        // dist-tag, never `latest`, so an automated snapshot never becomes the default install.
        let mut args = vec!["publish", "--access", &access, "--no-workspaces"];
        if self.provenance.contains(&pkg.name) {
            args.push("--provenance");
        }
        let tag = dist_tag(&pkg.version);
        if let Some(tag) = &tag {
            args.push("--tag");
            args.push(tag);
        }
        let out = self.runner.run("npm", &args, pkg_dir)?;
        if !out.success {
            bail!("`npm publish` for {} failed:\n{}", pkg.name, out.stderr);
        }
        Ok(())
    }

    /// The tool owns the build: if the package declares a `scripts.build`, it runs `npm run build`
    /// in the publish job before `npm publish`. A package without a build script publishes as-is.
    fn build_command(&self, pkg: &Pkg) -> Result<Option<String>> {
        let manifest = Manifest::read(&pkg.manifest_path)?;
        Ok(manifest
            .script("build")
            .map(|_| "npm run build".to_string()))
    }

    /// Strip npm's pack/publish lifecycle hooks so npm can't re-run a build behind the tool's back.
    /// Returns the removed hook names; saves the manifest only when something changed.
    fn strip_publish_hooks(&self, pkg: &Pkg) -> Result<Vec<String>> {
        let mut manifest = Manifest::read(&pkg.manifest_path)?;
        let mut removed = Vec::new();
        for hook in PUBLISH_LIFECYCLE_HOOKS {
            if manifest.remove_key(&["scripts", hook])? {
                removed.push(hook.to_string());
            }
        }
        if !removed.is_empty() {
            manifest.save()?;
        }
        Ok(removed)
    }
}

/// npm lifecycle scripts that run a build at pack/publish time. The tool injects `npm run build`
/// into the publish job itself, so these are removed to prevent a double build (or surprising
/// publish-time behavior the release pipeline doesn't control).
const PUBLISH_LIFECYCLE_HOOKS: [&str; 4] = ["prepublish", "prepublishOnly", "prepack", "prepare"];

/// The npm dist-tag for a version: a prerelease's leading identifier (`1.2.3-dev.abc` → `dev`,
/// `2.0.0-beta.1` → `beta`), or `None` for a normal release (which publishes under `latest`).
fn dist_tag(version: &str) -> Option<String> {
    let pre = version.split_once('-')?.1;
    let id = pre.split('.').next().unwrap_or(pre);
    (!id.is_empty()).then(|| id.to_string())
}

fn skip_reason(manifest: &Manifest) -> Result<Option<String>> {
    let json = manifest.json_value()?;
    let missing_name = json.get("name").and_then(Value::as_str).is_none();
    let missing_version = json.get("version").and_then(Value::as_str).is_none();

    Ok(match (missing_name, missing_version) {
        (true, true) => Some("missing \"name\" and \"version\"".to_string()),
        (true, false) => Some("missing \"name\"".to_string()),
        (false, true) => Some("missing \"version\"".to_string()),
        (false, false) => None,
    })
}

/// Whether the root carries any package manager's lockfile.
fn has_root_lockfile(root: &Path) -> bool {
    [
        "bun.lock",
        "bun.lockb",
        "pnpm-lock.yaml",
        "yarn.lock",
        "package-lock.json",
    ]
    .iter()
    .any(|f| root.join(f).exists())
}

fn lockfile_update_command(root: &Path) -> (&'static str, Vec<&'static str>) {
    if root.join("bun.lock").exists() || root.join("bun.lockb").exists() {
        ("bun", vec!["install", "--lockfile-only"])
    } else if root.join("pnpm-lock.yaml").exists() {
        ("pnpm", vec!["install", "--lockfile-only"])
    } else if root.join("yarn.lock").exists() {
        ("yarn", vec!["install", "--mode=update-lockfile"])
    } else {
        ("npm", vec!["install", "--package-lock-only"])
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
/// Classify the repo from the root manifest's `workspaces` field.
fn layout_of(root: &Manifest) -> Result<Layout> {
    let json = root.json_value()?;
    // Only the two shapes npm itself accepts count as a workspace declaration; anything else
    // (absent, or a malformed value) is a plain single-package repo.
    let is_workspace = matches!(
        json.get("workspaces"),
        Some(Value::Array(_)) | Some(Value::Object(_))
    );
    if !is_workspace {
        return Ok(Layout::Single);
    }
    Ok(Layout::Workspace {
        patterns: workspace_patterns(&json),
        root_is_member: skip_reason(root)?.is_none(),
    })
}

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
///
/// Only a *simple* range — one comparator operator followed by a full `x.y.z` version — is
/// rewritable. Everything else is returned untouched, because there is no way to move it to the
/// new version without either corrupting it or breaking the install:
///
/// - `workspace:` protocol ranges (resolved later, at publish time)
/// - tags and partial ranges with no full version to replace (`*`, `latest`, `1.x`, `^1`)
/// - specs that merely *contain* digits: tarball URLs, `file:` paths, `git+ssh://…#v1.2.3`,
///   `github:user/repo#tag`, `npm:` aliases. Splitting these at the first digit produced garbage
///   like `https://registry.npmjs.org/@scope/pkg/-/pkg-0.2.0` — a URL with the `.tgz` lopped off,
///   pointing at a version that is not published until *after* this run.
/// - compound ranges (`>=1.0.0 <2.0.0`, `1.x || 2.x`), where replacing one version silently drops
///   the rest of the constraint.
///
/// A pinned dep left alone here keeps resolving to the version it already names, so the lockfile
/// refresh still succeeds. Moving it is the author's call, once the new version is published.
fn reformat_range(old: &str, new_version: &str) -> String {
    let trimmed = old.trim();
    match split_comparator(trimmed) {
        Some((op, version)) if is_exact_version(version) => format!("{op}{new_version}"),
        _ => old.to_string(),
    }
}

/// Split a simple range into its leading comparator and the rest, longest operator first.
/// `None` for anything that is not a bare version or a single-comparator range.
fn split_comparator(range: &str) -> Option<(&str, &str)> {
    for op in [">=", "<=", "=", "^", "~", ">", "<", ""] {
        if let Some(rest) = range.strip_prefix(op) {
            return Some((op, rest));
        }
    }
    None
}

/// A complete `x.y.z` version, optionally with a `-prerelease` and/or `+build` suffix.
/// Partial versions (`1`, `1.2`, `1.x`) are rejected: there is nothing to rewrite in them.
fn is_exact_version(v: &str) -> bool {
    let core = v.split(['-', '+']).next().unwrap_or(v);
    let mut parts = core.split('.');
    let numeric = |p: Option<&str>| matches!(p, Some(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
    numeric(parts.next())
        && numeric(parts.next())
        && numeric(parts.next())
        && parts.next().is_none()
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
    fn discovers_the_root_package_of_a_single_package_repo() {
        // No `workspaces` field: the root manifest is the one package.
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path().join("package.json"),
            r#"{ "name": "mytool", "version": "0.1.0" }"#,
        );

        let pkgs = NpmAdapter::new(tmp.path()).discover_packages().unwrap();
        assert_eq!(pkgs.len(), 1, "got: {pkgs:?}");
        assert_eq!(pkgs[0].name, "mytool");
        assert!(pkgs[0].publishable);
        assert_eq!(pkgs[0].manifest_path, tmp.path().join("package.json"));
    }

    #[test]
    fn private_workspace_root_is_not_a_member() {
        // The usual monorepo root: a private container with no version. It must stay out of both
        // discovery and the skipped-manifest notes.
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

        let adapter = NpmAdapter::new(root);
        let names: Vec<_> = adapter
            .discover_packages()
            .unwrap()
            .iter()
            .map(|p| p.name.clone())
            .collect();
        assert_eq!(names, ["@x/a"]);
        assert_eq!(adapter.skipped_workspace_manifests().unwrap(), vec![]);
    }

    #[test]
    fn workspace_root_that_is_itself_a_package_is_a_member() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root.join("package.json"),
            r#"{ "name": "top", "version": "3.0.0", "workspaces": ["packages/*"] }"#,
        );
        write(
            root.join("packages/a/package.json"),
            r#"{ "name": "@x/a", "version": "1.0.0" }"#,
        );

        let pkgs = NpmAdapter::new(root).discover_packages().unwrap();
        let names: Vec<_> = pkgs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["@x/a", "top"]);
    }

    #[test]
    fn build_command_present_only_when_a_build_script_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let with = tmp.path().join("with/package.json");
        write(
            with.clone(),
            r#"{ "name": "@x/a", "version": "1.0.0", "scripts": { "build": "tsc" } }"#,
        );
        let without = tmp.path().join("without/package.json");
        write(
            without.clone(),
            r#"{ "name": "@x/b", "version": "1.0.0", "scripts": { "test": "vitest" } }"#,
        );

        let adapter = NpmAdapter::new(tmp.path());
        assert_eq!(
            adapter
                .build_command(&dummy_pkg("@x/a", with.to_str().unwrap()))
                .unwrap(),
            Some("npm run build".to_string())
        );
        assert_eq!(
            adapter
                .build_command(&dummy_pkg("@x/b", without.to_str().unwrap()))
                .unwrap(),
            None
        );
    }

    #[test]
    fn strip_publish_hooks_removes_pack_publish_lifecycle_and_keeps_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("package.json");
        write(
            path.clone(),
            "{\n  \"name\": \"@x/a\",\n  \"scripts\": {\n    \"build\": \"tsc\",\n    \"prepare\": \"npm run build\",\n    \"prepublishOnly\": \"npm run build\",\n    \"test\": \"vitest\"\n  }\n}\n",
        );

        let adapter = NpmAdapter::new(tmp.path());
        let removed = adapter
            .strip_publish_hooks(&dummy_pkg("@x/a", path.to_str().unwrap()))
            .unwrap();
        assert_eq!(removed, vec!["prepublishOnly", "prepare"]);

        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(
            after,
            "{\n  \"name\": \"@x/a\",\n  \"scripts\": {\n    \"build\": \"tsc\",\n    \"test\": \"vitest\"\n  }\n}\n"
        );

        // Idempotent: a second pass finds nothing to remove and leaves the file untouched.
        let removed_again = adapter
            .strip_publish_hooks(&dummy_pkg("@x/a", path.to_str().unwrap()))
            .unwrap();
        assert!(removed_again.is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), after);
    }

    #[test]
    fn skips_workspace_manifests_without_release_identity() {
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
            root.join("packages/fixture/package.json"),
            r#"{ "name": "@x/fixture", "private": true }"#,
        );
        write(
            root.join("packages/anonymous/package.json"),
            r#"{ "version": "0.0.0", "private": true }"#,
        );

        let adapter = NpmAdapter::new(root);
        let pkgs = adapter.discover_packages().unwrap();
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "@x/a");

        let skipped = adapter.skipped_workspace_manifests().unwrap();
        let skipped_paths: Vec<_> = skipped
            .iter()
            .map(|s| {
                s.path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(
            skipped_paths,
            [
                "packages/anonymous/package.json",
                "packages/fixture/package.json"
            ]
        );
        assert_eq!(skipped[0].reason, "missing \"name\"");
        assert_eq!(skipped[1].reason, "missing \"version\"");
    }

    #[test]
    fn malformed_workspace_manifest_is_still_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root.join("package.json"),
            r#"{ "name": "root", "private": true, "workspaces": ["packages/*"] }"#,
        );
        write(root.join("packages/broken/package.json"), "{ nope");

        let adapter = NpmAdapter::new(root);
        let err = adapter.discover_packages().unwrap_err().to_string();
        assert!(
            err.contains("parsing") && err.contains("packages/broken/package.json"),
            "got: {err}"
        );
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

    /// The failure this guards is one-way: npm has no un-publish for making a package private
    /// again. A manifest that asks to stay restricted must reach `npm publish` as `restricted`.
    #[test]
    fn publish_honours_publish_config_access_instead_of_forcing_public() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg_dir = tmp.path().join("packages/a");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let manifest = pkg_dir.join("package.json");
        std::fs::write(
            &manifest,
            r#"{"name":"@x/a","version":"1.2.3","publishConfig":{"access":"restricted"}}"#,
        )
        .unwrap();

        let fake = FakeRunner::new(true, "", "");
        let adapter = NpmAdapter::with_runner(tmp.path(), Box::new(fake.clone()));
        let pkg = dummy_pkg("@x/a", manifest.to_str().unwrap());

        adapter.publish(&pkg, None).unwrap();
        let calls = fake.calls.lock().unwrap();
        assert_eq!(
            calls[0].1,
            ["publish", "--access", "restricted", "--no-workspaces"],
            "a CLI --access overrides publishConfig, so it must carry the manifest's answer"
        );
    }

    /// A scoped package's first publish fails without an explicit `--access`, so "no opinion in
    /// the manifest" must still mean public — this is the path every existing repo is on.
    #[test]
    fn publish_defaults_to_public_when_the_manifest_says_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg_dir = tmp.path().join("packages/a");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let manifest = pkg_dir.join("package.json");
        std::fs::write(&manifest, r#"{"name":"@x/a","version":"1.2.3"}"#).unwrap();

        let fake = FakeRunner::new(true, "", "");
        let adapter = NpmAdapter::with_runner(tmp.path(), Box::new(fake.clone()));
        let pkg = dummy_pkg("@x/a", manifest.to_str().unwrap());

        adapter.publish(&pkg, None).unwrap();
        assert_eq!(
            fake.calls.lock().unwrap()[0].1,
            ["publish", "--access", "public", "--no-workspaces"]
        );
    }

    #[test]
    fn a_missing_root_manifest_discovers_nothing_rather_than_failing() {
        // A Cargo-rooted polyglot repo has no root package.json. Erroring here used to abort
        // `init`/`check`/`version`/`publish` outright — taking down the *other* enabled adapters
        // with it — the moment npm was added to `release.toml`.
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        let adapter = NpmAdapter::new(tmp.path());
        assert!(adapter.discover_packages().unwrap().is_empty());
        assert!(adapter.skipped_workspace_manifests().unwrap().is_empty());
    }

    #[test]
    fn declared_packages_are_found_without_any_root_manifest() {
        // The ES-Runtime shape: root is a virtual Cargo workspace, the JS packages are independent
        // projects elsewhere in the tree, and `[discovery] npm` is what names them.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        for (dir, json) in [
            ("packages/redis", r#"{"name":"@x/redis","version":"0.0.1"}"#),
            (
                "packages/postgres",
                r#"{"name":"@x/postgres","version":"0.0.1"}"#,
            ),
            ("types", r#"{"name":"@x/types","version":"0.1.0"}"#),
            (
                "website",
                r#"{"name":"website","version":"0.1.0","private":true}"#,
            ),
        ] {
            fs::create_dir_all(root.join(dir)).unwrap();
            fs::write(root.join(dir).join("package.json"), json).unwrap();
        }

        let adapter = NpmAdapter::new(root)
            .with_packages(vec!["packages/*".to_string(), "types".to_string()]);
        let names: Vec<String> = adapter
            .discover_packages()
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        // `website` is outside the declaration, so it is not a member at all — not even a skipped
        // one. Only what was declared is released.
        assert_eq!(names, ["@x/postgres", "@x/redis", "@x/types"]);
    }

    #[test]
    fn a_pnpm_workspace_declares_its_members() {
        // pnpm does not use package.json's `workspaces` field, so this repo used to look like a
        // single-package one whose private root was then skipped: zero packages, silently.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(
            root.join("package.json"),
            r#"{"name":"root","private":true}"#,
        )
        .unwrap();
        fs::write(
            root.join("pnpm-workspace.yaml"),
            "packages:\n  - 'packages/*'\n  - apps/web\n",
        )
        .unwrap();
        for dir in ["packages/a", "packages/b", "apps/web"] {
            fs::create_dir_all(root.join(dir)).unwrap();
            let name = dir.rsplit('/').next().unwrap();
            fs::write(
                root.join(dir).join("package.json"),
                format!(r#"{{"name":"{name}","version":"1.0.0"}}"#),
            )
            .unwrap();
        }

        let names: Vec<String> = NpmAdapter::new(root)
            .discover_packages()
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, ["a", "b", "web"]);
    }

    #[test]
    fn a_pnpm_workspace_without_packages_falls_through() {
        // A pnpm-workspace.yaml carrying only a `catalog:` declares no members. Reading that as
        // "the member list is empty" would blank discovery for a repo that is really a plain
        // single-package one.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(
            root.join("package.json"),
            r#"{"name":"solo","version":"1.0.0"}"#,
        )
        .unwrap();
        fs::write(
            root.join("pnpm-workspace.yaml"),
            "catalog:\n  react: ^19.0.0\n",
        )
        .unwrap();

        let names: Vec<String> = NpmAdapter::new(root)
            .discover_packages()
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, ["solo"]);
    }

    #[test]
    fn a_negated_pattern_excludes_a_member() {
        // `!` exclusions are accepted by npm, pnpm, and bun alike. Globbed as a literal path they
        // matched nothing and silently did nothing, so an excluded package still got released.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(
            root.join("package.json"),
            r#"{"name":"root","private":true}"#,
        )
        .unwrap();
        fs::write(
            root.join("pnpm-workspace.yaml"),
            "packages:\n  - 'packages/*'\n  - '!packages/private-app'\n",
        )
        .unwrap();
        for dir in ["packages/lib", "packages/private-app"] {
            fs::create_dir_all(root.join(dir)).unwrap();
            let name = dir.rsplit('/').next().unwrap();
            fs::write(
                root.join(dir).join("package.json"),
                format!(r#"{{"name":"{name}","version":"1.0.0"}}"#),
            )
            .unwrap();
        }

        let names: Vec<String> = NpmAdapter::new(root)
            .discover_packages()
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, ["lib"]);
    }

    #[test]
    fn a_declaration_overrides_the_root_workspaces_field() {
        // Both present: the declaration is the deliberate, tool-local answer and wins, so a repo
        // can release a subset of what its package manager treats as members.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(
            root.join("package.json"),
            r#"{"name":"root","private":true,"workspaces":["apps/*"]}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("apps/a")).unwrap();
        fs::write(
            root.join("apps/a/package.json"),
            r#"{"name":"a","version":"1.0.0"}"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("libs/b")).unwrap();
        fs::write(
            root.join("libs/b/package.json"),
            r#"{"name":"b","version":"2.0.0"}"#,
        )
        .unwrap();

        let adapter = NpmAdapter::new(root).with_packages(vec!["libs/*".to_string()]);
        let names: Vec<String> = adapter
            .discover_packages()
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, ["b"]);
    }

    #[test]
    fn update_lockfile_defaults_to_npm_without_a_known_lockfile() {
        let tmp = tempfile::tempdir().unwrap();
        // A root package.json with no lockfile beside it yet — npm is the right default.
        fs::write(tmp.path().join("package.json"), "{}").unwrap();
        let fake = FakeRunner::new(true, "", "");
        let adapter = NpmAdapter::with_runner(tmp.path(), Box::new(fake.clone()));

        adapter.update_lockfile(tmp.path()).unwrap();

        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls[0].0, "npm");
        assert_eq!(calls[0].1, ["install", "--package-lock-only"]);
        assert_eq!(calls[0].2, tmp.path());
    }

    #[test]
    fn update_lockfile_is_a_noop_when_the_root_has_nothing_to_install() {
        // The explicitly-declared layout: the root belongs to another ecosystem, and each package
        // is its own install with its own lockfile beside it. `npm install` here would fail, and
        // there is no root lockfile that could have gone stale.
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        let fake = FakeRunner::new(true, "", "");
        let adapter = NpmAdapter::with_runner(tmp.path(), Box::new(fake.clone()));

        adapter.update_lockfile(tmp.path()).unwrap();

        assert!(fake.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn update_lockfile_uses_bun_when_a_bun_lockfile_exists() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("bun.lock"), "").unwrap();
        let fake = FakeRunner::new(true, "", "");
        let adapter = NpmAdapter::with_runner(tmp.path(), Box::new(fake.clone()));

        adapter.update_lockfile(tmp.path()).unwrap();

        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls[0].0, "bun");
        assert_eq!(calls[0].1, ["install", "--lockfile-only"]);
        assert_eq!(calls[0].2, tmp.path());
    }

    #[test]
    fn prerelease_publishes_under_its_dist_tag() {
        let fake = FakeRunner::new(true, "", "");
        let adapter = NpmAdapter::with_runner("/repo", Box::new(fake.clone()));
        let mut pkg = dummy_pkg("@x/a", "/repo/packages/a/package.json");
        pkg.version = "1.2.3-dev.abc1234".to_string();

        adapter.publish(&pkg, None).unwrap();
        let calls = fake.calls.lock().unwrap();
        assert_eq!(
            calls[0].1,
            [
                "publish",
                "--access",
                "public",
                "--no-workspaces",
                "--tag",
                "dev"
            ]
        );
    }

    #[test]
    fn dist_tag_only_for_prereleases() {
        assert_eq!(dist_tag("1.2.3"), None);
        assert_eq!(dist_tag("1.2.3-dev.abc"), Some("dev".to_string()));
        assert_eq!(dist_tag("2.0.0-beta.1"), Some("beta".to_string()));
        assert_eq!(dist_tag("2.0.0-rc"), Some("rc".to_string()));
    }

    #[test]
    fn reformat_range_preserves_operator() {
        assert_eq!(reformat_range("^1.0.0", "2.0.0"), "^2.0.0");
        assert_eq!(reformat_range("~1.0.0", "2.0.0"), "~2.0.0");
        assert_eq!(reformat_range("1.0.0", "2.0.0"), "2.0.0");
        assert_eq!(reformat_range(">=1.0.0", "2.0.0"), ">=2.0.0");
        assert_eq!(reformat_range("*", "2.0.0"), "*");
        assert_eq!(reformat_range("workspace:^", "2.0.0"), "workspace:^");
        assert_eq!(reformat_range("<=1.0.0", "2.0.0"), "<=2.0.0");
        assert_eq!(reformat_range("^1.0.0-rc.1", "2.0.0-rc.2"), "^2.0.0-rc.2");
    }

    /// A dep pinned to something that is not a plain semver range must survive untouched. These
    /// all contain digits, and the old first-digit split turned them into garbage — a mangled
    /// tarball URL that no registry can serve, which then failed the lockfile refresh.
    #[test]
    fn reformat_range_leaves_non_semver_specs_alone() {
        for spec in [
            "https://registry.npmjs.org/@scope/pkg/-/pkg-0.14.1.tgz",
            "file:../packages/pkg",
            "git+ssh://git@github.com/o/r.git#v1.2.3",
            "github:owner/repo#v1.2.3",
            "npm:@scope/other@^1.2.3",
            "latest",
            "1.x",
            "^1",
            ">=1.0.0 <2.0.0",
            "1.x || 2.x",
            ">= 1.0.0",
        ] {
            assert_eq!(
                reformat_range(spec, "0.2.0"),
                spec,
                "must not rewrite {spec}"
            );
        }
    }

    #[test]
    fn resolve_workspace_range_mapping() {
        assert_eq!(resolve_workspace_range("workspace:*", "1.2.3"), "1.2.3");
        assert_eq!(resolve_workspace_range("workspace:^", "1.2.3"), "^1.2.3");
        assert_eq!(resolve_workspace_range("workspace:~", "1.2.3"), "~1.2.3");
        assert_eq!(resolve_workspace_range("workspace:1.0.0", "1.2.3"), "1.0.0");
    }
}
