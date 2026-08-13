//! External-process execution behind a trait, so adapter registry/publish calls are testable
//! without a live `npm`/`cargo` or network. Shared by every adapter.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

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

/// How long a single external command may run before it is killed.
///
/// This is a hang guard, not a latency budget: `cargo publish` legitimately blocks for minutes
/// waiting for the crates.io index to carry the version it just uploaded. A run that reaches this
/// limit is stuck, and without it a wedged child holds the CI runner until the job-level timeout
/// with no indication of which command hung.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(900);

/// Attempts for a read-only probe, and the first backoff between them.
pub const PROBE_ATTEMPTS: u32 = 3;
pub const PROBE_BACKOFF: Duration = Duration::from_millis(750);

/// The production runner — shells out for real.
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[&str], cwd: &Path) -> Result<CommandOutput> {
        run_with_timeout(program, args, cwd, COMMAND_TIMEOUT)
    }
}

/// Spawn `program`, capture both streams, and kill it if it outlives `timeout`.
///
/// The pipes are drained by their own threads rather than read after `wait`: a child that fills
/// the 64 KiB pipe buffer blocks forever on write while the parent blocks on wait, and a timeout
/// that can itself deadlock is worse than none. `cargo publish` on a large workspace produces
/// enough output to hit that.
fn run_with_timeout(
    program: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> Result<CommandOutput> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn `{program}`"))?;

    let mut out_pipe = child.stdout.take().expect("stdout piped");
    let mut err_pipe = child.stderr.take().expect("stderr piped");
    let out_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        buf
    });
    let err_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child
            .try_wait()
            .with_context(|| format!("waiting on `{program}`"))?
        {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "`{program} {}` did not finish within {}s and was killed",
                    args.join(" "),
                    timeout.as_secs()
                );
            }
            None => thread::sleep(Duration::from_millis(25)),
        }
    };

    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    Ok(CommandOutput {
        success: status.success(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// Run a **read-only** registry probe, retrying transient failures with exponential backoff.
///
/// Only for commands that are safe to repeat — `npm view`, `cargo info`. A publish must never come
/// through here: it is not idempotent at the registry, and a retry after a response that was lost
/// in transit would attempt to publish a version that already exists.
///
/// A failure that is *not* transient returns immediately, so `is_published`'s 404 path — the
/// expected "not published yet" answer — costs nothing.
pub fn run_probe(
    runner: &dyn CommandRunner,
    program: &str,
    args: &[&str],
    cwd: &Path,
) -> Result<CommandOutput> {
    run_probe_with(runner, program, args, cwd, PROBE_ATTEMPTS, PROBE_BACKOFF)
}

pub fn run_probe_with(
    runner: &dyn CommandRunner,
    program: &str,
    args: &[&str],
    cwd: &Path,
    attempts: u32,
    backoff: Duration,
) -> Result<CommandOutput> {
    let mut delay = backoff;
    let mut last = runner.run(program, args, cwd)?;
    for attempt in 2..=attempts {
        if last.success || !is_transient(&last.stderr) {
            return Ok(last);
        }
        if !delay.is_zero() {
            thread::sleep(delay);
        }
        delay = delay.saturating_mul(2);
        eprintln!(
            "`{program} {}` failed transiently; retry {attempt}/{attempts}",
            args.join(" ")
        );
        last = runner.run(program, args, cwd)?;
    }
    Ok(last)
}

/// Whether a failure looks like the network or the registry rather than an answer.
///
/// Deliberately a denylist of transient signals, not an allowlist of permanent ones: misreading a
/// real "not found" as transient only wastes a few seconds, while misreading a dropped connection
/// as permanent aborts a release that would have succeeded.
fn is_transient(stderr: &str) -> bool {
    const SIGNALS: &[&str] = &[
        "econnreset",
        "econnrefused",
        "etimedout",
        "eai_again",
        "enotfound",
        "socket hang up",
        "network timeout",
        "timed out",
        "connection reset",
        "temporarily unavailable",
        "service unavailable",
        "bad gateway",
        "gateway timeout",
        "too many requests",
        "rate limit",
        "429",
        "502",
        "503",
        "504",
    ];
    let haystack = stderr.to_lowercase();
    SIGNALS.iter().any(|signal| haystack.contains(signal))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Replies from a scripted list, one per call, so a test can model "fails twice then works".
    struct ScriptedRunner {
        replies: Vec<CommandOutput>,
        calls: AtomicUsize,
    }

    impl ScriptedRunner {
        fn new(replies: Vec<(bool, &str)>) -> Self {
            Self {
                replies: replies
                    .into_iter()
                    .map(|(success, stderr)| CommandOutput {
                        success,
                        stdout: String::new(),
                        stderr: stderr.to_string(),
                    })
                    .collect(),
                calls: AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl CommandRunner for ScriptedRunner {
        fn run(&self, _: &str, _: &[&str], _: &Path) -> Result<CommandOutput> {
            let i = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.replies[i.min(self.replies.len() - 1)].clone())
        }
    }

    fn probe(runner: &ScriptedRunner) -> CommandOutput {
        run_probe_with(runner, "npm", &["view"], Path::new("."), 3, Duration::ZERO).unwrap()
    }

    #[test]
    fn a_transient_failure_is_retried_until_it_succeeds() {
        let runner = ScriptedRunner::new(vec![
            (false, "npm ERR! network socket hang up"),
            (false, "npm ERR! 503 Service Unavailable"),
            (true, ""),
        ]);
        assert!(probe(&runner).success);
        assert_eq!(runner.calls(), 3);
    }

    /// The expected answer from `is_published` for an unpublished version. Retrying it would add
    /// seconds of latency to the common path of every release.
    #[test]
    fn a_404_is_an_answer_and_is_not_retried() {
        let runner = ScriptedRunner::new(vec![(false, "npm ERR! code E404")]);
        assert!(!probe(&runner).success);
        assert_eq!(runner.calls(), 1);
    }

    #[test]
    fn retries_are_bounded_and_the_last_failure_is_returned() {
        let runner = ScriptedRunner::new(vec![(false, "ETIMEDOUT")]);
        let out = probe(&runner);
        assert!(!out.success);
        assert!(out.stderr.contains("ETIMEDOUT"));
        assert_eq!(runner.calls(), 3, "must not retry forever");
    }

    #[test]
    fn a_hung_command_is_killed_rather_than_holding_the_runner() {
        let (program, args): (&str, Vec<&str>) = if cfg!(windows) {
            ("powershell", vec!["-Command", "Start-Sleep -Seconds 30"])
        } else {
            ("sh", vec!["-c", "sleep 30"])
        };
        let err = run_with_timeout(program, &args, Path::new("."), Duration::from_millis(200))
            .unwrap_err();
        assert!(err.to_string().contains("did not finish"), "{err}");
    }

    /// The reader threads exist so a child that outruns the pipe buffer cannot deadlock the wait
    /// loop. 200 KiB is comfortably past the usual 64 KiB pipe capacity.
    #[test]
    fn output_larger_than_the_pipe_buffer_does_not_deadlock() {
        if cfg!(windows) {
            return;
        }
        let out = run_with_timeout(
            "sh",
            &["-c", "yes abcdefghij | head -c 200000"],
            Path::new("."),
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(out.success);
        assert_eq!(out.stdout.len(), 200_000);
    }
}
