//! Interactive prompts for the `version` command. Behind a trait so the flow can be driven
//! by a scripted fake in tests. The real impl uses [`inquire`] for arrow-key selection,
//! spacebar multi-select, and confirm prompts.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use inquire::{MultiSelect, Select};

use crate::adapter::{Bump, Pkg};

/// One release candidate: the package plus the head of each version line it can advance.
///
/// A package mid-prerelease has two independent heads — the manifest holds the prerelease
/// (`1.0.0-beta.3`) while the last stable tag holds the released line (`0.13.0`). The
/// Major/Minor/Patch groups advance the stable line; the prerelease group advances the other.
pub struct Candidate<'a> {
    pub pkg: &'a Pkg,
    /// Version of the highest stable tag, or `None` if the package never shipped a stable release.
    pub stable_base: Option<String>,
}

impl Candidate<'_> {
    /// Whether the manifest currently sits on a prerelease.
    pub fn on_prerelease(&self) -> bool {
        self.pkg.version.contains('-')
    }

    /// The version a stable (Major/Minor/Patch) bump is computed from. Only a package mid-prerelease
    /// reads the tag: for everything else the manifest is the head, which keeps a package whose
    /// manifest legitimately runs ahead of its last tag from regressing.
    pub fn stable_head(&self) -> &str {
        match &self.stable_base {
            Some(v) if self.on_prerelease() => v,
            _ => &self.pkg.version,
        }
    }
}

/// The interactions the `version` command needs from the user.
pub trait Prompt {
    /// Choose release candidates grouped by bump type.
    fn choose_bumps(&self, pending: &[Candidate]) -> Result<HashMap<String, Bump>>;
    /// Show the computed plan + changed-file summary and ask for final confirmation.
    fn confirm(
        &self,
        plan: &crate::summary::Plan,
        diff_stat: &str,
        skip_pr: bool,
        release_branch: &str,
        commit_title: &str,
    ) -> Result<bool>;
    /// Ask whether to return to main and delete the local release branch after it has been pushed.
    fn confirm_post_release_cleanup(&self, release_branch: &str) -> Result<bool>;
}

/// The real terminal prompt (arrow keys + spacebar via `inquire`).
pub struct StdinPrompt;

impl Prompt for StdinPrompt {
    fn choose_bumps(&self, pending: &[Candidate]) -> Result<HashMap<String, Bump>> {
        let mut selected = HashMap::new();
        let mut remaining: Vec<&Candidate> = pending.iter().collect();

        for (label, bump) in [
            ("Major", Bump::Major),
            ("Minor", Bump::Minor),
            ("Patch", Bump::Patch),
        ] {
            if remaining.is_empty() {
                break;
            }
            let chosen = choose_bump_group(label, &remaining, Some(&bump))?;
            println!("{}", group_summary(label, &chosen, remaining.len()));
            let chosen_set: HashSet<String> = chosen.into_iter().collect();
            for cand in &remaining {
                if chosen_set.contains(&cand.pkg.name) {
                    selected.insert(cand.pkg.name.clone(), bump.clone());
                }
            }
            remaining.retain(|cand| !chosen_set.contains(&cand.pkg.name));
        }

        if !remaining.is_empty() {
            let chosen = choose_bump_group("Other release types", &remaining, None)?;
            println!(
                "{}",
                group_summary("Other release types", &chosen, remaining.len())
            );
            let chosen_set: HashSet<String> = chosen.into_iter().collect();
            for cand in &remaining {
                if chosen_set.contains(&cand.pkg.name) {
                    selected.insert(
                        cand.pkg.name.clone(),
                        choose_detailed_bump(&cand.pkg.name, &cand.pkg.version)?,
                    );
                }
            }
        }

        Ok(selected)
    }

    fn confirm(
        &self,
        plan: &crate::summary::Plan,
        diff_stat: &str,
        skip_pr: bool,
        release_branch: &str,
        commit_title: &str,
    ) -> Result<bool> {
        crate::review::run(plan, diff_stat, skip_pr, release_branch, commit_title)
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

fn choose_bump_group(
    label: &str,
    pending: &[&Candidate],
    bump: Option<&Bump>,
) -> Result<Vec<String>> {
    let mut choices = vec![format!("All remaining packages ({})", pending.len())];
    for cand in pending {
        choices.push(candidate_line(cand, bump)?);
    }
    println!();
    let chosen = MultiSelect::new(&format!("{label} releases"), choices)
        .with_help_message("↑↓ move · space toggle · enter confirm")
        .raw_prompt()?;

    if chosen.iter().any(|item| item.index == 0) {
        return Ok(pending.iter().map(|c| c.pkg.name.clone()).collect());
    }
    Ok(chosen
        .iter()
        .filter_map(|item| pending.get(item.index.saturating_sub(1)))
        .map(|c| c.pkg.name.clone())
        .collect())
}

/// One selectable row. A group with a known bump shows the resulting version so the number is never
/// a surprise; the open-ended group shows the manifest version it will ask questions about instead.
///
/// A package mid-prerelease renders its stable head, not the manifest — the Major/Minor/Patch groups
/// advance the released line — with the in-flight prerelease called out so the row can't be read as
/// having lost it.
fn candidate_line(cand: &Candidate, bump: Option<&Bump>) -> Result<String> {
    let name = &cand.pkg.name;
    let Some(bump) = bump else {
        return Ok(format!("{name}  current {}", cand.pkg.version));
    };
    let head = cand.stable_head();
    let next = crate::version::apply_bump(head, bump)?;
    if cand.on_prerelease() && cand.stable_base.is_some() {
        return Ok(format!(
            "{name}  {head} -> {next}  [{} in flight]",
            cand.pkg.version
        ));
    }
    Ok(format!("{name}  {head} -> {next}"))
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

    fn pkg_at(version: &str) -> Pkg {
        Pkg {
            name: "@opentf/std".to_string(),
            version: version.to_string(),
            manifest_path: std::path::PathBuf::from("manifest"),
            changelog_path: std::path::PathBuf::from("CHANGELOG.md"),
            publishable: true,
            internal_deps: vec![],
        }
    }

    #[test]
    fn a_prerelease_package_shows_its_stable_line_in_the_bump_groups() {
        let pkg = pkg_at("1.0.0-beta.3");
        let cand = Candidate {
            pkg: &pkg,
            stable_base: Some("0.13.0".to_string()),
        };
        assert_eq!(
            candidate_line(&cand, Some(&Bump::Minor)).unwrap(),
            "@opentf/std  0.13.0 -> 0.14.0  [1.0.0-beta.3 in flight]"
        );
        // The open-ended group is where the prerelease line is advanced, so it shows the manifest.
        assert_eq!(
            candidate_line(&cand, None).unwrap(),
            "@opentf/std  current 1.0.0-beta.3"
        );
    }

    #[test]
    fn a_stable_package_shows_the_plain_bump() {
        let pkg = pkg_at("0.13.0");
        let cand = Candidate {
            pkg: &pkg,
            stable_base: None,
        };
        assert_eq!(
            candidate_line(&cand, Some(&Bump::Minor)).unwrap(),
            "@opentf/std  0.13.0 -> 0.14.0"
        );
    }

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
