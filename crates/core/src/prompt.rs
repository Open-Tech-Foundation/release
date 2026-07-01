//! Interactive prompts for the `version` command. Behind a trait so the flow can be driven
//! by a scripted fake in tests. The real impl uses [`inquire`] for arrow-key selection,
//! spacebar multi-select, and confirm prompts.

use anyhow::Result;
use inquire::{Confirm, MultiSelect, Select};

use crate::adapter::{Bump, Pkg};

/// The interactions the `version` command needs from the user.
pub trait Prompt {
    /// Choose which of the pending packages to release; returns their names.
    fn select_packages(&self, pending: &[&Pkg]) -> Result<Vec<String>>;
    /// Interactive flow to determine the next version bump for a package.
    fn choose_bump(&self, pkg_name: &str, current_version: &str) -> Result<Bump>;
    /// Show the computed plan + changed-file summary and ask for final confirmation.
    fn confirm(&self, plan: &crate::summary::Plan, diff_stat: &str, skip_pr: bool) -> Result<bool>;
    /// Ask whether to return to main and delete the local release branch after it has been pushed.
    fn confirm_post_release_cleanup(&self, release_branch: &str) -> Result<bool>;
}

/// The real terminal prompt (arrow keys + spacebar via `inquire`).
pub struct StdinPrompt;

impl Prompt for StdinPrompt {
    fn select_packages(&self, pending: &[&Pkg]) -> Result<Vec<String>> {
        let labels: Vec<String> = pending
            .iter()
            .map(|p| format!("{}  current {}", p.name, p.version))
            .collect();
        println!();
        let chosen = MultiSelect::new("Release candidates", labels)
            .with_help_message("↑↓ move · space toggle · enter confirm")
            .raw_prompt()?;
        Ok(chosen
            .iter()
            .map(|o| pending[o.index].name.clone())
            .collect())
    }

    fn choose_bump(&self, pkg_name: &str, current_version: &str) -> Result<Bump> {
        println!();
        let parts: Vec<&str> = current_version.split('-').collect();
        let is_prerelease = parts.len() > 1;

        if is_prerelease {
            let pre_part = parts[1];
            let current_channel = pre_part.split('.').next().unwrap();
            let msg =
                format!("{pkg_name} is currently on the {current_channel} channel. Next step?");
            let opts = vec![
                format!("Continue {current_channel} prerelease"),
                "Switch prerelease channel".to_string(),
                "Graduate to stable".to_string(),
            ];
            let choice = Select::new(&msg, opts).prompt()?;
            if choice == "Graduate to stable" {
                Ok(Bump::Graduate)
            } else if choice == "Switch prerelease channel" {
                let ch = Select::new("Prerelease channel", vec!["alpha", "beta", "rc"]).prompt()?;
                Ok(Bump::Prerelease(ch.to_string()))
            } else {
                Ok(Bump::Prerelease(current_channel.to_string()))
            }
        } else {
            let rtype = Select::new(
                &format!("{pkg_name} release track"),
                vec!["Stable", "Pre-release"],
            )
            .prompt()?;

            let is_pre = rtype == "Pre-release";
            let channel = if is_pre {
                Some(Select::new("Prerelease channel", vec!["alpha", "beta", "rc"]).prompt()?)
            } else {
                None
            };

            let bump_str = Select::new("Version bump", vec!["Major", "Minor", "Patch"]).prompt()?;

            Ok(match (bump_str, channel) {
                ("Major", None) => Bump::Major,
                ("Minor", None) => Bump::Minor,
                ("Patch", None) => Bump::Patch,
                ("Major", Some(c)) => Bump::PreMajor(c.to_string()),
                ("Minor", Some(c)) => Bump::PreMinor(c.to_string()),
                ("Patch", Some(c)) => Bump::PrePatch(c.to_string()),
                _ => unreachable!(),
            })
        }
    }

    fn confirm(&self, plan: &crate::summary::Plan, diff_stat: &str, skip_pr: bool) -> Result<bool> {
        crate::review::run(plan, diff_stat, skip_pr)
    }

    fn confirm_post_release_cleanup(&self, release_branch: &str) -> Result<bool> {
        Ok(Confirm::new(&format!(
            "Post-release cleanup: switch to main, pull tags, and delete local branch `{release_branch}`?"
        ))
        .with_default(true)
        .prompt()?)
    }
}
