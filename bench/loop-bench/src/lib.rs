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
//! | `NOT-RUN` | the task was requested but harbor never produced a trial dir | **yes** |
//! | `UNREADABLE` | the stream had lines but not one parsed as an event — schema drift or plumbing, not loop evidence | no |
//! | `STUCK-LOOP` | the engine's own `loop_detected` fired and the task did not pass | **yes** |
//! | `BUDGET-CAP` | the harness's `STELLA_BUDGET` denied the turn — a cost decision, not a loop defect | no |
//! | `SILENT-DEATH` | zero tool calls and no terminal event: the loop vanished | **yes** |
//! | `ZERO-WORK` | zero tool calls with a stated terminal event | **yes** |
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

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

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
    /// non-worker (triage/plan/judge) ones. Retries do not add records: a
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
    /// Lines of the stream that parsed as events.
    pub parsed_lines: u32,
    /// Lines that did not parse. `parsed_lines == 0` with unparsable lines
    /// present is the `UNREADABLE` verdict: schema drift or plumbing, not
    /// loop evidence (#611).
    pub unparsable_lines: u32,
    /// The task was requested this run but harbor produced no trial directory
    /// at all. Synthesized by [`analyze`]; gates red — a task that never
    /// launched must not read as a smaller, healthy run (#611).
    pub not_run: bool,
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

    /// The one-line loop verdict — the thing the reward number hides. See the
    /// crate docs for the ratified vocabulary and precedence.
    #[must_use]
    pub fn loop_verdict(&self) -> &'static str {
        if self.reward == Some(1.0) {
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
        } else {
            "ran (unsolved)"
        }
    }

    /// The loop misbehaved — the red half of the gate. A pass is never
    /// broken; `UNREADABLE` and `BUDGET-CAP` are excluded (not loop
    /// evidence); `NOT-RUN`, `STUCK-LOOP`, and a zero-work non-pass gate red.
    #[must_use]
    pub fn loop_broken(&self) -> bool {
        if self.reward == Some(1.0) {
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

/// Walk `<job_dir>/<task>__<id>/` trials and distill each into a report.
///
/// `requested` is the task set this run resolved. When present it does two
/// things (#611): trial dirs for tasks outside it are skipped — a stale job
/// directory from an earlier run must not contaminate this one's gate — and
/// requested tasks with no trial dir at all are synthesized as `NOT-RUN` rows
/// that count toward the gate, so a task harbor never launched cannot read as
/// a smaller, healthy run. Pass `None` for `--analyze-only`, which reads
/// whatever a finished jobs dir holds.
///
/// One known blind spot, which under-reports rather than inventing a signal:
/// only single-step trials are found. A multi-step dataset relocates its logs
/// to `steps/<name>/agent/` and removes the trial-root `agent/`, so every
/// trial reports as "no event stream".
pub fn analyze(job_dir: &Path, requested: Option<&[String]>) -> Vec<TrialReport> {
    let mut reports = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(job_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            // Trial dirs are `<task>__<trialid>`; skip config.json etc.
            let Some((task, _)) = name.split_once("__") else {
                continue;
            };
            if let Some(wanted) = requested
                && !wanted.iter().any(|t| t == task)
            {
                continue;
            }
            seen.push(task.to_string());
            let events_path = path.join("agent").join("stella-events.jsonl");
            // A trial with no readable stream is the WORST outcome — the agent
            // never started, or died before writing a line — so it must not be
            // dropped from the table. Skipping it made a launch failure look
            // like a clean run with fewer rows, and left the gate green.
            let mut report = match std::fs::read_to_string(&events_path) {
                Ok(raw) => distill_events(task, &raw),
                Err(err) => TrialReport {
                    task: task.to_string(),
                    zero_work: true,
                    last_error: Some(truncate(
                        &format!("no event stream ({}): {err}", events_path.display()),
                        90,
                    )),
                    ..Default::default()
                },
            };
            let (reward, problem) = read_reward(&path);
            report.reward = reward;
            if let Some(problem) = problem {
                report.last_error = Some(match report.last_error.take() {
                    Some(existing) => truncate(&format!("{existing}; {problem}"), 90),
                    None => truncate(&problem, 90),
                });
            }
            reports.push(report);
        }
    }
    if let Some(wanted) = requested {
        for task in wanted {
            if !seen.iter().any(|t| t == task) {
                reports.push(TrialReport {
                    task: task.clone(),
                    not_run: true,
                    ..Default::default()
                });
            }
        }
    }
    reports.sort_by(|a, b| a.task.cmp(&b.task));
    reports
}

/// Read the verifier's reward for one trial. Returns the reward and, when the
/// file exists but cannot be used, a problem description for `last_error` —
/// a corrupt reward must not silently downgrade a passing task to `None`
/// (#611). A *missing* file stays a quiet `None`: the trial simply never
/// reached the verifier.
///
/// Harbor accepts EITHER `verifier/reward.txt` or `verifier/reward.json`
/// (`harbor.models.trial.paths.TrialPaths`); only the text form is read here,
/// because the terminal-bench mapper writes exactly that. A dataset passed via
/// `--dataset` whose verifier emits the JSON form reports `None` for every
/// trial, which reads as "never reached the verifier" — under-crediting, never
/// over-crediting, so the loop gate stays honest.
pub fn read_reward(trial_dir: &Path) -> (Option<f64>, Option<String>) {
    let path = trial_dir.join("verifier").join("reward.txt");
    match std::fs::read_to_string(&path) {
        Ok(content) => match content.trim().parse::<f64>() {
            Ok(reward) => (Some(reward), None),
            Err(_) => (
                None,
                Some(format!(
                    "unreadable reward.txt: unparsable `{}`",
                    truncate(content.trim(), 20)
                )),
            ),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (None, None),
        Err(err) => (None, Some(format!("unreadable reward.txt: {err}"))),
    }
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

/// Width of the rendered table, in columns: 24 (task) + 14 (verdict) + 6 + 6 +
/// 5 + 4 + 4 for the counters + 7 for `$`, plus the nine separator spaces and
/// the six of `reward`. The verdict column is exactly as wide as the longest
/// verdict string, `ran (unsolved)` — an 8-wide column overflowed it and
/// shifted every counter on that row.
const TABLE_WIDTH: usize = 85;

pub fn print_table(reports: &[TrialReport]) {
    println!(
        "\n{:<24} {:>14} {:>6} {:>6} {:>5} {:>4} {:>4} {:>7}  reward",
        "task", "verdict", "calls", "tools", "wr", "ov", "gq", "$"
    );
    println!("{}", "─".repeat(TABLE_WIDTH));
    let mut solved = 0usize;
    let mut loop_broken = 0usize;
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
        if r.reward == Some(1.0) {
            solved += 1;
        }
        if r.loop_broken() {
            loop_broken += 1;
        }
        if r.project_overview_calls > 0 {
            overview_used += 1;
        }
        if r.graph_query_calls > 0 {
            graph_used += 1;
        }
        spend += r.spend_usd;
    }
    let n = reports.len();
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
