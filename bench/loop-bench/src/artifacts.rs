//! What harbor leaves in a trial directory *besides* the event stream (#1299).
//!
//! The harness used to read two files per trial —
//! `agent/stella-events.jsonl` and `verifier/reward.txt` — and harbor writes
//! more than that. Two of the three holes this module closes come from the
//! same omission:
//!
//! * **A crash was invisible.** When the agent raises, harbor records an
//!   [`ExceptionInfo`] on the trial's `result.json` (and its message alone in
//!   `exception.txt`). A trial that died partway through therefore carried an
//!   explicit "this did not finish" marker that nothing read, so it reported
//!   as `ran (unsolved)` — indistinguishable from a turn that tried and was
//!   wrong. An infrastructure failure reading as a capability failure sends
//!   the operator hunting a reasoning bug that does not exist.
//! * **A multi-step trial read as empty.** Harbor relocates each step's logs
//!   into `steps/<name>/agent/` and *removes* the now-empty trial-root
//!   `agent/` (`TrialPaths.cleanup_empty_mount_dirs`), so every multi-step
//!   trial reported "no event stream" — a whole dataset shape scoring as
//!   infrastructure failure.
//!
//! Everything here is a *read* of harbor's own record. The verdict this feeds
//! is an observation ("harbor recorded an exception"), never an inference
//! ("the stream looks short, it probably died") — the distinction the
//! adapter's own `exit_cause.py` draws between an answered question and an
//! unobserved one, and for the same reason: a guess presented as a finding
//! erases the real cause from the record.
//!
//! [`ExceptionInfo`]: https://github.com/laude-institute/harbor
//! (`harbor.models.trial.result.ExceptionInfo`)

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The adapter's event stream, under the trial's `agent/` mount.
pub const EVENTS_NAME: &str = "stella-events.jsonl";

/// Harbor's own record of the trial (`harbor.models.trial.result.TrialResult`):
/// `task_name`, `exception_info`, `step_results`, and — for a multi-step
/// trial — the aggregated `verifier_result`.
///
/// `TrialPaths.result_path` names `result.json` while the class docstring
/// directly above it says `results.json`. Harbor's code wins, but both are
/// tried: reading the wrong one costs the crash signal entirely, and the cost
/// of trying two names is one `stat`.
const RESULT_NAMES: [&str; 2] = ["result.json", "results.json"];

/// The exception message alone, at the trial root
/// (`TrialPaths.exception_message_path`). Only read when `result.json` is
/// missing or unparseable — the result file carries the same message *with*
/// its exception type, which is the half that says whether a human should
/// look at Stella or at Docker.
const EXCEPTION_NAME: &str = "exception.txt";

/// The adapter's post-mortem for a SIGKILL'd trial (`exit_cause.py`, #1178):
/// OOM-kill vs an external teardown, decided from container evidence *while
/// the container still existed*. It answers the question a bare exit 137
/// cannot, so when it is present it is worth a line of its own.
const EXIT_CAUSE_NAME: &str = "stella-exit-cause.json";

/// One event stream, and which step produced it.
#[derive(Debug, Clone)]
pub struct StepStream {
    /// `None` for a single-step trial; harbor's step name otherwise.
    pub step: Option<String>,
    /// The stream text, or the message that stands in for it when the file
    /// could not be read. An unreadable stream is reported, never skipped.
    pub raw: Result<String, String>,
}

/// Everything one trial directory says about itself.
#[derive(Debug, Default, Clone)]
pub struct TrialArtifacts {
    /// One entry per event stream found, in the order the steps ran. Always
    /// non-empty: a trial with no stream at all yields a single entry whose
    /// `raw` is the error, so the trial still reaches the table.
    pub streams: Vec<StepStream>,
    /// The verifier's reward, or `None` when the trial never got that far.
    pub reward: Option<f64>,
    /// Set when a reward exists but cannot be used — a corrupt reward must
    /// not silently downgrade a passing task to "never verified".
    pub reward_problem: Option<String>,
    /// `"<ExceptionType>: <message>"` when harbor recorded the trial as having
    /// raised. This is the crash signal: an observation harbor made, not an
    /// inference from the stream's shape.
    pub crash: Option<String>,
    /// The adapter's SIGKILL post-mortem verdict, when it wrote one.
    pub exit_cause: Option<String>,
    /// Harbor's own `task_name` for the trial. Authoritative where the
    /// directory name is not: harbor truncates the task name to 32 characters
    /// when naming the directory.
    pub task_name: Option<String>,
}

/// Read one trial directory: its stream(s), its reward, and whether harbor
/// recorded it as having crashed.
#[must_use]
pub fn read_trial(trial_dir: &Path) -> TrialArtifacts {
    let result = read_result(trial_dir);
    let agent_dirs = agent_dirs(trial_dir, result.as_ref());
    let (reward, reward_problem) = read_reward(trial_dir, result.as_ref());
    TrialArtifacts {
        crash: read_crash(trial_dir, result.as_ref()),
        exit_cause: read_exit_cause(&agent_dirs),
        task_name: result
            .as_ref()
            .and_then(|v| v.get("task_name"))
            .and_then(Value::as_str)
            .map(str::to_string),
        streams: read_streams(&agent_dirs),
        reward,
        reward_problem,
    }
}

/// The agent log directories to read, in step order.
///
/// A single-step trial has exactly one, `agent/`. A multi-step trial has one
/// per step under `steps/<name>/agent/`, and its root `agent/` has been
/// removed. When neither exists the root is returned anyway, so the caller
/// reports the *expected* path in its "no event stream" message rather than a
/// speculative per-step one.
fn agent_dirs(trial_dir: &Path, result: Option<&Value>) -> Vec<(Option<String>, PathBuf)> {
    let root = trial_dir.join("agent");
    if root.join(EVENTS_NAME).is_file() {
        return vec![(None, root)];
    }
    let mut steps: Vec<(Option<String>, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(trial_dir.join("steps")) {
        for entry in entries.flatten() {
            let dir = entry.path().join("agent");
            if dir.join(EVENTS_NAME).is_file() {
                steps.push((Some(entry.file_name().to_string_lossy().into_owned()), dir));
            }
        }
    }
    if steps.is_empty() {
        return vec![(None, root)];
    }
    // Order matters: the fold takes the *last* step's terminal event as the
    // trial's, so a shuffled sequence would judge the wrong step's ending.
    // `result.json` lists `step_results` in execution order, which is the only
    // authoritative answer; a name sort is the fallback for a trial whose
    // result file never landed (which is exactly the crashed case).
    match step_order(result) {
        Some(order) => steps.sort_by_key(|(name, _)| {
            let position = name
                .as_ref()
                .and_then(|n| order.iter().position(|s| s == n));
            // Steps harbor did not name sort after the ones it did, by name,
            // rather than silently leading.
            (position.unwrap_or(usize::MAX), name.clone())
        }),
        None => steps.sort_by(|a, b| a.0.cmp(&b.0)),
    }
    steps
}

/// The step names in execution order, from harbor's own `step_results`.
fn step_order(result: Option<&Value>) -> Option<Vec<String>> {
    let steps = result?.get("step_results")?.as_array()?;
    let order: Vec<String> = steps
        .iter()
        .filter_map(|s| s.get("step_name").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    (!order.is_empty()).then_some(order)
}

/// Read every stream named by [`agent_dirs`].
fn read_streams(agent_dirs: &[(Option<String>, PathBuf)]) -> Vec<StepStream> {
    agent_dirs
        .iter()
        .map(|(step, dir)| {
            let path = dir.join(EVENTS_NAME);
            StepStream {
                step: step.clone(),
                // The message is unchanged from when this was inline in
                // `analyze`: a trial with no readable stream is the worst
                // outcome, and its row has to name the path that was missing.
                raw: std::fs::read_to_string(&path)
                    .map_err(|err| format!("no event stream ({}): {err}", path.display())),
            }
        })
        .collect()
}

/// Read the verifier's reward, and any problem worth putting on the row.
///
/// Three sources, in descending directness:
///
/// 1. `verifier/reward.txt` — what the terminal-bench mapper writes for a
///    single-step trial.
/// 2. `result.json`'s `verifier_result.rewards.reward` — harbor's own
///    aggregate. This is the *only* reward a multi-step trial has: harbor
///    folds the per-step rewards by the task's `multi_step_reward_strategy`
///    (last, or mean) and there is no trial-root `verifier/` at all. Taking
///    harbor's aggregate rather than inventing a fold here means the harness
///    and the dataset agree on what the task's score was.
/// 3. Nothing — a quiet `None`. The trial never reached the verifier, which
///    is not the same as scoring zero.
///
/// Harbor also accepts `verifier/reward.json`; it is deliberately not read,
/// because the terminal-bench mapper writes the text form and a dataset that
/// writes the JSON form should report `None` (under-crediting) rather than
/// have this file guess at its key set.
#[must_use]
pub fn read_reward(trial_dir: &Path, result: Option<&Value>) -> (Option<f64>, Option<String>) {
    let path = trial_dir.join("verifier").join("reward.txt");
    match std::fs::read_to_string(&path) {
        Ok(content) => match content.trim().parse::<f64>() {
            Ok(reward) => (Some(reward), None),
            Err(_) => (
                None,
                Some(format!(
                    "unreadable reward.txt: unparsable `{}`",
                    crate::truncate(content.trim(), 20)
                )),
            ),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (aggregated_reward(result), None),
        Err(err) => (None, Some(format!("unreadable reward.txt: {err}"))),
    }
}

/// Harbor's aggregated reward off `result.json`, for a trial with no
/// `verifier/reward.txt` of its own.
fn aggregated_reward(result: Option<&Value>) -> Option<f64> {
    result?
        .get("verifier_result")?
        .get("rewards")?
        .get("reward")?
        .as_f64()
}

/// Harbor's record of the trial, if it wrote one.
fn read_result(trial_dir: &Path) -> Option<Value> {
    RESULT_NAMES
        .iter()
        .filter_map(|name| std::fs::read_to_string(trial_dir.join(name)).ok())
        .find_map(|raw| serde_json::from_str::<Value>(&raw).ok())
}

/// Whether harbor recorded this trial as having raised, and with what.
///
/// A multi-step trial can raise in one step and still write a clean
/// trial-level record, so the per-step results are checked too. The *first*
/// failing step is reported rather than the last: it is the one that explains
/// everything after it.
fn read_crash(trial_dir: &Path, result: Option<&Value>) -> Option<String> {
    if let Some(crash) = result.and_then(|v| exception_label(v.get("exception_info"))) {
        return Some(crash);
    }
    if let Some(steps) = result
        .and_then(|v| v.get("step_results"))
        .and_then(Value::as_array)
        && let Some(crash) = steps.iter().find_map(|step| {
            let label = exception_label(step.get("exception_info"))?;
            let name = step.get("step_name").and_then(Value::as_str);
            Some(match name {
                Some(name) => format!("step `{name}` raised {label}"),
                None => label,
            })
        })
    {
        return Some(crash);
    }
    // No result file (or an unparseable one) and an exception message at the
    // root: harbor got far enough to name the failure but not to record it
    // structurally. Worth strictly more than nothing.
    let text = std::fs::read_to_string(trial_dir.join(EXCEPTION_NAME)).ok()?;
    let line = text.lines().find(|line| !line.trim().is_empty())?;
    Some(line.trim().to_string())
}

/// `"<type>: <message>"` from one `exception_info` object, or `None` when the
/// field is absent or JSON `null` — which is what a trial that did *not* raise
/// looks like, and must not be mistaken for a crash with an empty message.
fn exception_label(info: Option<&Value>) -> Option<String> {
    let info = info.filter(|value| !value.is_null())?;
    let kind = info
        .get("exception_type")
        .and_then(Value::as_str)
        .unwrap_or("exception");
    let message = info
        .get("exception_message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    Some(if message.is_empty() {
        kind.to_string()
    } else {
        format!("{kind}: {message}")
    })
}

/// The adapter's SIGKILL post-mortem, when one was written next to a stream.
///
/// Only the verdict and its detail are lifted; the evidence stays in the file
/// for whoever disputes the classification. A trial killed while the container
/// was being torn down and one killed by the OOM killer both exit 137, and
/// which of the two it was decides whether the fix is a memory limit or a
/// timeout — so it is worth its own line on the row.
fn read_exit_cause(agent_dirs: &[(Option<String>, PathBuf)]) -> Option<String> {
    agent_dirs.iter().find_map(|(_, dir)| {
        let raw = std::fs::read_to_string(dir.join(EXIT_CAUSE_NAME)).ok()?;
        let value: Value = serde_json::from_str(&raw).ok()?;
        let verdict = value.get("verdict").and_then(Value::as_str)?;
        let detail = value.get("detail").and_then(Value::as_str).unwrap_or("");
        Some(if detail.is_empty() {
            verdict.to_string()
        } else {
            format!("{verdict} — {detail}")
        })
    })
}
