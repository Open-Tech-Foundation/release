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
pub fn run(
    adapter: &dyn Adapter,
    root: &Path,
    opts: &VersionOptions,
    config: &crate::config::ReleaseConfig,
) -> Result<()> {
    run_many(&[adapter], root, opts, config)
}

/// Wire up real side effects and run a single release transaction across all enabled adapters.
pub fn run_many(
    adapters: &[&dyn Adapter],
    root: &Path,
    opts: &VersionOptions,
    config: &crate::config::ReleaseConfig,
) -> Result<()> {
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
    orchestrate_many(
        adapters,
        &repo,
        &repo,
        &forge,
        &prompt,
        root,
        &today,
        &opts,
        config,
        &hook_runner,
    )
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
    config: &crate::config::ReleaseConfig,
    hook_runner: &dyn crate::hooks::HookRunner,
) -> Result<()> {
    orchestrate_many(
        &[adapter],
        repo,
        git,
        forge,
        prompt,
        root,
        today,
        opts,
        config,
        hook_runner,
    )
}

struct AdapterPackages<'a> {
    adapter: &'a dyn Adapter,
    packages: Vec<Pkg>,
}

/// The testable core of the multi-adapter `version` flow.
#[allow(clippy::too_many_arguments)]
pub fn orchestrate_many(
    adapters: &[&dyn Adapter],
    repo: &dyn RepoState,
    git: &dyn GitOps,
    forge: &dyn Forge,
    prompt: &dyn Prompt,
    root: &Path,
    today: &str,
    opts: &VersionOptions,
    config: &crate::config::ReleaseConfig,
    hook_runner: &dyn crate::hooks::HookRunner,
) -> Result<()> {
    if adapters.is_empty() {
        println!("Nothing to release: no adapters are enabled.");
        return Ok(());
    }

    if !config.hooks.pre_version.is_empty() {
        hook_runner.run_hooks(root, &config.hooks.pre_version)?;
    }

    let mut adapter_packages = Vec::with_capacity(adapters.len());
    let mut seen = HashSet::new();
    for adapter in adapters {
        let packages = adapter.discover_packages()?;
        for pkg in &packages {
            if !seen.insert(pkg.name.clone()) {
                bail!(
                    "duplicate package name across enabled adapters: {}",
                    pkg.name
                );
            }
        }
        adapter_packages.push(AdapterPackages {
            adapter: *adapter,
            packages,
        });
    }

    // 1. Strict preflight — abort before any prompt or mutation.
    let all_packages: Vec<Pkg> = adapter_packages
        .iter()
        .flat_map(|ctx| ctx.packages.iter().cloned())
        .collect();
    let violations = preflight::check_with_options(
        repo,
        &all_packages,
        &[],
        preflight::CheckOptions {
            allow_first_release: opts.first_release,
        },
    )?;
    if !violations.is_empty() {
        bail!("{}", preflight::format_violations(&violations));
    }

    // 2. Pending = publishable packages that carry curated [Unreleased] notes.
    let mut empties: HashMap<&str, bool> = HashMap::new();
    let mut generated_notes: HashMap<&str, String> = HashMap::new();
    let is_generated = config.changelog_strategy == crate::config::ChangelogStrategy::Generated;

    for p in &all_packages {
        if is_generated {
            let last = repo.last_tag(&p.name)?;
            let notes = repo.commits_since(last.as_deref(), p.manifest_path.parent().unwrap())?;
            generated_notes.insert(p.name.as_str(), notes.clone());
            empties.insert(p.name.as_str(), notes.is_empty());
        } else {
            empties.insert(p.name.as_str(), unreleased_is_empty(&p.changelog_path)?);
        }
    }
    let pending: Vec<&Pkg> = all_packages
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

    let by_name: HashMap<&str, &Pkg> = all_packages.iter().map(|p| (p.name.as_str(), p)).collect();
    let pending_names: HashSet<&str> = pending.iter().map(|p| p.name.as_str()).collect();

    let mut selected: HashMap<String, Bump> = HashMap::new();
    for name in &selected_names {
        if !pending_names.contains(name.as_str()) {
            bail!("selected package is not in the pending list: {name}");
        }
        let pkg = by_name[name.as_str()];
        selected.insert(name.clone(), prompt.choose_bump(name, &pkg.version)?);
    }

    // 4. Cascade through dependents within each adapter (private leaves excluded).
    let mut adapter_bumps = Vec::with_capacity(adapter_packages.len());
    let mut new_versions: HashMap<String, String> = HashMap::new();
    for ctx in &adapter_packages {
        let ctx_names: HashSet<&str> = ctx.packages.iter().map(|p| p.name.as_str()).collect();
        let selected_for_adapter: HashMap<String, Bump> = selected
            .iter()
            .filter(|(name, _)| ctx_names.contains(name.as_str()))
            .map(|(name, bump)| (name.clone(), bump.clone()))
            .collect();
        let graph = Graph::build(&ctx.packages)?;
        let bumps = graph.cascade(ctx.adapter, &selected_for_adapter)?;
        for (name, bump) in &bumps {
            let pkg = by_name[name.as_str()];
            new_versions.insert(name.clone(), apply_bump(&pkg.version, bump)?);
        }
        adapter_bumps.push(bumps);
    }

    if new_versions.is_empty() {
        println!("Nothing to release: no selected package was publishable.");
        return Ok(());
    }

    // 5. Build the summary plan.
    let mut changes: Vec<VersionChange> = adapter_bumps
        .iter()
        .flat_map(|bumps| bumps.iter())
        .map(|(name, bump)| {
            let pkg = by_name[name.as_str()];
            let is_selected = selected.contains_key(name);
            let mut note = change_note(pkg, bump, is_selected, &new_versions);
            if is_generated && is_selected {
                let gen = generated_notes
                    .get(name.as_str())
                    .cloned()
                    .unwrap_or_default();
                if !gen.is_empty() {
                    note = format!("{note}\n\nGenerated notes:\n{gen}");
                }
            }
            VersionChange {
                name: name.clone(),
                old_version: pkg.version.clone(),
                new_version: new_versions[name].clone(),
                selected: is_selected,
                note,
            }
        })
        .collect();
    changes.sort_by(|a, b| b.selected.cmp(&a.selected).then(a.name.cmp(&b.name)));

    let mut range_updates: Vec<RangeUpdate> = Vec::new();
    let mut apply_ranges: Vec<(usize, String, String)> = Vec::new();
    for (idx, ctx) in adapter_packages.iter().enumerate() {
        for p in &ctx.packages {
            for dep in &p.internal_deps {
                if let Some(new_dep_ver) = new_versions.get(&dep.name) {
                    range_updates.push(RangeUpdate {
                        consumer: p.name.clone(),
                        dep: dep.name.clone(),
                        old_range: dep.range.clone(),
                        new_range: ctx.adapter.format_range(new_dep_ver),
                        consumer_private: !p.publishable,
                    });
                    apply_ranges.push((idx, p.name.clone(), dep.name.clone()));
                }
            }
        }
    }
    range_updates.sort_by(|a, b| a.consumer.cmp(&b.consumer).then(a.dep.cmp(&b.dep)));

    let plan = Plan {
        changes,
        range_updates,
    };
    let summary_text = summary::render(&plan);

    // 6. Dry run: print the plan and stop, writing nothing.
    if opts.dry_run {
        print!("{summary_text}");
        return Ok(());
    }

    // 7. Confirm. On cancel, write nothing.
    if !prompt.confirm(&summary_text)? {
        println!("Cancelled. Nothing written.");
        return Ok(());
    }

    // 8. Branch guard: clean tree, on `main`, then cut release/*.
    if !git.is_clean()? {
        bail!("working tree is not clean; commit or stash first");
    }
    let branch = git.current_branch()?;
    if branch != "main" {
        bail!("must be on `main` to start a release (currently on `{branch}`)");
    }
    let release_branch = format!("release/{today}");
    git.create_branch(&release_branch)?;

    // 9. Apply: versions, then internal ranges, then changelogs, then lockfiles.
    for (idx, ctx) in adapter_packages.iter().enumerate() {
        for name in adapter_bumps[idx].keys() {
            let new_ver = &new_versions[name];
            ctx.adapter.write_version(by_name[name.as_str()], new_ver)?;
        }
    }
    for (idx, consumer, dep) in &apply_ranges {
        let ctx = &adapter_packages[*idx];
        ctx.adapter
            .update_dep_range(by_name[consumer.as_str()], dep, &new_versions[dep])?;
    }
    // Several packages can map to a single CHANGELOG.md — cargo lockstep crates and generic
    // packages share the root file. Each rewrite moves `[Unreleased]` into a dated section, so
    // writing the same file twice would garble it (the second pass operates on the freshly
    // emptied `[Unreleased]`). Write each changelog exactly once. Names are visited in sorted
    // order so the package that "owns" a shared file is deterministic.
    let mut changelog_names: Vec<&String> = new_versions.keys().collect();
    changelog_names.sort();
    let mut written_changelogs: HashSet<&Path> = HashSet::new();
    for name in changelog_names {
        let pkg = by_name[name.as_str()];
        if !written_changelogs.insert(pkg.changelog_path.as_path()) {
            continue; // already rewritten via another package sharing this file
        }
        let new_ver = &new_versions[name];
        if is_generated {
            let gen = generated_notes
                .get(name.as_str())
                .cloned()
                .unwrap_or_default();
            crate::changelog::prepend_generated(&pkg.changelog_path, new_ver, today, &gen)?;
        } else {
            // Auto-bumped-only packages (empty [Unreleased]) get the stub.
            changelog::release_unreleased(
                &pkg.changelog_path,
                new_ver,
                today,
                empties[name.as_str()],
            )?;
        }
    }
    for (idx, ctx) in adapter_packages.iter().enumerate() {
        if !adapter_bumps[idx].is_empty() {
            ctx.adapter.update_lockfile(root)?;
        }
    }

    if !config.hooks.post_version.is_empty() {
        hook_runner.run_hooks(root, &config.hooks.post_version)?;
    }

    // 10. Commit, push, open the PR.
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
    use std::cell::RefCell;
    use std::path::Path;

    use crate::forge::Forge;

    struct FakeVersionAdapter {
        packages: Vec<Pkg>,
        writes: RefCell<Vec<(String, String)>>,
        lockfile_updates: RefCell<usize>,
    }

    impl FakeVersionAdapter {
        fn new(pkg: Pkg) -> Self {
            Self::with_packages(vec![pkg])
        }

        fn with_packages(packages: Vec<Pkg>) -> Self {
            Self {
                packages,
                writes: RefCell::new(Vec::new()),
                lockfile_updates: RefCell::new(0),
            }
        }
    }

    impl Adapter for FakeVersionAdapter {
        fn discover_packages(&self) -> Result<Vec<Pkg>> {
            Ok(self.packages.clone())
        }

        fn write_version(&self, pkg: &Pkg, new: &str) -> Result<()> {
            self.writes
                .borrow_mut()
                .push((pkg.name.clone(), new.to_string()));
            Ok(())
        }

        fn update_dep_range(&self, _: &Pkg, _: &str, _: &str) -> Result<()> {
            Ok(())
        }

        fn format_range(&self, version: &str) -> String {
            version.to_string()
        }

        fn resolve_workspace_links(&self, _: &Pkg) -> Result<()> {
            Ok(())
        }

        fn update_lockfile(&self, _: &Path) -> Result<()> {
            *self.lockfile_updates.borrow_mut() += 1;
            Ok(())
        }

        fn dependent_bump(&self, _: Bump, _: &DepKind) -> Bump {
            Bump::Patch
        }

        fn is_published(&self, _: &Pkg, _: &str) -> Result<bool> {
            Ok(false)
        }

        fn publish(&self, _: &Pkg, _: Option<&Path>) -> Result<()> {
            Ok(())
        }
    }

    struct FakeRepo;

    impl RepoState for FakeRepo {
        fn last_tag(&self, pkg_name: &str) -> Result<Option<String>> {
            Ok(Some(format!("{pkg_name}@1.0.0")))
        }

        fn commit_count_since(&self, _: &str, _: &Path) -> Result<usize> {
            Ok(0)
        }

        fn commits_since(&self, _: Option<&str>, _: &Path) -> Result<String> {
            Ok(String::new())
        }
    }

    struct FakeGit {
        branch: RefCell<String>,
        created: RefCell<Vec<String>>,
        commits: RefCell<Vec<String>>,
        pushes: RefCell<Vec<String>>,
    }

    impl FakeGit {
        fn new() -> Self {
            Self {
                branch: RefCell::new("main".to_string()),
                created: RefCell::new(Vec::new()),
                commits: RefCell::new(Vec::new()),
                pushes: RefCell::new(Vec::new()),
            }
        }
    }

    impl GitOps for FakeGit {
        fn is_clean(&self) -> Result<bool> {
            Ok(true)
        }

        fn current_branch(&self) -> Result<String> {
            Ok(self.branch.borrow().clone())
        }

        fn create_branch(&self, name: &str) -> Result<()> {
            self.created.borrow_mut().push(name.to_string());
            *self.branch.borrow_mut() = name.to_string();
            Ok(())
        }

        fn add_all(&self) -> Result<()> {
            Ok(())
        }

        fn commit(&self, message: &str) -> Result<()> {
            self.commits.borrow_mut().push(message.to_string());
            Ok(())
        }

        fn push_branch(&self, name: &str) -> Result<()> {
            self.pushes.borrow_mut().push(name.to_string());
            Ok(())
        }

        fn create_tag(&self, _: &str) -> Result<()> {
            Ok(())
        }

        fn push_tag(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    struct FakePrompt;

    impl Prompt for FakePrompt {
        fn select_packages(&self, pending: &[&Pkg]) -> Result<Vec<String>> {
            Ok(pending.iter().map(|pkg| pkg.name.clone()).collect())
        }

        fn choose_bump(&self, _: &str, _: &str) -> Result<Bump> {
            Ok(Bump::Patch)
        }

        fn confirm(&self, _: &str) -> Result<bool> {
            Ok(true)
        }
    }

    struct FakeForge {
        prs: RefCell<Vec<String>>,
    }

    impl Forge for FakeForge {
        fn open_pr(&self, branch: &str, _: &str, _: &str) -> Result<()> {
            self.prs.borrow_mut().push(branch.to_string());
            Ok(())
        }

        fn create_release(&self, _: &str, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
    }

    fn test_pkg(root: &Path, name: &str) -> Pkg {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let changelog = dir.join("CHANGELOG.md");
        std::fs::write(
            &changelog,
            "# Changelog\n\n## [Unreleased]\n\n### Added\n- change\n",
        )
        .unwrap();
        Pkg {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            manifest_path: dir.join("manifest"),
            changelog_path: changelog,
            publishable: true,
            internal_deps: vec![],
        }
    }

    #[test]
    fn apply_bump_increments_and_resets() {
        assert_eq!(apply_bump("1.2.3", &Bump::Major).unwrap(), "2.0.0");
        assert_eq!(apply_bump("1.2.3", &Bump::Minor).unwrap(), "1.3.0");
        assert_eq!(apply_bump("1.2.3", &Bump::Patch).unwrap(), "1.2.4");
        // Test transition from pre-release to stable
        assert_eq!(apply_bump("0.1.0-next.1", &Bump::Patch).unwrap(), "0.1.1");

        // Test pre-release bumps
        assert_eq!(
            apply_bump("1.2.3", &Bump::PreMinor("beta".to_string())).unwrap(),
            "1.3.0-beta.0"
        );
        assert_eq!(
            apply_bump("1.3.0-beta.0", &Bump::Prerelease("beta".to_string())).unwrap(),
            "1.3.0-beta.1"
        );
        assert_eq!(
            apply_bump("1.3.0-beta.1", &Bump::Prerelease("beta".to_string())).unwrap(),
            "1.3.0-beta.2"
        );
        assert_eq!(
            apply_bump("1.2.3", &Bump::PrePatch("rc".to_string())).unwrap(),
            "1.2.4-rc.0"
        );

        // Test switch channel
        assert_eq!(
            apply_bump("1.3.0-alpha.1", &Bump::Prerelease("beta".to_string())).unwrap(),
            "1.3.0-beta.0"
        );

        // Test graduate
        assert_eq!(
            apply_bump("1.3.0-beta.2", &Bump::Graduate).unwrap(),
            "1.3.0"
        );
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

    #[test]
    fn orchestrate_many_versions_all_adapters_in_one_release_transaction() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let a = FakeVersionAdapter::new(test_pkg(root, "npm-lib"));
        let b = FakeVersionAdapter::new(test_pkg(root, "cargo-lib"));
        let repo = FakeRepo;
        let git = FakeGit::new();
        let forge = FakeForge {
            prs: RefCell::new(Vec::new()),
        };
        let prompt = FakePrompt;
        let hook_runner = crate::hooks::fakes::FakeHookRunner::new();
        let config = crate::config::ReleaseConfig {
            changelog_strategy: crate::config::ChangelogStrategy::Curated,
            ..Default::default()
        };

        orchestrate_many(
            &[&a, &b],
            &repo,
            &git,
            &forge,
            &prompt,
            root,
            "2026-06-28",
            &VersionOptions::default(),
            &config,
            &hook_runner,
        )
        .unwrap();

        assert_eq!(git.created.borrow().as_slice(), ["release/2026-06-28"]);
        assert_eq!(git.commits.borrow().len(), 1);
        assert_eq!(git.pushes.borrow().as_slice(), ["release/2026-06-28"]);
        assert_eq!(forge.prs.borrow().as_slice(), ["release/2026-06-28"]);
        assert_eq!(
            a.writes.borrow().as_slice(),
            [("npm-lib".to_string(), "1.0.1".to_string())]
        );
        assert_eq!(
            b.writes.borrow().as_slice(),
            [("cargo-lib".to_string(), "1.0.1".to_string())]
        );
        assert_eq!(*a.lockfile_updates.borrow(), 1);
        assert_eq!(*b.lockfile_updates.borrow(), 1);
    }

    #[test]
    fn shared_changelog_is_rewritten_exactly_once() {
        // Two packages mapping to one CHANGELOG.md (the cargo-lockstep / generic case). The file
        // must be moved into a single dated section, not rewritten once per package — a second
        // pass would operate on the freshly emptied [Unreleased] and emit a duplicate heading.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let changelog = root.join("CHANGELOG.md");
        std::fs::write(
            &changelog,
            "# Changelog\n\n## [Unreleased]\n\n### Added\n- shared change\n",
        )
        .unwrap();
        let mk = |name: &str| Pkg {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            manifest_path: root.join(name).join("manifest"),
            changelog_path: changelog.clone(),
            publishable: true,
            internal_deps: vec![],
        };
        let adapter = FakeVersionAdapter::with_packages(vec![mk("lib-a"), mk("lib-b")]);
        let git = FakeGit::new();
        let forge = FakeForge {
            prs: RefCell::new(Vec::new()),
        };
        let config = crate::config::ReleaseConfig {
            changelog_strategy: crate::config::ChangelogStrategy::Curated,
            ..Default::default()
        };

        orchestrate_many(
            &[&adapter],
            &FakeRepo,
            &git,
            &forge,
            &FakePrompt,
            root,
            "2026-06-28",
            &VersionOptions::default(),
            &config,
            &crate::hooks::fakes::FakeHookRunner::new(),
        )
        .unwrap();

        let after = std::fs::read_to_string(&changelog).unwrap();
        assert_eq!(
            after.matches("## [1.0.1] - 2026-06-28").count(),
            1,
            "shared changelog must get exactly one dated section:\n{after}"
        );
        assert_eq!(
            after.matches("## [Unreleased]").count(),
            1,
            "a single fresh [Unreleased] must remain:\n{after}"
        );
        assert!(
            after.contains("- shared change"),
            "the curated notes must survive:\n{after}"
        );
    }
}
