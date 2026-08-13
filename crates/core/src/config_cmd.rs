use std::path::Path;

use anyhow::Result;
use inquire::error::{InquireError, InquireResult};
use inquire::{MultiSelect, Select, Text};

use crate::config::{
    format_tag, ChangelogScope, ChangelogStrategy, Ecosystem, GithubReleaseNotes, Mode,
    PackageEntry, ReleaseConfig, Target, COMMON_TAG_FORMATS, DEFAULT_VERSION_FIELD,
};
use crate::discover::{declares_npm_workspaces, scan_npm_candidates, GenericCandidate};
use crate::init::{
    adopt_package, sync_package_blocks, unconfigured_packages, AdapterFactory, UnconfiguredPackage,
};

/// How a package the repo has but `release.toml` does not is labelled in the picker.
const NEW_MARKER: &str = "[new]";

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

/// A package's answer to "which tag line do you own?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagFormatChoice {
    /// Use the repo-wide `tag_format`.
    Inherit,
    /// This package tags itself with its own format.
    Scoped(String),
}

/// What to do with a package the repo releases that `release.toml` does not mention yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewPackageAction {
    /// Write its `[[package]]` block, then edit it like any other.
    Add,
    /// Record it in `skip_publish` so it stops being offered.
    Skip,
    /// Decide later — nothing is written.
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

/// The menus, as a seam the tests drive without a terminal.
///
/// **Esc means back.** Every prompt that returns an `Option` reports Esc as `None` — the caller
/// abandons that edit and returns to the menu above, saving nothing. Prompts whose menu already
/// has a *Back* row report Esc as that row instead. Only [`ConfigPrompt::action`], the root menu,
/// can end the session, and it takes two presses.
pub trait ConfigPrompt {
    fn action(&self) -> Result<ConfigAction>;
    fn hook_stage(&self) -> Result<HookStage>;
    fn ecosystems(&self, current: &[Ecosystem]) -> Result<Option<Vec<Ecosystem>>>;
    /// Confirm which scanned npm packages this repo releases, returning indices into `found`.
    /// `defaults` are the ones to start checked.
    fn npm_packages(
        &self,
        found: &[GenericCandidate],
        defaults: &[usize],
    ) -> Result<Option<Vec<usize>>>;
    fn package<'a>(&self, packages: &'a [PackageEntry]) -> Result<Option<&'a str>>;
    /// Pick a package to edit. `configured` have blocks; `new` are packages found in the repo with
    /// no block yet, and must be shown as such rather than silently mixed in.
    fn package_to_edit(
        &self,
        configured: &[PackageEntry],
        new: &[String],
    ) -> Result<Option<String>>;
    /// Decide what to do with a package picked from the `new` list.
    fn new_package(&self, name: &str) -> Result<NewPackageAction>;
    fn package_field(&self, package: &PackageEntry) -> Result<PackageField>;
    fn mode(&self, current: Mode) -> Result<Option<Mode>>;
    fn global_field(&self) -> Result<GlobalField>;
    fn changelog_scope(&self, current: &ChangelogScope) -> Result<Option<ChangelogScope>>;
    fn changelog_strategy(&self, current: &ChangelogStrategy) -> Result<Option<ChangelogStrategy>>;
    fn github_release_notes(
        &self,
        current: &GithubReleaseNotes,
    ) -> Result<Option<GithubReleaseNotes>>;
    fn tag_format(&self, current: &str) -> Result<Option<String>>;
    /// Pick a package's own tag format, or inherit the repo's. Offering the same list the
    /// repo-wide prompt does keeps a scoped format from being a typo away from a broken release.
    fn package_tag_format(
        &self,
        name: &str,
        repo: &str,
        current: Option<&str>,
    ) -> Result<Option<TagFormatChoice>>;
    /// Pick the git host. Only GitHub is wired up today, so this is a list, not a free-text field
    /// that happily accepts a typo or a provider with no implementation behind it.
    fn provider(&self, current: &str) -> Result<Option<String>>;
    /// Check off the packages this repo must never version or publish. `all` is every package name
    /// the repo knows about, `current` the ones already skipped.
    fn skip_publish(&self, all: &[String], current: &[String]) -> Result<Option<Vec<String>>>;
    /// Check off the older tag formats to keep reading as release history. `choices` are the common
    /// formats plus whatever is already configured; `current` starts checked.
    fn legacy_tag_formats(
        &self,
        choices: &[String],
        current: &[String],
    ) -> Result<Option<Vec<String>>>;
    /// Re-pick a package's build targets, with the configured ones pre-checked.
    fn targets(&self, current: &[Target]) -> Result<Option<Vec<Target>>>;
    /// Flip an on/off package flag, starting on its current value.
    fn toggle(&self, prompt: &str, help: &str, current: bool) -> Result<Option<bool>>;
    fn text(&self, prompt: &str, current: &str) -> Result<Option<String>>;
}

/// Esc is *back*, not *quit*: `inquire` reports it as `OperationCanceled`, which would otherwise
/// bubble out of the menu loop as an error and end the whole session — losing the developer's place
/// for what everywhere else is an undo. Ctrl-C (`OperationInterrupted`) still quits, which is the
/// convention users already have from every other TUI.
fn cancellable<T>(result: InquireResult<T>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(InquireError::OperationCanceled) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub struct StdinConfigPrompt;

impl ConfigPrompt for StdinConfigPrompt {
    /// The root menu, where there is nothing above to go back to. Esc here arms the exit and says
    /// so; a second Esc leaves. One stray press on the way out of a submenu therefore cannot end
    /// the session, and nobody has to hunt for the *Exit* row to leave.
    fn action(&self) -> Result<ConfigAction> {
        let choices = vec![
            "Lifecycle Hooks",
            "Ecosystems",
            "Packages",
            "Global Settings",
            "Exit",
        ];
        let mut armed = false;
        loop {
            let chosen = cancellable(
                Select::new("What would you like to configure?", choices.clone())
                    .with_help_message(if armed {
                        "Esc again to exit"
                    } else {
                        "Esc twice to exit; inside a menu, Esc goes back"
                    })
                    .prompt(),
            )?;
            let Some(chosen) = chosen else {
                if armed {
                    return Ok(ConfigAction::Exit);
                }
                armed = true;
                continue;
            };
            return Ok(match chosen {
                "Lifecycle Hooks" => ConfigAction::LifecycleHooks,
                "Ecosystems" => ConfigAction::Ecosystems,
                "Packages" => ConfigAction::Packages,
                "Global Settings" => ConfigAction::GlobalSettings,
                _ => ConfigAction::Exit,
            });
        }
    }

    fn npm_packages(
        &self,
        found: &[GenericCandidate],
        defaults: &[usize],
    ) -> Result<Option<Vec<usize>>> {
        let labels: Vec<String> = found.iter().map(GenericCandidate::label).collect();
        let chosen = cancellable(
            MultiSelect::new("Which of these does this repo release?", labels)
                .with_default(defaults)
                .with_help_message(
                    "saved as [discovery] npm in release.toml, so version/check/publish all read \
                     the same set — leave out fixtures, examples, and anything you never publish",
                )
                .raw_prompt(),
        )?;
        Ok(chosen.map(|chosen| chosen.iter().map(|o| o.index).collect()))
    }

    fn hook_stage(&self) -> Result<HookStage> {
        let choices = vec![
            "pre_version",
            "post_version",
            "pre_publish",
            "post_publish",
            "Back",
        ];
        let chosen = cancellable(Select::new("Which hook stage?", choices).prompt())?;
        Ok(match chosen.unwrap_or("Back") {
            "pre_version" => HookStage::PreVersion,
            "post_version" => HookStage::PostVersion,
            "pre_publish" => HookStage::PrePublish,
            "post_publish" => HookStage::PostPublish,
            _ => HookStage::Back,
        })
    }

    fn ecosystems(&self, current: &[Ecosystem]) -> Result<Option<Vec<Ecosystem>>> {
        let labels: Vec<&str> = Ecosystem::ALL.iter().map(|e| e.label()).collect();
        let defaults: Vec<usize> = current
            .iter()
            .filter_map(|a| Ecosystem::ALL.iter().position(|e| e == a))
            .collect();
        let chosen = cancellable(
            MultiSelect::new("Enabled Ecosystems:", labels)
                .with_default(&defaults)
                .prompt(),
        )?;
        Ok(chosen.map(|chosen| {
            Ecosystem::ALL
                .iter()
                .copied()
                .filter(|eco| chosen.contains(&eco.label()))
                .collect()
        }))
    }

    fn package<'a>(&self, packages: &'a [PackageEntry]) -> Result<Option<&'a str>> {
        if packages.is_empty() {
            println!("No configured packages in release.toml.");
            return Ok(None);
        }
        let mut names: Vec<String> = packages.iter().map(|p| p.name.clone()).collect();
        names.push("Back".to_string());
        let Some(chosen) = cancellable(Select::new("Which package?", names).prompt())? else {
            return Ok(None);
        };
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

    fn package_to_edit(
        &self,
        configured: &[PackageEntry],
        new: &[String],
    ) -> Result<Option<String>> {
        if configured.is_empty() && new.is_empty() {
            println!("No configured packages in release.toml, and none found in the repo.");
            return Ok(None);
        }
        let mut labels: Vec<String> = configured.iter().map(|p| p.name.clone()).collect();
        labels.extend(new.iter().map(|name| format!("{name} {NEW_MARKER}")));
        labels.push("Back".to_string());
        let chosen = cancellable(
            Select::new("Which package?", labels)
                .with_help_message(
                    "packages marked [new] are in this repo but not yet in release.toml — pick one \
                     to release it or skip it for good",
                )
                .prompt(),
        )?;
        let Some(chosen) = chosen else {
            return Ok(None);
        };
        if chosen == "Back" {
            return Ok(None);
        }
        Ok(Some(
            chosen
                .strip_suffix(&format!(" {NEW_MARKER}"))
                .unwrap_or(&chosen)
                .to_string(),
        ))
    }

    fn new_package(&self, name: &str) -> Result<NewPackageAction> {
        let choices = vec![
            "Release it — write its [[package]] block",
            "Skip it — never version or publish it",
            "Back",
        ];
        let chosen = cancellable(
            Select::new(&format!("{name} is not in release.toml yet:"), choices)
                .with_help_message(
                    "skipping records it in `skip_publish`, so it stops being offered here",
                )
                .raw_prompt(),
        )?;
        Ok(match chosen.map(|choice| choice.index) {
            Some(0) => NewPackageAction::Add,
            Some(1) => NewPackageAction::Skip,
            _ => NewPackageAction::Back,
        })
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
        let chosen = cancellable(Select::new("Which package field?", choices).prompt())?;
        Ok(match chosen.unwrap_or("Back") {
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
        })
    }

    fn mode(&self, current: Mode) -> Result<Option<Mode>> {
        let choices = vec!["publish", "build-only"];
        let default = match current {
            Mode::Publish => 0,
            Mode::BuildOnly => 1,
        };
        let chosen = cancellable(
            Select::new("Package mode:", choices)
                .with_starting_cursor(default)
                .prompt(),
        )?;
        Ok(chosen.map(|chosen| match chosen {
            "publish" => Mode::Publish,
            _ => Mode::BuildOnly,
        }))
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
        let chosen = cancellable(Select::new("Which global setting?", choices).prompt())?;
        Ok(match chosen.unwrap_or("Back") {
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
        })
    }

    fn changelog_scope(&self, current: &ChangelogScope) -> Result<Option<ChangelogScope>> {
        let choices = vec!["root", "package"];
        let default = match current {
            ChangelogScope::Root => 0,
            ChangelogScope::Package => 1,
        };
        let chosen = cancellable(
            Select::new("Changelog scope:", choices)
                .with_starting_cursor(default)
                .prompt(),
        )?;
        Ok(chosen.map(|chosen| match chosen {
            "root" => ChangelogScope::Root,
            _ => ChangelogScope::Package,
        }))
    }

    fn changelog_strategy(&self, current: &ChangelogStrategy) -> Result<Option<ChangelogStrategy>> {
        let choices = vec!["curated", "generated"];
        let default = match current {
            ChangelogStrategy::Curated => 0,
            ChangelogStrategy::Generated => 1,
        };
        let chosen = cancellable(
            Select::new("Changelog strategy:", choices)
                .with_starting_cursor(default)
                .prompt(),
        )?;
        Ok(chosen.map(|chosen| match chosen {
            "generated" => ChangelogStrategy::Generated,
            _ => ChangelogStrategy::Curated,
        }))
    }

    fn github_release_notes(
        &self,
        current: &GithubReleaseNotes,
    ) -> Result<Option<GithubReleaseNotes>> {
        let choices = vec!["auto-generate", "curated-changelog", "semantic-commits"];
        let default = match current {
            GithubReleaseNotes::AutoGenerate => 0,
            GithubReleaseNotes::CuratedChangelog => 1,
            GithubReleaseNotes::SemanticCommits => 2,
        };
        let chosen = cancellable(
            Select::new("GitHub Release notes:", choices)
                .with_starting_cursor(default)
                .prompt(),
        )?;
        Ok(chosen.map(|chosen| match chosen {
            "curated-changelog" => GithubReleaseNotes::CuratedChangelog,
            "semantic-commits" => GithubReleaseNotes::SemanticCommits,
            _ => GithubReleaseNotes::AutoGenerate,
        }))
    }

    fn tag_format(&self, current: &str) -> Result<Option<String>> {
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
        let selected = cancellable(
            Select::new("Tag format:", choices)
                .with_starting_cursor(default)
                .prompt(),
        )?;
        let Some(selected) = selected else {
            return Ok(None);
        };
        if selected == "Custom" {
            // Esc out of the custom field goes back to the menu, not out of the edit entirely.
            cancellable(
                Text::new("Custom tag format:")
                    .with_default(current)
                    .prompt(),
            )
        } else {
            Ok(Some(
                selected
                    .strip_suffix(" (current)")
                    .unwrap_or(&selected)
                    .to_string(),
            ))
        }
    }

    fn package_tag_format(
        &self,
        name: &str,
        repo: &str,
        current: Option<&str>,
    ) -> Result<Option<TagFormatChoice>> {
        let inherit = format!("Inherit the repo's `{repo}`");
        let mut choices = vec![inherit.clone()];
        choices.extend(
            COMMON_TAG_FORMATS
                .iter()
                .filter(|format| **format != repo)
                .map(|format| (*format).to_string()),
        );
        // A format already scoped by hand stays on the list, so re-opening the prompt cannot lose
        // it just because it is not one of the built-ins.
        if let Some(current) = current.filter(|c| !choices.iter().any(|row| row == c)) {
            choices.push(current.to_string());
        }
        choices.push("Custom".to_string());

        let cursor = current
            .and_then(|current| choices.iter().position(|row| row == current))
            .unwrap_or(0);
        let chosen = cancellable(
            Select::new(&format!("Tag format for {name}:"), choices)
                .with_starting_cursor(cursor)
                .with_help_message(
                    "a package that must not share the repo's tag line needs `{name}` in its \
                     format, or two packages collide on one tag",
                )
                .prompt(),
        )?;
        let Some(chosen) = chosen else {
            return Ok(None);
        };
        if chosen == inherit {
            return Ok(Some(TagFormatChoice::Inherit));
        }
        if chosen == "Custom" {
            let typed = cancellable(
                Text::new("Custom tag format:")
                    .with_default(current.unwrap_or(repo))
                    .prompt(),
            )?;
            return Ok(typed.map(TagFormatChoice::Scoped));
        }
        Ok(Some(TagFormatChoice::Scoped(chosen)))
    }

    fn provider(&self, current: &str) -> Result<Option<String>> {
        // The same list `init` offers, so the two commands cannot disagree about what is supported.
        let choices = vec![
            "GitHub",
            "GitLab (Coming Soon)",
            "Bitbucket (Coming Soon)",
            "Gitea (Coming Soon)",
            "Codeberg (Coming Soon)",
        ];
        loop {
            let chosen = cancellable(
                Select::new("Which Git hosting provider do you use?", choices.clone())
                    .with_starting_cursor(usize::from(current != "github"))
                    .with_help_message("only GitHub is fully supported today")
                    .prompt(),
            )?;
            let Some(chosen) = chosen else {
                return Ok(None);
            };
            if chosen == "GitHub" {
                return Ok(Some("github".to_string()));
            }
            println!("Only GitHub is fully supported at this moment. Please select GitHub.");
        }
    }

    fn skip_publish(&self, all: &[String], current: &[String]) -> Result<Option<Vec<String>>> {
        if all.is_empty() {
            println!("No packages found to skip.");
            return Ok(None);
        }
        let checked: Vec<usize> = all
            .iter()
            .enumerate()
            .filter(|(_, name)| current.contains(name))
            .map(|(i, _)| i)
            .collect();
        let chosen = cancellable(
            MultiSelect::new("Packages this repo must not publish:", all.to_vec())
                .with_default(&checked)
                .with_help_message(
                    "checked packages are never versioned or published, and get no [[package]] \
                     block — for internal crates, fixtures, and anything released elsewhere",
                )
                .raw_prompt(),
        )?;
        Ok(chosen.map(|chosen| chosen.iter().map(|o| o.value.clone()).collect()))
    }

    fn legacy_tag_formats(
        &self,
        choices: &[String],
        current: &[String],
    ) -> Result<Option<Vec<String>>> {
        let checked: Vec<usize> = choices
            .iter()
            .enumerate()
            .filter(|(_, format)| current.contains(format))
            .map(|(i, _)| i)
            .collect();
        let chosen = cancellable(
            MultiSelect::new("Tag formats to read as release history:", choices.to_vec())
                .with_default(&checked)
                .with_help_message(
                    "new tags still use `tag_format`; these are only read, so a repo that renamed \
                     its tags keeps finding its older releases",
                )
                .raw_prompt(),
        )?;
        Ok(chosen.map(|chosen| chosen.iter().map(|o| o.value.clone()).collect()))
    }

    fn targets(&self, current: &[Target]) -> Result<Option<Vec<Target>>> {
        crate::init::pick_targets("Build targets:", current, crate::init::EDIT_TARGETS_HELP)
    }

    fn toggle(&self, prompt: &str, help: &str, current: bool) -> Result<Option<bool>> {
        let chosen = cancellable(
            Select::new(prompt, vec!["Yes", "No"])
                .with_starting_cursor(usize::from(!current))
                .with_help_message(help)
                .raw_prompt(),
        )?;
        Ok(chosen.map(|chosen| chosen.index == 0))
    }

    fn text(&self, prompt: &str, current: &str) -> Result<Option<String>> {
        cancellable(Text::new(prompt).with_initial_value(current).prompt())
    }
}

pub fn orchestrate(root: &Path, factory: &dyn AdapterFactory) -> Result<()> {
    orchestrate_with_prompt(root, factory, &StdinConfigPrompt)
}

pub fn orchestrate_with_prompt(
    root: &Path,
    factory: &dyn AdapterFactory,
    prompt: &dyn ConfigPrompt,
) -> Result<()> {
    let mut config = ReleaseConfig::load(root)?;

    loop {
        match prompt.action()? {
            ConfigAction::LifecycleHooks => edit_hooks(root, prompt, &mut config)?,
            ConfigAction::Ecosystems => {
                let Some(adapters) = prompt.ecosystems(&config.adapters)? else {
                    continue;
                };
                config.adapters = adapters;
                edit_npm_discovery(root, prompt, &mut config)?;
                // Enabling an ecosystem is only half an answer: without a block per package there
                // is no build step, no per-package setting to scope, and — for a package whose
                // publish needs a build — a broken release. Finish the job here rather than
                // leaving the repo in a state only `init` could complete.
                report_sync(sync_package_blocks(&mut config, factory, root)?);
                save(root, &config)?;
            }
            ConfigAction::Packages => edit_package(root, factory, prompt, &mut config)?,
            ConfigAction::GlobalSettings => edit_global(root, factory, prompt, &mut config)?,
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

    let Some(chosen) = prompt.npm_packages(&found, &defaults)? else {
        return Ok(());
    };
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
    let Some(edited) = edited else {
        return Ok(());
    };
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

/// Edit one package's settings, listing what the repo contains alongside what `release.toml`
/// already configures.
///
/// "I added a package" sends a developer straight here, so this menu re-reads the repo: a package
/// with no block is offered as `[new]`. Discovery is read-only, though — the file changes only
/// once the pick has been answered with *release it* or *skip it*. Merely looking at the menu, or
/// backing out of it, writes nothing, so a package the repo is not ready to release is not adopted
/// by accident.
fn edit_package(
    root: &Path,
    factory: &dyn AdapterFactory,
    prompt: &dyn ConfigPrompt,
    config: &mut ReleaseConfig,
) -> Result<()> {
    let discovered = unconfigured_packages(config, factory)?;
    let new_names: Vec<String> = discovered.iter().map(|d| d.pkg.name.clone()).collect();

    let Some(name) = prompt.package_to_edit(&config.packages, &new_names)? else {
        return Ok(());
    };

    if let Some(new) = discovered.iter().find(|d| d.pkg.name == name) {
        if !answer_new_package(root, factory, prompt, config, new)? {
            return Ok(());
        }
    }

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
        PackageField::Mode => match prompt.mode(package.mode)? {
            Some(mode) => package.mode = mode,
            None => return Ok(()),
        },
        PackageField::Command => match prompt.text("Build command:", &package.command)? {
            Some(command) => package.command = command,
            None => return Ok(()),
        },
        PackageField::Artifacts => match prompt.text("Artifacts glob:", &package.artifacts)? {
            Some(artifacts) => package.artifacts = artifacts,
            None => return Ok(()),
        },
        PackageField::Targets => match prompt.targets(&package.targets)? {
            Some(targets) => {
                package.targets = targets;
                package.matrix = !package.targets.is_empty();
            }
            None => return Ok(()),
        },
        PackageField::Checksums => {
            // Read by `github-release` at release time, so the workflow YAML is unaffected.
            let checksums = prompt.toggle(
                "Attach a checksums.txt (SHA-256)?",
                "one combined checksums.txt covering every asset on the release",
                package.checksums,
            )?;
            match checksums {
                Some(checksums) => package.checksums = checksums,
                None => return Ok(()),
            }
        }
        PackageField::Attest => {
            let attest = prompt.toggle(
                "Generate signed build provenance?",
                "proves each asset was built by this repo's workflow from this commit; verified \
                 with `gh attestation verify <file> --repo <owner/repo>`",
                package.attest,
            )?;
            match attest {
                Some(attest) => package.attest = attest,
                None => return Ok(()),
            }
            if package.attest {
                // Unlike checksums, this adds an `attestations: write` permission and a signing
                // step to the workflow, which only `upgrade` can write.
                println!("Run `otf-release upgrade` to regenerate the workflow with attestation.");
            }
        }
        PackageField::TagFormat => {
            let chosen =
                prompt.package_tag_format(name, &tag_format, package.tag_format.as_deref())?;
            let Some(chosen) = chosen else {
                return Ok(());
            };
            package.tag_format = match chosen {
                TagFormatChoice::Inherit => None,
                TagFormatChoice::Scoped(format) => {
                    format_tag(&format, name, "1.2.3")?;
                    Some(format)
                }
            };
        }
        PackageField::Changelog => {
            let current = package.changelog.as_deref().unwrap_or("");
            let Some(edited) = prompt.text(
                &format!(
                    "Changelog for {name}, relative to the repo root (blank = {scope} scope):"
                ),
                current,
            )?
            else {
                return Ok(());
            };
            package.changelog = optional_text(edited);
            package.validate_release_identity()?;
        }
        PackageField::GenericManifest => {
            let current = package.manifest.as_deref().unwrap_or("");
            let Some(edited) = prompt.text("Generic manifest:", current)? else {
                return Ok(());
            };
            package.manifest = optional_text(edited);
        }
        PackageField::GenericVersionField => {
            let current = package
                .version_field
                .as_deref()
                .unwrap_or(DEFAULT_VERSION_FIELD);
            let Some(edited) = prompt.text("Generic version field:", current)? else {
                return Ok(());
            };
            package.version_field = optional_text(edited);
        }
        PackageField::GenericPublishCommand => {
            let current = package.publish.as_deref().unwrap_or("");
            let Some(edited) = prompt.text("Generic publish command:", current)? else {
                return Ok(());
            };
            package.publish = optional_text(edited);
        }
        PackageField::Back => unreachable!(),
    }

    save(root, config)
}

/// Resolve a `[new]` pick. Returns whether the package now has a block and editing should carry on
/// into its fields; every outcome that writes something saves before returning, so the answer
/// survives whatever the caller does next.
fn answer_new_package(
    root: &Path,
    factory: &dyn AdapterFactory,
    prompt: &dyn ConfigPrompt,
    config: &mut ReleaseConfig,
    new: &UnconfiguredPackage,
) -> Result<bool> {
    let name = new.pkg.name.as_str();
    match prompt.new_package(name)? {
        NewPackageAction::Add => {
            for hook in adopt_package(config, factory, root, new)? {
                report_stripped_hook(name, &hook);
            }
            println!("Added a [[package]] block for {name}.");
            save(root, config)?;
            Ok(true)
        }
        NewPackageAction::Skip => {
            config.skip_publish.push(name.to_string());
            config.skip_publish.sort();
            config.skip_publish.dedup();
            println!(
                "{name} recorded in skip_publish — this repo will not version or publish it, and \
                 it will stop being offered here."
            );
            save(root, config)?;
            Ok(false)
        }
        NewPackageAction::Back => Ok(false),
    }
}

fn edit_global(
    root: &Path,
    factory: &dyn AdapterFactory,
    prompt: &dyn ConfigPrompt,
    config: &mut ReleaseConfig,
) -> Result<()> {
    match prompt.global_field()? {
        GlobalField::Provider => {
            let Some(provider) = prompt.provider(&config.provider)? else {
                return Ok(());
            };
            config.provider = provider;
            save(root, config)
        }
        GlobalField::SnapshotTag => {
            let current = config.snapshot_tag.as_deref().unwrap_or("");
            let Some(edited) = prompt.text("Snapshot tag:", current)? else {
                return Ok(());
            };
            config.snapshot_tag = optional_text(edited);
            save(root, config)
        }
        GlobalField::SkipPublish => {
            let all = known_package_names(config, factory)?;
            let Some(skipped) = prompt.skip_publish(&all, &config.skip_publish)? else {
                return Ok(());
            };
            config.skip_publish = skipped;
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
            let Some(edited) = prompt.text(
                &format!("Ignored publish paths for {name} (comma-separated globs):"),
                &current,
            )?
            else {
                return Ok(());
            };
            config
                .publish
                .ignore_paths
                .insert(name.to_string(), parse_csv(&edited));
            save(root, config)
        }
        GlobalField::TagFormat => {
            let Some(tag_format) = prompt.tag_format(&config.tag_format)? else {
                return Ok(());
            };
            format_tag(&tag_format, "package", "1.2.3")?;
            config.tag_format = tag_format;
            save(root, config)
        }
        GlobalField::LegacyTagFormats => {
            let choices = legacy_tag_format_choices(config);
            let Some(chosen) = prompt.legacy_tag_formats(&choices, &config.legacy_tag_formats)?
            else {
                return Ok(());
            };
            config.legacy_tag_formats = validated_tag_formats(chosen)?;
            save(root, config)
        }
        GlobalField::ChangelogScope => {
            let Some(scope) = prompt.changelog_scope(&config.changelog_scope)? else {
                return Ok(());
            };
            config.changelog_scope = scope;
            save(root, config)
        }
        GlobalField::ChangelogStrategy => {
            let Some(strategy) = prompt.changelog_strategy(&config.changelog_strategy)? else {
                return Ok(());
            };
            config.changelog_strategy = strategy;
            save(root, config)
        }
        GlobalField::GithubReleaseNotes => {
            let Some(notes) = prompt.github_release_notes(&config.github_release_notes)? else {
                return Ok(());
            };
            config.github_release_notes = notes;
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

/// Print what reconciling the package blocks changed, so an edit never silently rewrites the file.
/// Report a manifest edited to hand the build to the pipeline — a change on disk outside
/// `release.toml`, so it is never silent.
fn report_stripped_hook(package: &str, hook: &str) {
    println!(
        "Removed npm lifecycle hook `{hook}` from {package}. The release pipeline runs the \
         build itself — move any custom steps into a `build` script or [hooks] in release.toml."
    );
}

fn report_sync(sync: crate::init::PackageSync) {
    for (package, hook) in &sync.stripped_hooks {
        report_stripped_hook(package, hook);
    }
    if sync.is_empty() {
        return;
    }
    for name in &sync.added {
        println!("Added a [[package]] block for {name}.");
    }
    for name in &sync.removed {
        println!("Removed the [[package]] block for {name} — this repo no longer releases it.");
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

fn validated_tag_formats(formats: Vec<String>) -> Result<Vec<String>> {
    for format in &formats {
        format_tag(format, "package", "1.2.3")?;
    }
    Ok(formats)
}

/// The rows offered for `legacy_tag_formats`: the common patterns, plus anything already
/// configured (here or as the live `tag_format`) so a hand-written custom format is never dropped
/// just because it is not in the built-in list.
fn legacy_tag_format_choices(config: &ReleaseConfig) -> Vec<String> {
    let mut choices: Vec<String> = COMMON_TAG_FORMATS
        .iter()
        .map(|f| (*f).to_string())
        .collect();
    for format in config
        .legacy_tag_formats
        .iter()
        .chain(std::iter::once(&config.tag_format))
    {
        if !choices.contains(format) {
            choices.push(format.clone());
        }
    }
    // The format writing new tags is not history to read; it is already always consulted.
    choices.retain(|format| *format != config.tag_format);
    choices
}

/// Every package name this repo knows about: the blocks it configures, the packages its adapters
/// discover, and whatever is already skipped. The union matters — a name already in `skip_publish`
/// is invisible to discovery (that is the point of skipping it), so building the list from
/// discovery alone would silently drop every existing entry the moment the picker was confirmed.
fn known_package_names(
    config: &ReleaseConfig,
    factory: &dyn AdapterFactory,
) -> Result<Vec<String>> {
    let mut names: Vec<String> = config.skip_publish.clone();
    names.extend(config.packages.iter().map(|entry| entry.name.clone()));
    for eco in config
        .adapters
        .iter()
        .copied()
        .filter(|eco| *eco != Ecosystem::Generic)
    {
        let adapter = factory.make_with_discovery(eco, &config.discovery);
        names.extend(adapter.discover_packages()?.into_iter().map(|pkg| pkg.name));
    }
    names.sort();
    names.dedup();
    Ok(names)
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

    /// A factory whose adapters discover nothing: the package-block sync is exercised in its own
    /// tests, and these cover the editor's own behaviour.
    struct NoPackages;
    impl AdapterFactory for NoPackages {
        fn make(&self, _: Ecosystem) -> Box<dyn crate::adapter::Adapter> {
            Box::new(EmptyAdapter)
        }
    }

    /// A factory whose adapter finds one crate on disk — the "I just added a package" state that
    /// the Packages menu has to reconcile before it can offer anything to edit.
    struct DiscoversNewCrate;
    impl AdapterFactory for DiscoversNewCrate {
        fn make(&self, _: Ecosystem) -> Box<dyn crate::adapter::Adapter> {
            Box::new(OneCrateAdapter)
        }
    }

    struct OneCrateAdapter;
    impl crate::adapter::Adapter for OneCrateAdapter {
        fn discover_packages(&self) -> Result<Vec<crate::adapter::Pkg>> {
            Ok(vec![crate::adapter::Pkg {
                name: "new-crate".to_string(),
                version: "0.1.0".to_string(),
                manifest_path: Path::new("crates/new/Cargo.toml").to_path_buf(),
                changelog_path: Path::new("crates/new/CHANGELOG.md").to_path_buf(),
                publishable: true,
                internal_deps: Vec::new(),
            }])
        }
        fn write_version(&self, _: &crate::adapter::Pkg, _: &str) -> Result<()> {
            unreachable!()
        }
        fn update_dep_range(&self, _: &crate::adapter::Pkg, _: &str, _: &str) -> Result<()> {
            unreachable!()
        }
        fn format_range(&self, _: &str) -> String {
            unreachable!()
        }
        fn resolve_workspace_links(&self, _: &crate::adapter::Pkg) -> Result<()> {
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
        fn is_published(&self, _: &crate::adapter::Pkg, _: &str) -> Result<bool> {
            unreachable!()
        }
        fn publish(&self, _: &crate::adapter::Pkg, _: Option<&Path>) -> Result<()> {
            unreachable!()
        }
    }

    struct EmptyAdapter;
    impl crate::adapter::Adapter for EmptyAdapter {
        fn discover_packages(&self) -> Result<Vec<crate::adapter::Pkg>> {
            Ok(Vec::new())
        }
        fn write_version(&self, _: &crate::adapter::Pkg, _: &str) -> Result<()> {
            unreachable!()
        }
        fn update_dep_range(&self, _: &crate::adapter::Pkg, _: &str, _: &str) -> Result<()> {
            unreachable!()
        }
        fn format_range(&self, _: &str) -> String {
            unreachable!()
        }
        fn resolve_workspace_links(&self, _: &crate::adapter::Pkg) -> Result<()> {
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
        fn is_published(&self, _: &crate::adapter::Pkg, _: &str) -> Result<bool> {
            unreachable!()
        }
        fn publish(&self, _: &crate::adapter::Pkg, _: Option<&Path>) -> Result<()> {
            unreachable!()
        }
    }

    struct FakePrompt {
        actions: RefCell<Vec<ConfigAction>>,
        package: RefCell<Option<String>>,
        /// What to answer when the picked package is one the repo has and `release.toml` does not.
        new_package: RefCell<NewPackageAction>,
        /// Every `[new]` name the picker was offered, so a test can assert what was on screen.
        offered_new: RefCell<Vec<String>>,
        /// Simulate Esc at every value prompt.
        esc: RefCell<bool>,
        /// What the checklist prompts (skip-publish, legacy tag formats) come back with.
        checked: RefCell<Vec<String>>,
        /// Every row a checklist prompt was offered, so a test can assert what was on screen.
        offered_checks: RefCell<Vec<String>>,
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
                new_package: RefCell::new(NewPackageAction::Add),
                offered_new: RefCell::new(Vec::new()),
                esc: RefCell::new(false),
                checked: RefCell::new(Vec::new()),
                offered_checks: RefCell::new(Vec::new()),
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
        ) -> Result<Option<Vec<usize>>> {
            Ok(self.answer(defaults.to_vec()))
        }

        fn ecosystems(&self, _current: &[Ecosystem]) -> Result<Option<Vec<Ecosystem>>> {
            Ok(self.answer(vec![Ecosystem::Npm, Ecosystem::Generic]))
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

        fn package_to_edit(
            &self,
            configured: &[PackageEntry],
            new: &[String],
        ) -> Result<Option<String>> {
            self.offered_new.borrow_mut().extend_from_slice(new);
            let Some(name) = self.package.borrow().clone() else {
                return Ok(None);
            };
            let known = configured.iter().any(|p| p.name == name) || new.contains(&name);
            Ok(known.then_some(name))
        }

        fn new_package(&self, _name: &str) -> Result<NewPackageAction> {
            Ok(*self.new_package.borrow())
        }

        fn package_field(&self, _package: &PackageEntry) -> Result<PackageField> {
            Ok(*self.package_field.borrow())
        }

        fn mode(&self, _current: Mode) -> Result<Option<Mode>> {
            Ok(self.answer(*self.mode.borrow()))
        }

        fn global_field(&self) -> Result<GlobalField> {
            Ok(*self.global_field.borrow())
        }

        fn changelog_scope(&self, _current: &ChangelogScope) -> Result<Option<ChangelogScope>> {
            Ok(self.answer(self.scope.borrow().clone()))
        }

        fn changelog_strategy(
            &self,
            _current: &ChangelogStrategy,
        ) -> Result<Option<ChangelogStrategy>> {
            Ok(self.answer(self.strategy.borrow().clone()))
        }

        fn github_release_notes(
            &self,
            _current: &GithubReleaseNotes,
        ) -> Result<Option<GithubReleaseNotes>> {
            Ok(self.answer(self.github_release_notes.borrow().clone()))
        }

        fn tag_format(&self, _current: &str) -> Result<Option<String>> {
            self.typed()
        }

        fn provider(&self, _current: &str) -> Result<Option<String>> {
            self.typed()
        }

        fn package_tag_format(
            &self,
            _name: &str,
            _repo: &str,
            _current: Option<&str>,
        ) -> Result<Option<TagFormatChoice>> {
            // The queued reply, with a blank standing in for the "inherit" row.
            Ok(self.typed()?.map(|typed| match optional_text(typed) {
                Some(format) => TagFormatChoice::Scoped(format),
                None => TagFormatChoice::Inherit,
            }))
        }

        fn skip_publish(&self, all: &[String], _current: &[String]) -> Result<Option<Vec<String>>> {
            self.offered_checks.borrow_mut().extend_from_slice(all);
            Ok(self.answer(self.checked.borrow().clone()))
        }

        fn legacy_tag_formats(
            &self,
            choices: &[String],
            _current: &[String],
        ) -> Result<Option<Vec<String>>> {
            self.offered_checks.borrow_mut().extend_from_slice(choices);
            Ok(self.answer(self.checked.borrow().clone()))
        }

        fn targets(&self, _current: &[Target]) -> Result<Option<Vec<Target>>> {
            Ok(self.answer(self.targets.borrow().clone()))
        }

        fn toggle(&self, _prompt: &str, _help: &str, _current: bool) -> Result<Option<bool>> {
            Ok(self.answer(*self.toggle.borrow()))
        }

        fn text(&self, _prompt: &str, _current: &str) -> Result<Option<String>> {
            self.typed()
        }
    }

    impl FakePrompt {
        /// A value prompt's reply: `None` when the test is simulating Esc.
        fn answer<T>(&self, value: T) -> Option<T> {
            (!*self.esc.borrow()).then_some(value)
        }

        /// Same, for the prompts that consume the queued `text` replies — an Esc must not eat one,
        /// since the user never typed it.
        fn typed(&self) -> Result<Option<String>> {
            if *self.esc.borrow() {
                return Ok(None);
            }
            Ok(Some(self.text.borrow_mut().remove(0)))
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
                &NoPackages,
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

        orchestrate_with_prompt(tmp.path(), &NoPackages, &ecosystems_prompt()).unwrap();

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

        orchestrate_with_prompt(tmp.path(), &NoPackages, &ecosystems_prompt()).unwrap();

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
            fn ecosystems(&self, _: &[Ecosystem]) -> Result<Option<Vec<Ecosystem>>> {
                Ok(Some(vec![Ecosystem::Generic]))
            }
            fn action(&self) -> Result<ConfigAction> {
                Ok(ConfigAction::Exit)
            }
            fn npm_packages(
                &self,
                _: &[GenericCandidate],
                _: &[usize],
            ) -> Result<Option<Vec<usize>>> {
                panic!("npm is disabled — nothing to ask about");
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
            fn changelog_strategy(
                &self,
                _: &ChangelogStrategy,
            ) -> Result<Option<ChangelogStrategy>> {
                unreachable!()
            }
            fn github_release_notes(
                &self,
                _: &GithubReleaseNotes,
            ) -> Result<Option<GithubReleaseNotes>> {
                unreachable!()
            }
            fn tag_format(&self, _: &str) -> Result<Option<String>> {
                unreachable!()
            }
            fn provider(&self, _: &str) -> Result<Option<String>> {
                unreachable!()
            }
            fn package_tag_format(
                &self,
                _: &str,
                _: &str,
                _: Option<&str>,
            ) -> Result<Option<TagFormatChoice>> {
                unreachable!()
            }
            fn skip_publish(&self, _: &[String], _: &[String]) -> Result<Option<Vec<String>>> {
                unreachable!()
            }
            fn legacy_tag_formats(
                &self,
                _: &[String],
                _: &[String],
            ) -> Result<Option<Vec<String>>> {
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

    fn packages_menu(prompt: FakePrompt) -> FakePrompt {
        FakePrompt {
            actions: RefCell::new(vec![ConfigAction::Packages, ConfigAction::Exit]),
            ..prompt
        }
    }

    /// The skip-publish checklist has to offer packages that discovery cannot see: skipping one is
    /// exactly what hides it from the adapters. Building the rows from discovery alone would show
    /// the existing entries as unchecked-and-absent, and confirming the prompt would wipe them.
    #[test]
    fn skip_publish_offers_already_skipped_packages_alongside_discovered_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let mut saved = config();
        saved.adapters = vec![Ecosystem::Cargo];
        saved.skip_publish = vec!["retired-crate".to_string()];
        saved.save(tmp.path()).unwrap();

        let prompt = FakePrompt {
            actions: RefCell::new(vec![ConfigAction::GlobalSettings, ConfigAction::Exit]),
            global_field: RefCell::new(GlobalField::SkipPublish),
            checked: RefCell::new(vec!["retired-crate".to_string(), "new-crate".to_string()]),
            ..FakePrompt::default()
        };
        orchestrate_with_prompt(tmp.path(), &DiscoversNewCrate, &prompt).unwrap();

        // Rows: the skipped one (invisible to discovery), the configured block, the found crate.
        assert_eq!(
            prompt.offered_checks.borrow().as_slice(),
            ["new-crate", "pkg", "retired-crate"]
        );
        assert_eq!(
            ReleaseConfig::load(tmp.path()).unwrap().skip_publish,
            vec!["retired-crate".to_string(), "new-crate".to_string()]
        );
    }

    /// The legacy-format checklist is a closed list of *readable* history, so it must not offer the
    /// format writing new tags, and must not drop a custom one already configured by hand.
    #[test]
    fn legacy_tag_format_rows_keep_custom_entries_and_exclude_the_live_format() {
        let mut cfg = config();
        cfg.tag_format = "v{version}".to_string();
        cfg.legacy_tag_formats = vec!["release-{version}".to_string()];

        let choices = legacy_tag_format_choices(&cfg);

        assert!(choices.contains(&"release-{version}".to_string()));
        assert!(choices.contains(&"{name}@{version}".to_string()));
        assert!(
            !choices.contains(&"v{version}".to_string()),
            "the live tag_format is already read; offering it as history is noise: {choices:?}"
        );
    }

    /// A format that cannot produce a distinct tag is refused at the picker, not written and left
    /// for `version` to fail on later.
    #[test]
    fn legacy_tag_formats_are_validated_before_saving() {
        assert!(validated_tag_formats(vec!["{name}-latest".to_string()]).is_err());
        assert!(validated_tag_formats(vec!["v{version}".to_string()]).is_ok());
    }

    /// Esc is an undo, not a quit: it abandons the edit in progress and hands the developer back to
    /// the menu they came from, with the file exactly as it was. Before this, `inquire` reported it
    /// as an error that unwound the whole session — one stray press and you were back at the shell.
    #[test]
    fn esc_abandons_an_edit_without_saving() {
        let tmp = tempfile::tempdir().unwrap();
        config().save(tmp.path()).unwrap();
        let before = std::fs::read_to_string(ReleaseConfig::path(tmp.path())).unwrap();

        // Every menu, each with the field prompt it would land on.
        let escapes = [
            (
                ConfigAction::Packages,
                GlobalField::Back,
                PackageField::Command,
            ),
            (
                ConfigAction::Packages,
                GlobalField::Back,
                PackageField::Targets,
            ),
            (
                ConfigAction::Packages,
                GlobalField::Back,
                PackageField::Mode,
            ),
            (
                ConfigAction::GlobalSettings,
                GlobalField::Provider,
                PackageField::Back,
            ),
            (
                ConfigAction::GlobalSettings,
                GlobalField::TagFormat,
                PackageField::Back,
            ),
            (
                ConfigAction::GlobalSettings,
                GlobalField::ChangelogScope,
                PackageField::Back,
            ),
            (
                ConfigAction::Ecosystems,
                GlobalField::Back,
                PackageField::Back,
            ),
        ];
        for (action, global_field, package_field) in escapes {
            orchestrate_with_prompt(
                tmp.path(),
                &NoPackages,
                &FakePrompt {
                    actions: RefCell::new(vec![action, ConfigAction::Exit]),
                    package: RefCell::new(Some("pkg".to_string())),
                    package_field: RefCell::new(package_field),
                    global_field: RefCell::new(global_field),
                    esc: RefCell::new(true),
                    // Empty: an Esc must not consume a queued reply, and popping from an empty
                    // queue would panic if it did.
                    text: RefCell::new(Vec::new()),
                    ..FakePrompt::default()
                },
            )
            .unwrap();

            assert_eq!(
                std::fs::read_to_string(ReleaseConfig::path(tmp.path())).unwrap(),
                before,
                "Esc at {action:?}/{global_field:?}/{package_field:?} wrote to release.toml"
            );
        }
    }

    /// Toggling a build-only flag is the one edit whose prompt has no text field to back out of, so
    /// it gets its own check that Esc leaves the flag alone.
    #[test]
    fn esc_leaves_a_toggle_at_its_current_value() {
        let tmp = tempfile::tempdir().unwrap();
        config().save(tmp.path()).unwrap();

        orchestrate_with_prompt(
            tmp.path(),
            &NoPackages,
            &FakePrompt {
                actions: RefCell::new(vec![ConfigAction::Packages, ConfigAction::Exit]),
                package: RefCell::new(Some("pkg".to_string())),
                package_field: RefCell::new(PackageField::Checksums),
                toggle: RefCell::new(true),
                esc: RefCell::new(true),
                ..FakePrompt::default()
            },
        )
        .unwrap();

        assert!(!ReleaseConfig::load(tmp.path()).unwrap().packages[0].checksums);
    }

    /// Adding a package to the repo and reaching for `config` -> Packages is the obvious path, so
    /// that menu re-reads the repo rather than listing `release.toml` back. Reading is all it does:
    /// a package shows up as a choice, and choosing nothing changes nothing.
    #[test]
    fn packages_menu_lists_a_new_repo_package_without_writing_anything() {
        let tmp = tempfile::tempdir().unwrap();
        let mut saved = config();
        saved.adapters = vec![Ecosystem::Cargo];
        saved.save(tmp.path()).unwrap();
        let before = std::fs::read_to_string(ReleaseConfig::path(tmp.path())).unwrap();

        let prompt = packages_menu(FakePrompt::default());
        orchestrate_with_prompt(tmp.path(), &DiscoversNewCrate, &prompt).unwrap();

        assert_eq!(prompt.offered_new.borrow().as_slice(), ["new-crate"]);
        assert_eq!(
            std::fs::read_to_string(ReleaseConfig::path(tmp.path())).unwrap(),
            before,
            "looking at the menu must not adopt a package"
        );
    }

    /// Answering *release it* is the moment the block is written — and the pick then falls straight
    /// through into the field editor, so a new package can be configured in one visit.
    #[test]
    fn releasing_a_new_package_writes_its_block_and_edits_it_in_one_visit() {
        let tmp = tempfile::tempdir().unwrap();
        let mut saved = config();
        saved.adapters = vec![Ecosystem::Cargo];
        saved.save(tmp.path()).unwrap();

        orchestrate_with_prompt(
            tmp.path(),
            &DiscoversNewCrate,
            &packages_menu(FakePrompt {
                package: RefCell::new(Some("new-crate".to_string())),
                new_package: RefCell::new(NewPackageAction::Add),
                package_field: RefCell::new(PackageField::Command),
                text: RefCell::new(vec!["cargo build --release".to_string()]),
                ..FakePrompt::default()
            }),
        )
        .unwrap();

        let cfg = ReleaseConfig::load(tmp.path()).unwrap();
        let added = cfg.package("new-crate").expect("block written");
        assert_eq!(added.adapter, Ecosystem::Cargo);
        assert_eq!(added.command, "cargo build --release");
        assert_eq!(
            added.manifest.as_deref(),
            Some("crates/new/Cargo.toml"),
            "the block points at the manifest discovery found"
        );
        // Packages already configured are untouched by the visit.
        assert_eq!(cfg.package("pkg").unwrap().command, "old build");
    }

    /// The other half of the choice: a package this repo will never release is recorded once and
    /// stops cluttering the menu, instead of being re-offered on every visit.
    #[test]
    fn skipping_a_new_package_records_it_and_stops_offering_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mut saved = config();
        saved.adapters = vec![Ecosystem::Cargo];
        saved.save(tmp.path()).unwrap();

        orchestrate_with_prompt(
            tmp.path(),
            &DiscoversNewCrate,
            &packages_menu(FakePrompt {
                package: RefCell::new(Some("new-crate".to_string())),
                new_package: RefCell::new(NewPackageAction::Skip),
                // Skipping ends the visit: a field prompt here would be a bug.
                package_field: RefCell::new(PackageField::Command),
                ..FakePrompt::default()
            }),
        )
        .unwrap();

        let cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.skip_publish, vec!["new-crate".to_string()]);
        assert!(cfg.package("new-crate").is_none(), "{:?}", cfg.packages);

        let again = packages_menu(FakePrompt::default());
        orchestrate_with_prompt(tmp.path(), &DiscoversNewCrate, &again).unwrap();
        assert!(again.offered_new.borrow().is_empty());
    }

    /// Backing out of the decision is not a decision: nothing is adopted, nothing is skipped, and
    /// the package is still there to answer next time.
    #[test]
    fn backing_out_of_a_new_package_leaves_it_undecided() {
        let tmp = tempfile::tempdir().unwrap();
        let mut saved = config();
        saved.adapters = vec![Ecosystem::Cargo];
        saved.save(tmp.path()).unwrap();

        orchestrate_with_prompt(
            tmp.path(),
            &DiscoversNewCrate,
            &packages_menu(FakePrompt {
                package: RefCell::new(Some("new-crate".to_string())),
                new_package: RefCell::new(NewPackageAction::Back),
                package_field: RefCell::new(PackageField::Command),
                ..FakePrompt::default()
            }),
        )
        .unwrap();

        let cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert!(cfg.package("new-crate").is_none());
        assert!(cfg.skip_publish.is_empty());
    }

    #[test]
    fn edits_package_fields() {
        let tmp = tempfile::tempdir().unwrap();
        config().save(tmp.path()).unwrap();

        orchestrate_with_prompt(
            tmp.path(),
            &NoPackages,
            &package_prompt(PackageField::Mode, vec![]),
        )
        .unwrap();
        let mut cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.packages[0].mode, Mode::Publish);

        orchestrate_with_prompt(
            tmp.path(),
            &NoPackages,
            &package_prompt(PackageField::Command, vec!["new build"]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.packages[0].command, "new build");

        orchestrate_with_prompt(
            tmp.path(),
            &NoPackages,
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
            &NoPackages,
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
            &NoPackages,
            &package_prompt(PackageField::GenericManifest, vec!["jsr.json"]),
        )
        .unwrap();
        orchestrate_with_prompt(
            tmp.path(),
            &NoPackages,
            &package_prompt(PackageField::GenericVersionField, vec!["pkg.version"]),
        )
        .unwrap();
        orchestrate_with_prompt(
            tmp.path(),
            &NoPackages,
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

    /// `config` reflects what is in the file; it never re-enables an ecosystem you removed. And
    /// disabling npm takes its `[discovery]` list with it, so a stale member list cannot linger and
    /// silently come back the next time npm is switched on.
    #[test]
    fn disabling_npm_keeps_it_disabled_and_clears_its_discovery_list() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = config();
        cfg.adapters = vec![Ecosystem::Npm, Ecosystem::Cargo];
        cfg.discovery.npm = vec![
            "packages/postgres".to_string(),
            "packages/redis".to_string(),
        ];
        cfg.save(tmp.path()).unwrap();

        // A prompt that confirms whatever is already enabled, minus npm — i.e. the user unchecks
        // npm and presses enter.
        struct KeepMinusNpm;
        impl ConfigPrompt for KeepMinusNpm {
            fn action(&self) -> Result<ConfigAction> {
                Ok(ConfigAction::Ecosystems)
            }
            fn ecosystems(&self, current: &[Ecosystem]) -> Result<Option<Vec<Ecosystem>>> {
                Ok(Some(
                    current
                        .iter()
                        .copied()
                        .filter(|e| *e != Ecosystem::Npm)
                        .collect(),
                ))
            }
            fn npm_packages(
                &self,
                _: &[GenericCandidate],
                _: &[usize],
            ) -> Result<Option<Vec<usize>>> {
                panic!("npm is disabled — discovery must not be asked about");
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
            fn changelog_strategy(
                &self,
                _: &ChangelogStrategy,
            ) -> Result<Option<ChangelogStrategy>> {
                unreachable!()
            }
            fn github_release_notes(
                &self,
                _: &GithubReleaseNotes,
            ) -> Result<Option<GithubReleaseNotes>> {
                unreachable!()
            }
            fn tag_format(&self, _: &str) -> Result<Option<String>> {
                unreachable!()
            }
            fn provider(&self, _: &str) -> Result<Option<String>> {
                unreachable!()
            }
            fn package_tag_format(
                &self,
                _: &str,
                _: &str,
                _: Option<&str>,
            ) -> Result<Option<TagFormatChoice>> {
                unreachable!()
            }
            fn skip_publish(&self, _: &[String], _: &[String]) -> Result<Option<Vec<String>>> {
                unreachable!()
            }
            fn legacy_tag_formats(
                &self,
                _: &[String],
                _: &[String],
            ) -> Result<Option<Vec<String>>> {
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

        // One pass, then the loop is broken by the error the second `action()` would raise; run the
        // single edit directly instead so the test does not depend on loop control flow.
        let mut loaded = ReleaseConfig::load(tmp.path()).unwrap();
        loaded.adapters = KeepMinusNpm
            .ecosystems(&loaded.adapters)
            .unwrap()
            .expect("the fake never cancels");
        edit_npm_discovery(tmp.path(), &KeepMinusNpm, &mut loaded).unwrap();
        loaded.save(tmp.path()).unwrap();

        let back = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(back.adapters, vec![Ecosystem::Cargo]);
        assert!(back.discovery.npm.is_empty(), "{:?}", back.discovery.npm);
    }

    #[test]
    fn scopes_and_clears_a_packages_tag_format_and_changelog() {
        let tmp = tempfile::tempdir().unwrap();
        config().save(tmp.path()).unwrap();

        orchestrate_with_prompt(
            tmp.path(),
            &NoPackages,
            &package_prompt(PackageField::TagFormat, vec!["{name}@{version}"]),
        )
        .unwrap();
        orchestrate_with_prompt(
            tmp.path(),
            &NoPackages,
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
            &NoPackages,
            &package_prompt(PackageField::TagFormat, vec![""]),
        )
        .unwrap();
        orchestrate_with_prompt(
            tmp.path(),
            &NoPackages,
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
            &NoPackages,
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
            &NoPackages,
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

        orchestrate_with_prompt(tmp.path(), &NoPackages, &ecosystem_prompt).unwrap();
        let mut cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.adapters, vec![Ecosystem::Npm, Ecosystem::Generic]);

        orchestrate_with_prompt(
            tmp.path(),
            &NoPackages,
            &global_prompt(GlobalField::Provider, vec!["github"]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.provider, "github");

        orchestrate_with_prompt(
            tmp.path(),
            &NoPackages,
            &global_prompt(GlobalField::SnapshotTag, vec!["canary"]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.snapshot_tag.as_deref(), Some("canary"));

        orchestrate_with_prompt(
            tmp.path(),
            &NoPackages,
            &FakePrompt {
                checked: RefCell::new(vec!["@scope/old".to_string(), "pkg-internal".to_string()]),
                ..global_prompt(GlobalField::SkipPublish, vec![])
            },
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.skip_publish, vec!["@scope/old", "pkg-internal"]);

        orchestrate_with_prompt(
            tmp.path(),
            &NoPackages,
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
            &NoPackages,
            &global_prompt(GlobalField::TagFormat, vec!["{name}@{version}"]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.tag_format, "{name}@{version}");

        orchestrate_with_prompt(
            tmp.path(),
            &NoPackages,
            &FakePrompt {
                checked: RefCell::new(vec!["{name}@{version}".to_string()]),
                ..global_prompt(GlobalField::LegacyTagFormats, vec![])
            },
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.legacy_tag_formats, vec!["{name}@{version}"]);

        orchestrate_with_prompt(
            tmp.path(),
            &NoPackages,
            &global_prompt(GlobalField::ChangelogScope, vec![]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.changelog_scope, ChangelogScope::Root);

        orchestrate_with_prompt(
            tmp.path(),
            &NoPackages,
            &global_prompt(GlobalField::ChangelogStrategy, vec![]),
        )
        .unwrap();
        cfg = ReleaseConfig::load(tmp.path()).unwrap();
        assert_eq!(cfg.changelog_strategy, ChangelogStrategy::Generated);

        orchestrate_with_prompt(
            tmp.path(),
            &NoPackages,
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
