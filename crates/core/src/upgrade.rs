//! The `upgrade` command — regenerates the GitHub workflow from the existing config.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use inquire::Confirm;

use crate::config::ReleaseConfig;
use crate::init::render_workflow_for_root;

/// Options for an `upgrade` run.
#[derive(Debug, Clone, Default)]
pub struct UpgradeOptions {
    /// Overwrite existing files (`release.yml`) without prompting.
    pub force: bool,
}

/// Load the config and regenerate the workflow.
pub fn orchestrate(root: &Path, opts: &UpgradeOptions) -> Result<()> {
    let config = ReleaseConfig::load(root)
        .context("Could not load release.toml. Are you in an initialized repo?")?;
    let yaml = render_workflow_for_root(&config, root);
    let yml_path = root.join(".github/workflows/release.yml");

    if yml_path.exists() && !opts.force {
        let overwrite = Confirm::new(&format!("Overwrite {}?", yml_path.display()))
            .with_default(false)
            .prompt()?;
        if !overwrite {
            return Ok(());
        }
    }

    fs::create_dir_all(yml_path.parent().unwrap())
        .with_context(|| format!("creating {}", yml_path.parent().unwrap().display()))?;
    fs::write(&yml_path, yaml).with_context(|| format!("writing {}", yml_path.display()))?;
    println!("Upgraded {}", yml_path.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::config::{
        ChangelogScope, ChangelogStrategy, Ecosystem, GithubReleaseNotes, Hooks, Mode,
        PackageEntry, ReleaseConfig, Target,
    };

    use super::*;

    #[test]
    fn upgrade_uses_detected_npm_tool_from_repo_root() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("bun.lock"), "").unwrap();
        let config = ReleaseConfig {
            adapters: vec![Ecosystem::Npm],
            skip_publish: Vec::new(),
            hooks: Hooks::default(),
            publish: crate::config::PublishConfig::default(),
            packages: vec![PackageEntry {
                name: "docs-site".to_string(),
                adapter: Ecosystem::Npm,
                mode: Mode::Publish,
                matrix: false,
                targets: Vec::new(),
                command: "npm run build".to_string(),
                artifacts: "dist/**".to_string(),
                bin_name: None,
                compress: None,
                manifest: None,
                version_field: None,
                publish: None,
                archive: None,
                checksums: false,
                attest: false,
                include: Vec::new(),
            }],
            snapshot_tag: None,
            tag_format: "{name}@{version}".to_string(),
            legacy_tag_formats: Vec::new(),
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
        };
        config.save(tmp.path()).unwrap();

        orchestrate(tmp.path(), &UpgradeOptions { force: true }).unwrap();

        let workflow =
            fs::read_to_string(tmp.path().join(".github/workflows/release.yml")).unwrap();
        assert!(workflow.contains("permissions:\n  contents: write  # create tags and GitHub Releases\n  id-token: write\n"));
        assert!(workflow.contains("      - uses: oven-sh/setup-bun@v2\n"));
        assert!(workflow.contains("      - run: bun install --frozen-lockfile\n"));
        assert!(!workflow.contains("      - run: npm ci\n"));
    }

    #[test]
    fn upgrade_regenerates_publish_gating_and_concurrency() {
        // `upgrade` reads release.toml and regenerates the workflow through the same renderer as
        // `init`, so an existing repo picks up the ordering fix, the concurrency group, and the
        // dropped Windows install steps just by running `otf-release upgrade`.
        let tmp = tempfile::tempdir().unwrap();
        let config = ReleaseConfig {
            adapters: vec![Ecosystem::Npm],
            skip_publish: Vec::new(),
            hooks: Hooks::default(),
            publish: crate::config::PublishConfig::default(),
            packages: vec![PackageEntry {
                name: "@opentf/web-compiler".to_string(),
                adapter: Ecosystem::Npm,
                mode: Mode::Publish,
                matrix: true,
                targets: vec![Target::resolved("linux", "aarch64")],
                command: "cargo build --release --target {triple}".to_string(),
                artifacts: "target/{triple}/release/otfwc{ext}".to_string(),
                bin_name: Some("otfwc".to_string()),
                compress: Some("brotli".to_string()),
                manifest: None,
                version_field: None,
                publish: None,
                archive: None,
                checksums: false,
                attest: false,
                include: Vec::new(),
            }],
            snapshot_tag: None,
            tag_format: "{name}@{version}".to_string(),
            legacy_tag_formats: Vec::new(),
            provider: "github".to_string(),
            default_branch: "main".to_string(),
            changelog_strategy: ChangelogStrategy::Curated,
            changelog_scope: ChangelogScope::Package,
            github_release_notes: GithubReleaseNotes::AutoGenerate,
        };
        config.save(tmp.path()).unwrap();

        orchestrate(tmp.path(), &UpgradeOptions { force: true }).unwrap();

        let workflow =
            fs::read_to_string(tmp.path().join(".github/workflows/release.yml")).unwrap();
        assert!(workflow
            .contains("  publish:\n    needs: [check-release, publish-opentf-web-compiler]\n"));
        assert!(workflow.contains("    if: >-\n      always() &&"));
        assert!(workflow.contains("      needs.publish-opentf-web-compiler.result != 'failure'"));
        assert!(
            workflow.contains("\nconcurrency:\n  group: release\n  cancel-in-progress: false\n")
        );
    }
}
