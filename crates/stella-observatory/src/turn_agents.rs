// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The sub-agents one turn fanned out — the `delegate` children whose whole
//! recorded life lives under the parent's execution id, folded into one row
//! per child for the turn page.
//!
//! Two sources, joined by the child's `agent_id`:
//!
//! - The `sub_agent` bracket in the `events` journal (`Started`/`Finished`,
//!   `stella_protocol::SubAgentPhase`) carries the child's task, budget,
//!   depth, write access, pinned reasoning effort, start/finish timestamps,
//!   final status, cost, steps and summary. This is a sanctioned `events`
//!   read in the same sense as `sessions::execution_tendencies`: the filter
//!   is `execution_id`-first, served by the store's `UNIQUE (execution_id,
//!   seq)` index; a cross-execution children view would need a store-side
//!   projection, not a wider `WHERE`.
//! - The `telemetry` rows stamped with the child's `sub_agent_id` (schema
//!   v33, #4383) carry what the bracket cannot: the model and API provider
//!   the child's calls actually ran on, token totals, and wall-clock per
//!   call. A store older than v33 has no `sub_agent_id` column and degrades
//!   to bracket-only rows, not an error.
//!
//! A child with no `Finished` row reports `status: null` — the fold does not
//! guess whether it is still running or its parent died mid-flight; the
//! caller has the parent's own outcome to say which.
//!
//! # Lanes are the other fan-out, and they are a separate list
//!
//! A deck worker lane (`req:<n>` / `sub:<task-id>`) is not a `delegate` child:
//! it opens a **real execution row** of its own, with its own transcript at
//! `#transcript/<id>`, and nothing about it lives under the parent's id. So it
//! cannot join `agents` — every field that list carries comes from a bracket
//! and a metering stamp a lane has neither of. It rides as `lanes`, keyed off
//! `executions.parent_execution_id` (schema v36, #4628), and a store older
//! than that column answers with an empty list rather than an error.

use std::collections::BTreeMap;

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::db::{DbError, is_missing_schema};

/// One turn's sub-agents: `{ execution_id, agents: [...] }`, in the order
/// the children started.
pub(crate) fn execution_subagents(conn: &Connection, id: i64) -> Result<Value, DbError> {
    let mut agents: Vec<Value> = Vec::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();

    let mut stmt = match conn.prepare(
        "SELECT ts, payload FROM events
         WHERE execution_id = ?1 AND event_type = 'sub_agent'
         ORDER BY seq ASC",
    ) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(empty(id)),
        Err(e) => return Err(e.into()),
    };
    let mapped = stmt.query_map([id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    for row in mapped {
        let (ts, payload) = row?;
        let event: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
        // The journaled payload is the full internally-tagged event:
        // `{"type":"sub_agent","phase":{"phase":"started",…}}` — the bracket
        // rides one level down, under its own discriminant.
        let payload = event.get("phase").cloned().unwrap_or(Value::Null);
        let Some(agent_id) = payload.get("agent_id").and_then(Value::as_str) else {
            continue;
        };
        match payload.get("phase").and_then(Value::as_str) {
            Some("started") => {
                let entry = json!({
                    "agent_id": agent_id,
                    "instruction_preview": payload.get("instruction_preview").cloned().unwrap_or(Value::Null),
                    "effort": payload.get("effort").cloned().unwrap_or(Value::Null),
                    "budget_usd": payload.get("budget_usd").cloned().unwrap_or(Value::Null),
                    "write_access": payload.get("write_access").cloned().unwrap_or(json!(false)),
                    "depth": payload.get("depth").cloned().unwrap_or(json!(1)),
                    "started_ts": ts,
                    "finished_ts": Value::Null,
                    "status": Value::Null,
                    "summary": Value::Null,
                    "truncated": Value::Null,
                    "reason": Value::Null,
                    "cost_usd": Value::Null,
                    "steps": Value::Null,
                    "absorbed_messages": Value::Null,
                    "provider": Value::Null,
                    "model": Value::Null,
                    "models": 0,
                    "calls": 0,
                    "tokens_in": 0,
                    "tokens_out": 0,
                });
                index.insert(agent_id.to_string(), agents.len());
                agents.push(entry);
            }
            Some("finished") => {
                // A finish with no recorded start (a journal truncated at
                // the front) still gets a row: the finish carries enough to
                // be worth listing on its own.
                let at = *index.entry(agent_id.to_string()).or_insert_with(|| {
                    agents.push(json!({
                        "agent_id": agent_id,
                        "instruction_preview": Value::Null,
                        "effort": Value::Null,
                        "budget_usd": Value::Null,
                        "write_access": false,
                        "depth": 1,
                        "started_ts": Value::Null,
                        "provider": Value::Null,
                        "model": Value::Null,
                        "models": 0,
                        "calls": 0,
                        "tokens_in": 0,
                        "tokens_out": 0,
                    }));
                    agents.len() - 1
                });
                let entry = &mut agents[at];
                entry["finished_ts"] = json!(ts);
                for key in ["status", "summary", "truncated", "reason", "cost_usd"] {
                    entry[key] = payload.get(key).cloned().unwrap_or(Value::Null);
                }
                for key in ["steps", "absorbed_messages"] {
                    entry[key] = payload.get(key).cloned().unwrap_or(json!(0));
                }
            }
            _ => {}
        }
    }

    // The metering join. `MAX(model)` when a child spanned models is a
    // display pick, and `models` says when it happened rather than letting
    // the pick read as the whole story.
    let telemetry = conn.prepare(
        "SELECT sub_agent_id, MAX(provider), MAX(model), COUNT(DISTINCT model),
                COUNT(*), SUM(input_tokens), SUM(output_tokens), SUM(cost_usd)
         FROM telemetry
         WHERE execution_id = ?1 AND sub_agent_id IS NOT NULL
         GROUP BY sub_agent_id",
    );
    let mut telemetry = match telemetry {
        Ok(stmt) => stmt,
        // A pre-v33 store has no `sub_agent_id`: bracket-only rows.
        Err(e) if is_missing_schema(&e) => {
            return Ok(json!({
                "execution_id": id,
                "agents": agents,
                "lanes": dispatched_lanes(conn, id)?,
            }));
        }
        Err(e) => return Err(e.into()),
    };
    let mapped = telemetry.query_map([id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, i64>(6)?,
            r.get::<_, f64>(7)?,
        ))
    })?;
    for row in mapped {
        let (agent_id, provider, model, models, calls, tokens_in, tokens_out, cost) = row?;
        // A metered child with no bracket at all (both bracket rows lost)
        // still surfaces: the money is a fact about this turn.
        let at = *index.entry(agent_id.clone()).or_insert_with(|| {
            agents.push(json!({
                "agent_id": agent_id,
                "instruction_preview": Value::Null,
                "effort": Value::Null,
                "budget_usd": Value::Null,
                "write_access": false,
                "depth": 1,
                "started_ts": Value::Null,
                "finished_ts": Value::Null,
                "status": Value::Null,
                "summary": Value::Null,
                "truncated": Value::Null,
                "reason": Value::Null,
                "steps": Value::Null,
                "absorbed_messages": Value::Null,
                "cost_usd": Value::Null,
            }));
            agents.len() - 1
        });
        let entry = &mut agents[at];
        entry["provider"] = json!(provider);
        entry["model"] = json!(model);
        entry["models"] = json!(models);
        entry["calls"] = json!(calls);
        entry["tokens_in"] = json!(tokens_in);
        entry["tokens_out"] = json!(tokens_out);
        if entry["cost_usd"].is_null() {
            entry["cost_usd"] = json!(cost);
        }
    }

    Ok(json!({
        "execution_id": id,
        "agents": agents,
        "lanes": dispatched_lanes(conn, id)?,
    }))
}

/// The deck worker lanes this turn dispatched, oldest first.
///
/// Each is a whole execution — its own prompt, outcome, cost and transcript —
/// so the row carries enough to be opened rather than only counted. A store
/// predating `executions.parent_execution_id` answers with an empty list: a
/// turn whose lanes cannot be named reads as a turn that dispatched none,
/// which is the same thing every such store could ever have said.
fn dispatched_lanes(conn: &Connection, id: i64) -> Result<Vec<Value>, DbError> {
    let mut stmt = match conn.prepare(
        "SELECT id, kind, prompt, outcome, cost_usd, started_at, finished_at
         FROM executions WHERE parent_execution_id = ?1 ORDER BY id ASC",
    ) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mapped = stmt.query_map([id], |r| {
        Ok(json!({
            "execution_id": r.get::<_, i64>(0)?,
            "kind": r.get::<_, String>(1)?,
            // Clipped like the sessions listing's, and for the same reason:
            // a lane's prompt is carried whole in the store and a fan-out list
            // has no use for a multi-kilobyte one. Its own transcript page
            // serves the full text.
            "prompt": crate::db::truncate(&r.get::<_, String>(2)?, 240),
            "outcome": r.get::<_, Option<String>>(3)?,
            "cost_usd": r.get::<_, f64>(4)?,
            "started_at": r.get::<_, String>(5)?,
            "finished_at": r.get::<_, Option<String>>(6)?,
        }))
    })?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row?);
    }
    Ok(out)
}

/// The shape a store with no `events` table answers with.
fn empty(id: i64) -> Value {
    json!({ "execution_id": id, "agents": [], "lanes": [] })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two tables in the store's current shape — the schema_conformance
    /// suite holds these fixtures to the real `Store::open` migrations.
    fn conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch(
            "CREATE TABLE events (
               execution_id INTEGER NOT NULL,
               seq INTEGER NOT NULL,
               ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               event_type TEXT NOT NULL,
               payload TEXT NOT NULL,
               UNIQUE (execution_id, seq)
             );
             CREATE TABLE telemetry (
               execution_id INTEGER NOT NULL,
               step INTEGER NOT NULL,
               ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               provider TEXT NOT NULL,
               call_role TEXT NOT NULL DEFAULT 'unknown',
               model TEXT NOT NULL,
               input_tokens INTEGER NOT NULL,
               output_tokens INTEGER NOT NULL,
               cache_read_tokens INTEGER NOT NULL,
               cache_miss_tokens INTEGER NOT NULL,
               cost_usd REAL NOT NULL,
               duration_ms INTEGER NOT NULL,
               retries INTEGER NOT NULL,
               tool_calls INTEGER NOT NULL,
               sub_agent_id TEXT,
               UNIQUE (execution_id, step)
             );",
        )
        .expect("schema");
        conn
    }

    fn event(conn: &Connection, id: i64, seq: i64, ts: &str, bracket: Value) {
        // The journaled payload is the full internally-tagged event, bracket
        // one level down — the shape `Store::record_event` writes, held to
        // the real writer by the schema_conformance suite.
        let payload = json!({ "type": "sub_agent", "phase": bracket });
        conn.execute(
            "INSERT INTO events (execution_id, seq, ts, event_type, payload)
             VALUES (?1, ?2, ?3, 'sub_agent', ?4)",
            rusqlite::params![id, seq, ts, payload.to_string()],
        )
        .expect("event");
    }

    /// **The witness for the fold.** A started+finished bracket joined with
    /// its metering rows becomes one row carrying task, effort, timestamps,
    /// status, spend, model and provider; an unfinished child reports a null
    /// status rather than a guessed one.
    #[test]
    fn a_bracket_and_its_metering_fold_into_one_row_per_child() {
        let conn = conn();
        event(
            &conn,
            7,
            1,
            "2026-08-23 10:00:00",
            json!({
                "phase": "started", "agent_id": "search-1",
                "instruction_preview": "find the retry policy",
                "budget_usd": 0.25, "write_access": false, "depth": 1,
                "effort": "high",
            }),
        );
        event(
            &conn,
            7,
            2,
            "2026-08-23 10:00:01",
            json!({
                "phase": "started", "agent_id": "audit-2",
                "instruction_preview": "audit the ports",
                "budget_usd": null, "write_access": true, "depth": 1,
            }),
        );
        event(
            &conn,
            7,
            9,
            "2026-08-23 10:02:30",
            json!({
                "phase": "finished", "agent_id": "search-1",
                "status": "completed", "summary": "retry policy lives in retry.rs",
                "truncated": false, "cost_usd": 0.004, "steps": 3,
                "absorbed_messages": 9,
            }),
        );
        for (step, agent, tokens) in [
            (1, "search-1", 900),
            (2, "search-1", 400),
            (3, "audit-2", 70),
        ] {
            conn.execute(
                "INSERT INTO telemetry (execution_id, step, provider, model, input_tokens,
                   output_tokens, cache_read_tokens, cache_miss_tokens, cost_usd,
                   duration_ms, retries, tool_calls, sub_agent_id)
                 VALUES (7, ?1, 'zai', 'glm-5.2', ?2, 50, 0, 0, 0.002, 1200, 0, 1, ?3)",
                rusqlite::params![step, tokens, agent],
            )
            .expect("telemetry");
        }

        let out = execution_subagents(&conn, 7).expect("fold");
        let agents = out["agents"].as_array().expect("agents");
        assert_eq!(agents.len(), 2, "{out}");

        let search = &agents[0];
        assert_eq!(search["agent_id"], "search-1");
        assert_eq!(search["effort"], "high");
        assert_eq!(search["status"], "completed");
        assert_eq!(search["started_ts"], "2026-08-23 10:00:00");
        assert_eq!(search["finished_ts"], "2026-08-23 10:02:30");
        assert_eq!(search["provider"], "zai");
        assert_eq!(search["model"], "glm-5.2");
        assert_eq!(search["calls"], 2);
        assert_eq!(search["tokens_in"], 1300);
        assert_eq!(search["steps"], 3);
        assert_eq!(
            search["cost_usd"], 0.004,
            "the finish's settled cost outranks the metering sum"
        );

        let audit = &agents[1];
        assert_eq!(audit["agent_id"], "audit-2");
        assert_eq!(
            audit["status"],
            Value::Null,
            "no finish recorded — not guessed"
        );
        assert_eq!(audit["finished_ts"], Value::Null);
        assert_eq!(audit["effort"], Value::Null, "no pinned effort recorded");
        assert_eq!(audit["calls"], 1);
        assert_eq!(
            audit["cost_usd"], 0.002,
            "an unfinished child's spend is the metering sum"
        );
    }

    /// A turn with no children answers the empty shape, and a store whose
    /// `telemetry` predates `sub_agent_id` (v33) degrades to bracket-only
    /// rows instead of erroring.
    #[test]
    fn no_children_is_empty_and_an_old_store_degrades_to_bracket_only() {
        let conn = conn();
        let out = execution_subagents(&conn, 1).expect("empty");
        assert_eq!(out, json!({ "execution_id": 1, "agents": [], "lanes": [] }));

        conn.execute_batch(
            "ALTER TABLE telemetry RENAME TO telemetry_v33;
             CREATE TABLE telemetry AS SELECT execution_id, step, provider, model
             FROM telemetry_v33;",
        )
        .expect("drop the column");
        event(
            &conn,
            2,
            1,
            "2026-08-23 10:00:00",
            json!({
                "phase": "started", "agent_id": "d-1",
                "instruction_preview": "x", "write_access": false, "depth": 1,
            }),
        );
        let out = execution_subagents(&conn, 2).expect("degrades");
        assert_eq!(out["agents"].as_array().map(Vec::len), Some(1));
        assert_eq!(out["agents"][0]["provider"], Value::Null);
    }
}
