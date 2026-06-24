//! External-process execution behind a trait, so adapter registry/publish calls are testable
//! without a live `npm`/`cargo` or network. Shared by every adapter.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

/// Result of running an external command, normalized for both the real and faked runners.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Seam over external process execution.
pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str], cwd: &Path) -> Result<CommandOutput>;
}

/// The production runner — shells out for real.
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[&str], cwd: &Path) -> Result<CommandOutput> {
        let out = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .output()
            .with_context(|| format!("failed to spawn `{program}`"))?;
        Ok(CommandOutput {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}
