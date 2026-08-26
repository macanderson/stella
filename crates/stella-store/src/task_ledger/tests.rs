// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The scripted session #5039's definition of done asks for: two tasks whose
//! work interleaves across two turns, and the two questions the store must be
//! able to answer about either of them.

use stella_protocol::{
    AgentEvent, FileChangeKind, ModelCallRole, TaskId, TaskItem, TaskStatus, ToolCall, ToolOutput,
    UsageIncompleteReason,
};

use crate::Store;

const SESSION: &str = "ses-ledger";

fn tool_start(call_id: &str, name: &str, task: Option<&str>) -> AgentEvent {
    AgentEvent::ToolStart {
        call: ToolCall {
            call_id: call_id.into(),
            name: name.into(),
            input: serde_json::json!({ "path": "src/auth.rs" }),
        },
        sub_agent_id: None,
        task_id: task.map(TaskId::new),
    }
}

fn tool_result(call_id: &str, task: Option<&str>) -> AgentEvent {
    AgentEvent::ToolResult {
        call_id: call_id.into(),
        output: ToolOutput::Ok {
            content: "ok".into(),
            data: None,
        },
        duration_ms: 12,
        speculated: false,
        sub_agent_id: None,
        task_id: task.map(TaskId::new),
    }
}

fn file_change(path: &str, task: Option<&str>) -> AgentEvent {
    AgentEvent::FileChange {
        path: path.into(),
        kind: FileChangeKind::Modified,
        added: 9,
        removed: 2,
        diff: None,
        minimal: true,
        task_id: task.map(TaskId::new),
    }
}

/// One committed model call, priced and metered.
fn step_usage(
    cost_usd: f64,
    input: u64,
    cached: u64,
    output: u64,
    task: Option<&str>,
) -> AgentEvent {
    AgentEvent::StepUsage {
        step: 0,
        turn_instance: Some(0),
        call_seq: Some(0),
        role: ModelCallRole::Worker,
        provider: "anthropic".into(),
        upstream_provider: None,
        output_text: None,
        model: "claude-fable-5".into(),
        input_tokens: input,
        output_tokens: output,
        cached_input_tokens: cached,
        cache_write_tokens: 0,
        reasoning_tokens: None,
        estimated_input_tokens: input,
        cost_usd,
        duration_ms: 900,
        retries: 0,
        tool_calls: 1,
        complete: true,
        finish_reason: None,
        effort: None,
        max_output_tokens: None,
        temperature: None,
        params: None,
        sub_agent_id: None,
        task_id: task.map(TaskId::new),
    }
}

/// A paid call that died without accounting for itself.
fn usage_incomplete(task: Option<&str>) -> AgentEvent {
    AgentEvent::UsageIncomplete {
        role: ModelCallRole::Worker,
        provider: "anthropic".into(),
        model: "claude-fable-5".into(),
        reason: UsageIncompleteReason::Timeout,
        duration_ms: 30_000,
        retries: Some(1),
        partial: None,
        sub_agent_id: None,
        task_id: task.map(TaskId::new),
    }
}

fn board_row(id: &str, subject: &str, status: TaskStatus) -> TaskItem {
    TaskItem {
        id: id.into(),
        subject: subject.into(),
        description: None,
        status,
        owner: None,
        contract: None,
    }
}

/// The session the assertions below read: two turns, three board tasks, and
/// work that interleaves rather than arriving in tidy blocks — which is the
/// case a timestamp window could never have separated.
fn scripted_session() -> Store {
    let store = Store::in_memory().expect("store");

    let first = store
        .begin_execution(
            "deck",
            "wire the auth redirect",
            "anthropic",
            "claude-fable-5",
        )
        .expect("first turn");
    store
        .set_execution_session(first, SESSION)
        .expect("stamp session");
    let stream = [
        // Untagged: the turn opened before any task started.
        tool_start("c0", "read_file", None),
        tool_result("c0", None),
        step_usage(0.01, 1_000, 0, 100, None),
        // Task 1 runs, and finishes.
        tool_start("c1", "edit_file", Some("1")),
        tool_result("c1", Some("1")),
        file_change("src/layout.rs", Some("1")),
        step_usage(0.04, 2_000, 1_000, 200, Some("1")),
        // Task 3 opens in the same turn — the interleave.
        tool_start("c2", "edit_file", Some("3")),
        tool_result("c2", Some("3")),
    ];
    for (seq, event) in stream.iter().enumerate() {
        store
            .record_event(first, seq as u64, event)
            .expect("record");
    }

    let second = store
        .begin_execution("deck", "keep going", "anthropic", "claude-fable-5")
        .expect("second turn");
    store
        .set_execution_session(second, SESSION)
        .expect("stamp session");
    let stream = [
        file_change("src/auth.rs", Some("3")),
        step_usage(0.06, 4_000, 3_000, 300, Some("3")),
        // One call for task 3 died without a usage envelope.
        usage_incomplete(Some("3")),
        step_usage(0.02, 1_500, 1_200, 90, Some("3")),
        // ...and one of task 2's calls, so the fold cannot be summing the
        // whole session and calling it task 3's.
        step_usage(0.50, 9_000, 0, 900, Some("2")),
    ];
    for (seq, event) in stream.iter().enumerate() {
        store
            .record_event(second, seq as u64, event)
            .expect("record");
    }

    store
        .record_task_board(
            second,
            Some(SESSION),
            &[
                board_row("1", "fold the rail", TaskStatus::Completed),
                board_row("2", "port the printer", TaskStatus::Pending),
                board_row("3", "wire the auth redirect", TaskStatus::InProgress),
            ],
            1,
        )
        .expect("mirror the board");
    store
}

/// **The witness.** "Task 3's events" — the question SPEC 7.1 calls a task's
/// evidence ledger, and the one nothing could answer before the tag existed.
///
/// On the old code every event was untagged, so this returns nothing at all;
/// the only way to attempt an answer was to guess a timestamp window, which
/// the interleave above makes unguessable — task 1's and task 3's work share a
/// turn, and task 2's cost lands between two of task 3's.
#[test]
fn the_store_answers_task_threes_events() {
    let store = scripted_session();
    let ledger = store
        .task_events(SESSION, &TaskId::new("3"))
        .expect("read the ledger");

    assert_eq!(ledger.skipped, 0);
    let tags: Vec<&str> = ledger
        .events
        .iter()
        .filter_map(|record| record.event.task_id())
        .map(TaskId::as_str)
        .collect();
    assert_eq!(
        tags, ["3"; 6],
        "every row of a task's ledger is that task's, and nothing else's"
    );

    // In stream order across both turns: the edit that opened at the end of
    // turn one, then turn two's change and its metering.
    let shape: Vec<String> = ledger
        .events
        .iter()
        .map(|record| record.event.type_tag().to_string())
        .collect();
    assert_eq!(
        shape,
        [
            "tool_start",
            "tool_result",
            "file_change",
            "step_usage",
            "usage_incomplete",
            "step_usage",
        ],
        "ordered by (execution_id, seq), so a task's ledger reads as it happened"
    );

    // And it names the edit — a ledger that could not say which file the task
    // touched would not be evidence of anything.
    let edited: Vec<&str> = ledger
        .events
        .iter()
        .filter_map(|record| match &record.event {
            stella_protocol::AgentEvent::FileChange { path, .. } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(edited, ["src/auth.rs"]);
}

/// **The witness's other half.** "Task 3's cost", in the shape SPEC 7.1 names.
#[test]
fn the_store_answers_task_threes_cost() {
    let store = scripted_session();
    let cost = store
        .task_cost(SESSION, &TaskId::new("3"))
        .expect("read the cost")
        .expect("task 3 is on the board");

    // `$` — task 3's two committed calls, and neither task 1's nor task 2's.
    assert!(
        (cost.spent_usd - 0.08).abs() < 1e-9,
        "spent {spent}",
        spent = cost.spent_usd
    );
    // `model calls` counts what landed; the call that died is counted apart.
    assert_eq!(cost.model_calls, 2);
    assert_eq!(cost.unaccounted_calls, 1);
    assert!(
        cost.is_lower_bound(),
        "a task with an unaccounted call reports a floor, not a total"
    );
    // `tok`, and the two counts `cache rd%` is computed from.
    assert_eq!(cost.total_tokens(), 5_890);
    assert_eq!(cost.input_tokens, 5_500);
    assert_eq!(cost.cached_input_tokens, 4_200);

    // `est remain`: task 1 is the only completed sibling, and it cost $0.04,
    // which task 3 has already passed — so the estimate floors at zero rather
    // than reporting a negative remainder.
    assert_eq!(cost.estimated_remaining_usd, Some(0.0));
}

/// A task nobody has started yet still has a row, reading zero — the state a
/// plan panel renders before any work happens. Its estimate is this session's
/// own evidence: one completed task, which cost $0.04.
#[test]
fn an_untouched_task_costs_nothing_and_is_estimated_from_its_finished_siblings() {
    let store = scripted_session();
    let cost = store
        .task_cost(SESSION, &TaskId::new("2"))
        .expect("read the cost")
        .expect("task 2 is on the board");
    assert!((cost.spent_usd - 0.50).abs() < 1e-9);
    assert_eq!(cost.model_calls, 1);
    assert!(!cost.is_lower_bound());
    // $0.04 mean over the one completed task, already exceeded.
    assert_eq!(cost.estimated_remaining_usd, Some(0.0));
}

/// A terminal task has nothing remaining, and says so as a fact rather than an
/// estimate — that is what makes a finished line read as finished.
#[test]
fn a_completed_task_has_nothing_remaining() {
    let store = scripted_session();
    let cost = store
        .task_cost(SESSION, &TaskId::new("1"))
        .expect("read the cost")
        .expect("task 1 is on the board");
    assert!((cost.spent_usd - 0.04).abs() < 1e-9);
    assert_eq!(cost.estimated_remaining_usd, Some(0.0));
}

/// With no finished sibling there is nothing to extrapolate from, and the
/// answer is absence. This is the clause that keeps `est remain` from becoming
/// the `det %` SPEC 6.1 dropped: a number nothing measures is not offered.
#[test]
fn an_estimate_with_no_finished_sibling_is_absent_rather_than_invented() {
    let store = Store::in_memory().expect("store");
    let turn = store
        .begin_execution("deck", "p", "anthropic", "claude-fable-5")
        .expect("turn");
    store.set_execution_session(turn, SESSION).expect("stamp");
    store
        .record_event(turn, 0, &step_usage(0.03, 100, 0, 10, Some("1")))
        .expect("record");
    store
        .record_task_board(
            turn,
            Some(SESSION),
            &[board_row("1", "the only task", TaskStatus::InProgress)],
            1,
        )
        .expect("mirror");

    let cost = store
        .task_cost(SESSION, &TaskId::new("1"))
        .expect("read")
        .expect("on the board");
    assert_eq!(cost.estimated_remaining_usd, None);
}

/// Untagged work is in no task's ledger and no task's cost — it is not
/// silently swept into whichever task ran nearest it.
#[test]
fn untagged_work_belongs_to_no_task() {
    let store = scripted_session();
    let total: f64 = store
        .session_task_costs(SESSION)
        .expect("read")
        .iter()
        .map(|c| c.spent_usd)
        .sum();
    // The session spent $0.63; one cent of it was before any task started.
    assert!((total - 0.62).abs() < 1e-9, "attributed {total}");
}

/// A tag whose board row a `/clear` removed still reports its evidence and its
/// cost: the journal is the audit trail and the mirror is not. With no row
/// there is no status, so there is no estimate either.
#[test]
fn a_tag_outlives_the_board_row_it_names() {
    let store = scripted_session();
    store
        .clear_session_tasks(SESSION)
        .expect("clear the mirror");

    let ledger = store
        .task_events(SESSION, &TaskId::new("3"))
        .expect("read the ledger");
    assert_eq!(ledger.events.len(), 6, "the evidence survives the clear");

    let cost = store
        .task_cost(SESSION, &TaskId::new("3"))
        .expect("read")
        .expect("the tag still has a cost");
    assert!((cost.spent_usd - 0.08).abs() < 1e-9);
    assert_eq!(
        cost.estimated_remaining_usd, None,
        "with no board row there is no status to project from"
    );
}

/// The ledger stops at the session boundary: another session's task 3 is a
/// different task, and the ordinal ids guarantee the collision.
#[test]
fn one_sessions_ledger_never_reaches_another() {
    let store = scripted_session();
    let other = store
        .begin_execution("deck", "elsewhere", "anthropic", "claude-fable-5")
        .expect("turn");
    store
        .set_execution_session(other, "ses-elsewhere")
        .expect("stamp");
    store
        .record_event(other, 0, &step_usage(9.99, 10, 0, 1, Some("3")))
        .expect("record");

    let cost = store
        .task_cost(SESSION, &TaskId::new("3"))
        .expect("read")
        .expect("on the board");
    assert!(
        (cost.spent_usd - 0.08).abs() < 1e-9,
        "another session's task 3 must not be folded in: {}",
        cost.spent_usd
    );
    assert!(
        store
            .task_events(SESSION, &TaskId::new("3"))
            .expect("read")
            .events
            .iter()
            .all(|record| record.execution_id != other)
    );
}

/// Board order, extended over tags the board has no row for — so a panel
/// renders `2` before `10`, which a lexical sort would not.
#[test]
fn costs_come_back_in_board_order() {
    let store = scripted_session();
    let turn = store
        .begin_execution("deck", "later", "anthropic", "claude-fable-5")
        .expect("turn");
    store.set_execution_session(turn, SESSION).expect("stamp");
    store
        .record_event(turn, 0, &step_usage(0.01, 10, 0, 1, Some("10")))
        .expect("record");

    let ids: Vec<String> = store
        .session_task_costs(SESSION)
        .expect("read")
        .iter()
        .map(|c| c.task_id.as_str().to_string())
        .collect();
    assert_eq!(ids, ["1", "2", "3", "10"]);
}
