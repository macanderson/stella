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
//!
//! # What identifies a row (the v28 change, #4033)
//!
//! A row is identified by the **event-stream `seq` that announced it**, never
//! by its `call_id`.
//!
//! `call_id` is only unique within one model *response*. Several providers
//! mint it positionally — `moonshotai/kimi-k3` through OpenRouter emits
//! `{tool_name}:{index_within_response}` — so `read_file:0` is the id of the
//! first read in *every* response of a turn. Keyed on `(execution_id,
//! call_id)`, this projection read the second announcement as a
//! re-announcement of the first and updated that row in place. One observed
//! execution made 176 tool calls and projected **4 rows**; store-wide on that
//! workspace 595 of 4,874 calls (12.2%) were erased. The loss is worst exactly
//! where it matters most — a stuck, high-call-count turn collapses hardest, so
//! a degrading agent looks quiet.
//!
//! The engine never believed the premise. `stella_core`'s dispatch loop keys
//! its answered set "by original index, not call_id: ids are only guaranteed
//! unique within one response by SOME providers, and an id-keyed set would let
//! one answered duplicate silently absorb the other". Dispatch emits exactly
//! one `ToolStart` per call it runs, and answers same-id duplicates within one
//! response separately. Only this projection merged them.
//!
//! The announcing `seq` is the one identity that is unique per call by
//! construction, available to *both* writers (the live fold is handed it by
//! [`Store::record_event`], the repair fold selects it from `events`), and
//! **stable across a re-fold** — which is what keeps the repair idempotent and
//! keeps a genuine re-announcement (the same event folded twice) collapsing to
//! the one row it describes, instead of minting a second.
//!
//! A `tool_result` carries no such seq — `call_id` is the only identity it
//! shares with its start — so it attaches to the **oldest still-`running`**
//! row bearing that id. Within a step that is the call it answers; across
//! steps every earlier namesake has already settled, so it cannot reach back
//! and overwrite one.

use std::collections::VecDeque;

use rusqlite::{Connection, OptionalExtension, params};
use stella_protocol::{AgentEvent, ErrorClass, ToolOutput};

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
    /// Returned an error.
    Error,
    /// Never returned — its turn ended (or its process died) first.
    ///
    /// Distinct from [`Self::Error`] because the two answer different
    /// questions: an error is a fact about the *tool*, an abandonment is a
    /// fact about the *turn*. Folding them together (which is what this
    /// projection did before v24) charged an "error" to whatever tool
    /// happened to be in flight at every interrupt, inflating exactly the
    /// per-tool error rates a reliability ceiling reads (#3146).
    Abandoned,
}

impl ToolCallState {
    /// The stored string — the `tool_calls.state` CHECK constraint's domain.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Abandoned => "abandoned",
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
            "abandoned" => Self::Abandoned,
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
    /// Which kind of failure this was (#3145) — the `error_class` column.
    /// `None` is **unclassified**, not a class: either the call did not fail,
    /// or the site that produced the failure has not been audited into an
    /// [`ErrorClass`] yet. A per-tool error-rate ceiling must not read it as
    /// a defect.
    pub error_class: Option<ErrorClass>,
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

/// The stored spelling of an [`ErrorClass`] — its `snake_case` token, or the
/// empty string for unclassified. Named because three writers must agree on
/// it (the live fold, the repair fold's bulk insert, and the interrupted
/// sweep), and because `''` carries the meaning the column's whole point
/// rests on: *not audited into a class*, which is not the same as any class.
fn class_token(class: Option<ErrorClass>) -> &'static str {
    match class {
        Some(class) => class.as_str(),
        None => "",
    }
}

/// `'mcp'` for a namespaced MCP tool, `'native'` otherwise — the only two
/// surfaces either producer emits.
fn surface_of(name: &str) -> &'static str {
    if name.starts_with("mcp__") {
        "mcp"
    } else {
        "native"
    }
}

/// The `event_seq` of a row whose announcing event is not known: a call
/// recovered from a `tool_result` whose `tool_start` was never persisted, and
/// every row written by a build older than v28.
///
/// Negative on purpose — `tool_calls_by_event_seq` is UNIQUE only over
/// `event_seq >= 0`, so any number of unidentified rows coexist while every
/// identified one stays unique. A total unique index would fail to build on a
/// legacy file holding two of them, which fails the migration and takes the
/// workspace's whole store with it (the same reasoning that made the index it
/// replaces partial).
pub(crate) const UNKNOWN_EVENT_SEQ: i64 = -1;

/// Fold one `tool_start` into the projection, inside the caller's
/// transaction.
///
/// `event_seq` is the announcing event's position in the execution's event
/// stream, and it is this row's identity — see the module header for why
/// `call_id` cannot be (#4033). Folding the *same* announcement twice updates
/// the row it already has rather than minting a second; two distinct
/// announcements that merely share a `call_id` are two calls and get two rows.
///
/// The position is `max(seq) + 1` over the execution, read under the
/// caller's write transaction so two concurrent appends cannot pick the same
/// one. `seq` is the call's ordinal within its execution — not the event
/// stream's `seq`, which counts every event of every kind.
fn project_tool_start(
    tx: &Connection,
    execution_id: i64,
    event_seq: i64,
    call_id: &str,
    name: &str,
    args_json: &str,
) -> rusqlite::Result<()> {
    let existing: Option<i64> = tx
        .query_row(
            "SELECT seq FROM tool_calls \
             WHERE execution_id = ?1 AND event_seq = ?2 AND event_seq >= 0",
            params![execution_id, event_seq],
            |row| row.get(0),
        )
        .optional()?;
    let digest = fnv_hex(args_json);
    if let Some(seq) = existing {
        // The same announcement, folded again: refresh the payload, keep the
        // position and whatever terminal state a result may already have set.
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
         (execution_id, seq, event_seq, call_id, name, surface, args_json, args_digest, reason, \
          ok, state, error, bytes_out, duration_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, '', 0, 'running', '', 0, 0)",
        params![
            execution_id,
            seq,
            event_seq,
            call_id,
            name,
            surface_of(name),
            args_json,
            digest
        ],
    )?;
    Ok(())
}

/// The settled outcome of one call — everything a `tool_result` (or its
/// absence) decides about a row, as one value.
///
/// Both writers go through [`SettledOutcome::from_output`]: the live fold
/// ([`project_event`]) and the repair fold
/// ([`Store::materialize_tool_calls`]) must read a `ToolOutput` identically,
/// or a repair would rewrite history the live path had already recorded
/// differently.
struct SettledOutcome {
    state: ToolCallState,
    error: String,
    error_class: Option<ErrorClass>,
    bytes_out: i64,
    duration_ms: i64,
}

impl SettledOutcome {
    /// What one delivered `tool_result` payload settles to. Abandonment never
    /// comes through here — an abandoned call has no `ToolOutput` to read,
    /// which is exactly why it carries no class.
    fn from_output(output: &ToolOutput, duration_ms: i64) -> Self {
        let (state, error, error_class, bytes_out) = match output {
            ToolOutput::Ok { content, .. } => {
                (ToolCallState::Ok, String::new(), None, content.len() as i64)
            }
            ToolOutput::Error { message, class } => {
                let len = message.len() as i64;
                (ToolCallState::Error, message.clone(), *class, len)
            }
        };
        Self {
            state,
            error,
            error_class,
            bytes_out,
            duration_ms,
        }
    }
}

/// Fold one `tool_result` into the projection, inside the caller's
/// transaction.
///
/// A result carries no announcing `seq` — `call_id` is the only identity it
/// shares with its start — so it settles the **oldest still-`running`** row
/// bearing that id. Within one step that is the call it answers; across steps
/// every earlier namesake has already settled, so a later step's result cannot
/// reach back and overwrite one (#4033).
///
/// Two fallbacks, in order. A result for a call that has already settled
/// refreshes the newest row with that id — a re-delivered result must not mint
/// a phantom call. When no row bears the id at all — a result whose start was
/// never persisted, which a pre-v18 stream or a torn write can produce — it
/// inserts a complete row instead of dropping the result on the floor. A call
/// that demonstrably happened must appear in the count; attributing it with an
/// unknown name is a smaller lie than omitting it.
fn project_tool_result(
    tx: &Connection,
    execution_id: i64,
    call_id: &str,
    settled: &SettledOutcome,
) -> rusqlite::Result<()> {
    const SETTLE: &str = "UPDATE tool_calls \
         SET ok = ?3, state = ?4, error = ?5, error_class = ?6, bytes_out = ?7, \
             duration_ms = ?8 \
         WHERE execution_id = ?1 AND seq = ?2";
    let oldest_open: Option<i64> = tx.query_row(
        "SELECT min(seq) FROM tool_calls \
         WHERE execution_id = ?1 AND call_id = ?2 AND state = 'running'",
        params![execution_id, call_id],
        |row| row.get(0),
    )?;
    let seq = match oldest_open {
        Some(seq) => Some(seq),
        // Already settled: a re-delivered result refreshes the newest row
        // with this id rather than minting a phantom call beside it.
        None => tx.query_row(
            "SELECT max(seq) FROM tool_calls WHERE execution_id = ?1 AND call_id = ?2",
            params![execution_id, call_id],
            |row| row.get::<_, Option<i64>>(0),
        )?,
    };
    if let Some(seq) = seq {
        tx.execute(
            SETTLE,
            params![
                execution_id,
                seq,
                i64::from(settled.state.is_ok()),
                settled.state.as_str(),
                settled.error,
                class_token(settled.error_class),
                settled.bytes_out,
                settled.duration_ms
            ],
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
         (execution_id, seq, event_seq, call_id, name, surface, args_json, args_digest, reason, \
          ok, state, error, error_class, bytes_out, duration_ms) \
         VALUES (?1, ?2, ?3, ?4, '(unknown)', 'native', '{}', '', '', ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            execution_id,
            seq,
            UNKNOWN_EVENT_SEQ,
            call_id,
            i64::from(settled.state.is_ok()),
            settled.state.as_str(),
            settled.error,
            class_token(settled.error_class),
            settled.bytes_out,
            settled.duration_ms
        ],
    )?;
    Ok(())
}

/// Fold one event into the `tool_calls` projection, inside the caller's
/// transaction. Every non-tool event is a no-op.
///
/// Takes the already-appended event, and the `seq` it was appended at, so the
/// projection and the log it derives from commit together *and* agree on what
/// identifies a call — see this module's header for why that seq is the
/// identity and `call_id` cannot be.
pub(crate) fn project_event(
    tx: &Connection,
    execution_id: i64,
    event_seq: i64,
    event: &AgentEvent,
) -> rusqlite::Result<()> {
    match event {
        AgentEvent::ToolStart { call } => {
            let args_json = serde_json::to_string(&call.input).unwrap_or_else(|_| "{}".into());
            project_tool_start(
                tx,
                execution_id,
                event_seq,
                &call.call_id,
                &call.name,
                &args_json,
            )
        }
        AgentEvent::ToolResult {
            call_id,
            output,
            duration_ms,
            ..
        } => {
            let settled = SettledOutcome::from_output(output, *duration_ms as i64);
            project_tool_result(tx, execution_id, call_id, &settled)
        }
        _ => Ok(()),
    }
}

/// One call as the repair fold reconstructs it from the event stream, before
/// it becomes a row.
///
/// Carries the two facts a [`ToolCallRow`] has nowhere to put and the re-fold
/// must not invent: the `event_seq` that identifies the call, and the `ts` of
/// the event that announced it. Without that timestamp a re-fold re-dates
/// every call it rewrites to the moment of the re-fold — which for the v27 →
/// v28 migration would mean re-dating *every call in history* to the morning
/// the user upgraded, and every per-day rollup with them.
struct FoldedCall {
    event_seq: i64,
    ts: String,
    call_id: String,
    name: String,
    args_json: String,
    /// `None` until a `tool_result` settles it — and still `None` at the end
    /// for a call whose turn ended first, which is what makes it abandoned.
    settled: Option<SettledOutcome>,
}

/// Fold an execution's `(seq, ts, payload)` tool events into its calls, in
/// announcement order.
///
/// Pure, and deliberately the same rule as the live path
/// ([`project_tool_start`] / [`project_tool_result`]) rather than a second
/// reading of it: a start is identified by its own `seq`, and a result settles
/// the oldest call bearing its `call_id` that has not settled yet. The two
/// writers disagreeing is how a repair rewrites history the live path had
/// already recorded correctly.
fn fold_tool_stream(events: &[(i64, String, String)]) -> Vec<FoldedCall> {
    let mut calls: Vec<FoldedCall> = Vec::new();
    // call_id -> positions announced and not yet settled, oldest first.
    let mut open: std::collections::HashMap<String, VecDeque<usize>> =
        std::collections::HashMap::new();
    for (seq, ts, payload) in events {
        let Ok(event) = serde_json::from_str::<AgentEvent>(payload) else {
            continue;
        };
        match event {
            AgentEvent::ToolStart { call } => {
                let args_json = serde_json::to_string(&call.input).unwrap_or_else(|_| "{}".into());
                // The same announcement seen twice is one call: its `seq` is
                // the identity, so refresh in place rather than minting a
                // second row beside it.
                if let Some(existing) = calls.iter_mut().find(|c| c.event_seq == *seq) {
                    existing.name = call.name;
                    existing.args_json = args_json;
                    continue;
                }
                open.entry(call.call_id.clone())
                    .or_default()
                    .push_back(calls.len());
                calls.push(FoldedCall {
                    event_seq: *seq,
                    ts: ts.clone(),
                    call_id: call.call_id,
                    name: call.name,
                    args_json,
                    settled: None,
                });
            }
            AgentEvent::ToolResult {
                call_id,
                output,
                duration_ms,
                ..
            } => {
                let settled = SettledOutcome::from_output(&output, duration_ms as i64);
                if let Some(index) = open.get_mut(&call_id).and_then(VecDeque::pop_front) {
                    calls[index].settled = Some(settled);
                    continue;
                }
                // No open call with this id: either a re-delivered result for
                // one already settled, or a result whose start never reached
                // the log. Mirror the live fallbacks exactly.
                match calls.iter_mut().rev().find(|c| c.call_id == call_id) {
                    Some(existing) => existing.settled = Some(settled),
                    None => calls.push(FoldedCall {
                        event_seq: UNKNOWN_EVENT_SEQ,
                        ts: ts.clone(),
                        call_id,
                        name: "(unknown)".to_string(),
                        args_json: "{}".to_string(),
                        settled: Some(settled),
                    }),
                }
            }
            _ => {}
        }
    }
    calls
}

/// Re-fold one execution's projection from its `events` stream, inside the
/// caller's transaction, and return how many calls it holds.
///
/// Shared by the repair path ([`Store::materialize_tool_calls`]) and the
/// v27 → v28 migration, which re-folds every history the `call_id` key had
/// collapsed. Both need the identical fold, and a second copy of it is a
/// second answer to "how many tool calls were there".
///
/// Returns `0` without touching a row when the stream carries no tool events
/// at all: "nothing to fold from" is not "no calls happened", and an execution
/// whose events were pruned out from under its rows must keep them.
pub(crate) fn refold_tool_calls(tx: &Connection, execution_id: i64) -> rusqlite::Result<usize> {
    let announced = {
        let mut stmt = tx.prepare(
            "SELECT seq, ts, payload FROM events \
             WHERE execution_id = ?1 AND event_type IN ('tool_start', 'tool_result') \
             ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![execution_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        out
    };
    if announced.is_empty() {
        return Ok(0);
    }
    let calls = fold_tool_stream(&announced);
    for (seq, call) in calls.iter().enumerate() {
        let settled = call.settled.as_ref();
        tx.execute(
            "INSERT OR REPLACE INTO tool_calls \
             (execution_id, seq, event_seq, call_id, name, surface, args_json, args_digest, \
              reason, ok, state, error, error_class, bytes_out, duration_ms, ts) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, '', ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                execution_id,
                seq as i64,
                call.event_seq,
                call.call_id,
                call.name,
                surface_of(&call.name),
                call.args_json,
                fnv_hex(&call.args_json),
                i64::from(settled.is_some_and(|s| s.state.is_ok())),
                settled
                    .map_or(ToolCallState::Abandoned, |s| s.state)
                    .as_str(),
                settled.map_or(ABANDONED, |s| s.error.as_str()),
                class_token(settled.and_then(|s| s.error_class)),
                settled.map_or(0, |s| s.bytes_out),
                settled.map_or(0, |s| s.duration_ms),
                call.ts,
            ],
        )?;
    }
    // Any row past the fold's end is left over from a shorter earlier fold —
    // which is exactly what every history the `call_id` key collapsed looks
    // like. INSERT OR REPLACE overwrites positions 0..n; nothing but this
    // clears n.. .
    tx.execute(
        "DELETE FROM tool_calls WHERE execution_id = ?1 AND seq >= ?2",
        params![execution_id, calls.len() as i64],
    )?;
    Ok(calls.len())
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
                  reason, ok, state, error, error_class, bytes_out, duration_ms) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
                    class_token(row.error_class),
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
    /// (turn cut off mid-tool) is recorded as an abandoned, failed call so the
    /// count stays honest. Idempotent, and identical to what the live fold
    /// wrote — both go through `refold_tool_calls`'s rule, keyed on the
    /// announcing event's `seq`. Returns the count. (Named, not linked: that
    /// function is `pub(crate)`, and this doc is public.)
    pub fn materialize_tool_calls(&self, execution_id: i64) -> Result<usize> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let n = refold_tool_calls(&tx, execution_id)?;
        tx.commit()?;
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
            "UPDATE tool_calls SET state = 'abandoned', ok = 0, error = ?2 \
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
}

#[cfg(test)]
mod tests;
