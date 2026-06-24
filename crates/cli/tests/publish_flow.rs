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

use opentf_release_adapters::npm::{CommandOutput, CommandRunner, NpmAdapter};
use opentf_release_core::forge::Forge;
use opentf_release_core::git::GitOps;
use opentf_release_core::publish::{orchestrate, PublishOptions};

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
            fail_once: Arc::new(Mutex::new(fail_once.iter().map(|s| s.to_string()).collect())),
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
        self.tags.borrow_mut().push(name.to_string());
        Ok(())
    }
    fn push_tag(&self, _name: &str) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct FakeForge {
    releases: RefCell<Vec<String>>,
}
impl Forge for FakeForge {
    fn open_pr(&self, _branch: &str, _title: &str, _body: &str) -> Result<()> {
        Ok(())
    }
    fn create_release(&self, tag: &str, _title: &str, _notes: &str) -> Result<()> {
        self.releases.borrow_mut().push(tag.to_string());
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

    orchestrate(&adapter, &git, &forge, &PublishOptions::default()).unwrap();

    assert_eq!(
        *runner.publish_log.lock().unwrap(),
        vec!["@x/core@1.0.0", "@x/sdk@1.0.0", "@x/mid@1.0.0"]
    );
    assert_eq!(git.tags.borrow().len(), 3);
    assert_eq!(forge.releases.borrow().len(), 3);

    // Second run publishes nothing (everything is already at its published version).
    let published_before = runner.publish_log.lock().unwrap().len();
    let tags_before = git.tags.borrow().len();
    orchestrate(&adapter, &git, &forge, &PublishOptions::default()).unwrap();
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
    let err = orchestrate(&adapter, &git, &forge, &PublishOptions::default()).unwrap_err();
    assert!(err.to_string().contains("publish"), "got: {err}");
    assert_eq!(*runner.attempts.lock().unwrap(), vec!["core", "sdk"]);
    assert!(runner.published.lock().unwrap().contains("@x/core@1.0.0"));
    assert!(!runner.published.lock().unwrap().contains("@x/sdk@1.0.0"));
    assert!(!runner.published.lock().unwrap().contains("@x/mid@1.0.0"));

    // Resume: core is skipped (already published), sdk + mid go through.
    orchestrate(&adapter, &git, &forge, &PublishOptions::default()).unwrap();
    assert!(runner.published.lock().unwrap().contains("@x/sdk@1.0.0"));
    assert!(runner.published.lock().unwrap().contains("@x/mid@1.0.0"));
}
