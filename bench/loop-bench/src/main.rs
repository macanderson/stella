//! `loop-bench` — an inexpensive turn-loop + context-query correctness harness
//! over Terminal-Bench.
//!
//! The full benchmark (task pass rate) is expensive: Docker containers plus a
//! real model per task. But most of what actually breaks a coding agent is not
//! the model — it is the *loop*: aborting a task having done zero work, dying
//! without saying why, verification that lies, code-intelligence tools that go
//! unused. Those are observable in Stella's own event stream, and they show up
//! on a **cheap** model just as clearly as an expensive one.
//!
//! So this tool runs N Terminal-Bench tasks through Stella — cheap model,
//! budget-capped, in parallel — and reports the correctness signals that the
//! pass-rate number hides:
//!
//! - **Loop health**: did the turn execute real work (tool calls) or abort
//!   before doing anything? did it emit a terminal event, or vanish silently?
//!   how many model calls / tool calls did it take?
//! - **Context-query health**: did `project_overview` / `graph_query` get
//!   used at all, and did the index build?
//! - **Reward**: the Terminal-Bench verifier's pass/fail, for reference.
//!
//! It shells out to the `stella` binary and `harbor` — it has **no dependency
//! on any stella crate**, so it compiles in seconds and never drags the whole
//! workspace into a bench iteration.
//!
//! ```bash
//! # cheapest: 4 tasks on a flash-tier model, $0.20/task cap, 4 concurrent
//! cargo run -p loop-bench -- --n 4
//!
//! # pick tasks + model explicitly
//! cargo run -p loop-bench -- --tasks fix-git,prove-plus-comm -m openrouter/z-ai/glm-5.2
//!
//! # analyze a finished jobs dir without spending anything
//! cargo run -p loop-bench -- --analyze-only --jobs-dir /path/to/jobs --job-name my-run
//! ```
//!
//! Run it from the workspace root: the harbor adapter is put on `PYTHONPATH`
//! by the relative path `ADAPTER_PYTHONPATH`.
//!
//! Exit codes, for a caller that wants to gate on this (no CI job runs it
//! today — it is invoked manually):
//!
//! - `0` — every trial that reported did real work (or passed).
//! - `1` — the loop misbehaved on at least one trial, *or* no trial artifacts
//!   were found at all (the run never produced anything to judge).
//! - `2` — bad invocation: the task list resolved to nothing.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use clap::Parser;
use serde_json::Value;

/// A small, representative default pool — mixed languages and difficulties, the
/// same tasks the loop-hardening work was measured against. `--n` takes the
/// first N; `--tasks` overrides entirely.
const DEFAULT_POOL: &[&str] = &[
    "fix-git",
    "prove-plus-comm",
    "overfull-hbox",
    "cobol-modernization",
    "git-multibranch",
    "polyglot-c-py",
    "kv-store-grpc",
    "nginx-request-logging",
];

/// A flash-tier model: cheapest thing that still exercises the whole loop.
/// Loop and context correctness do not need a strong model — only a running
/// one — so the default optimizes for cost, not pass rate.
const DEFAULT_MODEL: &str = "openrouter/z-ai/glm-4.7-flash";

/// Where the harbor adapter package lives, relative to the workspace root.
/// harbor loads `stella_harbor:StellaAgent` by import path, so this has to be
/// on `PYTHONPATH` — which is also why this tool must be run from the root.
const ADAPTER_PYTHONPATH: &str = "bench/harbor_adapter";

#[derive(Parser, Debug)]
#[command(
    name = "loop-bench",
    about = "Inexpensive turn-loop + context-query correctness over Terminal-Bench"
)]
struct Args {
    /// Number of tasks from the default pool (ignored when --tasks is given).
    #[arg(long, default_value_t = 4)]
    n: usize,

    /// Explicit comma-separated task names (overrides --n / the default pool).
    #[arg(long, value_delimiter = ',')]
    tasks: Vec<String>,

    /// Provider/model to run. Defaults to a flash-tier model for cheapness.
    #[arg(short = 'm', long, default_value = DEFAULT_MODEL)]
    model: String,

    /// Concurrent trials.
    #[arg(long, default_value_t = 4)]
    concurrent: usize,

    /// Per-task USD budget cap (STELLA_BUDGET).
    #[arg(long, default_value_t = 0.20)]
    budget: f64,

    /// Harbor dataset name.
    #[arg(long, default_value = "terminal-bench")]
    dataset: String,

    /// Where harbor writes job results.
    #[arg(long, default_value = "loop-bench-jobs")]
    jobs_dir: String,

    /// Job name (the sub-directory under --jobs-dir).
    #[arg(long, default_value = "loop-bench")]
    job_name: String,

    /// Path to the (linux) stella binary uploaded into each container. Falls
    /// back to $STELLA_BINARY.
    #[arg(long, env = "STELLA_BINARY")]
    stella_binary: Option<String>,

    /// Skip the harbor run; only analyze an existing jobs dir. Free.
    #[arg(long)]
    analyze_only: bool,

    /// Emit the report as JSON (for CI) instead of the human table.
    #[arg(long)]
    json: bool,
}

/// The per-task loop + context signals, distilled from one trial's event
/// stream and its verifier reward.
#[derive(Debug, Default, Clone, serde::Serialize)]
struct TrialReport {
    /// The Terminal-Bench task name, parsed off the `<task>__<id>` trial dir.
    task: String,
    /// The verifier's reward, or `None` when the trial produced no
    /// `verifier/reward.txt` (it never got that far, or is still running).
    /// `None` is NOT zero: an unfinished trial must not read as a failure.
    reward: Option<f64>,
    /// `step_usage` events — one per *committed* model call, including the
    /// non-worker (triage/plan/judge) ones. Retries do not add records: a
    /// retried call still emits a single `step_usage` carrying its own
    /// `retries` count. A call that failed after dispatch emits
    /// `usage_incomplete` instead and is NOT counted, so a turn whose every
    /// call died reports zero model calls.
    model_calls: u32,
    /// `tool_start` events. Zero is the signal this harness exists for; see
    /// `zero_work` below.
    tool_calls: u32,
    /// Mutating file tools among `tool_calls` — the cheapest proxy for "the
    /// agent actually changed the repo" as opposed to reading around it.
    file_writes: u32,
    project_overview_calls: u32,
    graph_query_calls: u32,
    /// Pipeline stages in first-seen order, de-duplicated: this is the set of
    /// stages the turn reached, not a transition log, so a verify → execute
    /// repair loop appears once. For a zero-work stop the pipeline has not
    /// looped yet, so the last entry is exactly where it died.
    stages: Vec<String>,
    /// A terminal event (`complete` or a non-retryable `error`) reached the
    /// stream. Its absence on a zero-work turn is a *silent death* — the loop
    /// stopped with no explanation, the worst failure mode. Its absence on a
    /// turn that DID work is deliberately not flagged (see `silent`), which
    /// also means a crash *mid*-work is invisible to the verdict and reports
    /// as an ordinary `ran (unsolved)`.
    terminal_event: bool,
    /// The turn ended without executing a single tool call. Paired with a
    /// non-pass, this is the "chose nothing" death the hardening work targets.
    zero_work: bool,
    /// The last `error` event's message, truncated for the table. Retryable
    /// warnings land here too — it is the most recent explanation, not a
    /// verdict.
    last_error: Option<String>,
}

impl TrialReport {
    /// A zero-work stop with no terminal event: the loop vanished mid-setup
    /// with no explanation. Only meaningful WITH `zero_work` — a run that did
    /// real work but lacks a clean `complete` (exited via budget/step-cap
    /// after the work landed) is not "silent", it just ended untidily.
    #[must_use]
    fn silent(&self) -> bool {
        self.zero_work && !self.terminal_event
    }

    /// The one-line loop verdict — the thing the reward number hides. Reward
    /// wins (a solved task did the work, by definition); otherwise a run that
    /// executed no tool at all is the "chose nothing" death class, and a run
    /// that did work but did not pass simply ran.
    #[must_use]
    fn loop_verdict(&self) -> &'static str {
        if self.reward == Some(1.0) {
            "solved"
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

    /// The loop misbehaved: it did zero work and did not pass. Silent or
    /// stated, a zero-work non-pass is the failure this harness gates on —
    /// independent of the model and the reward.
    #[must_use]
    fn loop_broken(&self) -> bool {
        self.zero_work && self.reward != Some(1.0)
    }
}

fn main() {
    let args = Args::parse();
    let tasks = resolve_tasks(&args);
    if tasks.is_empty() {
        eprintln!("no tasks to run");
        std::process::exit(2);
    }

    if !args.analyze_only
        && let Err(code) = run_harbor(&args, &tasks)
    {
        eprintln!("harbor run failed (exit {code}); analyzing whatever landed");
    }

    let job_dir = Path::new(&args.jobs_dir).join(&args.job_name);
    let reports = analyze(&job_dir);
    if reports.is_empty() {
        eprintln!(
            "no trial artifacts found under {} — nothing to report",
            job_dir.display()
        );
        std::process::exit(1);
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&reports).unwrap_or_else(|_| "[]".into())
        );
    } else {
        print_table(&reports);
    }

    // Exit non-zero if the LOOP misbehaved (silent death or zero-work), even
    // when some tasks passed — this tool gates on loop health, not pass rate.
    if reports.iter().any(TrialReport::loop_broken) {
        std::process::exit(1);
    }
}

fn resolve_tasks(args: &Args) -> Vec<String> {
    if !args.tasks.is_empty() {
        return args.tasks.clone();
    }
    // `--n 0` would measure nothing, so it floors at one task. Asking for more
    // than the pool holds silently under-measures the run — say so, because a
    // green gate over fewer tasks than the operator asked for is a lie.
    let want = args.n.max(1);
    let pool = DEFAULT_POOL.len();
    if want > pool {
        eprintln!(
            "warning: --n {want} exceeds the {pool} tasks in the default pool; \
             running {pool} (pass --tasks to name more)"
        );
    }
    DEFAULT_POOL
        .iter()
        .take(want)
        .map(|s| s.to_string())
        .collect()
}

/// Build and run the harbor command. Returns Err(exit_code) on a non-zero
/// harbor exit — non-fatal, since partial results are still worth analyzing.
fn run_harbor(args: &Args, tasks: &[String]) -> Result<(), i32> {
    // A non-positive (or NaN) cap denies the very first model call, which
    // would report as a zero-work loop failure for every task — the harness
    // manufacturing the signal it gates on. Warn loudly rather than hand
    // harbor a cap that cannot pass. The threshold is not `<= 0.0` because the
    // cap reaches stella at four decimals (see the `STELLA_BUDGET` env below),
    // so a positive-but-tinier-than-that cap is rounded to `0.0000` and is
    // exactly as unspendable — check the value that will actually be sent, not
    // the one the operator typed.
    if !args.budget.is_finite() || args.budget < 0.000_05 {
        eprintln!(
            "warning: --budget {} reaches stella as {:.4}, which is not a spendable \
             cap; every trial will be denied before its first tool call and report as \
             a loop failure",
            args.budget, args.budget
        );
    }
    // The adapter is imported by path, so it has to exist relative to the cwd.
    // Catch the wrong-directory mistake here instead of inside harbor, where
    // it surfaces as an opaque Python ImportError per trial.
    if !Path::new(ADAPTER_PYTHONPATH).is_dir() {
        eprintln!(
            "warning: {ADAPTER_PYTHONPATH} not found relative to the current directory; \
             run loop-bench from the workspace root or harbor cannot import the adapter"
        );
    }

    let mut cmd = Command::new("harbor");
    cmd.arg("run")
        .args(["--dataset", &args.dataset])
        .args(["--agent-import-path", "stella_harbor:StellaAgent"])
        .args(["-m", &args.model])
        .args(["-k", "1"])
        .args(["-n", &args.concurrent.to_string()])
        .args(["--job-name", &args.job_name])
        .args(["--jobs-dir", &args.jobs_dir])
        .arg("-y");
    for task in tasks {
        cmd.args(["-i", task]);
    }

    cmd.env("STELLA_BUDGET", format!("{:.4}", args.budget));
    match &args.stella_binary {
        Some(path) => {
            cmd.env("STELLA_BINARY", path);
        }
        // The adapter resolves STELLA_BINARY → `stella` on PATH →
        // ./target/release/stella. On a dev machine the PATH hit lands first
        // and is a host (darwin/arm64) build, which the amd64 container cannot
        // execute — so name that step, not just the last one.
        None => eprintln!(
            "warning: no --stella-binary / $STELLA_BINARY set; the adapter will fall \
             back to `stella` on PATH, then target/release/stella — both must be a \
             LINUX amd64 build to run in the task containers"
        ),
    }
    // The adapter is loaded by import path; make it importable.
    let pythonpath = match std::env::var("PYTHONPATH") {
        Ok(existing) if !existing.is_empty() => {
            format!("{ADAPTER_PYTHONPATH}:{existing}")
        }
        _ => ADAPTER_PYTHONPATH.to_string(),
    };
    cmd.env("PYTHONPATH", pythonpath);

    eprintln!(
        "▶ loop-bench: {} task(s) on {} (budget ${:.2}/task, {} concurrent)",
        tasks.len(),
        args.model,
        args.budget,
        args.concurrent
    );
    let status = cmd.status().map_err(|e| {
        eprintln!(
            "could not launch harbor ({e}); is it on PATH? `pip install -e bench/harbor_adapter`"
        );
        127
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(status.code().unwrap_or(1))
    }
}

/// Walk `<job_dir>/<task>__<id>/` trials and distill each into a report.
///
/// Two known blind spots, both of which under-report rather than invent a
/// signal:
///
/// - The requested task list is NOT reconciled against what landed. Harbor
///   filters `-i` names by glob and only errors when *nothing* matched, so a
///   run that asked for eight tasks and matched six reports six healthy rows
///   and exits `0`. Cross-check the row count against `--n` before trusting a
///   green gate.
/// - Only single-step trials are found. A multi-step dataset relocates its
///   logs to `steps/<name>/agent/` and removes the trial-root `agent/`, so
///   every trial reports as "no event stream".
fn analyze(job_dir: &Path) -> Vec<TrialReport> {
    let Ok(entries) = std::fs::read_dir(job_dir) else {
        return Vec::new();
    };
    let mut reports = Vec::new();
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
        let events_path = path.join("agent").join("stella-events.jsonl");
        // A trial with no readable stream is the WORST outcome — the agent
        // never started, or died before writing a line — so it must not be
        // dropped from the table. Skipping it made a launch failure look like
        // a clean run with fewer rows, and left the gate green.
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
        report.reward = read_reward(&path);
        reports.push(report);
    }
    reports.sort_by(|a, b| a.task.cmp(&b.task));
    reports
}

/// Read the verifier's reward for one trial.
///
/// Harbor accepts EITHER `verifier/reward.txt` or `verifier/reward.json`
/// (`harbor.models.trial.paths.TrialPaths`); only the text form is read here,
/// because the terminal-bench mapper writes exactly that. A dataset passed via
/// `--dataset` whose verifier emits the JSON form reports `None` for every
/// trial, which reads as "never reached the verifier" — under-crediting, never
/// over-crediting, so the loop gate stays honest.
fn read_reward(trial_dir: &Path) -> Option<f64> {
    std::fs::read_to_string(trial_dir.join("verifier").join("reward.txt"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// The heart of the tool: turn one event stream into loop + context signals.
fn distill_events(task: &str, raw: &str) -> TrialReport {
    let mut r = TrialReport {
        task: task.to_string(),
        ..Default::default()
    };
    let mut seen_stage = std::collections::BTreeSet::new();
    let mut lines = 0usize;
    let mut parsed = 0usize;
    for line in raw.lines() {
        lines += 1;
        let Ok(ev) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        parsed += 1;
        match ev.get("type").and_then(Value::as_str) {
            Some("step_usage") => r.model_calls += 1,
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
    if parsed == 0 {
        r.last_error = Some(truncate(
            &format!(
                "{lines} line(s), none a parseable event — an empty, non-JSON, or \
                 truncated stream"
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
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    if n == 0 {
        return String::new();
    }
    s.chars().take(n - 1).collect::<String>() + "…"
}

/// Width of the rendered table, in columns: 24 (task) + 14 (verdict) + 6 + 6 +
/// 5 + 4 + 4 for the counters, plus the eight separator spaces and the six of
/// `reward`. The verdict column is exactly as wide as the longest verdict
/// string, `ran (unsolved)` — an 8-wide column overflowed it and shifted every
/// counter on that row.
const TABLE_WIDTH: usize = 77;

fn print_table(reports: &[TrialReport]) {
    println!(
        "\n{:<24} {:>14} {:>6} {:>6} {:>5} {:>4} {:>4}  reward",
        "task", "verdict", "calls", "tools", "wr", "ov", "gq"
    );
    println!("{}", "─".repeat(TABLE_WIDTH));
    let mut solved = 0usize;
    let mut loop_broken = 0usize;
    let mut overview_used = 0usize;
    let mut graph_used = 0usize;
    for r in reports {
        let reward = r.reward.map(|v| format!("{v:.1}")).unwrap_or("-".into());
        println!(
            "{:<24} {:>14} {:>6} {:>6} {:>5} {:>4} {:>4}  {}",
            truncate(&r.task, 24),
            r.loop_verdict(),
            r.model_calls,
            r.tool_calls,
            r.file_writes,
            r.project_overview_calls,
            r.graph_query_calls,
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
        if r.silent()
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
    }
    let n = reports.len();
    println!("{}", "─".repeat(TABLE_WIDTH));
    println!(
        "LOOP: {loop_broken}/{n} broken (silent-death or zero-work)   \
         CONTEXT: project_overview {overview_used}/{n}, graph_query {graph_used}/{n}   \
         REWARD: {solved}/{n} solved"
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
mod tests {
    use super::*;

    fn ev(events: &[&str]) -> String {
        events.join("\n")
    }

    #[test]
    fn a_healthy_run_is_distilled_as_ran_with_tool_and_context_use() {
        let stream = ev(&[
            r#"{"type":"stage","name":"triage"}"#,
            r#"{"type":"step_usage","role":"worker"}"#,
            r#"{"type":"tool_start","call":{"name":"project_overview"}}"#,
            r#"{"type":"tool_start","call":{"name":"graph_query"}}"#,
            r#"{"type":"tool_start","call":{"name":"edit_file"}}"#,
            r#"{"type":"complete","cost_usd":0.01}"#,
        ]);
        let r = distill_events("demo", &stream);
        assert_eq!(r.model_calls, 1);
        assert_eq!(r.tool_calls, 3);
        assert_eq!(r.file_writes, 1);
        assert_eq!(r.project_overview_calls, 1);
        assert_eq!(r.graph_query_calls, 1);
        assert!(r.terminal_event);
        assert!(!r.zero_work);
        assert_eq!(r.loop_verdict(), "ran (unsolved)");
    }

    #[test]
    fn a_zero_work_abort_is_flagged() {
        let stream = ev(&[
            r#"{"type":"stage","name":"triage"}"#,
            r#"{"type":"step_usage","role":"triage"}"#,
            r#"{"type":"error","message":"could not resolve worker","retryable":false}"#,
        ]);
        let r = distill_events("dead", &stream);
        assert!(r.zero_work, "no tool calls => zero work");
        assert!(
            r.terminal_event,
            "the non-retryable error is a terminal signal"
        );
        assert!(!r.silent(), "a stated error is not a silent death");
        assert_eq!(r.loop_verdict(), "ZERO-WORK");
        assert!(r.loop_broken());
    }

    #[test]
    fn a_stream_that_stops_with_no_terminal_event_is_a_silent_death() {
        // triage + plan, then the stream just ends — the exact silent-abort
        // shape the scope-review bug produced.
        let stream = ev(&[
            r#"{"type":"stage","name":"triage"}"#,
            r#"{"type":"stage","name":"plan"}"#,
            r#"{"type":"step_usage","role":"plan"}"#,
        ]);
        let r = distill_events("silent", &stream);
        assert!(!r.terminal_event);
        assert!(r.silent(), "zero work + no terminal event = silent death");
        assert_eq!(r.loop_verdict(), "SILENT-DEATH");
        assert!(r.loop_broken());
    }

    #[test]
    fn a_solved_task_is_never_a_silent_death_even_without_a_complete_event() {
        // 177-tool run that the verifier passed, but the stream ends without a
        // clean `complete` (exited via budget/step-cap). Reward wins.
        let mut stream = String::from(r#"{"type":"stage","name":"execute"}"#);
        for _ in 0..50 {
            stream.push('\n');
            stream.push_str(r#"{"type":"tool_start","call":{"name":"edit_file"}}"#);
        }
        let mut r = distill_events("busy", &stream);
        r.reward = Some(1.0);
        assert!(!r.terminal_event, "no complete event in this stream");
        assert!(!r.silent(), "work happened, so it is not silent");
        assert_eq!(r.loop_verdict(), "solved");
        assert!(!r.loop_broken());
    }

    #[test]
    fn a_batched_edit_counts_as_a_file_write() {
        // `apply_edits` is the batch form of `edit_file`; a run that only ever
        // edits in batches must not report zero writes.
        let stream = ev(&[
            r#"{"type":"tool_start","call":{"name":"apply_edits"}}"#,
            r#"{"type":"tool_start","call":{"name":"delete_file"}}"#,
        ]);
        let r = distill_events("batch", &stream);
        assert_eq!(r.tool_calls, 2);
        assert_eq!(r.file_writes, 1, "a deletion is not a write");
    }

    #[test]
    fn a_truncated_value_still_fits_its_column() {
        // The ellipsis has to come out of the budget, not be appended past it,
        // or the fixed-width task column shifts the whole row.
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abcd", 3), "ab…");
        assert_eq!(truncate("abcd", 3).chars().count(), 3);
        assert_eq!(truncate("abc", 0), "");
        // Multi-byte input must be split on characters, never bytes.
        assert_eq!(truncate("ααββ", 3), "αα…");
    }

    #[test]
    fn a_stream_of_non_events_names_itself_instead_of_reading_as_a_silent_death() {
        // stella failing before it opens the stream leaves its plain-text
        // complaint in the file the adapter uploads. Every line is unparseable,
        // so the counters are all zero and the verdict is SILENT-DEATH — true,
        // but the cause is plumbing, not the loop, and the row has to say so.
        let stream = ev(&[
            "models: no credentials for provider `openrouter`",
            "aborting",
        ]);
        let r = distill_events("garbled", &stream);
        assert!(r.silent(), "no events at all is still a silent death");
        let err = r.last_error.expect("an unparseable stream explains itself");
        assert!(
            err.starts_with("2 line(s), none a parseable event"),
            "{err}"
        );
    }

    #[test]
    fn a_retryable_warning_is_not_a_terminal_event() {
        let stream =
            ev(&[r#"{"type":"error","message":"degraded: no witness author","retryable":true}"#]);
        let r = distill_events("warn", &stream);
        assert!(
            !r.terminal_event,
            "a retryable degradation warning must not count as a terminal signal"
        );
    }
}
