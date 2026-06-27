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
use crate::config::{Ecosystem, Mode, PackageEntry, ReleaseConfig, DEFAULT_VERSION_FIELD};
use crate::discover::{scan_generic_candidates, GenericCandidate};

/// A sensible default cross-compile target set (each emitted with an `# edit me` marker).
pub const DEFAULT_TARGETS: &[(&str, &str)] = &[
    ("Linux x64", "x86_64-unknown-linux-gnu"),
    ("Linux ARM64", "aarch64-unknown-linux-gnu"),
    ("Linux x86 (32-bit)", "i686-unknown-linux-gnu"),
    ("macOS ARM64", "aarch64-apple-darwin"),
    ("macOS x64", "x86_64-apple-darwin"),
    ("Windows x64", "x86_64-pc-windows-msvc"),
    ("Windows ARM64", "aarch64-pc-windows-msvc"),
    ("Windows x86 (32-bit)", "i686-pc-windows-msvc"),
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
        adapters: enabled,
        packages,
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
    s.push_str("          tag=\"v$version\"\n");
    s.push_str("          if git ls-remote --exit-code --tags origin \"refs/tags/$tag\" >/dev/null 2>&1; then\n");
    s.push_str("            echo \"Tag $tag already exists; skipping build.\"\n");
    s.push_str("            echo \"should_release=false\" >> \"$GITHUB_OUTPUT\"\n");
    s.push_str("          else\n");
    s.push_str("            echo \"should_release=true\" >> \"$GITHUB_OUTPUT\"\n");
    s.push_str("          fi\n\n");
}

/// Render `.github/workflows/release.yml` from the config.
///
/// Shape:
/// - one `build-<pkg>` job per package that has a build command (matrix or single runner),
/// - a single `publish` job (if any registry adapter is active) that sets up the needed
///   toolchains and runs `otf-release publish` once — it publishes only `publish`-mode packages
///   across every enabled ecosystem (npm, crates.io, generic),
/// - a `github-release` job if any package is `build-only` — attaches its artifacts to a
///   GitHub Release `vX.Y.Z`, idempotently. **No registry push for build-only packages.**
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
    let version_cmd = version_read_cmd(config.packages.first());
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
        render_github_release(
            &mut s,
            &needs,
        );
    }

    s
}

/// The shell snippet that reads the release version for the GitHub Release tag, based on the
/// first build-only package: a manifest field (generic), `package.json` (npm), or `Cargo.toml`.
fn version_read_cmd(entry: Option<&PackageEntry>) -> String {
    match entry {
        Some(e) if e.adapter == Ecosystem::Generic => {
            let manifest = e.manifest.as_deref().unwrap_or("deno.json");
            let field = e.version_field.as_deref().unwrap_or("version");
            if manifest.ends_with(".json") {
                format!("node -p \"require('./{manifest}').{field}\"")
            } else if manifest.ends_with(".toml") {
                format!("grep -m1 '^{field}' {manifest} | cut -d '\"' -f2 | tr -d '\"'")
            } else {
                format!("cat {manifest}  # edit me: extract the {field} value")
            }
        }
        Some(e) if e.adapter == Ecosystem::Npm => {
            "node -p \"require('./package.json').version\"".to_string()
        }
        _ => "cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version'".to_string(),
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
        for target in &entry.targets {
            let os = runner_os(target);
            let ext = if os == "windows-latest" { ".exe" } else { "" };
            s.push_str(&format!(
                "          - {{ target: \"{}\", os: \"{}\", ext: \"{}\" }}  # edit me\n",
                target, os, ext
            ));
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
                s.push_str("        with:\n          targets: ${{ matrix.target }}\n");
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
            "        run: {}  # edit me: cross-compile with ${{{{ matrix.target }}}}\n",
            entry.command
        ));
    } else {
        s.push_str(&format!("        run: {}\n", entry.command));
    }
    s.push_str("      - uses: actions/upload-artifact@v4\n");
    s.push_str("        with:\n");
    if entry.matrix {
        s.push_str(&format!(
            "          name: {art_slug}-${{{{ matrix.target }}}}\n"
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

/// The GitHub Release job for `build-only` packages: attach the staged artifacts to a tag
/// `vX.Y.Z`, idempotently (skip an existing tag). No registry push.
fn render_github_release(s: &mut String, needs: &[String]) {
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
    s.push_str("          tag=\"v${{ needs.check-release.outputs.version }}\"\n");
    s.push_str("          if gh release view \"$tag\" >/dev/null 2>&1; then\n");
    s.push_str("            echo \"Release $tag already exists; nothing to do.\"\n");
    s.push_str("            exit 0\n");
    s.push_str("          fi\n");
    if staged {
        s.push_str("          shopt -s globstar\n");
        s.push_str("          mkdir -p .flat-artifacts\n");
        s.push_str("          for file in .artifacts/**/*; do\n");
        s.push_str("            if [ -f \"$file\" ]; then\n");
        s.push_str("              dir_name=$(basename \"$(dirname \"$file\")\")\n");
        s.push_str("              file_name=$(basename \"$file\")\n");
        s.push_str("              mv \"$file\" \".flat-artifacts/${dir_name}---${file_name}\"\n");
        s.push_str("            fi\n");
        s.push_str("          done\n");
        s.push_str(
            "          gh release create \"$tag\" --target main --title \"$tag\" --generate-notes .flat-artifacts/*\n",
        );
    } else {
        s.push_str("          gh release create \"$tag\" --target main --title \"$tag\" --generate-notes\n");
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
            .filter(|(_, (label, _))| !label.contains("32-bit"))
            .map(|(i, _)| i)
            .collect();
        let labels: Vec<String> = DEFAULT_TARGETS
            .iter()
            .map(|(label, triple)| format!("{} - {}", label, triple))
            .collect();
        let selected = MultiSelect::new("  Target triples:", labels)
            .with_default(&defaults)
            .with_help_message(MULTI_HELP)
            .raw_prompt()?;
        selected
            .iter()
            .map(|s| DEFAULT_TARGETS[s.index].1.to_string())
            .collect()
    } else {
        Vec::new()
    };

    let default_cmd = match (kind, matrix) {
        (Some("Rust / Cargo"), true) => "rustup target add ${{ matrix.target }} && cargo build --release --target ${{ matrix.target }}",
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
                .filter(|(_, (label, _))| !label.contains("32-bit"))
                .map(|(i, _)| i)
                .collect();
            let labels: Vec<String> = DEFAULT_TARGETS
                .iter()
                .map(|(label, triple)| format!("{} - {}", label, triple))
                .collect();
            let selected = MultiSelect::new("Target triples:", labels)
                .with_default(&defaults)
                .with_help_message(MULTI_HELP)
                .raw_prompt()?;
            selected
                .iter()
                .map(|s| DEFAULT_TARGETS[s.index].1.to_string())
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
            &format!("{} exists. Overwrite?", path.display()),
            vec!["No", "Yes"],
        )
        .raw_prompt()?
        .index
            == 1)
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
                "x86_64-unknown-linux-gnu".into(),
                "x86_64-pc-windows-msvc".into(),
            ],
            command: "cargo build --release -p otf-release".into(),
            artifacts: "target/${{ matrix.target }}/release/otf-release*".into(),
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
            adapters: vec![Ecosystem::Npm],
            packages: vec![],
        };
        let out = render_workflow(&config);
        assert!(out.contains("  publish:\n"));
        assert!(out.contains("      - uses: actions/setup-node@v4\n"));
        assert!(out.contains("        run: otf-release publish\n"));
        // No build steps, so no needs and no artifact download.
        assert!(out.contains("needs: [check-release]"));
        assert!(!out.contains("github-release"));
    }

    #[test]
    fn cargo_build_only_renders_github_release_no_registry() {
        let config = ReleaseConfig {
            adapters: vec![Ecosystem::Cargo],
            packages: vec![cargo_build_only("opentf-release")],
        };
        let out = render_workflow(&config);
        // Build matrix with per-target runners.
        assert!(out.contains("  build-opentf-release:\n"));
        assert!(out.contains("    runs-on: ${{ matrix.os }}\n"));
        assert!(out.contains(
            "          - { target: \"x86_64-pc-windows-msvc\", os: \"windows-latest\" }  # edit me\n"
        ));
        assert!(out.contains("        run: cargo build --release -p otf-release"));
        // Ships via a GitHub Release, idempotently — no registry, no cargo publish.
        assert!(out.contains("permissions:\n  contents: write"));
        assert!(out.contains("  github-release:\n"));
        assert!(out.contains("    needs: [check-release, build-opentf-release]\n"));
        assert!(out.contains("          if gh release view \"$tag\" >/dev/null 2>&1; then\n"));
        assert!(!out.contains("cargo publish"));
        assert!(!out.contains("crates.io"));
        // build-only cargo: no publish job at all.
        assert!(!out.contains("  publish:\n"));
    }

    #[test]
    fn generic_build_only_renders_no_toolchain_and_manifest_version() {
        let config = ReleaseConfig {
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
        assert!(!out.contains("  publish:\n"));
        assert!(!out.contains("crates.io"));
    }

    #[test]
    fn generic_publish_renders_publish_job_with_edit_me_toolchain() {
        let config = ReleaseConfig {
            adapters: vec![Ecosystem::Generic],
            packages: vec![generic_pkg("jsr-lib", Some("npx jsr publish"))],
        };
        let out = render_workflow(&config);
        assert!(out.contains("  build-jsr-lib:\n"));
        // A unified publish job runs `otf-release publish` (which runs the configured command).
        assert!(out.contains("  publish:\n"));
        assert!(out.contains("    needs: [check-release, build-jsr-lib]\n"));
        assert!(out.contains("        run: otf-release publish --artifacts-dir .artifacts\n"));
        // The tool can't know a generic registry's toolchain/secret → edit-me markers.
        assert!(out.contains("# edit me: set up the toolchain your generic publish command needs"));
        assert!(!out.contains("github-release"));
    }

    #[test]
    fn polyglot_renders_one_publish_job_and_release() {
        let config = ReleaseConfig {
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
        // cargo side is build-only: a GitHub Release depending on the cargo build.
        assert!(out.contains("  github-release:\n"));
        assert!(out.contains("    needs: [check-release, build-web-compiler]\n"));
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
