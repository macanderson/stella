//! The receipt fold: rebuilding the messages one past model call was **sent**,
//! from its persisted manifest plus the execution's event journal.
//!
//! `stella inspect <execution> --step N` answers this on the CLI through
//! `stella_store::Store::reconstruct_call`. This crate links no workspace crate
//! but `stella-home` (see `crate::db`'s module docs and the crate README), so
//! the fold is re-implemented here rather than imported: an **acknowledged
//! copy**, the same bargain `project_id_for` and the exploration re-hash already
//! take. Three things keep the copy honest:
//!
//! - `tests/schema_conformance.rs` drives this route against a database built
//!   by the real `Store::open` migration path and seeded through the real write
//!   API, so a renamed column fails at `cargo test`.
//! - That suite seeds the block digests by hashing `stella_protocol`'s own
//!   serialization of a `ToolCall`/`ToolOutput`, so a change to those wire
//!   shapes flips the verification verdict and fails there too — the one piece
//!   of this module that is a *byte-level* coupling rather than a column-level
//!   one (see `tool_call_preimage`).
//! - Everything the fold cannot vouch for is reported, never smoothed over:
//!   `unresolved` and `digest_mismatches` are separate lists here for the same
//!   reason they are separate fields on `stella_store::Reconstruction`.
//!
//! # What "verified" means
//!
//! Every block whose preimage came from the journal is re-hashed and compared
//! against the digest the receipt recorded at emission, so a torn journal or a
//! fabricated block surfaces as a mismatch instead of a plausible-looking lie.
//! The two gap kinds — the system prefix and the assembled user/recall message
//! — are stored as local bytes in `context_blocks.content`, so their check is
//! tautological and is deliberately **not** counted as evidence: they report
//! `digest_verified: null`, not `true`.
//!
//! # Reconstructable boundary
//!
//! Byte-exact reconstruction holds for the ordinary turn: system prompt, user
//! goal, assistant text, and real tool round-trips. It does not cover blocks
//! with no journal preimage — budget-abort synthetic tool results, discarded
//! speculation, attachments — which surface in `unresolved` rather than
//! silently corrupting the output.

use std::collections::HashMap;

use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::db::{DbError, is_missing_schema};

/// The whole `/api/execution-context` payload for one execution: the index of
/// its recorded model calls, plus — when `step` addresses one — that call's
/// reconstruction.
///
/// The index is always present because it is what the drawer drills from: a
/// step row can only offer "context sent" if a receipt exists at that step.
/// `turn` and `call_seq` default to 0 at the router, matching `stella inspect`,
/// where `--turn`/`--call-seq` are the worker call unless asked otherwise.
pub(crate) fn payload(
    conn: &Connection,
    id: i64,
    turn: i64,
    step: Option<i64>,
    call_seq: i64,
    full: bool,
) -> Result<Value, DbError> {
    let calls = recorded_calls(conn, id)?;
    if calls.is_empty() {
        return Ok(no_receipts(id));
    }
    let mut out = json!({
        "execution_id": id,
        "receipts_recorded": true,
        "calls": Value::Array(calls),
        "context": Value::Null,
    });
    if let Some(step) = step {
        out["context"] = call_context(conn, id, turn, step, call_seq, full)?;
    }
    Ok(out)
}

/// The explicit "no receipts recorded" answer — a state, never an error.
///
/// Reached three ways, all of them ordinary: the workspace has no `store.db`
/// yet, the file predates the receipts plane so `step_receipt` does not exist,
/// or the execution ran with `context.lifecycle` off and wrote none. The
/// wording mirrors what `stella inspect` prints for the same case, because a
/// reader who has seen one should recognise the other.
pub(crate) fn no_receipts(id: i64) -> Value {
    json!({
        "execution_id": id,
        "receipts_recorded": false,
        "calls": [],
        "context": Value::Null,
        "note": format!(
            "Execution {id} has no recorded receipts. Receipts are written by builds \
             carrying the context-receipts plane; an execution from before it landed, \
             or one recorded with context.lifecycle off, has none."
        ),
    })
}

/// Every model call this execution recorded a receipt for, in wire order — one
/// row per call, not per step: a step that ran the worker plus an overflow
/// summarizer appears twice, keyed apart by `call_seq`.
fn recorded_calls(conn: &Connection, id: i64) -> Result<Vec<Value>, DbError> {
    let sql = "SELECT turn_instance, step, call_seq, call_role, provider, model,
                      estimated_input_tokens, effective_budget_tokens,
                      compiled_frame_id, frame_hash
               FROM step_receipt WHERE execution_id = ?1
               ORDER BY turn_instance, step, call_seq";
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mapped = stmt.query_map([id], |r| {
        Ok(json!({
            "turn": r.get::<_, i64>(0)?,
            "step": r.get::<_, i64>(1)?,
            "call_seq": r.get::<_, i64>(2)?,
            "call_role": r.get::<_, String>(3)?,
            "provider": r.get::<_, String>(4)?,
            "model": r.get::<_, String>(5)?,
            "estimated_input_tokens": r.get::<_, i64>(6)?,
            "effective_budget_tokens": r.get::<_, i64>(7)?,
            // Absent for every receipt written while the context lifecycle was
            // off. Null reads as "not computed", never as "computed and empty".
            "compiled_frame_id": r.get::<_, Option<String>>(8)?,
            "frame_hash": r.get::<_, Option<String>>(9)?,
        }))
    })?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row?);
    }
    Ok(out)
}

/// One addressed call's reconstruction, or an honest `found: false` when
/// nothing was recorded at those coordinates.
fn call_context(
    conn: &Connection,
    id: i64,
    turn: i64,
    step: i64,
    call_seq: i64,
    full: bool,
) -> Result<Value, DbError> {
    let entries = manifest_entries(conn, id, turn, step, call_seq)?;
    let mut out = json!({
        "turn": turn,
        "step": step,
        "call_seq": call_seq,
        "found": !entries.is_empty(),
    });
    if entries.is_empty() {
        out["note"] = json!(format!(
            "no receipt for execution {id} turn {turn} step {step} call-seq {call_seq}"
        ));
        return Ok(out);
    }
    let preimages = Preimages::index(&journal_payloads(conn, id)?);
    let era = journal_era(conn, id);
    crate::db::merge(&mut out, reconstruct(&entries, &preimages, era, full));
    Ok(out)
}

/// One call's manifest in wire order, each entry joined to its block registry
/// row.
///
/// A `LEFT JOIN`, deliberately: a manifest citing a block that was never
/// registered must come back as an entry with no `kind` — an unresolved block
/// the reader is told about — rather than vanish from the wire order.
pub(crate) fn manifest_entries(
    conn: &Connection,
    id: i64,
    turn: i64,
    step: i64,
    call_seq: i64,
) -> Result<Vec<ManifestEntry>, DbError> {
    let sql = "SELECT sm.block_id, sm.cache_zone, sm.resident_since_step, sm.message_index,
                      sm.call_id, cb.kind, cb.token_cost, cb.content_digest, cb.content,
                      cb.call_id
               FROM step_manifest sm
               LEFT JOIN context_blocks cb
                 ON cb.execution_id = sm.execution_id AND cb.block_id = sm.block_id
               WHERE sm.execution_id = ?1 AND sm.turn_instance = ?2 AND sm.step = ?3
                 AND sm.call_seq = ?4
               ORDER BY sm.ordinal";
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mapped = stmt.query_map([id, turn, step, call_seq], |r| {
        Ok(ManifestEntry {
            block_id: r.get(0)?,
            cache_zone: r.get(1)?,
            resident_since_step: r.get(2)?,
            message_index: r.get(3)?,
            occurrence_call_id: r.get(4)?,
            kind: r.get(5)?,
            token_cost: r.get(6)?,
            content_digest: r.get(7)?,
            content: r.get(8)?,
            birth_call_id: r.get(9)?,
        })
    })?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row?);
    }
    Ok(out)
}

/// The execution's slice of the journal that block preimages resolve from.
///
/// The third sanctioned read of `events`, on the same bargain as the other
/// two: `execution_id`-first, which the store's `UNIQUE (execution_id, seq)`
/// index serves directly, and only on a per-execution detail route.
pub(crate) fn journal_payloads(conn: &Connection, id: i64) -> Result<Vec<String>, DbError> {
    let sql = "SELECT payload FROM events
               WHERE execution_id = ?1
                 AND event_type IN ('tool_start', 'tool_result', 'text', 'compaction')
               ORDER BY seq ASC";
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mapped = stmt.query_map([id], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row?);
    }
    Ok(out)
}

/// One row of a call's manifest, already joined to its block registry entry.
///
/// The `Option` fields are all "the join found nothing / the column is NULL",
/// never a default: a manifest citing a block that was never registered has no
/// `kind`, and that is what makes it unresolvable rather than empty.
pub(crate) struct ManifestEntry {
    /// Content-addressed block id (`blk_…`) — the identity the manifest cites.
    pub(crate) block_id: String,
    /// Which cache zone this occurrence sat in (`stable_prefix`, `volatile`, …).
    pub(crate) cache_zone: String,
    /// The step this block has been resident since — how long it has been paid
    /// for.
    pub(crate) resident_since_step: i64,
    /// The sent-message this block belonged to; blocks sharing an index regroup
    /// into one message.
    pub(crate) message_index: i64,
    /// This occurrence's tool-call attribution (`step_manifest.call_id`, v15).
    /// Wins over `birth_call_id` for a rebuilt tool result: block ids are
    /// content-addressed, so two calls with byte-identical results share one
    /// registry row and its `call_id` names only the call that minted it first.
    pub(crate) occurrence_call_id: Option<String>,
    /// The block's kind (`system_prefix`, `tool_result`, …); `None` when the
    /// manifest cited a block with no registry row.
    pub(crate) kind: Option<String>,
    /// The block's cost under the engine's token rule. `None` means **not
    /// derivable**, not free.
    pub(crate) token_cost: Option<i64>,
    /// `sha256:<64 hex>` over the block's preimage, recorded at emission.
    pub(crate) content_digest: Option<String>,
    /// Local-only preimage, present for the gap kinds the journal cannot
    /// resolve (the system prefix and the assembled user/recall message).
    pub(crate) content: Option<String>,
    /// The block's birth provenance (`context_blocks.call_id`).
    pub(crate) birth_call_id: Option<String>,
}

/// Per-execution preimage index built once from the event journal: tool calls
/// and outputs by `call_id`, assistant text by content digest.
///
/// The values are the exact preimage **strings** the digests were taken over,
/// not parsed structures, because verification is the point: a shape that
/// round-trips through a different serializer is a different preimage.
#[derive(Default)]
pub(crate) struct Preimages {
    tool_calls: HashMap<String, String>,
    tool_outputs: HashMap<String, String>,
    /// `content_digest` (`sha256:<hex>`) → the assistant text bytes.
    text_by_digest: HashMap<String, String>,
    /// `content_digest` (`sha256:<hex>`) → the serialized post-rewrite tool
    /// output a compaction pass journaled (#1667). Consulted before the
    /// `call_id` route, which can only reach the pre-compaction bytes.
    rewrites_by_digest: HashMap<String, String>,
}

impl Preimages {
    /// Index one execution's `tool_start` / `tool_result` / `text` payloads.
    ///
    /// A payload that no longer parses as JSON is skipped: one unreadable event
    /// costs the blocks that resolve through it, which then report as
    /// unresolved, and must not cost the rest of the reconstruction.
    pub(crate) fn index(payloads: &[String]) -> Self {
        let mut out = Self::default();
        for payload in payloads {
            let Ok(event) = serde_json::from_str::<Value>(payload) else {
                continue;
            };
            match event["type"].as_str().unwrap_or_default() {
                "tool_start" => {
                    let call = &event["call"];
                    if let Some(call_id) = call["call_id"].as_str() {
                        out.tool_calls
                            .insert(call_id.to_owned(), tool_call_preimage(call));
                    }
                }
                "tool_result" => {
                    if let Some(call_id) = event["call_id"].as_str() {
                        out.tool_outputs
                            .insert(call_id.to_owned(), compact(&event["output"]));
                    }
                }
                "text" => {
                    // Spelled `delta` in journals written before #1886.
                    let body = event["text"].as_str().or_else(|| event["delta"].as_str());
                    if let Some(text) = body {
                        out.text_by_digest
                            .insert(format!("sha256:{}", sha256_hex(text)), text.to_owned());
                    }
                }
                "compaction" => {
                    for rewrite in event["rewrites"].as_array().into_iter().flatten() {
                        if let (Some(digest), Some(content)) = (
                            rewrite["content_digest"].as_str(),
                            rewrite["content"].as_str(),
                        ) {
                            out.rewrites_by_digest
                                .insert(digest.to_owned(), content.to_owned());
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// The exact bytes a `tool_call` block's digest was taken over:
/// `serde_json::to_string` of `stella_protocol::ToolCall`, whose fields
/// serialize in **declaration** order (`call_id`, `name`, `input`).
///
/// Built by hand rather than by re-serializing the payload's own object,
/// because `serde_json::Value` keeps its map sorted — re-serializing would
/// emit `call_id`, `input`, `name` and every digest would mismatch. The nested
/// `input` is a `Value` on both sides, so it is safe (and necessary) to
/// serialize it as one.
///
/// This is the module's only byte-level coupling to another crate's wire shape.
/// `schema_conformance.rs` seeds its digests from `stella_protocol`'s own
/// serialization, so a reordered field fails there rather than in a user's
/// dashboard.
fn tool_call_preimage(call: &Value) -> String {
    format!(
        "{{\"call_id\":{},\"name\":{},\"input\":{}}}",
        compact(&call["call_id"]),
        compact(&call["name"]),
        compact(&call["input"])
    )
}

/// One JSON value as compact bytes; an unserializable value reads as `null`
/// rather than failing the whole reconstruction.
fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned())
}

/// `sha256` hex of a string. The sha2 0.11 output does not `LowerHex`, so the
/// bytes are formatted one at a time — the same shape `fsview` uses.
fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Which compaction-journaling era wrote an execution's events — an
/// acknowledged mirror of `stella_store::JournalEra`, on the same bargain as
/// the rest of this module (this crate links no store; see the module docs).
///
/// The point of the stamp is that it is *recorded*, not inferred: a compacted
/// block on a pre-#1667 journal mismatches as a matter of course, and styling
/// that as tampering is a false alarm on ordinary housekeeping. The dashboard
/// used to do exactly that for every era at once (#1981).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum JournalEra {
    /// Compaction rewrote tool results in place and journaled nothing about
    /// it. The default, and what a store too old to carry the column reads as:
    /// nothing known must never be rendered as an integrity failure.
    #[default]
    CompactionUnjournaled,
    /// Every compaction rewrite is journaled (#1667), so a mismatch has no
    /// routine explanation left.
    CompactionJournaled,
}

impl JournalEra {
    /// The wire spelling, shared by convention with `stella inspect --format
    /// json`'s `journal_era` — the two are pinned by a test on each side
    /// rather than by linkage.
    fn tag(self) -> &'static str {
        match self {
            Self::CompactionUnjournaled => "compaction_unjournaled",
            Self::CompactionJournaled => "compaction_journaled",
        }
    }
}

/// The era stamped on an execution row (`executions.journal_era`, schema v22).
///
/// Every failure reads as [`JournalEra::CompactionUnjournaled`], and they are
/// all ordinary: a store written before v22 has no such column, and an
/// execution the dashboard was pointed at may not exist. Neither is a state in
/// which this crate may claim an integrity failure on a user's telemetry, so
/// the unknown answer is the quiet one.
pub(crate) fn journal_era(conn: &Connection, id: i64) -> JournalEra {
    let code: Result<i64, _> = conn.query_row(
        "SELECT journal_era FROM executions WHERE id = ?1",
        [id],
        |r| r.get(0),
    );
    match code {
        Ok(1) => JournalEra::CompactionJournaled,
        _ => JournalEra::CompactionUnjournaled,
    }
}

/// One message under construction — the regrouping target for every block
/// sharing a `message_index`.
struct Message {
    role: &'static str,
    body: String,
    blocks: Vec<Value>,
}

/// Fold one call's manifest into the messages it was sent, with the honest
/// accounting of what could not be vouched for.
///
/// `full` lifts the per-body clip, exactly as it does on the transcript route:
/// both go through `set_journal_body`, so the two views can never disagree
/// about what "clipped" means.
pub(crate) fn reconstruct(
    entries: &[ManifestEntry],
    preimages: &Preimages,
    era: JournalEra,
    full: bool,
) -> Value {
    let mut messages: Vec<Message> = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();
    let mut mismatches: Vec<String> = Vec::new();
    let mut current: Option<i64> = None;

    for entry in entries {
        let Some(kind) = entry.kind.as_deref() else {
            // A manifest citing a block that was never registered: there is
            // nothing to resolve, and inventing an empty message for it would
            // be a claim about what the model saw.
            unresolved.push(entry.block_id.clone());
            continue;
        };
        let Some(content) = resolve_content(entry, kind, preimages) else {
            unresolved.push(entry.block_id.clone());
            continue;
        };
        // Local gap content is stored bytes, so re-hashing it proves nothing;
        // `None` says "not evidence" where `true` would claim it was.
        let digest_verified = match &entry.content {
            Some(_) => None,
            None => Some(digest_matches(entry.content_digest.as_deref(), &content)),
        };
        if digest_verified == Some(false) {
            mismatches.push(entry.block_id.clone());
        }
        if current != Some(entry.message_index) {
            messages.push(Message {
                role: role_for(kind),
                body: String::new(),
                blocks: Vec::new(),
            });
            current = Some(entry.message_index);
        }
        let Some(message) = messages.last_mut() else {
            continue;
        };
        if !append_block(message, entry, kind, &content) {
            unresolved.push(entry.block_id.clone());
        }
        message.blocks.push(json!({
            "block_id": entry.block_id,
            "kind": kind,
            "cache_zone": entry.cache_zone,
            "token_cost": entry.token_cost,
            "resident_since_step": entry.resident_since_step,
            "digest_verified": digest_verified,
        }));
    }

    let rendered: Vec<Value> = messages
        .into_iter()
        .enumerate()
        .map(|(index, message)| {
            let mut out = json!({
                "index": index,
                "role": message.role,
                "blocks": Value::Array(message.blocks),
            });
            crate::db::set_journal_body(&mut out, &message.body, full);
            out
        })
        .collect();

    // `verified` stays era-blind: a mismatch happened or it did not. The era
    // decides only how loudly it is reported, which is what
    // `digest_mismatch_severity` carries — the same three words `stella
    // inspect --format json` emits, so the dashboard and the CLI cannot come
    // to different conclusions about one execution (#1981).
    let severity = if mismatches.is_empty() {
        "none"
    } else if era == JournalEra::CompactionJournaled {
        "integrity"
    } else {
        "compaction"
    };
    json!({
        "verified": unresolved.is_empty() && mismatches.is_empty(),
        "unresolved": unresolved,
        "digest_mismatches": mismatches,
        "journal_era": era.tag(),
        "digest_mismatch_severity": severity,
        "messages": rendered,
    })
}

/// Resolve one block's exact preimage: gap kinds carry it locally, every other
/// kind is recovered from the journal index. `None` is a documented
/// non-reconstructable case, never a silent empty body.
fn resolve_content(entry: &ManifestEntry, kind: &str, preimages: &Preimages) -> Option<String> {
    if let Some(content) = &entry.content {
        return Some(content.clone());
    }
    match kind {
        // Digest-keyed first (#1667): a compacted block's journaled
        // replacement is the exact preimage; the `call_id` route below
        // reaches only the pre-compaction bytes.
        "tool_result" => entry
            .content_digest
            .as_deref()
            .and_then(|digest| preimages.rewrites_by_digest.get(digest))
            .or_else(|| preimages.tool_outputs.get(entry.birth_call_id.as_deref()?))
            .cloned(),
        "tool_call" => preimages
            .tool_calls
            .get(entry.birth_call_id.as_deref()?)
            .cloned(),
        "assistant_text" => preimages
            .text_by_digest
            .get(entry.content_digest.as_deref()?)
            .cloned(),
        _ => None,
    }
}

/// Whether resolved bytes really are this block's bytes.
///
/// Not a formality: a tool block's preimage is found by `call_id`, and one call
/// can leave several content-addressed blocks behind (compaction stubs and ages
/// results in place). They all resolve to the same journal event, and only the
/// digest says which of them those bytes actually are.
fn digest_matches(recorded: Option<&str>, content: &str) -> bool {
    let Some(recorded) = recorded else {
        return false;
    };
    let expected = recorded.strip_prefix("sha256:").unwrap_or(recorded);
    sha256_hex(content) == expected
}

/// The role a block kind opens a message with — the mapping
/// `stella_store::reconstruct` uses. `summary` is a **user** message: the
/// overflow summarizer splices it as one, so rebuilding it as assistant-
/// authored would show a transcript the model never saw.
fn role_for(kind: &str) -> &'static str {
    match kind {
        "system_prefix" => "system",
        "user_goal" | "steered" | "summary" | "recalled_frame" | "attachment" => "user",
        "tool_result" => "tool",
        // assistant_text, tool_call and anything else open an assistant message.
        _ => "assistant",
    }
}

/// Append one resolved block to the message it belongs to, rendered the way
/// `stella inspect` renders it (`crates/stella-cli/src/inspect.rs`'s
/// `message_body`): text kinds concatenate, tool traffic gets its arrow line.
///
/// Returns `false` when the bytes resolved but did not parse into the shape the
/// kind promises — an unresolved block, not a fabricated one.
fn append_block(message: &mut Message, entry: &ManifestEntry, kind: &str, content: &str) -> bool {
    match kind {
        // Append, not assign: one message can decompose into several text
        // blocks (the recall block splits per item), cut so that concatenating
        // them in manifest order reproduces the original bytes exactly.
        "system_prefix" | "user_goal" | "steered" | "assistant_text" | "summary"
        | "recalled_frame" => {
            message.body.push_str(content);
            true
        }
        "tool_call" => {
            let Ok(call) = serde_json::from_str::<Value>(content) else {
                return false;
            };
            separate(&mut message.body);
            message.body.push_str(&format!(
                "→ tool_call {} {} {}",
                call["call_id"].as_str().unwrap_or_default(),
                call["name"].as_str().unwrap_or_default(),
                compact(&call["input"])
            ));
            true
        }
        "tool_result" => {
            let Ok(output) = serde_json::from_str::<Value>(content) else {
                return false;
            };
            separate(&mut message.body);
            // `ToolOutput` is externally tagged: `{"ok":{"content":…}}` or
            // `{"error":{"message":…}}` — the tag is the verdict.
            // The occurrence's own id wins; birth provenance is the best a
            // pre-v15 row can know, and an absent one renders as empty rather
            // than as some other call's id.
            let call_id = match entry.occurrence_call_id.as_deref() {
                Some(id) => id,
                None => entry.birth_call_id.as_deref().unwrap_or_default(),
            };
            message.body.push_str(&format!("← tool_result {call_id}\n"));
            match output["ok"]["content"].as_str() {
                Some(ok) => message.body.push_str(ok),
                None => message.body.push_str(&format!(
                    "error: {}",
                    output["error"]["message"].as_str().unwrap_or_default()
                )),
            }
            true
        }
        // An attachment is carried by its message rather than printed into it,
        // and is only ever resolvable when the receipt stored its bytes.
        "attachment" => serde_json::from_str::<Value>(content).is_ok(),
        _ => false,
    }
}

/// Put a newline between two rendered segments of one message body, matching
/// how `stella inspect` joins them.
fn separate(body: &mut String) {
    if !body.is_empty() {
        body.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(block_id: &str, kind: &str, message_index: i64) -> ManifestEntry {
        ManifestEntry {
            block_id: block_id.to_owned(),
            cache_zone: "cacheable".to_owned(),
            resident_since_step: 0,
            message_index,
            occurrence_call_id: None,
            kind: Some(kind.to_owned()),
            token_cost: Some(10),
            content_digest: None,
            content: None,
            birth_call_id: None,
        }
    }

    fn gap(block_id: &str, kind: &str, message_index: i64, content: &str) -> ManifestEntry {
        ManifestEntry {
            content_digest: Some(format!("sha256:{}", sha256_hex(content))),
            content: Some(content.to_owned()),
            ..entry(block_id, kind, message_index)
        }
    }

    #[test]
    fn text_preimages_index_under_both_wire_spellings() {
        // `text` spelled its body `delta` before #1886; a journal can hold
        // either generation and both must resolve as digest preimages.
        let payloads = vec![
            r#"{"type":"text","delta":"old body"}"#.to_owned(),
            r#"{"type":"text","text":"new body"}"#.to_owned(),
        ];
        let preimages = Preimages::index(&payloads);
        for body in ["old body", "new body"] {
            assert_eq!(
                preimages
                    .text_by_digest
                    .get(&format!("sha256:{}", sha256_hex(body)))
                    .map(String::as_str),
                Some(body),
            );
        }
    }

    #[test]
    fn a_tool_round_trip_rebuilds_verified_from_the_journal() {
        let payloads = vec![
            r#"{"type":"tool_start","call":{"call_id":"c1","name":"read_file","input":{"path":"a.rs"}}}"#.to_owned(),
            r#"{"type":"tool_result","call_id":"c1","output":{"ok":{"content":"fn a() {}"}},"duration_ms":5,"speculated":false}"#.to_owned(),
        ];
        let preimages = Preimages::index(&payloads);
        let call_preimage = preimages.tool_calls.get("c1").cloned().unwrap();
        let output_preimage = preimages.tool_outputs.get("c1").cloned().unwrap();
        let entries = vec![
            gap("blk_sys", "system_prefix", 0, "you are careful"),
            ManifestEntry {
                content_digest: Some(format!("sha256:{}", sha256_hex(&call_preimage))),
                birth_call_id: Some("c1".to_owned()),
                ..entry("blk_call", "tool_call", 1)
            },
            ManifestEntry {
                content_digest: Some(format!("sha256:{}", sha256_hex(&output_preimage))),
                birth_call_id: Some("c1".to_owned()),
                ..entry("blk_res", "tool_result", 2)
            },
        ];

        let out = reconstruct(&entries, &preimages, JournalEra::CompactionJournaled, false);
        assert_eq!(out["verified"], true, "{out}");
        assert_eq!(out["messages"][0]["role"], "system");
        assert_eq!(out["messages"][0]["body"], "you are careful");
        // A locally-stored gap block re-hashes to itself, so it is not evidence.
        assert!(out["messages"][0]["blocks"][0]["digest_verified"].is_null());
        assert_eq!(out["messages"][1]["role"], "assistant");
        assert!(
            out["messages"][1]["body"]
                .as_str()
                .unwrap()
                .starts_with("→ tool_call c1 read_file "),
            "{out}"
        );
        assert_eq!(out["messages"][1]["blocks"][0]["digest_verified"], true);
        assert_eq!(out["messages"][2]["role"], "tool");
        assert_eq!(out["messages"][2]["body"], "← tool_result c1\nfn a() {}");
    }

    #[test]
    fn a_block_with_no_preimage_is_unresolved_rather_than_invented() {
        let preimages = Preimages::index(&[]);
        let entries = vec![ManifestEntry {
            birth_call_id: Some("missing".to_owned()),
            ..entry("blk_orphan", "tool_result", 0)
        }];
        let out = reconstruct(&entries, &preimages, JournalEra::CompactionJournaled, false);
        assert_eq!(out["verified"], false);
        assert_eq!(out["unresolved"], json!(["blk_orphan"]));
        assert_eq!(out["messages"], json!([]));
    }

    #[test]
    fn resolved_bytes_that_do_not_re_hash_are_a_mismatch_not_a_silent_swap() {
        // The failure this check exists for: a journal event resolves, but its
        // bytes are not the ones the receipt recorded a digest over.
        let payloads =
            vec![r#"{"type":"text","delta":"what the journal actually holds"}"#.to_owned()];
        let preimages = Preimages::index(&payloads);
        let entries = vec![ManifestEntry {
            content_digest: Some(format!("sha256:{}", sha256_hex("what was recorded"))),
            ..entry("blk_text", "assistant_text", 0)
        }];
        // Nothing resolves at all when the digest is the lookup key…
        let out = reconstruct(&entries, &preimages, JournalEra::CompactionJournaled, false);
        assert_eq!(out["unresolved"], json!(["blk_text"]));

        // …so stage the mismatch through a kind that resolves by call id.
        let payloads = vec![
            r#"{"type":"tool_result","call_id":"c1","output":{"ok":{"content":"other bytes"}}}"#
                .to_owned(),
        ];
        let preimages = Preimages::index(&payloads);
        let entries = vec![ManifestEntry {
            content_digest: Some(format!("sha256:{}", sha256_hex("not what is there"))),
            birth_call_id: Some("c1".to_owned()),
            ..entry("blk_res", "tool_result", 0)
        }];
        let out = reconstruct(&entries, &preimages, JournalEra::CompactionJournaled, false);
        assert_eq!(out["verified"], false);
        assert_eq!(out["digest_mismatches"], json!(["blk_res"]));
        assert_eq!(out["messages"][0]["blocks"][0]["digest_verified"], false);
    }

    #[test]
    fn a_tool_call_preimage_keeps_declaration_order_not_sorted_keys() {
        // Re-serializing the payload's own object would sort the keys to
        // call_id, input, name and every tool_call digest would mismatch.
        let call = serde_json::json!({
            "call_id": "c1", "name": "read_file", "input": {"path": "a.rs"}
        });
        assert_eq!(
            tool_call_preimage(&call),
            r#"{"call_id":"c1","name":"read_file","input":{"path":"a.rs"}}"#
        );
    }

    #[test]
    fn an_error_result_renders_as_an_error_not_an_empty_body() {
        let payloads = vec![
            r#"{"type":"tool_result","call_id":"e1","output":{"error":{"message":"file not found"}}}"#
                .to_owned(),
        ];
        let preimages = Preimages::index(&payloads);
        let entries = vec![ManifestEntry {
            content_digest: Some(format!(
                "sha256:{}",
                sha256_hex(r#"{"error":{"message":"file not found"}}"#)
            )),
            birth_call_id: Some("e1".to_owned()),
            ..entry("blk_err", "tool_result", 0)
        }];
        let out = reconstruct(&entries, &preimages, JournalEra::CompactionJournaled, false);
        assert_eq!(out["verified"], true, "{out}");
        assert_eq!(
            out["messages"][0]["body"],
            "← tool_result e1\nerror: file not found"
        );
    }
}
