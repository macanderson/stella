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
//! against the digest the receipt recorded, so a torn journal or a fabricated
//! block surfaces as a mismatch instead of a plausible-looking lie.
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

use rusqlite::params;
use sha2::{Digest, Sha256};
use stella_protocol::{
    AgentEvent, CompletionMessage, MessageRole, ToolCall, ToolOutput, ToolResult,
};

use crate::{Result, Store};

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
    /// Block ids whose resolved preimage did NOT re-hash to the recorded digest
    /// — a torn-journal or tampering signal. Empty on the clean path.
    pub digest_mismatches: Vec<String>,
}

impl Reconstruction {
    /// Whether every block resolved and every journal-resolved digest matched —
    /// the step is a faithful, verified reconstruction of what the model saw.
    pub fn is_verified(&self) -> bool {
        self.unresolved.is_empty() && self.digest_mismatches.is_empty()
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
        })
    }

    /// Index the execution's `tool_start` / `tool_result` / `text` events into a
    /// preimage lookup. Mirrors [`Store::materialize_tool_calls`]'s read shape.
    fn journal_preimages(&self, execution_id: i64) -> Result<JournalPreimages> {
        journal_preimages(&self.lock(), execution_id)
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
             WHERE execution_id = ?1 AND event_type IN ('tool_start', 'tool_result', 'text') \
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
            AgentEvent::Text { delta } => {
                out.text_by_digest
                    .insert(format!("sha256:{}", sha256_hex(&delta)), delta);
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
