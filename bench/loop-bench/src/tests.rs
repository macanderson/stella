//! The ratified verdict contract (#611): what makes a run red, and what is
//! informational.

use std::path::{Path, PathBuf};

use super::*;
use crate::reconcile::trial_dir_prefix;

fn ev(events: &[&str]) -> String {
    events.join("\n")
}

/// The common single-trial request: one task, one trial.
fn one_trial_each(tasks: &[String]) -> Requested<'_> {
    Requested {
        tasks,
        trials_per_task: 1,
    }
}

/// One trial directory with a single-step event stream, as harbor lays it out.
fn trial_dir(job: &Path, name: &str, events: &[&str]) -> PathBuf {
    let dir = job.join(name);
    let agent = dir.join("agent");
    std::fs::create_dir_all(&agent).expect("mkdir");
    std::fs::write(agent.join("stella-events.jsonl"), ev(events)).expect("write");
    dir
}

/// Harbor's own record of the trial.
fn write_result(trial: &Path, result: serde_json::Value) {
    std::fs::write(
        trial.join("result.json"),
        serde_json::to_string(&result).expect("serialize"),
    )
    .expect("write");
}

/// One `exception_info` object, as `harbor.models.trial.result` writes it.
fn exception(kind: &str, message: &str) -> serde_json::Value {
    serde_json::json!({
        "exception_type": kind,
        "exception_message": message,
        "exception_traceback": "Traceback (most recent call last): …",
        "occurred_at": "2026-08-04T00:00:00",
    })
}

fn write_reward(trial: &Path, reward: &str) {
    let verifier = trial.join("verifier");
    std::fs::create_dir_all(&verifier).expect("mkdir");
    std::fs::write(verifier.join("reward.txt"), reward).expect("write");
}

/// The single row an analysis of one trial should hold.
fn only(analysis: &Analysis) -> &TrialReport {
    match &analysis.trials[..] {
        [report] => report,
        rows => panic!("expected exactly one row, got {}", rows.len()),
    }
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
    // A 50-tool run (abbreviated from a real 177-tool trial) that the
    // verifier passed, but the stream ends without a clean `complete`
    // (exited via budget/step-cap). Reward wins.
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
    let analysis = analyze(dir.path(), Some(one_trial_each(&requested)));
    let tasks: Vec<&str> = analysis.trials.iter().map(|r| r.task.as_str()).collect();
    assert_eq!(
        tasks,
        vec!["missing", "wanted"],
        "stale dirs are skipped, missing tasks are synthesized"
    );
    let missing = &analysis.trials[0];
    assert!(missing.not_run);
    assert_eq!(missing.loop_verdict(), "NOT-RUN");
    assert!(
        missing.loop_broken(),
        "a task that never launched must not read as a smaller, healthy run"
    );

    // #1299: the skip is recorded rather than silent — a mistyped --tasks
    // otherwise looks exactly like a run harbor launched nothing for.
    let record = analysis
        .reconciliation
        .as_ref()
        .expect("a requested run reconciles");
    assert_eq!(record.unrequested.get("stale"), Some(&1));
    assert_eq!(record.missing(), vec![("missing", 1)]);
    assert!(record.surplus().is_empty());
    assert!(
        !record.contaminated(),
        "a skipped stale dir contributes to no figure, so it is a note, not a bad denominator"
    );
    assert!(
        record.findings().iter().any(|f| f.contains("stale")),
        "{:?}",
        record.findings()
    );

    // --analyze-only reads whatever is there: no filtering, no synthesis.
    let all = analyze(dir.path(), None);
    let tasks: Vec<&str> = all.trials.iter().map(|r| r.task.as_str()).collect();
    assert_eq!(tasks, vec!["stale", "wanted"]);
    assert!(all.reconciliation.is_none(), "nothing was requested");
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

/// #873: the pass floor is a SECOND gate, independent of loop health. A run
/// where every trial ran cleanly and solved nothing is `loop_broken == 0` —
/// the case the nightly job's floor exists to notice.
#[test]
fn the_pass_floor_is_independent_of_loop_health() {
    let healthy_unsolved = |task: &str| TrialReport {
        task: task.into(),
        tool_calls: 3,
        terminal_event: true,
        ..Default::default()
    };
    let reports = vec![
        healthy_unsolved("a"),
        healthy_unsolved("b"),
        TrialReport {
            reward: Some(1.0),
            ..healthy_unsolved("c")
        },
    ];

    let t = tally(&reports);
    assert_eq!(t.total, 3);
    assert_eq!(t.solved, 1);
    assert_eq!(t.loop_broken, 0, "healthy loops, one pass");

    assert!(
        !below_pass_floor(&reports, 0),
        "0 disables the floor — the default must never gate"
    );
    assert!(!below_pass_floor(&reports, 1), "1/3 clears a floor of 1");
    assert!(below_pass_floor(&reports, 2), "1/3 is below a floor of 2");
}

/// A `NOT-RUN` row counts in the denominator. A run that asked for four tasks
/// and launched one must not clear a floor by shrinking the task set — the
/// silent under-measurement the whole `requested` mechanism exists to stop.
#[test]
fn a_not_run_task_still_counts_against_the_floor() {
    let reports = vec![
        TrialReport {
            task: "ran".into(),
            reward: Some(1.0),
            tool_calls: 2,
            ..Default::default()
        },
        TrialReport {
            task: "never-launched".into(),
            not_run: true,
            ..Default::default()
        },
    ];
    let t = tally(&reports);
    assert_eq!(t.total, 2, "the denominator is what was asked for");
    assert_eq!(t.solved, 1);
    assert_eq!(t.loop_broken, 1, "NOT-RUN gates red on its own");
    assert!(below_pass_floor(&reports, 2), "1/2 is below a floor of 2");
}

/// #1299, hole 1: a trial that did real work and then died reports `CRASHED`,
/// not `ran (unsolved)`. The old wording said the agent tried the task and got
/// it wrong; what actually happened is that the machine killed it.
#[test]
fn a_crash_after_real_work_is_not_an_ordinary_failure() {
    let job = tempfile::tempdir().expect("job dir");
    let trial = trial_dir(
        job.path(),
        "died__t1",
        &[
            r#"{"type":"stage","name":"execute"}"#,
            r#"{"type":"tool_start","call":{"name":"edit_file"}}"#,
            r#"{"type":"step_usage","cost_usd":0.02}"#,
        ],
    );
    write_result(
        &trial,
        serde_json::json!({
            "task_name": "died",
            "exception_info": exception(
                "NonZeroAgentExitCodeError",
                "Stella exited with code 137; see stella-exit-cause.json",
            ),
        }),
    );

    let requested = vec!["died".to_string()];
    let analysis = analyze(job.path(), Some(one_trial_each(&requested)));
    let report = only(&analysis);
    assert_eq!(report.loop_verdict(), "CRASHED");
    assert!(report.crashed());
    assert!(
        report
            .crash
            .as_deref()
            .is_some_and(|crash| crash.starts_with("NonZeroAgentExitCodeError: Stella exited")),
        "{:?}",
        report.crash
    );
    assert!(
        !report.loop_broken(),
        "a trial the machine killed is not evidence about the loop"
    );
    assert_eq!(analysis.tally().crashed, 1);
    assert_eq!(analysis.tally().solved, 0);
}

/// #1299: naming a failure better must never make the gate weaker. A crash
/// that happened before any work is still `SILENT-DEATH` and still red — it
/// simply now carries the explanation it never had.
#[test]
fn a_crash_before_any_work_stays_red_and_gains_its_explanation() {
    let job = tempfile::tempdir().expect("job dir");
    let trial = trial_dir(
        job.path(),
        "vanished__t1",
        &[
            r#"{"type":"stage","name":"triage"}"#,
            r#"{"type":"stage","name":"plan"}"#,
        ],
    );
    write_result(
        &trial,
        serde_json::json!({
            "task_name": "vanished",
            "exception_info": exception("AgentTimeoutError", "agent exceeded its time limit"),
        }),
    );

    let requested = vec!["vanished".to_string()];
    let analysis = analyze(job.path(), Some(one_trial_each(&requested)));
    let report = only(&analysis);
    assert_eq!(
        report.loop_verdict(),
        "SILENT-DEATH",
        "a zero-work death keeps its gating verdict"
    );
    assert!(report.loop_broken());
    assert!(
        report.crash.is_some(),
        "the crash is still on the row — the verdict column holds one word, \
         and the explanation is a separate fact"
    );
    assert_eq!(
        analysis.tally().crashed,
        1,
        "counted from the crash record, not from the verdict it lost"
    );
}

/// #1299: `exception.txt` and the adapter's SIGKILL post-mortem (#1178) are
/// read when `result.json` never landed — the trial that crashed hardest is
/// the one least likely to have written a structured record.
#[test]
fn a_crash_is_read_from_the_files_a_dying_trial_manages_to_leave() {
    let job = tempfile::tempdir().expect("job dir");
    let trial = trial_dir(
        job.path(),
        "oom__t1",
        &[r#"{"type":"tool_start","call":{"name":"write_file"}}"#],
    );
    std::fs::write(
        trial.join("exception.txt"),
        "NonZeroAgentExitCodeError: Stella exited with code 137\n",
    )
    .expect("write");
    std::fs::write(
        trial.join("agent").join("stella-exit-cause.json"),
        serde_json::json!({
            "return_code": 137,
            "verdict": "oom-kill",
            "detail": "the container cgroup recorded oom_kill 1",
        })
        .to_string(),
    )
    .expect("write");

    let requested = vec!["oom".to_string()];
    let analysis = analyze(job.path(), Some(one_trial_each(&requested)));
    let report = only(&analysis);
    assert_eq!(report.loop_verdict(), "CRASHED");
    assert!(
        report.crash.as_deref().is_some_and(|c| c.contains("137")),
        "{:?}",
        report.crash
    );
    assert!(
        report
            .exit_cause
            .as_deref()
            .is_some_and(|cause| cause.starts_with("oom-kill")),
        "an OOM and a teardown both exit 137, and they have different fixes: {:?}",
        report.exit_cause
    );
}

/// #1299, hole 2: the denominator is the *trials* asked for, not the tasks. A
/// task that produced two of three trial dirs is two-thirds measured, and the
/// missing third is a row rather than a smaller divisor.
#[test]
fn a_partially_launched_task_is_still_measured_over_what_was_requested() {
    let job = tempfile::tempdir().expect("job dir");
    for name in ["half__t1", "half__t2"] {
        let trial = trial_dir(
            job.path(),
            name,
            &[r#"{"type":"tool_start","call":{"name":"edit_file"}}"#],
        );
        write_reward(&trial, "1.0");
    }

    let requested = vec!["half".to_string()];
    let analysis = analyze(
        job.path(),
        Some(Requested {
            tasks: &requested,
            trials_per_task: 3,
        }),
    );
    let counts = analysis.tally();
    assert_eq!(counts.total, 3, "the denominator is what was asked for");
    assert_eq!(counts.solved, 2);
    assert_eq!(
        counts.loop_broken, 1,
        "the trial that never launched gates red"
    );
    assert!(
        below_pass_floor(&analysis.trials, 3),
        "2 of 3 requested trials passed; over the 2 that ran it would read as all of them"
    );

    let record = analysis.reconciliation.expect("a requested run reconciles");
    assert_eq!(record.missing(), vec![("half", 1)]);
    assert!(!record.is_clean());
    assert!(
        !record.contaminated(),
        "a missing trial already has a row and a red gate; it is not a bad denominator"
    );
}

/// #1299, hole 2: a second run into the same job directory is reported, not
/// folded in. The extra trials render as ordinary healthy rows, so this is the
/// only place a reader would ever learn the figures cover two runs.
#[test]
fn a_second_run_in_one_job_dir_is_reported_rather_than_absorbed() {
    let job = tempfile::tempdir().expect("job dir");
    for name in ["twice__old", "twice__new"] {
        trial_dir(
            job.path(),
            name,
            &[r#"{"type":"tool_start","call":{"name":"edit_file"}}"#],
        );
    }

    let requested = vec!["twice".to_string()];
    let analysis = analyze(job.path(), Some(one_trial_each(&requested)));
    let record = analysis
        .reconciliation
        .as_ref()
        .expect("a requested run reconciles");
    assert_eq!(record.surplus(), vec![("twice", 1)]);
    assert!(
        record.contaminated(),
        "the run is scored over trials it did not ask for"
    );
    assert!(
        record
            .findings()
            .iter()
            .any(|finding| finding.contains("--job-name")),
        "the finding says how to fix it: {:?}",
        record.findings()
    );
}

/// #1299, hole 2: harbor truncates a task name to 32 characters when it names
/// the trial directory. Matching on the untruncated name produced *two* wrong
/// answers at once — every trial skipped as another run's, and the task that
/// really ran reported `NOT-RUN`.
#[test]
fn a_task_name_too_long_for_its_directory_still_reconciles() {
    let task = "a-terminal-bench-task-with-a-really-quite-long-name";
    let prefix = trial_dir_prefix(task);
    assert_eq!(prefix.chars().count(), 32, "harbor's own truncation width");
    assert_ne!(prefix, task);

    let job = tempfile::tempdir().expect("job dir");
    let trial = trial_dir(
        job.path(),
        &format!("{prefix}__t1"),
        &[r#"{"type":"tool_start","call":{"name":"edit_file"}}"#],
    );
    write_reward(&trial, "1.0");

    let requested = vec![task.to_string()];
    let analysis = analyze(job.path(), Some(one_trial_each(&requested)));
    let report = only(&analysis);
    assert!(!report.not_run, "the task ran; only its name did not fit");
    assert_eq!(
        report.task, task,
        "the row carries the name that was asked for"
    );
    assert_eq!(report.loop_verdict(), "solved");
    let record = analysis.reconciliation.expect("a requested run reconciles");
    assert!(record.is_clean(), "{:?}", record.findings());
}

/// #1299, hole 3: a multi-step trial is read. Harbor relocates each step's
/// logs into `steps/<name>/agent/` and removes the trial-root `agent/`, which
/// used to make every such trial report "no event stream".
#[test]
fn a_multi_step_trial_is_folded_from_its_step_logs() {
    let job = tempfile::tempdir().expect("job dir");
    let trial = job.path().join("staged__t1");
    for (step, events) in [
        (
            "01-setup",
            ev(&[
                r#"{"type":"stage","name":"triage"}"#,
                r#"{"type":"tool_start","call":{"name":"read_file"}}"#,
                r#"{"type":"step_usage","cost_usd":0.01}"#,
                r#"{"type":"complete"}"#,
            ]),
        ),
        (
            "02-solve",
            ev(&[
                r#"{"type":"stage","name":"execute"}"#,
                r#"{"type":"tool_start","call":{"name":"edit_file"}}"#,
                r#"{"type":"step_usage","cost_usd":0.03}"#,
                r#"{"type":"complete"}"#,
            ]),
        ),
    ] {
        let agent = trial.join("steps").join(step).join("agent");
        std::fs::create_dir_all(&agent).expect("mkdir");
        std::fs::write(agent.join("stella-events.jsonl"), events).expect("write");
    }
    // No trial-root `verifier/` either: harbor folds the per-step rewards by
    // the task's strategy and records the result here. Taking its aggregate is
    // what keeps the harness and the dataset agreeing on the score.
    write_result(
        &trial,
        serde_json::json!({
            "task_name": "staged",
            "verifier_result": {"rewards": {"reward": 1.0}},
            "step_results": [{"step_name": "01-setup"}, {"step_name": "02-solve"}],
        }),
    );

    let requested = vec!["staged".to_string()];
    let analysis = analyze(job.path(), Some(one_trial_each(&requested)));
    let report = only(&analysis);
    assert_eq!(report.step_names, vec!["01-setup", "02-solve"]);
    assert_eq!(report.tool_calls, 2, "counters sum across steps");
    assert_eq!(report.file_writes, 1);
    assert_eq!(report.model_calls, 2);
    assert!((report.spend_usd - 0.04).abs() < 1e-9);
    assert_eq!(report.stages, vec!["triage", "execute"]);
    assert!(
        report.passed(),
        "the reward came off harbor's own aggregate"
    );
    assert_eq!(report.loop_verdict(), "solved");
    assert!(!report.zero_work);
}

/// #1299: the fold takes the **last** step's ending, not any step's. A trial
/// that completed step one and vanished in step two ended by vanishing, and an
/// `any` fold would report it as having said goodbye.
#[test]
fn a_multi_step_fold_ends_where_the_last_step_ended() {
    let finished = distill_events(
        "staged",
        &ev(&[
            r#"{"type":"tool_start","call":{"name":"edit_file"}}"#,
            r#"{"type":"complete"}"#,
        ]),
    );
    let vanished = distill_events("staged", &ev(&[r#"{"type":"stage","name":"plan"}"#]));

    let folded = fold_steps(&[finished.clone(), vanished.clone()]);
    assert!(
        !folded.terminal_event,
        "the trial ended in the step that vanished"
    );
    assert!(
        !folded.zero_work,
        "one step doing no work is normal in a multi-step task; the trial did work"
    );
    assert_eq!(folded.loop_verdict(), "ran (unsolved)");

    // …and the other order is a trial that finished, however quiet its start.
    let folded = fold_steps(&[vanished, finished]);
    assert!(folded.terminal_event);
}

/// #1299: step order comes from harbor's `step_results`, not from sorting the
/// directory names — the fold reads the last step's ending, so the wrong order
/// judges the wrong step.
#[test]
fn step_order_follows_harbors_record_not_the_alphabet() {
    let job = tempfile::tempdir().expect("job dir");
    let trial = job.path().join("ordered__t1");
    for (step, events) in [
        (
            "zulu",
            ev(&[r#"{"type":"tool_start","call":{"name":"edit_file"}}"#]),
        ),
        ("alpha", ev(&[r#"{"type":"complete"}"#])),
    ] {
        let agent = trial.join("steps").join(step).join("agent");
        std::fs::create_dir_all(&agent).expect("mkdir");
        std::fs::write(agent.join("stella-events.jsonl"), events).expect("write");
    }
    write_result(
        &trial,
        serde_json::json!({
            "task_name": "ordered",
            "step_results": [{"step_name": "zulu"}, {"step_name": "alpha"}],
        }),
    );

    let requested = vec!["ordered".to_string()];
    let analysis = analyze(job.path(), Some(one_trial_each(&requested)));
    let report = only(&analysis);
    assert_eq!(
        report.step_names,
        vec!["zulu", "alpha"],
        "execution order, which a name sort would have reversed"
    );
    assert!(report.terminal_event, "`alpha` ran last and completed");
}

/// #1299: the JSON a CI job trends carries the reconciliation. An artifact
/// with the rows but not the check is the artifact that publishes the
/// flattering percentage.
#[test]
fn the_json_report_carries_the_denominator_check() {
    let job = tempfile::tempdir().expect("job dir");
    trial_dir(
        job.path(),
        "ran__t1",
        &[r#"{"type":"tool_start","call":{"name":"edit_file"}}"#],
    );
    let requested = vec!["ran".to_string(), "never".to_string()];
    let analysis = analyze(job.path(), Some(one_trial_each(&requested)));

    let json = serde_json::to_value(json_report(&analysis)).expect("serialize");
    assert_eq!(json["tally"]["total"], 2);
    assert_eq!(json["tally"]["loop_broken"], 1);
    assert_eq!(json["tally"]["crashed"], 0);
    assert_eq!(json["reconciliation"]["observed"]["never"], 0);
    assert_eq!(json["trials"].as_array().expect("rows").len(), 2);
}

/// `passed()` is exactly the reward test the verdict already made — a partial
/// reward is not a pass, and a missing one is not a failure.
#[test]
fn passed_is_reward_one_and_nothing_else() {
    let with = |reward| TrialReport {
        reward,
        ..Default::default()
    };
    assert!(with(Some(1.0)).passed());
    assert!(!with(Some(0.5)).passed(), "partial credit is not a pass");
    assert!(!with(Some(0.0)).passed());
    assert!(!with(None).passed(), "never verified is not a pass");
    assert_eq!(tally(&[with(Some(0.5)), with(None)]).solved, 0);
}
