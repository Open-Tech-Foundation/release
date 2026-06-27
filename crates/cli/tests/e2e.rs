//! Capstone end-to-end test: run `version` to produce a release on a branch, then (simulating
//! the merge) run `publish` against that same working tree. Uses the real npm adapter with a
//! command runner modelling the registry, real git pushing tags to a local bare remote, and a
//! fake forge. Proves the whole pipeline composes: bumps computed by `version` are exactly what
//! `publish` ships, in dependency order, with private apps excluded.

use std::cell::RefCell;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use otf_release_adapters::npm::{CommandOutput, CommandRunner, NpmAdapter};
use otf_release_core::adapter::{Bump, Pkg};
use otf_release_core::forge::Forge;
use otf_release_core::git::GitRepo;
use otf_release_core::prompt::Prompt;
use otf_release_core::publish::{self, PublishOptions};
use otf_release_core::version::{self, VersionOptions};

/// Models the npm registry: `view` reports published specs, `publish` records them, everything
/// else (e.g. `install --package-lock-only`) just succeeds.
#[derive(Clone)]
struct Registry {
    published: Arc<Mutex<HashSet<String>>>,
    log: Arc<Mutex<Vec<String>>>,
}
impl Registry {
    fn new() -> Self {
        Self {
            published: Arc::new(Mutex::new(HashSet::new())),
            log: Arc::new(Mutex::new(Vec::new())),
        }
    }
}
impl CommandRunner for Registry {
    fn run(&self, _program: &str, args: &[&str], cwd: &Path) -> Result<CommandOutput> {
        let ok = |s: &str| CommandOutput {
            success: true,
            stdout: s.to_string(),
            stderr: String::new(),
        };
        match args.first().copied() {
            Some("view") => {
                if self.published.lock().unwrap().contains(args[1]) {
                    Ok(ok("1.0.0\n"))
                } else {
                    Ok(CommandOutput {
                        success: false,
                        stdout: String::new(),
                        stderr: "npm error code E404".into(),
                    })
                }
            }
            Some("publish") => {
                let manifest: serde_json::Value =
                    serde_json::from_str(&fs::read_to_string(cwd.join("package.json")).unwrap())
                        .unwrap();
                let spec = format!(
                    "{}@{}",
                    manifest["name"].as_str().unwrap(),
                    manifest["version"].as_str().unwrap()
                );
                self.published.lock().unwrap().insert(spec.clone());
                self.log.lock().unwrap().push(spec);
                Ok(ok(""))
            }
            _ => Ok(ok("")),
        }
    }
}

struct ScriptedPrompt;
impl Prompt for ScriptedPrompt {
    fn select_packages(&self, _pending: &[&Pkg]) -> Result<Vec<String>> {
        Ok(vec!["@x/core".to_string()])
    }
    fn choose_bump(&self, _pkg_name: &str, _current_version: &str) -> Result<Bump> {
        Ok(Bump::Major)
    }
    fn confirm(&self, _summary: &str) -> Result<bool> {
        Ok(true)
    }
}

#[derive(Default)]
struct CapForge {
    releases: RefCell<Vec<String>>,
}
impl Forge for CapForge {
    fn open_pr(&self, _branch: &str, _title: &str, _body: &str) -> Result<()> {
        Ok(())
    }
    fn create_release(&self, tag: &str, _title: &str, _notes: &str) -> Result<()> {
        self.releases.borrow_mut().push(tag.to_string());
        Ok(())
    }
}

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

fn write(path: PathBuf, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

#[test]
fn version_then_publish_ships_exactly_the_computed_bumps() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write(
        root.join("package.json"),
        r#"{ "name": "root", "private": true, "workspaces": ["packages/*"] }"#,
    );
    write(
        root.join("packages/core/package.json"),
        "{\n  \"name\": \"@x/core\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    write(
        root.join("packages/core/CHANGELOG.md"),
        "# Changelog\n\n## [Unreleased]\n\n### Added\n- core change\n",
    );
    write(
        root.join("packages/sdk/package.json"),
        "{\n  \"name\": \"@x/sdk\",\n  \"version\": \"1.0.0\",\n  \"peerDependencies\": { \"@x/core\": \"^1.0.0\" }\n}\n",
    );
    write(
        root.join("packages/sdk/CHANGELOG.md"),
        "# Changelog\n\n## [Unreleased]\n\n## [1.0.0] - 2024-01-01\n- init\n",
    );
    write(
        root.join("packages/app/package.json"),
        "{\n  \"name\": \"@x/app\",\n  \"version\": \"1.0.0\",\n  \"private\": true,\n  \"dependencies\": { \"@x/core\": \"^1.0.0\" }\n}\n",
    );

    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "init"]);
    git(root, &["branch", "-M", "main"]);
    git(root, &["tag", "@x/sdk@1.0.0"]);

    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "--bare", "-q"]);
    git(
        root,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );

    let registry = Registry::new();
    let adapter = NpmAdapter::with_runner(root, Box::new(registry.clone()));
    let repo = GitRepo::new(root);
    let forge = CapForge::default();
    let prompt = ScriptedPrompt;
    let today = "2026-06-24";

    // 1. version: cut the release branch with bumps (core major -> sdk mirror major).
    let hooks = otf_release_core::config::Hooks::default();
    let hook_runner = otf_release_core::hooks::fakes::FakeHookRunner::new();
    version::orchestrate(
        &adapter,
        &repo,
        &repo,
        &forge,
        &prompt,
        root,
        today,
        &VersionOptions::default(),
        &hooks,
        &hook_runner,
    )
    .unwrap();

    // 2. (merge simulated by staying on the branch) publish reads the bumped working tree.
    publish::orchestrate(&adapter, &repo, &forge, &root, &PublishOptions::default(), &hooks, &hook_runner).unwrap();

    let published = registry.published.lock().unwrap();
    assert!(published.contains("@x/core@2.0.0"), "{published:?}");
    assert!(published.contains("@x/sdk@2.0.0"), "{published:?}");
    assert!(
        !published.iter().any(|s| s.starts_with("@x/app@")),
        "private app must never be published: {published:?}"
    );

    // Dependency order: core before sdk.
    let log = registry.log.lock().unwrap();
    let pos = |name: &str| log.iter().position(|s| s.starts_with(name)).unwrap();
    assert!(pos("@x/core@") < pos("@x/sdk@"));

    // A GitHub Release was created for each shipped package (from its dated changelog section).
    assert_eq!(forge.releases.borrow().len(), 2);
}
