//! The witness and its ladder-shape controls: no embedder, no index, depth monotonicity, and the default rung's own contract (#4494 split of `../tests.rs`).
use super::*;

/// **The witness.**
///
/// It proves the claim the whole ladder rests on, in the only way that
/// distinguishes it from its opposite: one search returns the file that
/// answers a plain-English question *whose words appear nowhere in that
/// file's name or body* — and returns it already carrying the structure that
/// would otherwise cost separate lookups on top of the grep that would have
/// failed anyway.
///
/// The fixture's own premise is asserted first. A fixture that drifts into
/// being grep-solvable would let this test pass while proving nothing, so
/// every word of the question is checked against both the answer's path and
/// its body, and the test fails loudly rather than vacuously.
#[tokio::test]
async fn one_search_call_answers_what_today_takes_several_tools() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = embedded_fixture(workspace.path(), &ConceptEmbedder).await;

    // The premise that makes this test worth anything: no lexical route
    // exists from the question to the answer.
    let answer_path = ANSWER;
    let body = fs::read_to_string(root.join(answer_path))
        .expect("read")
        .to_lowercase();
    for word in QUESTION.split_whitespace() {
        assert!(
            !answer_path.contains(word),
            "the question word `{word}` appears in the answer's path — the fixture would be \
             solvable by a filename match and proves nothing"
        );
        assert!(
            !body.contains(word),
            "the question word `{word}` appears in the answer's body — grep would find it and \
             this test proves nothing"
        );
    }

    let answer = dispatch(Some(&graph), &root, QUESTION, Some(&ConceptEmbedder)).await;
    assert_eq!(
        answer.strategies,
        vec![Strategy::Semantic],
        "the semantic strategy must be the one that answered; note={:?}",
        answer.note
    );
    assert_eq!(
        answer.hits.first().map(|hit| hit.path.as_str()),
        Some(answer_path),
        "expected the redaction file first; got {:?}",
        answer.hits
    );

    // Depth 8 buys symbols, kinds, imports, importers, signature, doc and
    // callers — the graph structure a grep cannot express.
    let output = render(
        Some(&graph),
        &root,
        QUESTION,
        &answer,
        SearchConfig {
            depth: Depth::new(8),
            budget: 200_000,
        },
    );
    let content = content_of(&output);
    assert!(
        content.contains(answer_path),
        "no answer path in: {content}"
    );
    assert!(
        content.contains("scrub_record"),
        "the hit did not carry its symbols — a second lookup would still be needed: {content}"
    );
    assert!(
        content.contains("signature:"),
        "the hit did not carry a signature: {content}"
    );
    assert!(
        content.contains("ranked by MEANING"),
        "the answer did not say why the file ranked: {content}"
    );

    // Deterministic: the identical query ranks identically, which is what
    // invariant 7 needs from output that reaches a prompt.
    let again = dispatch(Some(&graph), &root, QUESTION, Some(&ConceptEmbedder)).await;
    assert_eq!(answer.hits, again.hits);

    graph.shutdown();
}

/// The control that makes the witness above mean something.
///
/// A *surface* backend — one that compares text shape rather than meaning —
/// must NOT reach the answer, or the witness is measuring a coincidence. It
/// fails the moment the fixture drifts back into being surface-solvable.
#[tokio::test]
async fn a_surface_embedder_cannot_answer_the_same_question() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let surface = stella_embed::HashEmbedder::default();
    // Warmed under the SURFACE backend's own fingerprint: vectors are keyed
    // by it, so warming with the concept embedder would leave this ranking
    // nothing to read and the control would pass for the wrong reason.
    let (root, graph) = embedded_fixture(workspace.path(), &surface).await;

    assert_eq!(
        surface.similarity_posture(),
        SimilarityPosture::Surface,
        "this control is only meaningful against a backend that declares itself surface-only"
    );

    let answer = dispatch(Some(&graph), &root, QUESTION, Some(&surface)).await;
    assert_eq!(answer.strategies, vec![Strategy::Semantic]);
    assert_ne!(
        answer.hits.first().map(|hit| hit.path.as_str()),
        Some(ANSWER),
        "a surface embedder reached the answer, so the witness proves nothing about MEANING — \
         the fixture has become surface-solvable and needs a stronger decoy than {DECOY}"
    );

    graph.shutdown();
}

/// With no embedder the answer must still be useful, and must say which
/// strategy produced it — a name match wearing a meaning match's clothes is
/// worse than no answer.
///
/// The query is a *term*, not a symbol name: `scrub` appears inside
/// `scrub_record` and `Scrubber` but names neither, so it reaches the
/// term-matching rung rather than the exact one (#3125).
#[tokio::test]
async fn without_an_embedder_the_answer_is_useful_and_says_it_is_a_name_match() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = indexed_fixture(workspace.path());

    let answer = dispatch(Some(&graph), &root, "scrub", None).await;
    assert_eq!(answer.strategies, vec![Strategy::GraphNames]);
    assert_eq!(
        answer.hits.first().map(|hit| hit.path.as_str()),
        Some("src/hkey.rs"),
        "the name strategy must still find a symbol by name: {:?}",
        answer.hits
    );

    let content = content_of(&render(
        Some(&graph),
        &root,
        "scrub",
        &answer,
        SearchConfig::default(),
    ));
    assert!(
        content.contains("symbol NAMES (not by meaning)"),
        "the answer did not label itself a name match: {content}"
    );
    assert!(
        content.contains("via: names"),
        "the answer did not name the strategy that ran: {content}"
    );

    graph.shutdown();
}

/// A workspace the indexer cannot serve — no tree-sitter grammar, so an
/// empty graph — must still answer, and still say what it did.
#[tokio::test]
async fn with_no_index_at_all_the_file_scan_answers_and_labels_itself() {
    let workspace = tempfile::tempdir().expect("tempdir");
    fs::write(
        workspace.path().join("deploy.conf"),
        "# rotate the credential bundle nightly\nrotation = nightly\n",
    )
    .expect("write");
    let root = workspace.path().canonicalize().expect("canonicalize");

    let answer = dispatch(None, &root, "credential rotation", None).await;
    assert_eq!(answer.strategies, vec![Strategy::FileScan]);
    assert_eq!(
        answer.hits.first().map(|hit| hit.path.as_str()),
        Some("deploy.conf"),
        "the scan found nothing in an unindexable workspace: {:?}",
        answer.hits
    );

    let content = content_of(&render(
        None,
        &root,
        "credential rotation",
        &answer,
        SearchConfig::default(),
    ));
    assert!(
        content.contains("no index was available"),
        "the scan answer did not label itself: {content}"
    );
    assert!(
        content.contains("via: scan"),
        "the scan answer did not name its strategy: {content}"
    );
}

/// Depth monotonicity, on the rendered text rather than only on the facet
/// set: every line the shallower block emitted must survive into the deeper
/// one. A rung that reformatted a lower rung's line would make a depth sweep
/// uninterpretable without failing anything else.
#[tokio::test]
async fn each_depth_renders_a_superset_of_the_one_below() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = embedded_fixture(workspace.path(), &ConceptEmbedder).await;
    let answer = dispatch(Some(&graph), &root, QUESTION, Some(&ConceptEmbedder)).await;

    // The hit blocks only. The two header lines are deliberately excluded:
    // they *report* the depth ("… at depth 3"), so they are expected to
    // differ between rungs, and asserting over them would test the report
    // rather than the ladder.
    let block_at = |level: u8| {
        let content = content_of(&render(
            Some(&graph),
            &root,
            QUESTION,
            &answer,
            SearchConfig {
                depth: Depth::new(level),
                budget: 200_000,
            },
        ));
        content
            .lines()
            .skip_while(|line| !line.starts_with("via: "))
            .skip(1)
            .map(str::to_string)
            .collect::<Vec<_>>()
    };

    for level in 1..Depth::MAX.level() {
        let shallow = block_at(level);
        let deep = block_at(level + 1);
        assert!(!shallow.is_empty(), "depth {level} rendered no hit block");
        for line in &shallow {
            assert!(
                deep.contains(line),
                "depth {} lost the line `{line}` that depth {level} rendered",
                level + 1
            );
        }
        assert!(
            deep.len() >= shallow.len(),
            "depth {} rendered fewer lines than depth {level}",
            level + 1
        );
    }

    graph.shutdown();
}

/// **The default-configuration witness.** With no dial turned, the top hit
/// must arrive carrying its signature and source body — the exact facets
/// whose absence sends the reader back to open the file this answer exists
/// to replace.
#[tokio::test]
async fn the_default_answer_carries_the_body_without_a_follow_up_read() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = embedded_fixture(workspace.path(), &ConceptEmbedder).await;

    let answer = dispatch(Some(&graph), &root, QUESTION, Some(&ConceptEmbedder)).await;
    let content = content_of(&render(
        Some(&graph),
        &root,
        QUESTION,
        &answer,
        SearchConfig::default(),
    ));
    assert!(
        content.contains("signature:"),
        "the default answer carried no signature — a follow-up would be needed: {content}"
    );
    assert!(
        content.contains("body of `"),
        "the default answer carried no source body: {content}"
    );
    assert!(
        content.contains("scrub_record"),
        "the top hit lost its symbols at the default depth: {content}"
    );

    graph.shutdown();
}
