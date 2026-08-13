// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for `search`, in the crate so they exercise the shipped internals
//! rather than a re-implementation — the discipline `crate::graph::semantic`
//! already follows.
//!
//! The witness is [`one_search_call_answers_what_today_takes_several_tools`].
//! Everything else covers a way the tool can be wrong quietly: a fallback
//! that does not say it fell back, a depth rung that takes something away,
//! and a truncation nobody is told about.

use std::fs;
use std::path::Path;

use async_trait::async_trait;
use stella_core::search::{Depth, allocate, facets_at};
use stella_embed::{EmbedError, Embedder, EmbedderFingerprint, Embedding, SimilarityPosture};
use stella_graph::CodeGraph;
use stella_protocol::tool::ToolOutput;

use super::{
    ChunkWarmOutcome, Hit, Search, SearchConfig, Strategy, dispatch, render, warm_chunk_vectors,
};
use crate::registry::Tool;

/// The question the witness asks. Every word of it is checked against the
/// answer's path and body before the search runs — see the witness itself.
const QUESTION: &str = "removing sensitive credentials before they reach the log";

/// The file the question is about. Its name and body share no word with
/// [`QUESTION`] — asserted by the witness before it searches.
const ANSWER: &str = "src/hkey.rs";

/// The file a *lexical* method answers with instead.
///
/// It is stuffed with the question's own surface words (`removing`, `before`,
/// `they reach`, `the`) while being about queue arithmetic, so any strategy
/// that matches text rather than meaning ranks it first. Without this decoy
/// the fixture is only three files wide and a surface embedder can land on
/// the right one by luck — which it did, in a run of this suite, while the
/// witness reported success. A witness that passes for the wrong reason is
/// worse than no witness, and this file is what makes the assertion
/// load-bearing. `a_surface_embedder_cannot_answer_the_same_question` pins it.
const DECOY: &str = "src/reaching.rs";

/// The fixture: four files, one of which is the answer, and whose name and
/// body share no word with [`QUESTION`].
const FIXTURE: &[(&str, &str)] = &[
    (
        DECOY,
        "//! Removing an element before they reach the end of the queue.\n\
         pub struct Queue;\n\
         pub fn removing_before_they_reach_the_end(queue: &mut Queue) {\n\
         queue.reach_the_end();\n\
         queue.removing_before();\n\
         }\n",
    ),
    (
        "src/hkey.rs",
        "//! Keeps confidential material out of emitted diagnostics.\n\
         pub struct Scrubber;\n\
         pub fn scrub_record(record: &mut Record) {\n\
         record.password.clear();\n\
         record.secret_token.clear();\n\
         }\n",
    ),
    (
        "src/wire.rs",
        "//! HTTP request plumbing.\n\
         pub struct Socket;\n\
         pub fn send_request(socket: &Socket) -> Response {\n\
         socket.write_header();\n\
         socket.flush()\n\
         }\n",
    ),
    (
        "src/tally.rs",
        "//! Matrix arithmetic.\n\
         pub struct Matrix;\n\
         pub fn multiply(a: &Matrix, b: &Matrix) -> Matrix {\n\
         a.rows().sum(b)\n\
         }\n",
    ),
];

/// The four concepts the fixture separates on.
const LEXICON: &[(&str, usize)] = &[
    ("secret", 0),
    ("password", 0),
    ("credential", 0),
    ("confidential", 0),
    ("sensitive", 0),
    ("scrub", 0),
    ("redact", 0),
    ("log", 1),
    ("diagnostic", 1),
    ("emit", 1),
    ("http", 2),
    ("request", 2),
    ("socket", 2),
    ("header", 2),
    ("wire", 2),
    ("matrix", 3),
    ("multiply", 3),
    ("arithmetic", 3),
    ("sum", 3),
    ("row", 3),
];

const DIMS: usize = 4;

/// A deterministic, offline embedder that is genuinely *semantic*: different
/// surface words map to the same dimension, which is the whole property under
/// test. Deliberately not `HashEmbedder` — a hashing projection would make
/// the fixture surface-solvable and the witness would prove nothing.
///
/// Same shape as `stella-graph`'s `semantic_recall.rs::ConceptEmbedder`, so
/// the two suites agree on what "semantic" means here.
#[derive(Debug)]
struct ConceptEmbedder;

impl ConceptEmbedder {
    fn project(text: &str) -> Vec<f32> {
        let mut accumulator = vec![0.0f32; DIMS];
        for word in text
            .to_lowercase()
            .split(|c: char| !c.is_alphabetic())
            .filter(|word| !word.is_empty())
        {
            for (term, dimension) in LEXICON {
                if word.contains(term) {
                    accumulator[*dimension] += 1.0;
                }
            }
        }
        stella_embed::l2_normalize(&mut accumulator);
        accumulator
    }
}

#[async_trait]
impl Embedder for ConceptEmbedder {
    fn fingerprint(&self) -> EmbedderFingerprint {
        EmbedderFingerprint {
            model_id: "concept-lexicon".into(),
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
            .map(|text| Embedding {
                fingerprint: fingerprint.clone(),
                vector: Self::project(text),
            })
            .collect())
    }

    fn similarity_posture(&self) -> SimilarityPosture {
        SimilarityPosture::Semantic {
            admission_floor: 0.2,
        }
    }
}

/// Write the fixture and index it. Returns the canonical root and an open
/// graph; the caller shuts the graph down.
fn indexed_fixture(workspace: &Path) -> (std::path::PathBuf, CodeGraph) {
    for (path, body) in FIXTURE {
        let file = workspace.join(path);
        fs::create_dir_all(file.parent().expect("a parent")).expect("mkdir");
        fs::write(&file, body).expect("write");
    }
    let root = workspace.canonicalize().expect("canonicalize");
    let graph = CodeGraph::open(&root, &root.join("codegraph.db")).expect("open");
    graph.index_all().expect("index");
    (root, graph)
}

/// Same fixture, indexed at the real `.stella/private/codegraph.db` path
/// [`crate::graph::open_or_build`] resolves — what `warm_chunk_vectors` and
/// every production caller actually open, unlike [`indexed_fixture`]'s
/// `<root>/codegraph.db`, which only the tests calling `dispatch`/`render`
/// directly ever touch. Closes the graph before returning so the caller's own
/// connection (opened fresh, matching `stella init`'s real sequencing) is not
/// racing an already-open one.
fn write_fixture_at_the_real_workspace_path(workspace: &Path) -> std::path::PathBuf {
    for (path, body) in FIXTURE {
        let file = workspace.join(path);
        fs::create_dir_all(file.parent().expect("a parent")).expect("mkdir");
        fs::write(&file, body).expect("write");
    }
    let root = workspace.canonicalize().expect("canonicalize");
    let opened = crate::graph::open_or_build(&root).expect("open_or_build");
    opened.graph.shutdown();
    root
}

fn content_of(output: &ToolOutput) -> String {
    match output {
        ToolOutput::Ok { content } => content.clone(),
        ToolOutput::Error { message, .. } => panic!("expected an answer, got an error: {message}"),
    }
}

/// **The witness.**
///
/// It proves the claim the whole change rests on, in the only way that
/// distinguishes it from its opposite: one `search` call returns the file
/// that answers a plain-English question *whose words appear nowhere in that
/// file's name or body* — and returns it already carrying the structure that
/// today costs a `graph_query` and a `read_symbol` on top of the grep that
/// would have failed anyway.
///
/// The fixture's own premise is asserted first. A fixture that drifts into
/// being grep-solvable would let this test pass while proving nothing, so
/// every word of the question is checked against both the answer's path and
/// its body, and the test fails loudly rather than vacuously.
#[tokio::test]
async fn one_search_call_answers_what_today_takes_several_tools() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = indexed_fixture(workspace.path());

    // The premise that makes this test worth anything: no lexical route
    // exists from the question to the answer. If either assertion fires, the
    // fixture has become solvable by grep and the test below proves nothing.
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
    // callers — the graph structure that a grep cannot express and that the
    // model would otherwise pay separate calls for.
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
        "the hit did not carry its symbols — a second tool call would still be needed: {content}"
    );
    assert!(
        content.contains("signature:"),
        "the hit did not carry a signature: {content}"
    );
    assert!(
        content.contains("ranked by MEANING"),
        "the answer did not say why the file ranked: {content}"
    );

    // Deterministic: the identical query renders identically, which is what
    // invariant 7 needs from output that reaches the prompt.
    let again = dispatch(Some(&graph), &root, QUESTION, Some(&ConceptEmbedder)).await;
    assert_eq!(answer.hits, again.hits);

    graph.shutdown();
}

/// The control that makes the witness above mean something.
///
/// A *surface* backend — one that compares text shape rather than meaning —
/// must NOT reach the answer, or the witness is measuring a coincidence. This
/// test exists because an earlier version of the fixture was small enough
/// that `HashEmbedder` landed on the right file by luck and the witness went
/// green anyway. It is the same control `stella-graph`'s
/// `semantic_recall.rs::the_lexical_embedder_cannot_answer_the_same_question`
/// runs, pointed at this tool's dispatch, and it fails the moment the fixture
/// drifts back into being surface-solvable.
#[tokio::test]
async fn a_surface_embedder_cannot_answer_the_same_question() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = indexed_fixture(workspace.path());

    let surface = stella_embed::HashEmbedder::default();
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
/// term-matching rung rather than the exact one (#3125). An exact name takes
/// the shorter route now, which is
/// [`an_exact_name_without_an_embedder_is_still_a_certainty`]'s subject — this
/// test is about the rung that guesses.
#[tokio::test]
async fn without_an_embedder_the_answer_is_useful_and_says_it_is_a_name_match() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = indexed_fixture(workspace.path());

    // `scrub` is a fragment of `scrub_record`, not an exact symbol, so the
    // certainty rung stays out and the name rung is what answers.
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

/// A workspace the indexer cannot serve — no tree-sitter grammar, so an empty
/// graph — is the normal case on Terminal-Bench. It must still answer, and
/// still say what it did.
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
    let (root, graph) = indexed_fixture(workspace.path());
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
/// whose absence sends the agent back for the `read_file` this tool exists
/// to replace. Before the allocator's promotion pass and the depth default
/// moving to MAX, the default answer stopped at `imports:` and left ninety
/// percent of the budget unspent while promising "source already attached".
#[tokio::test]
async fn the_default_answer_carries_the_body_without_a_follow_up_read() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = indexed_fixture(workspace.path());

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
        "the default answer carried no signature — a follow-up call would be needed: {content}"
    );
    assert!(
        content.contains("body of `"),
        "the default answer carried no source body — the read_file this tool replaces would \
         still be paid: {content}"
    );
    assert!(
        content.contains("scrub_record"),
        "the top hit lost its symbols at the default depth: {content}"
    );

    graph.shutdown();
}

/// A file-scan that hit its walk cap must say so: on an unindexed tree
/// bigger than the cap, a silent miss reads as absence — the exact failure
/// the module docs argue against.
#[test]
fn a_capped_scan_discloses_what_it_never_saw() {
    let workspace = tempfile::tempdir().expect("tempdir");
    for index in 0..6 {
        fs::write(
            workspace.path().join(format!("file{index}.txt")),
            "nothing relevant\n",
        )
        .expect("write");
    }
    fs::write(workspace.path().join("aaa.txt"), "credential rotation\n").expect("write");
    let root = workspace.path().canonicalize().expect("canonicalize");

    let capped = super::scan::scan_hits_bounded(&root, "credential rotation", 10, 3);
    assert!(
        capped.exhausted,
        "a walk over 7 files with a 3-file cap must report exhaustion"
    );

    let full = super::scan::scan_hits_bounded(&root, "credential rotation", 10, 100);
    assert!(
        !full.exhausted,
        "a walk that saw the whole tree must not claim exhaustion"
    );
    assert_eq!(full.matched, 1);
    assert_eq!(
        full.hits.first().map(|hit| hit.path.as_str()),
        Some("aaa.txt")
    );
}

/// The capped walk is shallow-first: the files most likely to matter — the
/// root's own — are inside the cap, not past it. The old stack-based walk
/// descended the alphabetically last subtree first, so a capped scan kept
/// exactly the least useful corner of the tree.
#[test]
fn a_capped_scan_sees_the_root_before_the_leaves() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let deep = workspace.path().join("zzz").join("deeper");
    fs::create_dir_all(&deep).expect("mkdir");
    for index in 0..4 {
        fs::write(deep.join(format!("noise{index}.txt")), "rotation\n").expect("write");
    }
    fs::write(workspace.path().join("rotation.txt"), "rotation\n").expect("write");
    let root = workspace.path().canonicalize().expect("canonicalize");

    let capped = super::scan::scan_hits_bounded(&root, "rotation", 10, 1);
    assert_eq!(
        capped.hits.first().map(|hit| hit.path.as_str()),
        Some("rotation.txt"),
        "a 1-file cap must spend its budget at the root, not in the deepest subtree: {:?}",
        capped.hits
    );
}

/// A name-match answer wider than the shown list must disclose the cut:
/// "10 results" over 14 matching files reads as "only 10 matched".
#[tokio::test]
async fn a_cut_name_match_list_discloses_how_many_matched() {
    let workspace = tempfile::tempdir().expect("tempdir");
    for index in 0..14 {
        let file = workspace.path().join(format!("src/widget_{index:02}.rs"));
        fs::create_dir_all(file.parent().expect("a parent")).expect("mkdir");
        fs::write(&file, format!("pub fn widget_{index:02}() {{}}\n")).expect("write");
    }
    let root = workspace.path().canonicalize().expect("canonicalize");
    let graph = CodeGraph::open(&root, &root.join("codegraph.db")).expect("open");
    graph.index_all().expect("index");

    let answer = dispatch(Some(&graph), &root, "widget", None).await;
    assert_eq!(answer.strategies, vec![Strategy::GraphNames]);
    assert_eq!(answer.hits.len(), 10, "the shown list stays capped");
    let note = answer.note.expect("a cut list must carry a note");
    assert!(
        note.contains("14 files matched"),
        "the note must say how many matched: {note}"
    );

    graph.shutdown();
}

/// A misconfigured embedder must not eat the dispatch's own disclosure: over
/// a 14-match corpus whose name rung cuts its list, the one note line carries
/// **both** the cut ("14 files matched") and the misconfiguration — the
/// assignment this replaces kept only the newest caveat.
#[tokio::test]
async fn a_misconfigured_embedder_note_joins_the_cut_list_note_instead_of_replacing_it() {
    let workspace = tempfile::tempdir().expect("tempdir");
    for index in 0..14 {
        let file = workspace.path().join(format!("src/widget_{index:02}.rs"));
        fs::create_dir_all(file.parent().expect("a parent")).expect("mkdir");
        fs::write(&file, format!("pub fn widget_{index:02}() {{}}\n")).expect("write");
    }
    let root = workspace.path().canonicalize().expect("canonicalize");

    let report = super::report_with(
        &root,
        "widget",
        SearchConfig::default(),
        stella_embed::Resolution::Incomplete("STELLA_EMBED_MODEL is unset".into()),
    )
    .await;

    let note = report.note.expect("both caveats need a note to live in");
    assert!(
        note.contains("14 files matched"),
        "the cut-list disclosure must survive the misconfig note: {note}"
    );
    assert!(
        note.contains("misconfigured"),
        "the misconfiguration must still be said: {note}"
    );
}

/// The ranking's memory guard is disclosed when it fills — the promise
/// `RANK_CEILING`'s doc makes ("reported, never silent") held nowhere in the
/// code before this test existed.
#[test]
fn a_full_candidate_list_earns_a_ceiling_note() {
    assert!(super::ceiling_note(super::RANK_CEILING, 0).is_some());
    assert!(super::ceiling_note(0, super::RANK_CEILING).is_some());
    assert_eq!(super::ceiling_note(5, 7), None);
}

/// Two caveats must both survive into the one note line.
#[test]
fn merging_notes_loses_neither() {
    assert_eq!(
        super::merge_notes(Some("first".into()), Some("second".into())),
        Some("first; second".into())
    );
    assert_eq!(
        super::merge_notes(None, Some("only".into())),
        Some("only".into())
    );
    assert_eq!(
        super::merge_notes(Some("only".into()), None),
        Some("only".into())
    );
    assert_eq!(super::merge_notes(None, None), None);
}

/// An attribute between a declaration and its doc comment is not prose: the
/// quoted doc must skip `#[must_use]` and still reach the real comment above
/// it.
#[test]
fn a_doc_comment_walk_skips_attributes_instead_of_quoting_them() {
    let source = "/// Keeps confidential material out of diagnostics.\n\
                  #[must_use]\n\
                  pub fn scrub() {}\n";
    let symbol = stella_graph::NeighborhoodSymbol {
        name: "scrub".into(),
        kind: "function".into(),
        start_line: 3,
    };
    let doc = super::enrich::doc_comment(Some(source), &symbol).expect("a doc line");
    assert!(
        !doc.contains("must_use"),
        "the attribute was quoted as prose: {doc}"
    );
    assert!(
        doc.contains("confidential material"),
        "the real doc comment above the attribute was lost: {doc}"
    );
}

/// **The #3125 witness.** An exact symbol name must not lose to semantic
/// ranking. The embedder here scores every file identically, so the semantic
/// rung returns everything with ties broken by path — exactly the shape that
/// buried the one definition of `ContextFrame` at rank 5 of 139 on a real
/// corpus. Only the exact rung puts the defining file first, and the `via:`
/// line must name both contributors.
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
/// query MATCHED, not whatever sits first in the file. Before `Hit::focus`,
/// a chunk hit on the third function in a file rendered the body of an
/// unrelated one-line struct above it — the answer's most expensive facet
/// spent on its least relevant content.
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

/// A caller entry is a located citation (`fn x (path:line)`), never the
/// site-less `caller of X` placeholder that `ContextFrame::title` carries —
/// every caller deduplicated into one entry naming no file, which told the
/// model nothing it could follow.
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

    let block = super::enrich::render_hit(
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
        block.contains("callers:"),
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

/// A budget too small for every hit must say so. An agent that cannot tell a
/// complete answer from a truncated one stops looking, which is the exact
/// failure this line prevents.
#[test]
fn a_truncated_answer_says_it_was_truncated() {
    let hits: Vec<Hit> = (0..6)
        .map(|index| Hit {
            path: format!("src/file{index}.rs"),
            why: "ranked by MEANING against your query (cosine 0.900)".into(),
            focus: None,
        })
        .collect();
    let answer = super::Answer {
        hits,
        strategies: vec![Strategy::Semantic],
        note: None,
    };

    let content = content_of(&render(
        None,
        Path::new("/nonexistent"),
        "anything",
        &answer,
        SearchConfig {
            depth: Depth::MIN,
            budget: 130,
        },
    ));
    assert!(
        content.contains("TRUNCATED"),
        "a truncated answer did not say so: {content}"
    );
    assert!(
        content.contains("further result(s) were dropped"),
        "the truncation line did not say what was dropped: {content}"
    );

    // The complement: given room, the same answer must NOT claim truncation.
    let roomy = content_of(&render(
        None,
        Path::new("/nonexistent"),
        "anything",
        &answer,
        SearchConfig {
            depth: Depth::MIN,
            budget: 100_000,
        },
    ));
    assert!(
        !roomy.contains("TRUNCATED"),
        "an untruncated answer claimed truncation: {roomy}"
    );
}

/// The schema is the whole user interface, so its promises are pinned:
/// one required argument, no mode selector (invariant 9), and a description
/// that corrects the lexical reading of the name.
#[test]
fn the_schema_offers_one_query_and_no_mode_selector() {
    let schema = Search::default().schema();
    assert_eq!(schema.name, "search");
    assert!(schema.read_only);
    assert!(!schema.speculation_safe, "the graph open writes (#923)");

    let properties = schema.input_schema["properties"]
        .as_object()
        .expect("an object schema");
    assert_eq!(
        properties.keys().collect::<Vec<_>>(),
        vec!["query"],
        "a second parameter is a mode selector waiting to happen (invariant 9)"
    );
    assert_eq!(schema.input_schema["required"][0], "query");

    let description = &schema.description;
    assert!(
        description.contains("MEANING"),
        "the description must correct the lexical reading of the name `search`"
    );
    assert!(
        description.contains("search(\""),
        "the description must show worked argument shapes"
    );
    for shape in ["where are request headers", "CredentialStore"] {
        assert!(
            description.contains(shape),
            "the description lost the `{shape}` example — it must show BOTH a described \
             behaviour and a named symbol"
        );
    }
}

/// The dial is configuration, never a tool argument, and the advertised
/// schema must not move when it changes — tool schemas ride at position 0 of
/// the cached prefix (invariant 7).
#[test]
fn the_depth_dial_never_reaches_the_advertised_schema() {
    let shallow = Search::with_config(SearchConfig {
        depth: Depth::MIN,
        budget: 1_000,
    })
    .schema();
    let deep = Search::with_config(SearchConfig {
        depth: Depth::MAX,
        budget: 900_000,
    })
    .schema();
    assert_eq!(
        serde_json::to_string(&shallow).expect("serialize"),
        serde_json::to_string(&deep).expect("serialize"),
        "the depth setting changed the advertised schema, so a sweep would cost the prompt cache"
    );
}

/// The allocator and the renderer must agree about how many hits were shown;
/// they are separately written and a drift between them would silently drop
/// results with no truncation line.
#[test]
fn the_rendered_hit_count_matches_the_allocation() {
    let hits: Vec<Hit> = (0..7)
        .map(|index| Hit {
            path: format!("src/file{index}.rs"),
            why: "matched 1 query term(s)".into(),
            focus: None,
        })
        .collect();
    let answer = super::Answer {
        hits: hits.clone(),
        strategies: vec![Strategy::FileScan],
        note: None,
    };
    let config = SearchConfig {
        depth: Depth::new(3),
        budget: 900,
    };
    let allocation = allocate(hits.len(), config.depth, config.budget);
    let content = content_of(&render(
        None,
        Path::new("/nonexistent"),
        "anything",
        &answer,
        config,
    ));

    let shown = hits
        .iter()
        .filter(|hit| content.contains(&hit.path))
        .count();
    assert_eq!(shown, allocation.granted.len());
    assert_eq!(allocation.omitted, hits.len() - shown);
    assert!(facets_at(config.depth).len() >= 3);
}

/// The witness for the eager `stella init` chunk pass (#3098): a fixture
/// wider than one lazy per-query window (`MAX_FILES_PER_CHUNK_PASS`, 64)
/// would take several `search` calls to fully cover — this proves one
/// `warm_chunk_vectors` call finishes it, so `search` can rank by meaning on
/// its very first invocation rather than the ~25th on a repository this
/// crate's own size.
#[tokio::test]
async fn one_eager_pass_embeds_every_pending_chunk_no_matter_how_many_files() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = write_fixture_at_the_real_workspace_path(workspace.path());

    let outcome = warm_chunk_vectors(&root, &ConceptEmbedder, 1_000).await;
    let ChunkWarmOutcome::Warmed {
        files_embedded,
        files_remaining,
        unreadable,
    } = outcome
    else {
        panic!("expected Warmed, got {outcome:?}");
    };
    assert_eq!(
        files_embedded,
        FIXTURE.len(),
        "every fixture file has symbols to chunk"
    );
    assert_eq!(files_remaining, 0, "nothing left pending after one pass");
    assert_eq!(unreadable, 0);

    // Idempotent: a second pass over an already-warm index embeds nothing new.
    let again = warm_chunk_vectors(&root, &ConceptEmbedder, 1_000).await;
    assert_eq!(
        again,
        ChunkWarmOutcome::Warmed {
            files_embedded: 0,
            files_remaining: 0,
            unreadable: 0,
        },
        "re-running against a fully warm index must be a no-op, not a re-embed"
    );
}

/// A capped pass says honestly what it left behind, rather than reporting
/// success over a partial index — the same discipline
/// `crate::graph::semantic::WarmOutcome` holds for whole-file vectors.
#[tokio::test]
async fn a_capped_pass_reports_what_it_left_pending() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = write_fixture_at_the_real_workspace_path(workspace.path());

    // FIXTURE has 4 files; cap the pass at fewer files than that.
    let outcome = warm_chunk_vectors(&root, &ConceptEmbedder, 2).await;
    let ChunkWarmOutcome::Warmed {
        files_embedded,
        files_remaining,
        ..
    } = outcome
    else {
        panic!("expected Warmed, got {outcome:?}");
    };
    assert_eq!(files_embedded, 2, "the pass must stop exactly at its cap");
    assert!(
        files_remaining > 0,
        "a capped pass over a wider fixture must say something is still pending"
    );
}

/// #3128: two symbols that render byte-identical text collapse to one stored
/// row, so the pending-files pre-filter's raw symbol-count-vs-stored-count
/// comparison can never reach equality for that file — without the "no
/// progress" early exit, `warm_chunk_vectors` would re-select and re-stamp it
/// on every round until its cap, never returning promptly even once every
/// real symbol is embedded. This is the witness for that early exit.
#[tokio::test]
async fn a_file_whose_symbols_collide_on_rendered_text_does_not_spin_the_pass() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = {
        let file = workspace.path().join("src/dupes.rs");
        fs::create_dir_all(file.parent().expect("a parent")).expect("mkdir");
        // Two distinct symbols, same name, same kind, byte-identical body —
        // ordinary trait-stub-across-fixtures shape, not a contrived one.
        fs::write(
            &file,
            "mod a {\n    pub fn execute() { todo!() }\n}\n\
             mod b {\n    pub fn execute() { todo!() }\n}\n",
        )
        .expect("write");
        let root = workspace.path().canonicalize().expect("canonicalize");
        let opened = crate::graph::open_or_build(&root).expect("open_or_build");
        opened.graph.shutdown();
        root
    };

    // First pass: real embedding work happens (however few distinct chunks
    // there are), and the pass must still terminate rather than hang.
    let outcome = warm_chunk_vectors(&root, &ConceptEmbedder, 1_000).await;
    assert!(
        matches!(outcome, ChunkWarmOutcome::Warmed { .. }),
        "expected Warmed, got {outcome:?}"
    );

    // Second pass over the now-embedded fixture: with the collision, the
    // pre-filter still selects `src/dupes.rs` (symbol count 2 > stored rows
    // 1), so without the early exit this would spend its entire `limit`
    // re-visiting it. It must instead return quickly, having made no further
    // progress, and must not report a growing `files_embedded` each time.
    let again = warm_chunk_vectors(&root, &ConceptEmbedder, 1_000).await;
    let ChunkWarmOutcome::Warmed { files_embedded, .. } = again else {
        panic!("expected Warmed, got {again:?}");
    };
    assert!(
        files_embedded <= stella_graph::MAX_FILES_PER_CHUNK_PASS,
        "a fully-covered fixture must stop after at most one window, not spin \
         toward the cap: files_embedded={files_embedded}"
    );
}

/// **The witness for #3124.** A workspace with nothing left to embed must stop
/// describing itself as partial.
///
/// It is the same collision as #3128, read by a different consumer: the note
/// compared `embedded_chunk_count` against `symbol_count`, and chunk rows are
/// keyed on the hash of the rendered chunk, so the two byte-identical bodies
/// below store one row for two symbols and that comparison can never hold
/// again. Measured on a 158-file workspace, coverage froze at 1,810 of 1,814
/// and every search carried the warning — whose own advice is to go back to
/// `grep`, the call this module exists to remove.
///
/// The premise is asserted first, so this cannot pass vacuously: if the fixture
/// ever stops colliding, the old comparison would have succeeded too and the
/// test would be proving nothing.
#[tokio::test]
async fn a_fully_embedded_workspace_stops_calling_itself_partial() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let file = workspace.path().join("src/accessors.rs");
    fs::create_dir_all(file.parent().expect("a parent")).expect("mkdir");
    // Two distinct symbols, byte-identical rendered text — the ordinary
    // trait-accessor shape, not a contrived one.
    fs::write(
        &file,
        "mod a {\n    pub fn id() -> u8 { 0 }\n}\n\
         mod b {\n    pub fn id() -> u8 { 0 }\n}\n",
    )
    .expect("write");
    let root = workspace.path().canonicalize().expect("canonicalize");
    crate::graph::open_or_build(&root)
        .expect("open_or_build")
        .graph
        .shutdown();

    let fingerprint = ConceptEmbedder.fingerprint().id();
    let warmed = warm_chunk_vectors(&root, &ConceptEmbedder, 1_000).await;
    assert!(
        matches!(warmed, ChunkWarmOutcome::Warmed { .. }),
        "expected Warmed, got {warmed:?}"
    );

    let opened = crate::graph::open_or_build(&root).expect("open_or_build");
    super::catch_up_embeddings(&opened.graph, &ConceptEmbedder, &fingerprint)
        .await
        .expect("whole-file vectors");

    // The premise: the collision really happened, so the count comparison this
    // note used to make is unsatisfiable for this workspace.
    let symbols = opened.graph.symbol_count().expect("symbol_count");
    let chunks = opened
        .graph
        .embedded_chunk_count(&fingerprint)
        .expect("embedded_chunk_count");
    assert!(
        chunks < symbols,
        "fixture no longer collides ({chunks} chunk rows for {symbols} symbols) — the old \
         `chunks >= symbols` test would have passed and this witness proves nothing"
    );

    let note = super::coverage_note(&opened.graph, &fingerprint);
    opened.graph.shutdown();
    assert!(
        note.is_none(),
        "a workspace with nothing left to embed still calls itself partial: {note:?}"
    );
}

/// The other half of the same contract: a workspace that genuinely has work
/// left must still say so. Without this, #3124 could be "fixed" by never
/// warning at all, which loses the disclosure #3117 added.
#[tokio::test]
async fn a_workspace_with_work_left_still_says_it_is_partial() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = write_fixture_at_the_real_workspace_path(workspace.path());

    // Indexed, nothing embedded at all — the state right after `stella init`
    // on a build with no embedder configured.
    let opened = crate::graph::open_or_build(&root).expect("open_or_build");
    let note = super::coverage_note(&opened.graph, &ConceptEmbedder.fingerprint().id());
    opened.graph.shutdown();

    let note = note.expect("an unembedded workspace must disclose that it is partial");
    assert!(note.contains("PARTIAL INDEX"), "{note}");
}

/// The blank-query refusal lives in `report` — one validation both doors
/// share — and the agent's door still surfaces it. This pins the refusal
/// from the `execute` side so moving the check cannot silently lose it.
#[tokio::test]
async fn a_blank_query_is_refused_through_the_tool_door() {
    let workspace = tempfile::TempDir::new().unwrap();
    let output = Search::default()
        .execute(&serde_json::json!({"query": "   "}), workspace.path())
        .await;
    let ToolOutput::Error { message, .. } = output else {
        panic!("a blank query must be an error, got: {output:?}");
    };
    assert!(message.contains("`query` is required"), "{message}");
    assert!(
        !workspace.path().join(".stella").exists(),
        "a refused query must not create workspace state as a side effect"
    );
}

/// The #3144 witness: a mistyped `query` is a type mismatch, not a missing
/// field. On main, `{"query": 42}` fell through `as_str().unwrap_or_default()`
/// into the blank-query refusal — the model was told the field it sent was
/// missing, and retried against the wrong defect.
#[tokio::test]
async fn a_mistyped_query_is_refused_as_a_type_mismatch_not_as_missing() {
    let workspace = tempfile::TempDir::new().unwrap();
    let output = Search::default()
        .execute(&serde_json::json!({"query": 42}), workspace.path())
        .await;
    let ToolOutput::Error { message, .. } = output else {
        panic!("a mistyped query must be an error, got: {output:?}");
    };
    assert_eq!(message, "field `query` must be a string, got number");
    assert!(
        !message.contains("required"),
        "a present-but-mistyped field is not missing: {message}"
    );
}

/// `SearchReport` is the CLI's `--format json` contract: the decided answer
/// as data beside the rendering. Hit order is rank order, strategies carry
/// the exact labels the `via:` line prints, and the note survives verbatim.
#[test]
fn the_report_preserves_rank_order_and_strategy_labels() {
    let answer = super::Answer {
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
    };
    let report = super::SearchReport::of(&answer, rendered.clone());

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

/// **The witness for #3125.** A query that exactly names an indexed symbol
/// returns that symbol's defining file first, even when embedding rank puts
/// something else on top.
///
/// The fixture is built so semantic ranking is *wrong on purpose* and provably
/// so: the decoy's chunk contains only dimension-2 words, so it projects onto
/// the query's own direction and scores 1.0, while the definition's chunk
/// carries a dimension-3 word from its body and scores lower. On `main` the
/// ladder returns at the semantic rung and the decoy is rank 1.
///
/// Measured cause: asked for `ContextFrame` — one definition in its repository
/// — the semantic-only ladder returned it at rank 5 of 139, because the graph's
/// exact-name rung is unreachable whenever semantic returns anything, and an
/// admission floor of 0.25 against a corpus whose lowest cosine is 0.418 means
/// it always does.
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

    // The premise: the ranking really does prefer the decoy. Without this the
    // test could pass on `main` by luck and prove nothing.
    let (ranking, _) = super::semantic_hits(&graph, &ConceptEmbedder, "socket_registry_of")
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
/// On a workspace with no embedding backend — the common case on
/// Terminal-Bench — a symbol name should still be answered with a fact rather
/// than with a term-overlap guess.
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
    let (root, graph) = indexed_fixture(workspace.path());

    let answer = dispatch(Some(&graph), &root, QUESTION, Some(&ConceptEmbedder)).await;
    graph.shutdown();
    assert_eq!(
        answer.strategies,
        vec![Strategy::Semantic],
        "a multi-word question must not engage the exact-symbol rung"
    );
}
