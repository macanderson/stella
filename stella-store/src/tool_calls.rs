// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `tool_calls` projection — one queryable row per tool call, folded
//! from the append-only `events` stream.
//!
//! # Why this is a projection and not a log
//!
//! `events` is the source of truth: every `tool_start` and `tool_result` is
//! appended verbatim, in order, inside the turn. But answering "how many
//! times was `grep` called, and how slow is it?" from that table means
//! JSON-scanning every payload in history. `tool_calls` is the normalized
//! fold that makes those questions an index seek.
//!
//! # Why it is written LIVE (the v18 change)
//!
//! Until v18 this projection had exactly one writer:
//! [`Store::materialize_tool_calls`], called once from the CLI's turn
//! finalizer. Two consequences followed, and both were load-bearing bugs
//! rather than cosmetic ones:
//!
//! 1. **An in-flight turn reported zero tool calls.** Every count surface —
//!    the executions table, the tool leaderboard, the daily activity
//!    rollup — reads this projection, so a running agent showed no tool
//!    activity at all until it finished. For a dashboard whose purpose is
//!    catching a degrading agent *while it degrades*, the number was
//!    structurally unable to arrive in time.
//!
//! 2. **A turn that never finished lost its calls permanently.** SIGKILL, a
//!    panic, a laptop lid closing on a dying battery — any of them skips the
//!    finalizer, so the fold never runs, and nothing ever runs it later. The
//!    events were all safely on disk; the rows derived from them never
//!    appeared. Observed in the wild at 15% of one workspace's calls.
//!
//! So the fold now runs on every event, in the same transaction that appends
//! it ([`Store::record_event`]). A row exists from the moment a call is
//! announced, and it is committed with the event that announced it — there is
//! no window in which the log and its projection disagree.
//!
//! [`Store::materialize_tool_calls`] survives, unchanged in meaning, as the
//! **repair** path: an idempotent re-fold of one execution's whole stream.
//! That is what makes the live write safe to get wrong. Any row lost to a
//! crash mid-transaction, written by a pre-v18 binary, or corrupted by a bug
//! in the live fold is recoverable by replaying the log — which is the entire
//! reason the log is the source of truth and this is not.

use rusqlite::{Connection, OptionalExtension, params};
use stella_protocol::{AgentEvent, ToolOutput};

use crate::{Result, Store, fnv_hex};

/// Where one tool call is in its lifecycle.
///
/// The distinction `ok: bool` alone cannot draw: a call that has been
/// announced but has not returned is not a failure, and rendering it as one
/// (which is what a bare `ok = false` forces) makes every live turn look like
/// it is erroring. `Running` is a real, observable state and the dashboard
/// draws it as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallState {
    /// Announced by `tool_start`; no `tool_result` yet.
    Running,
    /// Returned successfully.
    Ok,
    /// Returned an error, or was abandoned when its turn ended.
    Error,
}

impl ToolCallState {
    /// The stored string — the `tool_calls.state` CHECK constraint's domain.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }

    /// Parse a stored value. Anything unrecognized reads as [`Self::Error`]
    /// rather than panicking or vanishing: this parses bytes from a file an
    /// older or newer build may have written, and a call whose state cannot
    /// be established is one the operator should see, not one the dashboard
    /// should silently drop.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "running" => Self::Running,
            "ok" => Self::Ok,
            _ => Self::Error,
        }
    }

    /// The legacy boolean this state supersedes, kept in lockstep in the `ok`
    /// column so every pre-v18 reader keeps working. A running call is not
    /// yet a success.
    #[must_use]
    pub fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }
}

/// One normalized tool-call row for the `tool_calls` log — a queryable
/// per-call record projected from the `events` stream. Stores shape, timing,
/// and success (never the full output — `bytes_out` is its size).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallRow {
    pub call_id: String,
    pub name: String,
    /// `"native"` | `"mcp"` — the only surfaces the producers derive, from
    /// the `mcp__` name prefix. Skill and agent invocations are logged in
    /// their own tables (`skill_usage`, `agent_uses`), not as surfaces here.
    pub surface: String,
    pub args_json: String,
    pub args_digest: String,
    /// Free-text "why" for the call. Currently always empty — the event
    /// stream the producer normalizes from carries no reason, so the column
    /// waits for a producer that captures one.
    pub reason: String,
    pub state: ToolCallState,
    pub error: String,
    pub bytes_out: i64,
    pub duration_ms: i64,
}

impl ToolCallRow {
    /// Whether the call succeeded — the `ok` column, **derived** rather than
    /// stored beside `state`.
    ///
    /// The table keeps both (`ok` for every pre-v18 reader, `state` for the
    /// lifecycle), but two struct fields that must agree are a disagreement
    /// waiting to be introduced by the next person who sets one of them. In
    /// memory there is one field and one answer.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.state.is_ok()
    }
}

/// The error stored on a call whose turn ended before it returned.
///
/// Named because two places must agree on it: the repair fold writes it, and
/// [`Store::reconcile_interrupted_executions`] writes it when sweeping a
/// crashed run's still-`running` rows. A call abandoned by a dead process and
/// one abandoned by a clean turn end are the same fact.
pub(crate) const ABANDONED: &str = "no result (turn ended before the tool returned)";

/// `'mcp'` for a namespaced MCP tool, `'native'` otherwise — the only two
/// surfaces either producer emits.
fn surface_of(name: &str) -> &'static str {
    if name.starts_with("mcp__") {
        "mcp"
    } else {
        "native"
    }
}

/// Fold one `tool_start` into the projection, inside the caller's
/// transaction.
///
/// A re-announced `call_id` updates the row it already has rather than
/// minting a second one: one `call_id` is one call however many times the
/// stream announced it, and a second row would double the count *and* leave
/// the result — keyed by `call_id` alone — attaching to whichever row it
/// found first. The first announcement keeps the position; the latest
/// payload wins, matching the repair fold's keep-rule exactly.
///
/// The position is `max(seq) + 1` over the execution, read under the
/// caller's write transaction so two concurrent appends cannot pick the same
/// one. `seq` is the call's ordinal within its execution — not the event
/// stream's `seq`, which counts every event of every kind.
fn project_tool_start(
    tx: &Connection,
    execution_id: i64,
    call_id: &str,
    name: &str,
    args_json: &str,
) -> rusqlite::Result<()> {
    let existing: Option<i64> = tx
        .query_row(
            "SELECT seq FROM tool_calls WHERE execution_id = ?1 AND call_id = ?2",
            params![execution_id, call_id],
            |row| row.get(0),
        )
        .optional()?;
    let digest = fnv_hex(args_json);
    if let Some(seq) = existing {
        // Re-announcement: refresh the payload, keep the position and
        // whatever terminal state a result may already have set.
        tx.execute(
            "UPDATE tool_calls SET name = ?3, surface = ?4, args_json = ?5, args_digest = ?6 \
             WHERE execution_id = ?1 AND seq = ?2",
            params![execution_id, seq, name, surface_of(name), args_json, digest],
        )?;
        return Ok(());
    }
    let seq: i64 = tx.query_row(
        "SELECT coalesce(max(seq) + 1, 0) FROM tool_calls WHERE execution_id = ?1",
        params![execution_id],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO tool_calls \
         (execution_id, seq, call_id, name, surface, args_json, args_digest, reason, \
          ok, state, error, bytes_out, duration_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '', 0, 'running', '', 0, 0)",
        params![
            execution_id,
            seq,
            call_id,
            name,
            surface_of(name),
            args_json,
            digest
        ],
    )?;
    Ok(())
}

/// Fold one `tool_result` into the projection, inside the caller's
/// transaction.
///
/// Normally this closes the row its `tool_start` opened. When no such row
/// exists — a result whose start was never persisted, which a pre-v18 stream
/// or a torn write can produce — it inserts a complete row instead of
/// dropping the result on the floor. A call that demonstrably happened must
/// appear in the count; attributing it with an unknown name is a smaller lie
/// than omitting it.
fn project_tool_result(
    tx: &Connection,
    execution_id: i64,
    call_id: &str,
    state: ToolCallState,
    error: &str,
    bytes_out: i64,
    duration_ms: i64,
) -> rusqlite::Result<()> {
    let updated = tx.execute(
        "UPDATE tool_calls \
         SET ok = ?3, state = ?4, error = ?5, bytes_out = ?6, duration_ms = ?7 \
         WHERE execution_id = ?1 AND call_id = ?2",
        params![
            execution_id,
            call_id,
            i64::from(state.is_ok()),
            state.as_str(),
            error,
            bytes_out,
            duration_ms
        ],
    )?;
    if updated > 0 {
        return Ok(());
    }
    let seq: i64 = tx.query_row(
        "SELECT coalesce(max(seq) + 1, 0) FROM tool_calls WHERE execution_id = ?1",
        params![execution_id],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO tool_calls \
         (execution_id, seq, call_id, name, surface, args_json, args_digest, reason, \
          ok, state, error, bytes_out, duration_ms) \
         VALUES (?1, ?2, ?3, '(unknown)', 'native', '{}', '', '', ?4, ?5, ?6, ?7, ?8)",
        params![
            execution_id,
            seq,
            call_id,
            i64::from(state.is_ok()),
            state.as_str(),
            error,
            bytes_out,
            duration_ms
        ],
    )?;
    Ok(())
}

/// Fold one event into the `tool_calls` projection, inside the caller's
/// transaction. Every non-tool event is a no-op.
///
/// Takes the already-appended event so the projection and the log it derives
/// from commit together — see this module's header for why that atomicity is
/// the point.
pub(crate) fn project_event(
    tx: &Connection,
    execution_id: i64,
    event: &AgentEvent,
) -> rusqlite::Result<()> {
    match event {
        AgentEvent::ToolStart { call } => {
            let args_json = serde_json::to_string(&call.input).unwrap_or_else(|_| "{}".into());
            project_tool_start(tx, execution_id, &call.call_id, &call.name, &args_json)
        }
        AgentEvent::ToolResult {
            call_id,
            output,
            duration_ms,
            ..
        } => {
            let (state, error, bytes) = match output {
                ToolOutput::Ok { content } => {
                    (ToolCallState::Ok, String::new(), content.len() as i64)
                }
                ToolOutput::Error { message } => {
                    let len = message.len() as i64;
                    (ToolCallState::Error, message.clone(), len)
                }
            };
            project_tool_result(
                tx,
                execution_id,
                call_id,
                state,
                &error,
                bytes,
                *duration_ms as i64,
            )
        }
        _ => Ok(()),
    }
}

impl Store {
    /// Record the normalized per-call `tool_calls` log for an execution.
    /// `seq` is the call's index in the batch; UNIQUE (execution_id, seq)
    /// guards double-writes. One transaction.
    ///
    /// This is the bulk writer the repair fold uses. The live path
    /// ([`Store::record_event`]) writes one row at a time instead, inside the
    /// event's own transaction.
    pub fn record_tool_calls(&self, execution_id: i64, calls: &[ToolCallRow]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for (seq, row) in calls.iter().enumerate() {
            tx.execute(
                "INSERT OR REPLACE INTO tool_calls \
                 (execution_id, seq, call_id, name, surface, args_json, args_digest, \
                  reason, ok, state, error, bytes_out, duration_ms) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    execution_id,
                    seq as i64,
                    row.call_id,
                    row.name,
                    row.surface,
                    row.args_json,
                    row.args_digest,
                    row.reason,
                    row.ok() as i64,
                    row.state.as_str(),
                    row.error,
                    row.bytes_out,
                    row.duration_ms,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Re-fold the `tool_calls` projection for one execution from its
    /// already-persisted `events` stream — the **repair** path.
    ///
    /// The live fold in [`Store::record_event`] keeps this table current
    /// during a turn; this rebuilds it from the log, which is what makes any
    /// live-write loss recoverable. It is called at turn end (so a stream
    /// written by a pre-v18 binary still lands) and by
    /// [`Store::reconcile_interrupted_executions`] at startup (so a crashed
    /// turn's calls appear even though its finalizer never ran).
    ///
    /// Rows are emitted in call order; a `tool_start` with no matching result
    /// (turn cut off mid-tool) is recorded as an abandoned, failed call so
    /// the count stays honest. A re-emitted `tool_start` for an already-seen
    /// `call_id` folds into that call's one row (first announcement keeps the
    /// position, the last one's payload wins), because one `call_id` is one
    /// call no matter how many times the stream announced it. Idempotent
    /// (INSERT OR REPLACE on seq). Returns the count.
    pub fn materialize_tool_calls(&self, execution_id: i64) -> Result<usize> {
        let payloads: Vec<String> = {
            let conn = self.lock();
            let mut stmt = conn.prepare(
                "SELECT payload FROM events \
                 WHERE execution_id = ?1 AND event_type IN ('tool_start', 'tool_result') \
                 ORDER BY seq ASC",
            )?;
            let rows = stmt.query_map(params![execution_id], |row| row.get::<_, String>(0))?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            out
        };

        use std::collections::HashMap;
        let mut order: Vec<String> = Vec::new();
        // call_id -> (name, surface, args_json)
        let mut starts: HashMap<String, (String, String, String)> = HashMap::new();
        // call_id -> (state, error, bytes_out, duration_ms)
        let mut results: HashMap<String, (ToolCallState, String, i64, i64)> = HashMap::new();

        for payload in &payloads {
            let Ok(ev) = serde_json::from_str::<AgentEvent>(payload) else {
                continue;
            };
            match ev {
                AgentEvent::ToolStart { call } => {
                    let args_json =
                        serde_json::to_string(&call.input).unwrap_or_else(|_| "{}".into());
                    // One position per call_id: pushing every announcement
                    // would mint a second row (and double the call count)
                    // for a re-emitted start, and its result — keyed by
                    // call_id alone — would attach to both.
                    if !starts.contains_key(&call.call_id) {
                        order.push(call.call_id.clone());
                    }
                    let surface = surface_of(&call.name).to_string();
                    starts.insert(call.call_id, (call.name, surface, args_json));
                }
                AgentEvent::ToolResult {
                    call_id,
                    output,
                    duration_ms,
                    ..
                } => {
                    let (state, error, bytes) = match output {
                        ToolOutput::Ok { content } => {
                            (ToolCallState::Ok, String::new(), content.len() as i64)
                        }
                        ToolOutput::Error { message } => {
                            let len = message.len() as i64;
                            (ToolCallState::Error, message, len)
                        }
                    };
                    results.insert(call_id, (state, error, bytes, duration_ms as i64));
                }
                _ => {}
            }
        }

        let mut rows: Vec<ToolCallRow> = Vec::with_capacity(order.len());
        for call_id in &order {
            let Some((name, surface, args_json)) = starts.get(call_id) else {
                continue;
            };
            let digest = fnv_hex(args_json);
            let (state, error, bytes_out, duration_ms) = match results.get(call_id) {
                Some((state, error, bytes, dur)) => (*state, error.clone(), *bytes, *dur),
                None => (ToolCallState::Error, ABANDONED.to_string(), 0, 0),
            };
            rows.push(ToolCallRow {
                call_id: call_id.clone(),
                name: name.clone(),
                surface: surface.clone(),
                args_json: args_json.clone(),
                args_digest: digest,
                reason: String::new(),
                state,
                error,
                bytes_out,
                duration_ms,
            });
        }
        let n = rows.len();
        self.record_tool_calls(execution_id, &rows)?;
        Ok(n)
    }

    /// Every execution that was never closed out — no `finished_at` — oldest
    /// first. Each is either running right now or died without its finalizer.
    pub fn unfinished_executions(&self) -> Result<Vec<i64>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT id FROM executions WHERE finished_at IS NULL ORDER BY id ASC")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Repair the observable plane for executions that never closed out, and
    /// return how many were repaired.
    ///
    /// This is the answer to "my machine died — what did I lose?" For each
    /// execution with no `finished_at`:
    ///
    /// 1. **Re-fold its tool calls from the log.** Idempotent, and the whole
    ///    point: a turn killed mid-flight has every one of its events safely
    ///    in `events`, and before this the rows derived from them simply
    ///    never appeared. Replaying the log recovers them exactly.
    /// 2. **Close any call still marked `running`** — an emergent effect of
    ///    the re-fold, not a separate pass: a `tool_start` with no
    ///    `tool_result` in the log folds to an abandoned error. A process
    ///    that is gone is not going to deliver those results, and a call
    ///    left `running` forever would keep counting as in-flight on every
    ///    future dashboard load.
    ///
    /// What it deliberately does **not** do is stamp an outcome on the
    /// execution. This runs at store open, and a second session opening the
    /// same workspace must not declare a *live* turn dead — the caller who
    /// knows the process is gone
    /// ([`Store::mark_execution_interrupted`]) makes that call. The re-fold
    /// is safe against a live execution with one visible wrinkle: an
    /// announced-but-unreturned call temporarily folds to that abandoned
    /// error, and is re-opened by its own `tool_result` when it lands.
    ///
    /// Best-effort per execution: one that fails to re-fold does not stop the
    /// rest, because a single unreadable stream must not block recovery of
    /// every other run in the workspace.
    pub fn reconcile_interrupted_executions(&self) -> Result<usize> {
        let unfinished = self.unfinished_executions()?;
        let mut repaired = 0;
        for execution_id in unfinished {
            if self.materialize_tool_calls(execution_id).is_ok() {
                repaired += 1;
            }
        }
        Ok(repaired)
    }

    /// Close one execution that died without its finalizer: stamp the
    /// outcome, and settle every call still marked `running`.
    ///
    /// Separate from [`Store::reconcile_interrupted_executions`] because only
    /// the caller can know the owning process is actually gone — see there.
    /// `finished_at` is taken from the execution's last event rather than the
    /// clock, so a run that died on Friday is not dated to the Monday someone
    /// reopened the workspace.
    pub fn mark_execution_interrupted(&self, execution_id: i64) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE tool_calls SET state = 'error', ok = 0, error = ?2 \
             WHERE execution_id = ?1 AND state = 'running'",
            params![execution_id, ABANDONED],
        )?;
        tx.execute(
            "UPDATE executions \
             SET finished_at = coalesce(
                     (SELECT max(ts) FROM events WHERE execution_id = ?1),
                     CURRENT_TIMESTAMP),
                 outcome = 'interrupted', \
                 usage_complete = 0, usage_status = 'incomplete' \
             WHERE id = ?1 AND finished_at IS NULL",
            params![execution_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The most recent `tool_calls` rows for one tool, newest first, capped
    /// at `limit`. Reads through the `tool_calls_by_name` index. The Tool
    /// Foundry gap detector consumes this for `name = "bash"` to mine
    /// repeated command shapes (`stella_core::detect_tool_gaps`); it is a
    /// general reader, not bash-specific. `limit == 0` returns an empty vec.
    pub fn recent_tool_calls(&self, name: &str, limit: usize) -> Result<Vec<ToolCallRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT call_id, name, surface, args_json, args_digest, reason, \
                    state, error, bytes_out, duration_ms \
             FROM tool_calls WHERE name = ?1 \
             ORDER BY execution_id DESC, seq DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![name, limit as i64], |r| {
            Ok(ToolCallRow {
                call_id: r.get(0)?,
                name: r.get(1)?,
                surface: r.get(2)?,
                args_json: r.get(3)?,
                args_digest: r.get(4)?,
                reason: r.get(5)?,
                state: ToolCallState::parse(&r.get::<_, String>(6)?),
                error: r.get(7)?,
                bytes_out: r.get(8)?,
                duration_ms: r.get(9)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

#[cfg(test)]
#[path = "tool_calls_tests.rs"]
mod tool_calls_tests;
