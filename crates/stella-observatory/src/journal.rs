// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One execution's transcript, projected from its slice of the `events`
//! journal — the answer text, reasoning and tool traffic the telemetry
//! projections deliberately do not carry.
//!
//! Split out of `db.rs`, which had reached 1482 of the guard's 1500 lines with
//! no baseline entry, so the next addition to it would have failed the gate
//! outright. The seam is not the line count, though: `db.rs` answers *what the
//! run cost and did* in aggregates, and this answers *what happened, in order,
//! for a person reading along*. They share only the store handle.
//!
//! # The transcript grammar
//!
//! The projection exists so `stella observe` reads like `stella run`. A reader
//! who lives in the Command Deck should not have to relearn the page, so the
//! wire shape here carries exactly what the deck's fold carries
//! (`crates/stella-tui/src/model.rs`) and `assets/index.html` renders it with
//! the deck's rails (`crates/stella-tui/src/render/row.rs::Rail`).
//!
//! Two facts about the wire that a per-row projection cannot see on its own,
//! and that this module therefore resolves for it:
//!
//! - **`ToolOutput` is externally tagged.** `{"ok":{"content":…}}` or
//!   `{"error":{"message":…}}`, and the tag is the success verdict — a
//!   `tool_result` carries no `error` field to consult.
//! - **The tool's name is on the call, not the result.** `ToolResult` is
//!   `{call_id, output, duration_ms, speculated}`, so a result row can only be
//!   labelled by correlating back to the `tool_start` that shares its id. The
//!   deck does this in its fold; every result row in this dashboard read
//!   `✓ result` until [`tool_names`] existed.

use std::collections::HashMap;

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::db::{DbError, is_missing_schema, truncate};

/// Char cap on one transcript body in the default
/// [`crate::db::Observatory::execution_journal`] payload. A single `read_file`
/// result can be megabytes; the page needs the shape of the turn, not the
/// bytes, and `?full=1` is the escape hatch for the reader who wants them.
pub(crate) const JOURNAL_BODY_CLIP: usize = 4_000;

/// One execution's transcript rows, in journal order.
///
/// This is the second sanctioned read of `events` (after `recall_timings`) and
/// it obeys the same bargain: the filter is `execution_id`-first, which the
/// store's `UNIQUE (execution_id, seq)` index serves directly. The whole
/// transcript is fetched once on open; while the execution is still running
/// the page polls back with `after_seq` set to the highest `seq` it already
/// has (#1476), so a long run's transcript is never re-downloaded in full —
/// the same index serves `seq > ?` directly. A cross-execution transcript view
/// would need a projection, not a wider `WHERE`.
///
/// `text_delta` rows never appear: the journal's `text` event is the
/// authoritative full answer and the deltas are a live-preview artifact. Most
/// stores hold none (the journal drops them at write time), but the filter is
/// contract, not assumption.
pub(crate) fn entries(
    conn: &Connection,
    id: i64,
    full: bool,
    after_seq: Option<i64>,
) -> Result<Value, DbError> {
    // `-1` rather than `0` when unfiltered: `seq` starts at 0
    // (`Store::record_event`'s first call), and a sentinel of 0 would silently
    // drop that opening row from every unfiltered fetch.
    let after = after_seq.unwrap_or(-1);
    let sql = "SELECT seq, ts, event_type, payload
         FROM events
         WHERE execution_id = ?1
           AND seq > ?2
           AND event_type IN ('stage', 'text', 'reasoning', 'tool_start',
                              'tool_result', 'speculation_discarded',
                              'turn_parked', 'turn_woken')
         ORDER BY seq ASC";
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(Value::Array(Vec::new())),
        Err(e) => return Err(e.into()),
    };
    let mapped = stmt.query_map([id, after], |r| {
        Ok(json!({
            "seq": r.get::<_, i64>(0)?,
            "ts": r.get::<_, String>(1)?,
            "type": r.get::<_, String>(2)?,
            "payload": r.get::<_, String>(3)?,
        }))
    })?;
    let mut rows = Vec::new();
    for row in mapped {
        rows.push(row?);
    }
    let names = tool_names(conn, id)?;
    Ok(Value::Array(
        rows.into_iter()
            .map(|row| journal_entry(row, full, &names))
            .collect(),
    ))
}

/// `call_id` -> tool name for every call this execution opened.
///
/// Resolved over the **whole** execution rather than over the batch being
/// returned, which is the detail that makes it correct under the incremental
/// poll: a `tool_result` routinely arrives in a later `after_seq` page than
/// the `tool_start` it answers, so a batch-local index would label the first
/// fetch's rows and silently stop labelling every later one — the failure that
/// looks like the feature working.
///
/// Only the two fields are read, via `json_extract`, so a long execution's
/// full argument objects never leave SQLite. Same `execution_id`-first bargain
/// as [`entries`]; a store predating the schema degrades to an empty map, and
/// an unlabelled row then falls back to a generic name rather than failing the
/// whole transcript.
fn tool_names(conn: &Connection, id: i64) -> Result<HashMap<String, String>, DbError> {
    let sql = "SELECT json_extract(payload, '$.call.call_id'),
                      json_extract(payload, '$.call.name')
               FROM events
               WHERE execution_id = ?1 AND event_type = 'tool_start'";
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(HashMap::new()),
        Err(e) => return Err(e.into()),
    };
    let rows = stmt.query_map([id], |r| {
        Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        if let (Some(call_id), Some(name)) = row? {
            out.insert(call_id, name);
        }
    }
    Ok(out)
}

/// Shape one raw `events` row into the transcript entry the page renders.
///
/// The payload is the internally-tagged `AgentEvent` JSON the store persisted
/// (`{"type":"text","text":…}`; rows written before #1886 spell the field
/// `delta`, so `text` reads both). Only the fields the transcript needs are
/// lifted out, keyed by the row's own `event_type` column. A payload that no
/// longer parses (a hand-edited store, a variant this binary predates) keeps
/// its seq/type header with no body rather than erroring — one unreadable row
/// must not blank the transcript around it.
fn journal_entry(row: Value, full: bool, names: &HashMap<String, String>) -> Value {
    let ty = row["type"].as_str().unwrap_or("").to_owned();
    let payload: Value = row["payload"]
        .as_str()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or(Value::Null);
    let mut out = json!({
        "seq": row["seq"],
        "ts": row["ts"],
        "type": ty,
    });
    if payload.is_null() {
        return out;
    }
    match ty.as_str() {
        "stage" => {
            out["label"] = payload["name"].clone();
        }
        "text" | "reasoning" => {
            // `text` carries `text` since #1886, `delta` before; `reasoning`
            // still carries `delta`. One bilingual read covers all three.
            let body = payload["text"]
                .as_str()
                .or_else(|| payload["delta"].as_str());
            set_journal_body(&mut out, body.unwrap_or(""), full);
        }
        "tool_start" => {
            out["call_id"] = payload["call"]["call_id"].clone();
            out["name"] = payload["call"]["name"].clone();
            let args = serde_json::to_string_pretty(&payload["call"]["input"]).unwrap_or_default();
            set_journal_body(&mut out, &args, full);
        }
        "tool_result" => {
            let call_id = payload["call_id"].as_str().unwrap_or_default();
            out["call_id"] = json!(call_id);
            out["duration_ms"] = payload["duration_ms"].clone();
            out["speculated"] = json!(payload["speculated"].as_bool().unwrap_or(false));
            // The name the call was made under. A result that resolves to no
            // call — a store whose `tool_start` rows were pruned, a journal
            // read mid-write — is labelled generically rather than left
            // anonymous or dropped.
            out["name"] = json!(names.get(call_id).map_or("tool", String::as_str));
            // `ToolOutput` is externally tagged: `{"ok":{"content":…}}` or
            // `{"error":{"message":…}}` — the tag is the ok/failed verdict.
            out["ok"] = json!(!payload["output"]["ok"].is_null());
            let body = payload["output"]["ok"]["content"]
                .as_str()
                .or_else(|| payload["output"]["error"]["message"].as_str())
                .unwrap_or("");
            set_journal_body(&mut out, body, full);
        }
        "speculation_discarded" => {
            out["call_id"] = payload["call_id"].clone();
            out["name"] = payload["name"].clone();
            out["reason"] = payload["reason"].clone();
        }
        // A parked span is the one thing that explains a wall-clock gap with
        // no events in it (#1857). Without its payload the row would say a
        // park happened but not what was waited on or for how long — which
        // is the entire question an operator opens this transcript to ask.
        "turn_parked" => {
            out["description"] = payload["description"].clone();
            out["poll_interval_secs"] = payload["poll_interval_secs"].clone();
            out["deadline_secs"] = payload["deadline_secs"].clone();
        }
        "turn_woken" => {
            out["reason"] = payload["reason"].clone();
            out["polls_used"] = payload["polls_used"].clone();
        }
        _ => {}
    }
    out
}

/// Attach `body` to a transcript entry, clipped to [`JOURNAL_BODY_CLIP`]
/// unless `full`, and record which of the two happened.
///
/// Shared with the sent-context messages (`sent_context`), so the transcript
/// and the reconstruction can never disagree about what "clipped" means or
/// where the cut falls.
pub(crate) fn set_journal_body(entry: &mut Value, body: &str, full: bool) {
    let clipped = !full && body.chars().count() > JOURNAL_BODY_CLIP;
    entry["body"] = if clipped {
        json!(truncate(body, JOURNAL_BODY_CLIP))
    } else {
        json!(body)
    };
    entry["truncated"] = json!(clipped);
}
