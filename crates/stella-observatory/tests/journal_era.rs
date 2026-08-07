// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **The two-era witness for #1981**, driven end to end through the served
//! `/api/execution-context` payload.
//!
//! A digest mismatch means two different things depending on who wrote the
//! journal. On a journal written before compaction recorded its replacement
//! bytes (#1667/PR #1979), a compacted tool result can only resolve through
//! the `call_id` fallback — so it mismatches, benignly, forever; #1668
//! downgraded every mismatch surface to a warning for exactly that reason. On
//! a journal that records every rewrite, the block resolves by digest to what
//! the step actually sent, so a mismatch has no routine explanation left and
//! is a real integrity signal again.
//!
//! Both readings are correct, for different journals. This suite pins that the
//! dashboard tells them apart: two executions identical in every byte except
//! `executions.journal_era` must not produce the same verdict. Before #1981
//! there was no era signal at all and they did.
//!
//! It lives in `tests/` rather than beside the unit tests because
//! `src/tests.rs` is close to the 1500-line ratchet, and it builds its fixture
//! with hand-written SQL for the same reason the production reader does — this
//! crate does not link `stella-store` outside the schema-drift gate.

use rusqlite::Connection;
use stella_observatory::respond;
use tempfile::TempDir;

/// A digest no content can re-hash to, so the block mismatches for a reason
/// the fixture states outright rather than by arranging a near-miss.
const UNREACHABLE_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

/// The execution stamped as having journaled its compaction rewrites.
const CURRENT_ERA: i64 = 1;
/// The execution stamped as predating that — what every row written before
/// schema v22 backfills to.
const LEGACY_ERA: i64 = 0;

/// Two executions differing in exactly one column: the era stamp. Each has one
/// recorded worker call whose single `tool_result` block resolves from the
/// journal and fails its digest check.
fn workspace_with_both_eras() -> TempDir {
    let dir = TempDir::new().unwrap();
    let private = dir.path().join(".stella/private");
    std::fs::create_dir_all(&private).unwrap();
    let conn = Connection::open(private.join("store.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE executions (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           kind TEXT NOT NULL, prompt TEXT NOT NULL,
           provider TEXT NOT NULL, model TEXT NOT NULL,
           started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           finished_at TEXT, outcome TEXT,
           cost_usd REAL NOT NULL DEFAULT 0,
           journal_era INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE events (
           execution_id INTEGER NOT NULL, seq INTEGER NOT NULL,
           ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           event_type TEXT NOT NULL, payload TEXT NOT NULL,
           UNIQUE (execution_id, seq));
         CREATE TABLE step_receipt (
           execution_id INTEGER NOT NULL, turn_instance INTEGER NOT NULL,
           step INTEGER NOT NULL, call_seq INTEGER NOT NULL DEFAULT 0,
           provider TEXT NOT NULL, model TEXT NOT NULL, call_role TEXT NOT NULL,
           effective_budget_tokens INTEGER NOT NULL,
           calibration_factor REAL NOT NULL,
           estimated_input_tokens INTEGER NOT NULL,
           compiled_frame_id TEXT, frame_hash TEXT,
           PRIMARY KEY (execution_id, turn_instance, step, call_seq));
         CREATE TABLE step_manifest (
           execution_id INTEGER NOT NULL, turn_instance INTEGER NOT NULL,
           step INTEGER NOT NULL, call_seq INTEGER NOT NULL DEFAULT 0,
           ordinal INTEGER NOT NULL, block_id TEXT NOT NULL,
           cache_zone TEXT NOT NULL, resident_since_step INTEGER NOT NULL,
           message_index INTEGER NOT NULL DEFAULT 0, call_id TEXT,
           PRIMARY KEY (execution_id, turn_instance, step, call_seq, ordinal));
         CREATE TABLE context_blocks (
           execution_id INTEGER NOT NULL, block_id TEXT NOT NULL,
           kind TEXT NOT NULL, origin_turn INTEGER NOT NULL,
           origin_step INTEGER NOT NULL, call_id TEXT, memory_id TEXT,
           token_cost INTEGER, content_digest TEXT NOT NULL,
           citation_label TEXT, content TEXT,
           first_seen_ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           PRIMARY KEY (execution_id, block_id));",
    )
    .unwrap();

    for (id, era) in [(1_i64, CURRENT_ERA), (2, LEGACY_ERA)] {
        conn.execute(
            "INSERT INTO executions (id, kind, prompt, provider, model, journal_era)
             VALUES (?1, 'run', 'fix it', 'zai', 'glm-5.2', ?2)",
            rusqlite::params![id, era],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (execution_id, seq, event_type, payload) VALUES (?1, 0, \
             'tool_result', '{\"type\":\"tool_result\",\"call_id\":\"c1\",\"output\":{\"ok\":\
             {\"content\":\"fn a() {}\"}},\"duration_ms\":1,\"speculated\":false}')",
            rusqlite::params![id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO step_receipt
               (execution_id, turn_instance, step, call_seq, provider, model, call_role,
                effective_budget_tokens, calibration_factor, estimated_input_tokens)
             VALUES (?1, 0, 1, 0, 'zai', 'glm-5.2', 'worker', 1000, 1.0, 10)",
            rusqlite::params![id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO context_blocks
               (execution_id, block_id, kind, origin_turn, origin_step, call_id,
                token_cost, content_digest, content)
             VALUES (?1, 'blk_res', 'tool_result', 0, 1, 'c1', 10, ?2, NULL)",
            rusqlite::params![id, UNREACHABLE_DIGEST],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO step_manifest
               (execution_id, turn_instance, step, call_seq, ordinal, block_id,
                cache_zone, resident_since_step, message_index, call_id)
             VALUES (?1, 0, 1, 0, 0, 'blk_res', 'volatile', 1, 0, 'c1')",
            rusqlite::params![id],
        )
        .unwrap();
    }
    dir
}

fn context_of(root: &std::path::Path, id: i64) -> serde_json::Value {
    let response = respond(
        root,
        &format!("/api/execution-context?id={id}&turn=0&step=1&call_seq=0"),
    );
    let payload: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    payload["context"].clone()
}

#[test]
fn the_same_mismatch_reads_as_housekeeping_on_a_legacy_journal_and_an_alarm_on_a_current_one() {
    let ws = workspace_with_both_eras();
    let current = context_of(ws.path(), 1);
    let legacy = context_of(ws.path(), 2);

    // The two reconstructions are the same failure: same block, same
    // unresolvable digest, same rebuilt bytes.
    assert_eq!(current["found"], true, "{current}");
    assert_eq!(legacy["found"], true, "{legacy}");
    assert_eq!(current["digest_mismatches"], serde_json::json!(["blk_res"]));
    assert_eq!(legacy["digest_mismatches"], current["digest_mismatches"]);
    assert_eq!(legacy["messages"], current["messages"]);
    assert_eq!(current["verified"], false);
    assert_eq!(legacy["verified"], false);

    // And they must still not be reported the same way.
    assert_eq!(
        legacy["digest_mismatch_severity"], "compaction",
        "a pre-#1667 journal mismatches on ordinary compaction — calling that \
         an integrity failure is the false alarm #1668 removed: {legacy}"
    );
    assert_eq!(
        current["digest_mismatch_severity"], "integrity",
        "this journal records every compaction rewrite, so nothing routine \
         accounts for these bytes: {current}"
    );
    assert_ne!(
        legacy["digest_mismatch_severity"],
        current["digest_mismatch_severity"]
    );

    // The era itself is on the payload too, so a script does not have to infer
    // it from the severity of a failure that may not have happened.
    assert_eq!(legacy["journal_era"], "compaction_unjournaled");
    assert_eq!(current["journal_era"], "compaction_journaled");
}

#[test]
fn a_store_too_old_to_carry_the_era_column_stays_quiet() {
    // The dashboard reads other people's databases, including ones written
    // before schema v22 added the column. That read must degrade to the benign
    // era rather than to a 500 or to an accusation.
    let dir = TempDir::new().unwrap();
    let private = dir.path().join(".stella/private");
    std::fs::create_dir_all(&private).unwrap();
    {
        let source = workspace_with_both_eras();
        std::fs::copy(
            source.path().join(".stella/private/store.db"),
            private.join("store.db"),
        )
        .unwrap();
        let conn = Connection::open(private.join("store.db")).unwrap();
        conn.execute_batch("ALTER TABLE executions DROP COLUMN journal_era;")
            .unwrap();
    }

    let context = context_of(dir.path(), 1);
    assert_eq!(context["found"], true, "the route still answers: {context}");
    assert_eq!(context["journal_era"], "compaction_unjournaled");
    assert_eq!(context["digest_mismatch_severity"], "compaction");
}
