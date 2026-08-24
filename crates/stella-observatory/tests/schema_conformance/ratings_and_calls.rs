//! The ratings feed, and the live/repaired tool-call projections (#4486 split of `../schema_conformance.rs`).
use super::*;

/// #2443, against the real schema: the ratings feed counts what the model
/// graded, and the Improve tab's two panels agree about the same turn.
///
/// `crates/stella-observatory/src/db.rs`'s `HAS_SELF_REVIEW` is a SQL mirror of
/// `stella_store::SelfReviewRow::is_empty` — mirrored because this crate must
/// not link `stella-store` in production. This suite is what keeps the mirror
/// honest: the store below is built through the real migration path by the real
/// writers, so a column renamed underneath the predicate fails here.
///
/// The ids are read out of the payloads rather than assumed, which is also the
/// product statement: the turn the lifecycle route names as unreadable is
/// exactly the one the ratings feed must not present as an unrated reflection.
#[test]
fn the_ratings_feed_counts_only_graded_turns_against_the_real_schema() {
    let workspace = real_store_workspace();
    let root: &Path = workspace.path();

    let lifecycle: serde_json::Value =
        serde_json::from_slice(&respond(root, "/api/context-lifecycle").body).expect("json");
    let unreadable: Vec<i64> = lifecycle["unreadable_reflections"]
        .as_array()
        .expect("unreadable_reflections")
        .iter()
        .map(|r| r["execution_id"].as_i64().expect("execution_id"))
        .collect();
    assert!(
        !unreadable.is_empty(),
        "the fixture seeds a parse failure through the real writer: {lifecycle}"
    );

    let reflections: serde_json::Value =
        serde_json::from_slice(&respond(root, "/api/reflections").body).expect("json");
    let ratings = reflections["ratings"].as_array().expect("ratings");
    assert!(
        !ratings.is_empty(),
        "and a real self-review, which must still be listed: {reflections}"
    );
    for rating in ratings {
        let id = rating["execution_id"].as_i64().expect("execution_id");
        assert!(
            !unreadable.contains(&id),
            "execution {id} reflected unreadably and was never graded, so it is \
             not a rating: {rating}"
        );
        let prose = |key: &str| rating[key].as_str().is_some_and(|s| !s.trim().is_empty());
        assert!(
            !rating["self_rating"].is_null()
                || !rating["delivered"].is_null()
                || prose("what_went_well")
                || prose("what_to_improve")
                || prose("critique"),
            "a listed rating carries an assessment: {rating}"
        );
    }
}

/// The live projection, seen through the dashboard's own route.
///
/// This is the end-to-end statement of the v18 change: a turn that has made
/// tool calls but has **not finished** reports them. Before v18 the
/// projection was built by the turn finalizer, so this count was zero until
/// the run ended — and stayed zero forever if it never did.
#[test]
fn an_unfinished_execution_reports_its_tool_calls() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");
    let id = store
        .begin_execution("run", "still going", "anthropic", "opus")
        .expect("begin");
    let call = ToolCall {
        call_id: "c1".into(),
        name: "read_file".into(),
        input: serde_json::json!({ "path": "a.rs" }),
    };
    store
        .record_event(id, 0, &AgentEvent::ToolStart { call: call.clone() })
        .expect("start");
    store
        .record_event(
            id,
            1,
            &AgentEvent::ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::Ok {
                    content: "fn a() {}".into(),
                    data: None,
                },
                duration_ms: 5,
                speculated: false,
            },
        )
        .expect("result");
    store
        .record_event(
            id,
            2,
            &AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: "c2".into(),
                    name: "bash".into(),
                    input: serde_json::json!({ "command": "cargo test" }),
                },
            },
        )
        .expect("second start");
    // No finish_execution: the turn is still running, which is exactly when
    // an operator needs these numbers.

    let executions: serde_json::Value =
        serde_json::from_slice(&respond(dir.path(), "/api/executions").body).expect("json");
    assert_eq!(
        executions[0]["tool_calls"], 2,
        "an in-flight turn reports its calls: {executions}"
    );
    assert!(
        executions[0]["outcome"].is_null(),
        "and is still visibly unfinished: {executions}"
    );

    let tools: serde_json::Value =
        serde_json::from_slice(&respond(dir.path(), "/api/tools").body).expect("json");
    let names: Vec<&str> = tools
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(
        names.contains(&"read_file") && names.contains(&"bash"),
        "the leaderboard sees both live calls: {names:?}"
    );
}

/// A crashed run's calls come back from the log, through the dashboard.
///
/// The complement of the test above: the live write is what keeps the
/// projection current, and the repair fold is what makes losing it
/// survivable. Both have to be true at the route, not just in the store.
#[test]
fn a_crashed_execution_recovers_its_calls_through_the_api() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");
    let id = store
        .begin_execution("run", "died mid-turn", "anthropic", "opus")
        .expect("begin");
    for (seq, call_id) in ["c1", "c2", "c3"].iter().enumerate() {
        store
            .record_event(
                id,
                seq as u64,
                &AgentEvent::ToolStart {
                    call: ToolCall {
                        call_id: (*call_id).into(),
                        name: "bash".into(),
                        input: serde_json::json!({ "command": "true" }),
                    },
                },
            )
            .expect("start");
    }
    // Exactly the state a pre-v18 crash left behind: every event on disk,
    // and no projected row anywhere. Done with a raw connection rather than
    // a store method — "delete the projection" is not an operation the
    // production API should offer just so a test can stage a crash.
    {
        let raw =
            rusqlite::Connection::open(dir.path().join(".stella/private/store.db")).expect("open");
        raw.execute("DELETE FROM tool_calls WHERE execution_id = ?1", [id])
            .expect("wipe the projection");
    }
    let before: serde_json::Value =
        serde_json::from_slice(&respond(dir.path(), "/api/executions").body).expect("json");
    assert_eq!(before[0]["tool_calls"], 0, "the calls are invisible");

    let repaired = store.reconcile_interrupted_executions().expect("reconcile");

    assert_eq!(repaired, 1);
    let after: serde_json::Value =
        serde_json::from_slice(&respond(dir.path(), "/api/executions").body).expect("json");
    assert_eq!(
        after[0]["tool_calls"], 3,
        "every call is back, replayed from the log: {after}"
    );
}

/// An interrupted turn's in-flight call must not count against its tool.
///
/// The sweep settles a still-`running` call as `'abandoned'` (v24), and the
/// leaderboard reports it in its own column: before #3146 it landed in
/// `errors`, so every ctrl-C or SIGKILL charged an "error" to whatever tool
/// happened to be in flight — usually the slowest, most-watched ones.
#[test]
fn an_abandoned_call_is_not_a_tool_error_on_the_leaderboard() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");
    let id = store
        .begin_execution("run", "interrupted mid-tool", "anthropic", "opus")
        .expect("begin");
    store
        .record_event(
            id,
            0,
            &AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: "c1".into(),
                    name: "bash".into(),
                    input: serde_json::json!({ "command": "false" }),
                },
            },
        )
        .expect("start c1");
    store
        .record_event(
            id,
            1,
            &AgentEvent::ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::error("exit status 1"),
                duration_ms: 3,
                speculated: false,
            },
        )
        .expect("c1 fails for real");
    store
        .record_event(
            id,
            2,
            &AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: "c2".into(),
                    name: "bash".into(),
                    input: serde_json::json!({ "command": "sleep 60" }),
                },
            },
        )
        .expect("start c2");
    // The owner is gone: the sweep settles c2 as abandoned, not errored.
    store.mark_execution_interrupted(id).expect("sweep");

    let tools: serde_json::Value =
        serde_json::from_slice(&respond(dir.path(), "/api/tools").body).expect("json");
    let bash = tools
        .as_array()
        .expect("array")
        .iter()
        .find(|t| t["name"] == "bash")
        .cloned()
        .expect("bash row");
    assert_eq!(bash["calls"], 2, "both calls count: {bash}");
    assert_eq!(
        bash["errors"], 1,
        "only the real failure is an error (#3146): {bash}"
    );
    assert_eq!(
        bash["abandoned"], 1,
        "the abandonment is visible, apart from the error count: {bash}"
    );
    assert_eq!(
        bash["errors_by_class"],
        serde_json::json!({ "unclassified": 1 }),
        "the class split carries only the real failure — an abandoned call \
         lands in no bucket: {bash}"
    );
}

/// Classified errors split by class on the leaderboard (#4550).
///
/// `errors` stays the total it always was; `errors_by_class` partitions it by
/// the `ErrorClass` wire token, with the store's `''` default counted as
/// `"unclassified"` — a key outside that vocabulary, so no future class can
/// shadow it. The payoff asserted at the end: "errors for bash excluding
/// invalid_input" is a subtraction over the payload, with no error prose
/// consulted.
#[test]
fn tool_errors_split_by_error_class_on_the_leaderboard() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");
    let id = store
        .begin_execution("run", "three flavors of failure", "anthropic", "opus")
        .expect("begin");
    let outputs = [
        ToolOutput::classified_error(ErrorClass::InvalidInput, "not the tool's schema"),
        ToolOutput::classified_error(ErrorClass::Environment, "network unreachable"),
        ToolOutput::error("not audited into a class yet"),
    ];
    for (i, output) in outputs.into_iter().enumerate() {
        let call_id = format!("c{i}");
        store
            .record_event(
                id,
                (2 * i) as u64,
                &AgentEvent::ToolStart {
                    call: ToolCall {
                        call_id: call_id.clone(),
                        name: "bash".into(),
                        input: serde_json::json!({ "command": "false" }),
                    },
                },
            )
            .expect("start");
        store
            .record_event(
                id,
                (2 * i + 1) as u64,
                &AgentEvent::ToolResult {
                    call_id,
                    output,
                    duration_ms: 1,
                    speculated: false,
                },
            )
            .expect("result");
    }

    let tools: serde_json::Value =
        serde_json::from_slice(&respond(dir.path(), "/api/tools").body).expect("json");
    let bash = tools
        .as_array()
        .expect("array")
        .iter()
        .find(|t| t["name"] == "bash")
        .cloned()
        .expect("bash row");
    assert_eq!(bash["errors"], 3, "the total is unchanged: {bash}");
    assert_eq!(
        bash["errors_by_class"],
        serde_json::json!({ "environment": 1, "invalid_input": 1, "unclassified": 1 }),
        "and the split partitions it by class: {bash}"
    );
    let excluding_invalid_input = bash["errors"].as_i64().expect("errors")
        - bash["errors_by_class"]["invalid_input"]
            .as_i64()
            .expect("invalid_input");
    assert_eq!(
        excluding_invalid_input, 2,
        "errors that are not the model's own mistake, derived without \
         touching a message string: {bash}"
    );
}
