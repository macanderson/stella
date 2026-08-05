//! End-to-end witness for `stella stats graph` (#896): drive the real binary
//! over real workspace stores, through the whole before/after flow.
//!
//! #896's acceptance is a *measurement* — "a measurable rise in graph-backed
//! retrieval" — which no diff can satisfy on its own. What can be built, and
//! what these tests pin, is the instrument that makes the claim checkable:
//! pool a run's workspaces into one number, persist it, and diff a later run
//! against it with a verdict that refuses to call a health regression a win.
//!
//! The unit tests in `stats_graph/tests.rs` cover the arithmetic and the
//! grading rule. These cover the parts only the binary has: flag parsing,
//! the pooled report, baseline files surviving a process boundary, and the
//! exit code a benchmark job would gate on.

use std::path::Path;
use std::process::{Command, Output};

use stella_protocol::{AgentEvent, ToolCall, ToolOutput};
use stella_store::Store;

/// Seed a workspace with `graph` resolved `graph_query` calls and `grep` grep
/// calls — the two sides of the adoption ratio.
fn seed(workspace: &Path, graph: usize, grep: usize) {
    std::fs::create_dir_all(workspace).expect("mkdir");
    let store = Store::open(workspace).expect("open store");
    let id = store
        .begin_execution("run", "prompt", "anthropic", "opus")
        .expect("execution");
    let mut seq = 0u64;
    let mut record = |event: &AgentEvent| {
        store.record_event(id, seq, event).expect("event");
        seq += 1;
    };
    for n in 0..graph {
        let call_id = format!("g{n}");
        record(&AgentEvent::ToolStart {
            call: ToolCall {
                call_id: call_id.clone(),
                name: "graph_query".into(),
                input: serde_json::json!({ "op": "definitions", "target": "greet" }),
            },
        });
        record(&AgentEvent::ToolResult {
            call_id,
            output: ToolOutput::Ok {
                content: "- lib.rs::greet\n  pub fn greet()".into(),
            },
            duration_ms: 1,
            speculated: false,
        });
    }
    for n in 0..grep {
        let call_id = format!("t{n}");
        record(&AgentEvent::ToolStart {
            call: ToolCall {
                call_id: call_id.clone(),
                name: "grep".into(),
                input: serde_json::json!({ "pattern": "greet" }),
            },
        });
        record(&AgentEvent::ToolResult {
            call_id,
            output: ToolOutput::Ok {
                content: "lib.rs:1:greet".into(),
            },
            duration_ms: 1,
            speculated: false,
        });
    }
}

fn run(cwd: &Path, data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stella"))
        .args(args)
        .current_dir(cwd)
        .env("STELLA_DATA_DIR", data)
        .output()
        .expect("run stella")
}

fn ok(cwd: &Path, data: &Path, args: &[&str]) -> String {
    let output = run(cwd, data, args);
    assert!(
        output.status.success(),
        "stella {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A benchmark run is N task workspaces, each with its own store. Without
/// pooling there is no run-level number to compare at all — only N reports to
/// sum by hand, which is how a measurement stops being reproducible.
#[test]
fn scanning_a_run_pools_every_task_workspace_into_one_number() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = tempfile::tempdir().expect("data");
    let run_dir = dir.path().join("run-1");
    seed(&run_dir.join("task-a"), 3, 7);
    seed(&run_dir.join("task-b"), 1, 9);

    let out = ok(
        dir.path(),
        data.path(),
        &["stats", "graph", "--scan", run_dir.to_str().unwrap()],
    );

    assert!(out.contains("pooled over 2 workspaces"), "{out}");
    // 4 graph_query of 20 structural searches — call-weighted across both.
    assert!(out.contains("graph_query 4 of 20"), "{out}");
    assert!(out.contains("20.0%"), "{out}");
}

/// The whole flow, across two processes: capture a baseline, then compare a
/// later observation against it and get a graded answer.
#[test]
fn a_baseline_survives_the_process_boundary_and_grades_the_next_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = tempfile::tempdir().expect("data");
    let before = dir.path().join("before");
    let after = dir.path().join("after");
    // Before: the near-zero adoption telemetry showed. After: the rise.
    seed(&before, 2, 40);
    seed(&after, 20, 20);
    let baseline = dir.path().join("baseline.json");

    ok(
        dir.path(),
        data.path(),
        &[
            "stats",
            "graph",
            "--workspace",
            before.to_str().unwrap(),
            "--save-baseline",
            baseline.to_str().unwrap(),
            "--label",
            "before the change",
        ],
    );
    assert!(baseline.is_file(), "the baseline file must exist");

    let out = ok(
        dir.path(),
        data.path(),
        &[
            "stats",
            "graph",
            "--workspace",
            after.to_str().unwrap(),
            "--baseline",
            baseline.to_str().unwrap(),
        ],
    );

    assert!(out.contains("IMPROVED"), "{out}");
    assert!(
        out.contains("before the change"),
        "the label carries: {out}"
    );
    // 2/42 ≈ 4.8% → 20/40 = 50%.
    assert!(out.contains("adoption"), "{out}");
    assert!(out.contains("+45.2pp"), "{out}");
}

/// The gate a benchmark job would use. Off by default — reporting a
/// regression and failing on one are different requests — and when asked for,
/// it fails with the reason rather than a bare exit code.
#[test]
fn fail_on_regression_is_opt_in_and_explains_itself() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = tempfile::tempdir().expect("data");
    let high = dir.path().join("high");
    let low = dir.path().join("low");
    seed(&high, 20, 20);
    seed(&low, 2, 40);
    let baseline = dir.path().join("baseline.json");

    ok(
        dir.path(),
        data.path(),
        &[
            "stats",
            "graph",
            "--workspace",
            high.to_str().unwrap(),
            "--save-baseline",
            baseline.to_str().unwrap(),
        ],
    );

    // Default: the regression is reported, and the command still succeeds.
    let reported = run(
        dir.path(),
        data.path(),
        &[
            "stats",
            "graph",
            "--workspace",
            low.to_str().unwrap(),
            "--baseline",
            baseline.to_str().unwrap(),
        ],
    );
    assert!(
        reported.status.success(),
        "reporting a regression is not itself a failure"
    );
    assert!(
        String::from_utf8_lossy(&reported.stdout).contains("REGRESSED"),
        "{}",
        String::from_utf8_lossy(&reported.stdout)
    );

    // Opted in: non-zero, with the reason on stderr.
    let gated = run(
        dir.path(),
        data.path(),
        &[
            "stats",
            "graph",
            "--workspace",
            low.to_str().unwrap(),
            "--baseline",
            baseline.to_str().unwrap(),
            "--fail-on-regression",
        ],
    );
    assert!(!gated.status.success(), "the gate must fail the job");
    let stderr = String::from_utf8_lossy(&gated.stderr);
    assert!(stderr.contains("adoption fell"), "{stderr}");
}

/// A thin sample is refused rather than graded. A "rise" measured over a
/// handful of calls is noise with a percentage sign, and grading it anyway is
/// how a fluke gets recorded as the evidence that closed an issue.
#[test]
fn a_thin_sample_is_graded_inconclusive_not_improved() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = tempfile::tempdir().expect("data");
    let before = dir.path().join("before");
    let after = dir.path().join("after");
    seed(&before, 0, 3);
    seed(&after, 3, 1);
    let baseline = dir.path().join("baseline.json");

    ok(
        dir.path(),
        data.path(),
        &[
            "stats",
            "graph",
            "--workspace",
            before.to_str().unwrap(),
            "--save-baseline",
            baseline.to_str().unwrap(),
        ],
    );
    let out = ok(
        dir.path(),
        data.path(),
        &[
            "stats",
            "graph",
            "--workspace",
            after.to_str().unwrap(),
            "--baseline",
            baseline.to_str().unwrap(),
        ],
    );

    assert!(out.contains("INCONCLUSIVE"), "{out}");
    assert!(out.contains("nothing here to grade"), "{out}");
}

/// The machine-readable path, for a benchmark harness that records the number
/// rather than reading it.
#[test]
fn json_comparison_is_a_parseable_receipt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = tempfile::tempdir().expect("data");
    let before = dir.path().join("before");
    let after = dir.path().join("after");
    seed(&before, 2, 40);
    seed(&after, 20, 20);
    let baseline = dir.path().join("baseline.json");

    ok(
        dir.path(),
        data.path(),
        &[
            "stats",
            "graph",
            "--workspace",
            before.to_str().unwrap(),
            "--save-baseline",
            baseline.to_str().unwrap(),
        ],
    );
    let out = ok(
        dir.path(),
        data.path(),
        &[
            "stats",
            "graph",
            "--format",
            "json",
            "--workspace",
            after.to_str().unwrap(),
            "--baseline",
            baseline.to_str().unwrap(),
        ],
    );

    let parsed: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(parsed["verdict"], "improved");
    assert_eq!(parsed["current_workspaces"], 1);
    assert!(parsed["adoption"]["delta"].as_f64().expect("delta") > 0.4);
    assert!(
        parsed["rationale"]
            .as_str()
            .expect("rationale")
            .contains("rose")
    );
}

/// A misspelled `--workspace` fails loudly. Silently reporting zeroes for a
/// path that does not exist is how a measurement quietly becomes meaningless.
#[test]
fn a_workspace_without_telemetry_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = tempfile::tempdir().expect("data");
    let empty = dir.path().join("not-a-workspace");
    std::fs::create_dir_all(&empty).expect("mkdir");

    let output = run(
        dir.path(),
        data.path(),
        &["stats", "graph", "--workspace", empty.to_str().unwrap()],
    );

    assert!(!output.status.success(), "a path with no store is an error");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no telemetry"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
