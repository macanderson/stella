// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for the live `tool_calls` projection.
//!
//! The load-bearing ones are [`count_is_visible_while_the_turn_is_still_running`]
//! and [`an_interrupted_execution_recovers_every_call_from_the_log`] — those
//! two are the bugs this module exists to fix, and each fails outright
//! against the pre-v18 write path.

use super::*;
use stella_protocol::{ToolCall, ToolOutput};

fn start(call_id: &str, name: &str) -> AgentEvent {
    AgentEvent::ToolStart {
        call: ToolCall {
            call_id: call_id.into(),
            name: name.into(),
            input: serde_json::json!({ "path": "a.rs" }),
        },
    }
}

fn ok_result(call_id: &str, content: &str, duration_ms: u64) -> AgentEvent {
    AgentEvent::ToolResult {
        call_id: call_id.into(),
        output: ToolOutput::Ok {
            content: content.into(),
        },
        duration_ms,
        speculated: false,
    }
}

fn err_result(call_id: &str, message: &str) -> AgentEvent {
    AgentEvent::ToolResult {
        call_id: call_id.into(),
        output: ToolOutput::Error {
            message: message.into(),
        },
        duration_ms: 1,
        speculated: false,
    }
}

/// Rows for one execution, ordered by call position.
fn rows(store: &Store, execution_id: i64) -> Vec<(i64, String, String, String)> {
    let conn = store.lock();
    let mut stmt = conn
        .prepare(
            "SELECT seq, name, state, error FROM tool_calls \
             WHERE execution_id = ?1 ORDER BY seq ASC",
        )
        .expect("prepare");
    let mapped = stmt
        .query_map(params![execution_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .expect("query");
    mapped.map(|r| r.expect("row")).collect()
}

fn fixture() -> (Store, i64) {
    let store = Store::in_memory().expect("store");
    let id = store
        .begin_execution("run", "prompt", "anthropic", "opus")
        .expect("begin");
    (store, id)
}

/// **The headline bug.** Before v18 the projection was built once, at turn
/// end, so a running turn reported zero tool calls no matter how many it had
/// made — which is precisely the number a dashboard exists to show.
#[test]
fn count_is_visible_while_the_turn_is_still_running() {
    let (store, id) = fixture();
    store
        .record_event(id, 0, &start("c1", "read_file"))
        .unwrap();
    store
        .record_event(id, 1, &ok_result("c1", "fn a() {}", 5))
        .unwrap();
    store.record_event(id, 2, &start("c2", "bash")).unwrap();

    // No finish_execution, no materialize — the turn is still in flight.
    let live = rows(&store, id);
    assert_eq!(live.len(), 2, "both calls are visible mid-turn: {live:?}");
    assert_eq!(live[0].2, "ok", "the returned call reads as ok");
    assert_eq!(
        live[1].2, "running",
        "the outstanding call reads as running, not as a failure"
    );
    assert_eq!(store.count("tool_calls").unwrap(), 2);
}

/// A running call is not a failed one. `ok` alone cannot say that, which is
/// why `state` exists — and `ok` stays in lockstep for pre-v18 readers.
#[test]
fn running_is_distinguishable_from_failed_and_ok_stays_in_lockstep() {
    let (store, id) = fixture();
    store.record_event(id, 0, &start("c1", "bash")).unwrap();
    store.record_event(id, 1, &start("c2", "bash")).unwrap();
    store
        .record_event(id, 2, &err_result("c2", "boom"))
        .unwrap();

    let conn = store.lock();
    let running_ok: i64 = conn
        .query_row(
            "SELECT ok FROM tool_calls WHERE execution_id = ?1 AND call_id = 'c1'",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    let failed: (String, String) = conn
        .query_row(
            "SELECT state, error FROM tool_calls WHERE execution_id = ?1 AND call_id = 'c2'",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(running_ok, 0, "a running call is not yet a success");
    assert_eq!(failed.0, "error");
    assert_eq!(failed.1, "boom");
}

/// **The durability bug.** A turn killed mid-flight never runs its finalizer,
/// so before v18 its calls existed in `events` and nowhere else — forever.
/// Reconciliation replays the log and recovers every one of them.
#[test]
fn an_interrupted_execution_recovers_every_call_from_the_log() {
    let (store, id) = fixture();
    // Simulate a pre-v18 stream (or a torn projection): events land, rows do
    // not. Deleting the projection is exactly the state a crash left behind.
    for (seq, event) in [
        start("c1", "read_file"),
        ok_result("c1", "x", 3),
        start("c2", "bash"),
        ok_result("c2", "y", 4),
        start("c3", "grep"),
    ]
    .iter()
    .enumerate()
    {
        store.record_event(id, seq as u64, event).unwrap();
    }
    store
        .lock()
        .execute("DELETE FROM tool_calls", [])
        .expect("wipe the projection");
    assert_eq!(store.count("tool_calls").unwrap(), 0);

    // The execution never finished — exactly what `finished_at IS NULL` means.
    assert_eq!(store.unfinished_executions().unwrap(), vec![id]);
    let repaired = store.reconcile_interrupted_executions().unwrap();

    assert_eq!(repaired, 1);
    let recovered = rows(&store, id);
    assert_eq!(recovered.len(), 3, "every call comes back: {recovered:?}");
    assert_eq!(recovered[2].2, "error");
    assert_eq!(
        recovered[2].3, ABANDONED,
        "the call that never returned is honest about why"
    );
}

/// Reconciliation runs at store open, where a *live* turn in another process
/// looks identical to a dead one. It must be safe either way: re-folding
/// writes what the live projection already wrote, and never invents an
/// outcome.
#[test]
fn reconciling_a_live_execution_is_a_no_op() {
    let (store, id) = fixture();
    store.record_event(id, 0, &start("c1", "bash")).unwrap();
    store
        .record_event(id, 1, &ok_result("c1", "out", 7))
        .unwrap();
    let before = rows(&store, id);

    store.reconcile_interrupted_executions().unwrap();

    assert_eq!(rows(&store, id), before, "nothing moved");
    let outcome: Option<String> = store
        .lock()
        .query_row(
            "SELECT outcome FROM executions WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(outcome, None, "a live turn is not declared dead");
}

/// Only the caller that knows the process is gone stamps the outcome — and
/// when it does, calls still marked running are settled rather than left
/// counting as in-flight on every future dashboard load.
#[test]
fn marking_interrupted_settles_running_calls_and_dates_from_the_log() {
    let (store, id) = fixture();
    store.record_event(id, 0, &start("c1", "bash")).unwrap();
    store.mark_execution_interrupted(id).unwrap();

    let settled = rows(&store, id);
    assert_eq!(settled[0].2, "error");
    assert_eq!(settled[0].3, ABANDONED);
    let (finished, outcome): (Option<String>, Option<String>) = store
        .lock()
        .query_row(
            "SELECT finished_at, outcome FROM executions WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(outcome.as_deref(), Some("interrupted"));
    assert!(finished.is_some(), "an interrupted run is closed out");
    assert!(store.unfinished_executions().unwrap().is_empty());
}

/// One `call_id` is one call however often the stream announces it. A second
/// row would double the count *and* split the result across two rows.
#[test]
fn a_re_announced_call_folds_into_one_row_keeping_its_position() {
    let (store, id) = fixture();
    store
        .record_event(id, 0, &start("c1", "read_file"))
        .unwrap();
    store.record_event(id, 1, &start("c2", "bash")).unwrap();
    // c1 re-announced under a corrected name, after c2 took position 1.
    store
        .record_event(id, 2, &start("c1", "read_file_v2"))
        .unwrap();
    store.record_event(id, 3, &ok_result("c1", "x", 9)).unwrap();

    let folded = rows(&store, id);
    assert_eq!(folded.len(), 2, "no second row for c1: {folded:?}");
    assert_eq!(folded[0].0, 0, "c1 keeps its original position");
    assert_eq!(folded[0].1, "read_file_v2", "the latest payload wins");
    assert_eq!(folded[0].2, "ok", "its result still attaches");
}

/// The live fold and the repair fold must agree, or turn-end re-materialization
/// would silently rewrite what the dashboard had been showing all turn.
#[test]
fn the_live_fold_and_the_repair_fold_agree() {
    let (store, id) = fixture();
    let events = [
        start("c1", "read_file"),
        ok_result("c1", "content", 3),
        start("c2", "mcp__srv__tool"),
        err_result("c2", "nope"),
        start("c3", "grep"),
    ];
    for (seq, event) in events.iter().enumerate() {
        store.record_event(id, seq as u64, event).unwrap();
    }
    // Settle the outstanding call the way a clean turn end would.
    store.mark_execution_interrupted(id).unwrap();
    let live = rows(&store, id);

    store.materialize_tool_calls(id).unwrap();

    assert_eq!(rows(&store, id), live, "the repair fold is a no-op here");
    assert_eq!(live[1].1, "mcp__srv__tool");
    let surface: String = store
        .lock()
        .query_row(
            "SELECT surface FROM tool_calls WHERE execution_id = ?1 AND seq = 1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(surface, "mcp", "the mcp__ prefix still classifies");
}

/// A result whose start never landed is still a call that happened. Dropping
/// it would undercount; a row with an unknown name is the smaller lie.
#[test]
fn a_result_without_a_start_still_counts() {
    let (store, id) = fixture();
    store
        .record_event(id, 0, &ok_result("orphan", "x", 2))
        .unwrap();

    let orphaned = rows(&store, id);
    assert_eq!(orphaned.len(), 1);
    assert_eq!(orphaned[0].1, "(unknown)");
    assert_eq!(orphaned[0].2, "ok");
}

/// The projection commits with the event that produced it. If the event
/// insert fails, no row may appear — otherwise the count would exceed the log
/// it claims to summarize.
#[test]
fn a_rejected_event_leaves_no_projected_row() {
    let (store, id) = fixture();
    store.record_event(id, 0, &start("c1", "bash")).unwrap();
    // seq 0 again: UNIQUE (execution_id, seq) rejects it.
    let dup = store.record_event(id, 0, &start("c2", "grep"));

    assert!(dup.is_err(), "a duplicate position is an error");
    let after = rows(&store, id);
    assert_eq!(after.len(), 1, "the rolled-back event projected nothing");
    assert_eq!(after[0].1, "bash");
}

/// Every state round-trips through storage, and an unrecognized one reads as
/// `Error` rather than panicking on bytes another build wrote.
#[test]
fn state_round_trips_and_degrades_safely() {
    for state in [
        ToolCallState::Running,
        ToolCallState::Ok,
        ToolCallState::Error,
    ] {
        assert_eq!(ToolCallState::parse(state.as_str()), state);
    }
    assert_eq!(
        ToolCallState::parse("from-the-future"),
        ToolCallState::Error
    );
    assert!(ToolCallState::Ok.is_ok());
    assert!(!ToolCallState::Running.is_ok());
}

/// Helper: a minimal successful `ToolCallRow` for the reader tests.
fn bash_row(call_id: &str, command: &str, ok: bool) -> ToolCallRow {
    ToolCallRow {
        call_id: call_id.into(),
        name: "bash".into(),
        surface: "native".into(),
        args_json: format!(
            "{{\"command\":{}}}",
            serde_json::to_string(command).unwrap()
        ),
        args_digest: call_id.into(),
        reason: String::new(),
        state: if ok {
            ToolCallState::Ok
        } else {
            ToolCallState::Error
        },
        error: String::new(),
        bytes_out: 0,
        duration_ms: 1,
    }
}

#[test]
fn recent_tool_calls_filters_by_name_and_returns_newest_first() {
    let store = Store::in_memory().unwrap();
    let e1 = store.begin_execution("run", "p", "zai", "glm-5.2").unwrap();
    store
        .record_tool_calls(
            e1,
            &[
                bash_row("c1", "jq '.a' a.json", true),
                ToolCallRow {
                    call_id: "c2".into(),
                    name: "read_file".into(),
                    surface: "native".into(),
                    args_json: "{}".into(),
                    args_digest: "c2".into(),
                    reason: String::new(),
                    state: ToolCallState::Ok,
                    error: String::new(),
                    bytes_out: 0,
                    duration_ms: 1,
                },
                bash_row("c3", "jq '.b' b.json", true),
            ],
        )
        .unwrap();
    let e2 = store.begin_execution("run", "p", "zai", "glm-5.2").unwrap();
    store
        .record_tool_calls(e2, &[bash_row("c4", "jq '.c' c.json", true)])
        .unwrap();

    // Newest execution first, then newest seq within an execution; read_file
    // is filtered out.
    let read = store.recent_tool_calls("bash", 10).unwrap();
    let ids: Vec<&str> = read.iter().map(|r| r.call_id.as_str()).collect();
    assert_eq!(ids, ["c4", "c3", "c1"]);
    assert!(read.iter().all(|r| r.name == "bash"));
    assert!(read[0].args_json.contains("jq '.c' c.json"));

    // The limit caps the window; 0 is empty.
    assert_eq!(store.recent_tool_calls("bash", 1).unwrap().len(), 1);
    assert!(store.recent_tool_calls("bash", 0).unwrap().is_empty());
    assert!(
        store
            .recent_tool_calls("nonexistent", 10)
            .unwrap()
            .is_empty()
    );
}

/// The durability level is a contract, not a preference: each level must map
/// to the exact pragma pair its documented failure model depends on. A
/// `paranoid` store that quietly omitted `fullfsync` would claim to survive
/// power loss while not doing the one thing that makes that true.
#[test]
fn every_durability_level_sets_the_pragmas_its_guarantee_rests_on() {
    use crate::migrations::Durability;

    // Empty and unrecognized both land on Full — a typo in an environment
    // variable must never silently downgrade a durability guarantee.
    for level in ["", "  ", "sloppy", "FULL", "full", " Full "] {
        assert_eq!(
            Durability::parse(level),
            Durability::Full,
            "{level:?} must resolve to Full"
        );
    }
    assert_eq!(Durability::parse("normal"), Durability::Normal);
    assert_eq!(Durability::parse("PARANOID"), Durability::Paranoid);

    // And the store actually applies the default. 2 is FULL, 1 is NORMAL;
    // the difference is whether a kernel panic loses committed telemetry.
    let store = Store::in_memory().expect("store");
    let synchronous: i64 = store
        .lock()
        .query_row("PRAGMA synchronous", [], |r| r.get(0))
        .expect("synchronous");
    assert_eq!(synchronous, 2, "the default is FULL (2), not NORMAL (1)");
}

/// #896: the projection records that a call happened, never what it answered.
/// `bytes_out` cannot tell "found three definitions" from "found none" — both
/// are `state = 'ok'` with a non-zero length — so any health metric whose
/// numerator is semantic has to read the answer back out of the log.
#[test]
fn answers_come_back_paired_with_their_arguments_newest_first() {
    let (store, id) = fixture();
    store
        .record_event(id, 0, &start("c1", "graph_query"))
        .unwrap();
    store
        .record_event(id, 1, &ok_result("c1", "- lib.rs::greet", 3))
        .unwrap();
    store.record_event(id, 2, &start("c2", "grep")).unwrap();
    store
        .record_event(id, 3, &ok_result("c2", "lib.rs:1:greet", 2))
        .unwrap();
    store
        .record_event(id, 4, &start("c3", "graph_query"))
        .unwrap();
    store
        .record_event(
            id,
            5,
            &ok_result("c3", "the code graph has no definitions for `x`", 1),
        )
        .unwrap();

    let answers = store.tool_call_answers("graph_query", 10).expect("answers");
    assert_eq!(answers.len(), 2, "only the named tool: {answers:?}");
    // Newest first, so a bounded read is a read of recent behaviour.
    assert!(answers[0].content.contains("has no definitions"));
    assert!(answers[1].content.contains("lib.rs::greet"));
    assert!(
        answers[0].args_json.contains("a.rs"),
        "arguments ride along"
    );
    assert!(answers.iter().all(|a| a.ok));

    assert_eq!(store.tool_call_answers("graph_query", 1).unwrap().len(), 1);
    assert!(
        store
            .tool_call_answers("graph_query", 0)
            .unwrap()
            .is_empty()
    );
    assert!(store.tool_call_answers("nope", 5).unwrap().is_empty());
}

/// A failed call carries its message as the answer, and a call that never
/// returned carries nothing — an abandoned turn is not an unresolved query.
#[test]
fn failed_calls_carry_their_message_and_abandoned_ones_are_omitted() {
    let (store, id) = fixture();
    store
        .record_event(id, 0, &start("c1", "graph_query"))
        .unwrap();
    store
        .record_event(id, 1, &err_result("c1", "code-graph query failed: boom"))
        .unwrap();
    // Announced, never returned.
    store
        .record_event(id, 2, &start("c2", "graph_query"))
        .unwrap();

    let answers = store.tool_call_answers("graph_query", 10).expect("answers");
    assert_eq!(answers.len(), 1, "{answers:?}");
    assert!(!answers[0].ok);
    assert!(answers[0].content.contains("boom"));
}

/// The adoption half of the metric: how often the graph was reached for at
/// all, next to the text tools it competes with.
#[test]
fn tool_call_counts_group_every_call_by_name() {
    let (store, id) = fixture();
    store.record_event(id, 0, &start("c1", "grep")).unwrap();
    store.record_event(id, 1, &ok_result("c1", "x", 1)).unwrap();
    store.record_event(id, 2, &start("c2", "grep")).unwrap();
    store.record_event(id, 3, &ok_result("c2", "y", 1)).unwrap();
    store
        .record_event(id, 4, &start("c3", "graph_query"))
        .unwrap();
    store.record_event(id, 5, &ok_result("c3", "z", 1)).unwrap();

    let counts = store.tool_call_counts().expect("counts");
    assert_eq!(
        counts[0],
        ("grep".to_string(), 2),
        "busiest first: {counts:?}"
    );
    assert!(counts.contains(&("graph_query".to_string(), 1)));
}
