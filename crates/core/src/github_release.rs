//! The `github-release` command — non-interactive, run in **CI** for `build-only` packages.
//!
//! This is the build-only twin of [`publish`](crate::publish): where `publish` pushes a package to
//! a registry, `github-release` attaches a package's cross-compiled binaries to a GitHub Release.
//! It exists so the generated `release.yml` never embeds a wall of inline bash (version reads,
//! changelog extraction, asset renaming, `gh release create`) — the workflow just calls
//! `otf-release github-release --package <pkg> --artifacts-dir .artifacts`, exactly as the registry
//! path calls `otf-release publish`. The tool owns the logic; the YAML stays a thin, stable call.
//!
//! What it does for each selected build-only package:
//!   1. reads the package's version from its manifest (via the adapter — the *same* read
//!      `check`/`publish` use, never a `cargo metadata | jq '.packages[0]'` guess),
//!   2. computes the tag from `tag_format`,
//!   3. skips idempotently if that release already exists (forward-resumable),
//!   4. builds the release body from `github_release_notes` (curated changelog / commit list /
//!      GitHub-generated),
//!   5. turns the staged `bin/<stage_as>/<bin>` tree into assets — raw OS/arch-named binaries
//!      (`<bin>-<os>-<arch>[.ext]`), or, when the package configures `archive`, `.tar.gz`/`.zip`
//!      bundles (with any `include` files) plus an optional `checksums.txt`, and
//!   6. creates the Release on `main` with those assets attached.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::adapter::{apply_changelog_scope, Adapter, Pkg};
use crate::changelog;
use crate::config::{format_tag, ChangelogScope, GithubReleaseNotes, PackageEntry, ReleaseConfig};
use crate::forge::{Forge, GhForge, ReleaseNotes};
use crate::git::{GitRepo, RepoState};

/// Options for a `github-release` run.
#[derive(Debug, Clone, Default)]
pub struct GithubReleaseOptions {
    /// Restrict the run to one build-only package. Required when the repo has more than one; the
    /// generated workflow always passes it.
    pub package: Option<String>,
    /// Root of the staged-artifact tree (`.artifacts/`) the build jobs uploaded. `None` (or a
    /// package with no build) creates a Release with no attached assets.
    pub artifacts_dir: Option<PathBuf>,
    /// Resolve the plan and print it, but create nothing.
    pub dry_run: bool,
}

/// Wire up the real git/forge and run the flow across every enabled adapter.
pub fn run_many(
    adapters: &[&dyn Adapter],
    root: &Path,
    opts: &GithubReleaseOptions,
    config: &ReleaseConfig,
) -> Result<()> {
    let repo = GitRepo::new(root);
    let forge = GhForge::new(root);
    orchestrate(adapters, &repo, &forge, root, opts, config)
}

/// The testable core: pick the build-only package(s), read each one's version, and create its
/// GitHub Release idempotently. Behind the `RepoState`/`Forge` traits so it runs without `git`/`gh`.
pub fn orchestrate(
    adapters: &[&dyn Adapter],
    history: &dyn RepoState,
    forge: &dyn Forge,
    root: &Path,
    opts: &GithubReleaseOptions,
    config: &ReleaseConfig,
) -> Result<()> {
    // Every configured build-only package. An npm matrix package is *not* build-only (its binaries
    // ship inside the tarball), so `is_build_only` already excludes it.
    let build_only: Vec<&PackageEntry> = config
        .packages
        .iter()
        .filter(|p| p.is_build_only())
        .collect();

    let selected: Vec<&PackageEntry> = match &opts.package {
        Some(name) => {
            let entry = build_only
                .iter()
                .find(|p| &p.name == name)
                .copied()
                .with_context(|| format!("no build-only package named `{name}` in release.toml"))?;
            vec![entry]
        }
        None => {
            if build_only.len() > 1 {
                bail!(
                    "more than one build-only package; pass --package to choose one of: {}",
                    build_only
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            build_only
        }
    };

    if selected.is_empty() {
        println!("No build-only packages to release.");
        return Ok(());
    }

    // Discover every package once, then look each selected entry up by name so its version comes
    // from the adapter that owns its manifest — the read that can never drift from `publish`.
    let mut discovered = Vec::new();
    for adapter in adapters {
        discovered.append(&mut adapter.discover_packages()?);
    }
    apply_changelog_scope(root, &config.changelog_scope, &mut discovered);

    for entry in selected {
        let pkg = discovered
            .iter()
            .find(|p| p.name == entry.name)
            .with_context(|| {
                format!(
                    "build-only package `{}` not found by any enabled adapter",
                    entry.name
                )
            })?;

        let tag = format_tag(&config.tag_format, &pkg.name, &pkg.version)?;

        let notes = release_notes(
            &config.github_release_notes,
            &config.changelog_scope,
            pkg,
            config,
            history,
            root,
        )?;

        let assets = match &opts.artifacts_dir {
            Some(dir) => stage_assets(dir, entry, root)?,
            None => Vec::new(),
        };

        // A dry run previews the plan and never touches the forge, so it works without `gh`.
        if opts.dry_run {
            println!("Would create release {tag}:");
            match &notes {
                ReleaseNotes::Body(_) => println!("  notes: curated"),
                ReleaseNotes::Generate => println!("  notes: GitHub-generated"),
            }
            for asset in &assets {
                println!("  asset: {}", asset.display());
            }
            continue;
        }

        // Idempotent: a re-run after a partial failure skips a release that already shipped.
        if forge.release_exists(&tag)? {
            println!("Release {tag} already exists; nothing to do.");
            continue;
        }

        forge.create_release_with_assets(&tag, &tag, &notes, Some("main"), &assets)?;
        println!("Released {tag} ({} asset(s))", assets.len());
    }

    Ok(())
}

/// Build the release body per the configured source. A curated/semantic source that turns up empty
/// falls back to GitHub-generated notes rather than shipping an empty release body.
fn release_notes(
    source: &GithubReleaseNotes,
    scope: &ChangelogScope,
    pkg: &Pkg,
    config: &ReleaseConfig,
    history: &dyn RepoState,
    root: &Path,
) -> Result<ReleaseNotes> {
    match source {
        GithubReleaseNotes::AutoGenerate => Ok(ReleaseNotes::Generate),
        GithubReleaseNotes::CuratedChangelog => {
            // In root scope `apply_changelog_scope` already pointed every package at the root
            // CHANGELOG.md; in package scope it is the package's own file. Either way the notes are
            // this package's own dated section — no cross-package aggregation.
            let _ = scope;
            match changelog::dated_section_notes(&pkg.changelog_path, &pkg.version)? {
                Some(body) if !body.trim().is_empty() => Ok(ReleaseNotes::Body(body)),
                _ => Ok(ReleaseNotes::Generate),
            }
        }
        GithubReleaseNotes::SemanticCommits => {
            // Commits since the package's previous matching tag (the current tag doesn't exist yet),
            // scoped to the whole repo to mirror the previous inline behavior.
            let previous = history.last_tag(&pkg.name, &config.history_tag_formats())?;
            let commits = history.commits_since(previous.as_deref(), root)?;
            if commits.trim().is_empty() {
                Ok(ReleaseNotes::Generate)
            } else {
                Ok(ReleaseNotes::Body(commits))
            }
        }
    }
}

/// Turn the staged `bin/<stage_as>/<bin>[.ext]` tree the build matrix uploaded into the release
/// assets, returning their paths. Each staged binary becomes either a raw, OS/arch-renamed file
/// (`<bin>-<os>-<arch>[.ext]`) or — when the package configures `archive` — a `.tar.gz`/`.zip` that
/// bundles the binary with any `include` files. When `checksums` is set, a combined `checksums.txt`
/// (SHA-256 of every asset) is added last. `darwin`→`macos`, `win32`→`windows`, `x64`→`x86-64`.
fn stage_assets(artifacts_dir: &Path, entry: &PackageEntry, root: &Path) -> Result<Vec<PathBuf>> {
    if !artifacts_dir.exists() {
        return Ok(Vec::new());
    }
    let slug = slug(&entry.name);
    let bin = entry.bin_name.clone().unwrap_or_else(|| slug.clone());

    let flat = artifacts_dir.join(format!(".flat-{slug}"));
    if flat.exists() {
        fs::remove_dir_all(&flat).with_context(|| format!("clearing {}", flat.display()))?;
    }
    fs::create_dir_all(&flat).with_context(|| format!("creating {}", flat.display()))?;

    // Only this package's uploaded artifacts: each is a directory named `<slug>-<name>-<arch>`.
    let mut source_dirs = Vec::new();
    for dir_entry in fs::read_dir(artifacts_dir)
        .with_context(|| format!("reading {}", artifacts_dir.display()))?
    {
        let path = dir_entry?.path();
        if !path.is_dir() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name == slug || name.starts_with(&format!("{slug}-")) {
            source_dirs.push(path);
        }
    }

    // Every staged binary, paired with its `<stage_as>` directory.
    let mut staged = Vec::new();
    for dir in source_dirs {
        collect_binaries(&dir, &mut staged)?;
    }

    // Extra files that go *inside* each archive (only meaningful when archiving).
    let includes = if entry.archive.is_some() {
        resolve_includes(root, &entry.include)?
    } else {
        Vec::new()
    };

    let mut assets = Vec::new();
    for (stage, src) in &staged {
        let file_name = src.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let (os, arch) = map_os_arch(stage);
        match entry.archive {
            Some(format) => {
                let stem = asset_stem(&bin, &os, &arch);
                let ext = format.extension_for(&os);
                let dest = flat.join(format!("{stem}.{ext}"));
                // The binary keeps its own name inside the archive; includes keep their repo path.
                let mut members = vec![(file_name.to_string(), src.clone())];
                members.extend(includes.iter().cloned());
                write_archive(ext, &dest, &members)?;
                assets.push(dest);
            }
            None => {
                let dest = flat.join(asset_file_name(&bin, stage, file_name));
                fs::copy(src, &dest)
                    .with_context(|| format!("copying {} -> {}", src.display(), dest.display()))?;
                assets.push(dest);
            }
        }
    }

    assets.sort();

    if entry.checksums && !assets.is_empty() {
        let checksums = flat.join("checksums.txt");
        write_checksums(&assets, &checksums)?;
        assets.push(checksums);
    }

    Ok(assets)
}

/// Recursively collect every file under `dir`, paired with its immediate `<stage_as>` parent
/// directory name (the Node `process.platform-process.arch` dir `otf-release build` staged into,
/// e.g. `linux-x64`, `darwin-arm64`, `win32-x64`).
fn collect_binaries(dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_binaries(&path, out)?;
            continue;
        }
        let stage = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        out.push((stage, path));
    }
    Ok(())
}

/// Resolve `include` globs (repo-relative) into `(archive_entry_name, source_path)` pairs, where the
/// entry name is the file's path relative to the repo root so bundled files keep their layout
/// (`types/x.d.ts` stays under `types/`). Missing patterns are simply skipped.
fn resolve_includes(root: &Path, patterns: &[String]) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for pattern in patterns {
        let joined = root.join(pattern);
        let full = joined
            .to_str()
            .with_context(|| format!("non-UTF-8 include pattern: {}", joined.display()))?;
        for entry in glob::glob(full).with_context(|| format!("bad include glob: {pattern}"))? {
            let path = match entry {
                Ok(p) => p,
                Err(_) => continue,
            };
            if !path.is_file() {
                continue;
            }
            let name = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if seen.insert(name.clone()) {
                out.push((name, path));
            }
        }
    }
    Ok(out)
}

/// `linux`/`x86-64` → `esrun-linux-x86-64`; a bare OS (no arch) → `esrun-<os>`.
fn asset_stem(bin: &str, os: &str, arch: &str) -> String {
    if arch.is_empty() {
        format!("{bin}-{os}")
    } else {
        format!("{bin}-{os}-{arch}")
    }
}

/// `linux-x64` + `esrun` → `esrun-linux-x86-64`; `win32-x64` + `esrun.exe` → `esrun-windows-x86-64.exe`.
fn asset_file_name(bin: &str, stage: &str, file_name: &str) -> String {
    let (os, arch) = map_os_arch(stage);
    let base = asset_stem(bin, &os, &arch);
    // Preserve a file extension (`.exe`) if the staged binary had one; a bare name keeps a bare name.
    match file_name.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => format!("{base}.{ext}"),
        _ => base,
    }
}

/// Map a `<stage_as>` dir to friendly `(os, arch)`: `darwin`→`macos`, `win32`→`windows`,
/// `x64`→`x86-64`; everything else passes through (`arm64`, `aarch64`, …).
fn map_os_arch(stage: &str) -> (String, String) {
    let (os_raw, arch_raw) = match stage.rsplit_once('-') {
        Some((os, arch)) => (os, arch),
        None => (stage, ""),
    };
    let os = match os_raw {
        "darwin" => "macos",
        "win32" => "windows",
        other => other,
    };
    let arch = match arch_raw {
        "x64" => "x86-64",
        other => other,
    };
    (os.to_string(), arch.to_string())
}

/// Write `members` (each `(name_inside_archive, source_file)`) into `dest` as a `.tar.gz` or `.zip`
/// (chosen by `ext`). Executable bits are preserved from the source files.
fn write_archive(ext: &str, dest: &Path, members: &[(String, PathBuf)]) -> Result<()> {
    let file =
        fs::File::create(dest).with_context(|| format!("creating archive {}", dest.display()))?;
    if ext == "zip" {
        use zip::write::SimpleFileOptions;
        let mut zip = zip::ZipWriter::new(file);
        for (name, src) in members {
            let mut options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            if let Some(mode) = unix_mode(src) {
                options = options.unix_permissions(mode);
            }
            zip.start_file(name, options)
                .with_context(|| format!("zip entry {name}"))?;
            let mut reader =
                fs::File::open(src).with_context(|| format!("reading {}", src.display()))?;
            std::io::copy(&mut reader, &mut zip)
                .with_context(|| format!("writing {name} into {}", dest.display()))?;
        }
        zip.finish().context("finalizing zip")?;
    } else {
        let gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(gz);
        for (name, src) in members {
            tar.append_path_with_name(src, name)
                .with_context(|| format!("tar entry {name}"))?;
        }
        tar.into_inner()
            .context("finalizing tar")?
            .finish()
            .context("finalizing gzip")?;
    }
    Ok(())
}

/// The Unix permission bits of `path`, or `None` on non-Unix hosts.
fn unix_mode(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).ok().map(|m| m.permissions().mode())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Write a `sha256sum`-style `checksums.txt` (`<hex>  <filename>`), one line per asset.
fn write_checksums(assets: &[PathBuf], dest: &Path) -> Result<()> {
    use sha2::{Digest, Sha256};
    let mut body = String::new();
    for asset in assets {
        let bytes = fs::read(asset).with_context(|| format!("reading {}", asset.display()))?;
        let digest = Sha256::digest(&bytes);
        let name = asset
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        body.push_str(&format!("{digest:x}  {name}\n"));
    }
    fs::write(dest, body).with_context(|| format!("writing {}", dest.display()))?;
    Ok(())
}

/// Lowercase a package name into a job/artifact-safe slug (`@x/cli` → `x-cli`), matching the slug
/// `init` uses to name the uploaded artifacts.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;

    use crate::adapter::{Bump, DepKind};
    use crate::config::{ArchiveFormat, Ecosystem, Mode, PackageEntry};
    use crate::forge::ReleaseNotes;

    /// One recorded `create_release_with_assets` call.
    struct CreatedRelease {
        tag: String,
        notes: ReleaseNotes,
        target: Option<String>,
        assets: Vec<PathBuf>,
    }

    struct FakeForge {
        existing: Vec<String>,
        created: RefCell<Vec<CreatedRelease>>,
    }

    impl FakeForge {
        fn new() -> Self {
            Self {
                existing: Vec::new(),
                created: RefCell::new(Vec::new()),
            }
        }

        fn with_existing(tag: &str) -> Self {
            Self {
                existing: vec![tag.to_string()],
                created: RefCell::new(Vec::new()),
            }
        }
    }

    impl Forge for FakeForge {
        fn open_pr(&self, _: &str, _: &str, _: &str) -> Result<()> {
            unreachable!("github-release never opens a PR")
        }
        fn create_release(&self, tag: &str, _: &str, notes: &str) -> Result<()> {
            self.created.borrow_mut().push(CreatedRelease {
                tag: tag.to_string(),
                notes: ReleaseNotes::Body(notes.to_string()),
                target: None,
                assets: Vec::new(),
            });
            Ok(())
        }
        fn release_exists(&self, tag: &str) -> Result<bool> {
            Ok(self.existing.iter().any(|t| t == tag))
        }
        fn create_release_with_assets(
            &self,
            tag: &str,
            _: &str,
            notes: &ReleaseNotes,
            target: Option<&str>,
            assets: &[PathBuf],
        ) -> Result<()> {
            self.created.borrow_mut().push(CreatedRelease {
                tag: tag.to_string(),
                notes: notes.clone(),
                target: target.map(str::to_string),
                assets: assets.to_vec(),
            });
            Ok(())
        }
    }

    struct FakeHistory {
        last: Option<String>,
        commits: String,
    }

    impl RepoState for FakeHistory {
        fn last_tag(&self, _: &str, _: &[String]) -> Result<Option<String>> {
            Ok(self.last.clone())
        }
        fn commit_count_since(&self, _: &str, _: &Path) -> Result<usize> {
            Ok(0)
        }
        fn changed_files_since(&self, _: &str, _: &Path) -> Result<Vec<PathBuf>> {
            Ok(Vec::new())
        }
        fn commits_since(&self, _: Option<&str>, _: &Path) -> Result<String> {
            Ok(self.commits.clone())
        }
    }

    struct FakeAdapter {
        packages: Vec<Pkg>,
    }

    impl Adapter for FakeAdapter {
        fn discover_packages(&self) -> Result<Vec<Pkg>> {
            Ok(self.packages.clone())
        }
        fn write_version(&self, _: &Pkg, _: &str) -> Result<()> {
            Ok(())
        }
        fn update_dep_range(&self, _: &Pkg, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn format_range(&self, v: &str) -> String {
            v.to_string()
        }
        fn resolve_workspace_links(&self, _: &Pkg) -> Result<()> {
            Ok(())
        }
        fn update_lockfile(&self, _: &Path) -> Result<()> {
            Ok(())
        }
        fn dependent_bump(&self, _: Bump, _: &DepKind) -> Bump {
            Bump::Patch
        }
        fn is_published(&self, _: &Pkg, _: &str) -> Result<bool> {
            Ok(false)
        }
        fn publish(&self, _: &Pkg, _: Option<&Path>) -> Result<()> {
            Ok(())
        }
    }

    fn pkg(name: &str, version: &str, changelog: PathBuf) -> Pkg {
        Pkg {
            name: name.to_string(),
            version: version.to_string(),
            manifest_path: PathBuf::from("Cargo.toml"),
            changelog_path: changelog,
            publishable: true,
            internal_deps: Vec::new(),
        }
    }

    fn build_only_entry(name: &str, bin: &str) -> PackageEntry {
        PackageEntry {
            name: name.to_string(),
            adapter: Ecosystem::Generic,
            mode: Mode::BuildOnly,
            matrix: true,
            targets: Vec::new(),
            command: "cargo build".to_string(),
            artifacts: String::new(),
            bin_name: Some(bin.to_string()),
            compress: None,
            manifest: Some("Cargo.toml".to_string()),
            version_field: Some("version".to_string()),
            publish: None,
            archive: None,
            checksums: false,
            include: Vec::new(),
        }
    }

    fn config_with(entry: PackageEntry, notes: GithubReleaseNotes) -> ReleaseConfig {
        ReleaseConfig {
            adapters: vec![Ecosystem::Generic],
            packages: vec![entry],
            tag_format: "v{version}".to_string(),
            github_release_notes: notes,
            ..ReleaseConfig::default()
        }
    }

    #[test]
    fn creates_release_on_main_with_curated_notes() {
        let tmp = tempfile::tempdir().unwrap();
        let changelog = tmp.path().join("CHANGELOG.md");
        std::fs::write(
            &changelog,
            "# Changelog\n\n## [1.2.3] - 2026-01-01\n\n- Added a thing\n",
        )
        .unwrap();
        let adapter = FakeAdapter {
            packages: vec![pkg("esrun", "1.2.3", changelog)],
        };
        let history = FakeHistory {
            last: None,
            commits: String::new(),
        };
        let forge = FakeForge::new();
        let config = config_with(
            build_only_entry("esrun", "esrun"),
            GithubReleaseNotes::CuratedChangelog,
        );

        orchestrate(
            &[&adapter],
            &history,
            &forge,
            tmp.path(),
            &GithubReleaseOptions::default(),
            &config,
        )
        .unwrap();

        let created = forge.created.borrow();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].tag, "v1.2.3");
        assert_eq!(created[0].target.as_deref(), Some("main"));
        match &created[0].notes {
            ReleaseNotes::Body(body) => assert!(body.contains("Added a thing")),
            other => panic!("expected curated body, got {other:?}"),
        }
    }

    #[test]
    fn missing_changelog_section_falls_back_to_generated_notes() {
        let tmp = tempfile::tempdir().unwrap();
        let changelog = tmp.path().join("CHANGELOG.md");
        std::fs::write(
            &changelog,
            "# Changelog\n\n## [9.9.9] - 2026-01-01\n\n- old\n",
        )
        .unwrap();
        let adapter = FakeAdapter {
            packages: vec![pkg("esrun", "1.2.3", changelog)],
        };
        let forge = FakeForge::new();
        let config = config_with(
            build_only_entry("esrun", "esrun"),
            GithubReleaseNotes::CuratedChangelog,
        );

        orchestrate(
            &[&adapter],
            &FakeHistory {
                last: None,
                commits: String::new(),
            },
            &forge,
            tmp.path(),
            &GithubReleaseOptions::default(),
            &config,
        )
        .unwrap();

        assert_eq!(forge.created.borrow()[0].notes, ReleaseNotes::Generate);
    }

    #[test]
    fn semantic_commits_use_the_commit_log() {
        let tmp = tempfile::tempdir().unwrap();
        let adapter = FakeAdapter {
            packages: vec![pkg("esrun", "1.2.3", tmp.path().join("CHANGELOG.md"))],
        };
        let forge = FakeForge::new();
        let config = config_with(
            build_only_entry("esrun", "esrun"),
            GithubReleaseNotes::SemanticCommits,
        );

        orchestrate(
            &[&adapter],
            &FakeHistory {
                last: Some("v1.2.2".to_string()),
                commits: "* fix a bug\n* add a feature".to_string(),
            },
            &forge,
            tmp.path(),
            &GithubReleaseOptions::default(),
            &config,
        )
        .unwrap();

        let created = forge.created.borrow();
        match &created[0].notes {
            ReleaseNotes::Body(body) => assert!(body.contains("fix a bug")),
            other => panic!("expected commit body, got {other:?}"),
        }
    }

    #[test]
    fn existing_release_is_skipped_idempotently() {
        let tmp = tempfile::tempdir().unwrap();
        let adapter = FakeAdapter {
            packages: vec![pkg("esrun", "1.2.3", tmp.path().join("CHANGELOG.md"))],
        };
        let forge = FakeForge::with_existing("v1.2.3");
        let config = config_with(
            build_only_entry("esrun", "esrun"),
            GithubReleaseNotes::AutoGenerate,
        );

        orchestrate(
            &[&adapter],
            &FakeHistory {
                last: None,
                commits: String::new(),
            },
            &forge,
            tmp.path(),
            &GithubReleaseOptions::default(),
            &config,
        )
        .unwrap();

        assert!(forge.created.borrow().is_empty());
    }

    #[test]
    fn stages_and_attaches_renamed_binaries() {
        let tmp = tempfile::tempdir().unwrap();
        let artifacts = tmp.path().join(".artifacts");
        // Two staged artifacts named like the matrix upload: `<slug>-<name>-<arch>/bin/<stage_as>/<bin>`.
        for (dir, stage, file) in [
            ("esrun-linux-x86_64", "linux-x64", "esrun"),
            ("esrun-windows-x86_64", "win32-x64", "esrun.exe"),
        ] {
            let staged = artifacts.join(dir).join("bin").join(stage);
            std::fs::create_dir_all(&staged).unwrap();
            std::fs::write(staged.join(file), b"binary").unwrap();
        }

        let adapter = FakeAdapter {
            packages: vec![pkg("esrun", "1.2.3", tmp.path().join("CHANGELOG.md"))],
        };
        let forge = FakeForge::new();
        let config = config_with(
            build_only_entry("esrun", "esrun"),
            GithubReleaseNotes::AutoGenerate,
        );

        orchestrate(
            &[&adapter],
            &FakeHistory {
                last: None,
                commits: String::new(),
            },
            &forge,
            tmp.path(),
            &GithubReleaseOptions {
                package: Some("esrun".to_string()),
                artifacts_dir: Some(artifacts),
                dry_run: false,
            },
            &config,
        )
        .unwrap();

        let created = forge.created.borrow();
        let names: Vec<String> = created[0]
            .assets
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.contains(&"esrun-linux-x86-64".to_string()),
            "{names:?}"
        );
        assert!(
            names.contains(&"esrun-windows-x86-64.exe".to_string()),
            "{names:?}"
        );
    }

    #[test]
    fn archives_and_checksums_are_produced() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("README.md"), b"readme").unwrap();
        let artifacts = tmp.path().join(".artifacts");
        for (dir, stage, file) in [
            ("esrun-linux-x86_64", "linux-x64", "esrun"),
            ("esrun-windows-x86_64", "win32-x64", "esrun.exe"),
        ] {
            let staged = artifacts.join(dir).join("bin").join(stage);
            std::fs::create_dir_all(&staged).unwrap();
            std::fs::write(staged.join(file), b"binary").unwrap();
        }

        let mut entry = build_only_entry("esrun", "esrun");
        entry.archive = Some(ArchiveFormat::Auto); // tar.gz on Unix, zip on Windows
        entry.checksums = true;
        entry.include = vec!["README.md".to_string()];

        let adapter = FakeAdapter {
            packages: vec![pkg("esrun", "1.2.3", tmp.path().join("CHANGELOG.md"))],
        };
        let forge = FakeForge::new();
        let config = config_with(entry, GithubReleaseNotes::AutoGenerate);

        orchestrate(
            &[&adapter],
            &FakeHistory {
                last: None,
                commits: String::new(),
            },
            &forge,
            tmp.path(),
            &GithubReleaseOptions {
                package: Some("esrun".to_string()),
                artifacts_dir: Some(artifacts),
                dry_run: false,
            },
            &config,
        )
        .unwrap();

        let created = forge.created.borrow();
        let names: Vec<String> = created[0]
            .assets
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.contains(&"esrun-linux-x86-64.tar.gz".to_string()),
            "{names:?}"
        );
        assert!(
            names.contains(&"esrun-windows-x86-64.zip".to_string()),
            "{names:?}"
        );
        assert!(names.contains(&"checksums.txt".to_string()), "{names:?}");

        // checksums.txt lists the archives but never itself.
        let checksums = created[0]
            .assets
            .iter()
            .find(|p| p.file_name().unwrap() == "checksums.txt")
            .unwrap();
        let text = std::fs::read_to_string(checksums).unwrap();
        assert!(text.contains("esrun-linux-x86-64.tar.gz"), "{text}");
        assert!(text.contains("esrun-windows-x86-64.zip"), "{text}");
        assert!(!text.contains("checksums.txt"), "{text}");

        // The tar.gz really bundles the binary and the included README at their expected names.
        let tgz = created[0]
            .assets
            .iter()
            .find(|p| p.to_string_lossy().ends_with(".tar.gz"))
            .unwrap();
        let gz = flate2::read::GzDecoder::new(std::fs::File::open(tgz).unwrap());
        let mut archive = tar::Archive::new(gz);
        let entries: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(entries.contains(&"esrun".to_string()), "{entries:?}");
        assert!(entries.contains(&"README.md".to_string()), "{entries:?}");
    }

    #[test]
    fn dry_run_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let adapter = FakeAdapter {
            packages: vec![pkg("esrun", "1.2.3", tmp.path().join("CHANGELOG.md"))],
        };
        let forge = FakeForge::new();
        let config = config_with(
            build_only_entry("esrun", "esrun"),
            GithubReleaseNotes::AutoGenerate,
        );

        orchestrate(
            &[&adapter],
            &FakeHistory {
                last: None,
                commits: String::new(),
            },
            &forge,
            tmp.path(),
            &GithubReleaseOptions {
                package: None,
                artifacts_dir: None,
                dry_run: true,
            },
            &config,
        )
        .unwrap();

        assert!(forge.created.borrow().is_empty());
    }

    #[test]
    fn unknown_package_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let adapter = FakeAdapter {
            packages: Vec::new(),
        };
        let forge = FakeForge::new();
        let config = config_with(
            build_only_entry("esrun", "esrun"),
            GithubReleaseNotes::AutoGenerate,
        );

        let err = orchestrate(
            &[&adapter],
            &FakeHistory {
                last: None,
                commits: String::new(),
            },
            &forge,
            tmp.path(),
            &GithubReleaseOptions {
                package: Some("nope".to_string()),
                artifacts_dir: None,
                dry_run: false,
            },
            &config,
        )
        .unwrap_err();
        assert!(err.to_string().contains("nope"));
    }

    #[test]
    fn asset_names_map_os_and_arch() {
        assert_eq!(
            asset_file_name("esrun", "linux-x64", "esrun"),
            "esrun-linux-x86-64"
        );
        assert_eq!(
            asset_file_name("esrun", "win32-x64", "esrun.exe"),
            "esrun-windows-x86-64.exe"
        );
        assert_eq!(
            asset_file_name("esrun", "darwin-arm64", "esrun"),
            "esrun-macos-arm64"
        );
        assert_eq!(
            asset_file_name("esrun", "linux-arm64", "esrun"),
            "esrun-linux-arm64"
        );
    }

    #[test]
    fn slug_matches_init() {
        assert_eq!(slug("@x/cli"), "x-cli");
        assert_eq!(slug("opentf-release"), "opentf-release");
    }
}
