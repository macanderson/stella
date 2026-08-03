//! The IVF accelerator for recall's cosine scan — **opt-in, never the default**.
//!
//! [`crate::candidates::score_nodes_by_vector`] scores every live vector under
//! the active fingerprint on every turn. That is honest and exact, and it is
//! linear in the corpus: "most similar" is not something SQLite can `ORDER BY`.
//! This module is the accelerator that makes it sublinear when a workspace asks
//! for one, and nothing else about recall changes.
//!
//! # Why an inverted file and not a graph index
//!
//! HNSW is the faster structure and it is the wrong one *here*, for a reason
//! specific to this plane rather than a general preference:
//!
//! - [`ContextStore::supersede_node`](crate::ContextStore::supersede_node) and
//!   [`restore_node`](crate::ContextStore::restore_node) flip a node's liveness
//!   **without touching the `embedding` table at all** — forgetting a memory
//!   writes a tombstone on `node`, and the vector is untouched because the
//!   content did not change.
//! - An `as_of` query changes the live set with no write of any kind.
//!
//! With an IVF the postings are ordinary rows and liveness stays exactly where
//! it already lives: the `JOIN node` under the shared
//! [`NODE_AS_OF`](crate::candidates::NODE_AS_OF) predicate. So supersede,
//! restore, and point-in-time recall need **zero index maintenance** — pinned by
//! `forgetting_a_node_needs_no_reindex` and
//! `a_point_in_time_probe_needs_no_reindex`. A graph index would need an
//! invalidation on every one of those three, and deletion is the weakest
//! operation HNSW has.
//!
//! # What is approximate, and what is not
//!
//! The approximation decides **which candidates are considered**. It never
//! changes the ordering rule: probed candidates are scored with the same
//! `cosine_blob` off the same borrowed BLOB, and the caller applies the same
//! `(score desc, id asc)` tiebreak it always did. A frame that the probe
//! considered is ranked exactly where the exact scan would have ranked it.
//!
//! What a user can observe is a frame the probe never looked at. That is a real
//! behavior change, which is why the setting ships off
//! ([`crate::RecallTuning::ann_enabled`]) and why a recall served by the index
//! says so on its result ([`crate::RecallResult::used_ann_index`]) rather than
//! leaving a caller to infer it — `docs/design/adaptive-context/adaptive-context.md` §5.5 asks for
//! honesty a caller can see, not honesty in prose.
//!
//! # Freshness
//!
//! A vector written after the last build has no assignment, and it is still a
//! candidate: every probe unions in the unassigned set. Past a drift tolerance
//! ([`unassigned_tolerance`]) the index is declared **stale** and the probe
//! declines, so recall runs the exact scan rather than serving a shrinking
//! fraction of the corpus. Nothing auto-rebuilds; `stella memory index` is the
//! explicit path.
//!
//! # It does not pay off on a small store, and it does not pretend to
//!
//! The probe widens itself until the postings it will read cover the depth the
//! ranking consumes, so on a corpus small enough that
//! `probes × (n / ceil(√n))` already exceeds that floor, the widening reads
//! every centroid and the probe degenerates into the exact scan **plus** `√n`
//! centroid cosines. Measured on the fixtures in `ann/tests.rs`: at 100 vectors
//! an enabled probe costs 300 cosines against the exact scan's 290 — a ~3%
//! loss — while at 400 it costs 410 against 590 and at 1,600 it costs 763
//! against 1,790.
//!
//! That crossover is stated rather than papered over with a size threshold that
//! silently disabled the index below some number. A workspace with 100 memories
//! does not need an accelerator, and a threshold would be one more hidden rule
//! deciding which scan ran; [`crate::RecallResult::vectors_scored`] reports the
//! candidate count on every recall, so the trade is visible at the size where it
//! stops being one.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};

use crate::candidates::NODE_AS_OF;
use crate::error::ContextError;
use crate::retrieval::cosine;
use crate::store::{ContextStore, blob_to_vector, vector_to_blob};

mod kmeans;

#[cfg(test)]
#[path = "ann/tests.rs"]
mod tests;

/// How many probed candidates recall over-fetches for every candidate it
/// actually needs.
///
/// A probe that returned exactly the shortlist size would be correct and would
/// still be wrong, because two consumers downstream read *depth* rather than the
/// shortlist:
///
/// - `coverage_score` averages the top `coverage_topk` cosines, and that mean is
///   the gate between grounded recall and labeled lexical fallback (`L-C6`). A
///   probe narrow enough to miss a strong hit lowers the mean and silently moves
///   the gate — a turn would fall back to lexical search because of an index
///   width, which is exactly the kind of invisible policy change §5.5 forbids.
/// - The strongest `max_vector_seeds` hits seed the graph expansion, so a thin
///   vector list thins the graph signal too.
///
/// 4x is the same shape as
/// [`DEFAULT_MMR_CANDIDATE_MULTIPLE`](crate::DEFAULT_MMR_CANDIDATE_MULTIPLE):
/// enough that the head of the list is settled well before the consumers stop
/// reading, while still `O(√n)` in the corpus.
pub(crate) const PROBE_OVERFETCH: usize = 4;

/// Rows a probe may leave unassigned before the index counts as stale.
///
/// Two terms, because drift is two different problems at two scales. The
/// constant floor lets a workspace write a handful of memories after indexing
/// without losing acceleration for it — the common case, and re-indexing after
/// every `stella memory` write would be absurd. The proportional term is what
/// actually protects recall: once an eighth of the corpus is outside the index,
/// the "accelerated" path is scanning that eighth linearly *and* probing, so it
/// is doing more work than the exact scan while considering fewer candidates.
///
/// Crossing it is not an error. The probe declines, recall runs the exact scan,
/// and the result is byte-identical to an unindexed store — pinned by
/// `a_stale_index_falls_back_to_the_exact_scan`.
pub(crate) fn unassigned_tolerance(vector_count: u64) -> u64 {
    64 + vector_count / 8
}

/// Which centroids' postings to read, given how strongly each scored.
///
/// Kept as its own struct so the probe's SQL builder takes ids and the width
/// policy stays testable on its own.
struct ProbePlan {
    /// Centroid ids to read, strongest first.
    ids: Vec<i64>,
    /// How many centroids the index holds in total.
    total: usize,
}

/// Everything a probe needs from the query side, in one borrow.
///
/// Grouped rather than passed as six positional arguments: half of them are a
/// `&str`, an `Option<&str>` and two `usize`s, which is exactly the shape a
/// caller transposes without the compiler noticing.
pub(crate) struct ProbeRequest<'a> {
    /// The embedder fingerprint whose index and vectors may be read (`L-C2`).
    pub(crate) fingerprint: &'a str,
    /// The embedded query.
    pub(crate) query_vec: &'a [f32],
    /// The domain-scope anti-set: nodes that must never be scored.
    pub(crate) excluded: &'a HashSet<i64>,
    /// Transaction-time cutoff, threaded into [`NODE_AS_OF`].
    pub(crate) as_of: Option<&'a str>,
    /// Posting lists to read, before over-fetch widens the plan.
    pub(crate) probes: usize,
    /// The candidate floor the width must clear. See
    /// `retrieval::ann_min_candidates`.
    pub(crate) min_candidates: usize,
}

/// What a probe found, and how hard it had to look.
pub(crate) struct AnnProbe {
    /// `(node_id, cosine)` for every candidate the probe considered —
    /// the same shape [`crate::candidates::score_nodes_by_vector`] returns, so
    /// the caller's sort, coverage estimate, and fusion are unchanged.
    pub(crate) scored: Vec<(i64, f32)>,
    /// How many centroid posting lists were read.
    pub(crate) probed_centroids: usize,
    /// How many centroids the index holds.
    pub(crate) centroid_count: usize,
}

/// Which classes of index work a call performs. Mirrors
/// [`ContextCompactPolicy`](crate::ContextCompactPolicy) deliberately: the two
/// verbs sit next to each other under `stella memory`, and a second vocabulary
/// for the same three ideas would be drift.
///
/// [`Self::default`] is **empty on purpose** — neither build nor drop, so it is
/// a no-op and [`ContextStore::index_ann`] refuses it. A policy that silently
/// did nothing is the failure mode `stella stats prune` already turned into a
/// hard error.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnnIndexPolicy {
    /// Build (or rebuild) the index for the active fingerprint. A rebuild
    /// replaces the previous one wholesale rather than patching it: incremental
    /// maintenance of a k-means index means centroids that drift away from the
    /// data they were fitted to, with nothing able to say how far.
    pub build: bool,
    /// Remove the index for the active fingerprint. Recall reverts to the exact
    /// scan with no other change.
    pub drop: bool,
    /// Compute the report without writing anything: the transaction is rolled
    /// back.
    pub dry_run: bool,
}

impl AnnIndexPolicy {
    /// The policy the flagless `stella memory index` builds.
    #[must_use]
    pub fn build_only() -> Self {
        Self {
            build: true,
            ..Self::default()
        }
    }

    /// Whether this policy would do nothing whatever the store contains.
    ///
    /// [`ContextStore::index_ann`] turns this into an error rather than an empty
    /// report, for the reason
    /// [`ContextCompactPolicy::is_noop`](crate::ContextCompactPolicy::is_noop)
    /// does: "nothing to index" is a fact about the store, "nothing selected" is
    /// a mistake in the call, and the two must not read alike.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        !self.build && !self.drop
    }
}

/// What [`ContextStore::index_ann`] built or removed (or, under
/// [`AnnIndexPolicy::dry_run`], would have).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnnIndexReport {
    /// Vectors under the active fingerprint that were clustered.
    pub vectors_indexed: u64,
    /// Centroids the build produced — `ceil(√vectors_indexed)`.
    pub centroids: u64,
    /// Posting rows a rebuild or a drop removed.
    pub postings_removed: u64,
    /// Whether the index was dropped rather than built.
    pub dropped: bool,
    /// Whether this was a dry run, in which case nothing above was written.
    pub dry_run: bool,
}

/// The build watermark for one fingerprint — what
/// [`ContextStore::ann_index_state`] reads back.
///
/// `None` from that reader means "never indexed under this embedder", which is
/// a different fact from "indexed and now stale": the second is
/// [`ContextStore::ann_index_drift`], and reporting them as one number is how a
/// user ends up rebuilding an index that was never there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnIndexState {
    /// The embedder fingerprint this index was built under (`L-C2`).
    pub fingerprint: String,
    /// When the build ran (RFC-3339).
    pub built_at: String,
    /// How many vectors it clustered.
    pub vector_count: u64,
    /// How many centroids it produced.
    pub centroid_count: u64,
}

impl ContextStore {
    /// Build, rebuild, or drop the IVF index for the active fingerprint — the
    /// engine behind `stella memory index`.
    ///
    /// Everything happens in one transaction (`L-L1`), so a kill mid-build
    /// leaves the previous index intact rather than a half-clustered one that
    /// would quietly serve wrong candidates. A
    /// [`no-op policy`](AnnIndexPolicy::is_noop) is an error, not an empty
    /// report.
    ///
    /// **This is the whole corpus's vectors in memory at once** — 1 KiB each at
    /// the default 256 dims, so 5,000 memories is ~5 MiB. That is acceptable for
    /// an explicit, occasional, offline command and would not be acceptable on
    /// the per-turn path, which is exactly why recall never triggers a build.
    pub fn index_ann(&self, policy: &AnnIndexPolicy) -> Result<AnnIndexReport, ContextError> {
        if policy.is_noop() {
            return Err(ContextError::InvalidInput(
                "nothing selected to index — a policy that neither builds nor drops is a \
                 mistake in the call, not a store that needs no index"
                    .into(),
            ));
        }
        let fingerprint = self.fingerprint().id();
        let now = self.clock().now_rfc3339();
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;

        let mut report = AnnIndexReport {
            dropped: policy.drop && !policy.build,
            dry_run: policy.dry_run,
            ..AnnIndexReport::default()
        };
        report.postings_removed = clear_index(&tx, &fingerprint)?;
        if policy.build {
            let built = build_index(&tx, &fingerprint, &now)?;
            report.vectors_indexed = built.0;
            report.centroids = built.1;
        }
        if policy.dry_run {
            // `tx` drops un-committed. A dry run must leave no trace, including
            // in the watermark that says when the index was last built.
            return Ok(report);
        }
        tx.commit()?;
        Ok(report)
    }

    /// The build watermark for the active fingerprint, or `None` if no index has
    /// ever been built under it.
    pub fn ann_index_state(&self) -> Result<Option<AnnIndexState>, ContextError> {
        index_state(&self.conn(), &self.fingerprint().id())
    }

    /// How many of this fingerprint's vectors the index does not cover — the
    /// number that decides whether a probe still runs.
    ///
    /// Reported as a count against the drift tolerance rather than as a bare
    /// boolean so `stella memory index` can say *how far* a store has drifted,
    /// which is the difference between "rebuild soon" and "your recalls have
    /// silently been exact all week".
    pub fn ann_index_drift(&self) -> Result<u64, ContextError> {
        unassigned_count(&self.conn(), &self.fingerprint().id(), i64::MAX)
    }
}

/// Delete every index row for one fingerprint, returning how many postings went.
///
/// A rebuild calls this first: centroid ids are positional, so leaving the old
/// centroids in place would leave postings pointing at representatives fitted to
/// a different corpus.
fn clear_index(tx: &Connection, fingerprint: &str) -> Result<u64, ContextError> {
    let postings = tx.execute(
        "DELETE FROM ann_assignment WHERE fingerprint = ?1",
        params![fingerprint],
    )? as u64;
    tx.execute(
        "DELETE FROM ann_centroid WHERE fingerprint = ?1",
        params![fingerprint],
    )?;
    tx.execute(
        "DELETE FROM ann_index_state WHERE fingerprint = ?1",
        params![fingerprint],
    )?;
    Ok(postings)
}

/// Cluster every vector under `fingerprint` and write the postings. Returns
/// `(vectors, centroids)`.
///
/// Reads **all** vectors under the fingerprint, with no liveness filter. A
/// superseded node's content still has a vector, and a point-in-time query can
/// still ask for it; filtering here would make the index disagree with the
/// `as_of` reader it serves.
fn build_index(tx: &Connection, fingerprint: &str, now: &str) -> Result<(u64, u64), ContextError> {
    // `ORDER BY content_hash` is what makes the build deterministic: the stride
    // initialization and the float accumulation order in `kmeans` are both
    // functions of this sequence, and SQLite's unordered scan order is not
    // stable across vacuum, page splits, or a restore.
    let mut stmt = tx.prepare(
        "SELECT content_hash, vector FROM embedding
          WHERE fingerprint = ?1
          ORDER BY content_hash",
    )?;
    let rows = stmt.query_map(params![fingerprint], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    let mut hashes: Vec<String> = Vec::new();
    let mut vectors: Vec<Vec<f32>> = Vec::new();
    for row in rows {
        let (hash, blob) = row?;
        hashes.push(hash);
        vectors.push(blob_to_vector(&blob)?);
    }
    if vectors.is_empty() {
        return Ok((0, 0));
    }

    let k = kmeans::cluster_count(vectors.len());
    let clustering = kmeans::cluster(&vectors, k);
    let mut posting_counts = vec![0u64; clustering.centroids.len()];
    {
        let mut insert = tx.prepare(
            "INSERT INTO ann_assignment (content_hash, fingerprint, centroid_id)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(content_hash, fingerprint) DO UPDATE SET centroid_id = excluded.centroid_id",
        )?;
        for (hash, &centroid) in hashes.iter().zip(clustering.assignment.iter()) {
            insert.execute(params![hash, fingerprint, centroid as i64])?;
            posting_counts[centroid] += 1;
        }
    }
    {
        let mut insert = tx.prepare(
            "INSERT INTO ann_centroid (fingerprint, centroid_id, vector, posting_count)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (id, centroid) in clustering.centroids.iter().enumerate() {
            insert.execute(params![
                fingerprint,
                id as i64,
                vector_to_blob(centroid),
                posting_counts[id] as i64
            ])?;
        }
    }
    tx.execute(
        "INSERT INTO ann_index_state (fingerprint, built_at, vector_count, centroid_count)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            fingerprint,
            now,
            vectors.len() as i64,
            clustering.centroids.len() as i64
        ],
    )?;
    Ok((vectors.len() as u64, clustering.centroids.len() as u64))
}

/// The build watermark for one fingerprint.
fn index_state(
    conn: &Connection,
    fingerprint: &str,
) -> Result<Option<AnnIndexState>, ContextError> {
    let row = conn
        .query_row(
            "SELECT built_at, vector_count, centroid_count
               FROM ann_index_state WHERE fingerprint = ?1",
            params![fingerprint],
            |r| {
                Ok(AnnIndexState {
                    fingerprint: fingerprint.to_string(),
                    built_at: r.get(0)?,
                    vector_count: r.get::<_, i64>(1)?.max(0) as u64,
                    centroid_count: r.get::<_, i64>(2)?.max(0) as u64,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// How many of this fingerprint's embeddings carry no assignment, counted up to
/// `limit`.
///
/// **No vector is decoded and no cosine is evaluated here.** It is an index scan
/// over `idx_embedding_fingerprint` with a primary-key probe into
/// `ann_assignment` per row, so a stale index costs a probe nothing but B-tree
/// lookups before it declines. That residual per-row cost is the one part of the
/// accelerated path that is still linear in the corpus, and it is stated rather
/// than hidden: it is bounded by `limit`, it moves no blob across the SQLite
/// boundary, and it is roughly three orders of magnitude cheaper per row than
/// the 256-dimension dot product it replaces.
fn unassigned_count(conn: &Connection, fingerprint: &str, limit: i64) -> Result<u64, ContextError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM (
             SELECT 1 FROM embedding e
              WHERE e.fingerprint = ?1
                AND NOT EXISTS (
                        SELECT 1 FROM ann_assignment a
                         WHERE a.content_hash = e.content_hash
                           AND a.fingerprint = e.fingerprint
                    )
              LIMIT ?2
         )",
        params![fingerprint, limit],
        |r| r.get(0),
    )?;
    Ok(n.max(0) as u64)
}

/// Score the query against every centroid and choose how many posting lists to
/// read.
///
/// The width starts at the caller's `probes` and **grows** until the postings it
/// would read reach `min_candidates`. Growing rather than trusting the setting
/// is what keeps a narrow probe from moving the coverage gate: `posting_count`
/// is an upper bound on live candidates (liveness and `excluded` remove some),
/// so this over-fetches by construction — see [`PROBE_OVERFETCH`].
fn plan_probe(
    conn: &Connection,
    req: &ProbeRequest<'_>,
) -> Result<Option<ProbePlan>, ContextError> {
    let mut stmt = conn.prepare(
        "SELECT centroid_id, vector, posting_count FROM ann_centroid WHERE fingerprint = ?1",
    )?;
    let rows = stmt.query_map(params![req.fingerprint], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Vec<u8>>(1)?,
            r.get::<_, i64>(2)?.max(0) as usize,
        ))
    })?;
    // (score, centroid id, postings). Scored against the query with the same
    // `cosine` the ranking uses, so this pass is counted as recall work — it is.
    let mut scored: Vec<(f32, i64, usize)> = Vec::new();
    for row in rows {
        let (id, blob, postings) = row?;
        let centroid = blob_to_vector(&blob)?;
        scored.push((cosine(req.query_vec, &centroid), id, postings));
    }
    if scored.is_empty() {
        return Ok(None);
    }
    // Strongest first, ties on the lower centroid id — the same explicit
    // tiebreak the node ranking uses, for the same reason: a `HashMap`-free but
    // still unordered SQLite scan must not decide which postings get read.
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));

    let total = scored.len();
    let mut width = req.probes.clamp(1, total);
    let mut reachable: usize = scored.iter().take(width).map(|(_, _, n)| *n).sum();
    while width < total && reachable < req.min_candidates {
        reachable += scored[width].2;
        width += 1;
    }
    Ok(Some(ProbePlan {
        ids: scored
            .into_iter()
            .take(width)
            .map(|(_, id, _)| id)
            .collect(),
        total,
    }))
}

/// Cosine-score the candidates in the probed posting lists, plus every vector
/// the index has not seen yet — `Ok(None)` when there is no usable index and
/// the caller must run the exact scan.
///
/// The four `Ok(None)` cases are all "run the exact scan, unchanged": no
/// watermark for this fingerprint, no centroids, an index whose drift crossed
/// [`unassigned_tolerance`], and an index with zero centroids after a build over
/// an empty corpus. None of them is an error, because none of them costs
/// correctness — only speed.
pub(crate) fn probe_by_vector(
    conn: &Connection,
    req: &ProbeRequest<'_>,
    cosine_blob: impl Fn(&[f32], &[u8]) -> f32,
) -> Result<Option<AnnProbe>, ContextError> {
    let Some(state) = index_state(conn, req.fingerprint)? else {
        return Ok(None);
    };
    if state.centroid_count == 0 {
        return Ok(None);
    }
    // Staleness first, before a single cosine: a probe that is about to decline
    // must not have paid for the centroid pass to find out.
    let tolerance = unassigned_tolerance(state.vector_count);
    let unassigned = unassigned_count(conn, req.fingerprint, tolerance as i64 + 1)?;
    if unassigned > tolerance {
        return Ok(None);
    }

    let Some(plan) = plan_probe(conn, req)? else {
        return Ok(None);
    };

    let mut scored: Vec<(i64, f32)> = Vec::new();
    // One `content_hash` can fan out to several node ids, and a row written
    // after the build appears in both arms below when a rebuild raced it. Seen
    // ids are tracked so a node is scored once: a duplicate in this list becomes
    // a duplicate rank in the RRF fusion, which would score one node twice.
    let mut seen: HashSet<i64> = HashSet::new();

    // Numbered explicitly from ?3: `?1` is the `as_of` cutoff `NODE_AS_OF`
    // binds, `?2` is the fingerprint. A bare `?` would collide with `?1`.
    let placeholders = (0..plan.ids.len())
        .map(|i| format!("?{}", i + 3))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT n.id, e.vector
           FROM ann_assignment a
           JOIN embedding e
             ON e.content_hash = a.content_hash AND e.fingerprint = a.fingerprint
           JOIN node n ON n.content_hash = e.content_hash
          WHERE a.fingerprint = ?2 AND a.centroid_id IN ({placeholders}) AND {NODE_AS_OF}"
    );
    {
        let mut stmt = conn.prepare(&sql)?;
        let mut bound: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(plan.ids.len() + 2);
        bound.push(&req.as_of);
        bound.push(&req.fingerprint);
        for id in &plan.ids {
            bound.push(id);
        }
        let mut rows = stmt.query(bound.as_slice())?;
        collect_scored(&mut rows, req, &mut seen, &mut scored, &cosine_blob)?;
    }

    // The unassigned arm. Vectors written since the build have no posting, and
    // dropping them would make a memory saved five minutes ago unrecallable —
    // the single worst thing an index could do to this plane.
    {
        let mut stmt = conn.prepare(&format!(
            "SELECT n.id, e.vector
               FROM embedding e
               JOIN node n ON n.content_hash = e.content_hash
              WHERE e.fingerprint = ?2 AND {NODE_AS_OF}
                AND NOT EXISTS (
                        SELECT 1 FROM ann_assignment a
                         WHERE a.content_hash = e.content_hash
                           AND a.fingerprint = e.fingerprint
                    )"
        ))?;
        let mut rows = stmt.query(params![req.as_of, req.fingerprint])?;
        collect_scored(&mut rows, req, &mut seen, &mut scored, &cosine_blob)?;
    }

    Ok(Some(AnnProbe {
        scored,
        probed_centroids: plan.ids.len(),
        centroid_count: plan.total,
    }))
}

/// Drain one `(node_id, vector)` cursor into the scored list, applying the
/// domain-scope anti-set and the seen-id dedup.
///
/// `excluded` is applied here rather than by re-filtering afterwards, exactly as
/// [`crate::candidates::score_nodes_by_vector`] does it: an out-of-scope node is
/// never even scored.
fn collect_scored(
    rows: &mut rusqlite::Rows<'_>,
    req: &ProbeRequest<'_>,
    seen: &mut HashSet<i64>,
    out: &mut Vec<(i64, f32)>,
    cosine_blob: &impl Fn(&[f32], &[u8]) -> f32,
) -> Result<(), ContextError> {
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        if req.excluded.contains(&id) || !seen.insert(id) {
            continue;
        }
        // Borrowed straight out of the statement's row buffer — no owned Vec,
        // the same discipline the exact scan keeps.
        let blob = row.get_ref(1)?.as_blob().map_err(rusqlite::Error::from)?;
        if !blob.len().is_multiple_of(4) {
            return Err(ContextError::Corruption(format!(
                "embedding blob length {} is not a multiple of 4",
                blob.len()
            )));
        }
        out.push((id, cosine_blob(req.query_vec, blob)));
    }
    Ok(())
}
