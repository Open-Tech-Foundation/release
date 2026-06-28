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

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use inquire::{MultiSelect, Select, Text};

use crate::adapter::{Adapter, Pkg};
use crate::config::{ChangelogStrategy, Ecosystem, Mode, PackageEntry, ReleaseConfig, Target, DEFAULT_VERSION_FIELD};
use crate::discover::{scan_generic_candidates, GenericCandidate};

/// A static definition for a default target to offer in the CLI prompt.
pub struct TargetDef {
    pub label: &'static str,
    pub name: &'static str,
    pub arch: &'static str,
    pub rust_triple: &'static str,
}

impl TargetDef {
    pub fn to_target(&self) -> Target {
        Target {
            name: self.name.to_string(),
            arch: self.arch.to_string(),
        }
    }
}

/// A sensible default cross-compile target set (each emitted with an `# edit me` marker).
pub const DEFAULT_TARGETS: &[TargetDef] = &[
    TargetDef { label: "Linux x64", name: "linux", arch: "x86_64", rust_triple: "x86_64-unknown-linux-gnu" },
    TargetDef { label: "Linux ARM64", name: "linux", arch: "aarch64", rust_triple: "aarch64-unknown-linux-gnu" },
    TargetDef { label: "Linux x86 (32-bit)", name: "linux", arch: "x86", rust_triple: "i686-unknown-linux-gnu" },
    TargetDef { label: "macOS ARM64", name: "macos", arch: "aarch64", rust_triple: "aarch64-apple-darwin" },
    TargetDef { label: "macOS x64", name: "macos", arch: "x86_64", rust_triple: "x86_64-apple-darwin" },
    TargetDef { label: "Windows x64", name: "windows", arch: "x86_64", rust_triple: "x86_64-pc-windows-msvc" },
    TargetDef { label: "Windows ARM64", name: "windows", arch: "aarch64", rust_triple: "aarch64-pc-windows-msvc" },
    TargetDef { label: "Windows x86 (32-bit)", name: "windows", arch: "x86", rust_triple: "i686-pc-windows-msvc" },
];

/// Map a target triple to a sensible default GitHub-hosted runner.
fn runner_os(target: &str) -> &'static str {
    if target.contains("windows") {
        "windows-latest"
    } else if target.contains("apple") || target.contains("darwin") {
        "macos-latest"
    } else {
        "ubuntu-latest"
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
}

/// The interactive choices `init` needs.
pub trait InitPrompt {
    /// Which ecosystems to enable (multi-select: `npm`, `crates.io`).
    fn select_adapters(&self) -> Result<Vec<Ecosystem>>;
    /// Which publishable packages need a build step before publish/release?
    fn select_build_packages(&self, publishable: &[&Pkg]) -> Result<Vec<String>>;
    /// The full build config for one selected package (`enabled` is the chosen adapter set).
    fn build_entry(&self, pkg_name: &str, enabled: &[Ecosystem]) -> Result<PackageEntry>;
    /// Choose/enter generic packages. `found` is what the repo scan inferred (manifests with a
    /// version); the user imports from those and/or adds more by hand. Asked only when the generic
    /// adapter is enabled.
    fn generic_packages(&self, found: &[GenericCandidate]) -> Result<Vec<PackageEntry>>;
    /// Confirm overwriting an existing file (only asked when not `--force`).
    fn confirm_overwrite(&self, path: &Path) -> Result<bool>;
    /// Ask for the tag to use for automated snapshot releases.
    fn snapshot_tag(&self) -> Result<String>;
    /// Ask for the git hosting provider.
    fn prompt_provider(&self) -> Result<String>;
    /// Ask for the changelog management strategy.
    fn prompt_changelog_strategy(&self) -> Result<ChangelogStrategy>;
}

/// Wire up the real prompt and run the generator.
pub fn run(factory: &dyn AdapterFactory, root: &Path, opts: &InitOptions) -> Result<()> {
    orchestrate(factory, &StdinInitPrompt, root, opts)
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
    for &eco in enabled.iter().filter(|e| **e != Ecosystem::Generic) {
        let adapter = factory.make(eco);
        for pkg in adapter.discover_packages()? {
            if pkg.publishable {
                publishable.push(pkg);
            }
        }
    }

    let refs: Vec<&Pkg> = publishable.iter().collect();
    let build_names = prompt.select_build_packages(&refs)?;
    let mut packages = Vec::new();
    for name in &build_names {
        packages.push(prompt.build_entry(name, &enabled)?);
    }

    // Generic packages have no native adapter discovery — scan the repo for known manifests and
    // let the user import from what we infer (plus add any by hand).
    if enabled.contains(&Ecosystem::Generic) {
        let found = scan_generic_candidates(root);
        packages.extend(prompt.generic_packages(&found)?);
    }

    let config = ReleaseConfig {
        hooks: crate::config::Hooks::default(),
        adapters: enabled,
        packages,
        snapshot_tag: Some(prompt.snapshot_tag()?),
        provider: prompt.prompt_provider()?,
        changelog_strategy: prompt.prompt_changelog_strategy()?,
    };

    // 1. Persist the source of truth.
    let toml_path = ReleaseConfig::path(root);
    if write_allowed(&toml_path, opts.force, prompt)? {
        config.save(root)?;
        println!("Wrote {}", toml_path.display());
    }

    // 2. Generate the workflow from it.
    let yaml = render_workflow(&config);
    let yml_path = root.join(".github/workflows/release.yml");
    if write_allowed(&yml_path, opts.force, prompt)? {
        fs::create_dir_all(yml_path.parent().unwrap())
            .with_context(|| format!("creating {}", yml_path.parent().unwrap().display()))?;
        fs::write(&yml_path, yaml).with_context(|| format!("writing {}", yml_path.display()))?;
        println!("Wrote {}", yml_path.display());
    }

    // 3. Generate snapshot workflow.
    let snapshot_yaml = render_snapshot_workflow(&config);
    let snapshot_yml_path = root.join(".github/workflows/snapshot.yml");
    if write_allowed(&snapshot_yml_path, opts.force, prompt)? {
        fs::create_dir_all(snapshot_yml_path.parent().unwrap())
            .with_context(|| format!("creating {}", snapshot_yml_path.parent().unwrap().display()))?;
        fs::write(&snapshot_yml_path, snapshot_yaml).with_context(|| format!("writing {}", snapshot_yml_path.display()))?;
        println!("Wrote {}", snapshot_yml_path.display());
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

/// The preliminary job that checks if a release is needed, guarding the expensive build steps.
fn render_check_release_job(s: &mut String, version_cmd: &str) {
    s.push_str("  check-release:\n");
    s.push_str("    runs-on: ubuntu-latest\n");
    s.push_str("    outputs:\n");
    s.push_str("      should_release: ${{ steps.check.outputs.should_release }}\n");
    s.push_str("      version: ${{ steps.check.outputs.version }}\n");
    s.push_str("    steps:\n");
    s.push_str("      - uses: actions/checkout@v4\n");
    s.push_str("      - id: check\n");
    s.push_str("        run: |\n");
    s.push_str(&format!(
        "          version=\"$({version_cmd})\"  # edit me: where the version lives\n"
    ));
    s.push_str("          echo \"version=$version\" >> \"$GITHUB_OUTPUT\"\n");
    s.push_str("          if [ \"$version\" = \"0.0.0\" ]; then\n");
    s.push_str("            echo \"Version is 0.0.0 (unreleased); skipping build.\"\n");
    s.push_str("            echo \"should_release=false\" >> \"$GITHUB_OUTPUT\"\n");
    s.push_str("            exit 0\n");
    s.push_str("          fi\n");
    s.push_str("          echo \"should_release=true\" >> \"$GITHUB_OUTPUT\"\n\n");
}

/// Render `.github/workflows/release.yml` from the config.
///
/// Shape:
/// - one `build-<pkg>` job per package that has a build command (matrix or single runner),
/// - a single `publish` job (if any registry adapter is active) that sets up the needed
///   toolchains and runs `otf-release publish` once — it publishes only `publish`-mode packages
///   across every enabled ecosystem (npm, crates.io, generic),
/// - a `github-release` job if any package is `build-only` — attaches its artifacts to
///   package-scoped GitHub Releases (`name@X.Y.Z`), idempotently. **No registry push for
///   build-only packages.**
pub fn render_snapshot_workflow(config: &ReleaseConfig) -> String {
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
        s.push_str("      - name: Install Node\n");
        s.push_str("        uses: actions/setup-node@v4\n");
        s.push_str("        with:\n");
        s.push_str("          node-version: 'lts/*'\n");
        s.push_str("          registry-url: 'https://registry.npmjs.org'\n");
    }

    s.push_str("      - name: Install otf-release\n");
    s.push_str("        run: curl -LsSf https://github.com/opentf-org/opentf-release/releases/latest/download/otf-release-installer.sh | sh\n");
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

pub fn render_workflow(config: &ReleaseConfig) -> String {
    let any_build_only = config.packages.iter().any(|p| p.mode == Mode::BuildOnly);
    let npm_enabled = config.adapters.contains(&Ecosystem::Npm);
    let cargo_publishes = config
        .packages
        .iter()
        .any(|p| p.adapter == Ecosystem::Cargo && p.mode == Mode::Publish);
    let generic_publishes = config
        .packages
        .iter()
        .any(|p| p.adapter == Ecosystem::Generic && p.mode == Mode::Publish);
    let needs_publish = npm_enabled || cargo_publishes || generic_publishes;

    let mut s = String::from("name: Release\n\non:\n  push:\n    branches: [main]\n");
    if any_build_only || needs_publish {
        s.push_str("\npermissions:\n  contents: write  # create tags and GitHub Releases\n");
    }
    s.push_str("\njobs:\n");
    let version_cmd = version_read_cmd(config);
    render_check_release_job(&mut s, &version_cmd);

    // Build jobs only for packages that actually declare a build command.
    let has_build = |p: &&PackageEntry| !p.command.trim().is_empty();
    for entry in config.packages.iter().filter(|p| has_build(p)) {
        render_build_job(&mut s, entry);
    }

    if needs_publish {
        let needs: Vec<String> = config
            .packages
            .iter()
            .filter(|p| p.mode == Mode::Publish && has_build(p))
            .map(|p| build_job(&p.name))
            .collect();
        render_publish_job(
            &mut s,
            &needs,
            npm_enabled,
            cargo_publishes,
            generic_publishes,
        );
    }

    if any_build_only {
        let build_only: Vec<&PackageEntry> = config
            .packages
            .iter()
            .filter(|p| p.mode == Mode::BuildOnly)
            .collect();
        let needs: Vec<String> = build_only
            .iter()
            .filter(|p| has_build(p))
            .map(|p| build_job(&p.name))
            .collect();
        render_github_release(&mut s, &needs, &build_only);
    }

    s
}

/// The shell snippet that reads the release version for the GitHub Release tag, based on the
/// first build-only package: a manifest field (generic), `package.json` (npm), or `Cargo.toml`.
fn version_read_cmd(config: &ReleaseConfig) -> String {
    match config.packages.first() {
        Some(e) if e.adapter == Ecosystem::Generic => {
            let manifest = e.manifest.as_deref().unwrap_or("deno.json");
            let field = e.version_field.as_deref().unwrap_or("version");
            if manifest.ends_with(".json") {
                format!("node -p \"require('./{manifest}').{field}\"")
            } else if manifest == "Cargo.toml" && field == "version" {
                "cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version'".to_string()
            } else if manifest.ends_with(".toml") {
                format!("grep -m1 '^{field}' {manifest} | cut -d '\"' -f2 | tr -d '\"'")
            } else {
                format!("cat {manifest}  # edit me: extract the {field} value")
            }
        }
        Some(e) if e.adapter == Ecosystem::Npm => {
            "node -p \"require('./package.json').version\"".to_string()
        }
        Some(_) => {
            "cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version'".to_string()
        }
        None if config.adapters.contains(&Ecosystem::Npm) => {
            "node -p \"require('./package.json').version\"".to_string()
        }
        None if config.adapters.contains(&Ecosystem::Cargo) => {
            "cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version'".to_string()
        }
        _ => {
            "echo 0.0.0  # edit me: where the version lives".to_string()
        }
    }
}



/// One build job: matrix or single runner, runs the package's command, uploads its artifacts.
fn render_build_job(s: &mut String, entry: &PackageEntry) {
    let job = build_job(&entry.name);
    let art_slug = slug(&entry.name);
    s.push_str(&format!("  {job}:\n"));
    s.push_str("    needs: [check-release]\n");
    s.push_str("    if: needs.check-release.outputs.should_release == 'true'\n");

    if entry.matrix {
        s.push_str("    runs-on: ${{ matrix.os }}\n");
        s.push_str("    strategy:\n      matrix:\n        include:\n");
        
        // Print all selected targets
        for target in &entry.targets {
            let rust_triple = DEFAULT_TARGETS
                .iter()
                .find(|d| d.name == target.name && d.arch == target.arch)
                .map(|d| d.rust_triple)
                .unwrap_or("x86_64-unknown-linux-gnu");
            let os = runner_os(rust_triple);
            let ext = if os == "windows-latest" { ".exe" } else { "" };
            s.push_str(&format!(
                "          - {{ rust_target: \"{}\", os: \"{}\", ext: \"{}\", name: \"{}\", arch: \"{}\" }}  # edit me\n",
                rust_triple, os, ext, target.name, target.arch
            ));
        }

        // Print all unselected defaults as commented-out lines
        for def in DEFAULT_TARGETS {
            let is_selected = entry.targets
                .iter()
                .any(|t| t.name == def.name && t.arch == def.arch);
            if !is_selected {
                let os = runner_os(def.rust_triple);
                let ext = if os == "windows-latest" { ".exe" } else { "" };
                s.push_str(&format!(
                    "          # - {{ rust_target: \"{}\", os: \"{}\", ext: \"{}\", name: \"{}\", arch: \"{}\" }}  # edit me\n",
                    def.rust_triple, os, ext, def.name, def.arch
                ));
            }
        }
    } else {
        s.push_str("    runs-on: ubuntu-latest  # edit me: choose a runner\n");
    }

    s.push_str("    steps:\n");
    s.push_str("      - uses: actions/checkout@v4\n");
    match entry.adapter {
        Ecosystem::Cargo => {
            s.push_str("      - uses: dtolnay/rust-toolchain@stable\n");
            if entry.matrix {
                s.push_str("        with:\n          targets: ${{ matrix.rust_target }}\n");
            }
        }
        Ecosystem::Npm => {
            s.push_str("      - uses: actions/setup-node@v4\n");
            s.push_str("        with:\n          node-version: 20\n");
            s.push_str("      - run: npm ci\n");
        }
        // Generic is language-agnostic: no toolchain is assumed — the command sets up its own.
        Ecosystem::Generic => {}
    }
    s.push_str(&format!("      - name: Build {}\n", entry.name));
    if entry.matrix {
        s.push_str(&format!(
            "        run: {}  # edit me: cross-compile with ${{{{ matrix.rust_target }}}}\n",
            entry.command
        ));
    } else {
        s.push_str(&format!("        run: {}\n", entry.command));
    }
    s.push_str("      - uses: actions/upload-artifact@v4\n");
    s.push_str("        with:\n");
    if entry.matrix {
        s.push_str(&format!(
            "          name: {art_slug}-${{{{ matrix.name }}}}-${{{{ matrix.arch }}}}\n"
        ));
    } else {
        s.push_str(&format!("          name: {art_slug}\n"));
    }
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
fn render_publish_job(s: &mut String, needs: &[String], npm: bool, cargo: bool, generic: bool) {
    s.push_str("  publish:\n");
    let mut actual_needs = vec!["check-release".to_string()];
    actual_needs.extend_from_slice(needs);
    needs_line(s, &actual_needs);
    s.push_str("    if: needs.check-release.outputs.should_release == 'true'\n");
    s.push_str("    runs-on: ubuntu-latest\n");
    s.push_str("    steps:\n");
    s.push_str("      - uses: actions/checkout@v4\n        with:\n          fetch-depth: 0\n");
    if npm {
        s.push_str("      - uses: actions/setup-node@v4\n");
        s.push_str("        with:\n          node-version: 20\n");
        s.push_str("          registry-url: https://registry.npmjs.org\n");
    }
    if cargo {
        s.push_str("      - uses: dtolnay/rust-toolchain@stable\n");
    }
    if generic {
        s.push_str("      # edit me: set up the toolchain your generic publish command needs\n");
    }
    let staged = download_artifacts(s, needs);
    if npm {
        s.push_str("      - run: npm ci\n");
    }
    s.push_str("      - name: Install otf-release\n");
    s.push_str("        run: curl -fsSL https://raw.githubusercontent.com/Open-Tech-Foundation/release/main/install.sh | bash\n");
    s.push_str("      - name: Publish\n");
    if staged {
        s.push_str("        run: otf-release publish --artifacts-dir .artifacts\n");
    } else {
        s.push_str("        run: otf-release publish\n");
    }
    s.push_str("        env:\n");
    if npm {
        s.push_str("          NODE_AUTH_TOKEN: ${{ secrets.NODE_AUTH_TOKEN }}\n");
    }
    if cargo {
        s.push_str("          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}\n");
    }
    if generic {
        s.push_str("          # edit me: any secret your generic publish command needs\n");
    }
    s.push_str("          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}\n");
    s.push('\n');
}

/// The GitHub Release job for `build-only` packages: attach each package's staged artifacts to a
/// package-scoped tag (`name@X.Y.Z`), idempotently (skip an existing release). No registry push.
fn render_github_release(s: &mut String, needs: &[String], build_only: &[&PackageEntry]) {
    s.push_str("  github-release:\n");
    let mut actual_needs = vec!["check-release".to_string()];
    actual_needs.extend_from_slice(needs);
    needs_line(s, &actual_needs);
    s.push_str("    if: needs.check-release.outputs.should_release == 'true'\n");
    s.push_str("    runs-on: ubuntu-latest\n");
    s.push_str("    steps:\n");
    s.push_str("      - uses: actions/checkout@v4\n        with:\n          fetch-depth: 0\n");
    let staged = download_artifacts(s, needs);
    s.push_str("      - name: Create GitHub Release\n");
    s.push_str("        env:\n          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}\n");
    s.push_str("        run: |\n");
    s.push_str("          version=\"${{ needs.check-release.outputs.version }}\"\n");
    for entry in build_only {
        let art_slug = slug(&entry.name);
        s.push_str(&format!("          tag=\"{}@$version\"\n", entry.name));
        s.push_str("          if gh release view \"$tag\" >/dev/null 2>&1; then\n");
        s.push_str("            echo \"Release $tag already exists; nothing to do.\"\n");
        s.push_str("          else\n");
        if staged {
            s.push_str(&format!("            rm -rf \".flat-artifacts-{art_slug}\"\n"));
            s.push_str(&format!("            mkdir -p \".flat-artifacts-{art_slug}\"\n"));
            s.push_str("            shopt -s nullglob globstar\n");
            s.push_str(&format!(
                "            for file in .artifacts/{art_slug}*/**/*; do\n"
            ));
            s.push_str("              if [ -f \"$file\" ]; then\n");
            s.push_str("                dir_name=$(basename \"$(dirname \"$file\")\")\n");
            s.push_str("                file_name=$(basename \"$file\")\n");
            s.push_str("                ext=\"${file_name##*.}\"\n");
            s.push_str("                if [ \"$ext\" = \"$file_name\" ]; then\n");
            s.push_str(&format!(
                "                  cp \"$file\" \".flat-artifacts-{art_slug}/${{dir_name}}\"\n"
            ));
            s.push_str("                else\n");
            s.push_str(&format!(
                "                  cp \"$file\" \".flat-artifacts-{art_slug}/${{dir_name}}.${{ext}}\"\n"
            ));
            s.push_str("                fi\n");
            s.push_str("              fi\n");
            s.push_str("            done\n");
            s.push_str(&format!(
                "            gh release create \"$tag\" --target main --title \"$tag\" --generate-notes .flat-artifacts-{art_slug}/*\n"
            ));
        } else {
            s.push_str("            gh release create \"$tag\" --target main --title \"$tag\" --generate-notes\n");
        }
        s.push_str("          fi\n");
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
            "build-only (GitHub Release artifacts)",
        ],
    )
    .raw_prompt()?
    .index
    {
        1 => Mode::BuildOnly,
        _ => Mode::Publish,
    };

    let matrix = Select::new(
        &format!("  {name} — build across a target matrix?"),
        vec!["Yes", "No"],
    )
    .raw_prompt()?
    .index
        == 0;
    let targets = if matrix {
        let defaults: Vec<usize> = DEFAULT_TARGETS
            .iter()
            .enumerate()
            .filter(|(_, def)| !def.label.contains("32-bit"))
            .map(|(i, _)| i)
            .collect();
        let labels: Vec<String> = DEFAULT_TARGETS
            .iter()
            .map(|def| format!("{} - {}-{}", def.label, def.name, def.arch))
            .collect();
        let selected = MultiSelect::new("  Target triples:", labels)
            .with_default(&defaults)
            .with_help_message(MULTI_HELP)
            .raw_prompt()?;
        selected
            .iter()
            .map(|s| DEFAULT_TARGETS[s.index].to_target())
            .collect()
    } else {
        Vec::new()
    };

    let default_cmd = match (kind, matrix) {
        (Some("Rust / Cargo"), true) => "rustup target add ${{ matrix.rust_target }} && cargo build --release --target ${{ matrix.rust_target }}",
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
        .prompt()?;

    let bin_name = if kind == Some("Rust / Cargo") {
        let n = Text::new(&format!("  {name} — binary name:"))
            .with_default(name)
            .prompt()?;
        Some(n)
    } else {
        None
    };

    let default_artifacts = match (kind, matrix) {
        (Some("Rust / Cargo"), true) => format!(
            "target/${{{{ matrix.target }}}}/release/{}${{{{ matrix.ext }}}}",
            bin_name.as_deref().unwrap()
        ),
        (Some("Rust / Cargo"), false) => format!("target/release/{}", bin_name.as_deref().unwrap()),
        (Some("Node / npm"), _) => "dist/*".to_string(),
        _ => "".to_string(),
    };
    let artifacts = Text::new(&format!("  {name} — artifacts to stage (optional):"))
        .with_default(&default_artifacts)
        .prompt()?;

    let publish = if mode == Mode::Publish {
        let cmd = Text::new(&format!(
            "  {name} — publish command (e.g. npx jsr publish):"
        ))
        .with_default("")
        .prompt()?;
        (!cmd.trim().is_empty()).then_some(cmd)
    } else {
        None
    };

    Ok(PackageEntry {
        name: name.to_string(),
        adapter: Ecosystem::Generic,
        mode,
        matrix,
        targets,
        command,
        artifacts,
        manifest: Some(manifest.to_string()),
        version_field: Some(version_field.to_string()),
        publish,
    })
}

/// The real terminal prompt for `init` — arrow-key select, spacebar multi-select, confirm.
pub struct StdinInitPrompt;

const MULTI_HELP: &str = "↑↓ move · space toggle · enter confirm";

impl InitPrompt for StdinInitPrompt {
    fn select_adapters(&self) -> Result<Vec<Ecosystem>> {
        let labels: Vec<&str> = Ecosystem::ALL.iter().map(|e| e.label()).collect();
        let chosen = MultiSelect::new("Adapters to enable:", labels)
            .with_help_message(MULTI_HELP)
            .raw_prompt()?;
        Ok(chosen.iter().map(|o| Ecosystem::ALL[o.index]).collect())
    }

    fn select_build_packages(&self, publishable: &[&Pkg]) -> Result<Vec<String>> {
        if publishable.is_empty() {
            return Ok(Vec::new());
        }
        let labels: Vec<String> = publishable.iter().map(|p| p.name.clone()).collect();
        let chosen = MultiSelect::new("Which packages need a build step before publish?", labels)
            .with_help_message(MULTI_HELP)
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
            let opt = Select::new(&format!("{pkg_name} — adapter:"), labels).raw_prompt()?;
            enabled[opt.index]
        };

        let mode = match Select::new(
            &format!("{pkg_name} — mode:"),
            vec![
                "publish (to registry)",
                "build-only (GitHub Release artifacts)",
            ],
        )
        .raw_prompt()?
        .index
        {
            1 => Mode::BuildOnly,
            _ => Mode::Publish,
        };

        let matrix = Select::new(
            &format!("{pkg_name} — build across a target matrix?"),
            vec!["Yes", "No"],
        )
        .raw_prompt()?
        .index
            == 0;
        let targets = if matrix {
            let defaults: Vec<usize> = DEFAULT_TARGETS
                .iter()
                .enumerate()
                .filter(|(_, def)| !def.label.contains("32-bit"))
                .map(|(i, _)| i)
                .collect();
            let labels: Vec<String> = DEFAULT_TARGETS
                .iter()
                .map(|def| format!("{} - {}-{}", def.label, def.name, def.arch))
                .collect();
            let selected = MultiSelect::new("Target triples:", labels)
                .with_default(&defaults)
                .with_help_message(MULTI_HELP)
                .raw_prompt()?;
            selected
                .iter()
                .map(|s| DEFAULT_TARGETS[s.index].to_target())
                .collect()
        } else {
            Vec::new()
        };

        let default_cmd = match adapter {
            Ecosystem::Cargo => "cargo build --release",
            Ecosystem::Npm => "npm run build",
            Ecosystem::Generic => "",
        };
        let command = Text::new(&format!("{pkg_name} — build command:"))
            .with_default(default_cmd)
            .prompt()?;
        let artifacts = Text::new(&format!("{pkg_name} — artifacts to stage:")).prompt()?;

        Ok(PackageEntry {
            name: pkg_name.to_string(),
            adapter,
            mode,
            matrix,
            targets,
            command,
            artifacts,
            manifest: None,
            version_field: None,
            publish: None,
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
            let first = out.is_empty() && found.is_empty();
            let question = if found.is_empty() {
                "Add a generic package?"
            } else {
                "Add another package by hand?"
            };
            if Select::new(question, vec!["Yes", "No"]).raw_prompt()?.index == 1 {
                break;
            }
            let name = Text::new("  name:").prompt()?;
            let manifest =
                Text::new("  manifest file holding the version (e.g. deno.json):").prompt()?;
            let version_field = Text::new("  version field:")
                .with_default(DEFAULT_VERSION_FIELD)
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
        .raw_prompt()?
        .index
            == 1)
    }

    fn snapshot_tag(&self) -> Result<String> {
        let tag = Select::new(
            "What tag should be used for ephemeral CI releases?",
            vec!["snapshot", "dev", "canary", "custom (type your own)"],
        ).prompt()?;

        if tag.starts_with("custom") {
            Ok(inquire::Text::new("Enter custom tag (e.g. edge):").prompt()?)
        } else {
            Ok(tag.to_string())
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
            .prompt()?;

            if ans == "GitHub" {
                return Ok("github".to_string());
            } else {
                println!("Only GitHub is fully supported at this moment. Please select GitHub.");
            }
        }
    }

    fn prompt_changelog_strategy(&self) -> Result<ChangelogStrategy> {
        let ans = Select::new(
            "How would you like to manage your changelogs?",
            vec![
                "Curated (Write them by hand in [Unreleased] sections of CHANGELOG.md)",
                "Generated (Automatically parse Git commits since the last tag)",
            ],
        )
        .prompt()?;
        
        if ans.starts_with("Curated") {
            Ok(ChangelogStrategy::Curated)
        } else {
            Ok(ChangelogStrategy::Generated)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
    }
    impl InitPrompt for FakePrompt {
        fn select_adapters(&self) -> Result<Vec<Ecosystem>> {
            Ok(self.adapters.clone())
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
        fn snapshot_tag(&self) -> Result<String> {
            Ok("snapshot".to_string())
        }
        fn prompt_provider(&self) -> Result<String> {
            Ok("github".to_string())
        }
        fn prompt_changelog_strategy(&self) -> Result<ChangelogStrategy> {
            Ok(ChangelogStrategy::Curated)
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

    fn cargo_build_only(name: &str) -> PackageEntry {
        PackageEntry {
            name: name.to_string(),
            adapter: Ecosystem::Cargo,
            mode: Mode::BuildOnly,
            matrix: true,
            targets: vec![
                crate::config::Target { name: "linux".into(), arch: "x86_64".into() },
                crate::config::Target { name: "windows".into(), arch: "x86_64".into() },
            ],
            command: "cargo build --release -p otf-release".into(),
            artifacts: "target/${{ matrix.rust_target }}/release/otf-release*".into(),
            manifest: None,
            version_field: None,
            publish: None,
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
            manifest: None,
            version_field: None,
            publish: None,
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
            manifest: Some("deno.json".into()),
            version_field: Some("version".into()),
            publish: publish.map(|s| s.into()),
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
            provider: "github".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            hooks: crate::config::Hooks::default(),
            adapters: vec![Ecosystem::Npm],
            packages: vec![],
        };
        let out = render_workflow(&config);
        assert!(out.contains("  publish:\n"));
        assert!(out.contains("      - uses: actions/setup-node@v4\n"));
        assert!(out.contains("          version=\"$(node -p \"require('./package.json').version\")\""));
        assert!(!out.contains("version=\"$(cargo metadata"));
        assert!(out.contains("      - name: Install otf-release\n"));
        assert!(out.contains("        run: otf-release publish\n"));
        // No build steps, so no needs and no artifact download.
        assert!(out.contains("needs: [check-release]"));
        assert!(!out.contains("github-release"));
    }

    #[test]
    fn cargo_build_only_renders_github_release_no_registry() {
        let config = ReleaseConfig {
            snapshot_tag: None,
            provider: "github".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            hooks: crate::config::Hooks::default(),
            adapters: vec![Ecosystem::Cargo],
            packages: vec![cargo_build_only("opentf-release")],
        };
        let out = render_workflow(&config);
        // Build matrix with per-target runners.
        assert!(out.contains("  build-opentf-release:\n"));
        assert!(out.contains("    runs-on: ${{ matrix.os }}\n"));
        assert!(out.contains(
            "name: \"windows\", arch: \"x86_64\""
        ));
        assert!(out.contains("        run: cargo build --release -p otf-release"));
        // Ships via a GitHub Release, idempotently — no registry, no cargo publish.
        assert!(out.contains("permissions:\n  contents: write"));
        assert!(out.contains("  github-release:\n"));
        assert!(out.contains("    needs: [check-release, build-opentf-release]\n"));
        assert!(out.contains("          tag=\"opentf-release@$version\"\n"));
        assert!(out.contains("            rm -rf \".flat-artifacts-opentf-release\"\n"));
        assert!(out.contains("          if gh release view \"$tag\" >/dev/null 2>&1; then\n"));
        assert!(!out.contains("tag=\"v${{ needs.check-release.outputs.version }}\""));
        assert!(!out.contains("refs/tags/$tag"));
        assert!(!out.contains("cargo publish"));
        assert!(!out.contains("crates.io"));
        // build-only cargo: no publish job at all.
        assert!(!out.contains("  publish:\n"));
    }

    #[test]
    fn generic_build_only_renders_no_toolchain_and_manifest_version() {
        let config = ReleaseConfig {
            snapshot_tag: None,
            provider: "github".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            hooks: crate::config::Hooks::default(),
            adapters: vec![Ecosystem::Generic],
            packages: vec![generic_pkg("release", None)],
        };
        let out = render_workflow(&config);
        assert!(out.contains("  build-release:\n"));
        // Language-agnostic: no rust/node toolchain step is injected.
        assert!(!out.contains("dtolnay/rust-toolchain"));
        assert!(!out.contains("setup-node"));
        // Version comes from the configured manifest (deno.json), shipped via a GitHub Release.
        assert!(out.contains("          version=\"$(node -p \"require('./deno.json').version\")\""));
        assert!(out.contains("  github-release:\n"));
        assert!(out.contains("          tag=\"release@$version\"\n"));
        assert!(!out.contains("  publish:\n"));
        assert!(!out.contains("crates.io"));
    }

    #[test]
    fn multiple_build_only_packages_get_package_scoped_releases() {
        let config = ReleaseConfig {
            snapshot_tag: None,
            provider: "github".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            hooks: crate::config::Hooks::default(),
            adapters: vec![Ecosystem::Cargo],
            packages: vec![cargo_build_only("cli-a"), cargo_build_only("cli-b")],
        };
        let out = render_workflow(&config);
        assert!(out.contains("          tag=\"cli-a@$version\"\n"));
        assert!(out.contains("          tag=\"cli-b@$version\"\n"));
        assert!(out.contains("            for file in .artifacts/cli-a*/**/*; do\n"));
        assert!(out.contains("            for file in .artifacts/cli-b*/**/*; do\n"));
        assert!(out.contains("            rm -rf \".flat-artifacts-cli-a\"\n"));
        assert!(out.contains("            rm -rf \".flat-artifacts-cli-b\"\n"));
        assert!(!out.contains("tag=\"v${{ needs.check-release.outputs.version }}\""));
    }

    #[test]
    fn generic_publish_renders_publish_job_with_edit_me_toolchain() {
        let config = ReleaseConfig {
            snapshot_tag: None,
            provider: "github".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            hooks: crate::config::Hooks::default(),
            adapters: vec![Ecosystem::Generic],
            packages: vec![generic_pkg("jsr-lib", Some("npx jsr publish"))],
        };
        let out = render_workflow(&config);
        assert!(out.contains("  build-jsr-lib:\n"));
        // A unified publish job runs `otf-release publish` (which runs the configured command).
        assert!(out.contains("  publish:\n"));
        assert!(out.contains("    needs: [check-release, build-jsr-lib]\n"));
        assert!(out.contains("      - name: Install otf-release\n"));
        assert!(out.contains("        run: otf-release publish --artifacts-dir .artifacts\n"));
        // The tool can't know a generic registry's toolchain/secret → edit-me markers.
        assert!(out.contains("# edit me: set up the toolchain your generic publish command needs"));
        assert!(!out.contains("github-release"));
    }

    #[test]
    fn polyglot_renders_one_publish_job_and_release() {
        let config = ReleaseConfig {
            snapshot_tag: None,
            provider: "github".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            hooks: crate::config::Hooks::default(),
            adapters: vec![Ecosystem::Npm, Ecosystem::Cargo],
            packages: vec![cargo_build_only("web-compiler"), npm_publish("docs-site")],
        };
        let out = render_workflow(&config);
        assert!(out.contains("  build-web-compiler:\n"));
        assert!(out.contains("  build-docs-site:\n"));
        // A single publish job (npm) depending on the npm build.
        assert!(out.contains("  publish:\n"));
        assert!(out.contains("    needs: [check-release, build-docs-site]\n"));
        assert!(out.contains("      - uses: actions/setup-node@v4\n"));
        assert!(out.contains("      - name: Install otf-release\n"));
        // cargo side is build-only: a GitHub Release depending on the cargo build.
        assert!(out.contains("  github-release:\n"));
        assert!(out.contains("    needs: [check-release, build-web-compiler]\n"));
        assert!(out.contains("          tag=\"web-compiler@$version\"\n"));
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

        // workflow generated from it.
        let yml = fs::read_to_string(tmp.path().join(".github/workflows/release.yml")).unwrap();
        assert!(yml.contains("  github-release:\n"));
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
