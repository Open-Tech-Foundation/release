//! Interactive prompts for the `version` command. Behind a trait so the flow can be driven
//! by a scripted fake in tests.

use std::io::{self, Write};

use anyhow::{anyhow, Result};

use crate::adapter::{Bump, Pkg};

/// The three interactions the `version` command needs from the user.
pub trait Prompt {
    /// Choose which of the pending packages to release; returns their names.
    fn select_packages(&self, pending: &[&Pkg]) -> Result<Vec<String>>;
    /// Choose a bump level for a selected package.
    fn choose_bump(&self, pkg_name: &str) -> Result<Bump>;
    /// Show the summary and ask for final confirmation.
    fn confirm(&self, summary: &str) -> Result<bool>;
}

/// The real terminal prompt.
pub struct StdinPrompt;

fn read_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

impl Prompt for StdinPrompt {
    fn select_packages(&self, pending: &[&Pkg]) -> Result<Vec<String>> {
        println!("Packages with [Unreleased] notes:");
        for (i, p) in pending.iter().enumerate() {
            println!("  {}) {} ({})", i + 1, p.name, p.version);
        }
        let line = read_line("Select to release (e.g. 1,3 or 'all'): ")?;
        if line.eq_ignore_ascii_case("all") {
            return Ok(pending.iter().map(|p| p.name.clone()).collect());
        }
        let mut selected = Vec::new();
        for token in line.split([',', ' ', '\t']).filter(|t| !t.is_empty()) {
            let idx: usize = token
                .parse()
                .map_err(|_| anyhow!("invalid selection: {token}"))?;
            let pkg = pending
                .get(idx.wrapping_sub(1))
                .ok_or_else(|| anyhow!("selection out of range: {idx}"))?;
            selected.push(pkg.name.clone());
        }
        Ok(selected)
    }

    fn choose_bump(&self, pkg_name: &str) -> Result<Bump> {
        loop {
            let line = read_line(&format!("Bump for {pkg_name} [major/minor/patch]: "))?;
            match line.to_ascii_lowercase().as_str() {
                "major" | "maj" => return Ok(Bump::Major),
                "minor" | "min" => return Ok(Bump::Minor),
                "patch" | "pat" => return Ok(Bump::Patch),
                _ => println!("please type major, minor, or patch"),
            }
        }
    }

    fn confirm(&self, summary: &str) -> Result<bool> {
        print!("{summary}");
        let line = read_line("Proceed? (y/N): ")?;
        Ok(matches!(line.to_ascii_lowercase().as_str(), "y" | "yes"))
    }
}
