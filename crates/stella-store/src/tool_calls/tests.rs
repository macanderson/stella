// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for the live `tool_calls` projection.
//!
//! The two this module exists to fix are [`count_is_visible_while_the_turn_is_still_running`]
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
        sub_agent_id: None,
    }
}

fn ok_result(call_id: &str, content: &str, duration_ms: u64) -> AgentEvent {
    AgentEvent::ToolResult {
        call_id: call_id.into(),
        output: ToolOutput::Ok {
            content: content.into(),
            data: None,
        },
        duration_ms,
        speculated: false,
        sub_agent_id: None,
    }
}

fn err_result(call_id: &str, message: &str) -> AgentEvent {
    AgentEvent::ToolResult {
        call_id: call_id.into(),
        output: ToolOutput::error(message),
        duration_ms: 1,
        speculated: false,
        sub_agent_id: None,
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
    assert_eq!(
        recovered[2].2, "abandoned",
        "a call that never returned is abandoned, not a tool error (#3146)"
    );
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
    assert_eq!(
        settled[0].2, "abandoned",
        "the interrupt sweep settles to abandoned, never to error (#3146)"
    );
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

/// **The #4033 witness.** Two steps that each announce `read_file:0` are two
/// calls, and must project two rows.
///
/// `call_id` is only unique within one model *response*: several providers
/// mint it as `{tool_name}:{index_within_response}`, so the first read of
/// every response carries the same id. Keyed on `(execution_id, call_id)` this
/// projection read the second announcement as a re-announcement of the first
/// and updated that row in place — one observed execution projected 4 rows
/// from 176 calls, and 12.2% of a workspace's calls were erased.
///
/// Fails against the pre-v28 key, which projects one row here.
#[test]
fn two_steps_announcing_one_call_id_project_two_rows() {
    let (store, id) = fixture();
    let read = |offset: i64| AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "read_file:0".into(),
            name: "read_file".into(),
            input: serde_json::json!({ "path": "deck_ui.rs", "offset": offset }),
        },
        sub_agent_id: None,
    };
    // Step 1 reads a window and gets its result; step 2 reads the next one.
    store.record_event(id, 0, &read(1)).unwrap();
    store
        .record_event(id, 1, &ok_result("read_file:0", "lines 1-40", 5))
        .unwrap();
    store.record_event(id, 2, &read(41)).unwrap();
    store
        .record_event(id, 3, &ok_result("read_file:0", "lines 41-80", 7))
        .unwrap();

    let folded = rows(&store, id);
    assert_eq!(
        folded.len(),
        2,
        "two announcements are two calls, not one re-announcement: {folded:?}"
    );
    let args: Vec<String> = {
        let conn = store.lock();
        let mut stmt = conn
            .prepare("SELECT args_json FROM tool_calls WHERE execution_id = ?1 ORDER BY seq ASC")
            .expect("prepare");
        let mapped = stmt
            .query_map(params![id], |r| r.get::<_, String>(0))
            .expect("query");
        mapped.map(|r| r.expect("row")).collect()
    };
    assert!(args[0].contains("\"offset\":1"), "{args:?}");
    assert!(args[1].contains("\"offset\":41"), "{args:?}");
    // Each result settled its own call rather than overwriting the other's.
    let sizes: Vec<i64> = {
        let conn = store.lock();
        let mut stmt = conn
            .prepare("SELECT bytes_out FROM tool_calls WHERE execution_id = ?1 ORDER BY seq ASC")
            .expect("prepare");
        let mapped = stmt
            .query_map(params![id], |r| r.get::<_, i64>(0))
            .expect("query");
        mapped.map(|r| r.expect("row")).collect()
    };
    assert_eq!(sizes, vec![10, 11], "each result settled its own row");
}

/// Two calls sharing an id *within one response* are also two calls — the
/// engine's dispatch loop answers them separately ("an id-keyed set would let
/// one answered duplicate silently absorb the other"), and the projection must
/// not re-merge what dispatch kept apart.
///
/// Their results are indistinguishable by id, so they settle oldest-open
/// first. That is the only available pairing, and it never erases either call.
#[test]
fn duplicate_ids_within_one_response_stay_two_rows() {
    let (store, id) = fixture();
    store
        .record_event(id, 0, &start("dup", "read_file"))
        .unwrap();
    store
        .record_event(id, 1, &start("dup", "read_file"))
        .unwrap();
    store
        .record_event(id, 2, &ok_result("dup", "first", 3))
        .unwrap();
    store
        .record_event(id, 3, &err_result("dup", "second"))
        .unwrap();

    let folded = rows(&store, id);
    assert_eq!(
        folded.len(),
        2,
        "neither duplicate absorbed the other: {folded:?}"
    );
    assert_eq!(folded[0].2, "ok", "the first result settled the older call");
    assert_eq!(folded[1].2, "error", "the second settled the younger");
}

/// The *same* announcement folded twice is still one call: the row is keyed on
/// the event's own `seq`, so a re-fold refreshes it in place rather than
/// minting a second. This is what keeps the repair path idempotent.
#[test]
fn re_folding_one_announcement_keeps_its_single_row() {
    let (store, id) = fixture();
    store
        .record_event(id, 0, &start("c1", "read_file"))
        .unwrap();
    store.record_event(id, 1, &start("c2", "bash")).unwrap();
    store.record_event(id, 2, &ok_result("c1", "x", 9)).unwrap();
    store.record_event(id, 3, &ok_result("c2", "y", 4)).unwrap();
    let live = rows(&store, id);

    store.materialize_tool_calls(id).unwrap();
    store.materialize_tool_calls(id).unwrap();

    assert_eq!(rows(&store, id), live, "re-folding twice changes nothing");
    assert_eq!(live.len(), 2);
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

/// The durability level is a contract, not a preference: each level must map
/// to the exact pragma pair its documented failure model depends on. A
/// `paranoid` store that quietly omitted `fullfsync` would claim to survive
/// power loss while not doing the one thing that makes that true.
#[test]
fn every_durability_level_sets_the_pragmas_its_guarantee_rests_on() {
    use crate::migrations::pragmas::Durability;

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

/// #3145, end to end through the live writer: a classified failure lands its
/// class in `tool_calls.error_class`, so "bash's error rate excluding model
/// misuse" is an index seek over a token rather than a match on prose.
///
/// The three rows are the three facts the column has to keep apart: a
/// classified failure, an unclassified one (a site not yet audited — which
/// must NOT read as any class), and a success.
#[test]
fn a_classified_error_lands_its_class_in_the_projection() {
    use stella_protocol::ErrorClass;
    let (store, id) = fixture();
    store
        .record_event(id, 0, &start("c1", "read_file"))
        .unwrap();
    store
        .record_event(
            id,
            1,
            &AgentEvent::ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::classified_error(
                    ErrorClass::InvalidInput,
                    "missing required field `path`",
                ),
                duration_ms: 1,
                speculated: false,
                sub_agent_id: None,
            },
        )
        .unwrap();
    store.record_event(id, 2, &start("c2", "bash")).unwrap();
    store
        .record_event(id, 3, &err_result("c2", "boom"))
        .unwrap();
    store.record_event(id, 4, &start("c3", "grep")).unwrap();
    store
        .record_event(id, 5, &ok_result("c3", "hit", 1))
        .unwrap();

    let classes = |store: &Store| -> Vec<(String, String)> {
        let conn = store.lock();
        let mut stmt = conn
            .prepare(
                "SELECT name, error_class FROM tool_calls \
                 WHERE execution_id = ?1 ORDER BY seq ASC",
            )
            .expect("prepare");
        let mapped = stmt
            .query_map(params![id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .expect("query");
        mapped.map(|r| r.expect("row")).collect()
    };

    // An unaudited site is unclassified, NOT `internal` — an error-rate
    // ceiling must not read our unfinished audit as our defects.
    let expected = vec![
        ("read_file".to_string(), "invalid_input".to_string()),
        ("bash".to_string(), String::new()),
        ("grep".to_string(), String::new()),
    ];
    assert_eq!(
        classes(&store),
        expected,
        "the live writer projects the class from the event's ToolOutput"
    );

    // The repair fold re-derives the same answer from the log — the whole
    // reason the log is the source of truth and this table is not.
    store.materialize_tool_calls(id).expect("re-fold");
    assert_eq!(
        classes(&store),
        expected,
        "the repair fold agrees with the live writer"
    );
}

/// A finished turn can still hold calls that never returned (the turn-end
/// fold settles them). The usage rollup's per-tool histogram must charge the
/// tool for its real failures and NOT for the turn's abandonments — before
/// #3146 both counted as `errors`, so every interrupt inflated exactly the
/// per-tool error rate a reliability ceiling reads.
#[test]
fn rollup_bucket_counts_errors_but_not_abandonment() {
    let (store, id) = fixture();
    store.record_event(id, 0, &start("c1", "bash")).unwrap();
    store
        .record_event(id, 1, &err_result("c1", "boom"))
        .unwrap();
    store.record_event(id, 2, &start("c2", "bash")).unwrap();
    store.materialize_tool_calls(id).unwrap();
    store.finish_execution(id, "completed", 0.0).unwrap();

    let rollup = store
        .execution_rollup(id, std::path::Path::new("/tmp/workspace"))
        .unwrap()
        .expect("a finished, accounted execution rolls up");
    let bucket = rollup
        .tool_histogram
        .iter()
        .find(|b| b.tool == "bash")
        .expect("bash bucket");
    assert_eq!(
        (bucket.calls, bucket.errors),
        (2, 1),
        "abandonment is a fact about the turn, not the tool (#3146)"
    );
}

/// #4550's project-side half: the rollup carries the classified split of the
/// errors it counts, so the hub can key an error rate by class with no string
/// ever matched. Abandonment stays out of the split for the same reason it
/// stays out of `errors` (#3146), and an unaudited site rides as `''` — not
/// as any class.
#[test]
fn rollup_splits_errors_by_class_and_still_ignores_abandonment() {
    use stella_protocol::ErrorClass;
    let (store, id) = fixture();
    store.record_event(id, 0, &start("c1", "bash")).unwrap();
    store
        .record_event(
            id,
            1,
            &AgentEvent::ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::classified_error(ErrorClass::Environment, "exit 1"),
                duration_ms: 1,
                speculated: false,
                sub_agent_id: None,
            },
        )
        .unwrap();
    store.record_event(id, 2, &start("c2", "bash")).unwrap();
    store
        .record_event(id, 3, &err_result("c2", "boom"))
        .unwrap();
    // Announced, never returned: settles to abandoned at the turn-end fold.
    store.record_event(id, 4, &start("c3", "bash")).unwrap();
    store.materialize_tool_calls(id).unwrap();
    store.finish_execution(id, "completed", 0.0).unwrap();

    let rollup = store
        .execution_rollup(id, std::path::Path::new("/tmp/workspace"))
        .unwrap()
        .expect("a finished, accounted execution rolls up");
    let split: Vec<(&str, i64)> = rollup
        .error_class_histogram
        .iter()
        .map(|b| (b.class.as_str(), b.errors))
        .collect();
    assert_eq!(
        split,
        vec![("", 1), ("environment", 1)],
        "classified and unaudited errors split apart; the abandoned call is neither"
    );
    assert_eq!(
        rollup
            .error_class_histogram
            .iter()
            .map(|b| b.errors)
            .sum::<i64>(),
        rollup.tool_histogram.iter().map(|b| b.errors).sum::<i64>(),
        "the split's total is exactly the errors column it splits"
    );
}

/// The repair fold keeps them apart too, or a turn-end re-materialization
/// would silently re-collapse what the live fold recorded correctly. Moved
/// here from `store::tests` with #4033: it is a projection test, and it
/// belongs beside the projection.
#[test]
fn materialize_keeps_two_announcements_sharing_a_call_id_apart() {
    let store = Store::in_memory().unwrap();
    let id = store
        .begin_execution("deck", "add a feature", "zai", "glm-5.2")
        .unwrap();

    // The same call_id announced twice by two different events: two calls.
    // `call_id` is unique only within one model response, so the repair fold
    // must key on the announcing event and keep them apart — folding them
    // erased 12.2% of one workspace's calls (#4033).
    store
        .record_event(
            id,
            0,
            &AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: "c1".into(),
                    name: "grep".into(),
                    input: serde_json::json!({"pattern": "first"}),
                },
                sub_agent_id: None,
            },
        )
        .unwrap();
    store
        .record_event(
            id,
            1,
            &AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: "c1".into(),
                    name: "grep".into(),
                    input: serde_json::json!({"pattern": "final"}),
                },
                sub_agent_id: None,
            },
        )
        .unwrap();
    store
        .record_event(
            id,
            2,
            &AgentEvent::ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::Ok {
                    content: "hit\n".into(),
                    data: None,
                },
                duration_ms: 12,
                speculated: false,
                sub_agent_id: None,
            },
        )
        .unwrap();

    let n = store.materialize_tool_calls(id).unwrap();
    assert_eq!(n, 2, "two announcements are two calls (#4033)");
    assert_eq!(store.count("tool_calls").unwrap(), 2);
    let calls: Vec<(String, i64, String)> = {
        let conn = store.lock();
        let mut stmt = conn
            .prepare(
                "SELECT args_json, ok, state FROM tool_calls \
                 WHERE execution_id = ?1 ORDER BY seq ASC",
            )
            .unwrap();
        let mapped = stmt
            .query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        mapped.map(|r| r.unwrap()).collect()
    };
    assert_eq!(
        calls[0].0, r#"{"pattern":"first"}"#,
        "each announcement keeps its own arguments"
    );
    assert_eq!(calls[1].0, r#"{"pattern":"final"}"#);
    // Only one result was delivered, and it settles the older open call; the
    // younger one is still outstanding, which is what abandonment means.
    assert_eq!(calls[0].1, 1, "the result attached to the call it answers");
    assert_eq!(
        calls[1].2, "abandoned",
        "an unanswered announcement is not silently merged away"
    );
}

/// **The witness for #4624.** The projection records which delegate ran each
/// call, `NULL` for the lead's own, and the repair fold agrees with the live
/// path about it.
///
/// Fails before this change: there was no column, so a child's calls sat
/// under the parent execution id indistinguishable from the lead's — the
/// table that answers "what did this turn do" could not answer "which of
/// these did child X do".
#[test]
fn a_delegates_calls_are_attributed_to_it_and_the_leads_read_null() {
    let (store, id) = fixture();

    let child = |call_id: &str| AgentEvent::ToolStart {
        call: ToolCall {
            call_id: call_id.into(),
            name: "search".into(),
            input: serde_json::json!({ "query": "retry" }),
        },
        sub_agent_id: Some("search-1".into()),
    };
    let child_result = |call_id: &str| AgentEvent::ToolResult {
        call_id: call_id.into(),
        output: ToolOutput::Ok {
            content: "retry.rs".into(),
            data: None,
        },
        duration_ms: 30,
        speculated: false,
        sub_agent_id: Some("search-1".into()),
    };

    for (seq, event) in [
        start("c1", "read_file"),
        ok_result("c1", "fn a() {}", 12),
        child("c2"),
        child_result("c2"),
    ]
    .into_iter()
    .enumerate()
    {
        store.record_event(id, seq as u64, &event).unwrap();
    }

    let owners = |store: &Store| -> Vec<(String, Option<String>)> {
        let conn = store.lock();
        let mut stmt = conn
            .prepare(
                "SELECT name, sub_agent_id FROM tool_calls \
                 WHERE execution_id = ?1 ORDER BY seq ASC",
            )
            .unwrap();
        let mapped = stmt
            .query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        mapped.map(|r| r.unwrap()).collect()
    };
    let expected = vec![
        ("read_file".to_string(), None),
        ("search".to_string(), Some("search-1".to_string())),
    ];
    assert_eq!(owners(&store), expected, "the live path attributes both");

    // The repair fold re-derives the same answer from the same events. The
    // two writers disagreeing is how a repair rewrites history the live path
    // had already recorded correctly.
    store.materialize_tool_calls(id).unwrap();
    assert_eq!(owners(&store), expected, "and so does the re-fold");
}
