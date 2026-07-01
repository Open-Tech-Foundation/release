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
            let chosen_set: HashSet<String> = chosen.into_iter().collect();
            for pkg in &remaining {
                if chosen_set.contains(&pkg.name) {
                    selected.insert(pkg.name.clone(), bump.clone());
                }
            }
            remaining.retain(|pkg| !chosen_set.contains(&pkg.name));
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
