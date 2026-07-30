// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **The schema-drift gate (#827).**
//!
//! `stella-observatory` deliberately does not link `stella-store` at runtime:
//! opening the store runs migrations, and migrations are writes, so an
//! observer that went through `Store::open` would mutate the thing it
//! observes. It hand-writes its SQL against those tables instead and opens
//! every file `SQLITE_OPEN_READ_ONLY`.
//!
//! That isolation is right, and it left a hole. The crate's own unit fixture
//! is a hand-written *subset* of the real schema, so a column the observatory
//! reads could be renamed or dropped in `stella-store` and the whole suite
//! would stay green while the live dashboard 500'd — the failure would first
//! be observed by a user, on their own telemetry, with no test having said a
//! word.
//!
//! This suite closes it. `stella-store` is a **dev**-dependency: the
//! production crate still links nothing, but the test builds its database
//! through the real `Store::open` migration path and drives every route
//! against it. A renamed column now fails here, at `cargo test`, instead of
//! at runtime.
//!
//! Two properties are asserted, and both are needed:
//!
//! 1. **No route errors.** A missing column surfaces as a 500, and
//!    [`respond`](stella_observatory::respond) turns any query failure into
//!    one.
//! 2. **Seeded data actually arrives.** Necessary because the production
//!    reader degrades a *missing table* to an empty payload on purpose (a
//!    fresh workspace must render, not fail). Without this half, dropping a
//!    table entirely would pass the no-500 check while silently blanking the
//!    dashboard.

use std::path::Path;

use stella_observatory::respond;
use stella_store::{
    AgentUseRow, ExecutionReflectionRow, FileTouchRow, McpUsageRow, MemoryCitationRow,
    ReflectionRow, SkillUsageRow, Store, TelemetryRow,
};

/// Every `/api/*` route the router serves, paired with the JSON pointer that
/// must resolve to seeded (non-empty) data.
///
/// `None` marks a route whose emptiness is legitimate in a store-only
/// fixture: the filesystem views read `.stella/` trees this test does not
/// build, and the fleet ledger lives in a different database written by a
/// different crate. They are still driven, because a route that panics or
/// 500s is a failure regardless of what it returns.
const ROUTES: &[(&str, Option<&str>)] = &[
    ("/api/meta", None),
    ("/api/v1/cursor", Some("/events")),
    ("/api/v1/snapshot", Some("/executions/0/id")),
    ("/api/overview", Some("/runs")),
    ("/api/executions", Some("/0/id")),
    ("/api/execution?id=1", Some("/steps/0/step")),
    ("/api/models", Some("/0/provider")),
    ("/api/tools", Some("/0/name")),
    ("/api/files", Some("/0/path")),
    ("/api/memory", Some("/citations/0/memory_id")),
    ("/api/mcp", Some("/0/server")),
    ("/api/fleet", None),
    ("/api/activity", Some("/0/day")),
    ("/api/projects", None),
    ("/api/hub-telemetry", None),
    ("/api/codegraph", None),
    ("/api/skills", None),
    ("/api/mcp-servers", None),
    ("/api/config", None),
    ("/api/memories", None),
    ("/api/explorations", None),
    ("/api/rules", Some("/db/0/rule_id")),
    ("/api/reflections", Some("/ratings/0/execution_id")),
];

/// One tool call's full event round-trip — the announcement and its result.
fn tool_round_trip(call_id: &str, name: &str) -> [stella_protocol::AgentEvent; 2] {
    use stella_protocol::{AgentEvent, ToolCall, ToolOutput};
    [
        AgentEvent::ToolStart {
            call: ToolCall {
                call_id: call_id.into(),
                name: name.into(),
                input: serde_json::json!({ "path": "src/lib.rs" }),
            },
        },
        AgentEvent::ToolResult {
            call_id: call_id.into(),
            output: ToolOutput::Ok {
                content: "fn a() {}".into(),
            },
            duration_ms: 12,
            speculated: false,
        },
    ]
}

/// Build a workspace whose `store.db` came from the **real** migration path,
/// seeded through the real write API — never hand-written DDL.
///
/// Every table the observatory reads is populated, so a query that stops
/// resolving is caught by the emptiness half of the gate rather than only by
/// the no-500 half.
fn real_store_workspace() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let store = Store::open(dir.path()).expect("Store::open runs the real migrations");

    let completed = store
        .begin_execution("run", "add a function", "zai", "glm-5.2")
        .expect("begin");
    // Tool calls arrive as events and are projected from them — seeding the
    // `tool_calls` table any other way would test a write path production
    // does not use.
    for (seq, event) in tool_round_trip("c1", "read_file").into_iter().enumerate() {
        store
            .record_event(completed, seq as u64, &event)
            .expect("tool event");
    }
    store
        .record_telemetry(
            completed,
            &TelemetryRow {
                step: 1,
                provider: "zai".into(),
                call_role: "worker".into(),
                model: "glm-5.2".into(),
                input_tokens: 1000,
                estimated_input_tokens: 900,
                output_tokens: 200,
                cache_read_tokens: 400,
                cache_miss_tokens: 600,
                cache_write_tokens: 50,
                cost_usd: 0.03,
                duration_ms: 1500,
                retries: 0,
                tool_calls: 2,
                usage_complete: true,
            },
        )
        .expect("telemetry");
    store
        .record_files_touched(
            completed,
            &[FileTouchRow {
                path: "src/lib.rs".into(),
                ops: "RU".into(),
                lines_added: 4,
                lines_removed: 1,
                events_json: "[]".into(),
            }],
        )
        .expect("files");
    store
        .record_memory_citations(
            completed,
            &[MemoryCitationRow {
                memory_id: "nod_abc".into(),
                useful_score: 4,
                truthful: true,
                remark: "load-bearing".into(),
            }],
        )
        .expect("citations");
    store
        .record_agent_uses(
            completed,
            &[AgentUseRow {
                agent: "reviewer".into(),
                version: 1,
                reason: "second opinion".into(),
            }],
        )
        .expect("agent uses");
    store
        .record_skill_usage(
            completed,
            &[SkillUsageRow {
                skill: "commit".into(),
                version: 2,
                reason: "ship it".into(),
            }],
        )
        .expect("skills");
    store
        .record_mcp_usage(
            completed,
            &[McpUsageRow {
                server: "github".into(),
                tool: "list_issues".into(),
                reason: "triage".into(),
                called_at_ms: 1_700_000_000_000,
            }],
        )
        .expect("mcp");
    store
        .record_execution_reflection(
            completed,
            &ExecutionReflectionRow {
                prompt: "add a function".into(),
                delivered: Some(true),
                self_rating: Some(8),
                what_went_well: "read the test first".into(),
                what_to_improve: "check the gate earlier".into(),
                critique: "solid".into(),
                produced_output: true,
                wrote_files: true,
                truncated: false,
            },
        )
        .expect("reflection");
    store
        .record_reflection(&ReflectionRow {
            execution_id: Some(completed),
            kind: "lesson".into(),
            content: "grep before you guess".into(),
            domains: "[]".into(),
            occurred_at: 1_700_000_000,
        })
        .expect("lesson");
    store
        .upsert_rule("no-todo", "never commit a TODO", "extension")
        .expect("rule");
    store
        .finish_execution(completed, "completed", 0.03)
        .expect("finish");

    // A second, unfinished execution: the observatory must render a run that
    // is still in flight (outcome NULL, finished_at NULL) without failing.
    store
        .begin_execution("goal", "make tests pass", "local", "llama")
        .expect("begin unfinished");
    dir
}

/// The gate itself: every route, against a real-migration database.
#[test]
fn every_route_survives_the_real_store_schema() {
    let workspace = real_store_workspace();
    let root: &Path = workspace.path();

    for (route, seeded_pointer) in ROUTES {
        let response = respond(root, route);
        assert_eq!(
            response.status,
            "200 OK",
            "{route} did not answer 200 — body: {}",
            String::from_utf8_lossy(&response.body)
        );
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).unwrap_or_else(|e| panic!("{route}: {e}"));
        assert!(
            body.get("error").is_none(),
            "{route} returned an error payload: {body}"
        );
        let Some(pointer) = seeded_pointer else {
            continue;
        };
        assert!(
            body.pointer(pointer).is_some(),
            "{route} lost its seeded data at {pointer} — a column this crate reads \
             was very likely renamed or dropped in stella-store. Body: {body}"
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
    use stella_protocol::{AgentEvent, ToolCall, ToolOutput};

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
    use stella_protocol::{AgentEvent, ToolCall};

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

/// Context-recall latency reaches the execution detail (#875).
///
/// Recall is on the first-token path of every turn, so a slow one delays
/// everything after it — and it was invisible in the dashboard, the receipt
/// and the log alike. This drives the real event through the real store and
/// reads it back through the real route.
#[test]
fn recall_latency_reaches_the_execution_detail() {
    use stella_protocol::{AgentEvent, ContextFrameRef, ProviderShare};

    let dir = tempfile::TempDir::new().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");
    let id = store
        .begin_execution("run", "slow recall", "anthropic", "opus")
        .expect("begin");
    store
        .record_event(
            id,
            0,
            &AgentEvent::ContextRecall {
                frames: vec![ContextFrameRef {
                    id: Some("nod_1".into()),
                    citation_label: "auth module".into(),
                    provider: "workspace-memory".into(),
                    source: "stella-context".into(),
                    kind: "memory".into(),
                    uri: None,
                    method: None,
                    token_cost: 90,
                    block_id: None,
                    content_digest: None,
                }],
                provider_mix: vec![ProviderShare {
                    provider: "workspace-memory".into(),
                    frames: 1,
                }],
                tokens: 90,
                usage: None,
                latency_ms: 1_450,
                used_ann_index: Some(false),
            },
        )
        .expect("recall event");

    let detail: serde_json::Value =
        serde_json::from_slice(&respond(dir.path(), &format!("/api/execution?id={id}")).body)
            .expect("json");

    assert_eq!(
        detail["recall"][0]["latency_ms"], 1_450,
        "the operator can see recall was the slow part: {detail}"
    );
    assert_eq!(
        detail["recall"][0]["used_ann_index"], false,
        "and that the accelerator did not fire, which is a different problem"
    );
    assert_eq!(detail["recall"][0]["frames"], 1);
    assert_eq!(detail["recall"][0]["tokens"], 90);
}

/// A stream recorded before the field existed must read as "not measured",
/// never as "instant" — `0 ms` would be a claim the data does not support.
#[test]
fn a_recall_without_a_measurement_reads_as_null_not_zero() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");
    let id = store
        .begin_execution("run", "legacy stream", "anthropic", "opus")
        .expect("begin");
    // Written straight to the table: this is a payload shaped the way a
    // pre-#875 binary wrote it, which no current constructor can produce.
    {
        let raw =
            rusqlite::Connection::open(dir.path().join(".stella/private/store.db")).expect("open");
        raw.execute(
            "INSERT INTO events (execution_id, seq, event_type, payload) \
             VALUES (?1, 0, 'context_recall', ?2)",
            rusqlite::params![
                id,
                r#"{"type":"context_recall","frames":[],"provider_mix":[],"tokens":0}"#
            ],
        )
        .expect("legacy event");
    }

    let detail: serde_json::Value =
        serde_json::from_slice(&respond(dir.path(), &format!("/api/execution?id={id}")).body)
            .expect("json");

    assert!(
        detail["recall"][0]["latency_ms"].is_null(),
        "unmeasured must not render as 0 ms: {detail}"
    );
    assert!(detail["recall"][0]["used_ann_index"].is_null());
}
