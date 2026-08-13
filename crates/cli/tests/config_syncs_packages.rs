//! `otf-release config` must leave a releasable repo behind.
//!
//! Enabling an ecosystem used to be only half an answer: `[discovery]` was written, the packages
//! were discovered, and not one `[[package]]` block existed for them. That state has no build step
//! for a package whose publish needs one (npm packs an unbuilt `dist/`) and nowhere to scope a
//! per-package `tag_format`, so two independently versioned packages collide on the repo's tag.
//! Only `init` could finish it, and `init` rewrites the whole file.
//!
//! Driven through the real [`NpmAdapter`] and [`CargoAdapter`] against real files — only the
//! terminal prompt is scripted.

use std::fs;
use std::path::Path;

use anyhow::Result;

use otf_release_adapters::cargo::CargoAdapter;
use otf_release_adapters::npm::NpmAdapter;
use otf_release_core::adapter::Adapter;
use otf_release_core::config::{
    ChangelogScope, ChangelogStrategy, Discovery, Ecosystem, GithubReleaseNotes, Mode,
    PackageEntry, ReleaseConfig, Target,
};
use otf_release_core::config_cmd::{
    ConfigAction, ConfigPrompt, GlobalField, HookStage, NewPackageAction, PackageField,
};
use otf_release_core::discover::GenericCandidate;
use otf_release_core::init::AdapterFactory;

struct RealFactory {
    root: std::path::PathBuf,
    discovery: Discovery,
}
impl AdapterFactory for RealFactory {
    fn make(&self, ecosystem: Ecosystem) -> Box<dyn Adapter> {
        self.make_with_discovery(ecosystem, &self.discovery)
    }
    fn make_with_discovery(&self, ecosystem: Ecosystem, discovery: &Discovery) -> Box<dyn Adapter> {
        match ecosystem {
            Ecosystem::Npm => {
                Box::new(NpmAdapter::new(self.root.clone()).with_packages(discovery.npm.clone()))
            }
            Ecosystem::Cargo => Box::new(CargoAdapter::new(self.root.clone())),
            other => panic!("unexpected ecosystem in this test: {other:?}"),
        }
    }
}

/// Opens *Ecosystems*, confirms exactly what is already enabled and already declared — the
/// "walk in, press enter, walk out" run — then exits.
struct ConfirmEcosystems {
    steps: std::cell::RefCell<Vec<ConfigAction>>,
}
impl ConfirmEcosystems {
    fn new() -> Self {
        Self {
            steps: std::cell::RefCell::new(vec![ConfigAction::Ecosystems, ConfigAction::Exit]),
        }
    }
}
impl ConfigPrompt for ConfirmEcosystems {
    fn action(&self) -> Result<ConfigAction> {
        Ok(self.steps.borrow_mut().remove(0))
    }
    fn ecosystems(&self, current: &[Ecosystem]) -> Result<Option<Vec<Ecosystem>>> {
        Ok(Some(current.to_vec()))
    }
    fn npm_packages(
        &self,
        found: &[GenericCandidate],
        defaults: &[usize],
    ) -> Result<Option<Vec<usize>>> {
        // Confirm the declared set unchanged.
        let _ = found;
        Ok(Some(defaults.to_vec()))
    }
    fn hook_stage(&self) -> Result<HookStage> {
        unreachable!()
    }
    fn package<'a>(&self, _: &'a [PackageEntry]) -> Result<Option<&'a str>> {
        unreachable!()
    }
    fn package_to_edit(&self, _: &[PackageEntry], _: &[String]) -> Result<Option<String>> {
        unreachable!()
    }
    fn new_package(&self, _: &str) -> Result<NewPackageAction> {
        unreachable!()
    }
    fn package_field(&self, _: &PackageEntry) -> Result<PackageField> {
        unreachable!()
    }
    fn mode(&self, _: Mode) -> Result<Option<Mode>> {
        unreachable!()
    }
    fn global_field(&self) -> Result<GlobalField> {
        unreachable!()
    }
    fn changelog_scope(&self, _: &ChangelogScope) -> Result<Option<ChangelogScope>> {
        unreachable!()
    }
    fn changelog_strategy(&self, _: &ChangelogStrategy) -> Result<Option<ChangelogStrategy>> {
        unreachable!()
    }
    fn github_release_notes(&self, _: &GithubReleaseNotes) -> Result<Option<GithubReleaseNotes>> {
        unreachable!()
    }
    fn tag_format(&self, _: &str) -> Result<Option<String>> {
        unreachable!()
    }
    fn targets(&self, _: &[Target]) -> Result<Option<Vec<Target>>> {
        unreachable!()
    }
    fn toggle(&self, _: &str, _: &str, _: bool) -> Result<Option<bool>> {
        unreachable!()
    }
    fn text(&self, _: &str, _: &str) -> Result<Option<String>> {
        unreachable!()
    }
}

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

/// A polyglot repo shaped like ES-Runtime: a Cargo workspace whose binary crate ships as a GitHub
/// Release, plus npm packages under `packages/` and no root `package.json`.
fn repo(root: &Path) {
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/runtime-cli\"]\n\n[workspace.package]\nversion = \"0.23.0\"\n",
    );
    write(
        &root.join("crates/runtime-cli/Cargo.toml"),
        "[package]\nname = \"es-runtime-cli\"\nversion.workspace = true\n",
    );
    write(
        &root.join("crates/runtime-cli/src/main.rs"),
        "fn main() {}\n",
    );
    write(
        &root.join("packages/postgres/package.json"),
        r#"{
  "name": "@opentf/esrun-postgres",
  "version": "0.0.1",
  "files": ["dist"],
  "scripts": { "build": "bun run build.mjs", "prepublishOnly": "npm run build" }
}
"#,
    );
    write(
        &root.join("packages/redis/package.json"),
        r#"{ "name": "@opentf/esrun-redis", "version": "0.0.1", "scripts": { "build": "bun run build.mjs" } }"#,
    );
    // No build script: published as-is by the catch-all job.
    write(
        &root.join("packages/types/package.json"),
        r#"{ "name": "@opentf/esrun-types", "version": "0.1.0" }"#,
    );
}

/// The exact `release.toml` this repo had after enabling npm through `config`: members declared,
/// no block for a single npm package.
fn half_configured() -> ReleaseConfig {
    ReleaseConfig {
        adapters: vec![Ecosystem::Npm, Ecosystem::Cargo],
        skip_publish: vec![],
        discovery: Discovery {
            npm: vec![
                "packages/postgres".to_string(),
                "packages/redis".to_string(),
                "packages/types".to_string(),
            ],
        },
        changelog_scope: ChangelogScope::Root,
        github_release_notes: GithubReleaseNotes::CuratedChangelog,
        packages: vec![PackageEntry {
            name: "es-runtime-cli".to_string(),
            adapter: Ecosystem::Cargo,
            mode: Mode::BuildOnly,
            matrix: true,
            targets: vec![Target {
                name: "linux".to_string(),
                arch: "x86_64".to_string(),
                ..Target::default()
            }],
            command: "cargo build --release --target {triple}".to_string(),
            artifacts: "target/{triple}/release/{bin}{ext}".to_string(),
            bin_name: Some("esrun".to_string()),
            compress: None,
            manifest: None,
            version_field: None,
            publish: None,
            archive: None,
            checksums: false,
            attest: false,
            include: Vec::new(),
            executable: None,
            tag_format: None,
            changelog: None,
        }],
        ..ReleaseConfig::default()
    }
}

#[test]
fn config_finishes_configuring_every_discovered_package() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    repo(root);
    half_configured().save(root).unwrap();

    let factory = RealFactory {
        root: root.to_path_buf(),
        discovery: half_configured().discovery,
    };
    otf_release_core::config_cmd::orchestrate_with_prompt(
        root,
        &factory,
        &ConfirmEcosystems::new(),
    )
    .unwrap();

    let saved = ReleaseConfig::load(root).unwrap();
    let names: Vec<&str> = saved.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "@opentf/esrun-postgres",
            "@opentf/esrun-redis",
            "@opentf/esrun-types",
            "es-runtime-cli",
        ],
        "every discovered package needs a block"
    );

    // The two drivers publish a built `dist/`, so their block carries the build the pipeline owns.
    for name in ["@opentf/esrun-postgres", "@opentf/esrun-redis"] {
        let entry = saved.package(name).unwrap();
        assert_eq!(entry.command, "npm run build", "{name}");
        assert_eq!(entry.adapter, Ecosystem::Npm, "{name}");
        assert_eq!(entry.mode, Mode::Publish, "{name}");
    }
    // The types package has no build script: identity only, published as-is.
    let types = saved.package("@opentf/esrun-types").unwrap();
    assert!(types.command.is_empty());
    assert_eq!(
        types.manifest.as_deref(),
        Some("packages/types/package.json")
    );

    // The hand-tuned build matrix is untouched.
    let cli = saved.package("es-runtime-cli").unwrap();
    assert_eq!(cli.bin_name.as_deref(), Some("esrun"));
    assert_eq!(cli.targets.len(), 1);

    // npm's own publish hook is stripped, so it cannot re-run the build behind the pipeline.
    let manifest = fs::read_to_string(root.join("packages/postgres/package.json")).unwrap();
    assert!(!manifest.contains("prepublishOnly"), "{manifest}");
    assert!(manifest.contains("\"build\""), "{manifest}");

    // And every npm package is now reachable in `config` → Packages, so a per-package tag format
    // can be scoped — the whole point of the block.
    let mut scoped = saved;
    for entry in &mut scoped.packages {
        if entry.adapter == Ecosystem::Npm {
            entry.tag_format = Some("{name}@{version}".to_string());
        }
    }
    let tags = scoped.tag_formats();
    assert_eq!(
        tags.tag_for("@opentf/esrun-types", "0.1.0").unwrap(),
        "@opentf/esrun-types@0.1.0"
    );
    assert_eq!(tags.tag_for("es-runtime-cli", "0.24.0").unwrap(), "v0.24.0");
}
