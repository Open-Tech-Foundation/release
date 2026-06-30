//! End-to-end tests for the `publish` command: a temp npm workspace with the npm adapter's
//! command runner programmed to model the registry (idempotent `npm view`, per-package
//! publish success/failure), plus fake git/forge capturing tags and releases. Verifies
//! topological order, idempotency, and halt-on-failure / forward-resume.

use std::cell::RefCell;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;

use otf_release_adapters::npm::{CommandOutput, CommandRunner, NpmAdapter};
use otf_release_core::adapter::{Adapter, Bump, DepKind, Pkg};
use otf_release_core::forge::Forge;
use otf_release_core::git::GitOps;
use otf_release_core::publish::{orchestrate, orchestrate_many, PublishOptions};

/// A command runner that models the npm registry across runs.
#[derive(Clone)]
struct PubRunner {
    published: Arc<Mutex<HashSet<String>>>, // specs known to the registry
    fail_once: Arc<Mutex<HashSet<String>>>, // pkg dir names whose first publish fails
    publish_log: Arc<Mutex<Vec<String>>>,   // specs successfully published, in order
    attempts: Arc<Mutex<Vec<String>>>,      // pkg dir names publish was attempted for
}

impl PubRunner {
    fn new(fail_once: &[&str]) -> Self {
        Self {
            published: Arc::new(Mutex::new(HashSet::new())),
            fail_once: Arc::new(Mutex::new(
                fail_once.iter().map(|s| s.to_string()).collect(),
            )),
            publish_log: Arc::new(Mutex::new(Vec::new())),
            attempts: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

fn ok(stdout: &str) -> CommandOutput {
    CommandOutput {
        success: true,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

fn err(stderr: &str) -> CommandOutput {
    CommandOutput {
        success: false,
        stdout: String::new(),
        stderr: stderr.to_string(),
    }
}

impl CommandRunner for PubRunner {
    fn run(&self, _program: &str, args: &[&str], cwd: &Path) -> Result<CommandOutput> {
        match args.first().copied() {
            Some("view") => {
                let spec = args[1];
                if self.published.lock().unwrap().contains(spec) {
                    Ok(ok("1.0.0\n"))
                } else {
                    Ok(err("npm error code E404"))
                }
            }
            Some("publish") => {
                let dir = cwd.file_name().unwrap().to_str().unwrap().to_string();
                self.attempts.lock().unwrap().push(dir.clone());
                if self.fail_once.lock().unwrap().remove(&dir) {
                    return Ok(err("publish boom"));
                }
                let manifest: serde_json::Value =
                    serde_json::from_str(&fs::read_to_string(cwd.join("package.json")).unwrap())
                        .unwrap();
                let spec = format!(
                    "{}@{}",
                    manifest["name"].as_str().unwrap(),
                    manifest["version"].as_str().unwrap()
                );
                self.published.lock().unwrap().insert(spec.clone());
                self.publish_log.lock().unwrap().push(spec);
                Ok(ok(""))
            }
            _ => Ok(ok("")),
        }
    }
}

#[derive(Default)]
struct FakeGit {
    tags: RefCell<Vec<String>>,
    /// When set, the next `create_tag` call fails (models a tagging step that died right after a
    /// successful registry publish).
    fail_create_tag_once: RefCell<bool>,
}
impl GitOps for FakeGit {
    fn is_clean(&self) -> Result<bool> {
        Ok(true)
    }
    fn current_branch(&self) -> Result<String> {
        Ok("main".to_string())
    }
    fn create_branch(&self, _name: &str) -> Result<()> {
        Ok(())
    }
    fn checkout_branch(&self, _name: &str) -> Result<()> {
        Ok(())
    }
    fn diff_stat(&self) -> Result<String> {
        Ok(String::new())
    }
    fn reset_hard(&self) -> Result<()> {
        Ok(())
    }
    fn add_all(&self) -> Result<()> {
        Ok(())
    }
    fn commit(&self, _message: &str) -> Result<()> {
        Ok(())
    }
    fn push_branch(&self, _name: &str) -> Result<()> {
        Ok(())
    }
    fn create_tag(&self, name: &str) -> Result<()> {
        if std::mem::take(&mut *self.fail_create_tag_once.borrow_mut()) {
            anyhow::bail!("create_tag boom");
        }
        self.tags.borrow_mut().push(name.to_string());
        Ok(())
    }
    fn push_tag(&self, _name: &str) -> Result<()> {
        Ok(())
    }
    fn tag_exists(&self, name: &str) -> Result<bool> {
        Ok(self.tags.borrow().iter().any(|t| t == name))
    }
}

#[derive(Default)]
struct FakeForge {
    releases: RefCell<Vec<String>>,
}

fn package_tag_options() -> PublishOptions {
    PublishOptions {
        tag_format: "{name}@{version}".to_string(),
        ..PublishOptions::default()
    }
}
impl Forge for FakeForge {
    fn open_pr(&self, _branch: &str, _title: &str, _body: &str) -> Result<()> {
        Ok(())
    }
    fn create_release(&self, tag: &str, _title: &str, _notes: &str) -> Result<()> {
        self.releases.borrow_mut().push(tag.to_string());
        Ok(())
    }
    fn release_exists(&self, tag: &str) -> Result<bool> {
        Ok(self.releases.borrow().iter().any(|t| t == tag))
    }
}

struct FakeAdapter {
    pkg: Pkg,
    published: RefCell<Vec<String>>,
}

impl FakeAdapter {
    fn new(root: &Path, name: &str) -> Self {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        let changelog_path = dir.join("CHANGELOG.md");
        fs::write(
            &changelog_path,
            "# Changelog\n\n## [Unreleased]\n\n## [1.0.0] - 2024-01-01\n- notes\n",
        )
        .unwrap();
        Self {
            pkg: Pkg {
                name: name.to_string(),
                version: "1.0.0".to_string(),
                manifest_path: dir.join("manifest"),
                changelog_path,
                publishable: true,
                internal_deps: vec![],
            },
            published: RefCell::new(Vec::new()),
        }
    }
}

impl Adapter for FakeAdapter {
    fn discover_packages(&self) -> Result<Vec<Pkg>> {
        Ok(vec![self.pkg.clone()])
    }

    fn write_version(&self, _: &Pkg, _: &str) -> Result<()> {
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
        Ok(())
    }

    fn dependent_bump(&self, _: Bump, _: &DepKind) -> Bump {
        Bump::Patch
    }

    fn is_published(&self, _: &Pkg, _: &str) -> Result<bool> {
        Ok(false)
    }

    fn publish(&self, pkg: &Pkg, _: Option<&Path>) -> Result<()> {
        self.published
            .borrow_mut()
            .push(format!("{}@{}", pkg.name, pkg.version));
        Ok(())
    }
}

fn write(path: PathBuf, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// Workspace chain: core <- sdk <- mid (each publishable, version 1.0.0, with a dated section).
fn write_chain(root: &Path) {
    write(
        root.join("package.json"),
        r#"{ "name": "root", "private": true, "workspaces": ["packages/*"] }"#,
    );
    for (dir, name, dep) in [
        ("core", "@x/core", None),
        ("sdk", "@x/sdk", Some("@x/core")),
        ("mid", "@x/mid", Some("@x/sdk")),
    ] {
        let deps = dep
            .map(|d| format!(",\n  \"dependencies\": {{ \"{d}\": \"^1.0.0\" }}"))
            .unwrap_or_default();
        write(
            root.join(format!("packages/{dir}/package.json")),
            &format!("{{\n  \"name\": \"{name}\",\n  \"version\": \"1.0.0\"{deps}\n}}\n"),
        );
        write(
            root.join(format!("packages/{dir}/CHANGELOG.md")),
            "# Changelog\n\n## [Unreleased]\n\n## [1.0.0] - 2024-01-01\n- notes\n",
        );
    }
}

#[test]
fn publishes_in_topo_order_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_chain(root);

    let runner = PubRunner::new(&[]);
    let adapter = NpmAdapter::with_runner(root, Box::new(runner.clone()));
    let git = FakeGit::default();
    let forge = FakeForge::default();

    let hooks = otf_release_core::config::Hooks::default();
    let hook_runner = otf_release_core::hooks::fakes::FakeHookRunner::new();
    orchestrate(
        &adapter,
        &git,
        &forge,
        root,
        &package_tag_options(),
        &hooks,
        &hook_runner,
    )
    .unwrap();

    assert_eq!(
        *runner.publish_log.lock().unwrap(),
        vec!["@x/core@1.0.0", "@x/sdk@1.0.0", "@x/mid@1.0.0"]
    );
    assert_eq!(git.tags.borrow().len(), 3);
    assert_eq!(forge.releases.borrow().len(), 3);

    // Second run publishes nothing (everything is already at its published version).
    let published_before = runner.publish_log.lock().unwrap().len();
    let tags_before = git.tags.borrow().len();
    let hooks = otf_release_core::config::Hooks::default();
    let hook_runner = otf_release_core::hooks::fakes::FakeHookRunner::new();
    orchestrate(
        &adapter,
        &git,
        &forge,
        root,
        &package_tag_options(),
        &hooks,
        &hook_runner,
    )
    .unwrap();
    assert_eq!(runner.publish_log.lock().unwrap().len(), published_before);
    assert_eq!(git.tags.borrow().len(), tags_before);
}

#[test]
fn halts_on_failure_and_resumes_forward() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_chain(root);

    let runner = PubRunner::new(&["sdk"]); // sdk's first publish fails
    let adapter = NpmAdapter::with_runner(root, Box::new(runner.clone()));
    let git = FakeGit::default();
    let forge = FakeForge::default();

    // First run halts at sdk; mid (its dependent) is never attempted.
    let hooks = otf_release_core::config::Hooks::default();
    let hook_runner = otf_release_core::hooks::fakes::FakeHookRunner::new();
    let err = orchestrate(
        &adapter,
        &git,
        &forge,
        root,
        &package_tag_options(),
        &hooks,
        &hook_runner,
    )
    .unwrap_err();
    assert!(err.to_string().contains("publish"), "got: {err}");
    assert_eq!(*runner.attempts.lock().unwrap(), vec!["core", "sdk"]);
    assert!(runner.published.lock().unwrap().contains("@x/core@1.0.0"));
    assert!(!runner.published.lock().unwrap().contains("@x/sdk@1.0.0"));
    assert!(!runner.published.lock().unwrap().contains("@x/mid@1.0.0"));

    // Resume: core is skipped (already published), sdk + mid go through.
    orchestrate(
        &adapter,
        &git,
        &forge,
        root,
        &package_tag_options(),
        &hooks,
        &hook_runner,
    )
    .unwrap();
    assert!(runner.published.lock().unwrap().contains("@x/sdk@1.0.0"));
    assert!(runner.published.lock().unwrap().contains("@x/mid@1.0.0"));
}

#[test]
fn resume_after_failed_tag_step_tags_without_republishing() {
    // The registry publish succeeds but the tagging step then fails. On resume the package must
    // get its tag + GitHub Release created *without* re-publishing the already-shipped version.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_chain(root);

    let runner = PubRunner::new(&[]);
    let adapter = NpmAdapter::with_runner(root, Box::new(runner.clone()));
    let git = FakeGit::default();
    *git.fail_create_tag_once.borrow_mut() = true; // first tag attempt dies
    let forge = FakeForge::default();
    let hooks = otf_release_core::config::Hooks::default();
    let hook_runner = otf_release_core::hooks::fakes::FakeHookRunner::new();

    // First run: core publishes, then create_tag fails → halt. core is on the registry but untagged.
    let err = orchestrate(
        &adapter,
        &git,
        &forge,
        root,
        &package_tag_options(),
        &hooks,
        &hook_runner,
    )
    .unwrap_err();
    assert!(err.to_string().contains("create_tag"), "got: {err}");
    assert!(runner.published.lock().unwrap().contains("@x/core@1.0.0"));
    assert!(git.tags.borrow().is_empty());
    assert!(forge.releases.borrow().is_empty());

    // Resume: core is already published, so it must NOT publish again, but it must now be tagged
    // and released. sdk + mid proceed normally afterward.
    orchestrate(
        &adapter,
        &git,
        &forge,
        root,
        &package_tag_options(),
        &hooks,
        &hook_runner,
    )
    .unwrap();

    assert_eq!(
        runner
            .publish_log
            .lock()
            .unwrap()
            .iter()
            .filter(|s| *s == "@x/core@1.0.0")
            .count(),
        1,
        "core must be published exactly once across both runs"
    );
    assert!(git.tags.borrow().iter().any(|t| t == "@x/core@1.0.0"));
    assert!(forge.releases.borrow().iter().any(|t| t == "@x/core@1.0.0"));
    assert!(git.tags.borrow().iter().any(|t| t == "@x/mid@1.0.0"));
}

#[test]
fn multi_adapter_publish_runs_hooks_once_for_the_whole_command() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let a = FakeAdapter::new(root, "a");
    let b = FakeAdapter::new(root, "b");
    let git = FakeGit::default();
    let forge = FakeForge::default();
    let hooks = otf_release_core::config::Hooks {
        pre_publish: vec!["pre".to_string()],
        post_publish: vec!["post".to_string()],
        ..Default::default()
    };
    let hook_runner = otf_release_core::hooks::fakes::FakeHookRunner::new();

    orchestrate_many(
        &[&a, &b],
        &git,
        &forge,
        root,
        &package_tag_options(),
        &hooks,
        &hook_runner,
    )
    .unwrap();

    assert_eq!(
        hook_runner.executed.borrow().as_slice(),
        ["pre".to_string(), "post".to_string()]
    );
    assert_eq!(a.published.borrow().as_slice(), ["a@1.0.0".to_string()]);
    assert_eq!(b.published.borrow().as_slice(), ["b@1.0.0".to_string()]);
}

/// A matrix publish-mode package must never reach the registry without its per-platform binaries.
/// With the package named in `require_staged` and no staged tree under `--artifacts-dir`, publish
/// must hard-fail instead of shipping a binary-less, broken package.
#[test]
fn matrix_package_without_staged_binaries_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        root.join("package.json"),
        r#"{ "name": "root", "private": true, "workspaces": ["packages/*"] }"#,
    );
    write(
        root.join("packages/wc/package.json"),
        "{\n  \"name\": \"@x/wc\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    write(
        root.join("packages/wc/CHANGELOG.md"),
        "# Changelog\n\n## [1.0.0] - 2024-01-01\n- notes\n",
    );

    let runner = PubRunner::new(&[]);
    let adapter = NpmAdapter::with_runner(root, Box::new(runner.clone()));
    let git = FakeGit::default();
    let forge = FakeForge::default();
    let hooks = otf_release_core::config::Hooks::default();
    let hook_runner = otf_release_core::hooks::fakes::FakeHookRunner::new();

    // An artifacts dir that exists but holds nothing for @x/wc.
    let artifacts = root.join(".artifacts");
    fs::create_dir_all(&artifacts).unwrap();

    let opts = PublishOptions {
        tag_format: "{name}@{version}".to_string(),
        artifacts_dir: Some(artifacts),
        require_staged: vec!["@x/wc".to_string()],
        ..PublishOptions::default()
    };

    let err = orchestrate(&adapter, &git, &forge, root, &opts, &hooks, &hook_runner).unwrap_err();
    assert!(
        err.to_string().contains("binary-less"),
        "expected a binary-less refusal, got: {err}"
    );
    // Nothing was published, tagged, or released.
    assert!(runner.publish_log.lock().unwrap().is_empty());
    assert!(git.tags.borrow().is_empty());
}
