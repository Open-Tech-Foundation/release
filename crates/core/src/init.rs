//! The `init` command — interactive setup. Writes `release.toml` (the source of truth) and a
//! `.github/workflows/release.yml` generated from it.
//!
//! `init` takes no ecosystem flag. It asks which adapters to enable (`npm`, `crates.io`), then,
//! for each package that needs a build step, its **mode** (`publish` to a registry, or
//! `build-only` → artifacts attached to a GitHub Release), build matrix, command, and artifacts.
//! All of that is persisted to [`config::ReleaseConfig`]; the other commands read it.
//!
//! The YAML rendering ([`render_workflow`]) is a pure function of the config with tests; the
//! interactive choices go through the [`InitPrompt`] trait, and package discovery through the
//! [`AdapterFactory`] trait, so the flow is testable.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use inquire::{MultiSelect, Select, Text};

use crate::adapter::{Adapter, Pkg};
use crate::config::{
    ArchiveFormat, ChangelogScope, ChangelogStrategy, Ecosystem, GithubReleaseNotes, Mode,
    PackageEntry, ReleaseConfig, Target, COMMON_TAG_FORMATS, DEFAULT_TAG_FORMAT,
    DEFAULT_VERSION_FIELD, TARGET_REGISTRY,
};
use crate::discover::{scan_generic_candidates, GenericCandidate};

const INSTALL_SH_URL: &str =
    "https://raw.githubusercontent.com/Open-Tech-Foundation/release/main/install.sh";
const INSTALL_PS1_URL: &str =
    "https://raw.githubusercontent.com/Open-Tech-Foundation/release/main/install.ps1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NpmTool {
    Npm,
    Bun,
    Pnpm,
    Yarn,
}

impl NpmTool {
    fn detect(root: &Path) -> Self {
        if root.join("bun.lockb").exists() || root.join("bun.lock").exists() {
            Self::Bun
        } else if root.join("pnpm-lock.yaml").exists() {
            Self::Pnpm
        } else if root.join("yarn.lock").exists() {
            Self::Yarn
        } else {
            Self::Npm
        }
    }

    fn setup_node(self, s: &mut String, registry: bool) {
        match self {
            Self::Bun => {
                s.push_str("      - uses: oven-sh/setup-bun@v2\n");
                if registry {
                    s.push_str("      - uses: actions/setup-node@v4\n");
                    s.push_str(
                        "        with:\n          node-version: 24\n          registry-url: https://registry.npmjs.org\n",
                    );
                }
            }
            Self::Pnpm => {
                s.push_str("      - uses: pnpm/action-setup@v4\n");
                s.push_str("        with:\n          version: latest\n");
                s.push_str("      - uses: actions/setup-node@v4\n");
                s.push_str("        with:\n          node-version: 24\n");
                if registry {
                    s.push_str("          registry-url: https://registry.npmjs.org\n");
                }
            }
            Self::Yarn => {
                s.push_str("      - uses: actions/setup-node@v4\n");
                s.push_str("        with:\n          node-version: 24\n");
                if registry {
                    s.push_str("          registry-url: https://registry.npmjs.org\n");
                }
            }
            Self::Npm => {
                s.push_str("      - uses: actions/setup-node@v4\n");
                s.push_str("        with:\n          node-version: 24\n");
                if registry {
                    s.push_str("          registry-url: https://registry.npmjs.org\n");
                }
            }
        }
    }

    fn install_command(self) -> &'static str {
        match self {
            Self::Npm => "npm ci",
            Self::Bun => "bun install --frozen-lockfile",
            Self::Pnpm => "pnpm install --frozen-lockfile",
            Self::Yarn => "yarn install --immutable",
        }
    }
}

/// Options for an `init` run.
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    /// Overwrite existing files (`release.toml`, `release.yml`) without prompting.
    pub force: bool,
}

/// Builds an [`Adapter`] for a given ecosystem. Implemented by the CLI (which owns the concrete
/// adapters); `init` uses it to discover each enabled ecosystem's packages.
pub trait AdapterFactory {
    fn make(&self, ecosystem: Ecosystem) -> Box<dyn Adapter>;

    /// Human-readable notes from adapter-specific discovery, such as skipped workspace manifests.
    fn discovery_notes(&self, _: Ecosystem) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
}

/// The interactive choices `init` needs.
pub trait InitPrompt {
    /// Which ecosystems to enable (multi-select: `npm`, `crates.io`).
    fn select_adapters(&self) -> Result<Vec<Ecosystem>>;
    /// Prompt JSR scaffold values.
    fn prompt_jsr_scaffold(
        &self,
        default_name: &str,
        default_version: &str,
        default_exports: &str,
    ) -> Result<(String, String)>;
    /// Which publishable packages need built artifacts before publish/release?
    fn select_build_packages(&self, publishable: &[&Pkg]) -> Result<Vec<String>>;
    /// The full build config for one selected package (`enabled` is the chosen adapter set).
    fn build_entry(&self, pkg_name: &str, enabled: &[Ecosystem]) -> Result<PackageEntry>;
    /// Choose/enter generic packages. `found` is what the repo scan inferred (manifests with a
    /// version); the user imports from those and/or adds more by hand. Asked only when the generic
    /// adapter is enabled.
    fn generic_packages(&self, found: &[GenericCandidate]) -> Result<Vec<PackageEntry>>;
    /// Confirm overwriting an existing file (only asked when not `--force`).
    fn confirm_overwrite(&self, path: &Path) -> Result<bool>;
    /// Ask for the git tag format used by version/preflight/publish.
    fn tag_format(&self, suggestion: &TagFormatSuggestion) -> Result<String>;
    /// Ask for the git hosting provider.
    fn prompt_provider(&self) -> Result<String>;
    /// Ask where release notes should be maintained.
    fn prompt_changelog_scope(&self) -> Result<ChangelogScope>;
    /// Ask how GitHub Release bodies should be generated.
    fn prompt_github_release_notes(&self) -> Result<GithubReleaseNotes>;
}

/// Wire up the real prompt and run the generator.
pub fn run(factory: &dyn AdapterFactory, root: &Path, opts: &InitOptions) -> Result<()> {
    print_intro();
    orchestrate(factory, &StdinInitPrompt, root, opts)
}

fn publish_ignore_paths_seed(
    discovered_publishable: &[Pkg],
    configured_packages: &[PackageEntry],
) -> HashMap<String, Vec<String>> {
    let mut names: Vec<String> = discovered_publishable
        .iter()
        .map(|pkg| pkg.name.clone())
        .collect();
    names.extend(configured_packages.iter().map(|pkg| pkg.name.clone()));
    names.sort();
    names.dedup();
    names.into_iter().map(|name| (name, Vec::new())).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagFormatSuggestion {
    pub default_format: String,
    pub detected_format: Option<String>,
}

impl TagFormatSuggestion {
    fn legacy_formats_for(&self, selected_format: &str) -> Vec<String> {
        self.detected_format
            .iter()
            .filter(|detected| detected.as_str() != selected_format)
            .cloned()
            .collect()
    }
}

/// A short, friendly preamble so a first-time dev knows what `init` will ask and that nothing is
/// locked in — every answer has a default and is editable afterward.
fn print_intro() {
    println!("\notf-release init — configure releases for this repo.\n");
    println!(
        "  • Writes release.toml (the editable source of truth) and a GitHub release workflow."
    );
    println!("  • Press Enter to accept the default shown in (parentheses); a hint sits under each prompt.");
    println!("  • Nothing is permanent — re-run init, edit release.toml by hand, or use `otf-release config`.\n");
}

fn suggest_tag_format(root: &Path, publishable_count: usize) -> TagFormatSuggestion {
    let detected_format = existing_tags(root).and_then(|tags| infer_tag_format(&tags));
    TagFormatSuggestion {
        default_format: detected_format.clone().unwrap_or_else(|| {
            if publishable_count > 1 {
                "{name}@{version}".to_string()
            } else {
                DEFAULT_TAG_FORMAT.to_string()
            }
        }),
        detected_format,
    }
}

fn existing_tags(root: &Path) -> Option<Vec<String>> {
    let out = Command::new("git")
        .args(["tag", "--list"])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

fn infer_tag_format(tags: &[String]) -> Option<String> {
    let mut counts = std::collections::HashMap::<&'static str, usize>::new();
    for tag in tags {
        if is_package_version_tag(tag, true) {
            *counts.entry("{name}@v{version}").or_default() += 1;
        } else if is_package_version_tag(tag, false) {
            *counts.entry("{name}@{version}").or_default() += 1;
        } else if parse_tag_version(tag.strip_prefix('v').unwrap_or_default()).is_some() {
            *counts.entry("v{version}").or_default() += 1;
        } else if parse_tag_version(tag).is_some() {
            *counts.entry("{version}").or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(format, _)| format.to_string())
}

fn is_package_version_tag(tag: &str, version_has_v: bool) -> bool {
    let Some((name, version)) = tag.rsplit_once('@') else {
        return false;
    };
    if name.is_empty() {
        return false;
    }
    let version = if version_has_v {
        version.strip_prefix('v').unwrap_or_default()
    } else {
        if version.starts_with('v') {
            return false;
        }
        version
    };
    parse_tag_version(version).is_some()
}

fn parse_tag_version(version: &str) -> Option<()> {
    let core = version.split('-').next().unwrap_or(version);
    let mut parts = core.split('.');
    parts.next()?.parse::<u64>().ok()?;
    parts.next()?.parse::<u64>().ok()?;
    parts.next()?.parse::<u64>().ok()?;
    Some(())
}

fn detect_jsr_exports_default(dir: &Path) -> &'static str {
    let files = ["src/index.ts", "mod.ts", "index.ts", "src/mod.ts"];
    for f in files {
        if dir.join(f).exists() {
            return match f {
                "src/index.ts" => "./src/index.ts",
                "mod.ts" => "./mod.ts",
                "index.ts" => "./index.ts",
                "src/mod.ts" => "./src/mod.ts",
                _ => "./src/index.ts",
            };
        }
    }
    "./src/index.ts"
}

/// The testable core of `init`.
pub fn orchestrate(
    factory: &dyn AdapterFactory,
    prompt: &dyn InitPrompt,
    root: &Path,
    opts: &InitOptions,
) -> Result<()> {
    let enabled = prompt.select_adapters()?;
    if enabled.is_empty() {
        bail!("No adapters selected — nothing to configure.");
    }

    // Discover publishable packages across every *discoverable* ecosystem (npm/cargo read
    // manifests). The generic adapter has nothing to discover — its packages are entered below.
    let mut publishable: Vec<Pkg> = Vec::new();
    let mut cargo_publishable: Vec<Pkg> = Vec::new();
    let mut npm_publishable: Vec<Pkg> = Vec::new();
    let mut jsr_publishable: Vec<Pkg> = Vec::new();
    for &eco in enabled.iter().filter(|e| **e != Ecosystem::Generic) {
        let adapter = factory.make(eco);
        for pkg in adapter.discover_packages()? {
            if pkg.publishable {
                match eco {
                    Ecosystem::Cargo => cargo_publishable.push(pkg.clone()),
                    Ecosystem::Npm => npm_publishable.push(pkg.clone()),
                    Ecosystem::Jsr => jsr_publishable.push(pkg.clone()),
                    Ecosystem::Generic => {}
                }
                publishable.push(pkg);
            }
        }
        for note in factory.discovery_notes(eco)? {
            println!("{note}");
        }
    }

    if enabled.contains(&Ecosystem::Jsr) && jsr_publishable.is_empty() {
        let jsr_adapter = factory.make(Ecosystem::Jsr);
        let jsr_pkgs = if !npm_publishable.is_empty() {
            let mut created_any = false;
            for npm_pkg in &npm_publishable {
                let pkg_dir = npm_pkg.manifest_path.parent().unwrap();
                let jsr_path = pkg_dir.join("jsr.json");

                let suggested_name = if npm_pkg.name.starts_with('@') {
                    npm_pkg.name.clone()
                } else {
                    format!("@scope/{}", npm_pkg.name)
                };

                let suggested_exports = detect_jsr_exports_default(pkg_dir);

                println!("\nScaffolding jsr.json for package: {}", npm_pkg.name);
                let (name, exports) = prompt.prompt_jsr_scaffold(
                    &suggested_name,
                    &npm_pkg.version,
                    suggested_exports,
                )?;

                let jsr_json = serde_json::json!({
                    "name": name,
                    "version": npm_pkg.version,
                    "exports": exports
                });

                let content = serde_json::to_string_pretty(&jsr_json)?;
                std::fs::write(&jsr_path, content)?;
                println!("Created default jsr.json at {}", jsr_path.display());
                created_any = true;
            }
            if created_any {
                jsr_adapter.discover_packages()?
            } else {
                Vec::new()
            }
        } else {
            let suggested_exports = detect_jsr_exports_default(root);
            println!("\nScaffolding a new JSR package at the repository root");
            let (name, exports) =
                prompt.prompt_jsr_scaffold("@scope/my-package", "0.1.0", suggested_exports)?;
            let jsr_path = root.join("jsr.json");
            let jsr_json = serde_json::json!({
                "name": name,
                "version": "0.1.0",
                "exports": exports
            });
            let content = serde_json::to_string_pretty(&jsr_json)?;
            std::fs::write(&jsr_path, content)?;
            println!("Created default jsr.json at {}", jsr_path.display());
            jsr_adapter.discover_packages()?
        };

        for pkg in jsr_pkgs {
            if pkg.publishable {
                jsr_publishable.push(pkg.clone());
                publishable.push(pkg);
            }
        }
    }

    let mut packages = Vec::new();

    // Cargo packages go through the interactive build-step prompt: they may be build-only, matrix,
    // or cross-compiled — decisions only the user can make. npm packages are auto-configured below,
    // so they are deliberately excluded from this prompt (and from the adapter choice).
    let cargo_refs: Vec<&Pkg> = cargo_publishable.iter().collect();
    let build_names = prompt.select_build_packages(&cargo_refs)?;
    let enabled_non_npm: Vec<Ecosystem> = enabled
        .iter()
        .copied()
        .filter(|e| *e != Ecosystem::Npm && *e != Ecosystem::Jsr)
        .collect();
    for name in &build_names {
        packages.push(prompt.build_entry(name, &enabled_non_npm)?);
    }

    // npm convention: the tool owns the build. For each publishable npm package with a `build`
    // script, inject an inline-build publish entry (built inside its own publish job, no separate
    // build job or artifact staging), and strip npm's pack/publish lifecycle hooks so npm can't
    // silently re-run a build behind the release pipeline.
    if !npm_publishable.is_empty() {
        let npm = factory.make(Ecosystem::Npm);
        for pkg in &npm_publishable {
            let removed = npm.strip_publish_hooks(pkg)?;
            if !removed.is_empty() {
                println!(
                    "Removed npm lifecycle hook(s) from {}: {}. The release pipeline runs the build \
                     itself — move any custom steps into a `build` script or [hooks] in release.toml.",
                    pkg.name,
                    removed.join(", ")
                );
            }
            if let Some(command) = npm.build_command(pkg)? {
                packages.push(PackageEntry {
                    name: pkg.name.clone(),
                    adapter: Ecosystem::Npm,
                    mode: Mode::Publish,
                    matrix: false,
                    targets: Vec::new(),
                    command,
                    artifacts: String::new(),
                    bin_name: None,
                    compress: None,
                    manifest: Some(rel_path(root, &pkg.manifest_path)),
                    version_field: None,
                    publish: None,
                    archive: None,
                    checksums: false,
                    include: Vec::new(),
                });
            }
        }
    }

    if !jsr_publishable.is_empty() {
        let jsr = factory.make(Ecosystem::Jsr);
        for pkg in &jsr_publishable {
            let command = jsr.build_command(pkg)?.unwrap_or_default();
            packages.push(PackageEntry {
                name: pkg.name.clone(),
                adapter: Ecosystem::Jsr,
                mode: Mode::Publish,
                matrix: false,
                targets: Vec::new(),
                command,
                artifacts: String::new(),
                bin_name: None,
                compress: None,
                manifest: Some(rel_path(root, &pkg.manifest_path)),
                version_field: None,
                publish: None,
                archive: None,
                checksums: false,
                include: Vec::new(),
            });
        }
    }

    // Generic packages have no native adapter discovery — scan the repo for known manifests and
    // let the user import from what we infer (plus add any by hand).
    if enabled.contains(&Ecosystem::Generic) {
        let found = scan_generic_candidates(root);
        packages.extend(prompt.generic_packages(&found)?);
    }

    let tag_suggestion = suggest_tag_format(root, publishable.len());
    let tag_format = prompt.tag_format(&tag_suggestion)?;
    crate::config::format_tag(&tag_format, "package", "1.2.3")?;
    let legacy_tag_formats = tag_suggestion.legacy_formats_for(&tag_format);

    let config = ReleaseConfig {
        hooks: crate::config::Hooks::default(),
        publish: crate::config::PublishConfig {
            ignore_paths: publish_ignore_paths_seed(&publishable, &packages),
        },
        adapters: enabled,
        skip_publish: Vec::new(),
        packages,
        snapshot_tag: None,
        tag_format,
        legacy_tag_formats,
        provider: prompt.prompt_provider()?,
        default_branch: crate::config::DEFAULT_BRANCH.to_string(),
        changelog_strategy: ChangelogStrategy::Curated,
        changelog_scope: prompt.prompt_changelog_scope()?,
        github_release_notes: prompt.prompt_github_release_notes()?,
    };

    // 1. Persist the source of truth.
    let toml_path = ReleaseConfig::path(root);
    if write_allowed(&toml_path, opts.force, prompt)? {
        config.save(root)?;
        println!("Wrote {}", toml_path.display());
    }

    // 2. Generate the workflow from it.
    let yaml = render_workflow_for_root(&config, root);
    let yml_path = root.join(".github/workflows/release.yml");
    if write_allowed(&yml_path, opts.force, prompt)? {
        fs::create_dir_all(yml_path.parent().unwrap())
            .with_context(|| format!("creating {}", yml_path.parent().unwrap().display()))?;
        fs::write(&yml_path, yaml).with_context(|| format!("writing {}", yml_path.display()))?;
        println!("Wrote {}", yml_path.display());
    }

    Ok(())
}

/// Whether we may write `path`: true unless it exists, isn't forced, and the user declines.
fn write_allowed(path: &Path, force: bool, prompt: &dyn InitPrompt) -> Result<bool> {
    if path.exists() && !force && !prompt.confirm_overwrite(path)? {
        println!("Left existing {} unchanged.", path.display());
        return Ok(false);
    }
    Ok(true)
}

/// A CI job name derived from a package name: `build-<slug>`.
fn build_job(name: &str) -> String {
    format!("build-{}", slug(name))
}

/// Lowercase a package name into a job/artifact-safe slug (`@x/cli` → `x-cli`).
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

fn release_output(name: &str) -> String {
    format!("release_{}", slug(name).replace('-', "_"))
}

/// Multi-select build targets from the built-in registry, returning fully-resolved [`Target`]s
/// (triple/runner/stage_as/ext/cross all filled). 32-bit targets are offered but off by default.
fn select_targets(prompt: &str) -> Result<Vec<Target>> {
    let defaults: Vec<usize> = TARGET_REGISTRY
        .iter()
        .enumerate()
        .filter(|(_, t)| t.default_on)
        .map(|(i, _)| i)
        .collect();
    let labels: Vec<String> = TARGET_REGISTRY
        .iter()
        .map(|t| format!("{} - {}-{}", t.label, t.name, t.arch))
        .collect();
    let selected = MultiSelect::new(prompt, labels)
        .with_default(&defaults)
        .with_help_message(
            "the widely-supported platforms are pre-selected; \
             space toggles · enter confirm",
        )
        .raw_prompt()?;
    Ok(selected
        .iter()
        .map(|s| {
            let info = &TARGET_REGISTRY[s.index];
            Target::resolved(info.name, info.arch)
        })
        .collect())
}

/// The preliminary job that checks if a release is needed, guarding the expensive build steps.
fn render_check_release_job(s: &mut String, config: &ReleaseConfig) {
    s.push_str("  check-release:\n");
    s.push_str("    runs-on: ubuntu-latest\n");
    s.push_str("    outputs:\n");
    s.push_str("      should_release: ${{ steps.check.outputs.should_release }}\n");
    for entry in &config.packages {
        s.push_str(&format!(
            "      {}: ${{{{ steps.check.outputs.{} }}}}\n",
            release_output(&entry.name),
            release_output(&entry.name)
        ));
    }
    s.push_str("    steps:\n");
    // `fetch-depth: 0` so release tags are present locally for `otf-release check` to compare
    // against — a shallow checkout carries no tags.
    s.push_str("      - uses: actions/checkout@v4\n");
    s.push_str("        with:\n");
    s.push_str("          fetch-depth: 0\n");
    push_install_otf_release(s);
    // The gate delegates to the binary, like every other job (`matrix`/`build`/`publish`): the tool
    // reads each package's version and tag with the *same* logic it publishes with, so the gate can
    // never drift. It prints `true` when any configured package has an untagged version to release.
    s.push_str("      - id: check\n");
    s.push_str("        run: |\n");
    s.push_str("          echo \"should_release=$(otf-release check");
    for entry in &config.packages {
        s.push_str(&format!(" --exclude-package {}", entry.name));
    }
    s.push_str(")\" >> \"$GITHUB_OUTPUT\"\n");
    for entry in &config.packages {
        s.push_str(&format!(
            "          echo \"{}=$(otf-release check --package {})\" >> \"$GITHUB_OUTPUT\"\n",
            release_output(&entry.name),
            entry.name
        ));
    }
    s.push('\n');
}

/// Render `.github/workflows/release.yml` from the config.
///
/// Shape:
/// - one `build-<pkg>` job per package that has a build command (matrix or single runner),
/// - a single `publish` job (if any registry adapter is active) that sets up the needed
///   toolchains and runs `otf-release publish` once — it publishes only `publish`-mode packages
///   across every enabled ecosystem (npm, crates.io, generic),
/// - a `github-release` job if any package is `build-only` — attaches its artifacts to
///   GitHub Releases tagged from `tag_format`, idempotently. **No registry push for
///   build-only packages.**
pub fn render_snapshot_workflow(config: &ReleaseConfig) -> String {
    render_snapshot_workflow_with_npm_tool(config, NpmTool::Npm)
}

fn render_snapshot_workflow_with_npm_tool(config: &ReleaseConfig, npm_tool: NpmTool) -> String {
    let mut s = String::new();
    s.push_str("name: Snapshot Release\n\n");
    s.push_str("on:\n");
    s.push_str("  push:\n");
    s.push_str("    branches: [\"main\"]\n\n");
    s.push_str("jobs:\n");
    s.push_str("  snapshot:\n");
    s.push_str("    runs-on: ubuntu-latest\n");
    s.push_str("    permissions:\n");
    s.push_str("      contents: write\n");
    s.push_str("      id-token: write\n");
    s.push_str("    steps:\n");
    s.push_str("      - uses: actions/checkout@v4\n");
    s.push_str("        with:\n");
    s.push_str("          fetch-depth: 0\n");

    if config.adapters.contains(&Ecosystem::Cargo) {
        s.push_str("      - name: Install Rust\n");
        s.push_str("        run: rustup update stable\n");
    }
    if config.adapters.contains(&Ecosystem::Npm) {
        npm_tool.setup_node(&mut s, true);
    }

    push_install_otf_release(&mut s);
    s.push_str("      - name: Snapshot Release\n");
    s.push_str("        env:\n");
    if config.adapters.contains(&Ecosystem::Cargo) {
        s.push_str("          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}\n");
    }
    if config.adapters.contains(&Ecosystem::Npm) {
        s.push_str("          NODE_AUTH_TOKEN: ${{ secrets.NPM_TOKEN }}\n");
    }
    s.push_str("        run: otf-release snapshot\n");
    s
}

/// Install `otf-release` on an ubuntu-only job: a single bash step, no `runner.os` guard. Every
/// generated job runs on `ubuntu-latest` except the build matrix fan-out, so this is the common case
/// — the Windows branch would never fire here and is left out as dead YAML.
fn push_install_otf_release(s: &mut String) {
    s.push_str("      - name: Install otf-release\n");
    s.push_str("        shell: bash\n");
    s.push_str(&format!(
        "        run: curl -fsSL {INSTALL_SH_URL} | bash\n"
    ));
}

/// Install `otf-release` on a job that may run on Windows (the build matrix fan-out, whose runner is
/// `${{ matrix.runner }}`): both the bash and PowerShell variants, each guarded by `runner.os` so
/// exactly the right one fires per runner.
fn push_install_otf_release_cross_platform(s: &mut String) {
    s.push_str("      - name: Install otf-release\n");
    s.push_str("        if: runner.os != 'Windows'\n");
    s.push_str("        shell: bash\n");
    s.push_str(&format!(
        "        run: curl -fsSL {INSTALL_SH_URL} | bash\n"
    ));
    s.push_str("      - name: Install otf-release\n");
    s.push_str("        if: runner.os == 'Windows'\n");
    s.push_str("        shell: pwsh\n");
    s.push_str(&format!("        run: irm {INSTALL_PS1_URL} | iex\n"));
}

pub fn render_workflow(config: &ReleaseConfig) -> String {
    render_workflow_with_npm_tool(config, NpmTool::Npm)
}

pub(crate) fn render_workflow_for_root(config: &ReleaseConfig, root: &Path) -> String {
    render_workflow_with_npm_tool(config, NpmTool::detect(root))
}

fn render_workflow_with_npm_tool(config: &ReleaseConfig, npm_tool: NpmTool) -> String {
    let any_build_only = config.packages.iter().any(|p| p.is_build_only());
    let npm_enabled = config.adapters.contains(&Ecosystem::Npm);
    let jsr_publishes = config
        .packages
        .iter()
        .any(|p| p.adapter == Ecosystem::Jsr && p.is_publish());
    let cargo_publishes = config
        .packages
        .iter()
        .any(|p| p.adapter == Ecosystem::Cargo && p.is_publish());
    let generic_publishes = config
        .packages
        .iter()
        .any(|p| p.adapter == Ecosystem::Generic && p.is_publish());
    let needs_publish = npm_enabled || jsr_publishes || cargo_publishes || generic_publishes;

    let mut s = String::from("name: Release\n\non:\n  push:\n    branches: [main]\n");
    if any_build_only || needs_publish {
        s.push_str("\npermissions:\n  contents: write  # create tags and GitHub Releases\n");
        if npm_enabled || jsr_publishes {
            s.push_str("  id-token: write\n");
        }
    }
    // Serialize release runs: two quick pushes to main must not run two `otf-release publish`
    // pipelines at once. `cancel-in-progress: false` lets an in-flight release finish rather than
    // being interrupted mid-publish.
    s.push_str("\nconcurrency:\n  group: release\n  cancel-in-progress: false\n");
    s.push_str("\njobs:\n");
    render_check_release_job(&mut s, config);

    // Build jobs only for packages that declare a build command *and* stage artifacts across
    // jobs. Inline-build npm packages build inside their own publish job, so they get none.
    let has_build = |p: &&PackageEntry| !p.command.trim().is_empty();
    for entry in config
        .packages
        .iter()
        .filter(|p| has_build(p) && !p.builds_inline())
    {
        render_build_job(&mut s, entry, npm_tool);
    }

    for entry in config
        .packages
        .iter()
        .filter(|p| p.is_publish() && has_build(p))
    {
        render_package_publish_job(&mut s, entry, npm_tool);
    }

    if needs_publish {
        render_publish_job(
            &mut s,
            npm_enabled,
            npm_tool,
            cargo_publishes,
            jsr_publishes,
            generic_publishes,
            &config
                .packages
                .iter()
                .filter(|p| p.is_publish() && has_build(p))
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
        );
    }

    if any_build_only {
        let build_only: Vec<&PackageEntry> = config
            .packages
            .iter()
            .filter(|p| p.is_build_only())
            .collect();
        for entry in build_only {
            let needs = if entry.command.trim().is_empty() {
                Vec::new()
            } else {
                vec![build_job(&entry.name)]
            };
            render_github_release(&mut s, &needs, entry);
        }
    }

    s
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// One build job: matrix or single runner, runs the package's command, uploads its artifacts.
fn render_build_job(s: &mut String, entry: &PackageEntry, npm_tool: NpmTool) {
    if entry.matrix {
        render_matrix_build_jobs(s, entry, npm_tool);
    } else {
        render_single_build_job(s, entry, npm_tool);
    }
}

/// Whether the build leg needs a Rust toolchain / a Node setup, inferred from the command and
/// adapter. A matrix npm package (a Rust binary shipped in an npm wrapper) needs both.
fn build_toolchains(entry: &PackageEntry) -> (bool, bool) {
    let rust = entry.command.contains("cargo");
    let node = entry.adapter == Ecosystem::Npm
        || entry.command.contains("npm")
        || entry.command.contains("node");
    (rust, node)
}

/// A matrix package builds as two jobs: a tiny `matrix-<slug>` job that emits the target matrix
/// from `release.toml` via `otf-release matrix` (so the list never drifts), and a `build-<slug>`
/// job that fans out over `fromJSON(...)` and calls `otf-release build` per target. The tool — not
/// hand-written YAML — owns the triple/runner/cross/stage_as reconciliation, so there are no
/// `# edit me` markers.
fn render_matrix_build_jobs(s: &mut String, entry: &PackageEntry, npm_tool: NpmTool) {
    let name = &entry.name;
    let art_slug = slug(name);
    let matrix_job = format!("matrix-{art_slug}");
    let build = build_job(name);

    // 1. Emit the matrix from release.toml.
    s.push_str(&format!("  {matrix_job}:\n"));
    s.push_str("    needs: [check-release]\n");
    s.push_str(&format!(
        "    if: needs.check-release.outputs.{} == 'true'\n",
        release_output(name)
    ));
    s.push_str("    runs-on: ubuntu-latest\n");
    s.push_str("    outputs:\n      matrix: ${{ steps.set.outputs.matrix }}\n");
    s.push_str("    steps:\n");
    s.push_str("      - uses: actions/checkout@v4\n");
    push_install_otf_release(s);
    s.push_str("      - id: set\n");
    s.push_str(&format!(
        "        run: echo \"matrix=$(otf-release matrix --package {name})\" >> \"$GITHUB_OUTPUT\"\n\n"
    ));

    // 2. Fan out over the matrix and build + stage each target.
    s.push_str(&format!("  {build}:\n"));
    s.push_str(&format!("    needs: [check-release, {matrix_job}]\n"));
    s.push_str(&format!(
        "    if: needs.check-release.outputs.{} == 'true'\n",
        release_output(name)
    ));
    s.push_str("    runs-on: ${{ matrix.runner }}\n");
    s.push_str("    strategy:\n      fail-fast: false\n");
    s.push_str(&format!(
        "      matrix: ${{{{ fromJSON(needs.{matrix_job}.outputs.matrix) }}}}\n"
    ));
    s.push_str("    steps:\n");
    s.push_str("      - uses: actions/checkout@v4\n");
    // Cross prep is driven by the selected target set and each matrix row's `cross` flag.
    if entry.targets.iter().any(|target| target.is_cross()) {
        s.push_str("      - name: Install cross toolchain\n");
        s.push_str("        if: ${{ matrix.cross }}\n");
        s.push_str("        run: |\n");
        s.push_str("          sudo apt-get update\n");
        s.push_str("          sudo apt-get install -y gcc-${{ matrix.arch }}-linux-gnu\n");
    }
    let (rust, node) = build_toolchains(entry);
    if rust {
        s.push_str("      - uses: dtolnay/rust-toolchain@stable\n");
        s.push_str("        with:\n          targets: ${{ matrix.triple }}\n");
    }
    if node {
        npm_tool.setup_node(s, false);
        s.push_str(&format!("      - run: {}\n", npm_tool.install_command()));
    }
    push_install_otf_release_cross_platform(s);
    s.push_str(&format!("      - name: Build {name}\n"));
    s.push_str(&format!(
        "        run: otf-release build --package {name} --target ${{{{ matrix.name }}}}/${{{{ matrix.arch }}}}\n"
    ));
    s.push_str("      - uses: actions/upload-artifact@v4\n");
    s.push_str("        with:\n");
    s.push_str(&format!(
        "          name: {art_slug}-${{{{ matrix.name }}}}-${{{{ matrix.arch }}}}\n"
    ));
    s.push_str(&format!("          path: .artifacts/{name}\n"));
    s.push('\n');
}

/// A non-matrix package builds on one runner with its plain command.
fn render_single_build_job(s: &mut String, entry: &PackageEntry, npm_tool: NpmTool) {
    let job = build_job(&entry.name);
    let art_slug = slug(&entry.name);
    s.push_str(&format!("  {job}:\n"));
    s.push_str("    needs: [check-release]\n");
    s.push_str(&format!(
        "    if: needs.check-release.outputs.{} == 'true'\n",
        release_output(&entry.name)
    ));
    s.push_str("    runs-on: ubuntu-latest\n");
    s.push_str("    steps:\n");
    s.push_str("      - uses: actions/checkout@v4\n");
    match entry.adapter {
        Ecosystem::Cargo => {
            s.push_str("      - uses: dtolnay/rust-toolchain@stable\n");
        }
        Ecosystem::Npm => {
            npm_tool.setup_node(s, false);
            s.push_str(&format!("      - run: {}\n", npm_tool.install_command()));
        }
        Ecosystem::Jsr => {
            s.push_str("      - uses: denoland/setup-deno@v1\n");
        }
        // Generic is language-agnostic: no toolchain is assumed — the command sets up its own.
        Ecosystem::Generic => {}
    }
    s.push_str(&format!("      - name: Build {}\n", entry.name));
    s.push_str(&format!("        run: {}\n", entry.command));
    s.push_str("      - uses: actions/upload-artifact@v4\n");
    s.push_str("        with:\n");
    s.push_str(&format!("          name: {art_slug}\n"));
    s.push_str(&format!("          path: {}\n", entry.artifacts));
    s.push('\n');
}

/// Format a `needs:` line, omitted entirely when there are no dependencies.
fn needs_line(s: &mut String, needs: &[String]) {
    if !needs.is_empty() {
        s.push_str(&format!("    needs: [{}]\n", needs.join(", ")));
    }
}

/// Download staged artifacts into `.artifacts/`, only when something fed this job.
fn download_artifacts(s: &mut String, needs: &[String]) -> bool {
    if needs.is_empty() {
        return false;
    }
    s.push_str("      - uses: actions/download-artifact@v4\n");
    s.push_str("        with:\n          path: .artifacts\n");
    true
}

/// The single registry publish job. Runs `otf-release publish` **once**; the tool loops every
/// enabled adapter internally, so this one job covers npm + crates.io + generic. It sets up only
/// the toolchains the active registries need; generic publish steps carry `# edit me` markers
/// since the tool can't know your registry's toolchain or secret.
///
/// This is the catch-all publisher for packages shipped **as-is** (no build step of their own):
/// every package with its own build gets a dedicated `publish-<pkg>` job and is listed in
/// `excluded_packages`, so this job never stages artifacts itself — it publishes what the registry
/// packs directly.
fn render_publish_job(
    s: &mut String,
    npm: bool,
    npm_tool: NpmTool,
    cargo: bool,
    jsr: bool,
    generic: bool,
    excluded_packages: &[&str],
) {
    s.push_str("  publish:\n");
    // Each excluded package has its own `publish-<pkg>` job (it needs a build). The catch-all
    // publishes everything else — including dependents that pin an *exact* version of one of those
    // packages (e.g. a JS package pinning a compiler). So this job must wait for those dedicated
    // publish jobs: otherwise a dependent can land on the registry before the package it pins exists,
    // or — worse — stay published pointing at a version whose publish failed.
    let dep_jobs: Vec<String> = excluded_packages
        .iter()
        .map(|name| format!("publish-{}", slug(name)))
        .collect();
    let mut needs = vec!["check-release".to_string()];
    needs.extend(dep_jobs.iter().cloned());
    needs_line(s, &needs);
    if dep_jobs.is_empty() {
        s.push_str("    if: needs.check-release.outputs.should_release == 'true'\n");
    } else {
        // `always()` keeps this job evaluating when the dep jobs were *skipped* (a release that
        // touches none of them) — without it GitHub auto-skips any job whose `needs` was skipped.
        // The result guards then abort only on a genuine failure/cancellation of a dep publish, so a
        // skipped dep still lets the catch-all run.
        let mut conditions = vec![
            "always()".to_string(),
            "needs.check-release.outputs.should_release == 'true'".to_string(),
        ];
        for job in &dep_jobs {
            conditions.push(format!("needs.{job}.result != 'failure'"));
            conditions.push(format!("needs.{job}.result != 'cancelled'"));
        }
        s.push_str("    if: >-\n");
        s.push_str(&format!("      {}\n", conditions.join(" &&\n      ")));
    }
    s.push_str("    runs-on: ubuntu-latest\n");
    s.push_str("    steps:\n");
    s.push_str("      - uses: actions/checkout@v4\n        with:\n          fetch-depth: 0\n");
    if npm {
        npm_tool.setup_node(s, true);
    }
    if cargo {
        s.push_str("      - uses: dtolnay/rust-toolchain@stable\n");
    }
    if jsr {
        s.push_str("      - uses: denoland/setup-deno@v1\n");
    }
    if generic {
        s.push_str("      # edit me: set up the toolchain your generic publish command needs\n");
    }
    if npm {
        s.push_str(&format!("      - run: {}\n", npm_tool.install_command()));
    }
    push_install_otf_release(s);
    s.push_str("      - name: Publish\n");
    s.push_str("        run: otf-release publish");
    for package in excluded_packages {
        s.push_str(&format!(" --exclude-package {package}"));
    }
    s.push('\n');
    s.push_str("        env:\n");
    if npm {
        s.push_str("          NODE_AUTH_TOKEN: ${{ secrets.NPM_TOKEN }}\n");
    }
    if cargo {
        s.push_str("          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}\n");
    }
    if jsr {
        s.push_str("          JSR_TOKEN: ${{ secrets.JSR_TOKEN }}\n");
    }
    if generic {
        s.push_str("          # edit me: any secret your generic publish command needs\n");
    }
    s.push_str("          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}\n");
    s.push('\n');
}

/// The package subdirectory (relative to the repo root) a package's build should run in, derived
/// from its manifest path. `None` for a root manifest (`package.json`), where no `working-directory`
/// is needed.
fn package_workdir(entry: &PackageEntry) -> Option<String> {
    let manifest = entry.manifest.as_deref()?;
    manifest
        .rsplit_once('/')
        .map(|(dir, _)| dir.to_string())
        .filter(|dir| !dir.is_empty())
}

/// Publish one configured build package after, and only after, its own build succeeds.
fn render_package_publish_job(s: &mut String, entry: &PackageEntry, npm_tool: NpmTool) {
    let name = &entry.name;
    let slug = slug(name);
    let inline = entry.builds_inline();
    s.push_str(&format!("  publish-{slug}:\n"));
    if inline {
        // No separate build job to wait on — the build happens in this job.
        s.push_str("    needs: [check-release]\n");
    } else {
        s.push_str(&format!(
            "    needs: [check-release, {}]\n",
            build_job(name)
        ));
    }
    s.push_str(&format!(
        "    if: needs.check-release.outputs.{} == 'true'\n",
        release_output(name)
    ));
    s.push_str("    runs-on: ubuntu-latest\n");
    s.push_str("    steps:\n");
    s.push_str("      - uses: actions/checkout@v4\n        with:\n          fetch-depth: 0\n");
    match entry.adapter {
        Ecosystem::Npm => npm_tool.setup_node(s, true),
        Ecosystem::Cargo => s.push_str("      - uses: dtolnay/rust-toolchain@stable\n"),
        Ecosystem::Jsr => s.push_str("      - uses: denoland/setup-deno@v1\n"),
        Ecosystem::Generic => {}
    }
    if inline {
        // The tool owns the build: install, run the package's build command in its own directory,
        // then publish. npm packs the freshly built output from this same runner — no artifact
        // upload/download, and npm's own pack/publish lifecycle hooks were stripped at init time.
        s.push_str(&format!("      - run: {}\n", npm_tool.install_command()));
        s.push_str(&format!("      - name: Build {name}\n"));
        s.push_str(&format!("        run: {}\n", entry.command));
        if let Some(dir) = package_workdir(entry) {
            s.push_str(&format!("        working-directory: {dir}\n"));
        }
    } else {
        s.push_str("      - uses: actions/download-artifact@v4\n");
        s.push_str("        with:\n");
        if entry.matrix {
            s.push_str(&format!("          pattern: {slug}-*\n"));
            s.push_str(&format!("          path: .artifacts/{name}\n"));
            s.push_str("          merge-multiple: true\n");
        } else {
            s.push_str(&format!("          name: {slug}\n"));
            s.push_str("          path: .artifacts\n");
        }
        if entry.adapter == Ecosystem::Npm {
            s.push_str(&format!("      - run: {}\n", npm_tool.install_command()));
        }
    }
    push_install_otf_release(s);
    s.push_str("      - name: Publish\n");
    if inline {
        s.push_str(&format!(
            "        run: otf-release publish --package {name}\n"
        ));
    } else {
        s.push_str(&format!(
            "        run: otf-release publish --package {name} --artifacts-dir .artifacts\n"
        ));
    }
    s.push_str("        env:\n");
    match entry.adapter {
        Ecosystem::Npm => s.push_str("          NODE_AUTH_TOKEN: ${{ secrets.NPM_TOKEN }}\n"),
        Ecosystem::Cargo => {
            s.push_str("          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}\n")
        }
        Ecosystem::Jsr => s.push_str("          JSR_TOKEN: ${{ secrets.JSR_TOKEN }}\n"),
        Ecosystem::Generic => {}
    }
    s.push_str("          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}\n\n");
}

/// The GitHub Release job for a `build-only` package: install `otf-release` and hand off to
/// `otf-release github-release`, which reads the version, builds the notes, renames the staged
/// binaries into OS/arch assets, and creates the Release — all in the binary, idempotently. The
/// YAML stays a thin, stable call (no inline `gh`/`awk`/`jq`), exactly like the registry
/// `publish` job. No registry push.
fn render_github_release(s: &mut String, needs: &[String], entry: &PackageEntry) {
    s.push_str(&format!("  github-release-{}:\n", slug(&entry.name)));
    let mut actual_needs = vec!["check-release".to_string()];
    actual_needs.extend_from_slice(needs);
    needs_line(s, &actual_needs);
    s.push_str(&format!(
        "    if: needs.check-release.outputs.{} == 'true'\n",
        release_output(&entry.name)
    ));
    s.push_str("    runs-on: ubuntu-latest\n");
    s.push_str("    steps:\n");
    // `fetch-depth: 0` so the previous release tags are present for semantic-commit notes.
    s.push_str("      - uses: actions/checkout@v4\n        with:\n          fetch-depth: 0\n");
    let staged = download_artifacts(s, needs);
    push_install_otf_release(s);
    s.push_str("      - name: Create GitHub Release\n");
    s.push_str("        env:\n          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}\n");
    if staged {
        s.push_str(&format!(
            "        run: otf-release github-release --package {} --artifacts-dir .artifacts\n\n",
            entry.name
        ));
    } else {
        s.push_str(&format!(
            "        run: otf-release github-release --package {}\n\n",
            entry.name
        ));
    }
}

/// Prompt for a generic package's build/publish commands and assemble its [`PackageEntry`].
/// `name`/`manifest`/`version_field` are already known (imported from the scan or hand-entered);
/// a publish command makes it `publish` mode, otherwise build-only.
fn configure_generic(
    name: &str,
    manifest: &str,
    version_field: &str,
    kind: Option<&str>,
) -> Result<PackageEntry> {
    let mode = match Select::new(
        &format!("  {name} — mode:"),
        vec![
            "publish (to registry)",
            "build-only (standalone binaries on a GitHub Release)",
        ],
    )
    .with_help_message(MODE_HELP)
    .raw_prompt()?
    .index
    {
        1 => Mode::BuildOnly,
        _ => Mode::Publish,
    };

    let matrix = Select::new(
        &format!("  {name} — cross-compile a binary per platform?"),
        vec!["Yes", "No"],
    )
    .with_help_message(MATRIX_HELP)
    .raw_prompt()?
    .index
        == 0;
    let targets = if matrix {
        select_targets("  Target platforms:")?
    } else {
        Vec::new()
    };

    // `otf-release build` runs `rustup target add {triple}` itself and substitutes the placeholders,
    // so the commands here use `{triple}`/`{ext}`/`{bin}`, not GitHub `${{ matrix.* }}` expressions.
    let default_cmd = match (kind, matrix) {
        (Some("Rust / Cargo"), true) => "cargo build --release --target {triple}",
        (Some("Rust / Cargo"), false) => "cargo build --release",
        (Some("Node / npm"), _) => "npm run build",
        (Some("Deno / JSR"), _) => "deno task build",
        (Some("Python / PyPI"), _) => "python -m build",
        (Some("PHP / Packagist"), _) => "composer build",
        (Some("Gleam / Hex"), _) => "gleam build",
        (Some("Elixir / Hex"), _) => "mix build",
        _ => "",
    };
    let command = Text::new(&format!("  {name} — build command (optional):"))
        .with_default(default_cmd)
        .with_help_message(if matrix {
            COMMAND_HELP
        } else {
            "runs in CI before release; leave blank for none"
        })
        .prompt()?;

    let bin_name = if kind == Some("Rust / Cargo") {
        let n = Text::new(&format!("  {name} — binary name:"))
            .with_default(name)
            .with_help_message(BIN_NAME_HELP)
            .prompt()?;
        Some(n)
    } else {
        None
    };

    let default_artifacts = match (kind, matrix) {
        (Some("Rust / Cargo"), true) => "target/{triple}/release/{bin}{ext}".to_string(),
        (Some("Rust / Cargo"), false) => format!("target/release/{}", bin_name.as_deref().unwrap()),
        (Some("Node / npm"), _) => "dist/*".to_string(),
        _ => String::new(),
    };
    let artifacts = Text::new(&format!("  {name} — artifacts to stage (optional):"))
        .with_default(&default_artifacts)
        .with_help_message(if matrix {
            ARTIFACTS_HELP
        } else {
            "files to attach/stage on release"
        })
        .prompt()?;

    let publish = if mode == Mode::Publish {
        let cmd = Text::new(&format!("  {name} — publish command:"))
            .with_default("")
            .with_placeholder("e.g. npx jsr publish")
            .with_help_message("the command CI runs to push this package to its registry")
            .prompt()?;
        (!cmd.trim().is_empty()).then_some(cmd)
    } else {
        None
    };

    // Build-only packaging: archive the staged binaries and/or emit a checksums file, like the
    // hand-written release scripts this replaces. `github-release` reads these; the workflow YAML is
    // unchanged (a thin call).
    let (archive, checksums, include) = if mode == Mode::BuildOnly {
        let archive = match Select::new(
            &format!("  {name} — package binaries as archives?"),
            vec![
                "No — attach the raw binaries",
                "auto (.tar.gz on Unix, .zip on Windows)",
                "tar.gz for every target",
                "zip for every target",
            ],
        )
        .with_help_message("an archive bundles the binary (and any extra files) per platform")
        .raw_prompt()?
        .index
        {
            1 => Some(ArchiveFormat::Auto),
            2 => Some(ArchiveFormat::TarGz),
            3 => Some(ArchiveFormat::Zip),
            _ => None,
        };
        let include = if archive.is_some() {
            let raw = Text::new(&format!(
                "  {name} — extra files to bundle in each archive (optional):"
            ))
            .with_default("")
            .with_placeholder("e.g. README.md LICENSE types/*.d.ts")
            .with_help_message(
                "space-separated repo-relative paths or globs, added beside the binary",
            )
            .prompt()?;
            raw.split_whitespace().map(str::to_string).collect()
        } else {
            Vec::new()
        };
        let checksums = Select::new(
            &format!("  {name} — also attach a checksums.txt (SHA-256)?"),
            vec!["Yes", "No"],
        )
        .with_help_message("one combined checksums.txt covering every asset on the release")
        .raw_prompt()?
        .index
            == 0;
        (archive, checksums, include)
    } else {
        (None, false, Vec::new())
    };

    Ok(PackageEntry {
        name: name.to_string(),
        adapter: Ecosystem::Generic,
        mode,
        matrix,
        targets,
        command,
        artifacts,
        bin_name,
        compress: None,
        manifest: Some(manifest.to_string()),
        version_field: Some(version_field.to_string()),
        publish,
        archive,
        checksums,
        include,
    })
}

/// The real terminal prompt for `init` — arrow-key select, spacebar multi-select, confirm.
pub struct StdinInitPrompt;

const MULTI_HELP: &str = "↑↓ move · space toggle · enter confirm";
const SELECT_HELP: &str = "↑↓ move · enter select";

const BUILD_PKGS_HELP: &str =
    "select packages that must produce artifacts first — for example a prebuilt binary, generated \
     dist files, or a bundled CLI. Packages you don't pick are published as-is. ↑↓ move · space toggle · enter confirm";
const MODE_HELP: &str =
    "publish → push to the registry  ·  build-only → standalone binaries on a GitHub Release (no registry)";
const MATRIX_HELP: &str =
    "Yes → cross-compile one binary per OS/arch (Rust, Go, …), staged per platform  ·  No → a single build";
const BIN_NAME_HELP: &str =
    "the compiled executable's base name; staged at bin/<platform>-<arch>/<name> inside the package";
const COMMAND_HELP: &str =
    "runs in CI for each target; {triple} {ext} {bin} are substituted per platform";
const ARTIFACTS_HELP: &str =
    "path to the binary the command produced; {triple} {ext} {bin} expand per target";
const TAG_FORMAT_HELP: &str =
    "e.g. v{version} (single package) or {name}@{version} (per-package tags in a monorepo)";
const CHANGELOG_SCOPE_HELP: &str =
    "Root → one shared CHANGELOG.md  ·  Per-package → each package keeps its own (best for monorepos)";
const NOTES_HELP: &str =
    "how the GitHub Release body is filled: auto (from PRs/commits), your CHANGELOG, or a commit list";

impl InitPrompt for StdinInitPrompt {
    fn select_adapters(&self) -> Result<Vec<Ecosystem>> {
        let labels: Vec<&str> = Ecosystem::ALL.iter().map(|e| e.label()).collect();
        let chosen = MultiSelect::new("Adapters to enable:", labels)
            .with_help_message(
                "the ecosystems/registries this repo releases to; pick all that apply. \
                 space toggles · enter confirm",
            )
            .raw_prompt()?;
        Ok(chosen.iter().map(|o| Ecosystem::ALL[o.index]).collect())
    }

    fn prompt_jsr_scaffold(
        &self,
        default_name: &str,
        _default_version: &str,
        default_exports: &str,
    ) -> Result<(String, String)> {
        use inquire::Text;
        let name = Text::new("JSR package name (e.g. @scope/name):")
            .with_default(default_name)
            .prompt()?;
        let exports = Text::new("JSR exports entrypoint (e.g. ./src/index.ts):")
            .with_default(default_exports)
            .prompt()?;
        Ok((name, exports))
    }

    fn select_build_packages(&self, publishable: &[&Pkg]) -> Result<Vec<String>> {
        if publishable.is_empty() {
            return Ok(Vec::new());
        }
        let labels: Vec<String> = publishable.iter().map(|p| p.name.clone()).collect();
        let chosen = MultiSelect::new(
            "Which packages need built artifacts before publish?",
            labels,
        )
        .with_help_message(BUILD_PKGS_HELP)
        .raw_prompt()?;
        Ok(chosen
            .iter()
            .map(|o| publishable[o.index].name.clone())
            .collect())
    }

    fn build_entry(&self, pkg_name: &str, enabled: &[Ecosystem]) -> Result<PackageEntry> {
        let adapter = if enabled.len() == 1 {
            enabled[0]
        } else {
            let labels: Vec<&str> = enabled.iter().map(|e| e.label()).collect();
            let opt = Select::new(&format!("{pkg_name} — adapter:"), labels)
                .with_help_message("which registry/ecosystem this package is released through")
                .raw_prompt()?;
            enabled[opt.index]
        };

        // An npm package is always published to the registry — its prebuilt binaries ship *inside*
        // the tarball, so "build-only" (= GitHub Release assets, no registry push) never applies.
        // Only cargo/generic packages, which can be distributed as standalone binaries, get the
        // choice.
        let mode = if adapter == Ecosystem::Npm {
            Mode::Publish
        } else {
            match Select::new(
                &format!("{pkg_name} — mode:"),
                vec![
                    "publish (to registry)",
                    "build-only (standalone binaries on a GitHub Release)",
                ],
            )
            .with_help_message(MODE_HELP)
            .raw_prompt()?
            .index
            {
                1 => Mode::BuildOnly,
                _ => Mode::Publish,
            }
        };

        let matrix = Select::new(
            &format!("{pkg_name} — cross-compile a binary per platform?"),
            vec!["Yes", "No"],
        )
        .with_help_message(MATRIX_HELP)
        .raw_prompt()?
        .index
            == 0;
        let targets = if matrix {
            select_targets("Target triples:")?
        } else {
            Vec::new()
        };

        // A matrix package compiles one binary per target; ask its name and template the build so
        // `otf-release build` can fill `{triple}`/`{ext}`/`{bin}` per target. An npm matrix package
        // decompresses its staged binary at install time, so default to brotli; Release assets
        // (build-only) ship raw.
        let (bin_name, compress, default_cmd, default_artifacts) = if matrix {
            let bin = Text::new(&format!("{pkg_name} — binary name:"))
                .with_default(&slug(pkg_name))
                .with_help_message(BIN_NAME_HELP)
                .prompt()?;
            let compress = (adapter == Ecosystem::Npm).then(|| "brotli".to_string());
            let cmd = if adapter == Ecosystem::Generic {
                ""
            } else {
                "cargo build --release --target {triple}"
            };
            (
                Some(bin),
                compress,
                cmd.to_string(),
                "target/{triple}/release/{bin}{ext}".to_string(),
            )
        } else {
            let cmd = match adapter {
                Ecosystem::Cargo => "cargo build --release",
                Ecosystem::Npm => "npm run build",
                Ecosystem::Jsr => "deno task build",
                Ecosystem::Generic => "",
            };
            (None, None, cmd.to_string(), String::new())
        };
        let command = Text::new(&format!("{pkg_name} — build command:"))
            .with_default(&default_cmd)
            .with_help_message(if matrix {
                COMMAND_HELP
            } else {
                "runs in CI before publish (e.g. a bundler). Leave blank if no build is needed."
            })
            .prompt()?;
        let artifacts = Text::new(&format!("{pkg_name} — artifacts to stage:"))
            .with_default(&default_artifacts)
            .with_help_message(if matrix {
                ARTIFACTS_HELP
            } else {
                "files to include when publishing (e.g. dist/**). Optional."
            })
            .prompt()?;

        Ok(PackageEntry {
            name: pkg_name.to_string(),
            adapter,
            mode,
            matrix,
            targets,
            command,
            artifacts,
            bin_name,
            compress,
            manifest: None,
            version_field: None,
            publish: None,
            archive: None,
            checksums: false,
            include: Vec::new(),
        })
    }

    fn generic_packages(&self, found: &[GenericCandidate]) -> Result<Vec<PackageEntry>> {
        let mut out = Vec::new();

        // 1. Import from what the repo scan inferred.
        if !found.is_empty() {
            let labels: Vec<String> = found.iter().map(GenericCandidate::label).collect();
            let chosen = MultiSelect::new("Detected packages to import:", labels)
                .with_help_message(MULTI_HELP)
                .raw_prompt()?;
            for opt in chosen {
                let c = &found[opt.index];
                out.push(configure_generic(
                    &c.name,
                    &c.manifest,
                    &c.version_field,
                    Some(c.kind),
                )?);
            }
        }

        // 2. Add any the scan missed (or all of them, if nothing was detected) by hand.
        loop {
            let question = if found.is_empty() {
                "Add a generic package?"
            } else {
                "Add another package by hand?"
            };
            if Select::new(question, vec!["Yes", "No"])
                .with_help_message(SELECT_HELP)
                .raw_prompt()?
                .index
                == 1
            {
                break;
            }
            let name = Text::new("  name:")
                .with_placeholder("@scope/pkg or my-tool")
                .with_help_message("the package name; also used in tags and the changelog")
                .prompt()?;
            let manifest = Text::new("  manifest file holding the version:")
                .with_placeholder("deno.json")
                .with_help_message("the file the version is read from and bumped in")
                .prompt()?;
            let version_field = Text::new("  version field:")
                .with_default(DEFAULT_VERSION_FIELD)
                .with_help_message(
                    "key inside the manifest; dot-paths like workspace.package.version work",
                )
                .prompt()?;
            out.push(configure_generic(&name, &manifest, &version_field, None)?);
        }
        Ok(out)
    }

    fn confirm_overwrite(&self, path: &Path) -> Result<bool> {
        Ok(Select::new(
            &format!("{} already exists. Overwrite?", path.display()),
            vec!["No", "Yes"],
        )
        .with_help_message(
            "regenerates this file from your answers; your other files are untouched",
        )
        .raw_prompt()?
        .index
            == 1)
    }

    fn tag_format(&self, suggestion: &TagFormatSuggestion) -> Result<String> {
        let help = match &suggestion.detected_format {
            Some(format) => format!(
                "detected existing tags like {format}; edit to migrate, old format will be kept as legacy history"
            ),
            None => TAG_FORMAT_HELP.to_string(),
        };
        let mut choices: Vec<String> = COMMON_TAG_FORMATS
            .iter()
            .map(|format| {
                if *format == suggestion.default_format {
                    format!("{format} (suggested)")
                } else {
                    (*format).to_string()
                }
            })
            .collect();
        choices.push("Custom".to_string());
        let default = COMMON_TAG_FORMATS
            .iter()
            .position(|format| *format == suggestion.default_format)
            .unwrap_or(0);
        let selected = Select::new("Git tag format:", choices)
            .with_starting_cursor(default)
            .with_help_message(&help)
            .prompt()?;
        if selected == "Custom" {
            Ok(Text::new("Custom git tag format:")
                .with_default(&suggestion.default_format)
                .with_help_message(TAG_FORMAT_HELP)
                .prompt()?)
        } else {
            Ok(selected
                .strip_suffix(" (suggested)")
                .unwrap_or(&selected)
                .to_string())
        }
    }

    fn prompt_provider(&self) -> Result<String> {
        loop {
            let ans = Select::new(
                "Which Git hosting provider do you use?",
                vec![
                    "GitHub",
                    "GitLab (Coming Soon)",
                    "Bitbucket (Coming Soon)",
                    "Gitea (Coming Soon)",
                    "Codeberg (Coming Soon)",
                ],
            )
            .with_help_message("only GitHub is fully supported today")
            .prompt()?;

            if ans == "GitHub" {
                return Ok("github".to_string());
            } else {
                println!("Only GitHub is fully supported at this moment. Please select GitHub.");
            }
        }
    }

    fn prompt_changelog_scope(&self) -> Result<ChangelogScope> {
        let ans = Select::new(
            "Where should release notes be maintained?",
            vec!["Root CHANGELOG.md", "Per-package CHANGELOG.md files"],
        )
        .with_help_message(CHANGELOG_SCOPE_HELP)
        .prompt()?;

        if ans.starts_with("Root") {
            Ok(ChangelogScope::Root)
        } else {
            Ok(ChangelogScope::Package)
        }
    }

    fn prompt_github_release_notes(&self) -> Result<GithubReleaseNotes> {
        let ans = Select::new(
            "What should GitHub Release descriptions contain?",
            vec![
                "Auto-generate with GitHub release notes",
                "Copy from the configured changelog",
                "Semantic-style commit list since the last matching tag",
            ],
        )
        .with_help_message(NOTES_HELP)
        .prompt()?;

        if ans.starts_with("Copy") {
            Ok(GithubReleaseNotes::CuratedChangelog)
        } else if ans.starts_with("Semantic") {
            Ok(GithubReleaseNotes::SemanticCommits)
        } else {
            Ok(GithubReleaseNotes::AutoGenerate)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    struct FakeAdapter {
        packages: Vec<Pkg>,
    }
    impl Adapter for FakeAdapter {
        fn discover_packages(&self) -> Result<Vec<Pkg>> {
            Ok(self.packages.clone())
        }
        fn write_version(&self, _: &Pkg, _: &str) -> Result<()> {
            unreachable!()
        }
        fn update_dep_range(&self, _: &Pkg, _: &str, _: &str) -> Result<()> {
            unreachable!()
        }
        fn format_range(&self, _: &str) -> String {
            unreachable!()
        }
        fn resolve_workspace_links(&self, _: &Pkg) -> Result<()> {
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
        fn is_published(&self, _: &Pkg, _: &str) -> Result<bool> {
            unreachable!()
        }
        fn publish(&self, _: &Pkg, _: Option<&Path>) -> Result<()> {
            unreachable!()
        }
        // Model an npm package that declares a `build` script, so the npm auto-path injects an
        // inline build. `strip_publish_hooks` keeps the default (removes nothing).
        fn build_command(&self, _: &Pkg) -> Result<Option<String>> {
            Ok(Some("npm run build".to_string()))
        }
    }

    /// A factory returning a fixed package set for every ecosystem.
    struct FakeFactory {
        packages: Vec<Pkg>,
    }
    impl AdapterFactory for FakeFactory {
        fn make(&self, _: Ecosystem) -> Box<dyn Adapter> {
            Box::new(FakeAdapter {
                packages: self.packages.clone(),
            })
        }
    }

    #[derive(Default)]
    struct FakePrompt {
        adapters: Vec<Ecosystem>,
        build_names: Vec<String>,
        entries: Vec<PackageEntry>,
        generic: Vec<PackageEntry>,
        overwrite: bool,
        tag_format: Option<String>,
    }
    impl InitPrompt for FakePrompt {
        fn select_adapters(&self) -> Result<Vec<Ecosystem>> {
            Ok(self.adapters.clone())
        }
        fn prompt_jsr_scaffold(
            &self,
            default_name: &str,
            _default_version: &str,
            default_exports: &str,
        ) -> Result<(String, String)> {
            Ok((default_name.to_string(), default_exports.to_string()))
        }
        fn select_build_packages(&self, _: &[&Pkg]) -> Result<Vec<String>> {
            Ok(self.build_names.clone())
        }
        fn build_entry(&self, name: &str, _: &[Ecosystem]) -> Result<PackageEntry> {
            Ok(self
                .entries
                .iter()
                .find(|e| e.name == name)
                .cloned()
                .unwrap())
        }
        fn generic_packages(&self, _: &[GenericCandidate]) -> Result<Vec<PackageEntry>> {
            Ok(self.generic.clone())
        }
        fn confirm_overwrite(&self, _: &Path) -> Result<bool> {
            Ok(self.overwrite)
        }
        fn tag_format(&self, suggestion: &TagFormatSuggestion) -> Result<String> {
            Ok(self
                .tag_format
                .clone()
                .unwrap_or_else(|| suggestion.default_format.clone()))
        }
        fn prompt_provider(&self) -> Result<String> {
            Ok("github".to_string())
        }
        fn prompt_changelog_scope(&self) -> Result<ChangelogScope> {
            Ok(ChangelogScope::Package)
        }
        fn prompt_github_release_notes(&self) -> Result<GithubReleaseNotes> {
            Ok(GithubReleaseNotes::AutoGenerate)
        }
    }

    fn pkg(name: &str, publishable: bool) -> Pkg {
        Pkg {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            manifest_path: PathBuf::from(format!("{name}/Cargo.toml")),
            changelog_path: PathBuf::from(format!("{name}/CHANGELOG.md")),
            publishable,
            internal_deps: vec![],
        }
    }

    fn npm_pkg(name: &str, manifest_path: &str) -> Pkg {
        Pkg {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            manifest_path: PathBuf::from(manifest_path),
            changelog_path: Path::new(manifest_path)
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("CHANGELOG.md"),
            publishable: true,
            internal_deps: vec![],
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn infers_tag_format_from_existing_tag_shapes() {
        let package_tags = vec![
            "@opentf/create-web@0.5.0".to_string(),
            "@opentf/web@0.5.0".to_string(),
            "@opentf/web@0.6.0-alpha.1".to_string(),
        ];
        assert_eq!(
            infer_tag_format(&package_tags).as_deref(),
            Some("{name}@{version}")
        );

        let package_v_tags = vec!["@opentf/web@v0.5.0".to_string()];
        assert_eq!(
            infer_tag_format(&package_v_tags).as_deref(),
            Some("{name}@v{version}")
        );

        let single_v_tags = vec!["v1.2.3".to_string(), "v1.3.0".to_string()];
        assert_eq!(
            infer_tag_format(&single_v_tags).as_deref(),
            Some("v{version}")
        );

        let single_plain_tags = vec!["1.2.3".to_string()];
        assert_eq!(
            infer_tag_format(&single_plain_tags).as_deref(),
            Some("{version}")
        );
    }

    #[test]
    fn suggests_package_scoped_tags_for_new_multi_package_repos() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            suggest_tag_format(tmp.path(), 2).default_format,
            "{name}@{version}"
        );
        assert_eq!(
            suggest_tag_format(tmp.path(), 1).default_format,
            DEFAULT_TAG_FORMAT
        );
    }

    fn cargo_build_only(name: &str) -> PackageEntry {
        PackageEntry {
            name: name.to_string(),
            adapter: Ecosystem::Cargo,
            mode: Mode::BuildOnly,
            matrix: true,
            targets: vec![
                crate::config::Target::resolved("linux", "x86_64"),
                crate::config::Target::resolved("windows", "x86_64"),
            ],
            command: "cargo build --release -p otf-release --target {triple}".into(),
            artifacts: "target/{triple}/release/otf-release{ext}".into(),
            bin_name: Some("otf-release".into()),
            compress: None,
            manifest: None,
            version_field: None,
            publish: None,
            archive: None,
            checksums: false,
            include: Vec::new(),
        }
    }

    fn npm_publish(name: &str) -> PackageEntry {
        PackageEntry {
            name: name.to_string(),
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
            archive: None,
            checksums: false,
            include: Vec::new(),
        }
    }

    fn generic_pkg(name: &str, publish: Option<&str>) -> PackageEntry {
        PackageEntry {
            name: name.to_string(),
            adapter: Ecosystem::Generic,
            mode: if publish.is_some() {
                Mode::Publish
            } else {
                Mode::BuildOnly
            },
            matrix: false,
            targets: vec![],
            command: "deno task build".into(),
            artifacts: "dist/*".into(),
            bin_name: None,
            compress: None,
            manifest: Some("deno.json".into()),
            version_field: Some("version".into()),
            publish: publish.map(|s| s.into()),
            archive: None,
            checksums: false,
            include: Vec::new(),
        }
    }

    #[test]
    fn slug_is_job_safe() {
        assert_eq!(slug("@x/cli"), "x-cli");
        assert_eq!(slug("opentf-release"), "opentf-release");
        assert_eq!(slug("web_compiler"), "web-compiler");
    }

    #[test]
    fn npm_only_renders_publish_job_no_release() {
        let config = ReleaseConfig {
            snapshot_tag: None,
            tag_format: "{name}@{version}".to_string(),
            legacy_tag_formats: Vec::new(),
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            hooks: crate::config::Hooks::default(),
            publish: crate::config::PublishConfig::default(),
            adapters: vec![Ecosystem::Npm],
            skip_publish: Vec::new(),
            packages: vec![],
        };
        let out = render_workflow(&config);
        assert!(out.contains("permissions:\n  contents: write  # create tags and GitHub Releases\n  id-token: write\n"));
        assert!(out.contains("  publish:\n"));
        assert!(out.contains("      - uses: actions/setup-node@v4\n"));
        assert!(out.contains("          node-version: 24\n"));
        // The gate delegates to the binary — no hand-rolled inline version reads in the YAML.
        assert!(out.contains("should_release=$(otf-release check)"));
        assert!(!out.contains("version=\"$(node -p"));
        assert!(!out.contains("version=\"$(cargo metadata"));
        assert!(out.contains("      - name: Install otf-release\n"));
        assert!(out.contains("        run: otf-release publish\n"));
        // No build steps, so no needs and no artifact download.
        assert!(out.contains("needs: [check-release]"));
        assert!(!out.contains("github-release"));
    }

    #[test]
    fn ubuntu_only_workflow_has_no_dead_windows_install_step_and_serializes_releases() {
        let config = ReleaseConfig {
            snapshot_tag: None,
            tag_format: "{name}@{version}".to_string(),
            legacy_tag_formats: Vec::new(),
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            hooks: crate::config::Hooks::default(),
            publish: crate::config::PublishConfig::default(),
            adapters: vec![Ecosystem::Npm],
            skip_publish: Vec::new(),
            packages: vec![],
        };
        let out = render_workflow(&config);
        // Release runs are serialized so two quick pushes don't publish concurrently.
        assert!(out.contains("\nconcurrency:\n  group: release\n  cancel-in-progress: false\n"));
        // No job here runs on Windows, so the PowerShell install branch is not emitted at all.
        assert!(out.contains("      - name: Install otf-release\n        shell: bash\n"));
        assert!(!out.contains("if: runner.os == 'Windows'"));
        assert!(!out.contains("install.ps1"));
    }

    #[test]
    fn catch_all_publish_waits_for_dedicated_publish_jobs() {
        // A dependent (web-cli) that exact-pins a package built + published by its own job
        // (web-compiler) must not publish until that job succeeds — or the pin dangles on the
        // registry pointing at a version that does not exist yet (or never will).
        let config = ReleaseConfig {
            snapshot_tag: None,
            tag_format: "{name}@{version}".to_string(),
            legacy_tag_formats: Vec::new(),
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            hooks: crate::config::Hooks::default(),
            publish: crate::config::PublishConfig::default(),
            adapters: vec![Ecosystem::Npm],
            skip_publish: Vec::new(),
            packages: vec![PackageEntry {
                name: "@opentf/web-compiler".into(),
                adapter: Ecosystem::Npm,
                mode: Mode::Publish,
                matrix: true,
                targets: vec![Target::resolved("linux", "aarch64")],
                command: "cargo build --release --target {triple}".into(),
                artifacts: "target/{triple}/release/otfwc{ext}".into(),
                bin_name: Some("otfwc".into()),
                compress: Some("brotli".into()),
                manifest: None,
                version_field: None,
                publish: None,
                archive: None,
                checksums: false,
                include: Vec::new(),
            }],
        };
        let out = render_workflow(&config);
        // The catch-all publish depends on the compiler's dedicated publish job…
        assert!(
            out.contains("  publish:\n    needs: [check-release, publish-opentf-web-compiler]\n")
        );
        // …and gates on it: `always()` so a skipped compiler (JS-only release) still lets JS publish,
        // with result guards that abort only on a real failure/cancellation.
        assert!(out.contains(
            "    if: >-\n      always() &&\n      needs.check-release.outputs.should_release == 'true' &&\n      needs.publish-opentf-web-compiler.result != 'failure' &&\n      needs.publish-opentf-web-compiler.result != 'cancelled'\n"
        ));
    }

    #[test]
    fn jsr_only_renders_publish_job_no_release() {
        let config = ReleaseConfig {
            snapshot_tag: None,
            tag_format: "{name}@{version}".to_string(),
            legacy_tag_formats: Vec::new(),
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            hooks: crate::config::Hooks::default(),
            publish: crate::config::PublishConfig::default(),
            adapters: vec![Ecosystem::Jsr],
            skip_publish: Vec::new(),
            packages: vec![
                PackageEntry {
                    name: "jsr-with-build".to_string(),
                    adapter: Ecosystem::Jsr,
                    mode: Mode::Publish,
                    matrix: false,
                    targets: Vec::new(),
                    command: "deno task build".to_string(),
                    artifacts: String::new(),
                    bin_name: None,
                    compress: None,
                    manifest: Some("packages/b/deno.json".to_string()),
                    version_field: None,
                    publish: None,
                    archive: None,
                    checksums: false,
                    include: Vec::new(),
                },
                PackageEntry {
                    name: "jsr-no-build".to_string(),
                    adapter: Ecosystem::Jsr,
                    mode: Mode::Publish,
                    matrix: false,
                    targets: Vec::new(),
                    command: String::new(),
                    artifacts: String::new(),
                    bin_name: None,
                    compress: None,
                    manifest: Some("packages/a/deno.json".to_string()),
                    version_field: None,
                    publish: None,
                    archive: None,
                    checksums: false,
                    include: Vec::new(),
                },
            ],
        };
        let out = render_workflow(&config);
        assert!(out.contains("permissions:\n  contents: write  # create tags and GitHub Releases\n  id-token: write\n"));
        // Check package-specific publish job for jsr-with-build
        assert!(out.contains("  publish-jsr-with-build:\n"));
        assert!(out.contains("      - uses: denoland/setup-deno@v1\n"));
        assert!(out.contains("        run: otf-release publish --package jsr-with-build\n"));
        // Check catch-all publish job for jsr-no-build
        assert!(out.contains("  publish:\n"));
        assert!(out.contains("      - uses: denoland/setup-deno@v1\n"));
        assert!(out.contains("        run: otf-release publish --exclude-package jsr-with-build\n"));
        assert!(out.contains("          JSR_TOKEN: ${{ secrets.JSR_TOKEN }}\n"));
    }

    #[test]
    fn npm_workflow_uses_detected_bun_lockfile() {
        let config = ReleaseConfig {
            snapshot_tag: None,
            tag_format: "{name}@{version}".to_string(),
            legacy_tag_formats: Vec::new(),
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            hooks: crate::config::Hooks::default(),
            publish: crate::config::PublishConfig::default(),
            adapters: vec![Ecosystem::Npm],
            skip_publish: Vec::new(),
            packages: vec![npm_publish("docs-site")],
        };
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("bun.lock"), "").unwrap();

        let out = render_workflow_for_root(&config, tmp.path());

        assert!(out.contains("      - uses: oven-sh/setup-bun@v2\n"));
        assert!(out.contains("      - uses: actions/setup-node@v4\n"));
        assert!(out.contains("          registry-url: https://registry.npmjs.org\n"));
        assert!(out.contains("      - run: bun install --frozen-lockfile\n"));
        assert!(!out.contains("      - run: npm ci\n"));
    }

    #[test]
    fn npm_tool_detection_prefers_bun_then_other_lockfiles() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(NpmTool::detect(tmp.path()), NpmTool::Npm);

        std::fs::write(tmp.path().join("yarn.lock"), "").unwrap();
        assert_eq!(NpmTool::detect(tmp.path()), NpmTool::Yarn);

        std::fs::write(tmp.path().join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(NpmTool::detect(tmp.path()), NpmTool::Pnpm);

        std::fs::write(tmp.path().join("bun.lockb"), "").unwrap();
        assert_eq!(NpmTool::detect(tmp.path()), NpmTool::Bun);
    }

    #[test]
    fn pnpm_and_yarn_workflows_do_not_use_corepack() {
        let config = ReleaseConfig {
            snapshot_tag: None,
            tag_format: "{name}@{version}".to_string(),
            legacy_tag_formats: Vec::new(),
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            hooks: crate::config::Hooks::default(),
            publish: crate::config::PublishConfig::default(),
            adapters: vec![Ecosystem::Npm],
            skip_publish: Vec::new(),
            packages: vec![npm_publish("docs-site")],
        };

        let pnpm = render_workflow_with_npm_tool(&config, NpmTool::Pnpm);
        assert!(pnpm.contains("      - uses: pnpm/action-setup@v4\n"));
        assert!(pnpm.contains("      - uses: actions/setup-node@v4\n"));
        assert!(pnpm.contains("          registry-url: https://registry.npmjs.org\n"));
        assert!(pnpm.contains("      - run: pnpm install --frozen-lockfile\n"));
        assert!(!pnpm.contains("corepack"));

        let yarn = render_workflow_with_npm_tool(&config, NpmTool::Yarn);
        assert!(yarn.contains("      - uses: actions/setup-node@v4\n"));
        assert!(yarn.contains("          registry-url: https://registry.npmjs.org\n"));
        assert!(yarn.contains("      - run: yarn install --immutable\n"));
        assert!(!yarn.contains("corepack"));
    }

    #[test]
    fn cargo_build_only_renders_github_release_no_registry() {
        let config = ReleaseConfig {
            snapshot_tag: None,
            tag_format: "{name}@{version}".to_string(),
            legacy_tag_formats: Vec::new(),
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            hooks: crate::config::Hooks::default(),
            publish: crate::config::PublishConfig::default(),
            adapters: vec![Ecosystem::Cargo],
            skip_publish: Vec::new(),
            packages: vec![cargo_build_only("opentf-release")],
        };
        let out = render_workflow(&config);
        // A dynamic matrix emitted from release.toml (no hand-maintained, `# edit me` target list).
        assert!(out.contains("  matrix-opentf-release:\n"));
        assert!(out.contains("        run: echo \"matrix=$(otf-release matrix --package opentf-release)\" >> \"$GITHUB_OUTPUT\"\n"));
        assert!(out.contains("  build-opentf-release:\n"));
        assert!(out.contains("    needs: [check-release, matrix-opentf-release]\n"));
        assert!(out.contains("    runs-on: ${{ matrix.runner }}\n"));
        assert!(out.contains(
            "      matrix: ${{ fromJSON(needs.matrix-opentf-release.outputs.matrix) }}\n"
        ));
        // The tool drives the build + staging per target; no `# edit me`, no inline triple list.
        assert!(out.contains("        run: otf-release build --package opentf-release --target ${{ matrix.name }}/${{ matrix.arch }}\n"));
        assert!(!out.contains("      - name: Install cross toolchain\n"));
        assert!(!out.contains("# edit me: cross-compile"));
        assert!(!out.contains("# edit me: choose a runner"));
        assert!(!out.contains("rust_target"));
        // Ships via a GitHub Release, idempotently — no registry, no cargo publish.
        assert!(out.contains("permissions:\n  contents: write"));
        assert!(out.contains("  github-release-opentf-release:\n"));
        assert!(out.contains("    needs: [check-release, build-opentf-release]\n"));
        // The release job is a thin call into the binary — the tool reads the version, builds the
        // notes, renames the staged binaries, and creates the release. No inline gh/awk/jq/flatten.
        assert!(out.contains("        run: otf-release github-release --package opentf-release --artifacts-dir .artifacts\n"));
        assert!(!out.contains("gh release create"));
        assert!(!out.contains("gh release view"));
        assert!(!out.contains("flat-artifacts"));
        assert!(!out.contains("asset_name="));
        assert!(!out.contains("cargo metadata --no-deps"));
        assert!(!out.contains("tag=\"v${{ needs.check-release.outputs.version }}\""));
        // check-release delegates the "is anything to release?" decision to the binary, and needs
        // full tag history (`fetch-depth: 0`) to compare against.
        assert!(out.contains(
            "  check-release:\n    runs-on: ubuntu-latest\n    outputs:\n      should_release:"
        ));
        assert!(
            out.contains("should_release=$(otf-release check --exclude-package opentf-release)")
        );
        assert!(!out.contains("git ls-remote"));
        assert!(!out.contains("cargo publish"));
        assert!(!out.contains("crates.io"));
        // build-only cargo: no publish job at all.
        assert!(!out.contains("  publish:\n"));
    }

    #[test]
    fn npm_matrix_build_only_still_publishes_with_binaries() {
        // build-only is meaningless for an npm matrix package: its per-platform binaries ship
        // inside the npm tarball, not as GitHub Release assets. So it must route to publish.
        let config = ReleaseConfig {
            snapshot_tag: None,
            tag_format: "v{version}".to_string(),
            legacy_tag_formats: Vec::new(),
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            hooks: crate::config::Hooks::default(),
            publish: crate::config::PublishConfig::default(),
            adapters: vec![Ecosystem::Npm],
            skip_publish: Vec::new(),
            packages: vec![PackageEntry {
                name: "@opentf/web-compiler".into(),
                adapter: Ecosystem::Npm,
                mode: Mode::BuildOnly, // ← the bug: an npm matrix package set build-only
                matrix: true,
                targets: vec![Target::resolved("linux", "aarch64")],
                command: "cargo build --release --target {triple}".into(),
                artifacts: "target/{triple}/release/otfwc{ext}".into(),
                bin_name: Some("otfwc".into()),
                compress: Some("brotli".into()),
                manifest: None,
                version_field: None,
                publish: None,
                archive: None,
                checksums: false,
                include: Vec::new(),
            }],
        };
        let out = render_workflow(&config);
        assert!(out.contains("      - name: Install cross toolchain\n"));
        assert!(out.contains("        if: ${{ matrix.cross }}\n"));
        // The binaries flow to publish (needs build, merges artifacts, runs --artifacts-dir)…
        assert!(out.contains("  publish:\n"));
        assert!(out.contains("  publish-opentf-web-compiler:\n"));
        assert!(out.contains("    needs: [check-release, build-opentf-web-compiler]\n"));
        assert!(out.contains("          pattern: opentf-web-compiler-*\n"));
        assert!(out.contains("          path: .artifacts/@opentf/web-compiler\n"));
        assert!(out.contains("        run: otf-release publish --package @opentf/web-compiler --artifacts-dir .artifacts\n"));
        // …and NOT to a cosmetic GitHub Release of raw binaries.
        assert!(!out.contains("  github-release:\n"));
        // A generated npm version read is confident — no stray `# edit me` hint.
        assert!(!out.contains("# edit me: where the version lives"));
    }

    #[test]
    fn npm_matrix_publish_stages_binaries_under_node_platform_dirs() {
        let config = ReleaseConfig {
            snapshot_tag: None,
            tag_format: "{name}@{version}".to_string(),
            legacy_tag_formats: Vec::new(),
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            hooks: crate::config::Hooks::default(),
            publish: crate::config::PublishConfig::default(),
            adapters: vec![Ecosystem::Npm],
            skip_publish: Vec::new(),
            packages: vec![PackageEntry {
                name: "@opentf/web-compiler".into(),
                adapter: Ecosystem::Npm,
                mode: Mode::Publish,
                matrix: true,
                targets: vec![
                    Target::resolved("linux", "aarch64"),
                    Target::resolved("windows", "x86_64"),
                ],
                command: "cargo build --release --target {triple}".into(),
                artifacts: "target/{triple}/release/otfwc{ext}".into(),
                bin_name: Some("otfwc".into()),
                compress: Some("brotli".into()),
                manifest: None,
                version_field: None,
                publish: None,
                archive: None,
                checksums: false,
                include: Vec::new(),
            }],
        };
        let out = render_workflow(&config);

        // A matrix npm package builds a Rust binary, so both toolchains are set up in the fan-out.
        assert!(out.contains("  matrix-opentf-web-compiler:\n"));
        assert!(out.contains("  build-opentf-web-compiler:\n"));
        assert!(out.contains(
            "release_opentf_web_compiler=$(otf-release check --package @opentf/web-compiler)"
        ));
        assert!(
            out.contains("if: needs.check-release.outputs.release_opentf_web_compiler == 'true'")
        );
        assert!(out.contains("      - uses: dtolnay/rust-toolchain@stable\n"));
        assert!(out.contains("          targets: ${{ matrix.triple }}\n"));
        assert!(out.contains("      - uses: actions/setup-node@v4\n"));
        assert!(out.contains("        if: runner.os != 'Windows'\n"));
        assert!(out.contains("        run: curl -fsSL https://raw.githubusercontent.com/Open-Tech-Foundation/release/main/install.sh | bash\n"));
        assert!(out.contains("        if: runner.os == 'Windows'\n"));
        assert!(out.contains("        run: irm https://raw.githubusercontent.com/Open-Tech-Foundation/release/main/install.ps1 | iex\n"));
        assert!(out
            .contains("        run: otf-release build --package @opentf/web-compiler --target ${{ matrix.name }}/${{ matrix.arch }}\n"));

        // The publish job merges each target's artifact back into `.artifacts/<package>` so the
        // staged `bin/<stage_as>/…` tree is whole before packing — the load-bearing fix.
        assert!(out.contains("  publish-opentf-web-compiler:\n"));
        assert!(out.contains("          pattern: opentf-web-compiler-*\n"));
        assert!(out.contains("          path: .artifacts/@opentf/web-compiler\n"));
        assert!(out.contains("          merge-multiple: true\n"));
        assert!(out.contains("        run: otf-release publish --package @opentf/web-compiler --artifacts-dir .artifacts\n"));
        assert!(out.contains("run: otf-release publish --exclude-package @opentf/web-compiler\n"));
        // Hygiene: the npm auth secret is NPM_TOKEN, matching the snapshot workflow.
        assert!(out.contains("          NODE_AUTH_TOKEN: ${{ secrets.NPM_TOKEN }}\n"));
        assert!(!out.contains("secrets.NODE_AUTH_TOKEN"));
        // A matrix publish package is never built or published binary-less / inline.
        assert!(!out.contains("rust_target"));
        assert!(!out.contains("# edit me: cross-compile"));
    }

    #[test]
    fn github_release_job_is_a_thin_call_for_every_notes_mode() {
        // The release body source (curated changelog / configured package changelogs / semantic
        // commits) is resolved inside `otf-release github-release`, so the *generated YAML* is the
        // same thin call for every mode — no inline awk/jq/gh/grep. The notes behavior itself is
        // covered by the `github_release` module's orchestrate tests.
        for notes in [
            GithubReleaseNotes::CuratedChangelog,
            GithubReleaseNotes::SemanticCommits,
            GithubReleaseNotes::AutoGenerate,
        ] {
            let config = ReleaseConfig {
                snapshot_tag: None,
                tag_format: "{name}@{version}".to_string(),
                legacy_tag_formats: Vec::new(),
                provider: "github".to_string(),
                default_branch: "main".to_string(),
                changelog_strategy: ChangelogStrategy::Curated,
                changelog_scope: ChangelogScope::Root,
                github_release_notes: notes,
                hooks: crate::config::Hooks::default(),
                publish: crate::config::PublishConfig::default(),
                adapters: vec![Ecosystem::Cargo],
                skip_publish: Vec::new(),
                packages: vec![cargo_build_only("otf-release")],
            };
            let out = render_workflow(&config);

            assert!(out.contains("        run: otf-release github-release --package otf-release --artifacts-dir .artifacts\n"));
            // None of the old inline notes/flatten bash survives in any mode.
            assert!(!out.contains("awk -v version"));
            assert!(!out.contains("changelog_files"));
            assert!(!out.contains("git log --no-merges"));
            assert!(!out.contains("grep -E"));
            assert!(!out.contains("gh release create"));
            assert!(!out.contains("notes_arg"));
        }
    }

    #[test]
    fn generic_build_only_renders_no_toolchain_and_manifest_version() {
        let config = ReleaseConfig {
            snapshot_tag: None,
            tag_format: "{name}@{version}".to_string(),
            legacy_tag_formats: Vec::new(),
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            hooks: crate::config::Hooks::default(),
            publish: crate::config::PublishConfig::default(),
            adapters: vec![Ecosystem::Generic],
            skip_publish: Vec::new(),
            packages: vec![generic_pkg("release", None)],
        };
        let out = render_workflow(&config);
        assert!(out.contains("  build-release:\n"));
        // Language-agnostic: no rust/node toolchain step is injected.
        assert!(!out.contains("dtolnay/rust-toolchain"));
        assert!(!out.contains("setup-node"));
        // Version, tag, notes, and asset renaming all happen inside the binary — the job is a thin
        // call, with no inline version read (`node -p`) or tag templating in the YAML.
        assert!(out.contains("  github-release-release:\n"));
        assert!(out.contains("        run: otf-release github-release --package release --artifacts-dir .artifacts\n"));
        assert!(!out.contains("node -p"));
        assert!(!out.contains("tag=\"release@$version\""));
        assert!(!out.contains("  publish:\n"));
        assert!(!out.contains("crates.io"));
    }

    #[test]
    fn multiple_build_only_packages_get_package_scoped_releases() {
        let config = ReleaseConfig {
            snapshot_tag: None,
            tag_format: "{name}@{version}".to_string(),
            legacy_tag_formats: Vec::new(),
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            hooks: crate::config::Hooks::default(),
            publish: crate::config::PublishConfig::default(),
            adapters: vec![Ecosystem::Cargo],
            skip_publish: Vec::new(),
            packages: vec![cargo_build_only("cli-a"), cargo_build_only("cli-b")],
        };
        let out = render_workflow(&config);
        // Each build-only package gets its own release job, each a thin per-package call.
        assert!(out.contains("  github-release-cli-a:\n"));
        assert!(out.contains("  github-release-cli-b:\n"));
        assert!(out.contains(
            "        run: otf-release github-release --package cli-a --artifacts-dir .artifacts\n"
        ));
        assert!(out.contains(
            "        run: otf-release github-release --package cli-b --artifacts-dir .artifacts\n"
        ));
        assert!(!out.contains("flat-artifacts"));
        assert!(!out.contains("tag=\"v${{ needs.check-release.outputs.version }}\""));
    }

    #[test]
    fn generic_publish_renders_publish_job_with_edit_me_toolchain() {
        let config = ReleaseConfig {
            snapshot_tag: None,
            tag_format: "{name}@{version}".to_string(),
            legacy_tag_formats: Vec::new(),
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            hooks: crate::config::Hooks::default(),
            publish: crate::config::PublishConfig::default(),
            adapters: vec![Ecosystem::Generic],
            skip_publish: Vec::new(),
            packages: vec![generic_pkg("jsr-lib", Some("npx jsr publish"))],
        };
        let out = render_workflow(&config);
        assert!(out.contains("  build-jsr-lib:\n"));
        // A unified publish job runs `otf-release publish` (which runs the configured command).
        assert!(out.contains("  publish-jsr-lib:\n"));
        assert!(out.contains("    needs: [check-release, build-jsr-lib]\n"));
        assert!(out.contains("      - name: Install otf-release\n"));
        assert!(out.contains(
            "        run: otf-release publish --package jsr-lib --artifacts-dir .artifacts\n"
        ));
        // The tool can't know a generic registry's toolchain/secret → edit-me markers.
        assert!(out.contains("# edit me: set up the toolchain your generic publish command needs"));
        assert!(!out.contains("github-release"));
    }

    #[test]
    fn polyglot_renders_one_publish_job_and_release() {
        let config = ReleaseConfig {
            snapshot_tag: None,
            tag_format: "{name}@{version}".to_string(),
            legacy_tag_formats: Vec::new(),
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
            hooks: crate::config::Hooks::default(),
            publish: crate::config::PublishConfig::default(),
            adapters: vec![Ecosystem::Npm, Ecosystem::Cargo],
            skip_publish: Vec::new(),
            packages: vec![cargo_build_only("web-compiler"), npm_publish("docs-site")],
        };
        let out = render_workflow(&config);
        // cargo build-only still stages per-platform binaries in a build job → GitHub Release.
        assert!(out.contains("  build-web-compiler:\n"));
        assert!(out.contains("  github-release-web-compiler:\n"));
        assert!(out.contains("    needs: [check-release, build-web-compiler]\n"));
        assert!(out.contains("        run: otf-release github-release --package web-compiler --artifacts-dir .artifacts\n"));
        // npm publish builds inline in its own publish job — no separate build job, no staging.
        assert!(!out.contains("  build-docs-site:\n"));
        assert!(out.contains("  publish-docs-site:\n"));
        assert!(out.contains("      - name: Build docs-site\n"));
        assert!(out.contains("        run: npm run build\n"));
        // The inline npm publish reads no staged artifacts (no `--artifacts-dir`).
        assert!(out.contains("        run: otf-release publish --package docs-site\n"));
        assert!(out.contains("      - uses: actions/setup-node@v4\n"));
        assert!(out.contains("      - name: Install otf-release\n"));
    }

    #[test]
    fn orchestrate_writes_release_toml_and_workflow() {
        let tmp = tempfile::tempdir().unwrap();
        let factory = FakeFactory {
            packages: vec![pkg("opentf-release", true), pkg("private-app", false)],
        };
        let prompt = FakePrompt {
            adapters: vec![Ecosystem::Cargo],
            build_names: vec!["opentf-release".into()],
            entries: vec![cargo_build_only("opentf-release")],
            ..FakePrompt::default()
        };
        orchestrate(&factory, &prompt, tmp.path(), &InitOptions { force: true }).unwrap();

        // release.toml persisted and parseable.
        let cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.adapters, vec![Ecosystem::Cargo]);
        assert_eq!(cfg.packages.len(), 1);
        assert_eq!(cfg.build_only_names(), vec!["opentf-release".to_string()]);
        assert_eq!(cfg.tag_format, DEFAULT_TAG_FORMAT);
        assert_eq!(cfg.snapshot_tag, None);
        assert_eq!(
            cfg.publish.ignore_paths.get("opentf-release"),
            Some(&Vec::new())
        );

        // workflow generated from it.
        let yml = fs::read_to_string(tmp.path().join(".github/workflows/release.yml")).unwrap();
        assert!(yml.contains("  github-release-opentf-release:\n"));
        assert!(!tmp.path().join(".github/workflows/snapshot.yml").exists());
    }

    #[test]
    fn orchestrate_suggests_existing_tag_format_and_preserves_legacy_when_changed() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q"]);
        fs::write(tmp.path().join("README.md"), "test\n").unwrap();
        git(tmp.path(), &["add", "-A"]);
        git(
            tmp.path(),
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=Test",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );
        git(tmp.path(), &["tag", "@opentf/web@0.5.0"]);

        let factory = FakeFactory {
            packages: vec![pkg("@opentf/web", true)],
        };
        let prompt = FakePrompt {
            adapters: vec![Ecosystem::Npm],
            tag_format: Some("{name}@v{version}".to_string()),
            ..FakePrompt::default()
        };
        orchestrate(&factory, &prompt, tmp.path(), &InitOptions { force: true }).unwrap();

        let cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.tag_format, "{name}@v{version}");
        assert_eq!(cfg.legacy_tag_formats, vec!["{name}@{version}"]);
    }

    #[test]
    fn orchestrate_collects_generic_packages_into_config() {
        let tmp = tempfile::tempdir().unwrap();
        // No npm/cargo discovery needed; generic packages are user-entered.
        let factory = FakeFactory { packages: vec![] };
        let prompt = FakePrompt {
            adapters: vec![Ecosystem::Generic],
            generic: vec![generic_pkg("jsr-lib", Some("npx jsr publish"))],
            ..FakePrompt::default()
        };
        orchestrate(&factory, &prompt, tmp.path(), &InitOptions { force: true }).unwrap();

        let cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.packages.len(), 1);
        let p = &cfg.packages[0];
        assert_eq!(p.adapter, Ecosystem::Generic);
        assert_eq!(p.manifest.as_deref(), Some("deno.json"));
        assert_eq!(p.publish.as_deref(), Some("npx jsr publish"));
        assert_eq!(p.mode, Mode::Publish);
        assert_eq!(cfg.publish.ignore_paths.get("jsr-lib"), Some(&Vec::new()));
    }

    #[test]
    fn orchestrate_persists_discovered_npm_manifest_path() {
        let tmp = tempfile::tempdir().unwrap();
        let factory = FakeFactory {
            packages: vec![npm_pkg(
                "@opentf/web-compiler",
                "packages/web-compiler/package.json",
            )],
        };
        // npm packages are auto-configured (no build prompt); the inline-build entry is created
        // from the discovered package + its `build` script.
        let prompt = FakePrompt {
            adapters: vec![Ecosystem::Npm],
            ..FakePrompt::default()
        };

        orchestrate(&factory, &prompt, tmp.path(), &InitOptions { force: true }).unwrap();

        // The discovered manifest path is persisted to release.toml — that's what `otf-release
        // check`/`publish` read the version from at runtime, so it must be recorded even though the
        // generated workflow no longer inlines a version-read for it.
        let cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.packages.len(), 1);
        assert_eq!(
            cfg.packages[0].manifest.as_deref(),
            Some("packages/web-compiler/package.json")
        );
        // Auto-detected inline build: no separate build job / artifact staging.
        assert_eq!(cfg.packages[0].command, "npm run build");
        assert!(cfg.packages[0].builds_inline());
        let yml = fs::read_to_string(tmp.path().join(".github/workflows/release.yml")).unwrap();
        assert!(yml.contains(
            "should_release=$(otf-release check --exclude-package @opentf/web-compiler)"
        ));
        assert!(!yml.contains("  build-opentf-web-compiler:\n"));
        assert!(!yml.contains("--artifacts-dir"));
        assert!(!yml.contains("workspaces"));
    }

    #[test]
    fn orchestrate_respects_overwrite_guard() {
        let tmp = tempfile::tempdir().unwrap();
        let toml_path = ReleaseConfig::path(tmp.path());
        fs::write(&toml_path, "SENTINEL").unwrap();

        let factory = FakeFactory {
            packages: vec![pkg("opentf-release", true)],
        };
        let decline = FakePrompt {
            adapters: vec![Ecosystem::Cargo],
            ..FakePrompt::default()
        };
        // Not forced + declines => release.toml untouched.
        orchestrate(&factory, &decline, tmp.path(), &InitOptions::default()).unwrap();
        assert_eq!(fs::read_to_string(&toml_path).unwrap(), "SENTINEL");

        // Forced => overwritten.
        orchestrate(&factory, &decline, tmp.path(), &InitOptions { force: true }).unwrap();
        assert!(ReleaseConfig::load(tmp.path()).is_ok());
    }
}
