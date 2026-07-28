//! The ratified verdict contract (#611): what makes a run red, and what is
//! informational.

use super::*;

fn ev(events: &[&str]) -> String {
    events.join("\n")
}

#[test]
fn a_healthy_run_is_distilled_as_ran_with_tool_and_context_use() {
    let stream = ev(&[
        r#"{"type":"stage","name":"triage"}"#,
        r#"{"type":"step_usage","role":"worker","cost_usd":0.01}"#,
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

/// #611: a stream of non-events is `UNREADABLE` — evidence about the
/// plumbing, not the loop — and is excluded from the gate. It still names
/// itself so the operator hunts the right layer.
#[test]
fn a_stream_of_non_events_is_unreadable_and_does_not_gate() {
    // stella failing before it opens the stream leaves its plain-text
    // complaint in the file the adapter uploads.
    let stream = ev(&[
        "models: no credentials for provider `openrouter`",
        "aborting",
    ]);
    let r = distill_events("garbled", &stream);
    assert_eq!(r.parsed_lines, 0);
    assert_eq!(r.unparsable_lines, 2);
    assert_eq!(r.loop_verdict(), "UNREADABLE");
    assert!(
        !r.loop_broken(),
        "schema drift must not make a healthy run gate red"
    );
    let err = r.last_error.expect("an unparseable stream explains itself");
    assert!(
        err.starts_with("2 line(s), none a parseable event"),
        "{err}"
    );
}

/// #611: partial drift is not UNREADABLE — as long as events parse, the run
/// is judged on them, and the parse counts are visible in the JSON.
#[test]
fn a_partially_unparsable_stream_is_still_judged_on_its_events() {
    let stream = ev(&[
        "not an event",
        r#"{"type":"tool_start","call":{"name":"edit_file"}}"#,
    ]);
    let r = distill_events("drift", &stream);
    assert_eq!((r.parsed_lines, r.unparsable_lines), (1, 1));
    assert!(!r.unreadable());
    assert_eq!(r.loop_verdict(), "ran (unsolved)");
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

/// #611: the engine's own loop detector firing on a non-pass is the
/// `STUCK-LOOP` verdict, and it gates red — a run that burns its budget
/// cycling the same two tools must not report `ran (unsolved)` and exit 0.
#[test]
fn a_detected_loop_on_a_non_pass_is_stuck_and_gates_red() {
    let stream = ev(&[
        r#"{"type":"tool_start","call":{"name":"read_file"}}"#,
        r#"{"type":"loop_detected","window":4}"#,
        r#"{"type":"tool_start","call":{"name":"read_file"}}"#,
        r#"{"type":"loop_detected","window":4}"#,
    ]);
    let r = distill_events("cycling", &stream);
    assert_eq!(r.loop_detected, 2);
    assert_eq!(r.loop_verdict(), "STUCK-LOOP");
    assert!(r.loop_broken());

    // …but reward still wins: a solved task did the work, by definition.
    let mut solved = distill_events("cycling", &stream);
    solved.reward = Some(1.0);
    assert_eq!(solved.loop_verdict(), "solved");
    assert!(!solved.loop_broken());
}

/// #611: the harness's own cost cap stopping the turn is `BUDGET-CAP`,
/// excluded from the gate — CI must not fail for a cost decision. A stuck
/// loop that ALSO trips the cap stays red: the cap firing there is a symptom.
#[test]
fn a_budget_denial_is_informational_unless_the_loop_was_stuck() {
    let stream = ev(&[r#"{"type":"budget_denied","message":"cap"}"#]);
    let r = distill_events("capped", &stream);
    assert!(r.budget_capped);
    assert!(r.zero_work);
    assert_eq!(r.loop_verdict(), "BUDGET-CAP");
    assert!(
        !r.loop_broken(),
        "a trial denied before its first tool call is a cost decision, not a loop defect"
    );

    let stuck_stream = ev(&[
        r#"{"type":"loop_detected","window":4}"#,
        r#"{"type":"budget_denied","message":"cap"}"#,
    ]);
    let stuck = distill_events("stuck-then-capped", &stuck_stream);
    assert_eq!(stuck.loop_verdict(), "STUCK-LOOP");
    assert!(stuck.loop_broken(), "the cap fired because the loop cycled");
}

/// #611: `step_usage.cost_usd` sums into the report, so an operator can tell
/// whether the per-task cap was hit when interpreting a verdict.
#[test]
fn spend_is_summed_from_step_usage() {
    let stream = ev(&[
        r#"{"type":"step_usage","cost_usd":0.05}"#,
        r#"{"type":"step_usage","cost_usd":0.07}"#,
        r#"{"type":"step_usage"}"#,
    ]);
    let r = distill_events("spend", &stream);
    assert!((r.spend_usd - 0.12).abs() < 1e-9);
    assert_eq!(r.model_calls, 3);
}

/// #611: a requested task with no trial dir is a `NOT-RUN` row that gates
/// red, and a stale trial dir from an earlier run is skipped — the report
/// covers exactly the tasks this run asked for.
#[test]
fn analyze_reconciles_the_requested_task_set() {
    let dir = tempfile::tempdir().expect("job dir");
    // One requested task landed; one stale dir from a previous run lingers.
    for (trial, event) in [
        (
            "wanted__t1",
            r#"{"type":"tool_start","call":{"name":"edit_file"}}"#,
        ),
        ("stale__t0", r#"{"type":"complete"}"#),
    ] {
        let agent = dir.path().join(trial).join("agent");
        std::fs::create_dir_all(&agent).expect("mkdir");
        std::fs::write(agent.join("stella-events.jsonl"), event).expect("write");
    }

    let requested = vec!["wanted".to_string(), "missing".to_string()];
    let reports = analyze(dir.path(), Some(&requested));
    let tasks: Vec<&str> = reports.iter().map(|r| r.task.as_str()).collect();
    assert_eq!(
        tasks,
        vec!["missing", "wanted"],
        "stale dirs are skipped, missing tasks are synthesized"
    );
    let missing = &reports[0];
    assert!(missing.not_run);
    assert_eq!(missing.loop_verdict(), "NOT-RUN");
    assert!(
        missing.loop_broken(),
        "a task that never launched must not read as a smaller, healthy run"
    );

    // --analyze-only reads whatever is there: no filtering, no synthesis.
    let all = analyze(dir.path(), None);
    let tasks: Vec<&str> = all.iter().map(|r| r.task.as_str()).collect();
    assert_eq!(tasks, vec!["stale", "wanted"]);
}

/// #611: a present-but-corrupt reward file appends to `last_error` instead of
/// silently downgrading a passing task; a missing file stays a quiet `None`.
#[test]
fn a_corrupt_reward_file_names_itself() {
    let dir = tempfile::tempdir().expect("trial dir");
    assert_eq!(
        read_reward(dir.path()),
        (None, None),
        "missing file is quiet"
    );

    let verifier = dir.path().join("verifier");
    std::fs::create_dir_all(&verifier).expect("mkdir");
    std::fs::write(verifier.join("reward.txt"), "not-a-number").expect("write");
    let (reward, problem) = read_reward(dir.path());
    assert_eq!(reward, None);
    let problem = problem.expect("a corrupt reward must explain itself");
    assert!(problem.contains("unreadable reward.txt"), "{problem}");

    std::fs::write(verifier.join("reward.txt"), "1.0\n").expect("write");
    assert_eq!(read_reward(dir.path()), (Some(1.0), None));
}
