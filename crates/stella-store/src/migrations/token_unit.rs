//! The v18 → v19 data migration: one token-counting rule, applied to history
//! (#925).
//!
//! Split out of [`super`] rather than living beside the other steps because it
//! is a different *kind* of migration. Every other entry in the ladder reshapes
//! schema — a column, an index, a rebuilt primary key — and can be read as DDL.
//! This one reshapes **data**: it re-derives a number on every row already at
//! rest, from each block's own preimage, which needs the journal index and the
//! digest check that the reconstruction path uses. That is enough logic to
//! deserve its own module and its own tests (`super` is also under a 1500-line
//! ratchet, which a step this size does not fit inside).
//!
//! See [`migrate_v18_to_v19`] for why history is rewritten rather than
//! reinterpreted at read time.

use rusqlite::params;

use super::table_exists;
use crate::Result;

/// v18 → v19: recompute every stored `context_blocks.token_cost` under the one
/// token-counting rule, and make the column nullable so a block whose preimage
/// is gone can say so (#925).
///
/// # Why history is rewritten rather than reinterpreted
///
/// Until now two rules were in play: the receipts emitter counted `chars()`,
/// the conversation estimator counted UTF-8 bytes. Every row written under the
/// character rule is *wrong under the surviving rule* for any non-ASCII
/// content, by up to 4x. There are two ways to make a reader correct again —
/// teach it both rules and pick per row, or move every row onto one rule — and
/// only the second leaves a store where `SELECT sum(token_cost)` means
/// something. A version branch in the read path would also have to be carried
/// by every future reader of this column, forever, to compensate for a bug
/// that lasted one release.
///
/// So the numbers move here, once, and no reader ever learns that the
/// character rule existed.
///
/// # Where the new number comes from
///
/// The block's own preimage — resolved by [`crate::reconstruct::resolve_content`],
/// the same function `stella inspect` reconstructs through, over the same
/// journal index. This is deliberately not a formula applied to the old
/// number: `ceil(chars/3.5)` is not invertible, so there is no arithmetic that
/// recovers a byte count from it. The cost is *re-derived from the content*,
/// which is also what makes the result verifiable — it is the number the
/// emitter would have written had it always used the surviving rule.
///
/// # Blocks whose preimage is gone
///
/// Not every stored block still has a preimage: spec §6.4's synthetic and
/// discarded-speculation tool results never had a journal event, and an
/// execution can carry `block_registered` events without the `tool_start` /
/// `tool_result` events its blocks refer to. `stella inspect` already refuses
/// to vouch for those blocks (they surface in `Reconstruction::unresolved`).
/// Their cost is set NULL — not left at the character-rule number, which would
/// silently keep two units in one column, and not zeroed, which would understate
/// a real cost and be indistinguishable from an empty block. NULL is the one
/// answer that is true: this store cannot derive that cost.
///
/// # Cost
///
/// One pass over `context_blocks`, plus one journal index per execution that
/// has blocks. Both are bounded by what a single workspace has recorded, and
/// the whole thing runs inside the runner's transaction — a crash leaves the
/// file at v18 with its original numbers, never half-converted.
pub(super) fn migrate_v18_to_v19(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    // Guard on the rebuild, not on the recompute: a file that already has the
    // v19 shape has already been converted, and re-running the recompute would
    // be harmless but pointless. `column_exists` cannot see nullability, so the
    // marker is the rebuild having happened at all — which the runner's
    // user_version stamp already guarantees, making this a belt-and-braces
    // check consistent with the rest of the ladder.
    if !table_exists(tx, "context_blocks")? {
        return Ok(());
    }

    let costs = recomputed_block_costs(tx)?;

    // SQLite cannot drop a NOT NULL constraint in place (lang_altertable §7),
    // so this is a rebuild. Column order and the primary key are carried over
    // verbatim; only `token_cost`'s nullability changes.
    tx.execute_batch(
        "CREATE TABLE context_blocks_v19 (
           execution_id INTEGER NOT NULL,
           block_id TEXT NOT NULL,
           kind TEXT NOT NULL,
           origin_turn INTEGER NOT NULL,
           origin_step INTEGER NOT NULL,
           call_id TEXT,
           memory_id TEXT,
           token_cost INTEGER,
           content_digest TEXT NOT NULL,
           citation_label TEXT,
           content TEXT,
           first_seen_ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           PRIMARY KEY (execution_id, block_id)
         );
         INSERT INTO context_blocks_v19
           (execution_id, block_id, kind, origin_turn, origin_step, call_id,
            memory_id, token_cost, content_digest, citation_label, content,
            first_seen_ts)
         SELECT execution_id, block_id, kind, origin_turn, origin_step, call_id,
                memory_id, token_cost, content_digest, citation_label, content,
                first_seen_ts
           FROM context_blocks;
         DROP TABLE context_blocks;
         ALTER TABLE context_blocks_v19 RENAME TO context_blocks;
         CREATE INDEX IF NOT EXISTS context_blocks_by_memory
           ON context_blocks(memory_id, execution_id);",
    )?;

    // Every row moves: the ones whose preimage resolved take the re-derived
    // byte-rule cost, and the rest go NULL. Blanket the column to NULL first so
    // a row missing from `costs` cannot keep its character-rule number by
    // omission — the failure mode would be invisible (a plausible number in a
    // column that is supposed to hold one unit) and is exactly what this
    // migration exists to end.
    tx.execute_batch("UPDATE context_blocks SET token_cost = NULL;")?;
    {
        let mut stmt = tx.prepare(
            "UPDATE context_blocks SET token_cost = ?3
             WHERE execution_id = ?1 AND block_id = ?2",
        )?;
        for (execution_id, block_id, cost) in &costs {
            stmt.execute(params![execution_id, block_id, i64::from(*cost)])?;
        }
    }
    Ok(())
}

/// Re-derive the byte-rule token cost of every block that still has a
/// preimage, as `(execution_id, block_id, token_cost)`.
///
/// Blocks whose preimage the store no longer holds are simply absent from the
/// result — the caller reads absence as NULL. Grouped by execution because the
/// journal index is per-execution and building it once per block would re-read
/// the whole journal for every row.
fn recomputed_block_costs(tx: &rusqlite::Transaction<'_>) -> Result<Vec<(i64, String, u32)>> {
    let execution_ids: Vec<i64> = {
        let mut stmt = tx.prepare("SELECT DISTINCT execution_id FROM context_blocks")?;
        let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut out = Vec::new();
    for execution_id in execution_ids {
        let blocks: Vec<crate::ContextBlockRow> = {
            let mut stmt = tx.prepare(
                "SELECT block_id, kind, origin_turn, origin_step, call_id, memory_id,
                        token_cost, content_digest, citation_label, content
                 FROM context_blocks WHERE execution_id = ?1",
            )?;
            let rows = stmt.query_map(params![execution_id], |r| {
                Ok(crate::ContextBlockRow {
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
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if blocks.is_empty() {
            continue;
        }
        let preimages = crate::reconstruct::journal_preimages(tx, execution_id)?;
        for block in blocks {
            let Some(content) = crate::reconstruct::resolve_content(&block, &preimages) else {
                continue;
            };
            // Resolving is not enough — the bytes have to be provably THIS
            // block's. A tool block's preimage is found by `call_id`, and
            // compaction can leave one call several blocks (the full result and
            // each aged form), all of which resolve to the same journal event.
            // Costing the wrong one produces a confidently wrong number that no
            // later reader could distinguish from a correct one, which is worse
            // than the mixed-unit column this migration exists to fix.
            if !crate::reconstruct::content_matches_digest(&block, &content) {
                continue;
            }
            out.push((
                execution_id,
                block.block_id,
                stella_protocol::estimate_token_cost(&content),
            ));
        }
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::apply_migration;
    use rusqlite::Connection;

    /// #925 end to end: a v18 file whose block costs were minted by a character
    /// count comes out the far side counted in bytes, from the blocks' own
    /// preimages — not reinterpreted at read time, and not left mixed.
    #[test]
    fn v19_migration_recounts_legacy_block_costs_in_bytes_from_their_preimages() {
        let mut conn = Connection::open_in_memory().expect("db");
        // A v18 file: `token_cost` is NOT NULL and every value in it was
        // produced by `ceil(chars / 3.5)`.
        conn.execute_batch(
            "CREATE TABLE context_blocks (
               execution_id INTEGER NOT NULL,
               block_id TEXT NOT NULL,
               kind TEXT NOT NULL,
               origin_turn INTEGER NOT NULL,
               origin_step INTEGER NOT NULL,
               call_id TEXT,
               memory_id TEXT,
               token_cost INTEGER NOT NULL,
               content_digest TEXT NOT NULL,
               citation_label TEXT,
               content TEXT,
               first_seen_ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               PRIMARY KEY (execution_id, block_id)
             );
             CREATE TABLE events (
               execution_id INTEGER NOT NULL, seq INTEGER NOT NULL,
               ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               event_type TEXT NOT NULL, payload TEXT NOT NULL,
               UNIQUE (execution_id, seq));",
        )
        .expect("v18 receipts shape");

        // Five CJK characters, three bytes each. The character rule called it
        // ceil(5/3.5) = 2; the surviving rule is ceil(15/3.5) = 5.
        let cjk = "日本語です";
        assert_eq!(cjk.chars().count(), 5);
        assert_eq!(cjk.len(), 15);
        // ASCII, where the two rules have always agreed — this row must come
        // out unchanged, or the migration is moving numbers it should not.
        let ascii = "fix the parser";
        assert_eq!(ascii.len(), ascii.chars().count());

        // The tool result's journal event. `resolve_content` rebuilds the
        // block's preimage as `serde_json::to_string(&ToolOutput)`, so both the
        // recorded digest and the expected cost are over THAT string, not over
        // the bare content — which is why the migration re-derives rather than
        // guessing, and why the digest is what proves it re-derived correctly.
        let output = stella_protocol::ToolOutput::Ok {
            content: "結果です".into(),
        };
        let tool_preimage = serde_json::to_string(&output).expect("serialize output");
        let tool_digest = {
            use sha2::{Digest, Sha256};
            use std::fmt::Write as _;
            let mut h = Sha256::new();
            h.update(tool_preimage.as_bytes());
            let mut hex = String::from("sha256:");
            for b in h.finalize() {
                let _ = write!(&mut hex, "{b:02x}");
            }
            hex
        };

        conn.execute_batch(&format!(
            "INSERT INTO context_blocks
               (execution_id, block_id, kind, origin_turn, origin_step, call_id,
                memory_id, token_cost, content_digest, content)
             VALUES
               -- gap kind: preimage is the local `content` column
               (1, 'blk_cjk', 'user_goal', 0, 0, NULL, NULL, 2, 'sha256:x', '{cjk}'),
               (1, 'blk_ascii', 'user_goal', 0, 0, NULL, NULL, 4, 'sha256:y', '{ascii}'),
               -- journal-backed: preimage is the tool_result event below
               (1, 'blk_tool', 'tool_result', 0, 1, 'c1', NULL, 3, '{tool_digest}', NULL),
               -- no preimage anywhere: the block's call was never journaled
               (1, 'blk_orphan', 'tool_result', 0, 2, 'gone', NULL, 7, 'sha256:w', NULL);"
        ))
        .expect("legacy rows");
        // Serialized from the real event type, not hand-rolled JSON: the
        // migration parses the same `AgentEvent` a live journal holds, so the
        // test would be worthless against an approximation of the wire shape.
        let payload = serde_json::to_string(&stella_protocol::AgentEvent::ToolResult {
            call_id: "c1".into(),
            output: output.clone(),
            duration_ms: 7,
            speculated: false,
        })
        .expect("serialize event");
        conn.execute(
            "INSERT INTO events (execution_id, seq, event_type, payload)
             VALUES (1, 0, 'tool_result', ?1)",
            [payload],
        )
        .expect("journal event");

        apply_migration(&mut conn, migrate_v18_to_v19, 19).expect("migrate");

        fn cost(conn: &Connection, id: &str) -> Option<i64> {
            conn.query_row(
                "SELECT token_cost FROM context_blocks WHERE block_id = ?1",
                [id],
                |r| r.get(0),
            )
            .expect("row survives the rebuild")
        }

        // The whole point: the multi-byte row is recounted in bytes.
        assert_eq!(
            cost(&conn, "blk_cjk"),
            Some(5),
            "a CJK goal must cost its 15 bytes, not its 5 characters"
        );
        // And the ASCII row is untouched, because the two rules agreed on it.
        assert_eq!(cost(&conn, "blk_ascii"), Some(4));
        // The journal-backed row is re-derived from the serialized ToolOutput.
        let expected_tool = stella_protocol::estimate_token_cost(&tool_preimage);
        assert_eq!(cost(&conn, "blk_tool"), Some(i64::from(expected_tool)));
        assert_ne!(
            cost(&conn, "blk_tool"),
            Some(3),
            "the stored character-rule number must not survive"
        );
        // No preimage, no number. Emphatically NOT the old 7, which would leave
        // this column holding two units, and not 0, which would understate a
        // real cost and read as an empty block.
        assert_eq!(
            cost(&conn, "blk_orphan"),
            None,
            "an underivable cost is absent, never a stale one"
        );

        // Everything else about the row survived the table rebuild.
        let (kind, digest, memory): (String, String, Option<String>) = conn
            .query_row(
                "SELECT kind, content_digest, memory_id FROM context_blocks
                 WHERE block_id = 'blk_orphan'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("orphan row kept");
        assert_eq!(kind, "tool_result");
        assert_eq!(digest, "sha256:w", "the evidence that IS still here stays");
        assert_eq!(memory, None);

        // The rebuilt column accepts NULL, which the v18 shape could not.
        conn.execute(
            "INSERT INTO context_blocks
               (execution_id, block_id, kind, origin_turn, origin_step, token_cost,
                content_digest)
             VALUES (1, 'blk_new', 'other', 0, 0, NULL, 'sha256:n')",
            [],
        )
        .expect("the v19 shape admits an underivable cost");

        // Re-running is safe: the second pass re-derives the same numbers from
        // the same preimages rather than compounding a conversion.
        apply_migration(&mut conn, migrate_v18_to_v19, 19).expect("idempotent");
        assert_eq!(
            cost(&conn, "blk_cjk"),
            Some(5),
            "a second pass is not a second conversion"
        );
        assert_eq!(cost(&conn, "blk_tool"), Some(i64::from(expected_tool)));
    }

    /// The migration must not depend on the receipts plane existing at all —
    /// a store whose executions all predate v11 has no `context_blocks`.
    #[test]
    fn v19_migration_is_a_no_op_on_a_file_without_the_receipts_plane() {
        let mut conn = Connection::open_in_memory().expect("db");
        apply_migration(&mut conn, migrate_v18_to_v19, 19).expect("no receipts plane is fine");
    }

    /// Resolving a preimage is not the same as proving it is *this* block's.
    ///
    /// Compaction stubs and ages tool results in place, so one `call_id` can
    /// leave several content-addressed blocks behind. All of them resolve to
    /// the single `tool_result` event in the journal, and only the recorded
    /// digest distinguishes the block those bytes belong to from the ones they
    /// do not. Found on real data: without this guard an aged block whose
    /// receipt said 493 tokens was recounted at 8717 — a 17.7x "correction",
    /// which is impossible for chars→bytes (4x is the ceiling) and is the
    /// signature of costing another block's content.
    #[test]
    fn v19_migration_refuses_a_preimage_that_does_not_match_the_blocks_digest() {
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE context_blocks (
               execution_id INTEGER NOT NULL, block_id TEXT NOT NULL, kind TEXT NOT NULL,
               origin_turn INTEGER NOT NULL, origin_step INTEGER NOT NULL, call_id TEXT,
               memory_id TEXT, token_cost INTEGER NOT NULL, content_digest TEXT NOT NULL,
               citation_label TEXT, content TEXT,
               first_seen_ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               PRIMARY KEY (execution_id, block_id));
             CREATE TABLE events (
               execution_id INTEGER NOT NULL, seq INTEGER NOT NULL,
               ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               event_type TEXT NOT NULL, payload TEXT NOT NULL,
               UNIQUE (execution_id, seq));",
        )
        .expect("v18 shape");

        // The journal holds the full result of call `c1`.
        let full = stella_protocol::ToolOutput::Ok {
            content: "a very long tool result ".repeat(40),
        };
        let full_preimage = serde_json::to_string(&full).expect("serialize");
        conn.execute(
            "INSERT INTO events (execution_id, seq, event_type, payload) VALUES (1, 0, 'tool_result', ?1)",
            [serde_json::to_string(&stella_protocol::AgentEvent::ToolResult {
                call_id: "c1".into(),
                output: full.clone(),
                duration_ms: 1,
                speculated: false,
            })
            .expect("serialize event")],
        )
        .expect("journal");

        // Two blocks for that one call: the full result, and the aged stub
        // compaction left in its place. Both name `c1`; only their digests
        // differ.
        let digest = |s: &str| {
            use sha2::{Digest, Sha256};
            use std::fmt::Write as _;
            let mut h = Sha256::new();
            h.update(s.as_bytes());
            let mut hex = String::from("sha256:");
            for b in h.finalize() {
                let _ = write!(&mut hex, "{b:02x}");
            }
            hex
        };
        conn.execute_batch(&format!(
            "INSERT INTO context_blocks
               (execution_id, block_id, kind, origin_turn, origin_step, call_id,
                token_cost, content_digest)
             VALUES
               (1, 'blk_full', 'tool_result', 0, 0, 'c1', 1, '{}'),
               (1, 'blk_aged', 'tool_result', 0, 5, 'c1', 1, '{}');",
            digest(&full_preimage),
            digest("[tool result aged out]")
        ))
        .expect("both blocks");

        apply_migration(&mut conn, migrate_v18_to_v19, 19).expect("migrate");

        let cost = |conn: &Connection, id: &str| -> Option<i64> {
            conn.query_row(
                "SELECT token_cost FROM context_blocks WHERE block_id = ?1",
                [id],
                |r| r.get(0),
            )
            .expect("row")
        };
        assert_eq!(
            cost(&conn, "blk_full"),
            Some(i64::from(stella_protocol::estimate_token_cost(
                &full_preimage
            ))),
            "the block those bytes DO belong to is recounted"
        );
        assert_eq!(
            cost(&conn, "blk_aged"),
            None,
            "the aged block's bytes are gone; costing the full result in its \
             place would be a confident lie, so it records no cost"
        );
    }

    /// A byte count is never below a character count, so no block's cost may
    /// fall. Pinned because a fall is the loudest possible signal that a row
    /// was costed against content that is not its own — it cannot happen from
    /// the unit change alone. Verified on a real 2,956-block store: 1,011
    /// unchanged (ASCII), 627 raised, 0 lowered.
    #[test]
    fn v19_migration_never_lowers_a_block_cost() {
        let mut conn = Connection::open_in_memory().expect("db");
        conn.execute_batch(
            "CREATE TABLE context_blocks (
               execution_id INTEGER NOT NULL, block_id TEXT NOT NULL, kind TEXT NOT NULL,
               origin_turn INTEGER NOT NULL, origin_step INTEGER NOT NULL, call_id TEXT,
               memory_id TEXT, token_cost INTEGER NOT NULL, content_digest TEXT NOT NULL,
               citation_label TEXT, content TEXT,
               first_seen_ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               PRIMARY KEY (execution_id, block_id));
             CREATE TABLE events (
               execution_id INTEGER NOT NULL, seq INTEGER NOT NULL,
               ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               event_type TEXT NOT NULL, payload TEXT NOT NULL,
               UNIQUE (execution_id, seq));",
        )
        .expect("v18 shape");

        // A spread of scripts, each stored with the cost the character rule
        // would have given it.
        let samples = [
            "plain ascii prose",
            "日本語のテキストです",
            "émojis 🚀🚀 and accents àéîöü",
            "",
            "中文内容混合 with English",
        ];
        for (i, text) in samples.iter().enumerate() {
            let char_rule = (text.chars().count() as f64 / 3.5).ceil() as i64;
            conn.execute(
                "INSERT INTO context_blocks
                   (execution_id, block_id, kind, origin_turn, origin_step,
                    token_cost, content_digest, content)
                 VALUES (1, ?1, 'user_goal', 0, 0, ?2, 'sha256:x', ?3)",
                rusqlite::params![format!("blk_{i}"), char_rule, text],
            )
            .expect("row");
        }

        apply_migration(&mut conn, migrate_v18_to_v19, 19).expect("migrate");

        let mut stmt = conn
            .prepare("SELECT block_id, token_cost FROM context_blocks ORDER BY block_id")
            .expect("prepare");
        let rows: Vec<(String, Option<i64>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("rows");
        for (i, (id, new)) in rows.iter().enumerate() {
            let text = samples[i];
            let char_rule = (text.chars().count() as f64 / 3.5).ceil() as i64;
            let new = new.expect("local content always resolves");
            assert!(
                new >= char_rule,
                "{id} ({text:?}) fell from {char_rule} to {new}"
            );
            assert_eq!(
                new,
                i64::from(stella_protocol::estimate_token_cost(text)),
                "{id} must land exactly on the shared rule"
            );
        }
    }
}
