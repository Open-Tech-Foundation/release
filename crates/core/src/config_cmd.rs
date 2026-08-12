use std::path::Path;

use anyhow::Result;
use inquire::{MultiSelect, Select, Text};

use crate::config::{
    format_tag, ChangelogScope, ChangelogStrategy, Ecosystem, GithubReleaseNotes, Mode,
    PackageEntry, ReleaseConfig, Target, COMMON_TAG_FORMATS, DEFAULT_VERSION_FIELD,
};
use crate::discover::{declares_npm_workspaces, scan_npm_candidates, GenericCandidate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigAction {
    LifecycleHooks,
    Ecosystems,
    Packages,
    GlobalSettings,
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookStage {
    PreVersion,
    PostVersion,
    PrePublish,
    PostPublish,
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageField {
    Mode,
    Command,
    Artifacts,
    Targets,
    Checksums,
    Attest,
    TagFormat,
    Changelog,
    GenericManifest,
    GenericVersionField,
    GenericPublishCommand,
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalField {
    Provider,
    SnapshotTag,
    SkipPublish,
    PublishIgnorePaths,
    TagFormat,
    LegacyTagFormats,
    ChangelogScope,
    ChangelogStrategy,
    GithubReleaseNotes,
    Back,
}

pub trait ConfigPrompt {
    fn action(&self) -> Result<ConfigAction>;
    fn hook_stage(&self) -> Result<HookStage>;
    fn ecosystems(&self, current: &[Ecosystem]) -> Result<Vec<Ecosystem>>;
    /// Confirm which scanned npm packages this repo releases, returning indices into `found`.
    /// `defaults` are the ones to start checked.
    fn npm_packages(&self, found: &[GenericCandidate], defaults: &[usize]) -> Result<Vec<usize>>;
    fn package<'a>(&self, packages: &'a [PackageEntry]) -> Result<Option<&'a str>>;
    fn package_field(&self, package: &PackageEntry) -> Result<PackageField>;
    fn mode(&self, current: Mode) -> Result<Mode>;
    fn global_field(&self) -> Result<GlobalField>;
    fn changelog_scope(&self, current: &ChangelogScope) -> Result<ChangelogScope>;
    fn changelog_strategy(&self, current: &ChangelogStrategy) -> Result<ChangelogStrategy>;
    fn github_release_notes(&self, current: &GithubReleaseNotes) -> Result<GithubReleaseNotes>;
    fn tag_format(&self, current: &str) -> Result<String>;
    /// Re-pick a package's build targets, with the configured ones pre-checked.
    fn targets(&self, current: &[Target]) -> Result<Vec<Target>>;
    /// Flip an on/off package flag, starting on its current value.
    fn toggle(&self, prompt: &str, help: &str, current: bool) -> Result<bool>;
    fn text(&self, prompt: &str, current: &str) -> Result<String>;
}

pub struct StdinConfigPrompt;

impl ConfigPrompt for StdinConfigPrompt {
    fn action(&self) -> Result<ConfigAction> {
        let choices = vec![
            "Lifecycle Hooks",
            "Ecosystems",
            "Packages",
            "Global Settings",
            "Exit",
        ];
        Ok(
            match Select::new("What would you like to configure?", choices).prompt()? {
                "Lifecycle Hooks" => ConfigAction::LifecycleHooks,
                "Ecosystems" => ConfigAction::Ecosystems,
                "Packages" => ConfigAction::Packages,
                "Global Settings" => ConfigAction::GlobalSettings,
                _ => ConfigAction::Exit,
            },
        )
    }

    fn npm_packages(&self, found: &[GenericCandidate], defaults: &[usize]) -> Result<Vec<usize>> {
        let labels: Vec<String> = found.iter().map(GenericCandidate::label).collect();
        let chosen = MultiSelect::new("Which of these does this repo release?", labels)
            .with_default(defaults)
            .with_help_message(
                "saved as [discovery] npm in release.toml, so version/check/publish all read the \
                 same set — leave out fixtures, examples, and anything you never publish",
            )
            .raw_prompt()?;
        Ok(chosen.iter().map(|o| o.index).collect())
    }

    fn hook_stage(&self) -> Result<HookStage> {
        let choices = vec![
            "pre_version",
            "post_version",
            "pre_publish",
            "post_publish",
            "Back",
        ];
        Ok(match Select::new("Which hook stage?", choices).prompt()? {
            "pre_version" => HookStage::PreVersion,
            "post_version" => HookStage::PostVersion,
            "pre_publish" => HookStage::PrePublish,
            "post_publish" => HookStage::PostPublish,
            _ => HookStage::Back,
        })
    }

    fn ecosystems(&self, current: &[Ecosystem]) -> Result<Vec<Ecosystem>> {
        let labels: Vec<&str> = Ecosystem::ALL.iter().map(|e| e.label()).collect();
        let defaults: Vec<usize> = current
            .iter()
            .filter_map(|a| Ecosystem::ALL.iter().position(|e| e == a))
            .collect();
        let chosen = MultiSelect::new("Enabled Ecosystems:", labels)
            .with_default(&defaults)
            .prompt()?;
        Ok(Ecosystem::ALL
            .iter()
            .copied()
            .filter(|eco| chosen.contains(&eco.label()))
            .collect())
    }

    fn package<'a>(&self, packages: &'a [PackageEntry]) -> Result<Option<&'a str>> {
        if packages.is_empty() {
            println!("No configured packages in release.toml.");
            return Ok(None);
        }
        let mut names: Vec<String> = packages.iter().map(|p| p.name.clone()).collect();
        names.push("Back".to_string());
        let chosen = Select::new("Which package?", names).prompt()?;
        if chosen == "Back" {
            Ok(None)
        } else {
            Ok(Some(
                packages
                    .iter()
                    .find(|p| p.name == chosen)
                    .map(|p| p.name.as_str())
                    .unwrap_or(""),
            ))
        }
    }

    fn package_field(&self, package: &PackageEntry) -> Result<PackageField> {
        let mut choices = vec!["Mode", "Build command", "Artifacts", "Build targets"];
        // Both only affect assets attached to a GitHub Release, which is a build-only concern.
        if package.is_build_only() {
            choices.extend(["Checksums", "Build provenance"]);
        }
        if package.adapter == Ecosystem::Generic {
            choices.extend([
                "Generic manifest",
                "Generic version field",
                "Generic publish command",
            ]);
        }
        choices.push("Back");
        Ok(
            match Select::new("Which package field?", choices).prompt()? {
                "Mode" => PackageField::Mode,
                "Build command" => PackageField::Command,
                "Artifacts" => PackageField::Artifacts,
                "Build targets" => PackageField::Targets,
                "Checksums" => PackageField::Checksums,
                "Build provenance" => PackageField::Attest,
                "Generic manifest" => PackageField::GenericManifest,
                "Generic version field" => PackageField::GenericVersionField,
                "Generic publish command" => PackageField::GenericPublishCommand,
                _ => PackageField::Back,
            },
        )
    }

    fn mode(&self, current: Mode) -> Result<Mode> {
        let choices = vec!["publish", "build-only"];
        let default = match current {
            Mode::Publish => 0,
            Mode::BuildOnly => 1,
        };
        Ok(
            match Select::new("Package mode:", choices)
                .with_starting_cursor(default)
                .prompt()?
            {
                "publish" => Mode::Publish,
                _ => Mode::BuildOnly,
            },
        )
    }

    fn global_field(&self) -> Result<GlobalField> {
        let choices = vec![
            "Provider",
            "Snapshot tag",
            "Skip publish packages",
            "Publish ignore paths",
            "Tag format",
            "Legacy tag formats",
            "Changelog scope",
            "Changelog strategy",
            "GitHub Release notes",
            "Back",
        ];
        Ok(
            match Select::new("Which global setting?", choices).prompt()? {
                "Provider" => GlobalField::Provider,
                "Snapshot tag" => GlobalField::SnapshotTag,
                "Skip publish packages" => GlobalField::SkipPublish,
                "Publish ignore paths" => GlobalField::PublishIgnorePaths,
                "Tag format" => GlobalField::TagFormat,
                "Legacy tag formats" => GlobalField::LegacyTagFormats,
                "Changelog scope" => GlobalField::ChangelogScope,
                "Changelog strategy" => GlobalField::ChangelogStrategy,
                "GitHub Release notes" => GlobalField::GithubReleaseNotes,
                _ => GlobalField::Back,
            },
        )
    }

    fn changelog_scope(&self, current: &ChangelogScope) -> Result<ChangelogScope> {
        let choices = vec!["root", "package"];
        let default = match current {
            ChangelogScope::Root => 0,
            ChangelogScope::Package => 1,
        };
        Ok(
            match Select::new("Changelog scope:", choices)
                .with_starting_cursor(default)
                .prompt()?
            {
                "root" => ChangelogScope::Root,
                _ => ChangelogScope::Package,
            },
        )
    }

    fn changelog_strategy(&self, current: &ChangelogStrategy) -> Result<ChangelogStrategy> {
        let choices = vec!["curated", "generated"];
        let default = match current {
            ChangelogStrategy::Curated => 0,
            ChangelogStrategy::Generated => 1,
        };
        Ok(
            match Select::new("Changelog strategy:", choices)
                .with_starting_cursor(default)
                .prompt()?
            {
                "generated" => ChangelogStrategy::Generated,
                _ => ChangelogStrategy::Curated,
            },
        )
    }

    fn github_release_notes(&self, current: &GithubReleaseNotes) -> Result<GithubReleaseNotes> {
        let choices = vec!["auto-generate", "curated-changelog", "semantic-commits"];
        let default = match current {
            GithubReleaseNotes::AutoGenerate => 0,
            GithubReleaseNotes::CuratedChangelog => 1,
            GithubReleaseNotes::SemanticCommits => 2,
        };
        Ok(
            match Select::new("GitHub Release notes:", choices)
                .with_starting_cursor(default)
                .prompt()?
            {
                "curated-changelog" => GithubReleaseNotes::CuratedChangelog,
                "semantic-commits" => GithubReleaseNotes::SemanticCommits,
                _ => GithubReleaseNotes::AutoGenerate,
            },
        )
    }

    fn tag_format(&self, current: &str) -> Result<String> {
        let mut choices: Vec<String> = COMMON_TAG_FORMATS
            .iter()
            .map(|format| {
                if *format == current {
                    format!("{format} (current)")
                } else {
                    (*format).to_string()
                }
            })
            .collect();
        choices.push("Custom".to_string());
        let default = COMMON_TAG_FORMATS
            .iter()
            .position(|format| *format == current)
            .unwrap_or(0);
        let selected = Select::new("Tag format:", choices)
            .with_starting_cursor(default)
            .prompt()?;
        if selected == "Custom" {
            Text::new("Custom tag format:")
                .with_default(current)
                .prompt()
                .map_err(Into::into)
        } else {
            Ok(selected
                .strip_suffix(" (current)")
                .unwrap_or(&selected)
                .to_string())
        }
    }

    fn targets(&self, current: &[Target]) -> Result<Vec<Target>> {
        crate::init::pick_targets("Build targets:", current, crate::init::EDIT_TARGETS_HELP)
    }

    fn toggle(&self, prompt: &str, help: &str, current: bool) -> Result<bool> {
        Ok(Select::new(prompt, vec!["Yes", "No"])
            .with_starting_cursor(usize::from(!current))
            .with_help_message(help)
            .raw_prompt()?
            .index
            == 0)
    }

    fn text(&self, prompt: &str, current: &str) -> Result<String> {
        Ok(Text::new(prompt).with_initial_value(current).prompt()?)
    }
}

pub fn orchestrate(root: &Path) -> Result<()> {
    orchestrate_with_prompt(root, &StdinConfigPrompt)
}

pub fn orchestrate_with_prompt(root: &Path, prompt: &dyn ConfigPrompt) -> Result<()> {
    let mut config = ReleaseConfig::load(root)?;

    loop {
        match prompt.action()? {
            ConfigAction::LifecycleHooks => edit_hooks(root, prompt, &mut config)?,
            ConfigAction::Ecosystems => {
                config.adapters = prompt.ecosystems(&config.adapters)?;
                edit_npm_discovery(root, prompt, &mut config)?;
                save(root, &config)?;
            }
            ConfigAction::Packages => edit_package(root, prompt, &mut config)?,
            ConfigAction::GlobalSettings => edit_global(root, prompt, &mut config)?,
            ConfigAction::Exit => break,
        }
    }

    Ok(())
}

/// Settle where npm's packages live, right after npm is enabled.
///
/// Skipped entirely when the repo declares `workspaces` in a root `package.json` — that
/// declaration already answers the question, and a second one here would only drift from it. When
/// there is none (a Cargo-rooted repo whose JS packages are independent projects, say), the repo
/// scan proposes what it found and the confirmed set is written to `[discovery] npm`. Re-running
/// this re-scans and starts from what is already declared, so a package added later shows up.
fn edit_npm_discovery(
    root: &Path,
    prompt: &dyn ConfigPrompt,
    config: &mut ReleaseConfig,
) -> Result<()> {
    if !config.adapters.contains(&Ecosystem::Npm) {
        config.discovery.npm.clear();
        return Ok(());
    }
    if declares_npm_workspaces(root) {
        println!(
            "npm packages come from the `workspaces` globs in the root package.json — nothing to \
             configure here."
        );
        return Ok(());
    }

    let found = scan_npm_candidates(root);
    if found.is_empty() {
        println!(
            "No package.json with a name and a version found — npm will discover no packages. \
             Add the package directories to `[discovery] npm` in release.toml by hand, or declare \
             `workspaces` in a root package.json."
        );
        return Ok(());
    }

    println!("\nFound {} npm package(s) in this repo:", found.len());
    for c in &found {
        println!("  {}", c.label());
    }

    // Start from what is already declared; on a first run nothing is, so the publishable ones are
    // checked and the private ones (apps, fixtures) are not.
    let declared = &config.discovery.npm;
    let defaults: Vec<usize> = found
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            if declared.is_empty() {
                !c.private
            } else {
                declared.contains(&c.dir())
            }
        })
        .map(|(i, _)| i)
        .collect();

    let chosen = prompt.npm_packages(&found, &defaults)?;
    config.discovery.npm = chosen
        .into_iter()
        .filter_map(|i| found.get(i))
        .map(GenericCandidate::dir)
        .collect();
    Ok(())
}

fn edit_hooks(root: &Path, prompt: &dyn ConfigPrompt, config: &mut ReleaseConfig) -> Result<()> {
    let stage = prompt.hook_stage()?;
    if stage == HookStage::Back {
        return Ok(());
    }

    let current = match stage {
        HookStage::PreVersion => &config.hooks.pre_version,
        HookStage::PostVersion => &config.hooks.post_version,
        HookStage::PrePublish => &config.hooks.pre_publish,
        HookStage::PostPublish => &config.hooks.post_publish,
        HookStage::Back => unreachable!(),
    };

    let current_str = current.join(", ");
    let edited = prompt.text(
        &format!("Commands for {} (comma-separated):", stage.label()),
        &current_str,
    )?;
    let new_hooks = parse_csv(&edited);

    match stage {
        HookStage::PreVersion => config.hooks.pre_version = new_hooks,
        HookStage::PostVersion => config.hooks.post_version = new_hooks,
        HookStage::PrePublish => config.hooks.pre_publish = new_hooks,
        HookStage::PostPublish => config.hooks.post_publish = new_hooks,
        HookStage::Back => unreachable!(),
    }
    save(root, config)
}

fn edit_package(root: &Path, prompt: &dyn ConfigPrompt, config: &mut ReleaseConfig) -> Result<()> {
    let Some(name) = prompt.package(&config.packages)? else {
        return Ok(());
    };
    let Some(idx) = config.packages.iter().position(|p| p.name == name) else {
        return Ok(());
    };

    let field = prompt.package_field(&config.packages[idx])?;
    if field == PackageField::Back {
        return Ok(());
    }

    // Read the repo-wide defaults before borrowing the entry mutably — the per-package prompts name
    // them, so "blank" visibly means "whatever the repo does".
    let tag_format = config.tag_format.clone();
    let scope = scope_label(&config.changelog_scope);
    let package = &mut config.packages[idx];
    let name = package.name.clone();
    let name = name.as_str();
    match field {
        PackageField::Mode => package.mode = prompt.mode(package.mode)?,
        PackageField::Command => {
            package.command = prompt.text("Build command:", &package.command)?;
        }
        PackageField::Artifacts => {
            package.artifacts = prompt.text("Artifacts glob:", &package.artifacts)?;
        }
        PackageField::Targets => {
            package.targets = prompt.targets(&package.targets)?;
            package.matrix = !package.targets.is_empty();
        }
        PackageField::Checksums => {
            // Read by `github-release` at release time, so the workflow YAML is unaffected.
            package.checksums = prompt.toggle(
                "Attach a checksums.txt (SHA-256)?",
                "one combined checksums.txt covering every asset on the release",
                package.checksums,
            )?;
        }
        PackageField::Attest => {
            package.attest = prompt.toggle(
                "Generate signed build provenance?",
                "proves each asset was built by this repo's workflow from this commit; verified \
                 with `gh attestation verify <file> --repo <owner/repo>`",
                package.attest,
            )?;
            if package.attest {
                // Unlike checksums, this adds an `attestations: write` permission and a signing
                // step to the workflow, which only `upgrade` can write.
                println!("Run `otf-release upgrade` to regenerate the workflow with attestation.");
            }
        }
        PackageField::TagFormat => {
            let current = package.tag_format.as_deref().unwrap_or("");
            let edited = optional_text(prompt.text(
                &format!("Tag format for {name} (blank = the repo's `{tag_format}`):"),
                current,
            )?);
            if let Some(format) = &edited {
                format_tag(format, name, "1.2.3")?;
            }
            package.tag_format = edited;
        }
        PackageField::Changelog => {
            let current = package.changelog.as_deref().unwrap_or("");
            let edited = optional_text(prompt.text(
                &format!(
                    "Changelog for {name}, relative to the repo root (blank = {scope} scope):"
                ),
                current,
            )?);
            package.changelog = edited;
            package.validate_release_identity()?;
        }
        PackageField::GenericManifest => {
            let current = package.manifest.as_deref().unwrap_or("");
            package.manifest = optional_text(prompt.text("Generic manifest:", current)?);
        }
        PackageField::GenericVersionField => {
            let current = package
                .version_field
                .as_deref()
                .unwrap_or(DEFAULT_VERSION_FIELD);
            package.version_field = optional_text(prompt.text("Generic version field:", current)?);
        }
        PackageField::GenericPublishCommand => {
            let current = package.publish.as_deref().unwrap_or("");
            package.publish = optional_text(prompt.text("Generic publish command:", current)?);
        }
        PackageField::Back => unreachable!(),
    }

    save(root, config)
}

fn edit_global(root: &Path, prompt: &dyn ConfigPrompt, config: &mut ReleaseConfig) -> Result<()> {
    match prompt.global_field()? {
        GlobalField::Provider => {
            config.provider = prompt.text("Provider:", &config.provider)?;
            save(root, config)
        }
        GlobalField::SnapshotTag => {
            let current = config.snapshot_tag.as_deref().unwrap_or("");
            config.snapshot_tag = optional_text(prompt.text("Snapshot tag:", current)?);
            save(root, config)
        }
        GlobalField::SkipPublish => {
            let current = config.skip_publish.join(", ");
            let edited = prompt.text("Skip publish packages (comma-separated):", &current)?;
            config.skip_publish = parse_csv(&edited);
            save(root, config)
        }
        GlobalField::PublishIgnorePaths => {
            let choices = publish_ignore_path_packages(config);
            let Some(name) = prompt.package(&choices)? else {
                return Ok(());
            };
            let current = config
                .publish
                .ignore_paths
                .get(name)
                .map(|paths| paths.join(", "))
                .unwrap_or_default();
            let edited = prompt.text(
                &format!("Ignored publish paths for {name} (comma-separated globs):"),
                &current,
            )?;
            config
                .publish
                .ignore_paths
                .insert(name.to_string(), parse_csv(&edited));
            save(root, config)
        }
        GlobalField::TagFormat => {
            let tag_format = prompt.tag_format(&config.tag_format)?;
            format_tag(&tag_format, "package", "1.2.3")?;
            config.tag_format = tag_format;
            save(root, config)
        }
        GlobalField::LegacyTagFormats => {
            let current = config.legacy_tag_formats.join(", ");
            let edited = prompt.text("Legacy tag formats (comma-separated):", &current)?;
            let legacy_tag_formats = parse_legacy_tag_formats(&edited)?;
            config.legacy_tag_formats = legacy_tag_formats;
            save(root, config)
        }
        GlobalField::ChangelogScope => {
            config.changelog_scope = prompt.changelog_scope(&config.changelog_scope)?;
            save(root, config)
        }
        GlobalField::ChangelogStrategy => {
            config.changelog_strategy = prompt.changelog_strategy(&config.changelog_strategy)?;
            save(root, config)
        }
        GlobalField::GithubReleaseNotes => {
            config.github_release_notes =
                prompt.github_release_notes(&config.github_release_notes)?;
            save(root, config)
        }
        GlobalField::Back => Ok(()),
    }
}

impl HookStage {
    fn label(self) -> &'static str {
        match self {
            HookStage::PreVersion => "pre_version",
            HookStage::PostVersion => "post_version",
            HookStage::PrePublish => "pre_publish",
            HookStage::PostPublish => "post_publish",
            HookStage::Back => "back",
        }
    }
}

fn save(root: &Path, config: &ReleaseConfig) -> Result<()> {
    config.save(root)?;
    println!("Saved.");
    Ok(())
}

fn parse_csv(text: &str) -> Vec<String> {
    text.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_legacy_tag_formats(text: &str) -> Result<Vec<String>> {
    let formats = parse_csv(text);
    for format in &formats {
        format_tag(format, "package", "1.2.3")?;
    }
    Ok(formats)
}

fn optional_text(text: String) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn scope_label(scope: &ChangelogScope) -> &'static str {
    match scope {
        ChangelogScope::Root => "root",
        ChangelogScope::Package => "package",
    }
}

fn publish_ignore_path_packages(config: &ReleaseConfig) -> Vec<PackageEntry> {
    let mut names: Vec<String> = config.publish.ignore_paths.keys().cloned().collect();
    names.extend(config.packages.iter().map(|pkg| pkg.name.clone()));
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|name| PackageEntry {
            name,
            adapter: Ecosystem::Generic,
            mode: Mode::Publish,
            matrix: false,
            targets: Vec::new(),
            command: String::new(),
            artifacts: String::new(),
            bin_name: None,
            compress: None,
            manifest: None,
            version_field: None,
            publish: None,
            archive: None,
            checksums: false,
            attest: false,
            executable: None,
            include: Vec::new(),
            tag_format: None,
            changelog: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::Path;

    struct FakePrompt {
        actions: RefCell<Vec<ConfigAction>>,
        package: RefCell<Option<String>>,
        package_field: RefCell<PackageField>,
        mode: RefCell<Mode>,
        global_field: RefCell<GlobalField>,
        scope: RefCell<ChangelogScope>,
        strategy: RefCell<ChangelogStrategy>,
        github_release_notes: RefCell<GithubReleaseNotes>,
        targets: RefCell<Vec<Target>>,
        toggle: RefCell<bool>,
        text: RefCell<Vec<String>>,
    }

    impl Default for FakePrompt {
        fn default() -> Self {
            Self {
                actions: RefCell::new(vec![ConfigAction::Exit]),
                package: RefCell::new(None),
                package_field: RefCell::new(PackageField::Back),
                mode: RefCell::new(Mode::BuildOnly),
                global_field: RefCell::new(GlobalField::Back),
                scope: RefCell::new(ChangelogScope::Package),
                strategy: RefCell::new(ChangelogStrategy::Curated),
                github_release_notes: RefCell::new(GithubReleaseNotes::AutoGenerate),
                targets: RefCell::new(Vec::new()),
                toggle: RefCell::new(false),
                text: RefCell::new(Vec::new()),
            }
        }
    }

    impl ConfigPrompt for FakePrompt {
        fn action(&self) -> Result<ConfigAction> {
            Ok(self.actions.borrow_mut().remove(0))
        }

        fn hook_stage(&self) -> Result<HookStage> {
            Ok(HookStage::Back)
        }

        fn npm_packages(
            &self,
            _found: &[GenericCandidate],
            defaults: &[usize],
        ) -> Result<Vec<usize>> {
            Ok(defaults.to_vec())
        }

        fn ecosystems(&self, _current: &[Ecosystem]) -> Result<Vec<Ecosystem>> {
            Ok(vec![Ecosystem::Npm, Ecosystem::Generic])
        }

        fn package<'a>(&self, packages: &'a [PackageEntry]) -> Result<Option<&'a str>> {
            let Some(name) = self.package.borrow().clone() else {
                return Ok(None);
            };
            Ok(packages
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.name.as_str()))
        }

        fn package_field(&self, _package: &PackageEntry) -> Result<PackageField> {
            Ok(*self.package_field.borrow())
        }

        fn mode(&self, _current: Mode) -> Result<Mode> {
            Ok(*self.mode.borrow())
        }

        fn global_field(&self) -> Result<GlobalField> {
            Ok(*self.global_field.borrow())
        }

        fn changelog_scope(&self, _current: &ChangelogScope) -> Result<ChangelogScope> {
            Ok(self.scope.borrow().clone())
        }

        fn changelog_strategy(&self, _current: &ChangelogStrategy) -> Result<ChangelogStrategy> {
            Ok(self.strategy.borrow().clone())
        }

        fn github_release_notes(
            &self,
            _current: &GithubReleaseNotes,
        ) -> Result<GithubReleaseNotes> {
            Ok(self.github_release_notes.borrow().clone())
        }

        fn tag_format(&self, _current: &str) -> Result<String> {
            Ok(self.text.borrow_mut().remove(0))
        }

        fn targets(&self, _current: &[Target]) -> Result<Vec<Target>> {
            Ok(self.targets.borrow().clone())
        }

        fn toggle(&self, _prompt: &str, _help: &str, _current: bool) -> Result<bool> {
            Ok(*self.toggle.borrow())
        }

        fn text(&self, _prompt: &str, _current: &str) -> Result<String> {
            Ok(self.text.borrow_mut().remove(0))
        }
    }

    #[test]
    fn toggles_checksums_and_attest_on_a_build_only_package() {
        let tmp = tempfile::tempdir().unwrap();
        let mut base = config();
        base.packages[0].mode = Mode::BuildOnly;
        base.save(tmp.path()).unwrap();

        for field in [PackageField::Checksums, PackageField::Attest] {
            orchestrate_with_prompt(
                tmp.path(),
                &FakePrompt {
                    toggle: RefCell::new(true),
                    ..package_prompt(field, vec![])
                },
            )
            .unwrap();
        }

        let cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert!(cfg.packages[0].checksums);
        assert!(cfg.packages[0].attest);

        // Both are `skip_serializing_if = "is_false"`, so only a `true` reaches release.toml —
        // absence is what "off" looks like on disk.
        let text = std::fs::read_to_string(ReleaseConfig::path(tmp.path())).unwrap();
        assert!(text.contains("checksums = true"), "{text}");
        assert!(text.contains("attest = true"), "{text}");
    }

    /// The ES-Runtime shape: a Cargo workspace at the root, JS packages scattered beneath it, and
    /// no root package.json anywhere. Enabling npm has to surface them.
    fn polyglot_repo(root: &std::path::Path) -> Vec<&'static str> {
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        let files = [
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
        ];
        for (dir, json) in files {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("package.json"), json).unwrap();
        }
        files.iter().map(|(d, _)| *d).collect()
    }

    fn ecosystems_prompt() -> FakePrompt {
        FakePrompt {
            actions: RefCell::new(vec![ConfigAction::Ecosystems, ConfigAction::Exit]),
            ..FakePrompt::default()
        }
    }

    #[test]
    fn enabling_npm_lists_and_records_the_repos_js_packages() {
        let tmp = tempfile::tempdir().unwrap();
        polyglot_repo(tmp.path());
        let mut base = config();
        base.adapters = vec![Ecosystem::Generic];
        base.save(tmp.path()).unwrap();

        orchestrate_with_prompt(tmp.path(), &ecosystems_prompt()).unwrap();

        let cfg = ReleaseConfig::load(tmp.path()).unwrap();
        // The publishable ones, recorded as directories. `website` is private, so it is offered
        // but starts unchecked — and the fake prompt takes the defaults.
        assert_eq!(
            cfg.discovery.npm,
            ["packages/postgres", "packages/redis", "types"]
        );

        let text = std::fs::read_to_string(ReleaseConfig::path(tmp.path())).unwrap();
        assert!(text.contains("[discovery]"), "{text}");
        for dir in ["packages/postgres", "packages/redis", "types"] {
            assert!(text.contains(&format!("\"{dir}\"")), "{text}");
        }
        assert!(!text.contains("website"), "{text}");
    }

    #[test]
    fn a_repo_that_declares_workspaces_itself_gets_no_discovery_table() {
        let tmp = tempfile::tempdir().unwrap();
        polyglot_repo(tmp.path());
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"name":"root","private":true,"workspaces":["packages/*"]}"#,
        )
        .unwrap();
        config().save(tmp.path()).unwrap();

        orchestrate_with_prompt(tmp.path(), &ecosystems_prompt()).unwrap();

        // npm's own declaration stays the single source of truth; duplicating it here would be one
        // more place to drift.
        let cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert!(cfg.discovery.npm.is_empty());
        let text = std::fs::read_to_string(ReleaseConfig::path(tmp.path())).unwrap();
        assert!(!text.contains("[discovery]"), "{text}");
    }

    #[test]
    fn disabling_npm_drops_the_declaration() {
        let tmp = tempfile::tempdir().unwrap();
        polyglot_repo(tmp.path());
        let mut base = config();
        base.discovery.npm = vec!["types".to_string()];
        base.save(tmp.path()).unwrap();

        // The fake prompt returns npm + generic, so keep npm out by asking for generic only.
        struct GenericOnly;
        impl ConfigPrompt for GenericOnly {
            fn ecosystems(&self, _: &[Ecosystem]) -> Result<Vec<Ecosystem>> {
                Ok(vec![Ecosystem::Generic])
            }
            fn action(&self) -> Result<ConfigAction> {
                Ok(ConfigAction::Exit)
            }
            fn npm_packages(&self, _: &[GenericCandidate], _: &[usize]) -> Result<Vec<usize>> {
                panic!("npm is disabled — nothing to ask about");
            }
            fn hook_stage(&self) -> Result<HookStage> {
                unreachable!()
            }
            fn package<'a>(&self, _: &'a [PackageEntry]) -> Result<Option<&'a str>> {
                unreachable!()
            }
            fn package_field(&self, _: &PackageEntry) -> Result<PackageField> {
                unreachable!()
            }
            fn mode(&self, _: Mode) -> Result<Mode> {
                unreachable!()
            }
            fn global_field(&self) -> Result<GlobalField> {
                unreachable!()
            }
            fn changelog_scope(&self, _: &ChangelogScope) -> Result<ChangelogScope> {
                unreachable!()
            }
            fn changelog_strategy(&self, _: &ChangelogStrategy) -> Result<ChangelogStrategy> {
                unreachable!()
            }
            fn github_release_notes(&self, _: &GithubReleaseNotes) -> Result<GithubReleaseNotes> {
                unreachable!()
            }
            fn tag_format(&self, _: &str) -> Result<String> {
                unreachable!()
            }
            fn targets(&self, _: &[Target]) -> Result<Vec<Target>> {
                unreachable!()
            }
            fn toggle(&self, _: &str, _: &str, _: bool) -> Result<bool> {
                unreachable!()
            }
            fn text(&self, _: &str, _: &str) -> Result<String> {
                unreachable!()
            }
        }

        let mut config = ReleaseConfig::load(tmp.path()).unwrap();
        config.adapters = vec![Ecosystem::Generic];
        edit_npm_discovery(tmp.path(), &GenericOnly, &mut config).unwrap();
        assert!(config.discovery.npm.is_empty());
    }

    fn config() -> ReleaseConfig {
        ReleaseConfig {
            otf_release_version: None,
            adapters: vec![Ecosystem::Npm],
            provider: "github".to_string(),
            snapshot_tag: Some("snapshot".to_string()),
            publish: crate::config::PublishConfig {
                ignore_paths: [("pkg".to_string(), Vec::new())].into_iter().collect(),
            },
            packages: vec![PackageEntry {
                name: "pkg".to_string(),
                adapter: Ecosystem::Generic,
                mode: Mode::BuildOnly,
                matrix: false,
                targets: vec![],
                command: "old build".to_string(),
                artifacts: "old/*".to_string(),
                bin_name: None,
                compress: None,
                manifest: Some("deno.json".to_string()),
                version_field: Some("version".to_string()),
                publish: None,
                archive: None,
                checksums: false,
                attest: false,
                executable: None,
                include: Vec::new(),
                tag_format: None,
                changelog: None,
            }],
            ..Default::default()
        }
    }

    fn package_prompt(field: PackageField, text: Vec<&str>) -> FakePrompt {
        FakePrompt {
            actions: RefCell::new(vec![ConfigAction::Packages, ConfigAction::Exit]),
            package: RefCell::new(Some("pkg".to_string())),
            package_field: RefCell::new(field),
            mode: RefCell::new(Mode::Publish),
            text: RefCell::new(text.into_iter().map(str::to_string).collect()),
            ..FakePrompt::default()
        }
    }

    fn global_prompt(field: GlobalField, text: Vec<&str>) -> FakePrompt {
        FakePrompt {
            actions: RefCell::new(vec![ConfigAction::GlobalSettings, ConfigAction::Exit]),
            package: RefCell::new(Some("pkg".to_string())),
            global_field: RefCell::new(field),
            scope: RefCell::new(ChangelogScope::Root),
            strategy: RefCell::new(ChangelogStrategy::Generated),
            github_release_notes: RefCell::new(GithubReleaseNotes::CuratedChangelog),
            text: RefCell::new(text.into_iter().map(str::to_string).collect()),
            ..FakePrompt::default()
        }
    }

    #[test]
    fn edits_package_fields() {
        let tmp = tempfile::tempdir().unwrap();
        config().save(tmp.path()).unwrap();

        orchestrate_with_prompt(tmp.path(), &package_prompt(PackageField::Mode, vec![])).unwrap();
        let mut cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.packages[0].mode, Mode::Publish);

        orchestrate_with_prompt(
            tmp.path(),
            &package_prompt(PackageField::Command, vec!["new build"]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.packages[0].command, "new build");

        orchestrate_with_prompt(
            tmp.path(),
            &package_prompt(PackageField::Artifacts, vec!["dist/**"]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.packages[0].artifacts, "dist/**");

        // The picker returns resolved targets, so the saved rows carry their triple and runner —
        // the config can't end up with a half-populated target.
        let picked = vec![
            Target::resolved("linux", "x86_64"),
            Target::resolved("linux-musl", "x86_64"),
        ];
        orchestrate_with_prompt(
            tmp.path(),
            &FakePrompt {
                targets: RefCell::new(picked),
                ..package_prompt(PackageField::Targets, vec![])
            },
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert!(cfg.packages[0].matrix);
        assert_eq!(cfg.packages[0].targets.len(), 2);
        assert_eq!(
            cfg.packages[0].targets[1].triple(),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(cfg.packages[0].targets[1].runner(), "ubuntu-latest");

        orchestrate_with_prompt(
            tmp.path(),
            &package_prompt(PackageField::GenericManifest, vec!["jsr.json"]),
        )
        .unwrap();
        orchestrate_with_prompt(
            tmp.path(),
            &package_prompt(PackageField::GenericVersionField, vec!["pkg.version"]),
        )
        .unwrap();
        orchestrate_with_prompt(
            tmp.path(),
            &package_prompt(PackageField::GenericPublishCommand, vec!["npx jsr publish"]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.packages[0].manifest.as_deref(), Some("jsr.json"));
        assert_eq!(
            cfg.packages[0].version_field.as_deref(),
            Some("pkg.version")
        );
        assert_eq!(cfg.packages[0].publish.as_deref(), Some("npx jsr publish"));
    }

    #[test]
    fn scopes_and_clears_a_packages_tag_format_and_changelog() {
        let tmp = tempfile::tempdir().unwrap();
        config().save(tmp.path()).unwrap();

        orchestrate_with_prompt(
            tmp.path(),
            &package_prompt(PackageField::TagFormat, vec!["{name}@{version}"]),
        )
        .unwrap();
        orchestrate_with_prompt(
            tmp.path(),
            &package_prompt(PackageField::Changelog, vec!["crates/dev-cli/CHANGELOG.md"]),
        )
        .unwrap();

        let cfg = ReleaseConfig::load(tmp.path()).unwrap();
        let pkg = cfg.package("pkg").unwrap();
        assert_eq!(pkg.tag_format.as_deref(), Some("{name}@{version}"));
        assert_eq!(
            pkg.changelog.as_deref(),
            Some("crates/dev-cli/CHANGELOG.md")
        );
        // The resolvers, not just the file, reflect it.
        assert_eq!(
            cfg.tag_formats().tag_for("pkg", "0.24.0").unwrap(),
            "pkg@0.24.0"
        );
        assert_eq!(
            cfg.changelog_layout().path_for(Path::new("/repo"), "pkg"),
            Some(Path::new("/repo/crates/dev-cli/CHANGELOG.md").to_path_buf())
        );

        // Blank clears the field back to the repo-wide setting.
        orchestrate_with_prompt(
            tmp.path(),
            &package_prompt(PackageField::TagFormat, vec![""]),
        )
        .unwrap();
        orchestrate_with_prompt(
            tmp.path(),
            &package_prompt(PackageField::Changelog, vec!["  "]),
        )
        .unwrap();

        let cfg = ReleaseConfig::load(tmp.path()).unwrap();
        let pkg = cfg.package("pkg").unwrap();
        assert!(pkg.tag_format.is_none());
        assert!(pkg.changelog.is_none());
    }

    #[test]
    fn rejects_a_package_tag_format_that_cannot_produce_a_tag() {
        let tmp = tempfile::tempdir().unwrap();
        config().save(tmp.path()).unwrap();

        let err = orchestrate_with_prompt(
            tmp.path(),
            &package_prompt(PackageField::TagFormat, vec!["latest"]),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("tag_format"), "{err:#}");

        // Nothing was written.
        assert!(ReleaseConfig::load(tmp.path())
            .unwrap()
            .package("pkg")
            .unwrap()
            .tag_format
            .is_none());
    }

    #[test]
    fn rejects_a_package_changelog_outside_the_repo() {
        let tmp = tempfile::tempdir().unwrap();
        config().save(tmp.path()).unwrap();

        let err = orchestrate_with_prompt(
            tmp.path(),
            &package_prompt(PackageField::Changelog, vec!["../elsewhere/CHANGELOG.md"]),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("inside the repo"), "{err:#}");
    }

    #[test]
    fn edits_global_settings_and_ecosystems() {
        let tmp = tempfile::tempdir().unwrap();
        config().save(tmp.path()).unwrap();

        let ecosystem_prompt = FakePrompt {
            actions: RefCell::new(vec![ConfigAction::Ecosystems, ConfigAction::Exit]),
            ..FakePrompt::default()
        };

        orchestrate_with_prompt(tmp.path(), &ecosystem_prompt).unwrap();
        let mut cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.adapters, vec![Ecosystem::Npm, Ecosystem::Generic]);

        orchestrate_with_prompt(
            tmp.path(),
            &global_prompt(GlobalField::Provider, vec!["github-enterprise"]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.provider, "github-enterprise");

        orchestrate_with_prompt(
            tmp.path(),
            &global_prompt(GlobalField::SnapshotTag, vec!["canary"]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.snapshot_tag.as_deref(), Some("canary"));

        orchestrate_with_prompt(
            tmp.path(),
            &global_prompt(GlobalField::SkipPublish, vec!["@scope/old, pkg-internal"]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.skip_publish, vec!["@scope/old", "pkg-internal"]);

        orchestrate_with_prompt(
            tmp.path(),
            &global_prompt(
                GlobalField::PublishIgnorePaths,
                vec!["docs/**, **/*.test.ts"],
            ),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(
            cfg.publish.ignore_paths.get("pkg").unwrap(),
            &vec!["docs/**".to_string(), "**/*.test.ts".to_string()]
        );

        orchestrate_with_prompt(
            tmp.path(),
            &global_prompt(GlobalField::TagFormat, vec!["{name}@{version}"]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.tag_format, "{name}@{version}");

        orchestrate_with_prompt(
            tmp.path(),
            &global_prompt(GlobalField::LegacyTagFormats, vec!["{name}@{version}"]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.legacy_tag_formats, vec!["{name}@{version}"]);

        orchestrate_with_prompt(
            tmp.path(),
            &global_prompt(GlobalField::ChangelogScope, vec![]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.changelog_scope, ChangelogScope::Root);

        orchestrate_with_prompt(
            tmp.path(),
            &global_prompt(GlobalField::ChangelogStrategy, vec![]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.changelog_strategy, ChangelogStrategy::Generated);

        orchestrate_with_prompt(
            tmp.path(),
            &global_prompt(GlobalField::GithubReleaseNotes, vec![]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(
            cfg.github_release_notes,
            GithubReleaseNotes::CuratedChangelog
        );
    }
}
