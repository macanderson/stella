// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `context_recall` projection. Split out of `db.rs`, which sat close to
//! the 1500-line cap. AGENTS.md's rule is to put new logic in a small
//! sibling file, not to grow the big one. `db/recall.rs` sits beside
//! `db.rs`, the way `crates/stella-core/src/driver/settlement.rs` sits
//! beside `driver.rs`.

use rusqlite::Connection;
use serde_json::{Value, json};

use super::{DbError, collect_rows_for};

/// One execution's context-recall events, read from its event stream.
///
/// Recall runs early in every turn, so a slow recall slows the whole turn.
/// Before this, a cold store or a stuck embedding call looked just like a
/// fast recall, right up to the moment it timed out.
///
/// This reads `events` directly, not a saved copy, and only on the
/// per-execution detail page. The `execution_id` filter uses the store's own
/// index, so it reads one turn's rows, not the whole table.
///
/// `latency_ms` can be missing on an old event. A missing value reads as
/// `null`, never `0`. "Not measured" and "took no time" are different facts,
/// and the dashboard must not mix them up.
///
/// `frames`, `provider_mix` and `usage` pass through as the raw JSON the
/// writer stored. `stella-protocol`'s `ContextFrameRef` and `ContextUsage`
/// already name each field the way a reader needs it: `citation_label`,
/// `kind`, `provider`, `source`, `content_digest`, `token_cost`, and the
/// per-provider `frames_served` and `frames_rejected` counts. Renaming a
/// field here would give it one more place to drift from the wire. A
/// `json_array_length` count would only say how many frames came back,
/// never which ones.
pub(super) fn recall_timings(conn: &Connection, execution_id: i64) -> Result<Vec<Value>, DbError> {
    collect_rows_for(
        conn,
        execution_id,
        "SELECT ts,
                json_extract(payload, '$.latency_ms'),
                json_extract(payload, '$.used_ann_index'),
                json_extract(payload, '$.tokens'),
                json_extract(payload, '$.frames'),
                json_extract(payload, '$.provider_mix'),
                json_extract(payload, '$.usage')
         FROM events
         WHERE execution_id = ?1 AND event_type = 'context_recall'
         ORDER BY seq ASC",
        |r| {
            Ok(json!({
                "ts": r.get::<_, String>(0)?,
                "latency_ms": r.get::<_, Option<i64>>(1)?,
                "used_ann_index": r.get::<_, Option<i64>>(2)?.map(|v| v != 0),
                "tokens": r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                "frames": parse_json_array(r.get::<_, Option<String>>(4)?),
                "provider_mix": parse_json_array(r.get::<_, Option<String>>(5)?),
                "usage": r
                    .get::<_, Option<String>>(6)?
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok()),
            }))
        },
    )
}

/// Turn one `json_extract` array cell back into a [`Value`].
///
/// A missing key, or JSON that fails to parse, becomes `[]`; the row is not
/// dropped. This is the same rule [`super::Observatory::session_turn_diff`]
/// uses for its `files` field: a broken store loses one field, not the
/// whole page.
fn parse_json_array(text: Option<String>) -> Value {
    text.and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(|| json!([]))
}
