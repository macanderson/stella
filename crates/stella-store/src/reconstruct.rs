//! Byte-exact step reconstruction (spec §5.1, increment 2). Given a persisted
//! receipt — the per-step manifest + the block registry — rebuild the exact
//! `Vec<CompletionMessage>` the model saw on that step, resolving each block's
//! preimage from the event journal (tool I/O by `call_id`, assistant text by
//! digest) and, only for the two kinds the fold cannot carry (the system prefix
//! and the assembled user/recall message), from the block's local-only content.
//!
//! This is the payoff of the whole receipts design: proof, after the fact, of
//! exactly what a model saw — reconstructed from the append-only fold, not from
//! any live engine state. What makes it *verifiable* rather than merely stored:
//! every journal-resolved block's recovered bytes are re-hashed and checked
//! against the digest the receipt recorded, so bytes that are not this block's
//! surface as a mismatch instead of a plausible-looking lie.
//!
//! A mismatch is a statement about *these bytes*, not about anyone's motives.
//! The common cause is mundane: compaction rewrites tool results in place, and
//! on a journal written before rewrites were journaled (#1667) the only
//! preimage under that `call_id` is the pre-compaction one, so replay recovers
//! a real output that is simply not the one this step sent. Journals written
//! since carry each replacement on the `Compaction` event, and a compacted
//! block resolves by digest to the exact bytes — so on a current journal a
//! mismatch is back to meaning something is genuinely unaccounted for.
//! Reporting the legacy case as tampering would be both wrong and
//! self-defeating — an alarm that fires on routine housekeeping is an alarm
//! nobody reads.
//!
//! Which of those two a mismatch is therefore depends on *who wrote the
//! journal*, and that is recorded rather than inferred: [`JournalEra`] is
//! stamped on the execution row when the run begins, and
//! [`Reconstruction::mismatch_severity`] is the one place the two eras are
//! told apart. Every surface that renders a mismatch — `stella inspect`,
//! `stella trace`, the deck's INSPECT overlay, the observatory — reads that
//! verdict rather than deciding for itself, because #1668 had to correct all
//! of them at once and a surface that reasons on its own is the one that will
//! be missed next time.
//!
//! # Reconstructable boundary (clean path only)
//!
//! Byte-exact reconstruction holds for the ordinary turn: system prompt, user
//! goal, assistant text, and real tool round-trips. It does NOT cover blocks
//! with no journal preimage — budget-abort synthetic tool results and
//! discarded-speculation results (spec §6.4, deferred) — nor `Attachment`
//! blocks (not decomposed yet). Those surface in [`Reconstruction::unresolved`]
//! rather than silently corrupting the output.

use std::collections::HashMap;
use std::fmt::Write as _;

use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
use stella_protocol::{
    AgentEvent, CompletionMessage, MessageRole, ToolCall, ToolOutput, ToolResult,
};

use crate::{Result, Store};

/// Which compaction-journaling era wrote an execution's events — the signal
/// that decides whether a digest mismatch is routine housekeeping or a real
/// integrity failure.
///
/// This is **stamped, not inferred**. The tempting cheap test — "did any
/// `Compaction` event in this execution carry rewrites?" — reads the absence
/// of a record as a statement about the writer, and it is wrong in a case that
/// really happens: an overflow-summary splice
/// (`stella_core::driver::apply_overflow_summary`) compacts and legitimately
/// journals no rewrites at all, so a current-era execution would be read as
/// legacy and a genuine integrity signal would be styled as housekeeping. The
/// column costs a migration; guessing costs the alarm.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum JournalEra {
    /// Compaction rewrote tool results in place and told the journal nothing
    /// about it (every journal written before #1667/PR #1979). A compacted
    /// block can only resolve through the `call_id` fallback, which reaches
    /// the pre-compaction bytes — so it mismatches, benignly and forever.
    ///
    /// The default, and what every row written before schema v22 backfills to:
    /// an era this build does not recognise reads as this one, so an
    /// unfamiliar stamp can only under-alarm.
    #[default]
    CompactionUnjournaled,
    /// Every compaction pass journals its replacement bytes on the
    /// `Compaction` event (#1667), so a compacted block resolves by digest to
    /// exactly what the step sent. A mismatch here has no housekeeping
    /// explanation left.
    CompactionJournaled,
}

impl JournalEra {
    /// The era this build writes. Stamped by
    /// [`Store::begin_execution`](crate::Store::begin_execution) onto every
    /// execution it opens — a statement about the *writer's* capability, which
    /// is the one thing no later reader can recover from the events alone.
    pub const CURRENT: Self = Self::CompactionJournaled;

    /// The `executions.journal_era` code for this era.
    pub fn code(self) -> i64 {
        match self {
            Self::CompactionUnjournaled => 0,
            Self::CompactionJournaled => 1,
        }
    }

    /// The era a stored code names. An unrecognised code — a row written by a
    /// *newer* build, read here after a downgrade — reads as
    /// [`Self::CompactionUnjournaled`], the benign direction: this build does
    /// not know what that journal guarantees, so it must not claim an
    /// integrity failure on its behalf.
    pub fn from_code(code: i64) -> Self {
        match code {
            1 => Self::CompactionJournaled,
            _ => Self::CompactionUnjournaled,
        }
    }
}

/// What a digest mismatch on one reconstruction actually means — the verdict
/// every rendering surface styles from, so none of them has to re-derive it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MismatchSeverity {
    /// No block mismatched. Nothing to report.
    None,
    /// The journal predates compaction journaling its rewrites, so the
    /// mismatched bytes are the pre-compaction output of a result that was
    /// rewritten in place. Routine; renders as a warning that names compaction
    /// as the cause, never as tampering.
    Compaction,
    /// The journal records every compaction rewrite, so compaction is no
    /// longer an available explanation: these bytes are unaccounted for.
    /// Renders as a real integrity signal.
    Integrity,
}

/// The outcome of reconstructing one step: the rebuilt messages plus the honest
/// accounting of anything the fold could not fully vouch for.
#[derive(Debug, Clone, PartialEq)]
pub struct Reconstruction {
    /// The rebuilt message sequence, in wire order.
    pub messages: Vec<CompletionMessage>,
    /// Block ids whose preimage could not be resolved from the journal or the
    /// local gap store — the documented non-reconstructable cases (synthetic
    /// results, discarded speculation, attachments). Empty on the clean path.
    pub unresolved: Vec<String>,
    /// Block ids whose resolved preimage did NOT re-hash to the recorded
    /// digest: the bytes in [`Self::messages`] for these blocks are the closest
    /// preimage the journal holds, not the exact bytes the step sent. Usually a
    /// compaction rewrite the journal was never told about. Empty on the clean
    /// path.
    pub digest_mismatches: Vec<String>,
    /// Which compaction-journaling era wrote this execution's journal, read
    /// from its `executions` row. Decides what
    /// [`Self::digest_mismatches`] *means* — see [`Self::mismatch_severity`].
    pub journal_era: JournalEra,
}

impl Reconstruction {
    /// Whether every block resolved and every journal-resolved digest matched —
    /// the step is a faithful, verified reconstruction of what the model saw.
    ///
    /// Deliberately era-blind: a mismatch is a mismatch, and a reconstruction
    /// with one is not something to vouch for whoever wrote it. The era
    /// changes how loudly it is *reported*, never whether it happened.
    pub fn is_verified(&self) -> bool {
        self.unresolved.is_empty() && self.digest_mismatches.is_empty()
    }

    /// What this reconstruction's digest mismatches mean, given who wrote the
    /// journal. **The single place the two eras are told apart** — every
    /// surface styles from this rather than reasoning about the era itself.
    pub fn mismatch_severity(&self) -> MismatchSeverity {
        if self.digest_mismatches.is_empty() {
            return MismatchSeverity::None;
        }
        match self.journal_era {
            JournalEra::CompactionUnjournaled => MismatchSeverity::Compaction,
            JournalEra::CompactionJournaled => MismatchSeverity::Integrity,
        }
    }
}

/// `sha256` hex of a string (byte-wise; the sha2 0.11 output does not `LowerHex`).
fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in h.finalize() {
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

/// Per-execution preimage index built once from the event journal: tool calls
/// and outputs by `call_id`, and assistant text keyed by its content digest.
///
/// `pub(crate)` so the v18 → v19 migration can recompute a stored block's token
/// cost from the *same* preimage this module reconstructs from. A migration
/// that resolved preimages its own way would be a second answer to "what bytes
/// was this block", which is the class of bug #925 was.
#[derive(Default)]
pub(crate) struct JournalPreimages {
    tool_calls: HashMap<String, ToolCall>,
    tool_outputs: HashMap<String, ToolOutput>,
    /// `content_digest` ("sha256:<hex>") → the assistant text bytes.
    text_by_digest: HashMap<String, String>,
    /// `content_digest` ("sha256:<hex>") → the serialized post-rewrite tool
    /// output a compaction pass journaled (#1667). Consulted before the
    /// `call_id` fallback: a compacted block's digest resolves here exactly,
    /// where the `call_id` route can only reach the pre-compaction bytes.
    rewrites_by_digest: HashMap<String, String>,
}

impl Store {
    /// Reconstruct the engine's own worker call at this step — the common case,
    /// [`Store::reconstruct_call`] at `call_seq` 0.
    pub fn reconstruct_worker_step(
        &self,
        execution_id: i64,
        turn_instance: u32,
        step: u64,
    ) -> Result<Reconstruction> {
        self.reconstruct_call(execution_id, turn_instance, step, 0)
    }

    /// Reconstruct the exact messages sent on one model call from its persisted
    /// receipt + the event journal. See the module docs for the reconstructable
    /// boundary; callers should check [`Reconstruction::is_verified`].
    ///
    /// `call_seq` selects which call at this step: 0 is the engine's worker
    /// (see [`Store::reconstruct_worker_step`]), 1 the overflow summarizer, 2+
    /// an allocated management role. [`Store::recorded_calls`] enumerates them.
    pub fn reconstruct_call(
        &self,
        execution_id: i64,
        turn_instance: u32,
        step: u64,
        call_seq: u64,
    ) -> Result<Reconstruction> {
        let manifest = self.step_manifest(execution_id, turn_instance, step, call_seq)?;
        let blocks: HashMap<String, crate::ContextBlockRow> = self
            .context_blocks(execution_id)?
            .into_iter()
            .map(|b| (b.block_id.clone(), b))
            .collect();
        let preimages = self.journal_preimages(execution_id)?;

        let mut messages: Vec<CompletionMessage> = Vec::new();
        let mut unresolved = Vec::new();
        let mut digest_mismatches = Vec::new();
        let mut current: Option<u64> = None;

        for entry in &manifest {
            let Some(block) = blocks.get(&entry.block_id) else {
                // A manifest cited a block never registered — cannot resolve.
                unresolved.push(entry.block_id.clone());
                continue;
            };
            let Some(content) = resolve_content(block, &preimages) else {
                unresolved.push(entry.block_id.clone());
                continue;
            };
            if !content_matches_digest(block, &content) {
                digest_mismatches.push(entry.block_id.clone());
            }
            // Regroup: a change in message_index starts a new CompletionMessage,
            // whose role is fixed by the first block that opens it.
            if current != Some(entry.message_index) {
                messages.push(empty_message_for(&block.kind));
                current = Some(entry.message_index);
            }
            let message = messages
                .last_mut()
                .expect("a message was just pushed for this group");
            append_block(
                message,
                block,
                &content,
                entry.call_id.as_deref(),
                &mut unresolved,
            );
        }

        Ok(Reconstruction {
            messages,
            unresolved,
            digest_mismatches,
            journal_era: self.journal_era(execution_id)?,
        })
    }

    /// Index the execution's `tool_start` / `tool_result` / `text` events into a
    /// preimage lookup. Mirrors [`Store::materialize_tool_calls`]'s read shape.
    fn journal_preimages(&self, execution_id: i64) -> Result<JournalPreimages> {
        journal_preimages(&self.lock(), execution_id)
    }

    /// The era stamped on an execution row. An execution that is not there at
    /// all reads as [`JournalEra::CompactionUnjournaled`] for the same reason
    /// an unknown code does: nothing is known about that journal, and "nothing
    /// known" must never be rendered as an integrity failure.
    fn journal_era(&self, execution_id: i64) -> Result<JournalEra> {
        let code: Option<i64> = self
            .lock()
            .query_row(
                "SELECT journal_era FROM executions WHERE id = ?1",
                params![execution_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(code.map_or(JournalEra::default(), JournalEra::from_code))
    }
}

/// [`Store::journal_preimages`] against a bare connection, so a migration
/// holding only its transaction can build the same index.
pub(crate) fn journal_preimages(
    conn: &rusqlite::Connection,
    execution_id: i64,
) -> Result<JournalPreimages> {
    let payloads: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT payload FROM events \
             WHERE execution_id = ?1 \
               AND event_type IN ('tool_start', 'tool_result', 'text', 'compaction') \
             ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(params![execution_id], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut out = JournalPreimages::default();
    for payload in &payloads {
        let Ok(event) = serde_json::from_str::<AgentEvent>(payload) else {
            continue;
        };
        match event {
            AgentEvent::ToolStart { call } => {
                out.tool_calls.insert(call.call_id.clone(), call);
            }
            AgentEvent::ToolResult {
                call_id, output, ..
            } => {
                out.tool_outputs.insert(call_id, output);
            }
            AgentEvent::Text { text } => {
                out.text_by_digest
                    .insert(format!("sha256:{}", sha256_hex(&text)), text);
            }
            AgentEvent::Compaction { rewrites, .. } => {
                for rewrite in rewrites {
                    out.rewrites_by_digest
                        .insert(rewrite.content_digest, rewrite.content);
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Whether resolved bytes really are this block's bytes: they re-hash to the
/// `content_digest` the receipt recorded.
///
/// Gap content is stored locally, so its check is tautological and is
/// deliberately skipped as evidence — the proof lives in the journal-resolved
/// kinds.
///
/// This is not a formality. [`resolve_content`] finds a tool block's preimage
/// by `call_id`, but `call_id` does not uniquely identify a *block*: compaction
/// stubs and ages tool results **in place**, so one call can leave several
/// content-addressed blocks behind — the full result and each aged form of it.
/// They all resolve to the same journal event, and only the digest says which
/// one those bytes actually are. A caller that skips this check does not get a
/// slightly-off answer; it gets a confident answer about the wrong content.
pub(crate) fn content_matches_digest(block: &crate::ContextBlockRow, content: &str) -> bool {
    if block.content.is_some() {
        return true;
    }
    let expected = block
        .content_digest
        .strip_prefix("sha256:")
        .unwrap_or(&block.content_digest);
    sha256_hex(content) == expected
}

/// Resolve one block's exact content: gap kinds carry it locally; every other
/// kind is recovered from the journal preimage index. `None` means the fold
/// does not carry this block's preimage (a documented non-reconstructable case).
///
/// Resolution alone does **not** establish that the bytes are this block's —
/// see [`content_matches_digest`], which every caller must apply.
///
/// `pub(crate)` for the v18 → v19 migration — see [`JournalPreimages`].
pub(crate) fn resolve_content(
    block: &crate::ContextBlockRow,
    preimages: &JournalPreimages,
) -> Option<String> {
    if let Some(content) = &block.content {
        return Some(content.clone());
    }
    match block.kind.as_str() {
        "tool_result" => {
            // Digest-keyed first (#1667): a compacted block's journaled
            // replacement is the exact preimage, while the `call_id` route
            // below reaches only the original `tool_result` event — the right
            // bytes for an untouched block, the pre-compaction bytes (a digest
            // mismatch) for a rewritten one.
            if let Some(content) = preimages.rewrites_by_digest.get(&block.content_digest) {
                return Some(content.clone());
            }
            let call_id = block.call_id.as_ref()?;
            let output = preimages.tool_outputs.get(call_id)?;
            serde_json::to_string(output).ok()
        }
        "tool_call" => {
            let call_id = block.call_id.as_ref()?;
            let call = preimages.tool_calls.get(call_id)?;
            serde_json::to_string(call).ok()
        }
        "assistant_text" => preimages.text_by_digest.get(&block.content_digest).cloned(),
        _ => None,
    }
}

/// An empty `CompletionMessage` with the role the given block kind opens.
fn empty_message_for(kind: &str) -> CompletionMessage {
    let role = match kind {
        "system_prefix" => MessageRole::System,
        // `summary` moved here in Phase 2 (#713). The comment it replaces said
        // "the summary is spliced as assistant-authored"; it is not — the
        // overflow summarizer splices `CompletionMessage::user`, so rebuilding
        // it as an assistant message would have produced a transcript the model
        // never saw. The mapping was unobservable until now because nothing
        // ever emitted a `summary` block, so no stored row is affected: this
        // corrects a latent bug rather than changing a behavior.
        //
        // `recalled_frame` and `attachment` join for the ordinary reason —
        // both hang off the user message that carries them.
        "user_goal" | "steered" | "summary" | "recalled_frame" | "attachment" => MessageRole::User,
        "tool_result" => MessageRole::Tool,
        // assistant_text, tool_call, and anything else open an assistant
        // message.
        _ => MessageRole::Assistant,
    };
    CompletionMessage {
        role,
        content: String::new(),
        tool_calls: Vec::new(),
        tool_results: Vec::new(),
        attachments: Vec::new(),
    }
}

/// Fold one resolved block into the message it belongs to.
///
/// `occurrence_call_id` is the manifest entry's own `call_id` (v15). It wins
/// over the block's birth `call_id` for the rebuilt `ToolResult`, because
/// `block_id` is content-addressed: two calls whose results are byte-identical
/// share one registry row, so `ContextBlockRow::call_id` names only the call
/// that minted it first. Emitting that id for the second occurrence would put a
/// `tool_use_id` in the reconstruction that the model never saw. `None` (every
/// pre-v15 row, and every non-tool block) falls back to birth provenance, which
/// is the best the older shape can know.
fn append_block(
    message: &mut CompletionMessage,
    block: &crate::ContextBlockRow,
    content: &str,
    occurrence_call_id: Option<&str>,
    unresolved: &mut Vec<String>,
) {
    match block.kind.as_str() {
        // APPEND, not assign. One message can now decompose into several text
        // blocks — the recall block splits per item (#713) — and the segments
        // were cut so that concatenating them in manifest order reproduces the
        // original bytes exactly. For the single-block kinds this is identical
        // to the assignment it replaces: the message was just created empty.
        "system_prefix" | "user_goal" | "steered" | "assistant_text" | "summary"
        | "recalled_frame" => {
            message.content.push_str(content);
        }
        "attachment" => match serde_json::from_str::<stella_protocol::Attachment>(content) {
            Ok(attachment) => message.attachments.push(attachment),
            Err(_) => unresolved.push(block.block_id.clone()),
        },
        "tool_call" => match serde_json::from_str::<ToolCall>(content) {
            Ok(call) => message.tool_calls.push(call),
            Err(_) => unresolved.push(block.block_id.clone()),
        },
        "tool_result" => match serde_json::from_str::<ToolOutput>(content) {
            Ok(output) => message.tool_results.push(ToolResult {
                call_id: occurrence_call_id
                    .map(str::to_owned)
                    .or_else(|| block.call_id.clone())
                    .unwrap_or_default(),
                output,
            }),
            Err(_) => unresolved.push(block.block_id.clone()),
        },
        _ => unresolved.push(block.block_id.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContextBlockRow, ManifestBlockRow, StepManifestRow};

    fn digest(s: &str) -> String {
        format!("sha256:{}", sha256_hex(s))
    }

    fn gap(block_id: &str, kind: &str, mi: u64, content: &str) -> ContextBlockRow {
        ContextBlockRow {
            block_id: block_id.into(),
            kind: kind.into(),
            origin_turn: 0,
            origin_step: mi,
            call_id: None,
            memory_id: None,
            token_cost: Some(10),
            content_digest: digest(content),
            citation_label: None,
            content: Some(content.into()),
        }
    }

    fn journal(
        block_id: &str,
        kind: &str,
        call_id: Option<&str>,
        content: &str,
    ) -> ContextBlockRow {
        ContextBlockRow {
            block_id: block_id.into(),
            kind: kind.into(),
            origin_turn: 0,
            origin_step: 0,
            call_id: call_id.map(str::to_owned),
            memory_id: None,
            token_cost: Some(10),
            content_digest: digest(content),
            citation_label: None,
            content: None,
        }
    }

    fn entry(block_id: &str, mi: u64) -> ManifestBlockRow {
        ManifestBlockRow {
            block_id: block_id.into(),
            cache_zone: "cacheable".into(),
            token_cost: Some(10),
            resident_since_step: 0,
            message_index: mi,
            call_id: None,
        }
    }

    #[test]
    fn reconstructs_a_tool_round_trip_byte_exact_and_verified_from_the_fold() {
        // The step-1 input the model saw: system, user, an assistant tool call,
        // and its tool result. Exactly the shape a real turn produces.
        let call = ToolCall {
            call_id: "c1".into(),
            name: "read_file".into(),
            input: serde_json::json!({ "path": "a.rs" }),
        };
        let output = ToolOutput::Ok {
            content: "fn a() {}".into(),
        };
        let call_json = serde_json::to_string(&call).unwrap();
        let output_json = serde_json::to_string(&output).unwrap();

        let original = vec![
            CompletionMessage {
                role: MessageRole::System,
                content: "you are careful".into(),
                tool_calls: vec![],
                tool_results: vec![],
                attachments: vec![],
            },
            CompletionMessage {
                role: MessageRole::User,
                content: "fix it".into(),
                tool_calls: vec![],
                tool_results: vec![],
                attachments: vec![],
            },
            CompletionMessage {
                role: MessageRole::Assistant,
                content: String::new(),
                tool_calls: vec![call.clone()],
                tool_results: vec![],
                attachments: vec![],
            },
            CompletionMessage {
                role: MessageRole::Tool,
                content: String::new(),
                tool_calls: vec![],
                tool_results: vec![ToolResult {
                    call_id: "c1".into(),
                    output: output.clone(),
                }],
                attachments: vec![],
            },
        ];

        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("run", "p", "anthropic", "opus")
            .unwrap();

        // The journal: the events whose preimages the tool blocks resolve from.
        store
            .record_event(id, 0, &AgentEvent::ToolStart { call: call.clone() })
            .unwrap();
        store
            .record_event(
                id,
                1,
                &AgentEvent::ToolResult {
                    call_id: "c1".into(),
                    output: output.clone(),
                    duration_ms: 5,
                    speculated: false,
                },
            )
            .unwrap();

        // The receipt: gap blocks carry local content; tool blocks carry only a
        // digest and resolve from the journal above.
        store
            .record_context_block(id, &gap("blk_sys", "system_prefix", 0, "you are careful"))
            .unwrap();
        store
            .record_context_block(id, &gap("blk_user", "user_goal", 1, "fix it"))
            .unwrap();
        store
            .record_context_block(
                id,
                &journal("blk_call", "tool_call", Some("c1"), &call_json),
            )
            .unwrap();
        store
            .record_context_block(
                id,
                &journal("blk_res", "tool_result", Some("c1"), &output_json),
            )
            .unwrap();

        store
            .record_step_manifest(
                id,
                &StepManifestRow {
                    turn_instance: 0,
                    step: 1,
                    call_seq: 0,
                    provider: "anthropic".into(),
                    model: "opus".into(),
                    call_role: "worker".into(),
                    effective_budget_tokens: 100,
                    calibration_factor: 1.0,
                    estimated_input_tokens: 40,
                    compiled_frame_id: None,
                    frame_hash: None,
                    blocks: vec![
                        entry("blk_sys", 0),
                        entry("blk_user", 1),
                        entry("blk_call", 2),
                        entry("blk_res", 3),
                    ],
                },
            )
            .unwrap();

        let recon = store.reconstruct_worker_step(id, 0, 1).unwrap();
        assert!(
            recon.is_verified(),
            "unresolved={:?} mismatches={:?}",
            recon.unresolved,
            recon.digest_mismatches
        );
        // Byte-exact via PartialEq (order-independent for ToolCall.input Value).
        assert_eq!(recon.messages, original);
    }

    #[test]
    fn a_compacted_result_reconstructs_to_the_bytes_the_model_received() {
        // The witness for #1667. A tool result was journaled, then compaction
        // rewrote it in place and journaled the replacement on its Compaction
        // event. A step AFTER the rewrite cites the post-compaction block, so
        // its reconstruction must show the stub the model actually received —
        // resolved by digest and verified — not the pre-compaction output the
        // `call_id` route reaches (which is what the pre-#1667 fallback
        // produced, as a digest mismatch).
        let original = ToolOutput::Ok {
            content: "a".repeat(4_000),
        };
        let stubbed = ToolOutput::Ok {
            content: "[tool output evicted to fit context]".into(),
        };
        let stubbed_json = serde_json::to_string(&stubbed).unwrap();

        let store = Store::in_memory().unwrap();
        let id = store.begin_execution("run", "p", "z", "m").unwrap();
        store
            .record_event(
                id,
                0,
                &AgentEvent::ToolResult {
                    call_id: "c1".into(),
                    output: original,
                    duration_ms: 5,
                    speculated: false,
                },
            )
            .unwrap();
        store
            .record_event(
                id,
                1,
                &AgentEvent::Compaction {
                    before_tokens: 1_200,
                    after_tokens: 40,
                    evicted: 1,
                    deduped: 0,
                    superseded: 0,
                    aged: 0,
                    summarized: 0,
                    evicted_blocks: vec!["blk_pre".into()],
                    deduped_blocks: vec![],
                    superseded_blocks: vec![],
                    aged_blocks: vec![],
                    summarized_blocks: vec![],
                    rewrites: vec![stella_protocol::CompactionRewrite {
                        block_id: "blk_post".into(),
                        content_digest: digest(&stubbed_json),
                        content: stubbed_json.clone(),
                    }],
                    effective_budget_tokens: 100,
                    calibration_factor: 1.0,
                },
            )
            .unwrap();
        store
            .record_context_block(
                id,
                &journal("blk_post", "tool_result", Some("c1"), &stubbed_json),
            )
            .unwrap();
        store
            .record_step_manifest(
                id,
                &StepManifestRow {
                    turn_instance: 0,
                    step: 2,
                    call_seq: 0,
                    provider: "z".into(),
                    model: "m".into(),
                    call_role: "worker".into(),
                    effective_budget_tokens: 100,
                    calibration_factor: 1.0,
                    estimated_input_tokens: 10,
                    compiled_frame_id: None,
                    frame_hash: None,
                    blocks: vec![entry("blk_post", 0)],
                },
            )
            .unwrap();

        let recon = store.reconstruct_worker_step(id, 0, 2).unwrap();
        assert!(
            recon.is_verified(),
            "the journaled rewrite must resolve by digest: unresolved={:?} mismatches={:?}",
            recon.unresolved,
            recon.digest_mismatches
        );
        assert_eq!(recon.messages.len(), 1);
        assert_eq!(recon.messages[0].tool_results[0].output, stubbed);
    }

    /// Seed one execution whose single manifest block resolves through the
    /// `call_id` fallback to bytes that are NOT its recorded digest — the
    /// shape a compaction rewrite leaves behind. Returns its id.
    ///
    /// Both eras of the witness below share it verbatim, because the whole
    /// claim is that *the same unresolvable block* reads differently depending
    /// only on who wrote the journal.
    fn seed_mismatching_block(store: &Store) -> i64 {
        let journaled = ToolOutput::Ok {
            content: "the original tool output".into(),
        };
        let sent = ToolOutput::Ok {
            content: "[tool output evicted to fit context]".into(),
        };
        let sent_json = serde_json::to_string(&sent).unwrap();

        let id = store.begin_execution("run", "p", "z", "m").unwrap();
        store
            .record_event(
                id,
                0,
                &AgentEvent::ToolResult {
                    call_id: "c1".into(),
                    output: journaled,
                    duration_ms: 5,
                    speculated: false,
                },
            )
            .unwrap();
        // No Compaction event carrying the replacement: the block's digest is
        // over `sent`, and the only preimage the journal holds under `c1` is
        // the pre-compaction output.
        store
            .record_context_block(
                id,
                &journal("blk_post", "tool_result", Some("c1"), &sent_json),
            )
            .unwrap();
        store
            .record_step_manifest(
                id,
                &StepManifestRow {
                    turn_instance: 0,
                    step: 1,
                    call_seq: 0,
                    provider: "z".into(),
                    model: "m".into(),
                    call_role: "worker".into(),
                    effective_budget_tokens: 100,
                    calibration_factor: 1.0,
                    estimated_input_tokens: 10,
                    compiled_frame_id: None,
                    frame_hash: None,
                    blocks: vec![entry("blk_post", 0)],
                },
            )
            .unwrap();
        id
    }

    #[test]
    fn the_same_mismatch_is_housekeeping_on_a_legacy_journal_and_an_alarm_on_a_current_one() {
        // The witness for #1981. #1668 downgraded the mismatch surface to a
        // benign warning because a mismatch was ROUTINE: compaction rewrote
        // tool results in place and journaled nothing, so every compacted
        // block mismatched forever. #1667/PR #1979 closed that — a current
        // journal carries the replacement bytes — which makes a mismatch there
        // mean something again. Both readings are correct, for different
        // journals, so the surface has to tell the two apart. Before this
        // change it could not: there was no era signal at all, and both of
        // these reconstructions rendered as the same warning.
        let store = Store::in_memory().unwrap();

        let current = seed_mismatching_block(&store);
        let legacy = seed_mismatching_block(&store);
        // What a row written before schema v22 looks like after the migration
        // backfills it: era 0, because the build that wrote it could not have
        // journaled a rewrite.
        store
            .lock()
            .execute(
                "UPDATE executions SET journal_era = 0 WHERE id = ?1",
                params![legacy],
            )
            .unwrap();

        let legacy_recon = store.reconstruct_worker_step(legacy, 0, 1).unwrap();
        let current_recon = store.reconstruct_worker_step(current, 0, 1).unwrap();

        // Same block, same bytes, same failure — the reconstructions differ in
        // nothing except who wrote the journal.
        assert_eq!(legacy_recon.digest_mismatches, vec!["blk_post".to_string()]);
        assert_eq!(
            current_recon.digest_mismatches,
            legacy_recon.digest_mismatches
        );
        assert_eq!(legacy_recon.messages, current_recon.messages);
        assert!(!legacy_recon.is_verified() && !current_recon.is_verified());

        // ...and yet they must not read the same.
        assert_eq!(
            legacy_recon.mismatch_severity(),
            MismatchSeverity::Compaction,
            "a pre-#1667 journal mismatches on ordinary compaction; calling that \
             an integrity failure is the false alarm #1668 removed"
        );
        assert_eq!(
            current_recon.mismatch_severity(),
            MismatchSeverity::Integrity,
            "this journal records every compaction rewrite, so compaction is not \
             an available explanation for these bytes"
        );
        assert_ne!(
            legacy_recon.mismatch_severity(),
            current_recon.mismatch_severity()
        );
    }

    #[test]
    fn a_clean_reconstruction_has_no_severity_to_report_in_either_era() {
        // Severity is about mismatches, not about the era: a current-era
        // journal with nothing wrong must not acquire an alarm just by being
        // current.
        let recon = Reconstruction {
            messages: Vec::new(),
            unresolved: vec!["blk_gap".into()],
            digest_mismatches: Vec::new(),
            journal_era: JournalEra::CompactionJournaled,
        };
        assert_eq!(recon.mismatch_severity(), MismatchSeverity::None);
        assert!(!recon.is_verified(), "an unresolved block is still a gap");
    }

    #[test]
    fn an_unrecognised_era_code_reads_as_the_oldest_one() {
        // A row written by a NEWER build, read here after a downgrade. This
        // build cannot know what that journal guarantees, and the honest
        // failure direction is to under-alarm rather than to accuse.
        assert_eq!(JournalEra::from_code(7), JournalEra::CompactionUnjournaled);
        assert_eq!(JournalEra::from_code(0), JournalEra::CompactionUnjournaled);
        assert_eq!(JournalEra::from_code(1), JournalEra::CompactionJournaled);
        assert_eq!(JournalEra::CURRENT.code(), 1);
        assert_eq!(JournalEra::default(), JournalEra::CompactionUnjournaled);
    }

    #[test]
    fn a_run_this_build_started_is_stamped_as_journaling_its_rewrites() {
        // The writer-side half of the era signal: the stamp has to be made
        // while this binary is the one talking, because nothing downstream can
        // recover it from the events.
        let store = Store::in_memory().unwrap();
        let id = store.begin_execution("run", "p", "z", "m").unwrap();
        let stamped: i64 = store
            .lock()
            .query_row(
                "SELECT journal_era FROM executions WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(JournalEra::from_code(stamped), JournalEra::CURRENT);
    }

    #[test]
    fn a_block_with_no_journal_preimage_surfaces_as_unresolved_not_a_lie() {
        // A tool_result block whose ToolResult event is absent (the deferred
        // synthetic/speculation cases) must be reported, never fabricated.
        let store = Store::in_memory().unwrap();
        let id = store.begin_execution("run", "p", "z", "m").unwrap();
        store
            .record_context_block(
                id,
                &journal(
                    "blk_orphan",
                    "tool_result",
                    Some("missing"),
                    "{\"ok\":{\"content\":\"x\"}}",
                ),
            )
            .unwrap();
        store
            .record_step_manifest(
                id,
                &StepManifestRow {
                    turn_instance: 0,
                    step: 0,
                    call_seq: 0,
                    provider: "z".into(),
                    model: "m".into(),
                    call_role: "worker".into(),
                    effective_budget_tokens: 1,
                    calibration_factor: 1.0,
                    estimated_input_tokens: 1,
                    compiled_frame_id: None,
                    frame_hash: None,
                    blocks: vec![entry("blk_orphan", 0)],
                },
            )
            .unwrap();

        let recon = store.reconstruct_worker_step(id, 0, 0).unwrap();
        assert!(!recon.is_verified());
        assert_eq!(recon.unresolved, vec!["blk_orphan".to_string()]);
    }
}
