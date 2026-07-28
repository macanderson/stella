//! The PR / CI monitor.
//!
//! Everything runs behind the [`GhCli`] port (real impl shells to `gh`), so
//! the reconciliation and the capped-wait state machine are exercised against
//! fakes. Two jobs:
//!
//! - [`Monitor::pr_status`] — **live reconcile** (L-V3): always ask `gh` for
//!   the PR's current state, never serve a cached value as if it were current.
//! - [`Monitor::watch_ci`] — a **capped deferred wait** (L-E4): poll CI on an
//!   interval; the wait window extends *only* when fresh evidence confirms
//!   runs are still progressing; a cumulative wall cap (default 2h) always
//!   bounds it, and a run that stops progressing without completing times out
//!   naming what was last observed. The timing decision is a pure function
//!   (`decide`, private to this module) so the cap arithmetic is table-testable
//!   with an injected [`Clock`]; the async loop only handles polling and
//!   sleeping.
//!
//! Long external waits are deferred waits with their own cumulative caps —
//! never a raised global turn timeout (L-E4).

use std::time::Duration;

use async_trait::async_trait;
use stella_core::Clock;
use stella_protocol::{AgentEvent, CiStatus, PrStatus};

use crate::ledger::CommitRecord;

// gh port

/// The port every `gh` command goes through. Real impl ([`SystemGhCli`])
/// spawns `gh`; tests inject fakes.
#[async_trait]
pub trait GhCli: Send + Sync {
    /// Run `gh <args>`. A non-zero exit is reported in the returned
    /// [`GhOutput`] (`success == false`); `Err` is reserved for a failure to
    /// spawn `gh` at all.
    async fn run(&self, args: &[&str]) -> Result<GhOutput, GhError>;
}

/// The result of one `gh` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl GhOutput {
    /// A successful invocation with the given stdout — test ergonomics.
    #[must_use]
    pub fn ok(stdout: impl Into<String>) -> Self {
        Self {
            success: true,
            code: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    /// A failed invocation with an exit code and stderr — test ergonomics.
    #[must_use]
    pub fn failed(code: i32, stderr: impl Into<String>) -> Self {
        Self {
            success: false,
            code: Some(code),
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }
}

/// A failure to *run* `gh` at all.
#[derive(Debug, thiserror::Error)]
pub enum GhError {
    #[error("failed to spawn `gh {command}`: {reason}")]
    Spawn { command: String, reason: String },

    #[error("`gh {command}` did not finish within {timeout_ms}ms")]
    TimedOut { command: String, timeout_ms: u64 },
}

/// Per-invocation bound on one `gh` call. Every cap in [`Monitor::watch_ci`]
/// is measured *between* polls, so without this a `gh` that connects and then
/// never answers (a proxy black-holing the request) would park the whole
/// watch inside a single `output()` await where no cap can fire. Three
/// default poll intervals — generous against real `gh` latency, small next to
/// the watch caps.
const GH_INVOCATION_TIMEOUT_MS: u64 = 90_000;

/// The production [`GhCli`]: spawns the real `gh` binary, with the process
/// environment scrubbed down to the GitHub auth variables `gh` genuinely
/// needs, and `kill_on_drop` so neither a cancelled watch nor an expired
/// invocation timeout strands the child.
///
/// Each invocation is bounded by [`GH_INVOCATION_TIMEOUT_MS`]; expiry
/// surfaces as [`GhError::TimedOut`], which [`Monitor::watch_ci`]'s
/// consecutive-poll-error tolerance rides out like any other failed poll.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemGhCli;

#[async_trait]
impl GhCli for SystemGhCli {
    async fn run(&self, args: &[&str]) -> Result<GhOutput, GhError> {
        let mut command = tokio::process::Command::new("gh");
        command.args(args);
        stella_tools::subprocess_env::scrub_sensitive_env_except(
            &mut command,
            stella_tools::subprocess_env::GITHUB_CLI_AUTH_ENV_VARS,
        );
        run_with_timeout(command, args.join(" "), GH_INVOCATION_TIMEOUT_MS).await
    }
}

/// Run a prepared child to completion under a per-invocation bound.
/// `kill_on_drop` is set here because it is what reaps the child when the
/// timeout wins the race and the `output()` future is dropped.
async fn run_with_timeout(
    mut command: tokio::process::Command,
    rendered: String,
    timeout_ms: u64,
) -> Result<GhOutput, GhError> {
    command.kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_millis(timeout_ms), command.output())
        .await
        .map_err(|_| GhError::TimedOut {
            command: rendered.clone(),
            timeout_ms,
        })?
        .map_err(|e| GhError::Spawn {
            command: rendered,
            reason: e.to_string(),
        })?;
    Ok(GhOutput {
        success: output.status.success(),
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

// Sleeper (deferred-wait pacing; injectable so caps are testable)

/// The pacing seam for the poll loop — real impl sleeps, the test impl
/// advances the injected [`Clock`] instead so a 2h cap is proven in
/// microseconds.
#[async_trait]
pub trait Sleeper: Send + Sync {
    async fn sleep(&self, ms: u64);
}

/// Production [`Sleeper`] — a real `tokio` sleep.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioSleeper;

#[async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, ms: u64) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}

// Errors

#[derive(Debug, thiserror::Error)]
pub enum MonitorError {
    #[error(transparent)]
    Gh(#[from] GhError),

    #[error("`gh {command}` failed (exit {code:?}): {stderr}")]
    Command {
        command: String,
        code: Option<i32>,
        stderr: String,
    },

    #[error("could not parse gh output: {detail}")]
    Parse { detail: String },

    /// A caller-supplied PR reference that `gh` would read as a flag rather
    /// than as a positional. There is no shell involved, so this is argument
    /// injection, not command injection — but `-R owner/repo` would silently
    /// retarget the query at another repository and `--jq`/`--template` would
    /// change the output shape out from under the JSON parse. Rejected before
    /// the `gh` call rather than spliced in.
    #[error("PR reference `{reference}` starts with `-`: gh would parse it as a flag")]
    InvalidPrReference { reference: String },
}

/// Reject a caller-supplied positional that `gh` would parse as a flag —
/// shared by [`Monitor::pr_status`] and [`Monitor::poll_ci`], which splice
/// their reference into the argv (see
/// [`MonitorError::InvalidPrReference`]).
fn ensure_positional(reference: &str) -> Result<(), MonitorError> {
    if reference.starts_with('-') {
        return Err(MonitorError::InvalidPrReference {
            reference: reference.to_string(),
        });
    }
    Ok(())
}

fn ensure_ok(output: GhOutput, command: &str) -> Result<GhOutput, MonitorError> {
    if output.success {
        Ok(output)
    } else {
        Err(MonitorError::Command {
            command: command.to_string(),
            code: output.code,
            stderr: output.stderr,
        })
    }
}

// CI run model

/// A CI run's lifecycle status, from `gh run list`'s `status` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiRunStatus {
    Queued,
    InProgress,
    Completed,
    /// A gh status this build does not model (`waiting`, `requested`,
    /// `pending` and friends are folded into [`Queued`](Self::Queued)) —
    /// treated as still active (not completed), so an unrecognized status
    /// keeps the wait alive under the caps rather than ending it green.
    Other(String),
}

impl CiRunStatus {
    fn parse(raw: &str) -> Self {
        match raw {
            "queued" | "pending" | "waiting" | "requested" => CiRunStatus::Queued,
            "in_progress" => CiRunStatus::InProgress,
            "completed" => CiRunStatus::Completed,
            other => CiRunStatus::Other(other.to_string()),
        }
    }

    fn is_completed(&self) -> bool {
        matches!(self, CiRunStatus::Completed)
    }

    fn is_in_progress(&self) -> bool {
        matches!(self, CiRunStatus::InProgress)
    }
}

/// A CI run's terminal conclusion (only meaningful once completed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiConclusion {
    Success,
    Failure,
    Cancelled,
    Skipped,
    TimedOut,
    ActionRequired,
    Neutral,
    Stale,
    Other(String),
}

impl CiConclusion {
    fn parse(raw: &str) -> Self {
        match raw {
            "success" => CiConclusion::Success,
            "failure" => CiConclusion::Failure,
            "cancelled" => CiConclusion::Cancelled,
            "skipped" => CiConclusion::Skipped,
            "timed_out" => CiConclusion::TimedOut,
            "action_required" => CiConclusion::ActionRequired,
            "neutral" => CiConclusion::Neutral,
            "stale" => CiConclusion::Stale,
            other => CiConclusion::Other(other.to_string()),
        }
    }

    /// Whether this conclusion is a failure for the purpose of the overall
    /// verdict. Success/Skipped/Neutral pass; everything else (including an
    /// unknown conclusion) is treated as a failure — we never dress an
    /// unrecognized terminal state up as green.
    #[must_use]
    pub fn is_failure(&self) -> bool {
        !matches!(
            self,
            CiConclusion::Success | CiConclusion::Skipped | CiConclusion::Neutral
        )
    }
}

/// One CI run in a poll snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiRun {
    pub name: String,
    pub status: CiRunStatus,
    pub conclusion: Option<CiConclusion>,
}

/// The set of runs observed in one poll.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CiSnapshot {
    pub runs: Vec<CiRun>,
}

/// The aggregate state of a snapshot for the wait loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchPhase {
    /// No runs observed yet — CI hasn't started.
    Pending,
    /// At least one run is still queued/in-progress.
    Progressing,
    /// Every observed run has completed.
    AllCompleted,
}

impl CiSnapshot {
    #[must_use]
    pub fn phase(&self) -> WatchPhase {
        if self.runs.is_empty() {
            WatchPhase::Pending
        } else if self.runs.iter().all(|r| r.status.is_completed()) {
            WatchPhase::AllCompleted
        } else {
            WatchPhase::Progressing
        }
    }

    /// Whether any run is actively executing — one form of "fresh evidence"
    /// that the wait should extend (L-E4).
    #[must_use]
    pub fn any_in_progress(&self) -> bool {
        self.runs.iter().any(|r| r.status.is_in_progress())
    }

    /// The overall verdict, meaningful when [`phase`](Self::phase) is
    /// [`WatchPhase::AllCompleted`]: failure if any run failed, else success.
    #[must_use]
    pub fn overall_conclusion(&self) -> CiConclusion {
        let any_failed = self.runs.iter().any(|r| {
            r.conclusion
                .as_ref()
                .map(CiConclusion::is_failure)
                .unwrap_or(true)
        });
        if any_failed {
            CiConclusion::Failure
        } else {
            CiConclusion::Success
        }
    }

    /// A stable one-line description of what was last observed — used both as
    /// the timeout's `last_observed` and the completion summary.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.runs.is_empty() {
            return "no CI runs observed".to_string();
        }
        let completed = self.runs.iter().filter(|r| r.status.is_completed()).count();
        let in_progress = self
            .runs
            .iter()
            .filter(|r| r.status.is_in_progress())
            .count();
        let queued = self.runs.len() - completed - in_progress;
        format!(
            "{} run(s): {completed} completed, {in_progress} in progress, {queued} queued",
            self.runs.len()
        )
    }

    /// A change-detection fingerprint: the run set is "the same" across polls
    /// iff this is unchanged. A stall (no change) plus no active run is what
    /// eventually times out.
    fn fingerprint(&self) -> String {
        self.runs
            .iter()
            .map(|r| format!("{}:{:?}:{:?}", r.name, r.status, r.conclusion))
            .collect::<Vec<_>>()
            .join("|")
    }
}

// The capped-wait decision (pure — L-E4 cap arithmetic lives here)

/// Why a watch stopped waiting without a completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutReason {
    /// Hit the cumulative wall-clock cap (default 2h).
    CumulativeCap,
    /// Runs stopped progressing (no change, nothing active) past the stall
    /// window.
    Stalled,
    /// No CI runs ever appeared within the startup grace window.
    NoRunsStarted,
}

/// One step's decision from the pure state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchDecision {
    /// Keep waiting (the loop sleeps one interval, then polls again).
    Continue,
    Completed {
        conclusion: CiConclusion,
    },
    TimedOut {
        reason: TimeoutReason,
        last_observed: String,
    },
}

/// Tuning for [`Monitor::watch_ci`]. Defaults: poll every 30s, cumulative cap
/// 2h (L-E4), stall after 20m of no progress, give CI 10m to start, tolerate 3
/// consecutive poll failures, and read at most 50 runs per poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchConfig {
    /// How long [`Monitor::watch_ci`] sleeps between polls. Not validated:
    /// `0` polls `gh` as fast as the process can spawn it until a cap fires,
    /// and under a clock that only advances via the [`Sleeper`] (the test
    /// shape) no cap ever fires at all. Keep it at seconds, not milliseconds.
    pub poll_interval_ms: u64,
    /// The hard ceiling on one watch: once this much has elapsed since the
    /// first poll, the wait ends as [`TimeoutReason::CumulativeCap`] however
    /// much fresh evidence CI is producing (L-E4). It bounds the error path
    /// too — it is checked on a failed poll as well as a successful one.
    pub max_total_ms: u64,
    /// How long a *started* run-set may sit without producing fresh evidence
    /// (a changed fingerprint, or a job in progress) before the wait ends as
    /// [`TimeoutReason::Stalled`].
    pub stall_timeout_ms: u64,
    /// How long to wait for CI to appear at all before ending as
    /// [`TimeoutReason::NoRunsStarted`]. Measured from the first poll, not
    /// from the last one, because a snapshot with no runs is not evidence.
    pub startup_grace_ms: u64,
    /// How many consecutive failed polls the watch rides out before giving up
    /// and returning the last error — a transient `gh` failure (a 403
    /// secondary rate limit, a DNS blip) must not throw away a wait the caps
    /// are designed to hold for two hours. The count resets on any successful
    /// poll, and the cumulative cap still bounds the error path. `0` restores
    /// the old behavior: the first error ends the watch.
    pub max_consecutive_poll_errors: u32,
    /// `gh run list --limit` for one poll — the truncation point, config
    /// rather than a magic number in the argv. A branch with more runs than
    /// this can report [`WatchPhase::AllCompleted`] while older runs go
    /// unobserved, so it is a tuning knob a caller must be able to see.
    pub run_list_limit: u32,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: 30_000,
            max_total_ms: 2 * 60 * 60 * 1_000, // 2h cumulative cap (L-E4)
            stall_timeout_ms: 20 * 60 * 1_000, // 20m without progress
            startup_grace_ms: 10 * 60 * 1_000, // 10m for CI to appear
            max_consecutive_poll_errors: 3,
            run_list_limit: 50,
        }
    }
}

/// The pure wait decision, given the elapsed clocks and the current phase.
///
/// Precedence: a completion always wins; otherwise the cumulative cap is the
/// hard ceiling (checked before the softer stall/startup timers); then a
/// still-pending run is bounded by the startup grace and a progressing-but-
/// stalled run by the stall window. `elapsed_since_progress_ms` is reset to 0
/// by the caller whenever it sees fresh evidence, so a genuinely active run
/// never trips the stall timer.
fn decide(
    phase: WatchPhase,
    elapsed_total_ms: u64,
    elapsed_since_progress_ms: u64,
    overall: CiConclusion,
    last_observed: &str,
    config: &WatchConfig,
) -> WatchDecision {
    if phase == WatchPhase::AllCompleted {
        return WatchDecision::Completed {
            conclusion: overall,
        };
    }
    if elapsed_total_ms >= config.max_total_ms {
        return WatchDecision::TimedOut {
            reason: TimeoutReason::CumulativeCap,
            last_observed: last_observed.to_string(),
        };
    }
    match phase {
        // The startup grace is measured from the FIRST poll, so a run set that
        // was observed and then vanished (a force-push retiring the runs, or a
        // `run_list_limit` cut that drops the last one) re-reads as `Pending`
        // and ends the watch as `NoRunsStarted` on the next poll rather than as
        // a stall. The wait still terminates under a cap either way; only the
        // reported reason is coarser than what happened.
        WatchPhase::Pending => {
            if elapsed_total_ms >= config.startup_grace_ms {
                WatchDecision::TimedOut {
                    reason: TimeoutReason::NoRunsStarted,
                    last_observed: last_observed.to_string(),
                }
            } else {
                WatchDecision::Continue
            }
        }
        WatchPhase::Progressing => {
            if elapsed_since_progress_ms >= config.stall_timeout_ms {
                WatchDecision::TimedOut {
                    reason: TimeoutReason::Stalled,
                    last_observed: last_observed.to_string(),
                }
            } else {
                WatchDecision::Continue
            }
        }
        // Handled above; here only to keep the match total.
        WatchPhase::AllCompleted => WatchDecision::Completed {
            conclusion: overall,
        },
    }
}

/// How a `watch_ci` call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiWatchOutcome {
    Completed {
        conclusion: CiConclusion,
        summary: String,
    },
    TimedOut {
        reason: TimeoutReason,
        last_observed: String,
        waited_ms: u64,
    },
}

// Monitor

/// PR/CI monitor over a [`GhCli`]. Holds an injected [`Clock`] and [`Sleeper`]
/// so the capped-wait timing is deterministic in tests.
pub struct Monitor<H: GhCli> {
    gh: H,
    clock: Box<dyn Clock>,
    sleeper: Box<dyn Sleeper>,
    config: WatchConfig,
}

impl<H: GhCli> Monitor<H> {
    /// A monitor with the production [`TokioSleeper`] and default
    /// [`WatchConfig`].
    #[must_use]
    pub fn new(gh: H, clock: Box<dyn Clock>) -> Self {
        Self {
            gh,
            clock,
            sleeper: Box::new(TokioSleeper),
            config: WatchConfig::default(),
        }
    }

    /// Override the watch tuning (builder style).
    #[must_use]
    pub fn with_config(mut self, config: WatchConfig) -> Self {
        self.config = config;
        self
    }

    /// Inject a [`Sleeper`] — the seam tests use to advance the clock instead
    /// of really sleeping.
    #[must_use]
    pub fn with_sleeper(mut self, sleeper: Box<dyn Sleeper>) -> Self {
        self.sleeper = sleeper;
        self
    }

    /// The live status of a PR — `pr` is anything `gh pr view` resolves: a
    /// number, a URL, or a branch name (the fleet passes the worker's branch,
    /// which is how a run's PR is found without knowing its number). Always
    /// reconciled against `gh` — never a cached value (L-V3). Maps gh's
    /// `state`/`isDraft` onto [`PrStatus`].
    ///
    /// A reference starting with `-` is rejected with
    /// [`MonitorError::InvalidPrReference`] instead of being spliced into the
    /// argv, where gh would parse it as a flag. No production behavior
    /// changes: the fleet's own caller passes a `fleet/`-prefixed branch.
    pub async fn pr_status(&self, pr: &str) -> Result<PrStatus, MonitorError> {
        ensure_positional(pr)?;
        let out = self
            .gh
            .run(&["pr", "view", pr, "--json", "state,isDraft"])
            .await?;
        let out = ensure_ok(out, "pr view --json state,isDraft")?;
        let value: serde_json::Value =
            serde_json::from_str(out.stdout.trim()).map_err(|e| MonitorError::Parse {
                detail: format!("pr view json: {e}"),
            })?;
        let state = value.get("state").and_then(|v| v.as_str()).unwrap_or("");
        let is_draft = value
            .get("isDraft")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        match state {
            "MERGED" => Ok(PrStatus::Merged),
            "CLOSED" => Ok(PrStatus::Closed),
            "OPEN" => Ok(if is_draft {
                PrStatus::Draft
            } else {
                PrStatus::Open
            }),
            other => Err(MonitorError::Parse {
                detail: format!("unexpected PR state `{other}`"),
            }),
        }
    }

    /// One poll of CI runs for a git ref (`gh run list --branch <ref>`),
    /// reconciled live. Reads at most [`WatchConfig::run_list_limit`] runs.
    ///
    /// `git_ref` gets the same screening as [`pr_status`](Self::pr_status): a
    /// ref starting with `-` is rejected with
    /// [`MonitorError::InvalidPrReference`] instead of being spliced in after
    /// `--branch`, where gh would parse it as a flag.
    pub async fn poll_ci(&self, git_ref: &str) -> Result<CiSnapshot, MonitorError> {
        ensure_positional(git_ref)?;
        let limit = self.config.run_list_limit.to_string();
        let out = self
            .gh
            .run(&[
                "run",
                "list",
                "--branch",
                git_ref,
                "--json",
                "status,conclusion,name",
                "--limit",
                &limit,
            ])
            .await?;
        let out = ensure_ok(out, "run list --json status,conclusion,name")?;
        parse_run_list(&out.stdout)
    }

    /// Watch CI for a git ref to completion, as a capped deferred wait
    /// (L-E4). Polls on the configured interval; extends only on fresh
    /// evidence (a changed snapshot or an actively-running job); bounded by
    /// the cumulative cap, the stall window, and the startup grace. Never
    /// raises a global timeout — this wait owns its own caps.
    ///
    /// A poll that *errors* (gh rate-limited, offline, not authenticated) is
    /// ridden out: the watch sleeps one interval and polls again, up to
    /// [`WatchConfig::max_consecutive_poll_errors`] failures in a row, and
    /// returns the last error once that is exceeded. The counter resets on any
    /// successful poll, and the cumulative cap still bounds the error path, so
    /// a permanently unreachable `gh` cannot spin past `max_total_ms`. An
    /// error is never treated as progress — an unreachable gh is not evidence
    /// that CI moved — so the stall window keeps running underneath it.
    pub async fn watch_ci(&self, git_ref: &str) -> Result<CiWatchOutcome, MonitorError> {
        let start = self.clock.now_ms();
        let mut last_progress = start;
        let mut last_fingerprint: Option<String> = None;
        let mut consecutive_errors: u32 = 0;

        loop {
            let snapshot = match self.poll_ci(git_ref).await {
                Ok(snapshot) => {
                    consecutive_errors = 0;
                    snapshot
                }
                Err(e) => {
                    consecutive_errors += 1;
                    let elapsed_total = self.clock.now_ms().saturating_sub(start);
                    if consecutive_errors > self.config.max_consecutive_poll_errors
                        || elapsed_total >= self.config.max_total_ms
                    {
                        return Err(e);
                    }
                    self.sleeper.sleep(self.config.poll_interval_ms).await;
                    continue;
                }
            };
            // Recomputed fresh each poll — the timeout/summary text always
            // reflects the latest observation, never a stale carry-over.
            let last_observed = snapshot.summary();
            let now = self.clock.now_ms();

            // Fresh evidence extends the wait: the run set changed since last
            // poll, OR a job is actively in progress (L-E4).
            let fingerprint = snapshot.fingerprint();
            let changed = last_fingerprint.as_deref() != Some(fingerprint.as_str());
            if changed || snapshot.any_in_progress() {
                last_progress = now;
            }
            last_fingerprint = Some(fingerprint);

            let elapsed_total = now.saturating_sub(start);
            let elapsed_since_progress = now.saturating_sub(last_progress);
            let decision = decide(
                snapshot.phase(),
                elapsed_total,
                elapsed_since_progress,
                snapshot.overall_conclusion(),
                &last_observed,
                &self.config,
            );

            match decision {
                WatchDecision::Continue => self.sleeper.sleep(self.config.poll_interval_ms).await,
                WatchDecision::Completed { conclusion } => {
                    return Ok(CiWatchOutcome::Completed {
                        conclusion,
                        summary: last_observed,
                    });
                }
                WatchDecision::TimedOut {
                    reason,
                    last_observed,
                } => {
                    return Ok(CiWatchOutcome::TimedOut {
                        reason,
                        last_observed,
                        waited_ms: elapsed_total,
                    });
                }
            }
        }
    }
}

fn parse_run_list(stdout: &str) -> Result<CiSnapshot, MonitorError> {
    let trimmed = stdout.trim();
    let json = if trimmed.is_empty() { "[]" } else { trimmed };
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(json).map_err(|e| MonitorError::Parse {
            detail: format!("run list json: {e}"),
        })?;
    let runs = rows
        .into_iter()
        .map(|row| {
            let name = row
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let status =
                CiRunStatus::parse(row.get("status").and_then(|v| v.as_str()).unwrap_or(""));
            let conclusion = row
                .get("conclusion")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(CiConclusion::parse);
            CiRun {
                name,
                status,
                conclusion,
            }
        })
        .collect();
    Ok(CiSnapshot { runs })
}

// Emit-shape helpers (protocol event values)

/// Build an [`AgentEvent::Commit`] from a ledger [`CommitRecord`] — the one
/// place the fleet turns a recorded commit into the wire event the TUI/JSON
/// serializer renders.
pub fn commit_event(commit: &CommitRecord) -> AgentEvent {
    AgentEvent::Commit {
        sha: commit.sha.clone(),
        message: commit.message.clone(),
    }
}

/// Build an [`AgentEvent::Pr`] from a reconciled PR url + status. The PR
/// number is derived from the url; `ci` is absent (not polled) — use
/// [`pr_event_with_ci`] when a checks verdict was observed alongside.
pub fn pr_event(url: impl Into<String>, status: PrStatus) -> AgentEvent {
    pr_event_with_ci(url, status, None)
}

/// Build an [`AgentEvent::Pr`] carrying an observed CI verdict for the PR's
/// head commit.
pub fn pr_event_with_ci(
    url: impl Into<String>,
    status: PrStatus,
    ci: Option<CiStatus>,
) -> AgentEvent {
    let url = url.into();
    AgentEvent::Pr {
        number: parse_pr_number(&url),
        url,
        status,
        ci,
    }
}

/// Extract the PR number from a GitHub-style PR url — the trailing integer
/// path segment of `…/pull/<n>`. Returns `None` for anything else rather
/// than guessing (an absent number renders as the bare url, never wrong).
pub fn parse_pr_number(url: &str) -> Option<u64> {
    let mut segments = url.trim_end_matches('/').rsplit('/');
    let number = segments.next()?.parse().ok()?;
    (segments.next()? == "pull").then_some(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Fakes

    /// A `Clock` whose time only moves when the test (or the advancing
    /// sleeper) says so.
    #[derive(Clone)]
    struct ManualClock(Arc<AtomicU64>);
    impl ManualClock {
        fn new() -> Self {
            Self(Arc::new(AtomicU64::new(0)))
        }
    }
    impl Clock for ManualClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    /// A `Sleeper` that advances the shared clock by exactly the requested
    /// interval instead of really sleeping — so a 2h cap is proven instantly
    /// and deterministically.
    struct AdvancingSleeper(Arc<AtomicU64>);
    #[async_trait]
    impl Sleeper for AdvancingSleeper {
        async fn sleep(&self, ms: u64) {
            self.0.fetch_add(ms, Ordering::SeqCst);
        }
    }

    /// A scripted `gh`: records calls, pops one response per call, and
    /// repeats the last response once its queue is down to one entry (so a
    /// "stuck CI" scenario is expressed by the same JSON forever).
    struct ScriptedGh {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        responses: Mutex<VecDeque<GhOutput>>,
    }
    impl ScriptedGh {
        fn new(responses: Vec<GhOutput>) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                responses: Mutex::new(responses.into()),
            }
        }
    }
    #[async_trait]
    impl GhCli for ScriptedGh {
        async fn run(&self, args: &[&str]) -> Result<GhOutput, GhError> {
            self.calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(args.iter().map(|s| s.to_string()).collect());
            let mut q = self.responses.lock().unwrap_or_else(|e| e.into_inner());
            let out = if q.len() > 1 {
                q.pop_front()
            } else {
                q.front().cloned()
            };
            Ok(out.unwrap_or_else(|| GhOutput::ok("[]")))
        }
    }

    fn run_json(runs: &[(&str, &str, &str)]) -> String {
        // (status, conclusion, name); empty conclusion -> null
        let items: Vec<String> = runs
            .iter()
            .map(|(status, concl, name)| {
                let concl_json = if concl.is_empty() {
                    "null".to_string()
                } else {
                    format!("\"{concl}\"")
                };
                format!(r#"{{"status":"{status}","conclusion":{concl_json},"name":"{name}"}}"#)
            })
            .collect();
        format!("[{}]", items.join(","))
    }

    fn monitor(gh: ScriptedGh, clock: ManualClock, config: WatchConfig) -> Monitor<ScriptedGh> {
        let sleeper = AdvancingSleeper(clock.0.clone());
        Monitor::new(gh, Box::new(clock))
            .with_config(config)
            .with_sleeper(Box::new(sleeper))
    }

    // pr_status

    #[tokio::test]
    async fn pr_status_maps_gh_state_and_draft() {
        let cases = [
            (r#"{"state":"OPEN","isDraft":false}"#, PrStatus::Open),
            (r#"{"state":"OPEN","isDraft":true}"#, PrStatus::Draft),
            (r#"{"state":"MERGED","isDraft":false}"#, PrStatus::Merged),
            (r#"{"state":"CLOSED","isDraft":false}"#, PrStatus::Closed),
        ];
        for (json, expected) in cases {
            let gh = ScriptedGh::new(vec![GhOutput::ok(json)]);
            let mon = Monitor::new(gh, Box::new(ManualClock::new()));
            assert_eq!(mon.pr_status("1").await.unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn pr_status_reconciles_live_every_call() {
        // Two different states from two calls prove there is no cached value:
        // the second call reflects the changed state.
        let gh = ScriptedGh::new(vec![
            GhOutput::ok(r#"{"state":"OPEN","isDraft":true}"#),
            GhOutput::ok(r#"{"state":"MERGED","isDraft":false}"#),
        ]);
        let mon = Monitor::new(gh, Box::new(ManualClock::new()));
        assert_eq!(mon.pr_status("1").await.unwrap(), PrStatus::Draft);
        assert_eq!(mon.pr_status("1").await.unwrap(), PrStatus::Merged);
    }

    #[tokio::test]
    async fn pr_status_surfaces_a_gh_command_failure() {
        let gh = ScriptedGh::new(vec![GhOutput::failed(1, "not found")]);
        let mon = Monitor::new(gh, Box::new(ManualClock::new()));
        assert!(matches!(
            mon.pr_status("999").await.unwrap_err(),
            MonitorError::Command { .. }
        ));
    }

    #[tokio::test]
    async fn pr_status_rejects_a_reference_gh_would_read_as_a_flag() {
        // `-R owner/repo` spliced into `gh pr view <pr>` retargets the query
        // at another repository; `--jq` breaks the JSON parse. The reference
        // is rejected before gh is invoked at all.
        let gh = ScriptedGh::new(vec![GhOutput::ok(r#"{"state":"OPEN","isDraft":false}"#)]);
        let calls = gh.calls.clone();
        let mon = Monitor::new(gh, Box::new(ManualClock::new()));
        for hostile in ["-R other/repo", "--jq", "-"] {
            match mon.pr_status(hostile).await {
                Err(MonitorError::InvalidPrReference { reference }) => {
                    assert_eq!(reference, hostile);
                }
                other => panic!("expected a rejected reference for {hostile}, got {other:?}"),
            }
        }
        assert!(
            calls.lock().unwrap().is_empty(),
            "a hostile reference never reaches gh"
        );
        // A normal reference (what the fleet actually passes) still works.
        assert_eq!(mon.pr_status("fleet/t1-abc").await.unwrap(), PrStatus::Open);
    }

    // per-invocation bound (SystemGhCli's real spawn path)

    #[cfg(unix)]
    #[tokio::test]
    async fn a_hung_child_is_bounded_by_the_invocation_timeout() {
        // watch_ci's caps are measured between polls, so one hung child must
        // not park the watch inside a single `output()` await. Exercises the
        // real spawn path with `sleep` standing in for a black-holed `gh`.
        let mut command = tokio::process::Command::new("sleep");
        command.arg("5");
        let started = std::time::Instant::now();
        let err = run_with_timeout(command, "run list".to_string(), 50)
            .await
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5));
        match err {
            GhError::TimedOut {
                command,
                timeout_ms,
            } => {
                assert_eq!(command, "run list");
                assert_eq!(timeout_ms, 50);
            }
            other => panic!("expected TimedOut, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_prompt_child_completes_under_the_invocation_timeout() {
        let command = tokio::process::Command::new("true");
        let out = run_with_timeout(command, "true".to_string(), GH_INVOCATION_TIMEOUT_MS)
            .await
            .unwrap();
        assert!(out.success);
    }

    // snapshot classification

    #[test]
    fn parse_run_list_reads_status_and_conclusion() {
        let snap = parse_run_list(&run_json(&[
            ("completed", "success", "CI"),
            ("in_progress", "", "E2E"),
        ]))
        .unwrap();
        assert_eq!(snap.runs.len(), 2);
        assert_eq!(snap.runs[0].status, CiRunStatus::Completed);
        assert_eq!(snap.runs[0].conclusion, Some(CiConclusion::Success));
        assert_eq!(snap.runs[1].status, CiRunStatus::InProgress);
        assert_eq!(snap.runs[1].conclusion, None);
    }

    #[test]
    fn empty_output_parses_to_a_pending_snapshot() {
        assert_eq!(parse_run_list("").unwrap().phase(), WatchPhase::Pending);
        assert_eq!(parse_run_list("[]").unwrap().phase(), WatchPhase::Pending);
    }

    #[test]
    fn phase_and_overall_conclusion_classify_correctly() {
        let progressing = parse_run_list(&run_json(&[
            ("completed", "success", "a"),
            ("in_progress", "", "b"),
        ]))
        .unwrap();
        assert_eq!(progressing.phase(), WatchPhase::Progressing);
        assert!(progressing.any_in_progress());

        let all_pass = parse_run_list(&run_json(&[
            ("completed", "success", "a"),
            ("completed", "skipped", "b"),
        ]))
        .unwrap();
        assert_eq!(all_pass.phase(), WatchPhase::AllCompleted);
        assert_eq!(all_pass.overall_conclusion(), CiConclusion::Success);

        let one_failed = parse_run_list(&run_json(&[
            ("completed", "success", "a"),
            ("completed", "failure", "b"),
        ]))
        .unwrap();
        assert_eq!(one_failed.overall_conclusion(), CiConclusion::Failure);
    }

    // decide(): the L-E4 cap arithmetic

    #[test]
    fn decide_completes_immediately_when_all_runs_completed() {
        let cfg = WatchConfig::default();
        let d = decide(
            WatchPhase::AllCompleted,
            0,
            0,
            CiConclusion::Success,
            "done",
            &cfg,
        );
        assert_eq!(
            d,
            WatchDecision::Completed {
                conclusion: CiConclusion::Success
            }
        );
    }

    #[test]
    fn decide_continues_while_progressing_under_all_caps() {
        let cfg = WatchConfig::default();
        assert_eq!(
            decide(
                WatchPhase::Progressing,
                60_000,
                0,
                CiConclusion::Success,
                "x",
                &cfg
            ),
            WatchDecision::Continue
        );
    }

    #[test]
    fn decide_hits_the_cumulative_cap_even_while_progressing() {
        let cfg = WatchConfig::default();
        // Actively progressing (since_progress = 0) but past the 2h wall cap.
        let d = decide(
            WatchPhase::Progressing,
            cfg.max_total_ms,
            0,
            CiConclusion::Success,
            "still building",
            &cfg,
        );
        assert_eq!(
            d,
            WatchDecision::TimedOut {
                reason: TimeoutReason::CumulativeCap,
                last_observed: "still building".to_string()
            }
        );
    }

    #[test]
    fn decide_stalls_when_progress_stops_short_of_completion() {
        let cfg = WatchConfig::default();
        let d = decide(
            WatchPhase::Progressing,
            cfg.stall_timeout_ms + 1,
            cfg.stall_timeout_ms,
            CiConclusion::Success,
            "1 run(s): 0 completed, 0 in progress, 1 queued",
            &cfg,
        );
        assert_eq!(
            d,
            WatchDecision::TimedOut {
                reason: TimeoutReason::Stalled,
                last_observed: "1 run(s): 0 completed, 0 in progress, 1 queued".to_string()
            }
        );
    }

    #[test]
    fn decide_times_out_when_no_runs_ever_start() {
        let cfg = WatchConfig::default();
        let d = decide(
            WatchPhase::Pending,
            cfg.startup_grace_ms,
            0,
            CiConclusion::Success,
            "no CI runs observed",
            &cfg,
        );
        assert_eq!(
            d,
            WatchDecision::TimedOut {
                reason: TimeoutReason::NoRunsStarted,
                last_observed: "no CI runs observed".to_string()
            }
        );
    }

    // watch_ci loop (fake clock + advancing sleeper)

    #[tokio::test]
    async fn watch_ci_completes_when_runs_finish() {
        let gh = ScriptedGh::new(vec![
            GhOutput::ok(run_json(&[("in_progress", "", "CI")])),
            GhOutput::ok(run_json(&[("completed", "success", "CI")])),
        ]);
        let cfg = WatchConfig {
            poll_interval_ms: 1_000,
            max_total_ms: 100_000,
            stall_timeout_ms: 50_000,
            startup_grace_ms: 50_000,
            ..WatchConfig::default()
        };
        let mon = monitor(gh, ManualClock::new(), cfg);
        let outcome = mon.watch_ci("feat/x").await.unwrap();
        assert_eq!(
            outcome,
            CiWatchOutcome::Completed {
                conclusion: CiConclusion::Success,
                summary: "1 run(s): 1 completed, 0 in progress, 0 queued".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn watch_ci_reports_failure_conclusion() {
        let gh = ScriptedGh::new(vec![GhOutput::ok(run_json(&[(
            "completed",
            "failure",
            "CI",
        )]))]);
        let cfg = WatchConfig {
            poll_interval_ms: 1_000,
            max_total_ms: 100_000,
            stall_timeout_ms: 50_000,
            startup_grace_ms: 50_000,
            ..WatchConfig::default()
        };
        let mon = monitor(gh, ManualClock::new(), cfg);
        assert!(matches!(
            mon.watch_ci("feat/x").await.unwrap(),
            CiWatchOutcome::Completed {
                conclusion: CiConclusion::Failure,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn watch_ci_hits_the_cumulative_cap_for_a_perpetually_running_job() {
        // Always in_progress → fresh every poll (never stalls); only the
        // cumulative cap can stop it.
        let gh = ScriptedGh::new(vec![GhOutput::ok(run_json(&[("in_progress", "", "CI")]))]);
        let cfg = WatchConfig {
            poll_interval_ms: 1_000,
            max_total_ms: 5_000,
            stall_timeout_ms: 1_000_000,
            startup_grace_ms: 1_000_000,
            ..WatchConfig::default()
        };
        let mon = monitor(gh, ManualClock::new(), cfg);
        let outcome = mon.watch_ci("feat/x").await.unwrap();
        match outcome {
            CiWatchOutcome::TimedOut {
                reason: TimeoutReason::CumulativeCap,
                waited_ms,
                ..
            } => assert!(waited_ms >= 5_000),
            other => panic!("expected a cumulative-cap timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn watch_ci_stalls_when_a_run_is_stuck_without_progressing() {
        // Always the SAME queued run (no change, nothing in progress) → the
        // stall window, not the cumulative cap, ends the wait.
        let gh = ScriptedGh::new(vec![GhOutput::ok(run_json(&[("queued", "", "CI")]))]);
        let cfg = WatchConfig {
            poll_interval_ms: 1_000,
            max_total_ms: 1_000_000,
            stall_timeout_ms: 5_000,
            startup_grace_ms: 1_000_000,
            ..WatchConfig::default()
        };
        let mon = monitor(gh, ManualClock::new(), cfg);
        match mon.watch_ci("feat/x").await.unwrap() {
            CiWatchOutcome::TimedOut {
                reason: TimeoutReason::Stalled,
                last_observed,
                ..
            } => assert!(last_observed.contains("queued")),
            other => panic!("expected a stall timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn watch_ci_times_out_when_ci_never_starts() {
        let gh = ScriptedGh::new(vec![GhOutput::ok("[]")]);
        let cfg = WatchConfig {
            poll_interval_ms: 1_000,
            max_total_ms: 1_000_000,
            stall_timeout_ms: 1_000_000,
            startup_grace_ms: 5_000,
            ..WatchConfig::default()
        };
        let mon = monitor(gh, ManualClock::new(), cfg);
        assert!(matches!(
            mon.watch_ci("feat/x").await.unwrap(),
            CiWatchOutcome::TimedOut {
                reason: TimeoutReason::NoRunsStarted,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn watch_ci_rides_out_a_transient_poll_failure() {
        // A 403 secondary rate limit (a non-zero gh exit) on the first poll
        // must not throw away a wait the caps hold for hours — the watch
        // sleeps one interval and polls again.
        let gh = ScriptedGh::new(vec![
            GhOutput::failed(1, "HTTP 403: API rate limit exceeded"),
            GhOutput::ok(run_json(&[("completed", "success", "CI")])),
        ]);
        let cfg = WatchConfig {
            poll_interval_ms: 1_000,
            max_total_ms: 100_000,
            stall_timeout_ms: 50_000,
            startup_grace_ms: 50_000,
            ..WatchConfig::default()
        };
        let mon = monitor(gh, ManualClock::new(), cfg);
        assert!(matches!(
            mon.watch_ci("feat/x").await.unwrap(),
            CiWatchOutcome::Completed {
                conclusion: CiConclusion::Success,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn watch_ci_gives_up_after_too_many_consecutive_poll_failures() {
        // Tolerance is bounded: a permanently broken gh still ends the watch
        // with the error rather than polling forever.
        let gh = ScriptedGh::new(vec![GhOutput::failed(1, "gh: not authenticated")]);
        let cfg = WatchConfig {
            poll_interval_ms: 1_000,
            max_total_ms: 1_000_000,
            stall_timeout_ms: 1_000_000,
            startup_grace_ms: 1_000_000,
            max_consecutive_poll_errors: 2,
            ..WatchConfig::default()
        };
        let calls = gh.calls.clone();
        let mon = monitor(gh, ManualClock::new(), cfg);
        assert!(matches!(
            mon.watch_ci("feat/x").await.unwrap_err(),
            MonitorError::Command { .. }
        ));
        // Two tolerated failures, then the third ends it.
        assert_eq!(calls.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn watch_ci_error_tolerance_still_respects_the_cumulative_cap() {
        // An unreachable gh must not let the error path outlive the wall cap:
        // each failed poll sleeps one interval, and the cap ends the watch
        // well before the error budget is spent.
        let gh = ScriptedGh::new(vec![GhOutput::failed(1, "network is unreachable")]);
        let cfg = WatchConfig {
            poll_interval_ms: 1_000,
            max_total_ms: 2_000,
            stall_timeout_ms: 1_000_000,
            startup_grace_ms: 1_000_000,
            max_consecutive_poll_errors: 1_000,
            ..WatchConfig::default()
        };
        let calls = gh.calls.clone();
        let mon = monitor(gh, ManualClock::new(), cfg);
        assert!(mon.watch_ci("feat/x").await.is_err());
        assert!(
            calls.lock().unwrap().len() <= 3,
            "the cap, not the error budget, ended the watch"
        );
    }

    #[tokio::test]
    async fn poll_ci_rejects_a_ref_gh_would_read_as_a_flag() {
        // The same screening pr_status has: `--jq` spliced after `--branch`
        // would change the output shape out from under the JSON parse.
        let gh = ScriptedGh::new(vec![GhOutput::ok("[]")]);
        let calls = gh.calls.clone();
        let mon = Monitor::new(gh, Box::new(ManualClock::new()));
        for hostile in ["--jq", "-R", "-"] {
            match mon.poll_ci(hostile).await {
                Err(MonitorError::InvalidPrReference { reference }) => {
                    assert_eq!(reference, hostile);
                }
                other => panic!("expected a rejected ref for {hostile}, got {other:?}"),
            }
        }
        assert!(
            calls.lock().unwrap().is_empty(),
            "a hostile ref never reaches gh"
        );
        // The branch the fleet actually passes still polls.
        assert!(mon.poll_ci("fleet/t1-abc").await.is_ok());
    }

    #[tokio::test]
    async fn poll_ci_asks_gh_for_the_configured_run_limit() {
        // The truncation point is config, not a magic number in the argv.
        let gh = ScriptedGh::new(vec![GhOutput::ok(run_json(&[(
            "completed",
            "success",
            "CI",
        )]))]);
        let calls = gh.calls.clone();
        let mon = Monitor::new(gh, Box::new(ManualClock::new())).with_config(WatchConfig {
            run_list_limit: 7,
            ..WatchConfig::default()
        });
        mon.poll_ci("feat/x").await.unwrap();
        let calls = calls.lock().unwrap();
        let argv = &calls[0];
        let limit = argv
            .iter()
            .position(|a| a == "--limit")
            .map(|i| argv[i + 1].as_str());
        assert_eq!(limit, Some("7"));
    }

    // emit-shape helpers

    #[test]
    fn commit_and_pr_event_helpers_build_protocol_events() {
        let commit = CommitRecord {
            sha: "abc".into(),
            branch: "fleet/t".into(),
            task_id: "t".into(),
            message: "feat: x".into(),
            timestamp_ms: 1,
        };
        match commit_event(&commit) {
            AgentEvent::Commit { sha, message } => {
                assert_eq!(sha, "abc");
                assert_eq!(message, "feat: x");
            }
            other => panic!("expected a commit event, got {other:?}"),
        }
        match pr_event("https://github.com/x/y/pull/1", PrStatus::Open) {
            AgentEvent::Pr {
                url,
                status,
                number,
                ci,
            } => {
                assert_eq!(url, "https://github.com/x/y/pull/1");
                assert_eq!(status, PrStatus::Open);
                assert_eq!(number, Some(1));
                assert_eq!(ci, None, "un-polled ci must be absent, never passing");
            }
            other => panic!("expected a pr event, got {other:?}"),
        }
    }

    #[test]
    fn pr_number_parses_from_pull_urls_only() {
        assert_eq!(
            parse_pr_number("https://github.com/macanderson/stella/pull/183"),
            Some(183)
        );
        assert_eq!(
            parse_pr_number("https://github.com/macanderson/stella/pull/183/"),
            Some(183)
        );
        assert_eq!(
            parse_pr_number("https://github.com/macanderson/stella/issues/183"),
            None
        );
        assert_eq!(parse_pr_number("not a url"), None);
        assert_eq!(
            parse_pr_number("https://github.com/macanderson/stella/pull/abc"),
            None
        );
    }
}
