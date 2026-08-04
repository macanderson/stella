//! Tests for the IVF accelerator.
//!
//! They fall into five groups, matching the five ways an approximate index can
//! be wrong in a way prose cannot catch: it can be nondeterministic, it can fail
//! to actually accelerate, it can lose the right answers, it can fail to get out
//! of the way when it is not usable, and it can require maintenance on the
//! operations this plane performs most (forget, restore, point-in-time).

use std::collections::HashSet;
use std::sync::Arc;

use rusqlite::params;
use tempfile::TempDir;

use super::*;
use crate::candidates::score_nodes_by_vector;
use crate::clock::FixedClock;
use crate::embed::{Embedder, HashEmbedder};
use crate::retrieval::{RecallResult, RecallTuning, cosine_blob};
use crate::store::{NodeInput, NodeKind};
use crate::writeback::ContextDelta;
use contextgraph_types::ContextQuery;

/// Deterministic filler content. Six topic families so the corpus has real
/// cluster structure to find — a corpus of near-identical strings would make
/// any index look perfect, and a corpus of pure noise would make any index look
/// broken.
fn body(i: usize) -> String {
    const TOPICS: [&str; 6] = [
        "open the sqlite connection in wal mode with foreign keys enabled",
        "render a stacked bar chart of quarterly revenue in the dashboard",
        "pack context frames to the token budget and report every drop",
        "retry the http request with exponential backoff and a jitter window",
        "parse the tree-sitter syntax tree and index every exported symbol",
        "rotate the signing key and re-encrypt the credentials at rest",
    ];
    format!("{} — note {i:06}", TOPICS[i % TOPICS.len()])
}

/// A store at `path` with `n` embedded nodes, opened with `tuning`.
async fn seed(path: &std::path::Path, n: usize, tuning: RecallTuning) -> ContextStore {
    let store = ContextStore::open_with(
        path,
        Arc::new(HashEmbedder::default()),
        FixedClock::shared(1_000),
    )
    .unwrap()
    .with_tuning(tuning);
    let mut delta = ContextDelta::new();
    for i in 0..n {
        delta = delta.with_node(
            NodeInput::new(NodeKind::Artifact, format!("note-{i}")).with_content(body(i)),
        );
    }
    if n > 0 {
        store.upsert(delta).await.unwrap();
    }
    store
}

/// Reopen the same file with different tuning — the store takes tuning at
/// construction, so this is how a test compares two rankings over one corpus.
fn reopen(path: &std::path::Path, tuning: RecallTuning) -> ContextStore {
    ContextStore::open_with(
        path,
        Arc::new(HashEmbedder::default()),
        FixedClock::shared(1_000),
    )
    .unwrap()
    .with_tuning(tuning)
}

fn ann_on() -> RecallTuning {
    RecallTuning {
        ann_enabled: true,
        ..RecallTuning::default()
    }
}

fn query(text: &str) -> ContextQuery {
    ContextQuery {
        goal: text.into(),
        query_text: Some(text.into()),
        embedding: None,
        kinds: vec![],
        anchors: vec![],
        max_frames: 5,
        max_tokens: 4000,
        as_of: None,
        representation_preferences: vec![],
    }
}

/// The observable shape of a recall — everything a caller can see EXCEPT the
/// three fields that exist to say which scan ran. Two recalls that agree here
/// are indistinguishable to any consumer of the plane.
fn observable(r: &RecallResult) -> Vec<String> {
    let mut out = vec![format!(
        "coverage={} lexical={} considered={} cut={}",
        r.coverage, r.used_lexical_fallback, r.considered, r.candidates_cut
    )];
    for f in &r.frames {
        out.push(format!(
            "{}|{}|{}|{}|{:?}",
            f.id, f.score, f.token_cost, f.title, f.content
        ));
    }
    for d in &r.dropped {
        out.push(format!("drop {}|{}|{:?}", d.id, d.token_cost, d.reason));
    }
    out
}

/// Every stored centroid, as `(centroid_id, raw vector blob, posting_count)`.
type CentroidRows = Vec<(i64, Vec<u8>, i64)>;
/// Every stored posting, as `(content_hash, centroid_id)`.
type AssignmentRows = Vec<(String, i64)>;

/// The index exactly as it sits on disk. Compared byte for byte, so a rebuild
/// that moved a single float would fail.
fn index_bytes(store: &ContextStore) -> (CentroidRows, AssignmentRows) {
    let conn = store.conn();
    let centroids = conn
        .prepare("SELECT centroid_id, vector, posting_count FROM ann_centroid ORDER BY centroid_id")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let assignments = conn
        .prepare("SELECT content_hash, centroid_id FROM ann_assignment ORDER BY content_hash")
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    (centroids, assignments)
}

// Determinism

/// §5.3 of `docs/design/adaptive-context/adaptive-context.md` requires identical inputs to
/// produce identical output. For an index built by k-means that is not a
/// formality: the textbook algorithm seeds its centroids at random, and a
/// randomly-seeded index would make "which memories can this workspace recall"
/// depend on when the index happened to be built.
///
/// Byte-identical is the assertion, not "similar" or "same cluster count" —
/// anything weaker would pass for an index that reshuffled every rebuild while
/// keeping its shape.
#[tokio::test]
async fn building_the_index_twice_is_byte_identical() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = seed(&path, 300, RecallTuning::default()).await;

    store.index_ann(&AnnIndexPolicy::build_only()).unwrap();
    let first = index_bytes(&store);
    store.index_ann(&AnnIndexPolicy::build_only()).unwrap();
    let second = index_bytes(&store);

    assert!(!first.0.is_empty(), "the build produced no centroids");
    assert_eq!(
        first.0, second.0,
        "two builds over the same rows must produce byte-identical centroid \
         blobs — a rebuild that moves them changes which memories are reachable"
    );
    assert_eq!(
        first.1, second.1,
        "two builds over the same rows must produce identical assignments"
    );
}

/// The same query against the same store must return the identical ranked list
/// every time, index or no index. Approximation is allowed to change *which*
/// candidates are considered; it is not allowed to make the answer depend on
/// the run.
#[tokio::test]
async fn repeated_probes_return_the_identical_ranked_list() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = seed(&path, 300, ann_on()).await;
    store.index_ann(&AnnIndexPolicy::build_only()).unwrap();

    let q = query("pack context frames to the token budget");
    let first = store.recall(&q).await.unwrap();
    assert!(
        first.used_ann_index,
        "the probe must have served this recall"
    );
    for round in 0..3 {
        let again = store.recall(&q).await.unwrap();
        assert_eq!(
            observable(&first),
            observable(&again),
            "recall {round} differed from the first over an unchanged store"
        );
    }
}

// Acceleration

/// The property the whole module exists for: with the index on, cosine calls
/// **per stored node** must FALL as the corpus grows.
///
/// Modeled on `recall_work_is_bounded_by_frame_count_not_corpus_size` in
/// `retrieval/tests.rs`, which pins the exact scan at *flat* per-node cost — the
/// best a linear scan can do. Sublinear means the ratio must go down, not merely
/// stay put, so that is the assertion.
///
/// It measures work, never wall clock: a timing assertion cannot tell "sublinear"
/// from "fast on this machine today", and is the kind of test that gets marked
/// flaky and deleted.
#[tokio::test]
async fn probed_cosine_cost_per_node_falls_as_the_corpus_grows() {
    // Three sizes spanning 16x. The ratio is what is asserted, so the absolute
    // sizes only have to be large enough that `ceil(√n)` centroids is a real
    // partition rather than a handful of buckets.
    const SIZES: [usize; 3] = [100, 400, 1600];
    let mut per_node: Vec<(usize, usize, f64)> = Vec::new();

    for corpus in SIZES {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        let store = seed(&path, corpus, ann_on()).await;
        store.index_ann(&AnnIndexPolicy::build_only()).unwrap();

        let q = query("pack context frames to the token budget");
        // Zeroed AFTER the build: building is an offline whole-corpus pass by
        // design, and folding it into a per-turn budget would measure the wrong
        // thing entirely.
        let _ = crate::cost_counters::take_cosine_calls();
        let result = store.recall(&q).await.unwrap();
        let calls = crate::cost_counters::take_cosine_calls();

        assert!(
            result.used_ann_index,
            "the probe must serve every size, or the ratio below compares the \
             index against the scan it replaces: corpus {corpus}"
        );
        assert!(
            !result.frames.is_empty(),
            "an accelerated recall must still return frames at corpus {corpus}"
        );
        per_node.push((corpus, calls, calls as f64 / corpus as f64));
    }

    for pair in per_node.windows(2) {
        let (small, large) = (&pair[0], &pair[1]);
        assert!(
            large.2 < small.2,
            "cosine cost per stored node must FALL as the corpus grows — \
             {:.3} at {} nodes vs {:.3} at {} nodes. A flat ratio is the linear \
             scan this index replaces: {per_node:?}",
            small.2,
            small.0,
            large.2,
            large.0
        );
    }
    // And the end-to-end claim in one number, so a regression that merely slows
    // the *rate* of improvement is still visible.
    let first = per_node[0].2;
    let last = per_node[SIZES.len() - 1].2;
    assert!(
        last < first / 2.0,
        "a 16x corpus must at least halve the per-node cosine cost: {first:.3} \
         to {last:.3} ({per_node:?})"
    );
}

// Recall quality

/// Vocabulary for the recall-quality corpus, kept apart from [`body`]'s six
/// topic families on purpose.
///
/// Six tight families would make this measurement dishonest: queries would land
/// squarely inside one cluster, every probe would find it, and the number would
/// say more about the fixture than about the index. These fragments are blended
/// four at a time instead, so documents form a *continuum* with no clean cluster
/// boundary — the case where an IVF partition actually has to make a judgment
/// call, and therefore the case worth measuring.
const FRAGMENTS: [&str; 16] = [
    "sqlite connection wal mode",
    "foreign keys pragma enforcement",
    "quarterly revenue bar chart",
    "dashboard rendering pipeline",
    "context frame token budget",
    "drop report truncation honesty",
    "exponential backoff retry jitter",
    "http request timeout ceiling",
    "tree-sitter syntax tree parse",
    "exported symbol index lookup",
    "signing key rotation schedule",
    "credential encryption at rest",
    "reciprocal rank fusion weight",
    "maximal marginal relevance diversity",
    "bitemporal supersession tombstone",
    "embedding fingerprint mismatch",
];

/// A document blending four fragments chosen by a fixed hash of its index.
///
/// A hash rather than an arithmetic stride: a stride like `(i * 7 + step) % 16`
/// has period 16, so the corpus would collapse into sixteen sets of identical
/// documents and every probe would score a perfect recall against a fixture
/// that had no near-neighbour question in it. This is still a pure function of
/// `i` — the same corpus every run — it just spreads.
fn blended(i: usize) -> String {
    let mut state = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xcbf2_9ce4_8422_2325;
    let mut parts: Vec<&str> = Vec::with_capacity(4);
    for _ in 0..4 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        parts.push(FRAGMENTS[(state % FRAGMENTS.len() as u64) as usize]);
    }
    format!("{} :: entry {i:06}", parts.join(" "))
}

/// How much of the exact top-k the probe finds, averaged over several queries.
///
/// Measured on the ranked similarity lists rather than on assembled frames, on
/// purpose: frames have been through dedup, MMR, and a token budget, so an
/// agreement measured there would be partly measuring those. The list is what
/// the index is actually responsible for.
async fn measure_recall_at_k(corpus: usize, probes: usize, k: usize) -> f64 {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = ContextStore::open_with(
        &path,
        Arc::new(HashEmbedder::default()),
        FixedClock::shared(1_000),
    )
    .unwrap();
    let mut delta = ContextDelta::new();
    for i in 0..corpus {
        delta = delta.with_node(
            NodeInput::new(NodeKind::Artifact, format!("entry-{i}")).with_content(blended(i)),
        );
    }
    store.upsert(delta).await.unwrap();
    store.index_ann(&AnnIndexPolicy::build_only()).unwrap();
    let fingerprint = store.fingerprint().id();
    let embedder = HashEmbedder::default();

    // Queries are three-fragment blends that match no document exactly, so the
    // answer is a genuine nearest-neighbour question rather than a lookup.
    let queries = [
        "sqlite connection wal mode foreign keys pragma enforcement drop report",
        "quarterly revenue bar chart dashboard rendering pipeline exported symbol",
        "context frame token budget drop report truncation honesty retry jitter",
        "exponential backoff retry jitter http request timeout ceiling wal mode",
        "tree-sitter syntax tree parse exported symbol index lookup revenue chart",
        "signing key rotation schedule credential encryption at rest token budget",
        "reciprocal rank fusion weight maximal marginal relevance diversity",
        "bitemporal supersession tombstone embedding fingerprint mismatch parse",
    ];
    let mut total = 0.0;
    for text in queries {
        let qv = embedder
            .embed(&[text.to_string()])
            .await
            .unwrap()
            .remove(0)
            .vector;
        let conn = store.conn();
        let mut exact =
            score_nodes_by_vector(&conn, &fingerprint, &qv, &HashSet::new(), None, cosine_blob)
                .unwrap();
        exact.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut probed = probe_by_vector(
            &conn,
            &ProbeRequest {
                fingerprint: &fingerprint,
                query_vec: &qv,
                excluded: &HashSet::new(),
                as_of: None,
                probes,
                min_candidates: k * PROBE_OVERFETCH,
            },
            cosine_blob,
        )
        .unwrap()
        .expect("a freshly built index must serve a probe")
        .scored;
        probed.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));

        let want: HashSet<i64> = exact.iter().take(k).map(|(id, _)| *id).collect();
        let got: HashSet<i64> = probed.iter().take(k).map(|(id, _)| *id).collect();
        total += want.intersection(&got).count() as f64 / want.len() as f64;
    }
    total / queries.len() as f64
}

/// The quality floor: over the deterministic corpus above, the probe's top-k
/// must agree with the exact scan's top-k at least this often.
///
/// **0.90, and it is asserted rather than left implicit** — an accelerator with
/// no stated recall target is one whose quality can regress to nothing without a
/// test noticing.
///
/// Why not 1.0: the index is approximate by construction, and the residual loss
/// is concentrated where it hurts least. A miss is a vector near a cluster
/// boundary, whose cosine is by definition close to that of the neighbours the
/// probe *did* return — so the candidate lost is almost always one the RRF
/// fusion would have ranked interchangeably with one that survived.
///
/// Why not lower: below roughly 0.9 the top-k mean `coverage_score` computes
/// starts to move, and that mean is the `L-C6` gate between grounded recall and
/// labeled lexical fallback. An index that quietly pushed turns into the
/// fallback arm would be changing retrieval *policy*, not just its speed.
///
/// Measured at [`crate::DEFAULT_ANN_PROBES`] over 600 vectors: **0.950**. The
/// 0.05 of headroom is deliberate, and there is no flake risk in it — the
/// corpus, the queries, and the clustering are all pure functions, so this
/// number only moves when the code does.
const MIN_RECALL_AT_K: f64 = 0.90;

/// The accuracy witness. Without it every other test here could pass over an
/// index that returned the wrong candidates quickly and consistently.
#[tokio::test]
async fn probed_top_k_agrees_with_the_exact_scan() {
    let measured = measure_recall_at_k(600, crate::DEFAULT_ANN_PROBES, 10).await;
    assert!(
        measured >= MIN_RECALL_AT_K,
        "recall@10 over a 600-vector corpus at {} probes was {measured:.3}, below \
         the {MIN_RECALL_AT_K} floor — the index is dropping candidates the exact \
         scan ranks in its top 10",
        crate::DEFAULT_ANN_PROBES
    );
}

/// Probe width is the accuracy dial, and it must actually turn. A width that
/// bought no accuracy would mean the setting is decoration and the real
/// behavior is whatever the over-fetch floor happens to force.
///
/// The two ends measured: 0.688 at 2 probes, 1.000 at 24 (of the 25 centroids a
/// 600-vector corpus builds). The upper end converging on the exact scan is the
/// half that matters — it says the only thing the approximation ever does is
/// decline to look, never mis-rank what it looked at.
#[tokio::test]
async fn widening_the_probe_never_loses_accuracy() {
    let narrow = measure_recall_at_k(600, 2, 10).await;
    let wide = measure_recall_at_k(600, 24, 10).await;
    assert!(
        wide > narrow,
        "a wider probe must buy accuracy, or the setting is decoration: \
         {narrow:.3} at 2 probes vs {wide:.3} at 24"
    );
    assert!(
        wide >= 0.99,
        "probing nearly every centroid must converge on the exact scan; got \
         {wide:.3}"
    );
}

// Falling back

/// The default path must stay the tested path. With the setting off, with no
/// index, and with a stale index, recall must return exactly what an unindexed
/// store returns — not "equivalent", identical.
#[tokio::test]
async fn an_absent_index_or_a_disabled_setting_recalls_exactly_as_before() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = seed(&path, 200, RecallTuning::default()).await;
    let q = query("pack context frames to the token budget");
    let baseline = store.recall(&q).await.unwrap();
    assert!(!baseline.used_ann_index);
    assert_eq!(
        baseline.ann_probes,
        (0, 0),
        "the exact scan probes nothing, and must say so"
    );
    assert_eq!(
        baseline.vectors_scored, 200,
        "the exact scan considers the whole live corpus"
    );
    drop(store);

    // Setting ON, no index built: the probe declines and the exact scan runs.
    let store = reopen(&path, ann_on());
    let no_index = store.recall(&q).await.unwrap();
    assert!(
        !no_index.used_ann_index,
        "there is no index to serve this recall"
    );
    assert_eq!(
        observable(&baseline),
        observable(&no_index),
        "enabling the setting without building an index must change nothing"
    );

    // Index built, setting OFF: still the exact scan.
    store.index_ann(&AnnIndexPolicy::build_only()).unwrap();
    drop(store);
    let store = reopen(&path, RecallTuning::default());
    let disabled = store.recall(&q).await.unwrap();
    assert!(!disabled.used_ann_index);
    assert_eq!(
        observable(&baseline),
        observable(&disabled),
        "a built index must be inert while the setting is off"
    );
}

/// Drift past the tolerance is not an error and not a degraded answer: the probe
/// declines and recall is exact again. The failure this forbids is an index that
/// keeps serving while covering a shrinking fraction of the corpus — which
/// would look like memories quietly becoming unrecallable as a workspace grew.
#[tokio::test]
async fn a_stale_index_falls_back_to_the_exact_scan() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = seed(&path, 200, ann_on()).await;
    store.index_ann(&AnnIndexPolicy::build_only()).unwrap();

    let q = query("pack context frames to the token budget");
    assert!(
        store.recall(&q).await.unwrap().used_ann_index,
        "the freshly built index must serve"
    );

    // Write past the tolerance: 64 + 200/8 = 89 unindexed rows are allowed.
    let tolerance = unassigned_tolerance(200);
    let mut delta = ContextDelta::new();
    for i in 0..(tolerance + 20) {
        delta = delta.with_node(
            NodeInput::new(NodeKind::Artifact, format!("late-{i}")).with_content(format!(
                "a memory written after the index was built, {i:06}"
            )),
        );
    }
    store.upsert(delta).await.unwrap();
    assert!(store.ann_index_drift().unwrap() > tolerance);

    let stale = store.recall(&q).await.unwrap();
    assert!(
        !stale.used_ann_index,
        "past the drift tolerance the probe must decline rather than serve a \
         shrinking fraction of the corpus"
    );
    drop(store);
    let exact = reopen(&path, RecallTuning::default())
        .recall(&q)
        .await
        .unwrap();
    assert_eq!(
        observable(&stale),
        observable(&exact),
        "a stale index must recall byte-identically to an unindexed store"
    );
}

/// A memory written after the build has no posting, and it must still be
/// recallable — immediately, without a rebuild. This is the correctness trap the
/// unassigned union exists for: the alternative is that the newest memory in a
/// workspace, which is the one most likely to be wanted, is the one the index
/// cannot see.
#[tokio::test]
async fn a_vector_written_after_the_build_is_still_a_candidate() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = seed(&path, 200, ann_on()).await;
    store.index_ann(&AnnIndexPolicy::build_only()).unwrap();

    store
        .upsert(
            ContextDelta::new().with_node(
                NodeInput::new(NodeKind::Memory, "the newest lesson")
                    .with_content("always rebuild the ivf index after a large import of memories"),
            ),
        )
        .await
        .unwrap();

    let result = store
        .recall(&query("rebuild the ivf index after a large import"))
        .await
        .unwrap();
    assert!(
        result.used_ann_index,
        "one unindexed row is well inside the drift tolerance"
    );
    assert!(
        result.frames.iter().any(|f| f.title == "the newest lesson"),
        "a memory written after the build must still be recallable without a \
         reindex; got {:?}",
        result.frames.iter().map(|f| &f.title).collect::<Vec<_>>()
    );
}

// Liveness needs no index maintenance

/// Forgetting a memory writes a tombstone on `node` and touches neither
/// `embedding` nor the index. The posting survives; the `JOIN node` under
/// `NODE_AS_OF` is what removes the row from the answer.
///
/// This is the property that chose IVF over HNSW, so it is asserted directly:
/// forget, recall again with the index still on and still un-rebuilt, and the
/// memory must be gone.
#[tokio::test]
async fn forgetting_a_node_needs_no_reindex() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = seed(&path, 200, ann_on()).await;
    store
        .upsert(
            ContextDelta::new().with_node(
                NodeInput::new(NodeKind::Memory, "doomed lesson")
                    .with_content("the mitochondrion is the powerhouse of the cell"),
            ),
        )
        .await
        .unwrap();
    store.index_ann(&AnnIndexPolicy::build_only()).unwrap();

    let q = query("the mitochondrion is the powerhouse of the cell");
    let before = store.recall(&q).await.unwrap();
    assert!(before.used_ann_index);
    let doomed = before
        .frames
        .iter()
        .find(|f| f.title == "doomed lesson")
        .expect("the memory is recallable before it is forgotten")
        .id
        .clone();

    assert!(store.supersede_node(&doomed).unwrap(), "the forget landed");
    // Deliberately NO rebuild here.
    let after = store.recall(&q).await.unwrap();
    assert!(
        after.used_ann_index,
        "a forget must not invalidate the index — that is the whole reason this \
         is an inverted file and not a graph"
    );
    assert!(
        !after.frames.iter().any(|f| f.id == doomed),
        "a forgotten memory must vanish from an indexed recall with no reindex"
    );

    // And the inverse, likewise free.
    assert!(store.restore_node(&doomed).unwrap());
    let restored = store.recall(&q).await.unwrap();
    assert!(restored.used_ann_index);
    assert!(
        restored.frames.iter().any(|f| f.id == doomed),
        "restore is the exact inverse, and it too needs no reindex"
    );
}

/// A point-in-time recall changes the live set with no write at all. The index
/// cannot possibly have been maintained for it — and does not need to be,
/// because the cutoff is applied by the same `NODE_AS_OF` predicate the exact
/// scan uses.
#[tokio::test]
async fn a_point_in_time_probe_needs_no_reindex() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let early = crate::clock::format_rfc3339(1_000);

    let store = seed(&path, 200, ann_on()).await;
    drop(store);
    // A second batch, later in transaction time.
    let store = ContextStore::open_with(
        &path,
        Arc::new(HashEmbedder::default()),
        FixedClock::shared(9_000),
    )
    .unwrap()
    .with_tuning(ann_on());
    store
        .upsert(
            ContextDelta::new().with_node(
                NodeInput::new(NodeKind::Memory, "late lesson")
                    .with_content("prefer an inverted file when deletes are common"),
            ),
        )
        .await
        .unwrap();
    store.index_ann(&AnnIndexPolicy::build_only()).unwrap();

    let q = query("prefer an inverted file when deletes are common");
    let now = store.recall(&q).await.unwrap();
    assert!(now.used_ann_index);
    assert!(
        now.frames.iter().any(|f| f.title == "late lesson"),
        "the late memory is visible at head"
    );

    let mut past = q.clone();
    past.as_of = Some(early);
    let then = store.recall(&past).await.unwrap();
    assert!(
        then.used_ann_index,
        "an as_of cutoff must not disable the index"
    );
    assert!(
        !then.frames.iter().any(|f| f.title == "late lesson"),
        "a memory recorded after the cutoff must not appear in a point-in-time \
         probe, with no reindex of any kind"
    );
}

/// The domain-scope anti-set is applied inside the probe, exactly as it is
/// inside the exact scan — an out-of-scope node must never even be scored, let
/// alone returned.
#[tokio::test]
async fn the_excluded_set_still_applies_to_a_probe() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = seed(&path, 200, ann_on()).await;
    store.index_ann(&AnnIndexPolicy::build_only()).unwrap();
    let fingerprint = store.fingerprint().id();

    let qv = HashEmbedder::default()
        .embed(&["pack context frames to the token budget".to_string()])
        .await
        .unwrap()
        .remove(0)
        .vector;

    let conn = store.conn();
    let open = probe_by_vector(
        &conn,
        &ProbeRequest {
            fingerprint: &fingerprint,
            query_vec: &qv,
            excluded: &HashSet::new(),
            as_of: None,
            probes: crate::DEFAULT_ANN_PROBES,
            min_candidates: 40,
        },
        cosine_blob,
    )
    .unwrap()
    .expect("index serves")
    .scored;
    assert!(!open.is_empty());

    let banned: HashSet<i64> = open.iter().take(5).map(|(id, _)| *id).collect();
    let scoped = probe_by_vector(
        &conn,
        &ProbeRequest {
            fingerprint: &fingerprint,
            query_vec: &qv,
            excluded: &banned,
            as_of: None,
            probes: crate::DEFAULT_ANN_PROBES,
            min_candidates: 40,
        },
        cosine_blob,
    )
    .unwrap()
    .expect("index serves")
    .scored;
    for (id, _) in &scoped {
        assert!(
            !banned.contains(id),
            "node {id} is in the anti-set and must never be scored"
        );
    }
}

// Lifecycle and migration

/// The engine refuses a policy that could never do anything, the way
/// `ContextStore::compact` does — "nothing to index" is a fact about the store
/// and "nothing selected" is a mistake in the call, and reading alike is how a
/// broken invocation looks like a clean run.
#[tokio::test]
async fn a_no_op_index_policy_is_a_hard_error() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = seed(&path, 10, RecallTuning::default()).await;
    let err = store.index_ann(&AnnIndexPolicy::default()).unwrap_err();
    assert!(
        matches!(err, ContextError::InvalidInput(_)),
        "unexpected error: {err:?}"
    );
}

/// A dry run reports what a build would do and leaves nothing behind —
/// including no watermark, since a watermark saying the index was built when it
/// was not is worse than no report at all.
#[tokio::test]
async fn a_dry_run_reports_without_writing() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = seed(&path, 120, ann_on()).await;

    let report = store
        .index_ann(&AnnIndexPolicy {
            build: true,
            dry_run: true,
            ..AnnIndexPolicy::default()
        })
        .unwrap();
    assert_eq!(report.vectors_indexed, 120);
    assert!(report.centroids > 0);
    assert!(report.dry_run);
    assert_eq!(
        store.ann_index_state().unwrap(),
        None,
        "a dry run must leave no watermark"
    );
    assert!(
        !store
            .recall(&query("pack context frames"))
            .await
            .unwrap()
            .used_ann_index,
        "a dry run must leave no index for a recall to find"
    );
}

/// Dropping the index reverts recall to the exact scan, and is the inverse of a
/// build in the only sense that matters: the answers come back identical.
#[tokio::test]
async fn dropping_the_index_reverts_to_the_exact_scan() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = seed(&path, 200, ann_on()).await;
    let q = query("pack context frames to the token budget");
    let exact = store.recall(&q).await.unwrap();

    store.index_ann(&AnnIndexPolicy::build_only()).unwrap();
    assert!(store.recall(&q).await.unwrap().used_ann_index);

    let report = store
        .index_ann(&AnnIndexPolicy {
            drop: true,
            ..AnnIndexPolicy::default()
        })
        .unwrap();
    assert!(report.dropped);
    assert_eq!(report.postings_removed, 200);
    assert_eq!(store.ann_index_state().unwrap(), None);

    let after = store.recall(&q).await.unwrap();
    assert!(!after.used_ann_index);
    assert_eq!(
        observable(&exact),
        observable(&after),
        "dropping the index must restore the exact scan's answers exactly"
    );
}

/// An existing v6 workspace — one that has been compacted but never seen an ANN
/// index — must migrate on open, and must keep migrating cleanly across
/// reopens. The `IF NOT EXISTS` in `MIGRATION_V7` is what makes the second open
/// a no-op; without it a store whose `user_version` was rewound by a partial
/// restore would fail to open at all.
#[tokio::test]
async fn a_v6_store_migrates_to_v7_idempotently() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    drop(crate::store::tests::open_legacy(&path, 6));

    for round in 0..3 {
        let store = ContextStore::open_with(
            &path,
            Arc::new(HashEmbedder::default()),
            FixedClock::shared(1_000),
        )
        .unwrap();
        {
            let conn = store.conn();
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                version,
                crate::store::SCHEMA_VERSION,
                "open {round} must leave the store at the current version"
            );
            for table in ["ann_centroid", "ann_assignment", "ann_index_state"] {
                let n: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                        params![table],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(n, 1, "{table} must exist after open {round}");
            }
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master \
                     WHERE type = 'index' AND name = 'idx_ann_assignment_probe'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "the probe index must exist after open {round}");
        }
        // A migrated store must still be a working store, not merely a
        // well-shaped one.
        store.integrity_check().unwrap();
        drop(store);
    }

    // And the index it never had can now be built over its rows.
    let store = seed(&path, 40, ann_on()).await;
    let report = store.index_ann(&AnnIndexPolicy::build_only()).unwrap();
    assert_eq!(report.vectors_indexed, 40);
}

/// The probe must be an index range scan. Without `idx_ann_assignment_probe`
/// SQLite would answer `centroid_id IN (…)` by scanning every posting in the
/// store — the accelerator would then read *more* rows than the exact scan it
/// replaces while considering fewer candidates.
#[tokio::test]
async fn the_probe_reads_postings_through_its_index() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = seed(&path, 40, ann_on()).await;
    let conn = store.conn();
    let plan: Vec<String> = conn
        .prepare(
            "EXPLAIN QUERY PLAN
             SELECT n.id, e.vector
               FROM ann_assignment a
               JOIN embedding e
                 ON e.content_hash = a.content_hash AND e.fingerprint = a.fingerprint
               JOIN node n ON n.content_hash = e.content_hash
              WHERE a.fingerprint = ?1 AND a.centroid_id IN (1, 2, 3)
                AND n.superseded_at IS NULL",
        )
        .unwrap()
        .query_map(params!["fp"], |r| r.get::<_, String>(3))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let plan = plan.join(" | ");
    assert!(
        plan.contains("idx_ann_assignment_probe"),
        "the probe must range-scan its own index; plan was: {plan}"
    );
}

// The clustering itself

/// `ceil(√n)` — the width that balances the centroid pass against the posting
/// reads. Pinned because both halves of the probe's cost depend on it.
#[test]
fn the_cluster_count_is_the_ceiling_of_the_square_root() {
    assert_eq!(kmeans::cluster_count(0), 0);
    assert_eq!(kmeans::cluster_count(1), 1);
    assert_eq!(kmeans::cluster_count(4), 2);
    assert_eq!(kmeans::cluster_count(5), 3);
    assert_eq!(kmeans::cluster_count(10_000), 100);
    assert_eq!(kmeans::cluster_count(10_001), 101);
}

/// Every vector gets a cluster, including the zero vector that empty content
/// projects to. A vector the clustering dropped would be a memory no probe
/// could ever return, and nothing downstream would report its absence.
#[test]
fn every_vector_lands_in_a_cluster_including_the_zero_vector() {
    let mut vectors: Vec<Vec<f32>> = (0..20)
        .map(|i| {
            let mut v = vec![0.0f32; 8];
            v[i % 8] = 1.0;
            v
        })
        .collect();
    vectors.push(vec![0.0f32; 8]);

    let clustering = kmeans::cluster(&vectors, kmeans::cluster_count(vectors.len()));
    assert_eq!(clustering.assignment.len(), vectors.len());
    for (i, &c) in clustering.assignment.iter().enumerate() {
        assert!(
            c < clustering.centroids.len(),
            "vector {i} was assigned to centroid {c}, which does not exist"
        );
    }
    assert_eq!(
        *clustering.assignment.last().unwrap(),
        0,
        "the zero vector ties against every centroid, and the documented \
         tiebreak sends a tie to the lowest centroid id"
    );
}

/// Clustering is a pure function of its input — no clock, no seed, no ambient
/// state. Asserted on the floats directly rather than through the database, so
/// a failure points at the algorithm rather than at the storage layer.
#[test]
fn clustering_is_a_pure_function_of_its_input() {
    let vectors: Vec<Vec<f32>> = (0..64)
        .map(|i| {
            let mut v = vec![0.0f32; 16];
            v[i % 16] = 1.0;
            v[(i * 7) % 16] += 0.5;
            crate::embed::l2_normalize(&mut v);
            v
        })
        .collect();
    let k = kmeans::cluster_count(vectors.len());
    let a = kmeans::cluster(&vectors, k);
    let b = kmeans::cluster(&vectors, k);
    assert_eq!(a.centroids, b.centroids);
    assert_eq!(a.assignment, b.assignment);
}
