//! Tests for `stella stats graph` — the report, the pooling, and the grading
//! rule that turns two reports into a verdict.

use super::*;

fn answer(op: &str, content: &str, ok: bool) -> stella_store::ToolCallAnswer {
    stella_store::ToolCallAnswer {
        args_json: format!("{{\"op\":\"{op}\",\"target\":\"x\"}}"),
        content: content.into(),
        ok,
    }
}

/// A report with the two headline rates set directly — the compare tests care
/// about the grading rule, not about how the rates were derived.
fn health_at(adoption: f64, resolved: f64, searches: i64) -> GraphHealth {
    let graph_query_calls = (searches as f64 * adoption).round() as i64;
    let mut total = GraphOpRow::new("TOTAL");
    total.calls = 100;
    total.resolved = (100.0 * resolved).round() as u64;
    total.resolved_rate = resolved;
    GraphHealth {
        ops: vec![],
        total,
        graph_query_calls,
        text_search_calls: searches - graph_query_calls,
        adoption_rate: adoption,
        workspaces: 1,
    }
}

fn snapshot(health: GraphHealth) -> GraphSnapshot {
    GraphSnapshot {
        schema: GRAPH_SNAPSHOT_SCHEMA,
        label: None,
        captured_at_unix: None,
        health,
    }
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

/// The whole point of the metric: an empty answer is a successful call, so
/// counting calls says nothing about whether the graph is working. This
/// splits the same `state = 'ok'` calls into resolved and the three distinct
/// ways a query fails to land.
#[test]
fn graph_health_splits_successful_calls_into_resolved_and_the_ways_they_miss() {
    use stella_tools::graph::{
        AMBIGUOUS_MARKER, NOT_INDEXED_MARKER, OUT_OF_ROOT_MARKER, UNRESOLVED_MARKER,
    };

    let answers = vec![
        answer("definitions", "- lib.rs::greet\n  pub fn greet()", true),
        answer(
            "definitions",
            &format!("{UNRESOLVED_MARKER} definitions for `nope`"),
            true,
        ),
        answer(
            "neighbors",
            &format!("`../other/x.rs` is {OUT_OF_ROOT_MARKER} `/w`"),
            true,
        ),
        answer(
            "neighbors",
            &format!("`README.md` {NOT_INDEXED_MARKER} (rust, …)"),
            true,
        ),
        answer(
            "neighbors",
            &format!("`x.rs` {AMBIGUOUS_MARKER}: `a/x.rs`"),
            true,
        ),
        answer("imports", "code-graph query failed: boom", false),
    ];
    let counts = vec![
        ("grep".to_string(), 30),
        ("glob".to_string(), 10),
        ("graph_query".to_string(), 10),
        ("bash".to_string(), 900),
    ];

    let health = graph_health(&answers, &counts);
    assert_eq!(health.total.calls, 6);
    assert_eq!(health.total.resolved, 1);
    assert_eq!(health.total.unresolved, 2, "generic miss + ambiguous");
    assert_eq!(health.total.out_of_root, 1);
    assert_eq!(health.total.not_indexed, 1);
    assert_eq!(health.total.failed, 1, "an errored call is not a miss");
    assert!((health.total.resolved_rate - 1.0 / 6.0).abs() < 1e-9);

    // Busiest op first, and each op keeps its own rate.
    assert_eq!(health.ops[0].op, "neighbors");
    assert_eq!(health.ops[0].calls, 3);
    assert_eq!(health.ops[0].resolved_rate, 0.0);

    // Adoption is measured against the text tools it competes with —
    // `bash` is excluded, or builds and git would drown the ratio.
    assert_eq!(health.graph_query_calls, 10);
    assert_eq!(health.text_search_calls, 40);
    assert!((health.adoption_rate - 0.2).abs() < 1e-9);
    assert_eq!(health.workspaces, 1);
}

#[test]
fn an_empty_store_reports_zeroes_rather_than_dividing_by_zero() {
    let health = graph_health(&[], &[]);
    assert_eq!(health.total.calls, 0);
    assert_eq!(health.total.resolved_rate, 0.0);
    assert_eq!(health.adoption_rate, 0.0);
    assert!(health.ops.is_empty());
}

/// End to end over a real store: record the same `tool_start` /
/// `tool_result` pairs the agent loop records, read them back, and check the
/// rate. This is what makes the metric a *measurement* of the running agent
/// rather than of a fixture.
#[test]
fn a_seeded_store_reports_a_real_resolved_query_rate() {
    use stella_tools::graph::OUT_OF_ROOT_MARKER;

    let store = Store::in_memory().expect("store");
    seed_graph_calls(
        &store,
        &[
            ("definitions", "- lib.rs::greet\n  pub fn greet()"),
            (
                "neighbors",
                &format!("`../sibling/x.rs` is {OUT_OF_ROOT_MARKER} `/w`"),
            ),
        ],
        1,
    );

    let answers = store
        .tool_call_answers("graph_query", 100)
        .expect("answers");
    let counts = store.tool_call_counts().expect("counts");
    let health = graph_health(&answers, &counts);

    assert_eq!(health.total.calls, 2);
    assert_eq!(health.total.resolved, 1);
    assert_eq!(health.total.out_of_root, 1);
    assert_eq!(health.total.resolved_rate, 0.5);
    assert_eq!(health.graph_query_calls, 2);
    assert_eq!(health.text_search_calls, 1);

    let table = render_graph_table(&health);
    assert!(table.contains("50.0%"), "{table}");
    assert!(
        table.contains("definitions") && table.contains("neighbors"),
        "{table}"
    );
    let csv = render_graph_csv(&health);
    assert!(csv.contains("TOTAL,2,1,0,1,0,0,0.5000"), "{csv}");
}

/// Record `graph_query` calls with the given (op, answer) pairs, plus
/// `grep_calls` grep calls, into one execution.
fn seed_graph_calls(store: &Store, calls: &[(&str, &str)], grep_calls: usize) {
    use stella_protocol::{AgentEvent, ToolCall, ToolOutput};

    let id = store
        .begin_execution("run", "prompt", "anthropic", "opus")
        .expect("begin");
    let mut seq = 0u64;
    let mut record = |event: &AgentEvent| {
        store.record_event(id, seq, event).expect("record");
        seq += 1;
    };
    for (n, (op, content)) in calls.iter().enumerate() {
        let call_id = format!("g{n}");
        record(&AgentEvent::ToolStart {
            call: ToolCall {
                call_id: call_id.clone(),
                name: "graph_query".into(),
                input: serde_json::json!({ "op": op, "target": "t" }),
            },
        });
        record(&AgentEvent::ToolResult {
            call_id,
            output: ToolOutput::Ok {
                content: (*content).into(),
            },
            duration_ms: 1,
            speculated: false,
        });
    }
    for n in 0..grep_calls {
        let call_id = format!("t{n}");
        record(&AgentEvent::ToolStart {
            call: ToolCall {
                call_id: call_id.clone(),
                name: "grep".into(),
                input: serde_json::json!({ "pattern": "x" }),
            },
        });
        record(&AgentEvent::ToolResult {
            call_id,
            output: ToolOutput::Ok {
                content: "lib.rs:1:x".into(),
            },
            duration_ms: 1,
            speculated: false,
        });
    }
}

// ---------------------------------------------------------------------------
// Pooling
// ---------------------------------------------------------------------------

/// Pooling sums counts and re-derives every rate, so the run-level figure is
/// call-weighted. The alternative — averaging the per-workspace rates — would
/// let a workspace with one query outvote one with a hundred, which is how a
/// benchmark sweep reports a number nothing that happened supports.
#[test]
fn pooling_weights_by_calls_not_by_workspace() {
    // Busy workspace: 90 of 100 searches were graph queries, all resolved.
    let mut busy = graph_health(
        &vec![answer("definitions", "- a.rs::x", true); 90],
        &[("graph_query".into(), 90), ("grep".into(), 10)],
    );
    busy.workspaces = 1;
    // Quiet workspace: one search, not a graph query, so 0% adoption.
    let quiet = graph_health(&[], &[("grep".into(), 1)]);

    let pooled = merge_health(&[busy, quiet]);

    assert_eq!(pooled.workspaces, 2);
    assert_eq!(pooled.graph_query_calls, 90);
    assert_eq!(pooled.text_search_calls, 11);
    // Call-weighted: 90/101 ≈ 89.1%. Averaging the two workspaces' rates
    // would have said (90% + 0%) / 2 = 45%.
    assert!(
        (pooled.adoption_rate - 90.0 / 101.0).abs() < 1e-9,
        "{}",
        pooled.adoption_rate
    );
    assert_eq!(pooled.total.calls, 90);
    assert_eq!(pooled.total.resolved, 90);
    assert_eq!(pooled.total.resolved_rate, 1.0);
}

/// Per-op rows merge by op name across workspaces, and the pooled rate is
/// re-derived rather than carried over from either side.
#[test]
fn pooling_merges_op_rows_across_workspaces() {
    use stella_tools::graph::UNRESOLVED_MARKER;

    let one = graph_health(
        &[answer("definitions", "- a.rs::x", true)],
        &[("graph_query".into(), 1)],
    );
    let two = graph_health(
        &[
            answer("definitions", &format!("{UNRESOLVED_MARKER} nope"), true),
            answer("neighbors", "- b.rs::y", true),
        ],
        &[("graph_query".into(), 2)],
    );

    let pooled = merge_health(&[one, two]);

    let defs = pooled
        .ops
        .iter()
        .find(|r| r.op == "definitions")
        .expect("definitions row");
    assert_eq!(defs.calls, 2);
    assert_eq!(defs.resolved, 1);
    assert_eq!(defs.unresolved, 1);
    assert_eq!(defs.resolved_rate, 0.5, "re-derived from the summed counts");
    assert_eq!(pooled.total.calls, 3);
    assert!((pooled.total.resolved_rate - 2.0 / 3.0).abs() < 1e-9);
}

#[test]
fn pooling_nothing_is_the_empty_report() {
    let pooled = merge_health(&[]);
    assert_eq!(pooled.workspaces, 0);
    assert_eq!(pooled.adoption_rate, 0.0);
    assert_eq!(pooled.total.calls, 0);
}

// ---------------------------------------------------------------------------
// The grading rule
// ---------------------------------------------------------------------------

/// The headline case the issue asks for: adoption up, health held.
#[test]
fn a_rise_with_a_held_resolve_rate_is_an_improvement() {
    let cmp = compare(
        &snapshot(health_at(0.02, 0.70, 500)),
        &health_at(0.20, 0.72, 500),
    );
    assert_eq!(cmp.verdict, GraphVerdict::Improved);
    assert!((cmp.adoption.delta - 0.18).abs() < 1e-9);
    assert!(cmp.rationale.contains("18.0 points"), "{}", cmp.rationale);
}

/// The trap this grading rule exists for. Queries made more often but landing
/// less often is not a win, however good the adoption number looks — so the
/// verdict must not be `Improved`, and must not be silently folded into a
/// plain `Regressed` either, because the fix is a different one.
#[test]
fn a_rise_bought_with_a_collapsed_resolve_rate_is_not_a_win() {
    let cmp = compare(
        &snapshot(health_at(0.02, 0.80, 500)),
        &health_at(0.30, 0.40, 500),
    );
    assert_eq!(cmp.verdict, GraphVerdict::ResolveRegressed);
    assert!(cmp.verdict.is_regression());
    assert!(
        cmp.rationale.contains("resolved-query rate fell"),
        "{}",
        cmp.rationale
    );
}

/// A resolve-rate collapse counts even when adoption did not move — the
/// health metric is not conditional on the adoption metric having a story.
#[test]
fn a_collapsed_resolve_rate_alone_is_a_regression() {
    let cmp = compare(
        &snapshot(health_at(0.20, 0.80, 500)),
        &health_at(0.20, 0.40, 500),
    );
    assert_eq!(cmp.verdict, GraphVerdict::ResolveRegressed);
}

#[test]
fn a_fall_in_adoption_is_a_regression() {
    let cmp = compare(
        &snapshot(health_at(0.20, 0.80, 500)),
        &health_at(0.05, 0.80, 500),
    );
    assert_eq!(cmp.verdict, GraphVerdict::Regressed);
    assert!(cmp.verdict.is_regression());
    assert!(
        cmp.rationale.contains("fell 15.0 points"),
        "{}",
        cmp.rationale
    );
}

#[test]
fn a_move_under_the_noise_floor_is_unchanged() {
    let cmp = compare(
        &snapshot(health_at(0.200, 0.80, 500)),
        &health_at(0.205, 0.80, 500),
    );
    assert_eq!(cmp.verdict, GraphVerdict::Unchanged);
    assert!(!cmp.verdict.is_regression());
}

/// A ratio over a handful of calls swings by tens of points on one more grep.
/// Grading it anyway is how a fluke gets recorded as the evidence that closed
/// an issue, so a small sample is refused rather than graded generously.
#[test]
fn too_few_searches_is_refused_rather_than_graded() {
    let cmp = compare(&snapshot(health_at(0.00, 0.0, 4)), &health_at(0.50, 1.0, 6));
    assert_eq!(cmp.verdict, GraphVerdict::Inconclusive);
    assert!(
        !cmp.verdict.is_regression(),
        "not a regression, just unproven"
    );
    assert!(
        cmp.rationale.contains("nothing here to grade"),
        "{}",
        cmp.rationale
    );

    // A large baseline does not rescue a thin current run, or vice versa.
    let thin_current = compare(
        &snapshot(health_at(0.02, 0.8, 900)),
        &health_at(0.9, 1.0, 5),
    );
    assert_eq!(thin_current.verdict, GraphVerdict::Inconclusive);
}

/// The comparison carries both sides' workspace counts, because a
/// one-workspace baseline against a sixty-workspace run is not a like-for-like
/// measurement and the reader has to be able to see that.
#[test]
fn the_comparison_reports_both_sides_provenance() {
    let mut base = health_at(0.02, 0.8, 500);
    base.workspaces = 1;
    let mut current = health_at(0.2, 0.8, 500);
    current.workspaces = 60;
    let mut snap = snapshot(base);
    snap.label = Some("pre-change, one checkout".into());

    let cmp = compare(&snap, &current);
    assert_eq!(cmp.baseline_workspaces, 1);
    assert_eq!(cmp.current_workspaces, 60);
    let table = render_comparison_table(&cmp);
    assert!(table.contains("workspaces pooled"), "{table}");
    assert!(table.contains("pre-change, one checkout"), "{table}");
    assert!(table.contains("IMPROVED"), "{table}");
}

#[test]
fn comparison_csv_carries_the_verdict() {
    let cmp = compare(
        &snapshot(health_at(0.02, 0.8, 500)),
        &health_at(0.20, 0.8, 500),
    );
    let csv = render_comparison_csv(&cmp);
    assert!(csv.starts_with("metric,baseline,current,delta\n"), "{csv}");
    assert!(csv.contains("adoption_rate,0.0200,0.2000,0.1800"), "{csv}");
    assert!(csv.contains("verdict,,IMPROVED,"), "{csv}");
}

// ---------------------------------------------------------------------------
// Baseline files
// ---------------------------------------------------------------------------

#[test]
fn a_baseline_round_trips_through_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested").join("baseline.json");
    let health = health_at(0.02, 0.8, 500);

    write_baseline(
        &path,
        &health,
        Some("before the change"),
        Some(1_700_000_000),
    )
    .expect("write");
    let read = read_baseline(&path).expect("read");

    assert_eq!(read.schema, GRAPH_SNAPSHOT_SCHEMA);
    assert_eq!(read.label.as_deref(), Some("before the change"));
    assert_eq!(read.captured_at_unix, Some(1_700_000_000));
    assert_eq!(read.health, health, "the report survives the round trip");
}

/// A snapshot from a newer build is refused rather than read with whatever
/// fields happen to overlap — a silently misread baseline produces a delta
/// that looks authoritative and means nothing.
#[test]
fn a_newer_schema_is_refused_not_guessed_at() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("baseline.json");
    let mut snap = snapshot(health_at(0.02, 0.8, 500));
    snap.schema = GRAPH_SNAPSHOT_SCHEMA + 1;
    std::fs::write(&path, serde_json::to_string(&snap).unwrap()).unwrap();

    let err = read_baseline(&path).expect_err("must refuse");
    assert!(err.contains("newer stella"), "{err}");
}

#[test]
fn a_file_that_is_not_a_snapshot_says_so() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nope.json");
    std::fs::write(&path, "{\"hello\":true}").unwrap();
    let err = read_baseline(&path).expect_err("must refuse");
    assert!(err.contains("not a graph snapshot"), "{err}");
}

// ---------------------------------------------------------------------------
// Finding workspaces
// ---------------------------------------------------------------------------

/// Make `root` look like a workspace with telemetry.
fn seed_workspace(root: &Path) {
    std::fs::create_dir_all(root).expect("mkdir");
    let store = Store::open(root).expect("open");
    seed_graph_calls(&store, &[("definitions", "- a.rs::x")], 1);
}

#[test]
fn scanning_finds_nested_workspaces_and_skips_noise() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    seed_workspace(&root.join("run-1").join("task-a"));
    seed_workspace(&root.join("run-1").join("task-b"));
    // Never descended into: a vendored checkout is not a workspace the agent
    // worked in, and counting it would inflate the run-level denominator.
    seed_workspace(&root.join("run-1").join("node_modules").join("pkg"));
    seed_workspace(&root.join(".cache").join("hidden"));

    let found = discover_workspaces(root, 4).expect("scan");

    assert_eq!(found.len(), 2, "found: {found:?}");
    assert!(
        found
            .iter()
            .all(|p| p.ends_with("task-a") || p.ends_with("task-b"))
    );
}

/// A workspace's own subdirectories are its source tree, not sibling
/// workspaces — descending into one would count the same store twice.
#[test]
fn scanning_does_not_descend_into_a_workspace_it_already_found() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("outer");
    seed_workspace(&root);
    seed_workspace(&root.join("inner"));

    let found = discover_workspaces(dir.path(), 4).expect("scan");

    assert_eq!(found, vec![root], "inner must not be counted separately");
}

#[test]
fn scanning_respects_the_depth_bound() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_workspace(&dir.path().join("a").join("b").join("c").join("deep"));

    assert!(
        discover_workspaces(dir.path(), 2)
            .expect("shallow")
            .is_empty()
    );
    assert_eq!(discover_workspaces(dir.path(), 4).expect("deep").len(), 1);
}

/// Reading two workspaces off disk and pooling them is the run-level number.
#[test]
fn two_workspaces_pool_into_one_run_level_report() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("task-a");
    let b = dir.path().join("task-b");
    seed_workspace(&a);
    seed_workspace(&b);

    let parts = [
        health_for(&a, 500).expect("a"),
        health_for(&b, 500).expect("b"),
    ];
    let pooled = merge_health(&parts);

    assert_eq!(pooled.workspaces, 2);
    assert_eq!(pooled.graph_query_calls, 2, "one graph_query per workspace");
    assert_eq!(pooled.text_search_calls, 2);
    assert_eq!(pooled.total.calls, 2);
    assert_eq!(pooled.total.resolved, 2);
    let table = render_graph_table(&pooled);
    assert!(table.contains("pooled over 2 workspaces"), "{table}");
}

/// A misspelled `--workspace` must fail loudly. Reporting zeroes for a path
/// that does not exist is how a measurement quietly becomes meaningless.
#[test]
fn an_explicit_workspace_without_telemetry_is_an_error_not_a_zero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let empty = dir.path().join("no-store");
    std::fs::create_dir_all(&empty).unwrap();

    let args = GraphArgs {
        format: StatsFormat::Table,
        limit: 500,
        workspace: vec![empty.clone()],
        scan: None,
        scan_depth: 4,
        save_baseline: None,
        label: None,
        baseline: None,
        fail_on_regression: false,
    };
    let err = resolve_roots(&args).expect_err("must refuse");
    assert!(err.contains("no telemetry"), "{err}");

    let missing = GraphArgs {
        workspace: vec![dir.path().join("does-not-exist")],
        ..args
    };
    let err = resolve_roots(&missing).expect_err("must refuse");
    assert!(err.contains("not a directory"), "{err}");
}

/// Scanning a tree with nothing in it is an error too: an empty pool silently
/// reported as 0% adoption is indistinguishable from a real zero.
#[test]
fn scanning_a_tree_with_no_workspaces_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let args = GraphArgs {
        format: StatsFormat::Table,
        limit: 500,
        workspace: vec![],
        scan: Some(dir.path().to_path_buf()),
        scan_depth: 4,
        save_baseline: None,
        label: None,
        baseline: None,
        fail_on_regression: false,
    };
    let err = resolve_roots(&args).expect_err("must refuse");
    assert!(err.contains("no workspaces with telemetry"), "{err}");
}
