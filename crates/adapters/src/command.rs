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

/// How a failed publish is retried. See [`run_publish`].
#[derive(Debug, Clone, Copy)]
pub struct PublishRetry {
    /// Total tries for a publish failing on the network or a 5xx.
    pub attempts: u32,
    /// Wait before the first transient retry; doubled after each one.
    pub backoff: Duration,
    /// How many registry rate limits are waited out before giving up.
    pub rate_limit_waits: u32,
    /// Wait for a rate limit whose response names no retry time.
    pub rate_limit_wait: Duration,
}

impl PublishRetry {
    /// crates.io lets a burst of 5 new crates through, then one every 10 minutes. Twelve waits
    /// therefore carry a first release of ~17 crates through in one run; each wait is bounded by
    /// [`RATE_LIMIT_MAX_WAIT`] so a misread retry time can't park the job for hours.
    pub const DEFAULT: Self = Self {
        attempts: 4,
        backoff: Duration::from_secs(30),
        rate_limit_waits: 12,
        rate_limit_wait: Duration::from_secs(600),
    };
}

/// Upper bound on a single rate-limit wait, whatever the registry asks for.
const RATE_LIMIT_MAX_WAIT: Duration = Duration::from_secs(3600);

/// Run a registry **publish**, retrying rate limits and transient failures.
///
/// Unlike [`run_probe`], a publish is not safe to repeat blindly: a response lost after the upload
/// landed would make the retry fail with "version already exists". So before every retry the
/// registry is asked through `already_published`; a version that made it is reported as success.
/// A rate limit is waited out for the time the registry names (`try again after <date>`, as
/// crates.io sends for new crates), or [`PublishRetry::rate_limit_wait`] when it names none. Any
/// other failure is returned at once.
pub fn run_publish(
    runner: &dyn CommandRunner,
    program: &str,
    args: &[&str],
    cwd: &Path,
    already_published: &dyn Fn() -> Result<bool>,
) -> Result<CommandOutput> {
    run_publish_with(
        runner,
        program,
        args,
        cwd,
        already_published,
        PublishRetry::DEFAULT,
        &thread::sleep,
    )
}

pub fn run_publish_with(
    runner: &dyn CommandRunner,
    program: &str,
    args: &[&str],
    cwd: &Path,
    already_published: &dyn Fn() -> Result<bool>,
    policy: PublishRetry,
    sleep: &dyn Fn(Duration),
) -> Result<CommandOutput> {
    let command = format!("{program} {}", args.join(" "));
    let mut transient_failures = 0;
    let mut rate_limit_waits = 0;
    let mut backoff = policy.backoff;
    loop {
        let out = runner.run(program, args, cwd)?;
        if out.success {
            return Ok(out);
        }
        let wait = if is_rate_limited(&out.stderr) {
            rate_limit_waits += 1;
            if rate_limit_waits > policy.rate_limit_waits {
                return Ok(out);
            }
            let wait = retry_after(&out.stderr, unix_now())
                .unwrap_or(policy.rate_limit_wait)
                .min(RATE_LIMIT_MAX_WAIT);
            eprintln!(
                "`{command}` was rate limited by the registry; waiting {}s before retry {rate_limit_waits}/{}",
                wait.as_secs(),
                policy.rate_limit_waits
            );
            wait
        } else if is_transient(&out.stderr) {
            transient_failures += 1;
            if transient_failures >= policy.attempts {
                return Ok(out);
            }
            let wait = backoff;
            backoff = backoff.saturating_mul(2);
            eprintln!(
                "`{command}` failed transiently; waiting {}s before retry {}/{}",
                wait.as_secs(),
                transient_failures + 1,
                policy.attempts
            );
            wait
        } else {
            return Ok(out);
        };
        sleep(wait);
        if already_published()? {
            eprintln!("`{command}` reached the registry despite the error; not publishing again");
            return Ok(CommandOutput {
                success: true,
                ..out
            });
        }
    }
}

/// Whether the registry refused the request for rate, as opposed to failing it.
fn is_rate_limited(stderr: &str) -> bool {
    let haystack = stderr.to_lowercase();
    [
        "429",
        "too many requests",
        "rate limit",
        "too many new crates",
    ]
    .iter()
    .any(|signal| haystack.contains(signal))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The wait a rate-limit response asks for: crates.io says `Please try again after
/// Wed, 07 Oct 2026 14:23:11 GMT`. A few seconds of margin absorb clock skew with the registry.
fn retry_after(stderr: &str, now: u64) -> Option<Duration> {
    const MARKER: &str = "try again after ";
    let lower = stderr.to_lowercase();
    let start = lower.find(MARKER)? + MARKER.len();
    let at = parse_http_date(&stderr[start..])?;
    Some(Duration::from_secs(at.saturating_sub(now) + 5))
}

/// Parse the leading IMF-fixdate (`Wed, 07 Oct 2026 14:23:11 GMT`) of `text` to Unix seconds.
fn parse_http_date(text: &str) -> Option<u64> {
    let mut words = text.split_whitespace();
    words.next()?; // weekday
    let day: u64 = words.next()?.parse().ok()?;
    let month = match words.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = words.next()?.parse().ok()?;
    let mut clock = words.next()?.split(':').map(|n| n.parse::<u64>().ok());
    let (h, m, s) = (clock.next()??, clock.next()??, clock.next()??);
    let days = u64::try_from(days_from_civil(year, month, day as i64)).ok()?;
    Some(days * 86_400 + h * 3600 + m * 60 + s)
}

/// Days since the Unix epoch for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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

    fn publish(runner: &ScriptedRunner, published_after_error: bool) -> CommandOutput {
        let policy = PublishRetry {
            attempts: 3,
            backoff: Duration::ZERO,
            rate_limit_waits: 2,
            rate_limit_wait: Duration::ZERO,
        };
        run_publish_with(
            runner,
            "cargo",
            &["publish"],
            Path::new("."),
            &|| Ok(published_after_error),
            policy,
            &|_| {},
        )
        .unwrap()
    }

    #[test]
    fn a_rate_limited_publish_waits_and_retries() {
        let runner = ScriptedRunner::new(vec![
            (
                false,
                "error: 429 Too Many Requests: You have published too many new crates",
            ),
            (true, ""),
        ]);
        assert!(publish(&runner, false).success);
        assert_eq!(runner.calls(), 2);
    }

    #[test]
    fn rate_limit_waits_are_bounded() {
        let runner = ScriptedRunner::new(vec![(false, "status 429 Too Many Requests")]);
        assert!(!publish(&runner, false).success);
        assert_eq!(runner.calls(), 3, "one try plus two waits");
    }

    /// The upload landed but the response was lost: publishing again would fail with "already
    /// exists", so the registry's answer wins.
    #[test]
    fn a_publish_that_landed_despite_the_error_is_not_repeated() {
        let runner = ScriptedRunner::new(vec![(false, "error: connection reset by peer")]);
        assert!(publish(&runner, true).success);
        assert_eq!(runner.calls(), 1);
    }

    #[test]
    fn transient_publish_failures_are_retried_a_bounded_number_of_times() {
        let runner = ScriptedRunner::new(vec![(false, "503 Service Unavailable")]);
        assert!(!publish(&runner, false).success);
        assert_eq!(runner.calls(), 3);
    }

    #[test]
    fn a_permanent_publish_failure_is_not_retried() {
        let runner = ScriptedRunner::new(vec![(false, "error: crate name is already taken")]);
        assert!(!publish(&runner, false).success);
        assert_eq!(runner.calls(), 1);
    }

    #[test]
    fn retry_after_reads_the_crates_io_date() {
        let msg = "You have published too many new crates in a short period of time. \
                   Please try again after Wed, 07 Oct 2026 14:23:11 GMT and see \
                   https://crates.io/docs/rate-limits for more details";
        let at = parse_http_date("Wed, 07 Oct 2026 14:23:11 GMT").unwrap();
        assert_eq!(at, 1_791_382_991);
        assert_eq!(retry_after(msg, at - 60), Some(Duration::from_secs(65)));
        // Already past: retry almost at once rather than not at all.
        assert_eq!(retry_after(msg, at + 60), Some(Duration::from_secs(5)));
        assert_eq!(retry_after("429 Too Many Requests", at), None);
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
