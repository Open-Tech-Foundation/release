//! Lifecycle hooks runner.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

/// A trait for executing lifecycle hook shell commands.
pub trait HookRunner {
    /// Execute a sequence of shell commands in order.
    fn run_hooks(&self, root: &Path, commands: &[String]) -> Result<()>;
}

/// The real runner that shells out via `sh -c`.
pub struct ShHookRunner;

impl HookRunner for ShHookRunner {
    fn run_hooks(&self, root: &Path, commands: &[String]) -> Result<()> {
        for cmd in commands {
            println!("> Running hook: {cmd}");
            let (shell, arg) = if cfg!(windows) {
                ("powershell", "-Command")
            } else {
                ("sh", "-c")
            };
            
            let status = Command::new(shell)
                .arg(arg)
                .arg(cmd)
                .current_dir(root)
                .status()
                .with_context(|| format!("failed to execute hook: {cmd}"))?;
            
            if !status.success() {
                bail!("hook failed with {status}: {cmd}");
            }
        }
        Ok(())
    }
}

pub mod fakes {
    use super::*;
    use std::cell::RefCell;

    pub struct FakeHookRunner {
        pub executed: RefCell<Vec<String>>,
    }

    impl FakeHookRunner {
        pub fn new() -> Self {
            Self {
                executed: RefCell::new(Vec::new()),
            }
        }
    }

    impl HookRunner for FakeHookRunner {
        fn run_hooks(&self, _root: &Path, commands: &[String]) -> Result<()> {
            for cmd in commands {
                self.executed.borrow_mut().push(cmd.clone());
            }
            Ok(())
        }
    }
}
