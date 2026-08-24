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
///
/// `reasoning` gets no such help at write time, because it has no
/// authoritative sibling: `AgentEvent::Reasoning` **is** the delta, so the
/// store holds one row per streamed fragment. [`coalesce_reasoning`] rejoins
/// them here, before shaping.
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
                              'turn_parked', 'turn_woken', 'file_change',
                              'candidate_delivery', 'turn_complete',
                              'steering_withheld', 'step_usage')
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
        coalesce_reasoning(rows)
            .into_iter()
            .map(|row| journal_entry(row, full, &names))
            .collect(),
    ))
}

/// One journal row by its seq, projected exactly as [`entries`] would project
/// it — the incremental tail protocol's answer recovery (#4566): a poll whose
/// suffix carries no `text` row re-reads the one the cursor remembers, an
/// indexed point read rather than a rescan.
///
/// `Value::Null` when the row does not exist (or the schema predates the
/// events table): the caller renders without an answer rather than failing
/// the tick. Tool names are not resolved — the one caller reads `text` rows,
/// which never need them.
pub(crate) fn entry_at(conn: &Connection, id: i64, seq: i64, full: bool) -> Result<Value, DbError> {
    let sql = "SELECT seq, ts, event_type, payload
         FROM events WHERE execution_id = ?1 AND seq = ?2";
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(Value::Null),
        Err(e) => return Err(e.into()),
    };
    let row = stmt
        .query_map([id, seq], |r| {
            Ok(json!({
                "seq": r.get::<_, i64>(0)?,
                "ts": r.get::<_, String>(1)?,
                "type": r.get::<_, String>(2)?,
                "payload": r.get::<_, String>(3)?,
            }))
        })?
        .next()
        .transpose()?;
    Ok(row.map_or(Value::Null, |row| journal_entry(row, full, &HashMap::new())))
}

/// Rejoin a run of consecutive `reasoning` rows into the one block of thought
/// it was streamed from.
///
/// `AgentEvent::Reasoning { delta }` is a *fragment* — the provider's stream
/// emits one per few tokens and `stella-cli`'s persistence hop writes every
/// one of them (`agent::persistence`, which drops only `TextDelta`, the
/// preview whose full text arrives later as `Text`). Reasoning has no such
/// later full event, so nothing upstream can drop the fragments and the store
/// legitimately holds them all. The consequence lands here: a turn that
/// thought for a page produced tens of thousands of `reasoning` rows, and the
/// transcript drew a titled, boxed fold around each one — `Let`, then
/// `me look`, then `at the issue. I` — which is unreadable as prose and, at
/// 39,778 rows on one execution, expensive to ship and to paint.
///
/// Only *adjacent* rows merge, so a reasoning block interrupted by a tool call
/// stays two blocks: the boundary between thoughts is a fact about the turn,
/// not a rendering artifact. The merged row keeps the **first** fragment's
/// timestamp — when the thinking began, which is what orders it against the
/// steps around it — and the **last** fragment's `seq`, which is what the
/// page's incremental poll sends back as `after_seq`; taking the first there
/// would re-fetch the whole run forever.
///
/// Merging before [`journal_entry`] rather than after is deliberate: the body
/// clip must measure the block a reader sees, not a fragment. Applied
/// per-fragment it never fired at all, so `truncated` was uniformly false on
/// exactly the transcripts most in need of the flag.
///
/// The live case still splits: fragments that arrive after a poll's cursor
/// cannot join a block already sent, so a running turn grows one fold per
/// poll rather than one per token. That is bounded by the poll interval
/// instead of by the model's token rate, and a finished execution — every
/// re-open of it, and every render of the standalone page — coalesces whole.
fn coalesce_reasoning(rows: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(rows.len());
    for row in rows {
        let is_reasoning = row["type"].as_str() == Some("reasoning");
        // A payload that no longer parses is left alone rather than merged:
        // `journal_entry` renders it as a bodyless header, and folding an
        // unreadable row into a readable one would silently discard the fact
        // that it was there.
        let delta = is_reasoning.then(|| reasoning_delta(&row)).flatten();
        let Some(delta) = delta else {
            out.push(row);
            continue;
        };
        let open_run = out
            .last_mut()
            .filter(|prev| prev["type"].as_str() == Some("reasoning"))
            .and_then(|prev| reasoning_delta(prev).map(|joined| (prev, joined)));
        match open_run {
            Some((prev, joined)) => {
                prev["seq"] = row["seq"].clone();
                let merged = json!({ "type": "reasoning", "delta": joined + &delta });
                prev["payload"] = json!(merged.to_string());
            }
            None => out.push(row),
        }
    }
    out
}

/// The reasoning text carried by one raw `events` row, or `None` if its
/// payload does not parse. Reads `delta` (the field `AgentEvent::Reasoning`
/// serializes) and falls back to `text` for the same bilingual reason
/// [`journal_entry`] does.
fn reasoning_delta(row: &Value) -> Option<String> {
    let payload: Value = serde_json::from_str(row["payload"].as_str()?).ok()?;
    let text = payload["delta"]
        .as_str()
        .or_else(|| payload["text"].as_str())?;
    Some(text.to_owned())
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
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
        ))
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
        // What the turn changed on disk, with the diff (#2421 gave the deck
        // and the plain surface this; the dashboard's transcript still ended
        // at "some files were touched", listed by path in a side panel with
        // no way to see WHAT changed — the one question a transcript is read
        // to answer).
        //
        // `added`/`removed` ride the event from git's own numstat and are
        // passed through untouched: they are the real delta, never recounted
        // from `diff`, which is a bounded rendering of the changed region.
        "file_change" => {
            out["path"] = payload["path"].clone();
            out["kind"] = payload["kind"].clone();
            out["added"] = payload["added"].clone();
            out["removed"] = payload["removed"].clone();
            if let Some(text) = payload["diff"].as_str().filter(|d| !d.trim().is_empty()) {
                // Text on the wire, hunks on the page: the event carries git's
                // `-p` patch because it is measured by shelling to git, and the
                // page has exactly one diff renderer, which draws hunks.
                //
                // `full` (the `?full=1` escape hatch) lifts the cap here the
                // same way it lifts the body clip above — a reader who asked
                // for everything gets everything.
                let cap = if full {
                    usize::MAX
                } else {
                    stella_diff::view::VIEW_CAP
                };
                let view = stella_diff::view::elide(&stella_diff::parse::hunks(text), cap);
                // `stella_diff::json::view` is the one place `{hunks, elided,
                // fold_before}` is assembled (#1511) — merged onto `out`
                // rather than hand-written here too.
                if let Value::Object(view_fields) = stella_diff::json::view(&view) {
                    out.as_object_mut()
                        .expect("object literal")
                        .extend(view_fields);
                }
            }
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
        // The delivery decision (#2942). `DeliveryOutcome` is nested under
        // `delivery` (not flattened — see `AgentEvent::CandidateDelivery`) and
        // internally tagged on `outcome`. Lift the tag plus whichever arm's
        // fields it selects so the transcript can say "delivered N files" or
        // "declined: <reason>" instead of drawing a blank row where the run
        // most needs to answer "did this candidate's work land?".
        "candidate_delivery" => {
            let delivery = &payload["delivery"];
            out["outcome"] = delivery["outcome"].clone();
            // `Delivered` counts — absent (null) on a decline, which the
            // renderer distinguishes by `outcome` rather than by presence.
            out["created"] = delivery["created"].clone();
            out["modified"] = delivery["modified"].clone();
            out["deleted"] = delivery["deleted"].clone();
            out["lines_added"] = delivery["lines_added"].clone();
            out["lines_removed"] = delivery["lines_removed"].clone();
            out["proven"] = delivery["proven"].clone();
            // `Declined` reason — absent on a delivery.
            out["reason"] = delivery["reason"].clone();
        }
        // The engine's per-turn ending (#3379). Selected so a reader can see
        // where one turn stopped and the next began — a staged run with a
        // revise loop is several turns, and before this signal existed the
        // journal showed one undivided sequence of steps.
        "turn_complete" => {
            out["model"] = payload["model"].clone();
            out["cost_usd"] = payload["cost_usd"].clone();
        }
        // The metering record of one committed model call. This is the row
        // that makes the transcript auditable per step: which provider served
        // the call (and, behind a gateway, which upstream vendor), what it
        // cost in tokens / cache traffic / wall clock, and why generation
        // stopped. Everything lifted is content-free except `output_text`,
        // which management and compaction calls carry because they emit no
        // separate `text` event — it takes the same clip policy as every
        // other body.
        "step_usage" => {
            for key in [
                "step",
                "role",
                "provider",
                "upstream_provider",
                "model",
                "input_tokens",
                "output_tokens",
                "cached_input_tokens",
                "cache_write_tokens",
                "reasoning_tokens",
                "estimated_input_tokens",
                "cost_usd",
                "duration_ms",
                "retries",
                "tool_calls",
                "complete",
                "finish_reason",
                "effort",
                "max_output_tokens",
                "sub_agent_id",
            ] {
                if !payload[key].is_null() {
                    out[key] = payload[key].clone();
                }
            }
            if let Some(text) = payload["output_text"].as_str() {
                set_journal_body(&mut out, text, full);
            }
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
