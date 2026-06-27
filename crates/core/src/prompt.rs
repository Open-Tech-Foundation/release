//! Interactive prompts for the `version` command. Behind a trait so the flow can be driven
//! by a scripted fake in tests. The real impl uses [`inquire`] for arrow-key selection,
//! spacebar multi-select, and confirm prompts.

use anyhow::Result;
use inquire::{MultiSelect, Select};

use crate::adapter::{Bump, Pkg};

/// The four interactions the `version` command needs from the user.
pub trait Prompt {
    /// Choose the release channel (e.g. None for stable, Some("beta") for beta)
    fn select_channel(&self) -> Result<Option<String>>;
    /// Choose which of the pending packages to release; returns their names.
    fn select_packages(&self, pending: &[&Pkg]) -> Result<Vec<String>>;
    /// Choose a bump level for a selected package.
    fn choose_bump(&self, pkg_name: &str) -> Result<Bump>;
    /// Show the summary and ask for final confirmation.
    fn confirm(&self, summary: &str) -> Result<bool>;
}

/// The real terminal prompt (arrow keys + spacebar via `inquire`).
pub struct StdinPrompt;

impl Prompt for StdinPrompt {
    fn select_channel(&self) -> Result<Option<String>> {
        let choice = Select::new(
            "Release channel:",
            vec!["stable (default)", "alpha", "beta", "rc"],
        )
        .prompt()?;
        
        match choice {
            "stable (default)" => Ok(None),
            ch => Ok(Some(ch.to_string())),
        }
    }

    fn select_packages(&self, pending: &[&Pkg]) -> Result<Vec<String>> {
        let labels: Vec<String> = pending
            .iter()
            .map(|p| format!("{} ({})", p.name, p.version))
            .collect();
        let chosen = MultiSelect::new("Select packages to release:", labels)
            .with_help_message("↑↓ move · space toggle · enter confirm")
            .raw_prompt()?;
        Ok(chosen
            .iter()
            .map(|o| pending[o.index].name.clone())
            .collect())
    }

    fn choose_bump(&self, pkg_name: &str) -> Result<Bump> {
        let choice = Select::new(
            &format!("Bump for {pkg_name}:"),
            vec![Bump::Graduate, Bump::Major, Bump::Minor, Bump::Patch, Bump::Prerelease],
        )
        .prompt()?;
        Ok(choice)
    }

    fn confirm(&self, summary: &str) -> Result<bool> {
        print!("{summary}");
        Ok(Select::new("Proceed?", vec!["No", "Yes"])
            .raw_prompt()?
            .index
            == 1)
    }
}
