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
//!
//! # Where a test lives
//!
//! This file holds the two real-migration fixtures (`real_store_workspace`
//! for `store.db`, `real_context_workspace` for `context.db`) and their
//! seeding helpers — the part every topic needs — plus the `mod` declarations
//! below. The `#[test]`s themselves live in `tests/schema_conformance/`,
//! split by subject the same way #4486 found this file at the 1500-line
//! ceiling with no headroom for the next drift assertion (#3923, #4479 had
//! already had to relocate one witness elsewhere for exactly this reason). A
//! new test joins the topic module it fits, or gets its own if none does; a
//! new fixture stays here only if more than one topic needs it.

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
    ("/api/rules", Some("/db/0/rule_id")),
    ("/api/reflections", Some("/ratings/0/execution_id")),
    (
        "/api/session-turn-diff?id=ses-1700000000000-424242&turn=1",
        Some("/files/0/hunks/0/lines/0/op"),
    ),
    // The context.db half is empty in a store-only fixture — this workspace
    // deliberately does not build that file, and its real-schema gate is
    // `context_lifecycle_returns_the_promotion_lineage` in the
    // `context_lifecycle` topic module (#1871). The pointer here covers the
    // half that reads `store.db`: the `parse_error` column
    // added in schema v23 (#2175), which is exactly the kind of new column
    // this suite exists to prove resolvable.
    (
        "/api/context-lifecycle",
        Some("/unreadable_reflections/0/execution_id"),
    ),
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
        data: None,
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
                contract: None,
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
    // The precomputed turn diff (#1870), through the real write API — what
    // `stella-cli`'s `turn_diff::record_turn_diff` persists at turn end.
    let turn_diff = serde_json::json!({
        "files": [{
            "path": "src/lib.rs",
            "added": 1,
            "removed": 1,
            "hunks": [{
                "old_start": 1, "old_count": 3, "new_start": 1, "new_count": 3,
                "lines": [
                    {"op": "equal", "text": "one"},
                    {"op": "remove", "text": "two"},
                    {"op": "add", "text": "TWO"},
                    {"op": "equal", "text": "three"},
                ],
            }],
            "skipped": false,
        }],
        "files_truncated": false,
    });
    store
        .record_session_turn_diff(SESSION_ID, 1, Some(completed), &turn_diff.to_string())
        .expect("turn diff");
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
            // Both of the log's writers (#3822): an installed definition and
            // a `task` delegation, so the AGENTS panel's discriminator is
            // exercised against the real schema rather than a hand-written
            // subset of it.
            &[
                AgentUseRow {
                    agent: "reviewer".into(),
                    version: 1,
                    reason: "second opinion".into(),
                    kind: stella_store::KIND_DEFINITION.into(),
                },
                AgentUseRow {
                    agent: "find-retry-policy".into(),
                    version: 1,
                    reason: "find the retry policy".into(),
                    kind: stella_store::KIND_DELEGATION.into(),
                },
            ],
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
                partial_run: false,
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
    let unfinished = store
        .begin_execution("goal", "make tests pass", "local", "llama")
        .expect("begin unfinished");

    // #2175: a turn whose reflection response the lesson parser could not
    // read. Seeded through the real writer so this suite proves the
    // `parse_error` column the lifecycle route selects — added in store schema
    // v23 — is resolvable against a store built by the migration path. It is a
    // different execution from the self-reviewed one above on purpose: the two
    // producers write the same row and must not clobber each other.
    store
        .record_reflection_parse_failure(
            unfinished,
            "Let me think about this turn step by step. First,",
        )
        .expect("reflection parse failure");
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
                stall_seconds_requested: None,
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
                stall_seconds_requested: None,
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

// ── Topic modules ──────────────────────────────────────────────────────
// Each declares `use super::*;` to reach the fixtures and imports above.
// New tests join the topic they fit, or start a new module if none does.
//
// This file is a `tests/*.rs` integration-test crate ROOT, so its own
// submodules resolve beside it (`tests/<name>.rs`) by default -- unlike a
// `src/`-side split, where a submodule of `foo.rs` resolves under `foo/`.
// The explicit `#[path]` on each is what keeps the topic files inside
// `tests/schema_conformance/` instead of littering `tests/` itself with
// files Cargo would otherwise try to auto-discover as their own separate
// integration-test binaries.
#[path = "schema_conformance/context_lifecycle.rs"]
mod context_lifecycle;
#[path = "schema_conformance/diffs_and_agents.rs"]
mod diffs_and_agents;
#[path = "schema_conformance/ratings_and_calls.rs"]
mod ratings_and_calls;
#[path = "schema_conformance/recall_and_receipts.rs"]
mod recall_and_receipts;
#[path = "schema_conformance/route_survival.rs"]
mod route_survival;
