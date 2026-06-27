//! The `version` command — interactive, run **locally**.
//!
//! Produces a release PR; never publishes, never writes to `main`. See
//! `docs/commands/version.md` for the full step sequence.
//!
//! The orchestration is split so the side effects (prompts, git mutations, the forge) are
//! injected as traits — [`orchestrate`] is the testable core; [`run`] wires up the real
//! implementations.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{anyhow, bail, Result};

use crate::adapter::{Adapter, Bump, DepKind, Pkg};
use crate::changelog;
use crate::date;
use crate::forge::{Forge, GhForge};
use crate::git::{GitOps, GitRepo, RepoState};
use crate::graph::Graph;
use crate::preflight;
use crate::prompt::{Prompt, StdinPrompt};
use crate::summary::{self, Plan, RangeUpdate, VersionChange};

/// Options for a `version` run (wired up by the CLI crate).
#[derive(Debug, Clone, Default)]
pub struct VersionOptions {
    /// Compute and print the plan, but write nothing.
    pub dry_run: bool,
    /// Allow first-release of packages that have no prior tag.
    pub first_release: bool,
    /// Skip opening the PR (e.g. if gh CLI is missing).
    pub skip_pr: bool,
}

/// Wire up the real adapter/git/forge/prompt and run the flow.
pub fn run(adapter: &dyn Adapter, root: &Path, opts: &VersionOptions, hooks: &crate::config::Hooks) -> Result<()> {
    let mut opts = opts.clone();
    let prompt = StdinPrompt;
    if std::process::Command::new("gh")
        .arg("--version")
        .output()
        .is_err()
    {
        if !Prompt::confirm(&prompt, "\nGitHub CLI (`gh`) is not installed. Continue anyway? (You will need to manually open the PR)")? {
            bail!("Cancelled.");
        }
        opts.skip_pr = true;
    }
    let repo = GitRepo::new(root);
    let forge = GhForge::new(root);
    let hook_runner = crate::hooks::ShHookRunner;
    let today = date::today();
    orchestrate(adapter, &repo, &repo, &forge, &prompt, root, &today, &opts, hooks, &hook_runner)
}

/// The testable core of the `version` flow. `today` is injected for deterministic output.
#[allow(clippy::too_many_arguments)]
pub fn orchestrate(
    adapter: &dyn Adapter,
    repo: &dyn RepoState,
    git: &dyn GitOps,
    forge: &dyn Forge,
    prompt: &dyn Prompt,
    root: &Path,
    today: &str,
    opts: &VersionOptions,
    hooks: &crate::config::Hooks,
    hook_runner: &dyn crate::hooks::HookRunner,
) -> Result<()> {
    if !hooks.pre_version.is_empty() {
        hook_runner.run_hooks(root, &hooks.pre_version)?;
    }

    let packages = adapter.discover_packages()?;

    // 1. Strict preflight — abort before any prompt or mutation.
    let violations = preflight::check(repo, &packages, &[])?;
    if !violations.is_empty() {
        bail!("{}", preflight::format_violations(&violations));
    }

    // 2. Pending = publishable packages that carry curated [Unreleased] notes.
    let mut empties: HashMap<&str, bool> = HashMap::new();
    for p in &packages {
        empties.insert(p.name.as_str(), unreleased_is_empty(&p.changelog_path)?);
    }
    let pending: Vec<&Pkg> = packages
        .iter()
        .filter(|p| p.publishable && !empties[p.name.as_str()])
        .collect();
    if pending.is_empty() {
        println!("Nothing to release: no package has [Unreleased] notes.");
        return Ok(());
    }

    // 3. Prompt: multi-select, then a bump per selected package.
    let selected_names = prompt.select_packages(&pending)?;
    if selected_names.is_empty() {
        println!("Nothing selected.");
        return Ok(());
    }
    
    let by_name: HashMap<&str, &Pkg> = packages.iter().map(|p| (p.name.as_str(), p)).collect();
    let pending_names: HashSet<&str> = pending.iter().map(|p| p.name.as_str()).collect();
    
    let mut selected: HashMap<String, Bump> = HashMap::new();
    for name in &selected_names {
        if !pending_names.contains(name.as_str()) {
            bail!("selected package is not in the pending list: {name}");
        }
        let pkg = by_name[name.as_str()];
        selected.insert(name.clone(), prompt.choose_bump(name, &pkg.version)?);
    }

    // 4. Cascade through dependents (private leaves excluded).
    let graph = Graph::build(&packages)?;
    let bumps = graph.cascade(adapter, &selected)?;

    // 5. Compute new versions.
    let mut new_versions: HashMap<String, String> = HashMap::new();
    for (name, bump) in &bumps {
        let pkg = by_name[name.as_str()];
        new_versions.insert(name.clone(), apply_bump(&pkg.version, bump)?);
    }

    // 6. Build the summary plan.
    let mut changes: Vec<VersionChange> = bumps
        .iter()
        .map(|(name, bump)| {
            let pkg = by_name[name.as_str()];
            let is_selected = selected.contains_key(name);
            VersionChange {
                name: name.clone(),
                old_version: pkg.version.clone(),
                new_version: new_versions[name].clone(),
                selected: is_selected,
                note: change_note(pkg, bump, is_selected, &new_versions),
            }
        })
        .collect();
    changes.sort_by(|a, b| b.selected.cmp(&a.selected).then(a.name.cmp(&b.name)));

    let mut range_updates: Vec<RangeUpdate> = Vec::new();
    for p in &packages {
        for dep in &p.internal_deps {
            if let Some(new_dep_ver) = new_versions.get(&dep.name) {
                range_updates.push(RangeUpdate {
                    consumer: p.name.clone(),
                    dep: dep.name.clone(),
                    old_range: dep.range.clone(),
                    new_range: adapter.format_range(new_dep_ver),
                    consumer_private: !p.publishable,
                });
            }
        }
    }
    range_updates.sort_by(|a, b| a.consumer.cmp(&b.consumer).then(a.dep.cmp(&b.dep)));

    let plan = Plan {
        changes,
        range_updates,
    };
    let summary_text = summary::render(&plan);

    // 7. Dry run: print the plan and stop, writing nothing.
    if opts.dry_run {
        print!("{summary_text}");
        return Ok(());
    }

    // 8. Confirm. On cancel, write nothing.
    if !prompt.confirm(&summary_text)? {
        println!("Cancelled. Nothing written.");
        return Ok(());
    }

    // 9. Branch guard: clean tree, on `main`, then cut release/*.
    if !git.is_clean()? {
        bail!("working tree is not clean; commit or stash first");
    }
    let branch = git.current_branch()?;
    if branch != "main" {
        bail!("must be on `main` to start a release (currently on `{branch}`)");
    }
    let release_branch = format!("release/{today}");
    git.create_branch(&release_branch)?;

    // 10. Apply: versions, then internal ranges, then changelogs, then the lockfile.
    for (name, new_ver) in &new_versions {
        adapter.write_version(by_name[name.as_str()], new_ver)?;
    }
    for r in &plan.range_updates {
        adapter.update_dep_range(by_name[r.consumer.as_str()], &r.dep, &new_versions[&r.dep])?;
    }
    for (name, new_ver) in &new_versions {
        let pkg = by_name[name.as_str()];
        // Auto-bumped-only packages (empty [Unreleased]) get the stub.
        changelog::release_unreleased(&pkg.changelog_path, new_ver, today, empties[name.as_str()])?;
    }
    adapter.update_lockfile(root)?;

    if !hooks.post_version.is_empty() {
        hook_runner.run_hooks(root, &hooks.post_version)?;
    }

    // 11. Commit, push, open the PR.
    let mut titles: Vec<String> = selected
        .keys()
        .map(|n| format!("{n}@{}", new_versions[n]))
        .collect();
    titles.sort();
    let commit_title = format!("chore(release): {}", titles.join(", "));
    git.add_all()?;
    git.commit(&commit_title)?;
    git.push_branch(&release_branch)?;

    if opts.skip_pr {
        println!("Skipped PR creation. Please manually open a PR for branch `{release_branch}` on GitHub.");
    } else {
        forge.open_pr(&release_branch, &commit_title, &summary_text)?;
        println!("Opened release PR from `{release_branch}`.");
    }

    Ok(())
}

/// Apply a bump to an `x.y.z` version (pre-release/build metadata is dropped unless entering/iterating prerelease).
fn apply_bump(version: &str, bump: &Bump) -> Result<String> {
    let mut parts = version.split('-');
    let core = parts.next().unwrap();
    let pre = parts.next();

    let mut core_parts = core.split('.');
    let mut next = || -> Result<u64> {
        core_parts
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow!("not a valid x.y.z version: {version}"))
    };
    let (major, minor, patch) = (next()?, next()?, next()?);

    match bump {
        Bump::Graduate => {
            if pre.is_none() {
                bail!("Cannot graduate a stable version: {version}. Select Major/Minor/Patch instead.");
            }
            Ok(format!("{major}.{minor}.{patch}"))
        }
        Bump::Prerelease(ch) => {
            if let Some(p) = pre {
                if p.starts_with(ch) {
                    let mut p_parts = p.split('.');
                    p_parts.next(); // skip channel name
                    let num: u64 = p_parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                    return Ok(format!("{core}-{ch}.{}", num + 1));
                }
            }
            Ok(format!("{core}-{ch}.0"))
        }
        Bump::PreMajor(ch) => Ok(format!("{}.0.0-{ch}.0", major + 1)),
        Bump::PreMinor(ch) => Ok(format!("{major}.{}.0-{ch}.0", minor + 1)),
        Bump::PrePatch(ch) => Ok(format!("{major}.{minor}.{}-{ch}.0", patch + 1)),
        Bump::Major => Ok(format!("{}.0.0", major + 1)),
        Bump::Minor => Ok(format!("{major}.{}.0", minor + 1)),
        Bump::Patch => Ok(format!("{major}.{minor}.{}", patch + 1)),
    }
}

/// The parenthetical reason shown in the summary for one change.
fn change_note(
    pkg: &Pkg,
    bump: &Bump,
    selected: bool,
    new_versions: &HashMap<String, String>,
) -> String {
    if selected {
        return format!("{}, selected", bump_word(bump));
    }
    match pkg
        .internal_deps
        .iter()
        .find(|d| new_versions.contains_key(&d.name))
    {
        Some(dep) if dep.kind == DepKind::PeerDep => {
            format!("mirror {} — peerDep on {}", bump_word(bump), dep.name)
        }
        Some(dep) => format!("{} — depends on {}", bump_word(bump), dep.name),
        None => bump_word(bump).to_string(),
    }
}

fn bump_word(bump: &Bump) -> &'static str {
    match bump {
        Bump::Graduate => "graduate",
        Bump::PreMajor(_) | Bump::Major => "major",
        Bump::PreMinor(_) | Bump::Minor => "minor",
        Bump::PrePatch(_) | Bump::Patch => "patch",
        Bump::Prerelease(_) => "prerelease",
    }
}

fn unreleased_is_empty(changelog_path: &Path) -> Result<bool> {
    if !changelog_path.exists() {
        return Ok(true);
    }
    Ok(changelog::parse_unreleased(changelog_path)?.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_bump_increments_and_resets() {
        assert_eq!(apply_bump("1.2.3", &Bump::Major).unwrap(), "2.0.0");
        assert_eq!(apply_bump("1.2.3", &Bump::Minor).unwrap(), "1.3.0");
        assert_eq!(apply_bump("1.2.3", &Bump::Patch).unwrap(), "1.2.4");
        // Test transition from pre-release to stable
        assert_eq!(apply_bump("0.1.0-next.1", &Bump::Patch).unwrap(), "0.1.1");
        
        // Test pre-release bumps
        assert_eq!(apply_bump("1.2.3", &Bump::PreMinor("beta".to_string())).unwrap(), "1.3.0-beta.0");
        assert_eq!(apply_bump("1.3.0-beta.0", &Bump::Prerelease("beta".to_string())).unwrap(), "1.3.0-beta.1");
        assert_eq!(apply_bump("1.3.0-beta.1", &Bump::Prerelease("beta".to_string())).unwrap(), "1.3.0-beta.2");
        assert_eq!(apply_bump("1.2.3", &Bump::PrePatch("rc".to_string())).unwrap(), "1.2.4-rc.0");

        // Test switch channel
        assert_eq!(apply_bump("1.3.0-alpha.1", &Bump::Prerelease("beta".to_string())).unwrap(), "1.3.0-beta.0");

        // Test graduate
        assert_eq!(apply_bump("1.3.0-beta.2", &Bump::Graduate).unwrap(), "1.3.0");
        assert!(apply_bump("1.3.0", &Bump::Graduate).is_err());

        assert!(apply_bump("nope", &Bump::Patch).is_err());
    }

    #[test]
    fn change_note_explains_cascade_reason() {
        let pkg = Pkg {
            name: "sdk".into(),
            version: "1.0.0".into(),
            manifest_path: "sdk/package.json".into(),
            changelog_path: "sdk/CHANGELOG.md".into(),
            publishable: true,
            internal_deps: vec![crate::adapter::InternalDep {
                name: "core".into(),
                kind: DepKind::PeerDep,
                range: "^1.0.0".into(),
            }],
        };
        let new_versions = HashMap::from([("core".to_string(), "2.0.0".to_string())]);
        assert_eq!(
            change_note(&pkg, &Bump::Major, false, &new_versions),
            "mirror major — peerDep on core"
        );
        assert_eq!(
            change_note(&pkg, &Bump::Major, true, &new_versions),
            "major, selected"
        );
    }
}
