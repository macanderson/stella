//! What changed between two model calls (#1511): `stella inspect --diff`,
//! served — a unified diff between one call's reconstructed context and an
//! earlier state of the same conversation.
//!
//! Printing the whole context (`sent_context`) answers "what did the model
//! see". It is the wrong tool for "what did *this* call see that the last one
//! didn't": a system prompt is hundreds of stable lines, and finding the
//! paragraph that moved by reading two of them side by side is not something
//! a person does reliably. The differ itself is `stella-diff` — the crate the
//! CLI's `inspect::diff` was extracted into so this route and the terminal
//! share one implementation instead of a fourth acknowledged copy.
//!
//! Baseline semantics mirror `crates/stella-cli/src/inspect.rs`
//! (`resolve_baseline`), because a reader who has scripted against one surface
//! must recognise the other:
//!
//! - `prev` / `first` search **same-role** calls over the **whole session**,
//!   oldest execution first — a system prompt is byte-stable inside one
//!   execution by design (the prompt-cache invariant), so the only place it
//!   can drift is across turns, and stopping at the execution boundary would
//!   report "no change" for exactly the comparison worth making.
//! - A role's first call has no predecessor, so `prev`/`first` resolve to
//!   `prompt` — the raw submitted bytes — and the payload reports the base it
//!   actually used, never just the one requested.
//!
//! Documents are rendered from **full** bodies: the transcript clip exists so
//! a drawer stays light, but a diff over clipped bodies would report the
//! elision point as a change, which is a lie about the prompt.

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::db::{DbError, is_missing_schema};
use crate::sent_context::{Preimages, journal_payloads, manifest_entries, reconstruct};

/// Context lines framing each hunk — the same default `stella inspect --diff`
/// and `git diff` use.
const DEFAULT_CONTEXT: usize = 3;

/// One recorded call's coordinates plus the role the baseline search matches
/// on. The execution id travels with the coordinates because a baseline can
/// live in an earlier turn, which is an earlier execution.
struct CallRef {
    execution_id: i64,
    turn: i64,
    step: i64,
    call_seq: i64,
    role: String,
}

impl CallRef {
    /// How this call is named in a `---`/`+++` label. The execution id is
    /// printed only when it differs from the call being inspected, so a
    /// same-turn comparison stays terse and a cross-turn one is unmistakable.
    fn label(&self, relative_to: i64) -> String {
        let prefix = if self.execution_id == relative_to {
            String::new()
        } else {
            format!("execution {} · ", self.execution_id)
        };
        format!(
            "{prefix}turn {} · step {} · seq {} ({})",
            self.turn, self.step, self.call_seq, self.role
        )
    }
}

/// The `/api/execution-context-diff` payload: the diff between the addressed
/// call's reconstruction and its resolved baseline, in the exact JSON shape
/// `stella inspect --diff --format json` emits (`base`, `base_label`,
/// `target_label`, `scope`, `changed`, `added`, `removed`, `minimal`,
/// `hunks[]`) plus `found`/`note` for the observatory's missing-is-a-state
/// posture.
pub(crate) fn payload(
    conn: &Connection,
    id: i64,
    turn: i64,
    step: i64,
    call_seq: i64,
    base: &str,
    only: &str,
) -> Result<Value, DbError> {
    let entries = manifest_entries(conn, id, turn, step, call_seq)?;
    if entries.is_empty() {
        return Ok(json!({
            "execution_id": id,
            "found": false,
            "note": format!(
                "no receipt for execution {id} turn {turn} step {step} call-seq {call_seq}"
            ),
        }));
    }
    let role = call_role_at(conn, id, turn, step, call_seq)?.unwrap_or_else(|| "worker".into());
    let target = CallRef {
        execution_id: id,
        turn,
        step,
        call_seq,
        role,
    };
    let preimages = Preimages::index(&journal_payloads(conn, id)?);
    let target_doc = render_document(&reconstruct(&entries, &preimages, true), only);

    let baseline = resolve_baseline(conn, &target, base, only)?;
    let diff = stella_diff::unified_diff(&baseline.document, &target_doc, DEFAULT_CONTEXT);
    Ok(json!({
        "execution_id": id,
        "found": true,
        "base": baseline.kind,
        "base_label": baseline.label,
        "target_label": target.label(id),
        "scope": scope_label(only),
        "changed": diff.changed(),
        "added": diff.added,
        "removed": diff.removed,
        "minimal": diff.minimal,
        "hunks": diff.hunks.iter().map(|h| json!({
            "old_start": h.old_start,
            "old_count": h.old_count,
            "new_start": h.new_start,
            "new_count": h.new_count,
            "lines": h.lines.iter().map(|l| json!({
                "op": l.op.tag(),
                "text": l.text,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    }))
}

/// The earlier state the diff is taken against, resolved to bytes plus the
/// label and kind the payload reports.
struct Baseline {
    label: String,
    kind: &'static str,
    document: String,
}

/// Mirror of the CLI's `resolve_baseline`: same-role, whole-session, oldest
/// first; `prev` takes the last earlier call, `first` the earliest; nothing
/// earlier resolves to the submitted prompt.
fn resolve_baseline(
    conn: &Connection,
    target: &CallRef,
    base: &str,
    only: &str,
) -> Result<Baseline, DbError> {
    if base == "prompt" {
        return prompt_baseline(conn, target.execution_id);
    }
    let mut turns = session_executions(conn, target.execution_id)?;
    if turns.is_empty() {
        turns.push(target.execution_id);
    }
    let mut earlier: Vec<CallRef> = Vec::new();
    for turn_execution in turns {
        // Later turns cannot be a baseline for an earlier one; ids are
        // allocated in submission order.
        if turn_execution > target.execution_id {
            break;
        }
        for call in recorded_calls_of(conn, turn_execution)? {
            if call.execution_id == target.execution_id
                && (call.turn, call.step, call.call_seq)
                    >= (target.turn, target.step, target.call_seq)
            {
                break;
            }
            if call.role == target.role {
                earlier.push(call);
            }
        }
    }
    let picked = if base == "first" {
        earlier.into_iter().next()
    } else {
        earlier.pop()
    };
    // Nothing of this role ran before: the only prior state that ever existed
    // is what the user typed, so that is the honest baseline rather than an
    // error or an empty diff.
    let Some(previous) = picked else {
        return prompt_baseline(conn, target.execution_id);
    };
    let entries = manifest_entries(
        conn,
        previous.execution_id,
        previous.turn,
        previous.step,
        previous.call_seq,
    )?;
    let preimages = Preimages::index(&journal_payloads(conn, previous.execution_id)?);
    Ok(Baseline {
        label: previous.label(target.execution_id),
        kind: if base == "first" { "first" } else { "prev" },
        document: render_document(&reconstruct(&entries, &preimages, true), only),
    })
}

/// The raw submitted prompt as a baseline — what the user actually typed,
/// before Stella assembled anything around it.
fn prompt_baseline(conn: &Connection, id: i64) -> Result<Baseline, DbError> {
    let prompt = match conn.query_row(
        "SELECT prompt FROM executions WHERE id = ?1",
        [id],
        |r| r.get::<_, String>(0),
    ) {
        Ok(prompt) => prompt,
        Err(rusqlite::Error::QueryReturnedNoRows) => String::new(),
        Err(e) if is_missing_schema(&e) => String::new(),
        Err(e) => return Err(e.into()),
    };
    Ok(Baseline {
        label: "prompt as submitted".to_string(),
        kind: "prompt",
        document: prompt,
    })
}

/// Every turn of the conversation containing `id`, oldest first — the same
/// walk `stella_store::Store::session_executions` performs, hand-written in
/// the observatory's established style. Empty when the execution has no
/// session (a one-shot `stella run` is its own session).
fn session_executions(conn: &Connection, id: i64) -> Result<Vec<i64>, DbError> {
    let sql = "SELECT id FROM executions
               WHERE session_id IS NOT NULL AND session_id <> ''
                 AND session_id = (SELECT session_id FROM executions WHERE id = ?1)
               ORDER BY id ASC";
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mapped = stmt.query_map([id], |r| r.get::<_, i64>(0))?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row?);
    }
    Ok(out)
}

/// One execution's recorded calls in wire order, as [`CallRef`]s.
fn recorded_calls_of(conn: &Connection, execution_id: i64) -> Result<Vec<CallRef>, DbError> {
    let sql = "SELECT turn_instance, step, call_seq, call_role
               FROM step_receipt WHERE execution_id = ?1
               ORDER BY turn_instance, step, call_seq";
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mapped = stmt.query_map([execution_id], |r| {
        Ok(CallRef {
            execution_id,
            turn: r.get(0)?,
            step: r.get(1)?,
            call_seq: r.get(2)?,
            role: r.get(3)?,
        })
    })?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row?);
    }
    Ok(out)
}

/// The addressed call's role from its receipt, `None` when the receipt row is
/// missing (the manifest can outlive a pre-receipt store's header row).
fn call_role_at(
    conn: &Connection,
    id: i64,
    turn: i64,
    step: i64,
    call_seq: i64,
) -> Result<Option<String>, DbError> {
    let sql = "SELECT call_role FROM step_receipt
               WHERE execution_id = ?1 AND turn_instance = ?2 AND step = ?3 AND call_seq = ?4";
    match conn.query_row(sql, [id, turn, step, call_seq], |r| r.get::<_, String>(0)) {
        Ok(role) => Ok(Some(role)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) if is_missing_schema(&e) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Flatten a reconstruction into the line-oriented document the diff
/// compares — the same shape as the CLI's `render_document`: messages headed
/// by role alone, never by index (an inserted message would renumber every
/// header after it, and a diff reporting forty changed lines because one
/// number shifted is worse than no diff at all).
fn render_document(recon: &Value, only: &str) -> String {
    let mut out = String::new();
    let Some(messages) = recon.get("messages").and_then(Value::as_array) else {
        return out;
    };
    for message in messages {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        if only != "all" && role != only {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("── {role} ──\n"));
        out.push_str(message.get("body").and_then(Value::as_str).unwrap_or(""));
        out.push('\n');
    }
    out
}

/// The scope wording `stella inspect --diff` reports for `--only`, mirrored so
/// scripted readers see one vocabulary.
fn scope_label(only: &str) -> String {
    if only == "all" {
        "all messages".to_string()
    } else {
        format!("{only} messages")
    }
}
