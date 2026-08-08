//! Store-level witnesses for the evidence gate (#2289).
//!
//! The scenario throughout is the incident quoted on
//! [`super::DEFAULT_RECENCY_WEIGHT`]: a workspace whose store holds five stale
//! notes about deleting test files, asked about something else entirely. On
//! the old behavior those five filled every recall slot on every prompt —
//! `max_frames` was a cap that always filled — and the witness stage went
//! looking for test files to delete. These tests pin the new contract: a
//! frame is recalled iff something ties it to *this* query, and a recall
//! where nothing qualifies returns **zero frames**.
//!
//! Each test forces the arm it means to exercise through `min_coverage`
//! (0.0 pins the fused arm, 1.1-clamped-to-1.0 pins the lexical fallback),
//! because which arm runs depends on the hash embedder's incidental trigram
//! coverage and a witness must not.

use std::sync::Arc;

use tempfile::TempDir;

use crate::clock::FixedClock;
use crate::embed::{
    EmbedError, Embedder, EmbedderFingerprint, Embedding, HashEmbedder, SimilarityPosture,
};
use crate::retrieval::RecallTuning;
use crate::store::{ContextStore, NodeInput, NodeKind};
use crate::writeback::{ContextDelta, FactAssertion};
use contextgraph_types::ContextQuery;

/// The five stale notes. Every one shares glue ("the", "from") with any
/// English prompt and none shares a distinctive term with [`QUERY`].
const CONTAMINANTS: [&str; 5] = [
    "remove the obsolete unit test files from the fixtures tree",
    "delete the stale golden snapshot files under the fixtures tree",
    "prune the disused integration test files and their helpers",
    "sweep the abandoned property test files out of the crate",
    "clear the orphaned example files from the samples tree",
];

/// An unrelated prompt: shares "the" (glue, df = N) with every contaminant
/// and no distinctive term with any of them.
const QUERY: &str = "restore the deck keybinding for scrollback paging";

fn base_query(goal: &str) -> ContextQuery {
    ContextQuery {
        goal: goal.into(),
        query_text: Some(goal.into()),
        embedding: None,
        kinds: vec![],
        anchors: vec![],
        max_frames: 10,
        max_tokens: 4000,
        as_of: None,
        representation_preferences: vec![],
    }
}

fn tuning(min_coverage: f32, require_evidence: bool) -> RecallTuning {
    RecallTuning {
        min_coverage,
        require_evidence,
        ..RecallTuning::default()
    }
}

async fn contaminated_store(t: RecallTuning) -> (TempDir, ContextStore) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = ContextStore::open_with(
        &path,
        Arc::new(HashEmbedder::default()),
        FixedClock::shared(1_000),
    )
    .unwrap()
    .with_tuning(t);
    let mut delta = ContextDelta::new();
    for (i, text) in CONTAMINANTS.iter().enumerate() {
        delta = delta
            .with_node(NodeInput::new(NodeKind::Artifact, format!("note-{i}")).with_content(*text));
    }
    store.upsert(delta).await.unwrap();
    (dir, store)
}

/// THE witness: five live nodes, an unrelated prompt, and the recall comes
/// back **empty** — where the old behavior filled all five slots with them,
/// on this prompt and every other, forever. The refusals are counted, not
/// silent (`L-C5`).
#[tokio::test]
async fn recall_abstains_when_nothing_ties_the_corpus_to_the_query() {
    let (_dir, store) = contaminated_store(tuning(0.0, true)).await;
    let result = store.recall(&base_query(QUERY)).await.unwrap();
    assert!(
        result.frames.is_empty(),
        "nothing in the store relates to the prompt, so nothing may be \
         recalled; got {} frames",
        result.frames.len()
    );
    assert_eq!(
        result.no_evidence_cut, 5,
        "the refusals are reported, never silent"
    );
}

/// The positive control: add one genuinely related note and the gate admits
/// exactly it — abstention is not blindness.
#[tokio::test]
async fn recall_admits_exactly_the_relevant_node() {
    let (_dir, store) = contaminated_store(tuning(0.0, true)).await;
    store
        .upsert(
            ContextDelta::new().with_node(
                NodeInput::new(NodeKind::Artifact, "deck-lesson")
                    .with_content("the deck keybinding map moved into the scrollback paging state"),
            ),
        )
        .await
        .unwrap();
    let result = store.recall(&base_query(QUERY)).await.unwrap();
    assert_eq!(
        result.frames.len(),
        1,
        "exactly the related note is admitted"
    );
    assert_eq!(result.frames[0].title, "deck-lesson");
    assert_eq!(result.no_evidence_cut, 5, "the contaminants were refused");
}

/// The gated lexical fallback: on a weak-coverage turn the fallback arm used
/// to admit any node matching any term — and every contaminant matches
/// "the". Now a glue-only match is refused there too, and a distinctive one
/// still gets through.
#[tokio::test]
async fn the_lexical_fallback_is_gated_too() {
    let (_dir, store) = contaminated_store(tuning(1.0, true)).await;
    let empty = store.recall(&base_query(QUERY)).await.unwrap();
    assert!(empty.used_lexical_fallback, "the arm this test pins");
    assert!(empty.frames.is_empty(), "glue-only matches admit nothing");
    assert_eq!(empty.no_evidence_cut, 5);

    store
        .upsert(
            ContextDelta::new().with_node(
                NodeInput::new(NodeKind::Artifact, "deck-lesson")
                    .with_content("the deck keybinding map for scrollback paging"),
            ),
        )
        .await
        .unwrap();
    let one = store.recall(&base_query(QUERY)).await.unwrap();
    assert!(one.used_lexical_fallback);
    assert_eq!(one.frames.len(), 1);
    assert_eq!(one.frames[0].title, "deck-lesson");
}

/// The escape hatch: `require_evidence: false` restores the pre-#2289
/// padding byte-for-byte — all five contaminants fill the budget again, and
/// the gate reports nothing because it judged nothing.
#[tokio::test]
async fn the_escape_hatch_restores_the_old_padding() {
    let (_dir, store) = contaminated_store(tuning(0.0, false)).await;
    let result = store.recall(&base_query(QUERY)).await.unwrap();
    assert_eq!(
        result.frames.len(),
        5,
        "with the gate off, the budget fills with whatever exists"
    );
    assert_eq!(result.no_evidence_cut, 0);
}

/// An anchor is evidence with no lexical overlap at all: the user named the
/// file, and that outranks every heuristic. Its 1-hop graph neighborhood is
/// grounded by the same naming.
#[tokio::test]
async fn anchors_and_their_neighbors_are_admitted_without_term_overlap() {
    let (_dir, store) = contaminated_store(tuning(0.0, true)).await;
    store
        .upsert(
            ContextDelta::new().with_fact(FactAssertion::new(
                NodeInput::new(NodeKind::File, "src/render_grid.rs")
                    .with_uri("file:///repo/src/render_grid.rs")
                    .with_content("owns the double buffer swap cadence"),
                "EXPLAINS",
                NodeInput::new(NodeKind::Concept, "flicker-lesson")
                    .with_content("the flicker came from swapping buffers mid pass"),
            )),
        )
        .await
        .unwrap();
    let mut q = base_query(QUERY);
    q.anchors = vec!["file:///repo/src/render_grid.rs".into()];
    let result = store.recall(&q).await.unwrap();
    let titles: Vec<&str> = result.frames.iter().map(|f| f.title.as_str()).collect();
    assert!(
        titles.contains(&"src/render_grid.rs"),
        "the anchored file is admitted: {titles:?}"
    );
    assert!(
        titles.contains(&"flicker-lesson"),
        "its 1-hop neighbor is admitted: {titles:?}"
    );
    assert_eq!(
        result.frames.len(),
        2,
        "and the contaminants still are not: {titles:?}"
    );
}

/// An embedder that maps every text to the same unit vector and declares
/// [`SimilarityPosture::Semantic`] — the shape a real ONNX/API backend
/// (#2288) will take. Under it, cosine alone admits; the same corpus that
/// [`recall_abstains_when_nothing_ties_the_corpus_to_the_query`] refused
/// under `Surface` is served.
struct ConstEmbedder;

#[async_trait::async_trait]
impl Embedder for ConstEmbedder {
    fn fingerprint(&self) -> EmbedderFingerprint {
        EmbedderFingerprint {
            model_id: "const-test".to_string(),
            revision: "1".to_string(),
            dims: 4,
            normalization: "l2".to_string(),
        }
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, EmbedError> {
        if texts.is_empty() {
            return Err(EmbedError::EmptyInput);
        }
        let fingerprint = self.fingerprint().id();
        Ok(texts
            .iter()
            .map(|_| Embedding {
                fingerprint: fingerprint.clone(),
                vector: vec![1.0, 0.0, 0.0, 0.0],
            })
            .collect())
    }

    fn similarity_posture(&self) -> SimilarityPosture {
        SimilarityPosture::Semantic {
            admission_floor: 0.5,
        }
    }
}

#[tokio::test]
async fn a_semantic_posture_admits_by_cosine_alone() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let store = ContextStore::open_with(&path, Arc::new(ConstEmbedder), FixedClock::shared(1_000))
        .unwrap()
        .with_tuning(tuning(0.0, true));
    let mut delta = ContextDelta::new();
    for (i, text) in CONTAMINANTS.iter().enumerate() {
        delta = delta
            .with_node(NodeInput::new(NodeKind::Artifact, format!("note-{i}")).with_content(*text));
    }
    store.upsert(delta).await.unwrap();

    let result = store.recall(&base_query(QUERY)).await.unwrap();
    assert_eq!(
        result.frames.len(),
        5,
        "every node clears the declared floor (cosine 1.0 ≥ 0.5), so the \
         semantic channel admits them all"
    );
    assert_eq!(result.no_evidence_cut, 0);
}
