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

use anyhow::{anyhow, bail, Context, Result};

use crate::adapter::{apply_changelog_layout, Adapter, Bump, DepKind, Pkg};
use crate::changelog;
use crate::date;
use crate::forge::{Forge, GhForge};
use crate::git::{GitOps, GitRepo, RepoState};
use crate::graph::Graph;
use crate::preflight;
use crate::prompt::{Candidate, Prompt, StdinPrompt};
use crate::summary::{self, Plan, RangeUpdate, VersionChange};
use crate::ui;

/// Options for a `version` run (wired up by the CLI crate).
#[derive(Debug, Clone, Default)]
pub struct VersionOptions {
    /// Compute and print the plan, but write nothing.
    pub dry_run: bool,
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
    let repo = GitRepo::new(root);
    if !opts.dry_run {
        if !repo.is_clean()? {
            bail!("working tree is not clean; commit or stash first");
        }
        let branch = repo.current_branch()?;
        if branch != config.default_branch {
            bail!(
                "must be on `{}` to start a release (currently on `{branch}`)",
                config.default_branch
            );
        }
    }
    if std::process::Command::new("gh")
        .arg("--version")
        .output()
        .is_err()
    {
        opts.skip_pr = true;
    }
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
        ui::warn("Nothing to release: no adapters are enabled.");
        return Ok(());
    }

    if !config.hooks.pre_version.is_empty() {
        hook_runner.run_hooks(root, &config.hooks.pre_version)?;
    }

    let mut adapter_packages = Vec::with_capacity(adapters.len());
    let mut seen = HashSet::new();
    for adapter in adapters {
        let mut packages = adapter.discover_packages()?;
        apply_changelog_layout(root, &config.changelog_layout(), &mut packages);
        config.apply_publish_skips(&mut packages);
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
    let tag_formats = config.tag_formats();
    let report = preflight::check_with_options(
        repo,
        &all_packages,
        &[],
        preflight::CheckOptions {
            tag_formats: tag_formats.clone(),
            ignore_paths: all_packages
                .iter()
                .filter_map(|pkg| {
                    let ignores = config.publish_ignore_paths_for(&pkg.name);
                    (!ignores.is_empty()).then(|| (pkg.name.clone(), ignores.to_vec()))
                })
                .collect(),
        },
    )?;
    // Warnings are carried into the plan rather than printed here: the review screen takes over
    // the terminal, so anything written now is gone by the time the decision is made.
    let warnings: Vec<summary::Warning> = report
        .warnings
        .iter()
        .map(|w| summary::Warning {
            package: w.package.clone(),
            message: w.message.clone(),
        })
        .collect();
    if !report.violations.is_empty() {
        bail!("{}", preflight::format_violations(&report.violations));
    }

    // 2. Pending = publishable packages that carry curated [Unreleased] notes.
    let mut empties: HashMap<&str, bool> = HashMap::new();
    let mut generated_notes: HashMap<&str, String> = HashMap::new();
    let is_generated = config.changelog_strategy == crate::config::ChangelogStrategy::Generated;

    for p in &all_packages {
        if is_generated {
            let last = repo.last_tag(&p.name, &tag_formats.history_for(&p.name))?;
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
        ui::warn("Nothing to release: no package has [Unreleased] notes.");
        return Ok(());
    }

    // Non-dry releases must start from a clean `main` before any interactive prompt. That way a
    // dirty tree fails immediately instead of after the user has selected packages and bumps.
    let starting_branch = if opts.dry_run {
        None
    } else {
        if !git.is_clean()? {
            bail!("working tree is not clean; commit or stash first");
        }
        let branch = git.current_branch()?;
        if branch != config.default_branch {
            bail!(
                "must be on `{}` to start a release (currently on `{branch}`)",
                config.default_branch
            );
        }
        Some(branch)
    };

    // 3. Prompt: group pending packages by bump type. Each candidate carries the head of its stable
    // line as well as its manifest version, so a package mid-prerelease can cut a stable release
    // from the last stable tag instead of from the in-flight prerelease.
    let candidates: Vec<Candidate> = pending
        .iter()
        .map(|pkg| {
            let stable_base = if pkg.version.contains('-') {
                repo.last_stable_version(&pkg.name, &tag_formats.history_for(&pkg.name))?
            } else {
                None
            };
            let first_release = repo
                .last_tag(&pkg.name, &tag_formats.history_for(&pkg.name))?
                .is_none();
            Ok(Candidate {
                pkg,
                stable_base,
                first_release,
            })
        })
        .collect::<Result<_>>()?;
    let stable_heads: HashMap<&str, &str> = candidates
        .iter()
        .map(|c| (c.pkg.name.as_str(), c.stable_head()))
        .collect();

    let selected = prompt.choose_bumps(&candidates)?;
    if selected.is_empty() {
        ui::info("Nothing selected.");
        return Ok(());
    }

    let by_name: HashMap<&str, &Pkg> = all_packages.iter().map(|p| (p.name.as_str(), p)).collect();
    let pending_names: HashSet<&str> = pending.iter().map(|p| p.name.as_str()).collect();

    for name in selected.keys() {
        if !pending_names.contains(name.as_str()) {
            bail!("selected package is not in the pending list: {name}");
        }
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
        let mut bumps = graph.cascade(ctx.adapter, &selected_for_adapter)?;
        // Lockstep crates share one version; reconcile their bumps so they can't diverge.
        reconcile_version_groups(&mut bumps, &ctx.adapter.version_groups()?)?;
        for (name, bump) in &bumps {
            let pkg = by_name[name.as_str()];
            let base = bump_base(pkg, bump, stable_heads.get(name.as_str()).copied());
            new_versions.insert(name.clone(), apply_bump(base, bump)?);
        }
        adapter_bumps.push(bumps);
    }

    if new_versions.is_empty() {
        ui::warn("Nothing to release: no selected package was publishable.");
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
            let new_version = new_versions[name].clone();
            let tag = tag_formats
                .tag_for(name, &new_version)
                .unwrap_or_else(|_| "<invalid tag format>".to_string());
            VersionChange {
                name: name.clone(),
                old_version: bump_base(pkg, bump, stable_heads.get(name.as_str()).copied())
                    .to_string(),
                new_version,
                selected: is_selected,
                note,
                tag,
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
                        kind: dep.kind.clone(),
                        old_range: dep.range.clone(),
                        new_range: ctx.adapter.preview_range(&dep.range, new_dep_ver),
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
        warnings,
    };
    let summary_text = summary::render(&plan);

    // 6. Dry run: print the plan and stop, writing nothing.
    if opts.dry_run {
        print!("{summary_text}");
        return Ok(());
    }

    // 7. Cut release/* from the already-validated starting branch.
    let branch = starting_branch.expect("non-dry release should validate starting branch");
    let release_branch = format!("release/{today}");
    git.create_branch(&release_branch)?;

    // 8. Apply: versions, then internal ranges, then changelogs, then lockfiles.
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
            )
            .with_context(|| {
                format!(
                    "releasing changelog for {name} at {}",
                    pkg.changelog_path.display()
                )
            })?;
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

    // 9. Final review: show the actual files and diff produced by the release edits. On cancel,
    // discard only the generated release-branch changes and return to the original branch.
    let mut titles: Vec<String> = selected
        .keys()
        .map(|n| format!("{n}@{}", new_versions[n]))
        .collect();
    titles.sort();
    let commit_title = format!("chore(release): {}", titles.join(", "));
    if !prompt.confirm(
        &plan,
        &git.diff_stat()?,
        opts.skip_pr,
        &release_branch,
        &commit_title,
    )? {
        git.reset_hard()?;
        git.checkout_branch(&branch)?;
        ui::info("Cancelled. Generated release changes were discarded.");
        return Ok(());
    }

    // 10. Commit, push, open the PR.
    git.add_all()?;
    git.commit(&commit_title)?;
    git.push_branch(&release_branch)?;
    ui::heading("Release branch ready");
    ui::detail(&format!("commit created: {commit_title}"));
    ui::detail(&format!("branch pushed: origin/{release_branch}"));

    if opts.skip_pr {
        ui::warn("PR skipped because GitHub CLI is unavailable.");
        ui::detail(&format!(
            "open a PR for `{release_branch}` on GitHub by hand"
        ));
    } else {
        forge.open_pr(&release_branch, &commit_title, &summary_text)?;
        ui::ok(&format!("PR opened from `{release_branch}`."));
    }
    if prompt.confirm_post_release_cleanup(&release_branch)? {
        git.return_to_default_branch(&branch)?;
        git.delete_local_branch(&release_branch)?;
        ui::ok(&format!(
            "Returned to `{branch}` and deleted local branch `{release_branch}`."
        ));
    } else {
        ui::heading("Post-release cleanup");
        for step in post_release_steps(&release_branch) {
            ui::detail(&step);
        }
        ui::detail("deletes only the local release branch, after the pushed PR branch exists");
    }

    Ok(())
}

/// The commands to run after the release PR merges, one per line so the caller can render them as
/// indented detail rather than embedding layout in a string.
fn post_release_steps(release_branch: &str) -> Vec<String> {
    vec![
        "git switch main".to_string(),
        "git pull --tags".to_string(),
        format!("git branch -D {release_branch}"),
    ]
}

/// Raise every bumped member of each lockstep group to the strongest bump in its group, so
/// packages versioned together (cargo `version.workspace = true`) resolve to one deterministic
/// version instead of diverging by the order their manifests were written. "Strongest" is a
/// prerelease-aware [`Bump::merge`], not a raw enum `max`, so a member bumped into a prerelease
/// channel drags the whole group into that channel rather than being masked by a stable sibling.
/// Members that received no bump are left untouched — the adapter still moves them on disk via the
/// shared version, and `publish` ships them from the bumped working tree. Errors if two members
/// were pushed into conflicting prerelease channels.
fn reconcile_version_groups(
    bumps: &mut HashMap<String, Bump>,
    groups: &[Vec<String>],
) -> Result<()> {
    for group in groups {
        let mut strongest: Option<Bump> = None;
        for bump in group.iter().filter_map(|name| bumps.get(name)) {
            strongest = Some(match strongest {
                None => bump.clone(),
                Some(acc) => acc.merge(bump).with_context(|| {
                    format!("reconciling lockstep group [{}]", group.join(", "))
                })?,
            });
        }
        let Some(strongest) = strongest else {
            continue; // no member of this group was bumped
        };
        for name in group {
            if let Some(slot) = bumps.get_mut(name) {
                *slot = strongest.clone();
            }
        }
    }
    Ok(())
}

/// Apply a bump to an `x.y.z` version (pre-release/build metadata is dropped unless entering/iterating prerelease).
/// The version a bump is computed from.
///
/// The stable and prerelease lines advance independently, and only one of them fits in the
/// manifest. A stable bump on a package mid-prerelease therefore reads the stable head from tags —
/// `0.13.0` + Minor is `0.14.0`, not the `1.1.0` the in-flight `1.0.0-beta.3` would produce.
/// Everything else keeps reading the manifest: prerelease bumps advance the line the manifest
/// holds, and a stable package's manifest may legitimately run ahead of its last tag.
fn bump_base<'a>(pkg: &'a Pkg, bump: &Bump, stable_head: Option<&'a str>) -> &'a str {
    match bump {
        Bump::Major | Bump::Minor | Bump::Patch if pkg.version.contains('-') => {
            stable_head.unwrap_or(&pkg.version)
        }
        _ => &pkg.version,
    }
}

pub(crate) fn apply_bump(version: &str, bump: &Bump) -> Result<String> {
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
        // Validated by the parse above, then returned untouched: the point is to ship the version
        // the manifest already carries.
        Bump::Initial => Ok(version.to_string()),
        Bump::Graduate => {
            if pre.is_none() {
                bail!("Cannot graduate a stable version: {version}. Select Major/Minor/Patch instead.");
            }
            Ok(format!("{major}.{minor}.{patch}"))
        }
        Bump::Prerelease(ch) => {
            if let Some(p) = pre {
                // Match the exact first dot-separated identifier, not a prefix: iterating channel
                // `rc` against an existing `rc2.1` (or `beta` against `beta2`) must start a fresh
                // `rc.0`, not treat `rc2` as the same channel and bump it to `rc2.2`.
                let mut p_parts = p.split('.');
                let existing_channel = p_parts.next().unwrap_or(p);
                if existing_channel == ch {
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
        Bump::Initial => "initial",
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
        groups: Vec<Vec<String>>,
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
                groups: Vec::new(),
                writes: RefCell::new(Vec::new()),
                lockfile_updates: RefCell::new(0),
            }
        }

        fn with_lockstep_group(mut self, names: &[&str]) -> Self {
            self.groups = vec![names.iter().map(|n| n.to_string()).collect()];
            self
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

        fn version_groups(&self) -> Result<Vec<Vec<String>>> {
            Ok(self.groups.clone())
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
        fn last_tag(&self, pkg_name: &str, _: &[String]) -> Result<Option<String>> {
            Ok(Some(format!("{pkg_name}@1.0.0")))
        }

        fn commit_count_since(&self, _: &str, _: &Path) -> Result<usize> {
            Ok(0)
        }

        fn changed_files_since(&self, _: &str, _: &Path) -> Result<Vec<std::path::PathBuf>> {
            Ok(Vec::new())
        }

        fn commits_since(&self, _: Option<&str>, _: &Path) -> Result<String> {
            Ok(String::new())
        }
    }

    struct FakeGit {
        branch: RefCell<String>,
        clean: bool,
        created: RefCell<Vec<String>>,
        commits: RefCell<Vec<String>>,
        pushes: RefCell<Vec<String>>,
        deleted: RefCell<Vec<String>>,
    }

    impl FakeGit {
        fn new() -> Self {
            Self {
                branch: RefCell::new("main".to_string()),
                clean: true,
                created: RefCell::new(Vec::new()),
                commits: RefCell::new(Vec::new()),
                pushes: RefCell::new(Vec::new()),
                deleted: RefCell::new(Vec::new()),
            }
        }

        fn dirty() -> Self {
            Self {
                clean: false,
                ..Self::new()
            }
        }
    }

    impl GitOps for FakeGit {
        fn is_clean(&self) -> Result<bool> {
            Ok(self.clean)
        }

        fn current_branch(&self) -> Result<String> {
            Ok(self.branch.borrow().clone())
        }

        fn create_branch(&self, name: &str) -> Result<()> {
            self.created.borrow_mut().push(name.to_string());
            *self.branch.borrow_mut() = name.to_string();
            Ok(())
        }

        fn checkout_branch(&self, name: &str) -> Result<()> {
            *self.branch.borrow_mut() = name.to_string();
            Ok(())
        }

        fn diff_stat(&self) -> Result<String> {
            Ok(" CHANGELOG.md | 2 ++\n 1 file changed, 2 insertions(+)\n".to_string())
        }

        fn reset_hard(&self) -> Result<()> {
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

        fn tag_exists(&self, _: &str) -> Result<bool> {
            Ok(false)
        }

        fn return_to_default_branch(&self, branch: &str) -> Result<()> {
            *self.branch.borrow_mut() = branch.to_string();
            Ok(())
        }

        fn delete_local_branch(&self, name: &str) -> Result<()> {
            self.deleted.borrow_mut().push(name.to_string());
            Ok(())
        }
    }

    struct FakePrompt;

    impl Prompt for FakePrompt {
        fn choose_bumps(&self, pending: &[Candidate]) -> Result<HashMap<String, Bump>> {
            Ok(pending
                .iter()
                .map(|c| (c.pkg.name.clone(), Bump::Patch))
                .collect())
        }

        fn confirm(
            &self,
            _: &crate::summary::Plan,
            _: &str,
            _: bool,
            _: &str,
            _: &str,
        ) -> Result<bool> {
            Ok(true)
        }

        fn confirm_post_release_cleanup(&self, _: &str) -> Result<bool> {
            Ok(true)
        }
    }

    /// Selects every pending package and returns a per-package bump from a scripted map.
    struct ScriptedBumpPrompt {
        bumps: HashMap<String, Bump>,
    }

    impl Prompt for ScriptedBumpPrompt {
        fn choose_bumps(&self, _: &[Candidate]) -> Result<HashMap<String, Bump>> {
            Ok(self.bumps.clone())
        }

        fn confirm(
            &self,
            _: &crate::summary::Plan,
            _: &str,
            _: bool,
            _: &str,
            _: &str,
        ) -> Result<bool> {
            Ok(true)
        }

        fn confirm_post_release_cleanup(&self, _: &str) -> Result<bool> {
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

        fn release_exists(&self, _: &str) -> Result<bool> {
            Ok(false)
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

    fn pkg_at(version: &str) -> Pkg {
        Pkg {
            name: "@scope/a".to_string(),
            version: version.to_string(),
            manifest_path: std::path::PathBuf::from("manifest"),
            changelog_path: std::path::PathBuf::from("CHANGELOG.md"),
            publishable: true,
            internal_deps: vec![],
        }
    }

    #[test]
    fn stable_bumps_on_a_prerelease_package_advance_the_stable_line() {
        let pkg = pkg_at("1.0.0-beta.3");
        // The in-flight beta must not decide the number: 0.13.0 + Minor is 0.14.0, not 1.1.0.
        assert_eq!(bump_base(&pkg, &Bump::Minor, Some("0.13.0")), "0.13.0");
        assert_eq!(
            apply_bump(bump_base(&pkg, &Bump::Minor, Some("0.13.0")), &Bump::Minor).unwrap(),
            "0.14.0"
        );
        assert_eq!(
            apply_bump(bump_base(&pkg, &Bump::Patch, Some("0.13.0")), &Bump::Patch).unwrap(),
            "0.13.1"
        );
    }

    #[test]
    fn prerelease_bumps_stay_on_the_manifest_line() {
        let pkg = pkg_at("1.0.0-beta.3");
        for bump in [
            Bump::Graduate,
            Bump::Prerelease("beta".to_string()),
            Bump::PreMinor("rc".to_string()),
        ] {
            assert_eq!(bump_base(&pkg, &bump, Some("0.13.0")), "1.0.0-beta.3");
        }
        assert_eq!(
            apply_bump(
                bump_base(&pkg, &Bump::Prerelease("beta".to_string()), Some("0.13.0")),
                &Bump::Prerelease("beta".to_string())
            )
            .unwrap(),
            "1.0.0-beta.4"
        );
        assert_eq!(
            apply_bump(
                bump_base(&pkg, &Bump::Graduate, Some("0.13.0")),
                &Bump::Graduate
            )
            .unwrap(),
            "1.0.0"
        );
    }

    #[test]
    fn a_stable_package_keeps_reading_its_manifest() {
        // Its manifest may legitimately run ahead of the last tag, so the tag must not win.
        let pkg = pkg_at("1.2.0");
        assert_eq!(bump_base(&pkg, &Bump::Minor, Some("1.1.0")), "1.2.0");
    }

    #[test]
    fn a_prerelease_package_without_a_stable_tag_falls_back_to_the_manifest() {
        let pkg = pkg_at("1.0.0-beta.3");
        assert_eq!(bump_base(&pkg, &Bump::Minor, None), "1.0.0-beta.3");
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

        // Channel match is on the exact first identifier, not a prefix: `rc` against `rc2.1` is a
        // different channel and must reset to `rc.0`, never bump `rc2` to `rc2.2`.
        assert_eq!(
            apply_bump("1.3.0-rc2.1", &Bump::Prerelease("rc".to_string())).unwrap(),
            "1.3.0-rc.0"
        );
        assert_eq!(
            apply_bump("1.3.0-beta2.0", &Bump::Prerelease("beta".to_string())).unwrap(),
            "1.3.0-beta.0"
        );
        // The reverse still iterates the true `rc2` channel.
        assert_eq!(
            apply_bump("1.3.0-rc2.1", &Bump::Prerelease("rc2".to_string())).unwrap(),
            "1.3.0-rc2.2"
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
    fn non_dry_release_checks_clean_tree_before_prompting() {
        struct PanicPrompt;

        impl Prompt for PanicPrompt {
            fn choose_bumps(&self, _: &[Candidate]) -> Result<HashMap<String, Bump>> {
                panic!("prompt should not be reached when the working tree is dirty");
            }

            fn confirm(
                &self,
                _: &crate::summary::Plan,
                _: &str,
                _: bool,
                _: &str,
                _: &str,
            ) -> Result<bool> {
                panic!("prompt should not be reached when the working tree is dirty");
            }

            fn confirm_post_release_cleanup(&self, _: &str) -> Result<bool> {
                panic!("prompt should not be reached when the working tree is dirty");
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let adapter = FakeVersionAdapter::new(test_pkg(root, "npm-lib"));
        let err = orchestrate_many(
            &[&adapter],
            &FakeRepo,
            &FakeGit::dirty(),
            &FakeForge {
                prs: RefCell::new(Vec::new()),
            },
            &PanicPrompt,
            root,
            "2026-06-28",
            &VersionOptions::default(),
            &crate::config::ReleaseConfig::default(),
            &crate::hooks::fakes::FakeHookRunner::new(),
        )
        .unwrap_err()
        .to_string();

        assert_eq!(err, "working tree is not clean; commit or stash first");
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
            otf_release_version: None,
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
        assert_eq!(git.current_branch().unwrap(), "main");
        assert_eq!(git.deleted.borrow().as_slice(), ["release/2026-06-28"]);
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
    fn release_cuts_from_and_returns_to_the_configured_default_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let adapter = FakeVersionAdapter::new(test_pkg(root, "npm-lib"));
        let git = FakeGit::new();
        *git.branch.borrow_mut() = "trunk".to_string();
        let forge = FakeForge {
            prs: RefCell::new(Vec::new()),
        };
        let config = crate::config::ReleaseConfig {
            otf_release_version: None,
            default_branch: "trunk".to_string(),
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

        assert_eq!(git.created.borrow().as_slice(), ["release/2026-06-28"]);
        // Cleanup returns to the configured branch, not a hardcoded `main`.
        assert_eq!(git.current_branch().unwrap(), "trunk");
    }

    #[test]
    fn release_rejects_starting_from_the_wrong_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let adapter = FakeVersionAdapter::new(test_pkg(root, "npm-lib"));
        let git = FakeGit::new(); // on `main`
        let config = crate::config::ReleaseConfig {
            otf_release_version: None,
            default_branch: "master".to_string(),
            changelog_strategy: crate::config::ChangelogStrategy::Curated,
            ..Default::default()
        };

        let err = orchestrate_many(
            &[&adapter],
            &FakeRepo,
            &git,
            &FakeForge {
                prs: RefCell::new(Vec::new()),
            },
            &FakePrompt,
            root,
            "2026-06-28",
            &VersionOptions::default(),
            &config,
            &crate::hooks::fakes::FakeHookRunner::new(),
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("must be on `master`"), "got: {err}");
    }

    #[test]
    fn post_release_next_steps_return_to_main_and_delete_local_branch() {
        let steps = post_release_steps("release/2026-06-28");
        assert_eq!(
            steps,
            vec![
                "git switch main".to_string(),
                "git pull --tags".to_string(),
                "git branch -D release/2026-06-28".to_string(),
            ]
        );
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
            otf_release_version: None,
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

    #[test]
    fn reconcile_version_groups_raises_bumped_members_to_max() {
        let mut bumps = HashMap::from([
            ("a".to_string(), Bump::Minor),
            ("b".to_string(), Bump::Major),
            ("loner".to_string(), Bump::Patch),
        ]);
        let groups = vec![
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            vec!["loner".to_string()],
        ];
        reconcile_version_groups(&mut bumps, &groups).unwrap();

        // a and b were both bumped → both raised to the group max (major).
        assert_eq!(bumps["a"], Bump::Major);
        assert_eq!(bumps["b"], Bump::Major);
        // c got no bump and stays absent (the adapter still moves it via the shared version).
        assert!(!bumps.contains_key("c"));
        // A singleton group is unaffected.
        assert_eq!(bumps["loner"], Bump::Patch);
    }

    #[test]
    fn reconcile_version_groups_prerelease_member_pulls_the_group_into_its_channel() {
        // One member is stable Patch, another entered a beta prerelease. The group must move
        // together into the prerelease — a stable sibling must not mask the prerelease intent.
        let mut bumps = HashMap::from([
            ("a".to_string(), Bump::Patch),
            ("b".to_string(), Bump::PreMinor("beta".to_string())),
        ]);
        let groups = vec![vec!["a".to_string(), "b".to_string()]];
        reconcile_version_groups(&mut bumps, &groups).unwrap();
        assert_eq!(bumps["a"], Bump::PreMinor("beta".to_string()));
        assert_eq!(bumps["b"], Bump::PreMinor("beta".to_string()));
    }

    #[test]
    fn reconcile_version_groups_errors_on_conflicting_prerelease_channels() {
        let mut bumps = HashMap::from([
            ("a".to_string(), Bump::PreMinor("beta".to_string())),
            ("b".to_string(), Bump::PreMinor("rc".to_string())),
        ]);
        let groups = vec![vec!["a".to_string(), "b".to_string()]];
        assert!(reconcile_version_groups(&mut bumps, &groups).is_err());
    }

    #[test]
    fn lockstep_group_selected_with_different_bumps_resolves_to_one_version() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Two lockstep crates at the same shared version, picked with *different* bumps.
        let adapter = FakeVersionAdapter::with_packages(vec![
            test_pkg(root, "crate-a"),
            test_pkg(root, "crate-b"),
        ])
        .with_lockstep_group(&["crate-a", "crate-b"]);
        let git = FakeGit::new();
        let forge = FakeForge {
            prs: RefCell::new(Vec::new()),
        };
        let prompt = ScriptedBumpPrompt {
            bumps: HashMap::from([
                ("crate-a".to_string(), Bump::Major),
                ("crate-b".to_string(), Bump::Minor),
            ]),
        };
        let config = crate::config::ReleaseConfig {
            otf_release_version: None,
            changelog_strategy: crate::config::ChangelogStrategy::Curated,
            ..Default::default()
        };

        orchestrate_many(
            &[&adapter],
            &FakeRepo,
            &git,
            &forge,
            &prompt,
            root,
            "2026-06-28",
            &VersionOptions::default(),
            &config,
            &crate::hooks::fakes::FakeHookRunner::new(),
        )
        .unwrap();

        // Both crates are written to the same version — the strongest (major) bump wins — instead
        // of diverging (2.0.0 vs 1.1.0) by write order.
        let mut writes = adapter.writes.borrow().clone();
        writes.sort();
        assert_eq!(
            writes,
            [
                ("crate-a".to_string(), "2.0.0".to_string()),
                ("crate-b".to_string(), "2.0.0".to_string()),
            ]
        );
    }

    /// A repo whose package shipped 0.13.0 on the stable line and is now mid-1.0 beta.
    struct TwoLineRepo;

    impl RepoState for TwoLineRepo {
        fn last_tag(&self, pkg_name: &str, _: &[String]) -> Result<Option<String>> {
            Ok(Some(format!("{pkg_name}@1.0.0-beta.3")))
        }

        fn last_stable_version(&self, _: &str, _: &[String]) -> Result<Option<String>> {
            Ok(Some("0.13.0".to_string()))
        }

        fn commit_count_since(&self, _: &str, _: &Path) -> Result<usize> {
            Ok(0)
        }

        fn changed_files_since(&self, _: &str, _: &Path) -> Result<Vec<std::path::PathBuf>> {
            Ok(Vec::new())
        }

        fn commits_since(&self, _: Option<&str>, _: &Path) -> Result<String> {
            Ok(String::new())
        }
    }

    fn run_two_line_release(bump: Bump) -> Vec<(String, String)> {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut pkg = test_pkg(root, "std");
        pkg.version = "1.0.0-beta.3".to_string();
        let adapter = FakeVersionAdapter::with_packages(vec![pkg]);
        let git = FakeGit::new();
        let forge = FakeForge {
            prs: RefCell::new(Vec::new()),
        };
        let prompt = ScriptedBumpPrompt {
            bumps: HashMap::from([("std".to_string(), bump)]),
        };
        let config = crate::config::ReleaseConfig {
            otf_release_version: None,
            changelog_strategy: crate::config::ChangelogStrategy::Curated,
            ..Default::default()
        };

        orchestrate_many(
            &[&adapter],
            &TwoLineRepo,
            &git,
            &forge,
            &prompt,
            root,
            "2026-07-25",
            &VersionOptions::default(),
            &config,
            &crate::hooks::fakes::FakeHookRunner::new(),
        )
        .unwrap();

        let writes = adapter.writes.borrow().clone();
        writes
    }

    #[test]
    fn a_minor_on_a_beta_package_cuts_from_the_stable_tag() {
        // 0.13.0 + minor, not the 1.1.0 the in-flight 1.0.0-beta.3 would have produced.
        assert_eq!(
            run_two_line_release(Bump::Minor),
            [("std".to_string(), "0.14.0".to_string())]
        );
    }

    #[test]
    fn graduating_a_beta_package_still_uses_its_prerelease() {
        // The other line is untouched by the stable head: beta.3 graduates to 1.0.0.
        assert_eq!(
            run_two_line_release(Bump::Graduate),
            [("std".to_string(), "1.0.0".to_string())]
        );
        assert_eq!(
            run_two_line_release(Bump::Prerelease("beta".to_string())),
            [("std".to_string(), "1.0.0-beta.4".to_string())]
        );
    }
}
