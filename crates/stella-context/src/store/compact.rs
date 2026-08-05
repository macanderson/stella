//! Compaction — the reclaim path the context plane never had.
//!
//! Until this module, `context.db` only grew. `supersede_node` writes a
//! tombstone, an edit closes a revision, and a re-embed leaves the previous
//! vector where it was; nothing anywhere deleted a row, so a workspace's
//! context file was a monotone function of its lifetime. That was a deliberate
//! posture, not an oversight — but it was over-broad, and this module narrows it
//! to what the law actually requires.
//!
//! # What `L-C3` protects, and what it does not
//!
//! `L-C3` ("corrections close-and-supersede, never delete") is a guarantee about
//! **queryability**: it forbids losing the ability to answer *what did we
//! believe at T*. It is not a guarantee about bytes. A row whose deletion cannot
//! change the answer to any point-in-time question is not protected by it, and
//! keeping such a row buys nothing but disk.
//!
//! `docs/adr/0010-incremental-authority-transfer.md` decision point 6 (ratified
//! 2026-07-26) says which rows those are:
//!
//! > `node`, `edge`, and `embedding` are a derived retrieval index over content,
//! > not a record store — `embedding` is already keyed by `(content_hash,
//! > fingerprint)` and rebuildable from content. … A record's content is
//! > authoritative; its index entries are disposable and may be dropped and
//! > rebuilt without consulting `context_records`.
//!
//! Disposable is not the same as unused, so this module reclaims only index
//! entries whose *owner is already gone* — plus one class that is opt-in
//! precisely because rebuilding it costs real work.
//!
//! # The three classes reclaimed
//!
//! 1. **Orphaned embeddings.** An `embedding` row whose `content_hash` appears
//!    on no `node` row *in any state* and matches no live `memory` revision.
//!    This is the mass. At the default 256 dims a vector is ~1 KiB of BLOB, and
//!    every memory *edit* strands exactly one: the mirror node is keyed by
//!    lineage, so an edit rewrites its `content_hash` in place and the previous
//!    revision's vector is left with nothing pointing at it (see the comment on
//!    `MemoryInput::as_node` in `writeback.rs`). Rebuildable on demand by
//!    `nodes_missing_embedding` plus `warm::warm_index`, which is exactly what
//!    makes it disposable.
//! 2. **Orphaned domain junctions.** A `node_domains` / `edge_domains` row whose
//!    `node_id` / `edge_id` no longer resolves. `store/domain.rs` contains zero
//!    `DELETE`s, so nothing has ever removed one.
//! 3. **Stale-fingerprint embeddings — opt-in only.** Rows written under an
//!    embedder other than the active one. Recall never reads them (`L-C2`
//!    forbids mixing fingerprints) and `warm` never sees them, so they are
//!    already inert; but re-embedding is real compute, so reclaiming them is
//!    never the default. See [`ContextCompactPolicy::stale_fingerprints`].
//!
//! # The three classes deliberately NOT reclaimed
//!
//! Named exclusions, in the manner of `stella_store::DEPENDENT_TABLES`'
//! `reflections` and export-ledger carve-outs: an omission that is not written
//! down reads as an oversight, and the next person "completes" it.
//!
//! - **`edge` rows — never, in any state.** They are the plane's only
//!   bi-temporal audit substrate. `edges_as_of` and `neighbors` answer "what did
//!   we believe at T" purely from `recorded_at`/`superseded_at` on rows that
//!   supersession *closed* rather than removed. Deleting a superseded edge does
//!   not save an index entry; it deletes the answer. That is a direct `L-C3`
//!   violation and the one thing this module must never be extended to do.
//! - **`memory` revision rows.** `MemoryLineageStats::superseded` is *derived*
//!   by counting exactly these rows on every call
//!   ([`ContextStore::memory_lineage_stats`]), so deleting them would silently
//!   change a number the CLI reports after every edit — with no watermark able
//!   to reconstruct it, because the count is per lineage and the scalar would
//!   not be.
//! - **Superseded `node` rows.** These look like the obvious next win and are
//!   not yet reclaimable. `stella memory restore` resolves its target *through*
//!   the surviving row (`restore_node` deliberately queries without a liveness
//!   filter, and `node_exists_any_state` exists only to see it), so dropping the
//!   row turns a reversible forget into a permanent one. Reclaiming them
//!   requires re-routing restore through the canonical tombstone in `store.db`,
//!   whose own DDL comment already anticipates this: a tombstone "must outlive
//!   edits to the thing it buries — including the row being deleted entirely"
//!   (`stella-store/src/ddl.rs`). Until restore reads `forgotten.content`, a
//!   superseded node is a record, not an index entry, and ADR 0010 does not
//!   cover it.
//!
//! # The watermark
//!
//! Compaction writes a singleton row into `context_compaction` (schema V6,
//! `MIGRATION_V6` in `store/schema.rs`) every time it runs, including when it
//! reclaims nothing. It follows `compacted_through_execution_id` in
//! `stella-store`'s export ledger, which is the best precedent in the tree for
//! this shape: when compaction deletes the row that proved a fact, a scalar must
//! remember in its place. `compacted_at` is written `MAX(existing, new)` and the
//! counters accumulate, so the row can only rise — a clock that jumped backwards
//! cannot un-record a compaction that happened.
//!
//! What it buys: `embedding` count minus live-node count is no longer readable
//! as "the index is broken", because the watermark says how much was reclaimed
//! and when. What it deliberately does *not* do is gate anything. Nothing
//! consults it before deleting — unlike the export ledger's floor, a re-run is
//! harmless here, because the second pass simply finds nothing.
//!
//! # One transaction
//!
//! Every class is deleted inside a single transaction (`L-L1`: every write batch
//! is one transaction), so a kill mid-compaction leaves the store exactly as it
//! was. `dry_run` computes the identical report and then drops the transaction
//! un-committed, which is how `stella-store`'s `prune` does it.

use rusqlite::{Connection, OptionalExtension, params};

use crate::candidates::NODE_AS_OF;
use crate::error::ContextError;

use super::{ContextStore, sha256_hex};

#[cfg(test)]
mod tests;

/// A compaction that reclaims at least this many rows runs `VACUUM` on its own,
/// handing the freed pages back to the filesystem instead of leaving them in the
/// file's freelist. `--vacuum` forces it for any non-empty compaction. Matches
/// `stella_store::prune`'s threshold — two reclaim paths in one product should
/// not have surprisingly different file-shrinking behaviour.
const LARGE_COMPACTION_ROWS: u64 = 10_000;

/// Scratch table holding the content hash of every live memory revision.
///
/// Materialized rather than expressed in SQL because SQLite has no `sha256`:
/// `memory` stores content, `embedding` is keyed by its hash, and only Rust can
/// bridge them. Temp-scoped and rebuilt per call, exactly like
/// `stella_store::prune`'s target table.
const LIVE_MEMORY_HASHES: &str = "temp.context_compact_live_memory_hashes";

/// An `embedding` row is orphaned when its content hash appears on no `node`
/// row **in any state** and matches no live `memory` revision.
///
/// "In any state" — no `superseded_at` filter — is the load-bearing part. A
/// forgotten memory's node still carries its hash, and `stella memory restore`
/// must find that node's vector still there; filtering to live nodes would make
/// a forget-then-compact silently destroy the vector a restore needs, turning a
/// reversible operation into a lossy one.
///
/// The live-`memory` clause is belt-and-braces today: every live revision is
/// mirrored to a node carrying the same content, so the first clause already
/// covers it. It is stated anyway so the predicate means what it says
/// independently of the mirror invariant — if mirroring ever changes, this must
/// fail closed rather than start deleting live memories' vectors.
const DELETE_ORPHANED_EMBEDDINGS: &str = "\
DELETE FROM embedding
 WHERE NOT EXISTS (
           SELECT 1 FROM node n WHERE n.content_hash = embedding.content_hash
       )
   AND NOT EXISTS (
           SELECT 1 FROM temp.context_compact_live_memory_hashes h
            WHERE h.content_hash = embedding.content_hash
       )";

/// Domain tags pointing at a node that is gone. Under `PRAGMA foreign_keys=ON`
/// the junction's `REFERENCES node(id)` makes these unreachable through the
/// normal write path — which is the point: they can only come from a store
/// written with the pragma off, a partial restore, or a hand-edited file, and
/// today nothing at all would ever remove them.
const DELETE_ORPHANED_NODE_DOMAINS: &str = "\
DELETE FROM node_domains
 WHERE NOT EXISTS (SELECT 1 FROM node n WHERE n.id = node_domains.node_id)";

/// Domain tags pointing at an edge that is gone. See
/// [`DELETE_ORPHANED_NODE_DOMAINS`]; nothing deletes edges either, so this is
/// the same defensive class.
const DELETE_ORPHANED_EDGE_DOMAINS: &str = "\
DELETE FROM edge_domains
 WHERE NOT EXISTS (SELECT 1 FROM edge e WHERE e.id = edge_domains.edge_id)";

/// Vectors under an embedder that is not the active one. Index-backed by
/// `idx_embedding_fingerprint` (schema V4), so this is a range delete rather
/// than a scan of every blob in the store.
const DELETE_STALE_FINGERPRINT_EMBEDDINGS: &str = "DELETE FROM embedding WHERE fingerprint <> ?1";

/// Which classes a compaction reclaims. Cheap to build, and every field is a
/// deliberate choice rather than a tuning knob.
///
/// [`Self::default`] is **empty on purpose** — no class enabled, so it is a
/// no-op and [`ContextStore::compact`] refuses it. A policy that silently did
/// nothing is the failure mode `stella stats prune` already turned into a hard
/// error; a `Default` that quietly deleted things would be worse.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextCompactPolicy {
    /// Reclaim index entries whose owner is gone: orphaned embeddings and
    /// orphaned `node_domains`/`edge_domains` rows (classes 1 and 2 in the
    /// module docs). Both are pure garbage — nothing can read them, and nothing
    /// has to be recomputed to replace them — so they travel together.
    pub orphans: bool,
    /// Also reclaim vectors written under an embedder other than the active
    /// fingerprint.
    ///
    /// Off by default, and it must stay that way. These rows are already
    /// invisible: `L-C2` forbids recall from mixing fingerprints, and
    /// `nodes_missing_embedding` only ever asks about the active one. So
    /// deleting them costs *nothing today* — the price is paid only if the
    /// workspace ever reverts to that embedder, which then has to re-embed the
    /// whole corpus. [`ContextCompactReport::stale_vectors_of_live_nodes`]
    /// quantifies that future bill before it is incurred.
    pub stale_fingerprints: bool,
    /// `VACUUM` afterwards to return freed pages to the filesystem. Also
    /// happens automatically past this module's large-compaction threshold.
    pub vacuum: bool,
    /// Compute the report without deleting anything: the transaction is rolled
    /// back and `VACUUM` is skipped.
    pub dry_run: bool,
}

impl ContextCompactPolicy {
    /// The policy the CLI's flagless `stella memory compact` builds: reclaim
    /// orphans, leave stale fingerprints alone.
    #[must_use]
    pub fn orphans_only() -> Self {
        Self {
            orphans: true,
            ..Self::default()
        }
    }

    /// Whether this policy would reclaim nothing whatever the store contains.
    ///
    /// [`ContextStore::compact`] turns this into an error rather than an empty
    /// report, because the two are not the same answer: "nothing to reclaim" is
    /// a fact about the store, "nothing selected" is a mistake in the call.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        !self.orphans && !self.stale_fingerprints
    }
}

/// What [`ContextStore::compact`] reclaimed (or, under
/// [`ContextCompactPolicy::dry_run`], would have reclaimed).
///
/// Counts are per class rather than one total, so an unexpected number can be
/// attributed to a predicate instead of to "compaction".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextCompactReport {
    /// `embedding` rows no node in any state and no live memory pointed at.
    pub orphaned_embeddings: u64,
    /// `node_domains` rows whose node is gone.
    pub orphaned_node_domains: u64,
    /// `edge_domains` rows whose edge is gone.
    pub orphaned_edge_domains: u64,
    /// `embedding` rows under a non-active fingerprint. Always `0` unless
    /// [`ContextCompactPolicy::stale_fingerprints`] was set.
    pub stale_fingerprint_embeddings: u64,
    /// Distinct content hashes of **live** nodes that lost a stale-fingerprint
    /// vector — the re-embedding bill if this workspace ever reverts to that
    /// embedder.
    ///
    /// Deliberately not called "rebuild cost": under the *active* fingerprint it
    /// is zero, because `warm` never sees a stale vector and would have
    /// recomputed these nodes anyway. Reporting it as work this compaction
    /// created would be false. It is a conditional future cost, and it is the
    /// number that makes `--stale-fingerprints` an informed choice.
    pub stale_vectors_of_live_nodes: u64,
    /// Whether `VACUUM` ran.
    pub vacuumed: bool,
}

impl ContextCompactReport {
    /// Total rows deleted across every class.
    #[must_use]
    pub fn total_rows(&self) -> u64 {
        self.orphaned_embeddings
            + self.orphaned_node_domains
            + self.orphaned_edge_domains
            + self.stale_fingerprint_embeddings
    }
}

/// The durable memory of every compaction this store has run — the scalar that
/// stands in for the deleted rows.
///
/// The counters are **lifetime totals across all compactions**, not the last
/// one's: each run adds to them, so the row can only rise. `compacted_at` is the
/// latest run's timestamp under the same rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionWatermark {
    /// When compaction last ran (RFC-3339).
    pub compacted_at: String,
    /// Orphaned embeddings reclaimed over this store's lifetime.
    pub orphaned_embeddings: u64,
    /// Orphaned `node_domains` rows reclaimed over this store's lifetime.
    pub orphaned_node_domains: u64,
    /// Orphaned `edge_domains` rows reclaimed over this store's lifetime.
    pub orphaned_edge_domains: u64,
    /// Stale-fingerprint embeddings reclaimed over this store's lifetime.
    pub stale_fingerprint_embeddings: u64,
}

impl ContextStore {
    /// Reclaim disposable index rows per `policy` — the engine behind `stella
    /// memory compact`.
    ///
    /// Everything it deletes is an index entry whose owner is already gone, or
    /// (opt-in) a vector under an embedder nothing reads. It never touches an
    /// `edge`, a `memory` revision, or a superseded `node`; see the module docs
    /// for each exclusion and its reason.
    ///
    /// All deletes commit in one transaction (`L-L1`), then the watermark is
    /// stamped in that same transaction, so a store can never be observed with
    /// rows reclaimed and no record of the reclaim. `VACUUM` runs after the
    /// commit — SQLite cannot vacuum inside a transaction.
    ///
    /// A [`no-op policy`](ContextCompactPolicy::is_noop) is an error, not an
    /// empty report.
    pub fn compact(
        &self,
        policy: &ContextCompactPolicy,
    ) -> Result<ContextCompactReport, ContextError> {
        if policy.is_noop() {
            return Err(ContextError::InvalidInput(
                "nothing selected to compact — a policy that reclaims no class is a \
                 mistake in the call, not a store that is already clean"
                    .into(),
            ));
        }
        let fingerprint = self.fingerprint().id();
        let now = self.clock().now_rfc3339();
        let conn = self.conn();
        let mut report = ContextCompactReport::default();

        {
            let tx = conn.unchecked_transaction()?;

            if policy.orphans {
                report.orphaned_embeddings = delete_orphaned_embeddings(&tx)?;
                report.orphaned_node_domains = tx.execute(DELETE_ORPHANED_NODE_DOMAINS, [])? as u64;
                report.orphaned_edge_domains = tx.execute(DELETE_ORPHANED_EDGE_DOMAINS, [])? as u64;
            }

            if policy.stale_fingerprints {
                // Measured BEFORE the delete: afterwards there is nothing left
                // to count.
                report.stale_vectors_of_live_nodes =
                    stale_vectors_of_live_nodes(&tx, &fingerprint)?;
                report.stale_fingerprint_embeddings =
                    tx.execute(DELETE_STALE_FINGERPRINT_EMBEDDINGS, params![fingerprint])? as u64;
            }

            if policy.dry_run {
                // `tx` drops un-committed, so every delete above is rolled back
                // and no watermark is stamped: a dry run must leave no trace,
                // including in the record of what has been compacted.
                return Ok(report);
            }

            record_watermark(&tx, &now, &report)?;
            tx.commit()?;
        }

        // Only now, outside the transaction: SQLite refuses to VACUUM inside one.
        let deleted = report.total_rows();
        if deleted > 0 && (policy.vacuum || deleted >= LARGE_COMPACTION_ROWS) {
            conn.execute_batch("VACUUM")?;
            report.vacuumed = true;
        }
        Ok(report)
    }

    /// What compaction has reclaimed from this store, or `None` if it has never
    /// run here.
    ///
    /// `None` is the honest answer for a store that predates compaction as well
    /// as one that has simply never been compacted; the two are the same state,
    /// because migration V6 creates the table empty rather than back-dating a
    /// run that did not happen.
    pub fn compaction_watermark(&self) -> Result<Option<CompactionWatermark>, ContextError> {
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT compacted_at, orphaned_embeddings, orphaned_node_domains,
                        orphaned_edge_domains, stale_fingerprint_embeddings
                 FROM context_compaction WHERE singleton = 1",
                [],
                |r| {
                    Ok(CompactionWatermark {
                        compacted_at: r.get(0)?,
                        orphaned_embeddings: r.get::<_, i64>(1)?.max(0) as u64,
                        orphaned_node_domains: r.get::<_, i64>(2)?.max(0) as u64,
                        orphaned_edge_domains: r.get::<_, i64>(3)?.max(0) as u64,
                        stale_fingerprint_embeddings: r.get::<_, i64>(4)?.max(0) as u64,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }
}

/// Delete every orphaned `embedding` row, returning how many went.
///
/// The live-memory half of [`DELETE_ORPHANED_EMBEDDINGS`] needs sha256, which
/// SQLite does not have, so the hashes are computed here and staged in
/// [`LIVE_MEMORY_HASHES`]. One row per live lineage, so the scratch table is
/// bounded by the number of memories, not by revisions or by corpus size.
fn delete_orphaned_embeddings(tx: &Connection) -> Result<u64, ContextError> {
    tx.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {LIVE_MEMORY_HASHES} (content_hash TEXT PRIMARY KEY);
         DELETE FROM {LIVE_MEMORY_HASHES};"
    ))?;
    {
        let mut read = tx.prepare("SELECT content FROM memory WHERE superseded_at IS NULL")?;
        let mut write = tx.prepare(&format!(
            "INSERT OR IGNORE INTO {LIVE_MEMORY_HASHES} (content_hash) VALUES (?1)"
        ))?;
        let rows = read.query_map([], |r| r.get::<_, String>(0))?;
        for content in rows {
            write.execute(params![sha256_hex(&content?)])?;
        }
    }
    let removed = tx.execute(DELETE_ORPHANED_EMBEDDINGS, [])? as u64;
    tx.execute_batch(&format!("DROP TABLE IF EXISTS {LIVE_MEMORY_HASHES};"))?;
    Ok(removed)
}

/// Distinct content hashes of currently-believed nodes that carry a
/// stale-fingerprint vector — see
/// [`ContextCompactReport::stale_vectors_of_live_nodes`].
///
/// Liveness comes from [`NODE_AS_OF`] with a NULL cutoff rather than a fifth
/// hand-written copy of `superseded_at IS NULL`. One shared predicate is the
/// whole reason #712 deliverable 7 exists: a reader that spells liveness itself
/// is a reader that can be forgotten when the rule changes.
fn stale_vectors_of_live_nodes(tx: &Connection, fingerprint: &str) -> Result<u64, ContextError> {
    let n: i64 = tx.query_row(
        &format!(
            "SELECT COUNT(DISTINCT n.content_hash)
               FROM node n
              WHERE {NODE_AS_OF}
                AND EXISTS (
                        SELECT 1 FROM embedding e
                         WHERE e.content_hash = n.content_hash AND e.fingerprint <> ?2
                    )"
        ),
        params![None::<&str>, fingerprint],
        |r| r.get(0),
    )?;
    Ok(n.max(0) as u64)
}

/// Stamp the compaction watermark, monotonically.
///
/// `MAX(existing, new)` on the timestamp and `+` on the counters means neither
/// can be walked backwards — not by a clock that jumped, not by a re-run that
/// found nothing. Written in the caller's transaction so the record and the
/// deletions it describes commit together.
fn record_watermark(
    tx: &Connection,
    now: &str,
    report: &ContextCompactReport,
) -> Result<(), ContextError> {
    tx.execute(
        "INSERT INTO context_compaction
             (singleton, compacted_at, orphaned_embeddings, orphaned_node_domains,
              orphaned_edge_domains, stale_fingerprint_embeddings)
         VALUES (1, ?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(singleton) DO UPDATE SET
             compacted_at = MAX(context_compaction.compacted_at, excluded.compacted_at),
             orphaned_embeddings =
                 context_compaction.orphaned_embeddings + excluded.orphaned_embeddings,
             orphaned_node_domains =
                 context_compaction.orphaned_node_domains + excluded.orphaned_node_domains,
             orphaned_edge_domains =
                 context_compaction.orphaned_edge_domains + excluded.orphaned_edge_domains,
             stale_fingerprint_embeddings =
                 context_compaction.stale_fingerprint_embeddings
                 + excluded.stale_fingerprint_embeddings",
        params![
            now,
            report.orphaned_embeddings as i64,
            report.orphaned_node_domains as i64,
            report.orphaned_edge_domains as i64,
            report.stale_fingerprint_embeddings as i64,
        ],
    )?;
    Ok(())
}
