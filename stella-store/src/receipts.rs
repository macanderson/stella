//! Context-receipts persistence (spec §4/§5): the block registry and the
//! per-step request manifest. Content-free — these rows carry digests, ids,
//! and small ints, never payload bytes. The preimage of any block lives in the
//! originating event in the journal; these tables are the queryable index over
//! it that makes any past step reconstructable and auditable.

use rusqlite::params;

use crate::{Result, Store, sqlite_i64};

/// One context block as registered at birth (`context_blocks`, spec §4).
/// `kind`/`cache_zone` are the wire enums already serialized to their
/// snake_case strings by the caller — the store stays string-typed and never
/// depends on the protocol enum shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBlockRow {
    pub block_id: String,
    pub kind: String,
    pub origin_turn: u32,
    pub origin_step: u64,
    pub call_id: Option<String>,
    pub memory_id: Option<String>,
    /// This block's cost under [`stella_protocol::tokens`]'s rule — the one
    /// rule, the same one `estimated_input_tokens` is summed with.
    ///
    /// `None` means **not derivable**, not "free". A block's cost is a function
    /// of its preimage, and the v18 → v19 migration recomputed every stored row
    /// from the preimage the store still holds; for blocks whose preimage the
    /// journal never carried (spec §6.4's synthetic and discarded-speculation
    /// results, and any execution whose tool events were never journaled) there
    /// is nothing left to derive a cost from. Carrying the old character-count
    /// number for those would leave the column mixing two units forever, which
    /// is the thing #925 was — so an underivable cost is absent rather than
    /// wrong. Live emission always knows the content, so the writer never
    /// stores `None`; only history can.
    pub token_cost: Option<u32>,
    pub content_digest: String,
    pub citation_label: Option<String>,
    /// Local-only preimage for gap kinds the journal cannot resolve (system
    /// prefix, assembled user/recall message); `None` for journal-resolvable
    /// kinds (spec §5.3). Never leaves the local store.
    pub content: Option<String>,
}

/// One block's membership in a step's manifest (`step_manifest`, spec §5),
/// in wire order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestBlockRow {
    pub block_id: String,
    pub cache_zone: String,
    /// Joined back from `context_blocks`. `None` covers both ways a cost can be
    /// unknown — the manifest cited a block that was never registered, and the
    /// block's preimage is no longer in the store (see
    /// [`ContextBlockRow::token_cost`]). A caller summing these must decide
    /// what an unknown addend means rather than silently reading it as zero.
    pub token_cost: Option<u32>,
    pub resident_since_step: u64,
    /// The sent-message this block belonged to; reconstruction regroups blocks
    /// sharing a `message_index` back into one `CompletionMessage` (spec §5.1).
    pub message_index: u64,
    /// The tool call this occurrence belongs to. Distinct from
    /// `ContextBlockRow::call_id`, which is birth provenance only: a
    /// content-addressed block minted by one call and repeated by another is
    /// registered once, so per-occurrence attribution has to live here.
    pub call_id: Option<String>,
}

/// A full per-step manifest: the header (`step_receipt`) plus its ordered
/// blocks (`step_manifest`). Persisted atomically so a receipt is never
/// half-written.
#[derive(Debug, Clone, PartialEq)]
pub struct StepManifestRow {
    pub turn_instance: u32,
    pub step: u64,
    /// Which model call at this step: 0 the engine's worker, 1 the overflow
    /// summarizer, 2+ an allocated management role. See
    /// `AgentEvent::StepManifest::call_seq`.
    pub call_seq: u64,
    pub provider: String,
    pub model: String,
    pub call_role: String,
    pub effective_budget_tokens: u64,
    pub calibration_factor: f64,
    pub estimated_input_tokens: u64,
    /// The compiled frame this receipt IS (ADR 0006 as amended): its
    /// content-addressed id and byte-stable hash. `None` for every receipt
    /// written while `context.lifecycle.enabled` was off — which is the
    /// default — and for every row that predates the frame. A reader must
    /// treat absence as "the lifecycle was off", never as a damaged receipt.
    pub compiled_frame_id: Option<String>,
    /// `sha256:<64 hex>` over the frame's canonical body. Travels with
    /// `compiled_frame_id`; the two are always both present or both absent.
    pub frame_hash: Option<String>,
    pub blocks: Vec<ManifestBlockRow>,
}

impl Store {
    /// Register one context block. Idempotent: a byte-identical block
    /// re-entering context resolves to the same `block_id`, so a repeat
    /// registration is a no-op (`INSERT OR IGNORE`), never an error or a
    /// double-count — matching the content-addressed identity contract.
    pub fn record_context_block(&self, execution_id: i64, row: &ContextBlockRow) -> Result<()> {
        let origin_step = sqlite_i64("context block origin step", row.origin_step)?;
        let token_cost = row.token_cost.map(i64::from);
        self.lock().execute(
            "INSERT OR IGNORE INTO context_blocks
               (execution_id, block_id, kind, origin_turn, origin_step, call_id, memory_id,
                token_cost, content_digest, citation_label, content)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                execution_id,
                row.block_id,
                row.kind,
                i64::from(row.origin_turn),
                origin_step,
                row.call_id,
                row.memory_id,
                token_cost,
                row.content_digest,
                row.citation_label,
                row.content,
            ],
        )?;
        Ok(())
    }

    /// Persist one step's full manifest — the header row plus every ordered
    /// block row — atomically. `INSERT OR REPLACE` so a re-emitted manifest for
    /// the same (turn_instance, step) overwrites cleanly rather than colliding
    /// on the primary key (the engine emits one manifest per step, but a caller
    /// that replays must be able to re-persist idempotently).
    pub fn record_step_manifest(&self, execution_id: i64, row: &StepManifestRow) -> Result<()> {
        let step = sqlite_i64("manifest step", row.step)?;
        let turn = i64::from(row.turn_instance);
        let budget = sqlite_i64("manifest effective budget", row.effective_budget_tokens)?;
        let estimated = sqlite_i64("manifest estimated input", row.estimated_input_tokens)?;
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let call_seq = sqlite_i64("manifest call seq", row.call_seq)?;
        tx.execute(
            "INSERT OR REPLACE INTO step_receipt
               (execution_id, turn_instance, step, call_seq, provider, model, call_role,
                effective_budget_tokens, calibration_factor, estimated_input_tokens,
                compiled_frame_id, frame_hash)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                execution_id,
                turn,
                step,
                call_seq,
                row.provider,
                row.model,
                row.call_role,
                budget,
                row.calibration_factor,
                estimated,
                row.compiled_frame_id,
                row.frame_hash,
            ],
        )?;
        // Clear any prior ordinals for this call before rewriting, so a
        // shorter re-emitted manifest cannot leave stale tail rows behind.
        // Scoped by call_seq: the auxiliary calls sharing this step keep theirs.
        tx.execute(
            "DELETE FROM step_manifest
             WHERE execution_id = ? AND turn_instance = ? AND step = ? AND call_seq = ?",
            params![execution_id, turn, step, call_seq],
        )?;
        // token_cost is a property of the block (context_blocks.token_cost),
        // not of a manifest entry, so step_manifest stores only ordering, zone,
        // and residency; the reader joins token_cost back from context_blocks.
        for (ordinal, block) in row.blocks.iter().enumerate() {
            let ordinal = sqlite_i64("manifest ordinal", ordinal as u64)?;
            let resident = sqlite_i64("manifest residency", block.resident_since_step)?;
            let message_index = sqlite_i64("manifest message index", block.message_index)?;
            tx.execute(
                "INSERT INTO step_manifest
                   (execution_id, turn_instance, step, call_seq, ordinal, block_id,
                    cache_zone, resident_since_step, message_index, call_id)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    execution_id,
                    turn,
                    step,
                    call_seq,
                    ordinal,
                    block.block_id,
                    block.cache_zone,
                    resident,
                    message_index,
                    block.call_id,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Every block registered for an execution, insertion order.
    pub fn context_blocks(&self, execution_id: i64) -> Result<Vec<ContextBlockRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT block_id, kind, origin_turn, origin_step, call_id, memory_id,
                    token_cost, content_digest, citation_label, content
             FROM context_blocks WHERE execution_id = ? ORDER BY rowid",
        )?;
        let rows = stmt
            .query_map(params![execution_id], |r| {
                Ok(ContextBlockRow {
                    block_id: r.get(0)?,
                    kind: r.get(1)?,
                    origin_turn: r.get::<_, i64>(2)? as u32,
                    origin_step: r.get::<_, i64>(3)? as u64,
                    call_id: r.get(4)?,
                    memory_id: r.get(5)?,
                    token_cost: r.get::<_, Option<i64>>(6)?.map(|c| c as u32),
                    content_digest: r.get(7)?,
                    citation_label: r.get(8)?,
                    content: r.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The ordered blocks of one step's manifest — the receipt of exactly what
    /// the model saw, in wire order.
    pub fn step_manifest(
        &self,
        execution_id: i64,
        turn_instance: u32,
        step: u64,
        call_seq: u64,
    ) -> Result<Vec<ManifestBlockRow>> {
        let step = sqlite_i64("manifest step", step)?;
        let call_seq = sqlite_i64("manifest call seq", call_seq)?;
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT sm.block_id, sm.cache_zone, cb.token_cost, sm.resident_since_step,
                    sm.message_index, sm.call_id
             FROM step_manifest sm
             LEFT JOIN context_blocks cb
               ON cb.execution_id = sm.execution_id AND cb.block_id = sm.block_id
             WHERE sm.execution_id = ? AND sm.turn_instance = ? AND sm.step = ?
               AND sm.call_seq = ?
             ORDER BY sm.ordinal",
        )?;
        let rows = stmt
            .query_map(
                params![execution_id, i64::from(turn_instance), step, call_seq],
                |r| {
                    Ok(ManifestBlockRow {
                        block_id: r.get(0)?,
                        cache_zone: r.get(1)?,
                        // NULL from either side: the LEFT JOIN found no block
                        // row (a manifest citing an unregistered block), or the
                        // block's own cost is not derivable. Both are "unknown",
                        // and both are now surfaced as such rather than
                        // flattened to 0 — a zero is indistinguishable from a
                        // genuinely empty block when a caller sums the column.
                        token_cost: r.get::<_, Option<i64>>(2)?.map(|c| c as u32),
                        resident_since_step: r.get::<_, i64>(3)? as u64,
                        message_index: r.get::<_, i64>(4)? as u64,
                        call_id: r.get(5)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The prompt exactly as it was submitted, before the context engine
    /// mutated it into a message array — the one artifact of a turn the user
    /// authored themselves.
    ///
    /// This is the baseline `stella inspect --diff` uses for a role's *first*
    /// call, which by definition has no predecessor to compare against. Diffing
    /// the assembled context against these bytes is the only way to see what
    /// Stella added on top of what was typed, which is otherwise invisible:
    /// the reconstruction shows the sum, never the delta.
    ///
    /// `None` when the execution id does not exist. Reads the same column the
    /// index preview does, but keyed rather than limit-bounded, so it still
    /// answers for an execution too old to appear in
    /// [`Store::inspectable_executions`].
    pub fn execution_prompt(&self, execution_id: i64) -> Result<Option<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT prompt FROM executions WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![execution_id], |r| r.get::<_, String>(0))?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// One execution's closeout header — the fields trace assembly (#1042)
    /// stamps onto a trajectory record. `None` when the id does not exist.
    /// Read AFTER `finish_execution_accounted` if `outcome`/`finished_at`
    /// must be settled; earlier reads honestly return them as `None`.
    pub fn execution_summary(&self, execution_id: i64) -> Result<Option<ExecutionSummary>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT kind, prompt, provider, model, started_at, finished_at, outcome, cost_usd
             FROM executions WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![execution_id], |r| {
            Ok(ExecutionSummary {
                kind: r.get(0)?,
                prompt: r.get(1)?,
                provider: r.get(2)?,
                model: r.get(3)?,
                started_at: r.get(4)?,
                finished_at: r.get(5)?,
                outcome: r.get(6)?,
                cost_usd: r.get(7)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Every execution of the same chat session as `execution_id`, oldest
    /// first — the *turns* of one conversation.
    ///
    /// A turn is an execution, not a `turn_instance`: each prompt a user
    /// submits at the deck opens a new execution row, and `turn_instance`
    /// counts sub-turns *inside* one of those (goal rounds, subagents) and is
    /// zero for almost every real call. So the comparison a person means by
    /// "the previous turn" is a comparison across executions, which is also the
    /// only boundary where a system prompt can legitimately drift — inside one
    /// execution it is byte-stable by design, for cache reasons.
    ///
    /// Empty when the execution has no `session_id` (a one-shot `stella run`,
    /// or a row predating session tracking); callers should treat that as "this
    /// execution is its own session" rather than as an error.
    pub fn session_executions(&self, execution_id: i64) -> Result<Vec<i64>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id FROM executions
             WHERE session_id IS NOT NULL
               AND session_id = (SELECT session_id FROM executions WHERE id = ?1)
             ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![execution_id], |r| r.get::<_, i64>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Executions with at least one recorded receipt, most recent first.
    pub fn inspectable_executions(&self, limit: u32) -> Result<Vec<InspectableExecution>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT e.id, e.kind, e.prompt, e.started_at, COUNT(*) AS calls
             FROM executions e
             JOIN step_receipt sr ON sr.execution_id = e.id
             GROUP BY e.id, e.kind, e.prompt, e.started_at
             ORDER BY e.id DESC LIMIT ?",
        )?;
        let rows = stmt
            .query_map(params![i64::from(limit)], |r| {
                Ok(InspectableExecution {
                    execution_id: r.get(0)?,
                    kind: r.get(1)?,
                    prompt: r.get(2)?,
                    started_at: r.get(3)?,
                    calls: r.get::<_, i64>(4)? as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Every recorded model call of an execution, in wire order — the index
    /// `stella inspect` lists and walks. One row per call, not per step: a step
    /// that ran the worker plus an overflow summarizer appears twice, keyed
    /// apart by `call_seq`.
    pub fn recorded_calls(&self, execution_id: i64) -> Result<Vec<RecordedCall>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT turn_instance, step, call_seq, call_role, provider, model,
                    estimated_input_tokens, compiled_frame_id, frame_hash
             FROM step_receipt WHERE execution_id = ?
             ORDER BY turn_instance, step, call_seq",
        )?;
        let rows = stmt
            .query_map(params![execution_id], |r| {
                Ok(RecordedCall {
                    turn_instance: r.get::<_, i64>(0)? as u32,
                    step: r.get::<_, i64>(1)? as u64,
                    call_seq: r.get::<_, i64>(2)? as u64,
                    call_role: r.get(3)?,
                    provider: r.get(4)?,
                    model: r.get(5)?,
                    estimated_input_tokens: r.get::<_, i64>(6)? as u64,
                    compiled_frame_id: r.get(7)?,
                    frame_hash: r.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

/// One execution that has at least one reconstructable model call — the index
/// `stella inspect` shows when given no arguments. Executions predating the
/// receipt plane (or run with the store disabled) have no rows and are omitted
/// rather than listed as empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectableExecution {
    pub execution_id: i64,
    pub kind: String,
    pub prompt: String,
    pub started_at: String,
    pub calls: u64,
}

/// One execution's closeout header ([`Store::execution_summary`]): what ran,
/// how it ended, and what it cost in total. `outcome`/`finished_at` are `None`
/// until `finish_execution_accounted` settles the row.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionSummary {
    pub kind: String,
    pub prompt: String,
    pub provider: String,
    pub model: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub outcome: Option<String>,
    pub cost_usd: f64,
}

/// One recorded model call — the header of a receipt, without its blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCall {
    pub turn_instance: u32,
    pub step: u64,
    pub call_seq: u64,
    pub call_role: String,
    pub provider: String,
    pub model: String,
    pub estimated_input_tokens: u64,
    /// This call's compiled-frame id, when one was built (Phase 2, #713).
    pub compiled_frame_id: Option<String>,
    /// Its byte-stable frame hash — what `stella inspect` prints to prove two
    /// calls saw byte-identical context, and what a replay re-derives to prove
    /// a stored receipt has not drifted.
    pub frame_hash: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(id: &str, step: u64) -> ContextBlockRow {
        ContextBlockRow {
            block_id: id.into(),
            kind: "tool_result".into(),
            origin_turn: 0,
            origin_step: step,
            call_id: Some(format!("call_{step}")),
            memory_id: None,
            token_cost: Some(100 + step as u32),
            content_digest: format!("sha256:{id}"),
            citation_label: None,
            content: None,
        }
    }

    #[test]
    fn the_submitted_prompt_is_readable_for_any_execution_by_id() {
        // `inspect --diff` needs the pre-mutation prompt for the execution it
        // was asked about, which the limit-bounded index cannot promise to
        // contain. Absent ids answer None rather than erroring: "no such
        // execution" is a fact, not a failure.
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("run", "do issue 859", "anthropic", "opus")
            .unwrap();
        assert_eq!(
            store.execution_prompt(id).unwrap().as_deref(),
            Some("do issue 859")
        );
        assert_eq!(store.execution_prompt(id + 999).unwrap(), None);
    }

    #[test]
    fn a_sessions_turns_come_back_oldest_first_and_never_mix_conversations() {
        let store = Store::in_memory().unwrap();
        let a1 = store.begin_execution("chat", "one", "z", "m").unwrap();
        let other = store
            .begin_execution("chat", "elsewhere", "z", "m")
            .unwrap();
        let a2 = store.begin_execution("chat", "two", "z", "m").unwrap();
        store.set_execution_session(a1, "ses-a").unwrap();
        store.set_execution_session(a2, "ses-a").unwrap();
        store.set_execution_session(other, "ses-b").unwrap();

        assert_eq!(store.session_executions(a2).unwrap(), vec![a1, a2]);
        assert_eq!(store.session_executions(other).unwrap(), vec![other]);

        // A session-less execution (a one-shot `stella run`) yields nothing
        // rather than sweeping up every other session-less row — callers read
        // the empty vec as "this execution is its own session".
        let loner = store.begin_execution("run", "solo", "z", "m").unwrap();
        assert!(store.session_executions(loner).unwrap().is_empty());
    }

    #[test]
    fn block_registration_is_idempotent_on_the_content_addressed_id() {
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("run", "p", "anthropic", "opus")
            .unwrap();
        store.record_context_block(id, &block("blk_a", 0)).unwrap();
        // Re-registering the same block (identical content re-enters context)
        // must be a silent no-op, never an error or a duplicate row.
        store.record_context_block(id, &block("blk_a", 0)).unwrap();
        store.record_context_block(id, &block("blk_b", 1)).unwrap();
        let blocks = store.context_blocks(id).unwrap();
        assert_eq!(blocks.len(), 2, "identical block must collapse to one row");
        assert_eq!(blocks[0].block_id, "blk_a");
        assert_eq!(blocks[1].call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn manifest_round_trips_in_wire_order_with_its_header() {
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("run", "p", "anthropic", "opus")
            .unwrap();
        store
            .record_context_block(id, &block("blk_sys", 0))
            .unwrap();
        store
            .record_context_block(id, &block("blk_tail", 3))
            .unwrap();
        let manifest = StepManifestRow {
            turn_instance: 0,
            step: 3,
            call_seq: 0,
            provider: "anthropic".into(),
            model: "opus".into(),
            call_role: "worker".into(),
            effective_budget_tokens: 136_363,
            calibration_factor: 1.1,
            estimated_input_tokens: 203,
            compiled_frame_id: None,
            frame_hash: None,
            blocks: vec![
                ManifestBlockRow {
                    block_id: "blk_sys".into(),
                    cache_zone: "stable_prefix".into(),
                    token_cost: Some(100),
                    resident_since_step: 0,
                    message_index: 0,
                    call_id: None,
                },
                ManifestBlockRow {
                    block_id: "blk_tail".into(),
                    cache_zone: "volatile".into(),
                    token_cost: Some(103),
                    resident_since_step: 3,
                    message_index: 1,
                    call_id: Some("call_a".into()),
                },
            ],
        };
        store.record_step_manifest(id, &manifest).unwrap();

        let back = store.step_manifest(id, 0, 3, 0).unwrap();
        assert_eq!(back.len(), 2);
        // Order is the receipt — index 0 is the system prefix.
        assert_eq!(back[0].block_id, "blk_sys");
        assert_eq!(back[0].cache_zone, "stable_prefix");
        assert_eq!(back[0].resident_since_step, 0);
        assert_eq!(back[1].block_id, "blk_tail");
        // token_cost is joined back from context_blocks (the block's property).
        assert_eq!(back[1].token_cost, Some(103));
    }

    #[test]
    fn re_emitting_a_shorter_manifest_leaves_no_stale_tail_rows() {
        let store = Store::in_memory().unwrap();
        let id = store
            .begin_execution("run", "p", "anthropic", "opus")
            .unwrap();
        let three = StepManifestRow {
            turn_instance: 1,
            step: 2,
            call_seq: 0,
            provider: "anthropic".into(),
            model: "opus".into(),
            call_role: "worker".into(),
            effective_budget_tokens: 100,
            calibration_factor: 1.0,
            estimated_input_tokens: 3,
            compiled_frame_id: None,
            frame_hash: None,
            blocks: (0..3)
                .map(|i| ManifestBlockRow {
                    block_id: format!("blk_{i}"),
                    cache_zone: "cacheable".into(),
                    token_cost: Some(1),
                    resident_since_step: 0,
                    message_index: 0,
                    call_id: None,
                })
                .collect(),
        };
        store.record_step_manifest(id, &three).unwrap();
        let shorter = StepManifestRow {
            blocks: three.blocks[..1].to_vec(),
            ..three.clone()
        };
        store.record_step_manifest(id, &shorter).unwrap();
        let back = store.step_manifest(id, 1, 2, 0).unwrap();
        assert_eq!(
            back.len(),
            1,
            "a shorter re-emission must replace, not append"
        );
    }
}
