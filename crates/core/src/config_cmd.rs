use std::path::Path;

use anyhow::Result;
use inquire::{MultiSelect, Select, Text};

use crate::config::{
    format_tag, ChangelogStrategy, Ecosystem, GithubReleaseNotes, Mode, PackageEntry,
    ReleaseConfig, Target, DEFAULT_VERSION_FIELD,
};

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
    GenericManifest,
    GenericVersionField,
    GenericPublishCommand,
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalField {
    Provider,
    SnapshotTag,
    TagFormat,
    ChangelogStrategy,
    GithubReleaseNotes,
    Back,
}

pub trait ConfigPrompt {
    fn action(&self) -> Result<ConfigAction>;
    fn hook_stage(&self) -> Result<HookStage>;
    fn ecosystems(&self, current: &[Ecosystem]) -> Result<Vec<Ecosystem>>;
    fn package<'a>(&self, packages: &'a [PackageEntry]) -> Result<Option<&'a str>>;
    fn package_field(&self, package: &PackageEntry) -> Result<PackageField>;
    fn mode(&self, current: Mode) -> Result<Mode>;
    fn global_field(&self) -> Result<GlobalField>;
    fn changelog_strategy(&self, current: &ChangelogStrategy) -> Result<ChangelogStrategy>;
    fn github_release_notes(&self, current: &GithubReleaseNotes) -> Result<GithubReleaseNotes>;
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
            "Tag format",
            "Changelog strategy",
            "GitHub Release notes",
            "Back",
        ];
        Ok(
            match Select::new("Which global setting?", choices).prompt()? {
                "Provider" => GlobalField::Provider,
                "Snapshot tag" => GlobalField::SnapshotTag,
                "Tag format" => GlobalField::TagFormat,
                "Changelog strategy" => GlobalField::ChangelogStrategy,
                "GitHub Release notes" => GlobalField::GithubReleaseNotes,
                _ => GlobalField::Back,
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
                save(root, &config)?;
            }
            ConfigAction::Packages => edit_package(root, prompt, &mut config)?,
            ConfigAction::GlobalSettings => edit_global(root, prompt, &mut config)?,
            ConfigAction::Exit => break,
        }
    }

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

    let package = &mut config.packages[idx];
    match field {
        PackageField::Mode => package.mode = prompt.mode(package.mode)?,
        PackageField::Command => {
            package.command = prompt.text("Build command:", &package.command)?;
        }
        PackageField::Artifacts => {
            package.artifacts = prompt.text("Artifacts glob:", &package.artifacts)?;
        }
        PackageField::Targets => {
            let current = targets_to_text(&package.targets);
            let edited = prompt.text("Targets (e.g. linux-x86_64, macos-aarch64):", &current)?;
            package.targets = parse_targets(&edited);
            package.matrix = !package.targets.is_empty();
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
        GlobalField::TagFormat => {
            let tag_format = prompt.text("Tag format:", &config.tag_format)?;
            format_tag(&tag_format, "package", "1.2.3")?;
            config.tag_format = tag_format;
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

fn optional_text(text: String) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn targets_to_text(targets: &[Target]) -> String {
    targets
        .iter()
        .map(|t| format!("{}-{}", t.name, t.arch))
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_targets(text: &str) -> Vec<Target> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| {
            let (name, arch) = s.split_once('-')?;
            Some(Target {
                name: name.to_string(),
                arch: arch.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakePrompt {
        actions: RefCell<Vec<ConfigAction>>,
        package: RefCell<Option<String>>,
        package_field: RefCell<PackageField>,
        mode: RefCell<Mode>,
        global_field: RefCell<GlobalField>,
        strategy: RefCell<ChangelogStrategy>,
        github_release_notes: RefCell<GithubReleaseNotes>,
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
                strategy: RefCell::new(ChangelogStrategy::Curated),
                github_release_notes: RefCell::new(GithubReleaseNotes::AutoGenerate),
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

        fn changelog_strategy(&self, _current: &ChangelogStrategy) -> Result<ChangelogStrategy> {
            Ok(self.strategy.borrow().clone())
        }

        fn github_release_notes(
            &self,
            _current: &GithubReleaseNotes,
        ) -> Result<GithubReleaseNotes> {
            Ok(self.github_release_notes.borrow().clone())
        }

        fn text(&self, _prompt: &str, _current: &str) -> Result<String> {
            Ok(self.text.borrow_mut().remove(0))
        }
    }

    fn config() -> ReleaseConfig {
        ReleaseConfig {
            adapters: vec![Ecosystem::Npm],
            provider: "github".to_string(),
            snapshot_tag: Some("snapshot".to_string()),
            packages: vec![PackageEntry {
                name: "pkg".to_string(),
                adapter: Ecosystem::Generic,
                mode: Mode::BuildOnly,
                matrix: false,
                targets: vec![],
                command: "old build".to_string(),
                artifacts: "old/*".to_string(),
                manifest: Some("deno.json".to_string()),
                version_field: Some("version".to_string()),
                publish: None,
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
            global_field: RefCell::new(field),
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

        orchestrate_with_prompt(
            tmp.path(),
            &package_prompt(PackageField::Targets, vec!["linux-x86_64, macos-aarch64"]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert!(cfg.packages[0].matrix);
        assert_eq!(cfg.packages[0].targets.len(), 2);

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
            &global_prompt(GlobalField::TagFormat, vec!["{name}@{version}"]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.tag_format, "{name}@{version}");

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
