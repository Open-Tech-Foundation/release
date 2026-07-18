//! End-to-end check of the npm "tool-owns-the-build" convention, driven through the real
//! [`NpmAdapter`] and real files on disk (only the terminal prompt is scripted):
//!
//! - `init` auto-detects a package's `scripts.build` and records an inline-build publish entry —
//!   no separate build job, no cross-job artifact staging.
//! - The generated `release.yml` builds in the package's own publish job and publishes without
//!   `--artifacts-dir`.
//! - npm's pack/publish lifecycle hooks are stripped from `package.json` (surgically, leaving the
//!   rest of the file byte-stable) so npm can't re-run a build behind the pipeline.

use std::fs;
use std::path::Path;

use anyhow::Result;

use otf_release_adapters::npm::NpmAdapter;
use otf_release_core::adapter::Adapter;
use otf_release_core::config::{
    ChangelogScope, Ecosystem, GithubReleaseNotes, PackageEntry, ReleaseConfig,
};
use otf_release_core::discover::GenericCandidate;
use otf_release_core::init::{
    orchestrate, AdapterFactory, InitOptions, InitPrompt, TagFormatSuggestion,
};

struct RealNpmFactory {
    root: std::path::PathBuf,
}
impl AdapterFactory for RealNpmFactory {
    fn make(&self, ecosystem: Ecosystem) -> Box<dyn Adapter> {
        match ecosystem {
            Ecosystem::Npm => Box::new(NpmAdapter::new(self.root.clone())),
            other => panic!("unexpected ecosystem in this test: {other:?}"),
        }
    }
}

/// Scripts an npm-only, non-interactive `init` run.
struct NpmOnlyPrompt;
impl InitPrompt for NpmOnlyPrompt {
    fn select_adapters(&self) -> Result<Vec<Ecosystem>> {
        Ok(vec![Ecosystem::Npm])
    }
    fn prompt_jsr_scaffold(
        &self,
        default_name: &str,
        _default_version: &str,
        default_exports: &str,
    ) -> Result<(String, String)> {
        Ok((default_name.to_string(), default_exports.to_string()))
    }
    fn select_build_packages(
        &self,
        publishable: &[&otf_release_core::adapter::Pkg],
    ) -> Result<Vec<String>> {
        // npm packages are auto-handled, so this prompt only ever sees non-npm packages.
        assert!(
            publishable.is_empty(),
            "npm packages must not reach the build prompt"
        );
        Ok(Vec::new())
    }
    fn build_entry(&self, _: &str, _: &[Ecosystem]) -> Result<PackageEntry> {
        unreachable!("no cargo/generic packages in this test")
    }
    fn generic_packages(&self, _: &[GenericCandidate]) -> Result<Vec<PackageEntry>> {
        Ok(Vec::new())
    }
    fn confirm_overwrite(&self, _: &Path) -> Result<bool> {
        Ok(true)
    }
    fn tag_format(&self, _: &TagFormatSuggestion) -> Result<String> {
        Ok("{name}@{version}".to_string())
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

#[test]
fn npm_init_injects_inline_build_and_strips_publish_hooks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // A private workspace root with one publishable member that both builds and carries npm
    // lifecycle hooks the pipeline must take ownership of.
    fs::write(
        root.join("package.json"),
        r#"{ "name": "root", "private": true, "workspaces": ["packages/*"] }"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("packages/lib")).unwrap();
    let member_path = root.join("packages/lib/package.json");
    let member_before = "{\n  \"name\": \"@acme/lib\",\n  \"version\": \"1.2.3\",\n  \"scripts\": {\n    \"build\": \"tsc -p .\",\n    \"prepublishOnly\": \"npm run build\",\n    \"prepack\": \"npm run build\",\n    \"test\": \"vitest\"\n  }\n}\n";
    fs::write(&member_path, member_before).unwrap();

    let factory = RealNpmFactory {
        root: root.to_path_buf(),
    };
    orchestrate(&factory, &NpmOnlyPrompt, root, &InitOptions { force: true }).unwrap();

    // 1. The member's package.json keeps `build`/`test` but loses the pack/publish hooks, and is
    //    otherwise byte-for-byte unchanged.
    let member_after = fs::read_to_string(&member_path).unwrap();
    let expected_after = "{\n  \"name\": \"@acme/lib\",\n  \"version\": \"1.2.3\",\n  \"scripts\": {\n    \"build\": \"tsc -p .\",\n    \"test\": \"vitest\"\n  }\n}\n";
    assert_eq!(member_after, expected_after);

    // 2. release.toml records one inline-build npm publish entry.
    let cfg = ReleaseConfig::load(root).unwrap();
    assert_eq!(cfg.packages.len(), 1);
    let p = &cfg.packages[0];
    assert_eq!(p.name, "@acme/lib");
    assert_eq!(p.adapter, Ecosystem::Npm);
    assert_eq!(p.command, "npm run build");
    assert_eq!(p.manifest.as_deref(), Some("packages/lib/package.json"));
    assert!(p.builds_inline());

    // 3. The workflow builds in the package's own publish job (scoped to its dir) and publishes
    //    with no separate build job and no artifact staging.
    let yml = fs::read_to_string(root.join(".github/workflows/release.yml")).unwrap();
    assert!(yml.contains("  publish-acme-lib:\n"), "yml:\n{yml}");
    assert!(
        !yml.contains("  build-acme-lib:\n"),
        "no build job expected"
    );
    assert!(yml.contains("      - name: Build @acme/lib\n"));
    assert!(yml.contains("        run: npm run build\n"));
    assert!(yml.contains("        working-directory: packages/lib\n"));
    assert!(yml.contains("        run: otf-release publish --package @acme/lib\n"));
    assert!(
        !yml.contains("--artifacts-dir"),
        "no artifact staging for inline npm build"
    );
    assert!(
        !yml.contains("upload-artifact"),
        "no artifact upload for inline npm build"
    );
}
