//! End-to-end test for the `version` command: a real temp git+npm workspace with the npm
//! adapter's command runner faked (no real npm/network), real git pushing to a local bare
//! remote, and fake forge/prompt. Verifies the release lands on `release/*` with correct
//! manifests, ranges, and changelogs — and never touches `main`.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

use opentf_release_adapters::npm::{CommandOutput, CommandRunner, NpmAdapter};
use opentf_release_core::adapter::{Bump, Pkg};
use opentf_release_core::forge::Forge;
use opentf_release_core::git::GitRepo;
use opentf_release_core::prompt::Prompt;
use opentf_release_core::version::{orchestrate, VersionOptions};

/// Every `npm` invocation "succeeds" (the version flow only calls `update_lockfile`).
struct OkRunner;
impl CommandRunner for OkRunner {
    fn run(&self, _program: &str, _args: &[&str], _cwd: &Path) -> Result<CommandOutput> {
        Ok(CommandOutput {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

struct ScriptedPrompt {
    selected: Vec<String>,
    bump: Bump,
}
impl Prompt for ScriptedPrompt {
    fn select_packages(&self, _pending: &[&Pkg]) -> Result<Vec<String>> {
        Ok(self.selected.clone())
    }
    fn choose_bump(&self, _pkg_name: &str) -> Result<Bump> {
        Ok(self.bump)
    }
    fn confirm(&self, _summary: &str) -> Result<bool> {
        Ok(true)
    }
}

struct CaptureForge {
    calls: RefCell<Vec<(String, String)>>,
}
impl Forge for CaptureForge {
    fn open_pr(&self, branch: &str, title: &str, _body: &str) -> Result<()> {
        self.calls
            .borrow_mut()
            .push((branch.to_string(), title.to_string()));
        Ok(())
    }
    fn create_release(&self, _tag: &str, _title: &str, _notes: &str) -> Result<()> {
        Ok(())
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn write(path: PathBuf, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn read(path: PathBuf) -> String {
    fs::read_to_string(path).unwrap()
}

fn capture(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Write the workspace manifests/changelogs: a selected lib (core), a peerDep dependent
/// (sdk, empty notes), and a private app.
fn write_workspace(root: &Path) {
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
        "# Changelog\n\n## [Unreleased]\n\n### Added\n- new core API\n",
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
}

/// Init the repo on `main`, commit, and tag sdk so it isn't treated as a first release.
fn init_repo(root: &Path) {
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "init"]);
    git(root, &["branch", "-M", "main"]);
    git(root, &["tag", "@x/sdk@1.0.0"]);
}

#[test]
fn version_flow_releases_on_a_branch_and_never_touches_main() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_workspace(root);
    init_repo(root);

    // A local bare remote so `git push` works offline (kept outside the workspace).
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "--bare", "-q"]);
    git(
        root,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );

    let adapter = NpmAdapter::with_runner(root, Box::new(OkRunner));
    let repo = GitRepo::new(root);
    let forge = CaptureForge {
        calls: RefCell::new(Vec::new()),
    };
    let prompt = ScriptedPrompt {
        selected: vec!["@x/core".to_string()],
        bump: Bump::Major,
    };

    orchestrate(
        &adapter,
        &repo,
        &repo,
        &forge,
        &prompt,
        root,
        "2026-06-24",
        &VersionOptions::default(),
    )
    .unwrap();

    // We are on the release branch, not main.
    assert_eq!(
        capture(root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "release/2026-06-24"
    );

    // core bumped major; sdk mirror-bumped major; its range to core updated.
    assert!(read(root.join("packages/core/package.json")).contains("\"version\": \"2.0.0\""));
    assert!(read(root.join("packages/sdk/package.json")).contains("\"version\": \"2.0.0\""));
    assert!(read(root.join("packages/sdk/package.json")).contains("\"@x/core\": \"^2.0.0\""));

    // Private app: range updated, version NOT bumped.
    let app = read(root.join("packages/app/package.json"));
    assert!(app.contains("\"@x/core\": \"^2.0.0\""));
    assert!(app.contains("\"version\": \"1.0.0\""));

    // Changelogs: core moves its notes; sdk (auto-bumped) gets the stub.
    let core_log = read(root.join("packages/core/CHANGELOG.md"));
    assert!(core_log.contains("## [2.0.0] - 2026-06-24"));
    assert!(core_log.contains("- new core API"));
    let sdk_log = read(root.join("packages/sdk/CHANGELOG.md"));
    assert!(sdk_log.contains("## [2.0.0] - 2026-06-24"));
    assert!(sdk_log.contains("_Dependency updates._"));

    // A PR was opened for the release branch.
    let calls = forge.calls.borrow();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "release/2026-06-24");

    // main is untouched: core is still 1.0.0 there.
    assert!(capture(root, &["show", "main:packages/core/package.json"]).contains("\"1.0.0\""));
}

#[test]
fn dry_run_prints_the_plan_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_workspace(root);
    init_repo(root);

    let adapter = NpmAdapter::with_runner(root, Box::new(OkRunner));
    let repo = GitRepo::new(root);
    let forge = CaptureForge {
        calls: RefCell::new(Vec::new()),
    };
    let prompt = ScriptedPrompt {
        selected: vec!["@x/core".to_string()],
        bump: Bump::Major,
    };

    orchestrate(
        &adapter,
        &repo,
        &repo,
        &forge,
        &prompt,
        root,
        "2026-06-24",
        &VersionOptions {
            dry_run: true,
            first_release: false,
        },
    )
    .unwrap();

    // Still on main, no branch created, nothing written, no PR opened.
    assert_eq!(
        capture(root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main"
    );
    assert!(read(root.join("packages/core/package.json")).contains("\"version\": \"1.0.0\""));
    assert!(read(root.join("packages/core/CHANGELOG.md")).contains("## [Unreleased]\n\n### Added"));
    assert!(capture(root, &["status", "--porcelain"]).is_empty());
    assert!(forge.calls.borrow().is_empty());
}
