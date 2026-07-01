//! Interactive prompts for the `version` command. Behind a trait so the flow can be driven
//! by a scripted fake in tests. The real impl uses [`inquire`] for arrow-key selection,
//! spacebar multi-select, and confirm prompts.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use inquire::{MultiSelect, Select};

use crate::adapter::{Bump, Pkg};

/// The interactions the `version` command needs from the user.
pub trait Prompt {
    /// Choose release candidates grouped by bump type.
    fn choose_bumps(&self, pending: &[&Pkg]) -> Result<HashMap<String, Bump>>;
    /// Show the computed plan + changed-file summary and ask for final confirmation.
    fn confirm(&self, plan: &crate::summary::Plan, diff_stat: &str, skip_pr: bool) -> Result<bool>;
    /// Ask whether to return to main and delete the local release branch after it has been pushed.
    fn confirm_post_release_cleanup(&self, release_branch: &str) -> Result<bool>;
}

/// The real terminal prompt (arrow keys + spacebar via `inquire`).
pub struct StdinPrompt;

impl Prompt for StdinPrompt {
    fn choose_bumps(&self, pending: &[&Pkg]) -> Result<HashMap<String, Bump>> {
        let mut selected = HashMap::new();
        let mut remaining: Vec<&Pkg> = pending.to_vec();

        for (label, bump) in [
            ("Major", Bump::Major),
            ("Minor", Bump::Minor),
            ("Patch", Bump::Patch),
        ] {
            if remaining.is_empty() {
                break;
            }
            let chosen = choose_bump_group(label, &remaining)?;
            println!("{}", group_summary(label, &chosen, remaining.len()));
            let chosen_set: HashSet<String> = chosen.into_iter().collect();
            for pkg in &remaining {
                if chosen_set.contains(&pkg.name) {
                    selected.insert(pkg.name.clone(), bump.clone());
                }
            }
            remaining.retain(|pkg| !chosen_set.contains(&pkg.name));
        }

        if !remaining.is_empty() {
            let chosen = choose_bump_group("Other release types", &remaining)?;
            println!(
                "{}",
                group_summary("Other release types", &chosen, remaining.len())
            );
            let chosen_set: HashSet<String> = chosen.into_iter().collect();
            for pkg in &remaining {
                if chosen_set.contains(&pkg.name) {
                    selected.insert(
                        pkg.name.clone(),
                        choose_detailed_bump(&pkg.name, &pkg.version)?,
                    );
                }
            }
        }

        Ok(selected)
    }

    fn confirm(&self, plan: &crate::summary::Plan, diff_stat: &str, skip_pr: bool) -> Result<bool> {
        crate::review::run(plan, diff_stat, skip_pr)
    }

    fn confirm_post_release_cleanup(&self, release_branch: &str) -> Result<bool> {
        Ok(Select::new(
            &format!(
                "Post-release cleanup: switch to main, pull tags, and delete local branch `{release_branch}`?"
            ),
            vec!["Yes", "No"],
        )
        .with_starting_cursor(0)
        .prompt()?
            == "Yes")
    }
}

fn choose_bump_group(label: &str, pending: &[&Pkg]) -> Result<Vec<String>> {
    let mut choices = vec![format!("All remaining packages ({})", pending.len())];
    choices.extend(
        pending
            .iter()
            .map(|p| format!("{}  current {}", p.name, p.version)),
    );
    println!();
    let chosen = MultiSelect::new(&format!("{label} releases"), choices)
        .with_help_message("↑↓ move · space toggle · enter confirm")
        .raw_prompt()?;

    if chosen.iter().any(|item| item.index == 0) {
        return Ok(pending.iter().map(|pkg| pkg.name.clone()).collect());
    }
    Ok(chosen
        .iter()
        .filter_map(|item| pending.get(item.index.saturating_sub(1)))
        .map(|pkg| pkg.name.clone())
        .collect())
}

fn group_summary(label: &str, chosen: &[String], pending_count: usize) -> String {
    if chosen.is_empty() {
        return format!("Skipped {label} releases: no packages selected.");
    }
    if chosen.len() == pending_count {
        return format!("Selected {label} releases: all remaining packages ({pending_count}).");
    }
    format!("Selected {label} releases: {}.", chosen.join(", "))
}

fn choose_detailed_bump(pkg_name: &str, current_version: &str) -> Result<Bump> {
    println!();
    let parts: Vec<&str> = current_version.split('-').collect();
    let is_prerelease = parts.len() > 1;

    if is_prerelease {
        let pre_part = parts[1];
        let current_channel = pre_part.split('.').next().unwrap();
        let msg = format!("{pkg_name} is currently on the {current_channel} channel. Next step?");
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
            vec!["Pre-release", "Stable"],
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_summary_names_skipped_groups() {
        assert_eq!(
            group_summary("Major", &[], 7),
            "Skipped Major releases: no packages selected."
        );
    }

    #[test]
    fn group_summary_names_all_remaining_selection() {
        let chosen = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            group_summary("Minor", &chosen, 2),
            "Selected Minor releases: all remaining packages (2)."
        );
    }

    #[test]
    fn group_summary_lists_partial_selection() {
        let chosen = vec!["@scope/a".to_string(), "@scope/b".to_string()];
        assert_eq!(
            group_summary("Patch", &chosen, 3),
            "Selected Patch releases: @scope/a, @scope/b."
        );
    }
}
