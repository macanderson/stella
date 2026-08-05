//! The distillation + reporting library behind the `loop-bench` binary — the
//! report types, the event-stream distiller, and the table renderer, split out
//! (#611) so the testable parts live in a library instead of a binary crate.
//!
//! # The verdict vocabulary — ratified as one contract (#611)
//!
//! A trial's `loop_verdict()` is one of, in precedence order:
//!
//! | Verdict | Meaning | In `loop_broken()`? |
//! |---|---|---|
//! | `solved` | the verifier passed it; reward wins | no |
//! | `NOT-RUN` | the task (or one of its trials) was requested but harbor never produced a trial dir | **yes** |
//! | `UNREADABLE` | the stream had lines but not one parsed as an event — schema drift or plumbing, not loop evidence | no |
//! | `STUCK-LOOP` | the engine's own `loop_detected` fired and the task did not pass | **yes** |
//! | `BUDGET-CAP` | the harness's `STELLA_BUDGET` denied the turn — a cost decision, not a loop defect | no |
//! | `SILENT-DEATH` | zero tool calls and no terminal event: the loop vanished | **yes** |
//! | `ZERO-WORK` | zero tool calls with a stated terminal event | **yes** |
//! | `CRASHED` | work happened, then harbor recorded the trial as having raised | no |
//! | `ran (unsolved)` | did real work, did not pass | no |
//!
//! The informational verdicts (`UNREADABLE`, `BUDGET-CAP`) are deliberately
//! excluded from the gate: both describe a run that is *not evidence about the
//! loop* — one because the stream cannot be read, one because the harness's
//! own cost cap stopped the turn before the loop could show anything. Folding
//! either into red would make the gate fail for reasons no loop fix can
//! address. `STUCK-LOOP` outranks `BUDGET-CAP` because a loop that cycles
//! until the cap trips is exactly the defect this harness exists to catch —
//! the cap firing is a symptom there, not the cause.
//!
//! # `CRASHED`, and why it sits where it does (#1299)
//!
//! A trial that dies partway through used to report `ran (unsolved)` — the
//! same words as a turn that tried the task and got it wrong. An
//! infrastructure failure wearing a capability failure's label sends the
//! operator hunting a reasoning bug that does not exist, and the two are not
//! even the same *kind* of fact: one says the agent was wrong, the other says
//! the agent never finished being anything.
//!
//! It is the **lowest-precedence** verdict above `ran (unsolved)`, which is
//! deliberate in both directions:
//!
//! * A crash never displaces a red verdict. A trial that did nothing and then
//!   died is still `SILENT-DEATH`/`ZERO-WORK` and still gates red — the crash
//!   record is added to its row as the explanation it previously lacked
//!   ("vanished in stage `plan`" now comes with harbor's exception), rather
//!   than replacing a gating verdict with a non-gating one. Naming a failure
//!   better must never make the gate weaker.
//! * `CRASHED` therefore only ever replaces `ran (unsolved)`, which does not
//!   gate either. So it costs the gate nothing and buys the table a true
//!   statement, which is exactly the trade this verdict is for.
//!
//! The signal is harbor's own `exception_info`, not an inference from the
//! stream's shape. A turn that did its work and ended without a clean
//! `complete` may simply have exited on a step cap; treating that as a death
//! would invent crashes. "Harbor recorded that this raised" is an observation,
//! and the harness reports observations.
//!
//! # The second gate: a pass-rate floor (#873)
//!
//! The nightly CI job also wants to notice the day the agent stops *solving*
//! things, which `loop_broken()` says nothing about — every trial can run
//! healthily and pass none of them. [`below_pass_floor`] is that check, kept
//! as a separate predicate with its own exit code so a red run still names
//! which of the two failed. It is off unless a floor is configured, because
//! the pass rate this harness produces is a flash-tier model's under a cap:
//! a floor is only meaningful once a job's own history has established one.

pub mod artifacts;
pub mod compare;
pub mod reconcile;

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::artifacts::{StepStream, TrialArtifacts};
use crate::reconcile::{Reconciliation, Requested, reported_task_name};

/// The per-task loop + context signals, distilled from one trial's event
/// stream and its verifier reward.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct TrialReport {
    /// The Terminal-Bench task name, parsed off the `<task>__<id>` trial dir.
    pub task: String,
    /// The verifier's reward, or `None` when the trial produced no
    /// `verifier/reward.txt` (it never got that far, or is still running).
    /// `None` is NOT zero: an unfinished trial must not read as a failure.
    pub reward: Option<f64>,
    /// `step_usage` events — one per *committed* model call, including the
    /// non-worker (triage/plan/verifier) ones. Retries do not add records: a
    /// retried call still emits a single `step_usage` carrying its own
    /// `retries` count. A call that failed after dispatch emits
    /// `usage_incomplete` instead and is NOT counted, so a turn whose every
    /// call died reports zero model calls.
    pub model_calls: u32,
    /// `tool_start` events. Zero is the signal this harness exists for; see
    /// `zero_work` below.
    pub tool_calls: u32,
    /// Mutating file tools among `tool_calls` — the cheapest proxy for "the
    /// agent actually changed the repo" as opposed to reading around it.
    pub file_writes: u32,
    pub project_overview_calls: u32,
    pub graph_query_calls: u32,
    /// Pipeline stages in first-seen order, de-duplicated: this is the set of
    /// stages the turn reached, not a transition log, so a verify → execute
    /// repair loop appears once. For a zero-work stop the pipeline has not
    /// looped yet, so the last entry is exactly where it died.
    pub stages: Vec<String>,
    /// A terminal event (`complete` or a non-retryable `error`) reached the
    /// stream. Its absence on a zero-work turn is a *silent death* — the loop
    /// stopped with no explanation, the worst failure mode. Its absence on a
    /// turn that DID work is deliberately not flagged (see `silent`), which
    /// also means a crash *mid*-work is invisible to the verdict and reports
    /// as an ordinary `ran (unsolved)`.
    pub terminal_event: bool,
    /// The turn ended without executing a single tool call. Paired with a
    /// non-pass, this is the "chose nothing" death the hardening work targets.
    pub zero_work: bool,
    /// `loop_detected` events from the engine's own loop detector — the turn
    /// was caught cycling. Any non-zero count on a non-pass is a `STUCK-LOOP`
    /// verdict and gates red (#611).
    pub loop_detected: u32,
    /// A `budget_denied` event reached the stream: the harness's own
    /// `STELLA_BUDGET` cap stopped the turn. `BUDGET-CAP` verdict, excluded
    /// from the gate — a cost decision is not a loop defect (#611).
    pub budget_capped: bool,
    /// Sum of `step_usage.cost_usd` — what the trial actually spent, so an
    /// operator can tell whether the per-task cap was hit when interpreting a
    /// verdict (#611).
    pub spend_usd: f64,
    /// Sum of `step_usage.input_tokens + output_tokens` across the trial's
    /// committed model calls. Not shown in the single-run table — its columns
    /// are for loop health, and a token count says nothing about that — but it
    /// is a first-class metric of an A/B (#876), where "the candidate wins by
    /// reading four times as much context" is a result the operator must see.
    pub tokens: u64,
    /// Sum of `step_usage.retries`. A retried call still emits one
    /// `step_usage`, so this is the retry *count*, not a second call tally.
    pub retries: u32,
    /// Lines of the stream that parsed as events.
    pub parsed_lines: u32,
    /// Lines that did not parse. `parsed_lines == 0` with unparsable lines
    /// present is the `UNREADABLE` verdict: schema drift or plumbing, not
    /// loop evidence (#611).
    pub unparsable_lines: u32,
    /// The task was requested this run but harbor produced no trial directory
    /// for it. Synthesized by [`analyze`]; gates red — a task that never
    /// launched must not read as a smaller, healthy run (#611). With
    /// `--trials N` this covers a *partial* launch too: a task that produced
    /// three of five trial dirs gets two `NOT-RUN` rows, so the denominator
    /// is the sample that was asked for rather than the one that survived
    /// (#1299).
    pub not_run: bool,
    /// Harbor recorded this trial as having raised — `"<Type>: <message>"`
    /// off its `result.json` (or `exception.txt`). The crash signal: an
    /// observation harbor made, never an inference from the stream (#1299).
    pub crash: Option<String>,
    /// The adapter's SIGKILL post-mortem (#1178) when it wrote one: `oom-kill`
    /// vs `external-teardown` vs `unattributed`, with its detail. Both 137s
    /// look identical from the exit code, and which one it was decides whether
    /// the fix is a memory limit or a timeout.
    pub exit_cause: Option<String>,
    /// Harbor step names this trial's stream(s) came from, in execution order.
    /// Empty for an ordinary single-step trial. Distinct from `stages`, which
    /// is Stella's own pipeline within one turn: these are harbor's steps,
    /// each a separate agent invocation (#1299).
    pub step_names: Vec<String>,
    /// The last `error` event's message, truncated for the table. Retryable
    /// warnings land here too — it is the most recent explanation, not a
    /// verdict.
    pub last_error: Option<String>,
}

impl TrialReport {
    /// A zero-work stop with no terminal event: the loop vanished mid-setup
    /// with no explanation. Only meaningful WITH `zero_work` — a run that did
    /// real work but lacks a clean `complete` (exited via budget/step-cap
    /// after the work landed) is not "silent", it just ended untidily.
    #[must_use]
    pub fn silent(&self) -> bool {
        self.zero_work && !self.terminal_event
    }

    /// The stream had lines but not one parsed as an event — the file holds
    /// something other than an event stream (a startup error printed on
    /// stdout, a truncated upload, the wrong path), so it is evidence about
    /// the plumbing, not the loop.
    #[must_use]
    pub fn unreadable(&self) -> bool {
        self.parsed_lines == 0 && self.unparsable_lines > 0
    }

    /// Harbor recorded this trial as having raised: it did not finish, whatever
    /// its reward says. See the crate docs for why this is read off harbor's
    /// record rather than inferred from the stream (#1299).
    #[must_use]
    pub fn crashed(&self) -> bool {
        self.crash.is_some()
    }

    /// The one-line loop verdict — the thing the reward number hides. See the
    /// crate docs for the ratified vocabulary and precedence.
    #[must_use]
    pub fn loop_verdict(&self) -> &'static str {
        if self.passed() {
            "solved"
        } else if self.not_run {
            "NOT-RUN"
        } else if self.unreadable() {
            "UNREADABLE"
        } else if self.loop_detected > 0 {
            "STUCK-LOOP"
        } else if self.budget_capped {
            "BUDGET-CAP"
        } else if self.zero_work {
            if self.silent() {
                "SILENT-DEATH"
            } else {
                "ZERO-WORK"
            }
        } else if self.crashed() {
            // Below every gating verdict on purpose: a crash explains a
            // zero-work death, it does not excuse it. See the crate docs.
            "CRASHED"
        } else {
            "ran (unsolved)"
        }
    }

    /// The verifier passed this trial. The reward is compared to exactly
    /// `1.0` — the same test `loop_verdict` and `loop_broken` already make —
    /// because Terminal-Bench rewards are binary; a partial-credit dataset
    /// would need this predicate revisited, not silently rounded.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.reward == Some(1.0)
    }

    /// The loop misbehaved — the red half of the gate. A pass is never
    /// broken; `UNREADABLE` and `BUDGET-CAP` are excluded (not loop
    /// evidence); `NOT-RUN`, `STUCK-LOOP`, and a zero-work non-pass gate red.
    #[must_use]
    pub fn loop_broken(&self) -> bool {
        if self.passed() {
            return false;
        }
        if self.not_run {
            return true;
        }
        if self.unreadable() {
            return false;
        }
        if self.loop_detected > 0 {
            return true;
        }
        if self.budget_capped {
            return false;
        }
        self.zero_work
    }
}

/// One run's distilled trials, and the requested-vs-reported check they were
/// taken over.
///
/// The two travel together on purpose: a set of rows is only interpretable
/// next to the question of whether it is all the rows there should be. Keeping
/// the reconciliation in a second place a caller has to remember to consult is
/// how a percentage over a shrunken denominator gets published (#1299).
#[derive(Debug, Default, Clone)]
pub struct Analysis {
    /// The per-trial reports, sorted by task.
    pub trials: Vec<TrialReport>,
    /// Requested vs reported. `None` for `--analyze-only`, which asked for
    /// nothing in particular and so has nothing to reconcile against.
    pub reconciliation: Option<Reconciliation>,
}

impl Analysis {
    /// The run-level counts both gates read.
    #[must_use]
    pub fn tally(&self) -> Tally {
        tally(&self.trials)
    }

    /// No trial artifacts at all — infrastructure, not a loop regression.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.trials.is_empty()
    }
}

/// Walk `<job_dir>/<task>__<id>/` trials and distill each into a report.
///
/// `requested` is what this run asked harbor for. When present it does three
/// things:
///
/// * trial dirs no requested task claims are skipped — a stale job directory
///   from an earlier run must not contaminate this one's gate (#611) — and
///   counted, so the skip is visible rather than silent (#1299);
/// * requested trials with no directory become `NOT-RUN` rows that gate red,
///   at *trial* granularity: three dirs for a five-trial task yields two
///   `NOT-RUN` rows, so the denominator is the sample that was asked for
///   (#611 for the task case, #1299 for the trial case);
/// * every attribution is recorded in the returned [`Reconciliation`], which
///   is what makes a mismatch loud.
///
/// Pass `None` for `--analyze-only`, which reads whatever a finished jobs dir
/// holds and reconciles nothing.
///
/// Multi-step trials are read too (#1299): harbor relocates each step's logs
/// into `steps/<name>/agent/` and removes the trial-root `agent/`, so those
/// streams are found there and folded by [`fold_steps`].
pub fn analyze(job_dir: &Path, requested: Option<Requested<'_>>) -> Analysis {
    let mut trials = Vec::new();
    let mut reconciliation = requested.map(|request| request.reconciliation());
    if let Ok(entries) = std::fs::read_dir(job_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            // Trial dirs are `<task>__<trialid>`; skip config.json etc.
            let Some((dir_prefix, _)) = name.split_once("__") else {
                continue;
            };
            let found = artifacts::read_trial(&path);
            let task = match (requested, reconciliation.as_mut()) {
                (Some(request), Some(record)) => {
                    let (matched, ambiguous) =
                        request.match_dir(dir_prefix, found.task_name.as_deref());
                    let Some(task) = matched else {
                        record.skipped(dir_prefix);
                        continue;
                    };
                    record.saw(task, ambiguous.then_some(dir_prefix));
                    task.to_string()
                }
                _ => reported_task_name(dir_prefix, found.task_name.as_deref()),
            };
            trials.push(distill_trial(&task, &found));
        }
    }
    if let Some(record) = &reconciliation {
        for (task, short) in record.missing() {
            for _ in 0..short {
                trials.push(TrialReport {
                    task: task.to_string(),
                    not_run: true,
                    ..Default::default()
                });
            }
        }
    }
    trials.sort_by(|a, b| a.task.cmp(&b.task));
    Analysis {
        trials,
        reconciliation,
    }
}

/// Fold one trial directory's artifacts into its report.
fn distill_trial(task: &str, found: &TrialArtifacts) -> TrialReport {
    let mut report = distill_streams(task, &found.streams);
    report.reward = found.reward;
    // Truncated to the same width as `last_error`: both print as a `└` line
    // under the row, and a longer one wraps the terminal rather than the cell.
    report.crash = found.crash.as_deref().map(|crash| truncate(crash, 90));
    report.exit_cause = found.exit_cause.as_deref().map(|cause| truncate(cause, 90));
    if let Some(problem) = &found.reward_problem {
        report.last_error = Some(match report.last_error.take() {
            Some(existing) => truncate(&format!("{existing}; {problem}"), 90),
            None => truncate(problem, 90),
        });
    }
    report
}

/// Distill every stream a trial produced — one for a single-step trial, one
/// per harbor step otherwise — and fold them into a single report.
fn distill_streams(task: &str, streams: &[StepStream]) -> TrialReport {
    let steps: Vec<TrialReport> = streams
        .iter()
        .map(|stream| match &stream.raw {
            Ok(raw) => distill_events(task, raw),
            // A trial with no readable stream is the WORST outcome — the agent
            // never started, or died before writing a line — so it must not be
            // dropped from the table. Skipping it made a launch failure look
            // like a clean run with fewer rows, and left the gate green.
            Err(problem) => TrialReport {
                task: task.to_string(),
                zero_work: true,
                last_error: Some(truncate(problem, 90)),
                ..Default::default()
            },
        })
        .collect();
    let mut report = fold_steps(&steps);
    report.step_names = streams
        .iter()
        .filter_map(|stream| stream.step.clone())
        .collect();
    report
}

/// Fold a multi-step trial's per-step reports into one row.
///
/// Counters sum, because the trial did all of it. Two fields do not, and both
/// choices are about not letting an early step vouch for a late one:
///
/// * `terminal_event` is the **last** step's. A trial that completed step one
///   and then vanished in step two ended by vanishing, and an `any` fold would
///   report it as having said goodbye.
/// * `zero_work` is recomputed from the summed tool calls, so it means "this
///   trial never did anything", not "some step didn't" — a step that only
///   reads is a normal part of a multi-step task.
///
/// `budget_capped` *is* an `any`: the cap is per-trial, so once it fires the
/// remaining steps were never given a fair chance either.
#[must_use]
pub fn fold_steps(steps: &[TrialReport]) -> TrialReport {
    if let [single] = steps {
        return single.clone();
    }
    let mut folded = TrialReport {
        task: steps.first().map(|s| s.task.clone()).unwrap_or_default(),
        ..Default::default()
    };
    let mut seen_stage = std::collections::BTreeSet::new();
    for step in steps {
        folded.model_calls = folded.model_calls.saturating_add(step.model_calls);
        folded.tool_calls = folded.tool_calls.saturating_add(step.tool_calls);
        folded.file_writes = folded.file_writes.saturating_add(step.file_writes);
        folded.project_overview_calls = folded
            .project_overview_calls
            .saturating_add(step.project_overview_calls);
        folded.graph_query_calls = folded
            .graph_query_calls
            .saturating_add(step.graph_query_calls);
        folded.loop_detected = folded.loop_detected.saturating_add(step.loop_detected);
        folded.retries = folded.retries.saturating_add(step.retries);
        folded.parsed_lines = folded.parsed_lines.saturating_add(step.parsed_lines);
        folded.unparsable_lines = folded
            .unparsable_lines
            .saturating_add(step.unparsable_lines);
        folded.tokens = folded.tokens.saturating_add(step.tokens);
        folded.spend_usd += step.spend_usd;
        folded.budget_capped |= step.budget_capped;
        folded.terminal_event = step.terminal_event;
        for stage in &step.stages {
            if seen_stage.insert(stage.clone()) {
                folded.stages.push(stage.clone());
            }
        }
        if step.last_error.is_some() {
            folded.last_error.clone_from(&step.last_error);
        }
    }
    folded.zero_work = folded.tool_calls == 0;
    folded
}

/// Read the verifier's reward for one trial. Returns the reward and, when the
/// file exists but cannot be used, a problem description for `last_error` —
/// a corrupt reward must not silently downgrade a passing task to `None`
/// (#611). A *missing* file stays a quiet `None`: the trial simply never
/// reached the verifier.
///
/// See [`artifacts::read_reward`] for the sources and their order; this is the
/// path-only form, for a caller that has not already read `result.json`.
pub fn read_reward(trial_dir: &Path) -> (Option<f64>, Option<String>) {
    let found = artifacts::read_trial(trial_dir);
    (found.reward, found.reward_problem)
}

/// The heart of the tool: turn one event stream into loop + context signals.
pub fn distill_events(task: &str, raw: &str) -> TrialReport {
    let mut r = TrialReport {
        task: task.to_string(),
        ..Default::default()
    };
    let mut seen_stage = std::collections::BTreeSet::new();
    for line in raw.lines() {
        let Ok(ev) = serde_json::from_str::<Value>(line) else {
            r.unparsable_lines += 1;
            continue;
        };
        r.parsed_lines += 1;
        match ev.get("type").and_then(Value::as_str) {
            Some("step_usage") => {
                r.model_calls += 1;
                r.spend_usd += ev.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
                for field in ["input_tokens", "output_tokens"] {
                    let count = ev.get(field).and_then(Value::as_u64).unwrap_or(0);
                    r.tokens = r.tokens.saturating_add(count);
                }
                let retries = ev.get("retries").and_then(Value::as_u64).unwrap_or(0);
                r.retries = r
                    .retries
                    .saturating_add(u32::try_from(retries).unwrap_or(u32::MAX));
            }
            Some("tool_start") => {
                r.tool_calls += 1;
                let name = ev
                    .get("call")
                    .and_then(|c| c.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match name {
                    "project_overview" => r.project_overview_calls += 1,
                    "graph_query" => r.graph_query_calls += 1,
                    // The content-producing file tools in the catalog.
                    // `apply_edits` is the batch form of `edit_file` and was
                    // missing here, so a run that edited exclusively in
                    // batches reported zero writes. `delete_file` is
                    // deliberately excluded: counting removals as writes would
                    // let a destructive loop look productive.
                    "write_file" | "edit_file" | "apply_edits" => r.file_writes += 1,
                    _ => {}
                }
            }
            Some("stage") => {
                if let Some(s) = ev.get("name").and_then(Value::as_str)
                    && seen_stage.insert(s.to_string())
                {
                    r.stages.push(s.to_string());
                }
            }
            Some("loop_detected") => r.loop_detected += 1,
            Some("budget_denied") => r.budget_capped = true,
            Some("complete") => r.terminal_event = true,
            Some("error") => {
                // A non-retryable error is a terminal signal; a retryable one
                // (a warning/degradation) is not.
                if ev.get("retryable").and_then(Value::as_bool) == Some(false) {
                    r.terminal_event = true;
                }
                if let Some(msg) = ev.get("message").and_then(Value::as_str) {
                    r.last_error = Some(truncate(msg, 90));
                }
            }
            _ => {}
        }
    }
    r.zero_work = r.tool_calls == 0;
    // Not one line of the file was an event. That is a different failure from
    // a turn that stayed quiet — the file holds something else entirely (a
    // stella startup error printed on stdout, a truncated upload, the wrong
    // path) — and unnamed it renders as an unexplained silent death, sending
    // the operator hunting the loop for a bug that is in the plumbing.
    if r.unreadable() {
        r.last_error = Some(truncate(
            &format!(
                "{} line(s), none a parseable event — an empty, non-JSON, or \
                 truncated stream",
                r.unparsable_lines
            ),
            90,
        ));
    }
    r
}

/// Clamp `s` to at most `n` characters *including* the ellipsis. The bound has
/// to cover the marker: a plain `take(n) + "…"` yields `n + 1` characters,
/// which overflowed the fixed-width task column and shifted every cell on that
/// row one place right.
pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    if n == 0 {
        return String::new();
    }
    s.chars().take(n - 1).collect::<String>() + "…"
}

/// The run-level counts both gates read, and the header of the JSON report a
/// CI consumer trends over time.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Tally {
    /// Rows in the report — including `NOT-RUN` ones, so the denominator is
    /// the task set that was *asked for*, never the smaller set that ran.
    pub total: usize,
    /// Trials the verifier passed.
    pub solved: usize,
    /// Trials whose loop misbehaved ([`TrialReport::loop_broken`]).
    pub loop_broken: usize,
    /// Trials harbor recorded as having raised (#1299). Counted from the crash
    /// record itself rather than from the `CRASHED` verdict, so it is the
    /// number of trials that did not finish — including the ones whose verdict
    /// a higher-precedence signal claimed (a crash that also died having done
    /// nothing is `SILENT-DEATH`, and is still a crash).
    pub crashed: usize,
}

/// Fold the per-trial reports into the run-level counts.
#[must_use]
pub fn tally(reports: &[TrialReport]) -> Tally {
    Tally {
        total: reports.len(),
        solved: reports.iter().filter(|r| r.passed()).count(),
        loop_broken: reports.iter().filter(|r| r.loop_broken()).count(),
        crashed: reports.iter().filter(|r| r.crashed()).count(),
    }
}

/// The pass-rate floor (#873) — the *second*, opt-in gate, kept deliberately
/// separate from [`TrialReport::loop_broken`].
///
/// This harness gates on loop health because pass rate is dominated by model
/// quality, and the default model here is a flash tier under a $0.20 cap: a
/// floor set by intuition rather than by an observed baseline is red every
/// night for a reason no loop fix can address. So `min_pass` of **0 disables
/// the check entirely**, which is the default, and a nightly job raises it
/// only once its own history says what "normal" is.
///
/// The floor is an absolute count of solved trials rather than a percentage.
/// With a pinned task list the two are the same statement, and the count
/// cannot round: `min_pass = 2` over four tasks is unambiguous where
/// "50%" over five is an argument about 2.5.
#[must_use]
pub fn below_pass_floor(reports: &[TrialReport], min_pass: usize) -> bool {
    min_pass > 0 && tally(reports).solved < min_pass
}

/// The JSON a CI consumer trends over time.
///
/// An object, where a single run used to emit a bare array of trials (#1299).
/// The rows alone are not interpretable: `6 solved` means one thing over eight
/// requested trials and another over six, and the artifact that carries the
/// rows without the reconciliation is exactly the artifact that publishes the
/// flattering number. The tally rides along for the same reason it is printed
/// under the table — so a consumer trends the counts the exit code was decided
/// from, rather than a second tally of its own that can drift from them.
#[derive(Debug, serde::Serialize)]
pub struct JsonReport<'a> {
    /// The per-trial rows, sorted by task. What the array used to hold.
    pub trials: &'a [TrialReport],
    /// The run-level counts both gates read.
    pub tally: Tally,
    /// Requested vs reported; `null` for `--analyze-only`.
    pub reconciliation: Option<&'a Reconciliation>,
}

/// Render an [`Analysis`] for a machine.
#[must_use]
pub fn json_report(analysis: &Analysis) -> JsonReport<'_> {
    JsonReport {
        trials: &analysis.trials,
        tally: analysis.tally(),
        reconciliation: analysis.reconciliation.as_ref(),
    }
}

/// Width of the rendered table, in columns: 24 (task) + 14 (verdict) + 6 + 6 +
/// 5 + 4 + 4 for the counters + 7 for `$`, plus the nine separator spaces and
/// the six of `reward`. The verdict column is exactly as wide as the longest
/// verdict string, `ran (unsolved)` — an 8-wide column overflowed it and
/// shifted every counter on that row.
const TABLE_WIDTH: usize = 85;

pub fn print_analysis(analysis: &Analysis) {
    print_table(&analysis.trials);
    if let Some(record) = &analysis.reconciliation {
        print_reconciliation(record);
    }
}

/// The requested-vs-reported block. Printed only when the two disagree —
/// a run whose accounting is exact should not have to be read to learn that.
///
/// It goes *after* the table because it is a statement about the table: these
/// are the rows that should have been above and were not, and these are the
/// rows above that this run did not ask for.
pub fn print_reconciliation(record: &Reconciliation) {
    let findings = record.findings();
    if findings.is_empty() {
        return;
    }
    println!("\nRECONCILIATION: requested and reported do not match");
    for finding in findings {
        println!("  ✗ {finding}");
    }
    if record.contaminated() {
        println!(
            "  ⚠ every figure above is computed over a task set that is not the one \
             requested — treat the percentages as unsourced until this is resolved."
        );
    }
}

pub fn print_table(reports: &[TrialReport]) {
    println!(
        "\n{:<24} {:>14} {:>6} {:>6} {:>5} {:>4} {:>4} {:>7}  reward",
        "task", "verdict", "calls", "tools", "wr", "ov", "gq", "$"
    );
    println!("{}", "─".repeat(TABLE_WIDTH));
    let mut overview_used = 0usize;
    let mut graph_used = 0usize;
    let mut spend = 0.0f64;
    for r in reports {
        let reward = r.reward.map(|v| format!("{v:.1}")).unwrap_or("-".into());
        println!(
            "{:<24} {:>14} {:>6} {:>6} {:>5} {:>4} {:>4} {:>7}  {}",
            truncate(&r.task, 24),
            r.loop_verdict(),
            r.model_calls,
            r.tool_calls,
            r.file_writes,
            r.project_overview_calls,
            r.graph_query_calls,
            format!("{:.2}", r.spend_usd),
            reward,
        );
        if let Some(err) = &r.last_error
            && r.reward != Some(1.0)
        {
            println!("{:>26}└ {err}", "");
        }
        // A silent death says nothing about itself, so name the last stage it
        // reached — for a zero-work stop the pipeline has not looped, so this
        // is exactly where it vanished. It is the only actionable datum such a
        // run leaves behind, and the reason `stages` is collected at all.
        if r.loop_verdict() == "SILENT-DEATH"
            && let Some(stage) = r.stages.last()
        {
            println!(
                "{:>26}└ vanished in stage `{stage}` (no terminal event)",
                ""
            );
        }
        // The crash line prints even for a row whose verdict is something
        // else, including `solved` (#1299). The verdict column holds one word
        // and precedence already spent it; "harbor recorded this as having
        // raised" is a separate fact, and the row is where an operator will
        // look for it.
        if let Some(crash) = &r.crash {
            println!("{:>26}└ crashed: {crash}", "");
        }
        if let Some(cause) = &r.exit_cause {
            println!("{:>26}└ exit cause: {cause}", "");
        }
        if !r.step_names.is_empty() {
            println!(
                "{:>26}└ folded from {} harbor step(s): {}",
                "",
                r.step_names.len(),
                r.step_names.join(" → ")
            );
        }
        if r.project_overview_calls > 0 {
            overview_used += 1;
        }
        if r.graph_query_calls > 0 {
            graph_used += 1;
        }
        spend += r.spend_usd;
    }
    // One definition of "solved" and "broken" for the table, the JSON header,
    // and both gates — the counts a CI job trends must be the counts the
    // exit code was decided from, not a second tally that can drift from it.
    let Tally {
        total: n,
        solved,
        loop_broken,
        crashed,
    } = tally(reports);
    println!("{}", "─".repeat(TABLE_WIDTH));
    println!(
        "LOOP: {loop_broken}/{n} broken (silent-death, zero-work, stuck-loop, or not-run)   \
         CONTEXT: project_overview {overview_used}/{n}, graph_query {graph_used}/{n}   \
         REWARD: {solved}/{n} solved   SPEND: ${spend:.2}"
    );
    if loop_broken > 0 {
        println!(
            "  ⚠ {loop_broken} task(s) exercised the loop badly — that is a correctness \
             signal independent of the model or the reward."
        );
    }
    // Said separately from the loop count, and in these words, because the
    // conclusion to draw is the opposite one: a crash is the harness or the
    // machine failing, and reading it as the agent being wrong is a week spent
    // tuning prompts against an OOM (#1299).
    if crashed > 0 {
        println!(
            "  ⚠ {crashed}/{n} trial(s) did not finish — harbor recorded an exception. \
             Those are infrastructure outcomes, NOT the model getting the task wrong; \
             the {solved}/{n} pass rate is over a run that partly did not happen."
        );
    }
    // A per-verdict tally, so a run is greppable at a glance.
    let mut verdicts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in reports {
        *verdicts.entry(r.loop_verdict()).or_default() += 1;
    }
    let tally = verdicts
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("  ");
    println!("  verdicts: {tally}");
}

#[cfg(test)]
mod tests;
