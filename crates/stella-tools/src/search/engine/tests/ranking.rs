//! Exact-symbol ranking: it outranks the semantic guess, leads the facets, keeps callers cited, and survives the report/JSON round trip (#4494 split of `../tests.rs`).
use super::*;

/// **The #3125 witness, uniform-tie shape.** An exact symbol name must not
/// lose to semantic ranking. The embedder here scores every file identically,
/// so the semantic rung returns everything with ties broken by path — only
/// the exact rung puts the defining file first, and the `via:` line must name
/// both contributors.
#[tokio::test]
async fn an_exact_symbol_name_outranks_the_semantic_guess() {
    /// Scores everything at cosine 1.0 — the admission floor rejects
    /// nothing, so the semantic rung can never come back empty.
    #[derive(Debug)]
    struct UniformEmbedder;

    #[async_trait]
    impl Embedder for UniformEmbedder {
        fn fingerprint(&self) -> EmbedderFingerprint {
            EmbedderFingerprint {
                model_id: "uniform".into(),
                revision: "1".into(),
                dims: DIMS,
                normalization: "l2".into(),
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
                admission_floor: 0.2,
            }
        }
    }

    let workspace = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(workspace.path().join("src")).expect("mkdir");
    // `aaa.rs` sorts first, so the all-tied semantic ranking leads with it;
    // only the exact rung can put the actual definition first.
    fs::write(
        workspace.path().join("src/aaa.rs"),
        "pub fn unrelated() {}\n",
    )
    .expect("write");
    fs::write(
        workspace.path().join("src/zzz.rs"),
        "pub fn uniquely_named_widget() {}\n",
    )
    .expect("write");
    let root = workspace.path().canonicalize().expect("canonicalize");
    let graph = CodeGraph::open(&root, &root.join("codegraph.db")).expect("open");
    graph.index_all().expect("index");

    let answer = dispatch(
        Some(&graph),
        &root,
        "uniquely_named_widget",
        Some(&UniformEmbedder),
    )
    .await;
    let top = answer.hits.first().expect("a hit");
    assert_eq!(
        top.path, "src/zzz.rs",
        "the defining file must be rank 1, not wherever the tie-break put it: {:?}",
        answer.hits
    );
    assert!(
        top.why.contains("EXACT name match"),
        "the pinned hit must say why it leads: {}",
        top.why
    );
    assert_eq!(
        answer.strategies,
        vec![Strategy::ExactSymbol, Strategy::Semantic],
        "via: must name both contributors"
    );

    graph.shutdown();
}

/// The body and signature the answer pays for must describe the symbol the
/// query MATCHED, not whatever sits first in the file.
#[tokio::test]
async fn the_matched_symbol_leads_the_detailed_facets() {
    let workspace = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(workspace.path().join("src")).expect("mkdir");
    // The first symbol shares no concept with the query; the second is the
    // match. Only a focus-aware renderer quotes the second's body.
    fs::write(
        workspace.path().join("src/mixed.rs"),
        "pub fn matrix_multiply_rows() {\n\
         let sum = 1;\n\
         }\n\
         pub fn scrub_credentials() {\n\
         let password = ();\n\
         let secret = ();\n\
         }\n",
    )
    .expect("write");
    let root = workspace.path().canonicalize().expect("canonicalize");
    let graph = CodeGraph::open(&root, &root.join("codegraph.db")).expect("open");
    graph.index_all().expect("index");
    crate::search::backfill::backfill_opened(&graph, &ConceptEmbedder, &mut |_| {}).await;

    let answer = dispatch(
        Some(&graph),
        &root,
        "sensitive credentials",
        Some(&ConceptEmbedder),
    )
    .await;
    let top = answer.hits.first().expect("a hit");
    assert_eq!(top.path, "src/mixed.rs");
    assert_eq!(
        top.focus.as_deref(),
        Some("scrub_credentials"),
        "the chunk that ranked must name the symbol it matched: {:?}",
        answer.hits
    );

    let content = content_of(&render(
        Some(&graph),
        &root,
        "sensitive credentials",
        &answer,
        SearchConfig::default(),
    ));
    assert!(
        content.contains("body of `scrub_credentials`"),
        "the body facet quoted the wrong symbol: {content}"
    );

    graph.shutdown();
}

/// A caller entry is a located citation (`fn x (path:line)`), never a
/// site-less `caller of X` placeholder — every caller deduplicated into one
/// entry naming no file says nothing a reader can follow.
#[tokio::test]
async fn caller_entries_carry_their_site_not_a_placeholder() {
    let workspace = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(workspace.path().join("src")).expect("mkdir");
    fs::write(
        workspace.path().join("src/callee.rs"),
        "pub fn target_fn() {}\n",
    )
    .expect("write");
    fs::write(
        workspace.path().join("src/caller.rs"),
        "pub fn calling_site() {\n    target_fn();\n}\n",
    )
    .expect("write");
    let root = workspace.path().canonicalize().expect("canonicalize");
    let graph = CodeGraph::open(&root, &root.join("codegraph.db")).expect("open");
    graph.index_all().expect("index");

    let block = enrich::render_hit(
        Some(&graph),
        &root,
        &Hit {
            path: "src/callee.rs".into(),
            why: "fixture".into(),
            focus: None,
        },
        Depth::new(8),
    );
    assert!(
        block.contains("callers (by name):"),
        "no callers line rendered: {block}"
    );
    assert!(
        block.contains("src/caller.rs"),
        "the caller entry must cite its site: {block}"
    );
    assert!(
        !block.contains("caller of "),
        "the placeholder title leaked into the answer: {block}"
    );

    graph.shutdown();
}

/// `SearchReport` is the `--format json` contract: the decided answer as
/// data beside the rendering. Hit order is rank order, strategies carry the
/// exact labels the `via:` line prints, and the note survives verbatim.
#[test]
fn the_report_preserves_rank_order_and_strategy_labels() {
    let answer = Answer {
        hits: vec![
            Hit {
                path: "src/first.rs".into(),
                why: "the top hit".into(),
                focus: None,
            },
            Hit {
                path: "src/second.rs".into(),
                why: "the runner-up".into(),
                focus: None,
            },
        ],
        strategies: vec![Strategy::Semantic, Strategy::GraphNames],
        note: Some("degraded: the backend hiccuped".into()),
    };
    let rendered = ToolOutput::Ok {
        content: "the rendered answer".into(),
        data: None,
    };
    let report = SearchReport::of(&answer, rendered.clone());

    let paths: Vec<&str> = report.hits.iter().map(|hit| hit.path.as_str()).collect();
    assert_eq!(paths, ["src/first.rs", "src/second.rs"]);
    assert_eq!(report.hits[0].why, "the top hit");
    assert_eq!(
        report.strategies,
        [Strategy::Semantic.label(), Strategy::GraphNames.label()]
    );
    assert_eq!(
        report.note.as_deref(),
        Some("degraded: the backend hiccuped")
    );
    assert_eq!(report.rendered, rendered);
}

/// **The witness for #3125, wrong-on-purpose ranking shape.** A query that
/// exactly names an indexed symbol returns that symbol's defining file
/// first, even when embedding rank puts something else on top.
///
/// The fixture is built so semantic ranking is *wrong on purpose* and
/// provably so: the decoy's chunk contains only dimension-2 words, so it
/// projects onto the query's own direction and scores 1.0, while the
/// definition's chunk carries a dimension-3 word from its body and scores
/// lower.
#[tokio::test]
async fn an_exact_symbol_name_beats_the_ranking_that_would_bury_it() {
    let workspace = tempfile::tempdir().expect("tempdir");
    for (path, body) in [
        // The definition. Its body pulls it toward another dimension, so it
        // cannot win the ranking on its own.
        (
            "src/holder.rs",
            "pub fn socket_registry_of(matrix: &Matrix) -> u8 {\n\
             matrix.rows().sum();\n\
             0\n\
             }\n",
        ),
        // The decoy: nothing but dimension-2 words, so it scores 1.0 against
        // this query and outranks the definition every time.
        (
            "src/wirehub.rs",
            "//! socket header request http wire\n\
             pub fn send_socket_header_request() {\n\
             socket_header_request_wire();\n\
             }\n",
        ),
    ] {
        let file = workspace.path().join(path);
        fs::create_dir_all(file.parent().expect("a parent")).expect("mkdir");
        fs::write(&file, body).expect("write");
    }
    let root = workspace.path().canonicalize().expect("canonicalize");
    let graph = CodeGraph::open(&root, &root.join("codegraph.db")).expect("open");
    graph.index_all().expect("index");
    crate::search::backfill::backfill_opened(&graph, &ConceptEmbedder, &mut |_| {}).await;

    // The premise: the ranking really does prefer the decoy. Without this the
    // test could pass by luck and prove nothing.
    let (ranking, _) = semantic_hits(&graph, &ConceptEmbedder, "socket_registry_of")
        .await
        .expect("the fixture must rank");
    assert_eq!(
        ranking.first().map(|hit| hit.path.as_str()),
        Some("src/wirehub.rs"),
        "the decoy must outrank the definition semantically, or this witness proves nothing: {ranking:?}"
    );

    let answer = dispatch(
        Some(&graph),
        &root,
        "socket_registry_of",
        Some(&ConceptEmbedder),
    )
    .await;
    graph.shutdown();

    assert_eq!(
        answer.hits.first().map(|hit| hit.path.as_str()),
        Some("src/holder.rs"),
        "the defining file must lead: {:?}",
        answer.hits
    );
    assert_eq!(
        answer.strategies,
        vec![Strategy::ExactSymbol, Strategy::Semantic],
        "`via:` must name both rungs, never silently skip one"
    );
    assert!(
        answer.hits[0].why.contains("EXACT"),
        "the leading hit must say it is a fact rather than a score: {:?}",
        answer.hits[0]
    );
    assert!(
        answer
            .hits
            .iter()
            .filter(|h| h.path == "src/holder.rs")
            .count()
            == 1,
        "a file reached by both rungs must be reported once: {:?}",
        answer.hits
    );
}

/// The exact rung needs no embedder: it reads the graph, not a vector index.
/// On a workspace with no embedding backend, a symbol name should still be
/// answered with a fact rather than with a term-overlap guess.
#[tokio::test]
async fn an_exact_name_without_an_embedder_is_still_a_certainty() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = indexed_fixture(workspace.path());

    let answer = dispatch(Some(&graph), &root, "scrub_record", None).await;
    graph.shutdown();
    assert_eq!(answer.strategies, vec![Strategy::ExactSymbol]);
    assert_eq!(
        answer.hits.first().map(|hit| hit.path.as_str()),
        Some("src/hkey.rs"),
        "{:?}",
        answer.hits
    );
}

/// A question is not a symbol, so the exact rung must stay out of the way —
/// otherwise every sentence-shaped search pays a graph lookup that can only
/// ever miss, and `via:` starts naming a strategy that did nothing.
#[tokio::test]
async fn a_sentence_never_reaches_the_exact_symbol_rung() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = embedded_fixture(workspace.path(), &ConceptEmbedder).await;

    let answer = dispatch(Some(&graph), &root, QUESTION, Some(&ConceptEmbedder)).await;
    graph.shutdown();
    assert_eq!(
        answer.strategies,
        vec![Strategy::Semantic],
        "a multi-word question must not engage the exact-symbol rung"
    );
}
