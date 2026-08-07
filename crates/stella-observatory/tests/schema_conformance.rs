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

use sha2::{Digest, Sha256};
use stella_context::{ContextDelta, ContextStore, EpisodeInput, LedgerAppend};
use stella_core::context_record::{
    Confidence, ObservationRecord, PromotionAction, PromotionActor, PromotionEventRecord,
    ProposalRecord, ProposalScore, RecordProposalKind, RecordProposalStatus,
    lifecycle::ObservationSource,
};
use stella_observatory::respond;
use stella_protocol::{
    AgentEvent, ContextFrameRef, ProviderShare, TaskItem, TaskStatus, ToolCall, ToolOutput,
};
use stella_store::{
    AgentUseRow, ContextBlockRow, ExecutionReflectionRow, FileTouchRow, ManifestBlockRow,
    McpUsageRow, MemoryCitationRow, ReflectionRow, SkillUsageRow, StepManifestRow, Store,
    TelemetryRow,
};

/// The seeded system prefix. Named because it is what the sent-context witness
/// looks for: the system prompt is the part of a model call that exists nowhere
/// else in the store, and reconstructing it is the whole point of the route.
const SYSTEM_PROMPT: &str = "you are stella, and you are careful";

/// The session the seeded execution is stamped with — what `/api/sessions`
/// lists and `/api/session` drills. Shaped like a real minted id
/// (`ses-<ms>-<pid>`) because the sessions view recovers a sort key from it.
const SESSION_ID: &str = "ses-1700000000000-424242";

/// The one line the second turn's system prefix gained — what the prompt-diff
/// witness expects to see as the sole `+` line (#1511).
const DRIFT_LINE: &str = "and be bold about it";

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
    ("/api/execution-journal?id=1", Some("/0/type")),
    ("/api/execution-context?id=1", Some("/calls/0/step")),
    (
        "/api/execution-context?id=1&turn=0&step=1&call_seq=0",
        Some("/context/messages/0/role"),
    ),
    ("/api/sessions", Some("/sessions/0/id")),
    (
        "/api/session?id=ses-1700000000000-424242",
        Some("/turns/0/id"),
    ),
    ("/api/execution-tendencies?id=1", Some("/retries")),
    (
        "/api/execution-context-diff?id=2&turn=0&step=1&call_seq=0",
        Some("/hunks/0/lines/0/op"),
    ),
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
    // Empty in a store-only fixture: the lifecycle reads context.db, which
    // this workspace deliberately does not build. Its own real-schema gate is
    // `context_lifecycle_returns_the_promotion_lineage` below (#1871).
    ("/api/context-lifecycle", None),
];

/// One tool call's full event round-trip — the announcement and its result.
///
/// Takes the call and output rather than building them, so the receipts seeded
/// below hash the **same** values these events carry. A second definition of
/// "the tool call" would let the two drift and quietly turn the digest gate
/// into a tautology.
fn tool_round_trip(call: &ToolCall, output: &ToolOutput) -> [AgentEvent; 2] {
    [
        AgentEvent::ToolStart { call: call.clone() },
        AgentEvent::ToolResult {
            call_id: call.call_id.clone(),
            output: output.clone(),
            duration_ms: 12,
            speculated: false,
        },
    ]
}

/// `sha256:<hex>` over exactly these bytes — the digest shape the receipts
/// plane records.
fn digest(preimage: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(preimage.as_bytes());
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
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
    store
        .set_execution_session(completed, SESSION_ID)
        .expect("session stamp");
    // Tool calls arrive as events and are projected from them — seeding the
    // `tool_calls` table any other way would test a write path production
    // does not use.
    let call = ToolCall {
        call_id: "c1".into(),
        name: "read_file".into(),
        input: serde_json::json!({ "path": "src/lib.rs" }),
    };
    let output = ToolOutput::Ok {
        content: "fn a() {}".into(),
    };
    for (seq, event) in tool_round_trip(&call, &output).into_iter().enumerate() {
        store
            .record_event(completed, seq as u64, &event)
            .expect("tool event");
    }
    // Two behavioural events for the tendencies fold: a retry and a steer-
    // stage loop detection. Seeded as events because that is the only place
    // production writes them — no projection exists.
    store
        .record_event(
            completed,
            2,
            &AgentEvent::Retry {
                attempt: 1,
                reason: "rate limited".into(),
            },
        )
        .expect("retry event");
    store
        .record_event(
            completed,
            3,
            &AgentEvent::LoopDetected {
                turn_instance: 0,
                kind: "exact_repeat".into(),
                pattern: vec!["read_file".into()],
                repeats: 3,
                evidence: "same call three times".into(),
                aborted: false,
            },
        )
        .expect("loop event");
    seed_receipt(&store, completed, &call, &output);
    store
        .record_task_board(
            completed,
            Some(SESSION_ID),
            &[TaskItem {
                id: "1".into(),
                subject: "add the function".into(),
                description: None,
                status: TaskStatus::Completed,
                owner: Some("lead".into()),
            }],
            1_700_000_000_000,
        )
        .expect("task board");
    store
        .upsert_pull_request(
            Some(SESSION_ID),
            "https://github.com/example/repo/pull/7",
            Some(7),
            "open",
            Some("passing"),
            1_700_000_000_000,
        )
        .expect("pull request");
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

    // A second turn of the same session whose system prefix gained exactly
    // one line — the drift `/api/execution-context-diff` exists to name
    // (#1511). Two byte-identical worker calls, so `prev` inside this turn is
    // an honest "no change" while `prev` across the turn boundary names the
    // line that moved.
    let drifted = store
        .begin_execution("run", "add another function", "zai", "glm-5.2")
        .expect("begin drifted");
    store
        .set_execution_session(drifted, SESSION_ID)
        .expect("session stamp 2");
    let drifted_prompt = format!("{SYSTEM_PROMPT}\n{DRIFT_LINE}");
    seed_system_receipt(&store, drifted, 1, &drifted_prompt);
    seed_system_receipt(&store, drifted, 2, &drifted_prompt);
    store
        .finish_execution(drifted, "completed", 0.01)
        .expect("finish drifted");

    // A last, unfinished execution: the observatory must render a run that
    // is still in flight (outcome NULL, finished_at NULL) without failing.
    store
        .begin_execution("goal", "make tests pass", "local", "llama")
        .expect("begin unfinished");
    dir
}

/// One worker call whose whole context is a single system-prefix block — the
/// smallest receipt that makes a call diffable. The block is a gap kind, so
/// its bytes are stored locally, exactly as the engine stores them.
fn seed_system_receipt(store: &Store, execution_id: i64, step: u64, system: &str) {
    store
        .record_context_block(
            execution_id,
            &ContextBlockRow {
                block_id: "blk_sys_drift".into(),
                kind: "system_prefix".into(),
                origin_turn: 0,
                origin_step: 0,
                call_id: None,
                memory_id: None,
                token_cost: Some(48),
                content_digest: digest(system),
                citation_label: None,
                content: Some(system.into()),
            },
        )
        .expect("drifted system block");
    store
        .record_step_manifest(
            execution_id,
            &StepManifestRow {
                turn_instance: 0,
                step,
                call_seq: 0,
                provider: "zai".into(),
                model: "glm-5.2".into(),
                call_role: "worker".into(),
                effective_budget_tokens: 136_363,
                calibration_factor: 1.1,
                estimated_input_tokens: 48,
                compiled_frame_id: None,
                frame_hash: None,
                blocks: vec![ManifestBlockRow {
                    block_id: "blk_sys_drift".into(),
                    cache_zone: "stable_prefix".into(),
                    token_cost: Some(48),
                    resident_since_step: 0,
                    message_index: 0,
                    call_id: None,
                }],
            },
        )
        .expect("drifted manifest");
}

/// One recorded model call's context receipt: the block registry plus the
/// ordered manifest `/api/execution-context` reconstructs from (#1475).
///
/// The two journal-resolved blocks take their digest over `stella_protocol`'s
/// **own** serialization of the very call and output the events above carry, so
/// this seeds the byte-level half of the drift gate: the observatory builds
/// that preimage by hand (it links no protocol crate), and reordering
/// `ToolCall`'s fields would make its digests stop matching — here, at
/// `cargo test`, rather than as an "unverified" banner on a user's dashboard.
/// The system prefix is a gap block: its preimage is stored locally, exactly as
/// the engine stores it, because the journal cannot carry it.
fn seed_receipt(store: &Store, execution_id: i64, call: &ToolCall, output: &ToolOutput) {
    let call_json = serde_json::to_string(call).expect("tool call json");
    let output_json = serde_json::to_string(output).expect("tool output json");
    let blocks = [
        ContextBlockRow {
            block_id: "blk_sys".into(),
            kind: "system_prefix".into(),
            origin_turn: 0,
            origin_step: 0,
            call_id: None,
            memory_id: None,
            token_cost: Some(40),
            content_digest: digest(SYSTEM_PROMPT),
            citation_label: None,
            content: Some(SYSTEM_PROMPT.into()),
        },
        ContextBlockRow {
            block_id: "blk_call".into(),
            kind: "tool_call".into(),
            origin_turn: 0,
            origin_step: 1,
            call_id: Some(call.call_id.clone()),
            memory_id: None,
            token_cost: Some(25),
            content_digest: digest(&call_json),
            citation_label: None,
            content: None,
        },
        ContextBlockRow {
            block_id: "blk_res".into(),
            kind: "tool_result".into(),
            origin_turn: 0,
            origin_step: 1,
            call_id: Some(call.call_id.clone()),
            memory_id: None,
            token_cost: Some(88),
            content_digest: digest(&output_json),
            citation_label: None,
            content: None,
        },
    ];
    for block in &blocks {
        store
            .record_context_block(execution_id, block)
            .expect("context block");
    }
    store
        .record_step_manifest(
            execution_id,
            &StepManifestRow {
                turn_instance: 0,
                step: 1,
                call_seq: 0,
                provider: "zai".into(),
                model: "glm-5.2".into(),
                call_role: "worker".into(),
                effective_budget_tokens: 136_363,
                calibration_factor: 1.1,
                estimated_input_tokens: 203,
                compiled_frame_id: None,
                frame_hash: None,
                blocks: vec![
                    ManifestBlockRow {
                        block_id: "blk_sys".into(),
                        cache_zone: "stable_prefix".into(),
                        token_cost: Some(40),
                        resident_since_step: 0,
                        message_index: 0,
                        call_id: None,
                    },
                    ManifestBlockRow {
                        block_id: "blk_call".into(),
                        cache_zone: "volatile".into(),
                        token_cost: Some(25),
                        resident_since_step: 1,
                        message_index: 1,
                        call_id: Some(call.call_id.clone()),
                    },
                    ManifestBlockRow {
                        block_id: "blk_res".into(),
                        cache_zone: "volatile".into(),
                        token_cost: Some(88),
                        resident_since_step: 1,
                        message_index: 2,
                        call_id: Some(call.call_id.clone()),
                    },
                ],
            },
        )
        .expect("step manifest");
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

/// Sent-context reconstruction, end to end against a real store (#1475).
///
/// The strongest form of the drift gate this suite exists for, because it
/// checks a *byte-level* coupling rather than a column-level one. The
/// observatory links no protocol crate, so it rebuilds a `tool_call` block's
/// preimage by hand in `stella_protocol::ToolCall`'s declaration order; the
/// receipt here was digested over that crate's own serializer. Reorder those
/// fields and `verified` flips to false right here — the alternative being a
/// silent "the journal is torn" banner on every dashboard in the field.
#[test]
fn sent_context_reconstructs_a_real_stores_receipt_and_verifies_it() {
    let workspace = real_store_workspace();
    let body = respond(
        workspace.path(),
        "/api/execution-context?id=1&turn=0&step=1&call_seq=0",
    )
    .body;
    let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let context = &v["context"];
    assert_eq!(context["found"], true, "{v}");
    assert_eq!(
        context["messages"][0]["body"], SYSTEM_PROMPT,
        "the system prompt the model was actually sent: {v}"
    );
    assert_eq!(context["messages"][0]["role"], "system");
    assert_eq!(
        context["messages"][2]["body"], "← tool_result c1\nfn a() {}",
        "the tool result, recovered from the journal: {v}"
    );
    assert_eq!(
        context["verified"], true,
        "a journal-resolved block stopped re-hashing to the digest \
         stella_protocol's own serializer produced — a wire shape moved under \
         the observatory's hand-built preimage: {v}"
    );
}

/// **The other half of the drift problem, and the one that actually bit.**
///
/// The gate above proves the queries work against a store migrated to *this*
/// build's schema. But this crate opens every file `SQLITE_OPEN_READ_ONLY`
/// and therefore never runs migrations — so it is routinely pointed at a
/// store several versions *behind* the binary reading it. Upgrade `stella`,
/// open the dashboard before running a single turn, and nothing has yet had
/// any reason to open that file read-write.
///
/// Found by pointing this crate at an untouched copy of a real 54 MB store:
/// every route referencing the v18 `tool_calls.state` column 500'd. A missing
/// *table* had always degraded to an empty payload; a missing *column* did
/// not, because `rusqlite` reports the two through different error variants
/// (`SqliteFailure` vs `SqlInputError`) and only the first was matched.
///
/// So this drives every route against a database deliberately rolled back to
/// the pre-v18 shape. None may error: an older store renders with a section
/// empty, and fills in the moment a turn migrates it.
#[test]
fn every_route_survives_a_store_older_than_this_build() {
    let workspace = real_store_workspace();
    let db = workspace.path().join(".stella/private/store.db");
    {
        let raw = rusqlite::Connection::open(&db).expect("open");
        // Rebuild `tool_calls` without `state` — exactly the shape a v17
        // binary left behind. Dropping the column is the honest simulation;
        // stubbing the queries would test the stub.
        raw.execute_batch(
            // The by-state index has to go first — it references the column,
            // and SQLite refuses to leave an index pointing at nothing.
            "DROP INDEX IF EXISTS tool_calls_by_state;
             ALTER TABLE tool_calls DROP COLUMN state;
             PRAGMA user_version = 17;",
        )
        .expect("roll the schema back");
    }

    for (route, _) in ROUTES {
        let response = respond(workspace.path(), route);
        assert_eq!(
            response.status,
            "200 OK",
            "{route} failed against a pre-v18 store — a read-only observer never migrates, \
             so it must degrade rather than 500. Body: {}",
            String::from_utf8_lossy(&response.body)
        );
    }

    // And the degradation is honest: the column is gone, so nothing is
    // claimed to be running rather than a number being invented.
    let cursor: serde_json::Value =
        serde_json::from_slice(&respond(workspace.path(), "/api/v1/cursor").body).expect("json");
    assert_eq!(cursor["tool_calls_running"], 0);
    assert!(
        cursor["events"].as_i64().unwrap_or(0) > 0,
        "the columns that DO exist still report: {cursor}"
    );
}

/// The #1511 witness: two calls of the same role whose system prefixes differ
/// by one line produce exactly one hunk naming that line, and a
/// byte-identical pair reports `changed: false`. Fails before the route
/// exists (404), and fails if the baseline search stops at the execution
/// boundary — the drift here is *across* turns, which is the only place a
/// byte-stable system prompt can drift.
#[test]
fn context_diff_names_the_moved_line_and_reports_identity_honestly() {
    let workspace = real_store_workspace();
    let root: &Path = workspace.path();

    // Across the turn boundary: execution 2's first worker call against
    // execution 1's — the same role, one line of drift, system scope.
    let body = respond(
        root,
        "/api/execution-context-diff?id=2&turn=0&step=1&call_seq=0&base=prev&only=system",
    )
    .body;
    let diff: serde_json::Value = serde_json::from_slice(&body).expect("diff json");
    assert_eq!(diff["found"], true, "{diff}");
    assert_eq!(diff["changed"], true, "{diff}");
    assert_eq!(diff["base"], "prev", "{diff}");
    assert_eq!(diff["minimal"], true, "{diff}");
    assert_eq!(diff["added"], 1, "one inserted line, one addition: {diff}");
    let hunks = diff["hunks"].as_array().expect("hunks");
    assert_eq!(hunks.len(), 1, "one contiguous change, one hunk: {diff}");
    let added: Vec<&str> = hunks[0]["lines"]
        .as_array()
        .expect("lines")
        .iter()
        .filter(|l| l["op"] == "add")
        .filter_map(|l| l["text"].as_str())
        .collect();
    assert_eq!(added, vec![DRIFT_LINE], "the diff names the moved line");
    assert!(
        diff["base_label"]
            .as_str()
            .unwrap_or_default()
            .contains("execution 1"),
        "a cross-turn baseline is unmistakable: {diff}"
    );

    // Inside the turn: step 2 against step 1, byte-identical by construction.
    let body = respond(
        root,
        "/api/execution-context-diff?id=2&turn=0&step=2&call_seq=0&base=prev&only=system",
    )
    .body;
    let same: serde_json::Value = serde_json::from_slice(&body).expect("diff json");
    assert_eq!(
        same["changed"], false,
        "byte-identical is not a change: {same}"
    );
    assert_eq!(same["base"], "prev", "{same}");
    assert!(
        !same["base_label"]
            .as_str()
            .unwrap_or_default()
            .contains("execution"),
        "a same-turn baseline stays terse: {same}"
    );

    // A role's first call has no predecessor: the resolved base is reported
    // as `prompt`, never silently claimed to be `prev`.
    let body = respond(
        root,
        "/api/execution-context-diff?id=1&turn=0&step=1&call_seq=0&base=prev",
    )
    .body;
    let first: serde_json::Value = serde_json::from_slice(&body).expect("diff json");
    assert_eq!(
        first["base"], "prompt",
        "prev on a first call resolves: {first}"
    );
    assert_eq!(first["base_label"], "prompt as submitted", "{first}");
}

/// One ledger append through the real write API, with the record's own
/// identity, hash and timestamps — the shape every production writer uses
/// (`crates/stella-cli/src/memory/observations.rs::append_observation` et al).
#[allow(clippy::too_many_arguments)] // mirrors LedgerAppend's own field list
fn append_lifecycle(
    store: &ContextStore,
    kind: &str,
    record_id: &str,
    lineage_id: &str,
    record_hash: &str,
    schema_version: &str,
    body: &str,
    observed_at: &str,
) {
    store
        .append_record(LedgerAppend {
            record_id,
            lineage_id,
            record_kind: kind,
            record_hash,
            schema_version,
            body,
            observed_at,
            supersedes: None,
        })
        .expect("ledger append");
}

/// The candidate the seeded proposal names — what the promotions timeline
/// must echo back.
const CANDIDATE_ID: &str = "grep-first-abc12345";

/// Build a workspace whose `context.db` came from the **real** migration path
/// (`ContextStore::open`), seeded through the real write APIs (#1871): a full
/// observation → proposal → promotion-event lineage, plus one episode through
/// the writeback path.
///
/// The second half of the bargain `real_store_workspace` documents for
/// `store.db`: production reads this file with hand-written SQL over a
/// read-only handle, so only a database built by `stella-context`'s own
/// migrations can prove those queries still resolve.
fn real_context_workspace() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let private = dir.path().join(".stella").join("private");
    std::fs::create_dir_all(&private).expect("private dir");
    let store = ContextStore::open(private.join("context.db"))
        .expect("ContextStore::open runs the real migrations");

    let observation = ObservationRecord::new(
        ObservationSource::ReflectionLesson,
        "reflection:1700000000",
        "task-1",
        "grep before you guess",
        Vec::new(),
        false,
        "2026-08-01T12:00:00Z",
    )
    .expect("observation");
    append_lifecycle(
        &store,
        "observation",
        &observation.record_id,
        &observation.lineage_id,
        &observation.record_hash,
        &observation.schema_version,
        &serde_json::to_string(&observation).expect("record json"),
        &observation.observed_at,
    );

    let proposal = ProposalRecord::new(
        RecordProposalKind::Knowledge,
        RecordProposalStatus::Eligible,
        CANDIDATE_ID,
        "Grep before you guess",
        "Search the tree before assuming a symbol's location.",
        Vec::new(),
        vec![observation.record_id.clone()],
        ProposalScore {
            occurrences: 4,
            distinct_tasks: 3,
            salient: true,
            rank: 0.9,
        },
        Confidence::new(90).expect("confidence"),
        "2026-08-01T12:05:00Z",
    )
    .expect("proposal");
    append_lifecycle(
        &store,
        "record_proposal",
        &proposal.record_id,
        &proposal.lineage_id,
        &proposal.record_hash,
        &proposal.schema_version,
        &serde_json::to_string(&proposal).expect("record json"),
        &proposal.observed_at,
    );

    let event = PromotionEventRecord::new(
        proposal.lineage_id.clone(),
        PromotionAction::Confirmed,
        PromotionActor::User,
        None,
        None,
        "kept from review",
        "2026-08-01T12:10:00Z",
    )
    .expect("promotion event");
    append_lifecycle(
        &store,
        "promotion_event",
        &event.record_id,
        &event.lineage_id,
        &event.record_hash,
        &event.schema_version,
        &serde_json::to_string(&event).expect("record json"),
        &event.occurred_at,
    );

    // One episode through the real writeback path. The embed decision runs on
    // the store's built-in hash embedder — nothing leaves the process.
    tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(async {
            store
                .upsert(ContextDelta::new().with_episode(EpisodeInput::new(
                    "added the parser fix",
                    "2026-08-01T12:00:00Z",
                    "2026-08-01T12:20:00Z",
                )))
                .await
                .expect("episode upsert");
        });

    (
        dir,
        proposal.lineage_id.clone(),
        observation.record_id.clone(),
    )
}

/// The #1871 witness: the route folds the seeded observation → proposal →
/// promotion-event lineage back out of a real-migration `context.db`. Fails
/// on main (the route is absent), and fails if any ledger or episode column
/// this crate reads is renamed in `stella-context`.
#[test]
fn context_lifecycle_returns_the_promotion_lineage() {
    let (workspace, proposal_lineage, observation_id) = real_context_workspace();

    let body = respond(workspace.path(), "/api/context-lifecycle").body;
    let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert!(v.get("error").is_none(), "{v}");
    assert_eq!(v["present"], true, "{v}");

    let proposal = &v["proposals"][0];
    assert_eq!(proposal["lineage_id"], proposal_lineage.as_str(), "{v}");
    assert_eq!(proposal["candidate_id"], CANDIDATE_ID, "{v}");
    assert_eq!(
        proposal["status"], "confirmed",
        "the decision standing is replayed from the event log, not stored: {v}"
    );
    assert_eq!(
        proposal["supporting_observations"][0],
        observation_id.as_str(),
        "the lineage reaches back to its evidence: {v}"
    );
    assert_eq!(
        proposal["events"][0]["action"], "confirmed",
        "the proposal carries its own slice of the audit trail: {v}"
    );

    assert_eq!(v["events"][0]["action"], "confirmed", "{v}");
    assert_eq!(
        v["events"][0]["candidate_id"], CANDIDATE_ID,
        "the timeline names the candidate its lineage points at: {v}"
    );

    assert_eq!(v["episodes"][0]["outcome"], "success", "{v}");
    assert_eq!(v["episodes"][0]["summary"], "added the parser fix", "{v}");

    let kinds: Vec<&str> = v["counts"]
        .as_array()
        .expect("counts")
        .iter()
        .filter_map(|c| c["kind"].as_str())
        .collect();
    for kind in ["observation", "record_proposal", "promotion_event"] {
        assert!(kinds.contains(&kind), "counts missing {kind}: {v}");
    }
}

/// Missing is a state: a workspace that has never built a context plane
/// answers with the full (empty) payload shape, never a 500 and never a
/// missing key.
#[test]
fn a_workspace_with_no_context_db_degrades_to_an_empty_lifecycle() {
    let workspace = real_store_workspace();
    let response = respond(workspace.path(), "/api/context-lifecycle");
    assert_eq!(
        response.status,
        "200 OK",
        "body: {}",
        String::from_utf8_lossy(&response.body)
    );
    let v: serde_json::Value = serde_json::from_slice(&response.body).expect("json");
    assert_eq!(v["present"], false, "{v}");
    for key in [
        "counts",
        "proposals",
        "events",
        "episodes",
        "selection_health",
    ] {
        assert_eq!(v[key], serde_json::json!([]), "{key} must be empty: {v}");
    }
}

/// The read-only observer never migrates, so it can be pointed at a
/// `context.db` older than the v8 lifecycle ledger — same hazard the pre-v18
/// store test above covers. The ledger sections degrade to empty and the
/// episode list (whose v8 columns are also gone) degrades with them; nothing
/// 500s, and everything fills in after the next session migrates the file.
#[test]
fn a_context_db_older_than_v8_degrades_to_empty_ledger_sections() {
    let (workspace, _, _) = real_context_workspace();
    {
        let raw = rusqlite::Connection::open(workspace.path().join(".stella/private/context.db"))
            .expect("open");
        // Rebuild the pre-v8 shape honestly: no ledger table, no lineage
        // columns on `episode`. Dropping the whole table also drops its
        // append-only triggers, exactly as a pre-v8 file never had them.
        raw.execute_batch(
            "DROP TABLE context_records;
             DROP INDEX IF EXISTS idx_episode_lineage;
             ALTER TABLE episode DROP COLUMN lineage_id;
             ALTER TABLE episode DROP COLUMN superseded_at;
             PRAGMA user_version = 7;",
        )
        .expect("roll the schema back");
    }

    let response = respond(workspace.path(), "/api/context-lifecycle");
    assert_eq!(
        response.status,
        "200 OK",
        "a pre-v8 context.db must degrade, not 500 — body: {}",
        String::from_utf8_lossy(&response.body)
    );
    let v: serde_json::Value = serde_json::from_slice(&response.body).expect("json");
    assert_eq!(v["present"], true, "the file exists and is reported: {v}");
    for key in [
        "counts",
        "proposals",
        "events",
        "episodes",
        "selection_health",
    ] {
        assert_eq!(v[key], serde_json::json!([]), "{key} must be empty: {v}");
    }
}
