//! Tests for the search ladder, driven against the shipped internals rather
//! than a re-implementation.
//!
//! The witness is [`ladder::one_search_call_answers_what_today_takes_several_tools`].
//! Everything else covers a way the search can be wrong quietly: a fallback
//! that does not say it fell back, a depth rung that takes something away,
//! and a truncation nobody is told about.
//!
//! This file holds the fixture every topic below shares — the four-file
//! [`FIXTURE`], [`ConceptEmbedder`]/[`BrokenEmbedder`], and the
//! indexing/warming helpers — plus the `mod` declarations. The `#[test]`s
//! themselves are split by subject into `tests/{ladder,disclosure,ranking,
//! eager_pass}.rs`, the same shape [`budget`]'s own header describes: this
//! file sat against the 1500-line ceiling (#4494) with no headroom for the
//! next test, the way #4456 had already left `search/engine/tests.rs` and
//! `stella-cli/src/config.rs` both one line from it (#3923, #4395's sibling
//! precedent for this exact shape).

use std::fs;
use std::path::Path;

use async_trait::async_trait;
use stella_core::search::{Depth, allocate, facets_at};
use stella_embed::{EmbedError, Embedder, EmbedderFingerprint, Embedding, SimilarityPosture};
use stella_graph::CodeGraph;
use stella_protocol::tool::ToolOutput;

use super::{
    Answer, ChunkWarmOutcome, Hit, SearchConfig, SearchReport, Strategy, coverage_note, dispatch,
    merge_notes, semantic_hits, warm_chunk_vectors, warm_chunk_vectors_with_progress,
};
use crate::search::semantic::{
    NO_FILE_CEILING, WarmOutcome, warm_file_vectors, warm_file_vectors_with_progress,
};
use crate::search::{codegraph, scan};

mod budget;
mod ceiling;
mod disclosure;
mod eager_pass;
mod ladder;
mod ranking;
mod refresh;
mod shims;
mod visibility;

use shims as uncached;
use shims::render;

/// `enrich`, with the fresh-cache wrapping [`shims`] explains.
mod enrich {
    pub(super) use super::shims::render_hit;
    pub(super) use crate::search::enrich::doc_comment;
}

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
/// the right one by luck. `a_surface_embedder_cannot_answer_the_same_question`
/// pins it.
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

/// A backend that always fails, for the named-failure passes.
#[derive(Debug)]
struct BrokenEmbedder;

#[async_trait]
impl Embedder for BrokenEmbedder {
    fn fingerprint(&self) -> EmbedderFingerprint {
        EmbedderFingerprint {
            model_id: "broken".into(),
            revision: "1".into(),
            dims: DIMS,
            normalization: "l2".into(),
        }
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Embedding>, EmbedError> {
        Err(EmbedError::Backend("upstream is down".into()))
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
/// [`codegraph::open_or_build`] resolves — what the warm passes and every
/// production caller actually open, unlike [`indexed_fixture`]'s
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
    let opened = codegraph::open_or_build(&root).expect("open_or_build");
    opened.graph.shutdown();
    root
}

/// [`indexed_fixture`] with its vectors already filled, by the same pass a
/// session runs in the background ([`crate::search::backfill`]).
///
/// **Every test that ranks by meaning goes through this.** Before #4043 a
/// search embedded whatever the index was missing on its way to answering, so
/// `indexed_fixture` alone was enough and the warming was invisible. It is
/// explicit now for the same reason it is explicit in production: filling the
/// index is not something a query is allowed to do.
async fn embedded_fixture(
    workspace: &Path,
    embedder: &dyn Embedder,
) -> (std::path::PathBuf, CodeGraph) {
    let (root, graph) = indexed_fixture(workspace);
    crate::search::backfill::backfill_opened(&graph, embedder, &mut |_| {}).await;
    (root, graph)
}

fn content_of(output: &ToolOutput) -> String {
    match output {
        ToolOutput::Ok { content, .. } => content.clone(),
        ToolOutput::Error { message, .. } => panic!("expected an answer, got an error: {message}"),
    }
}
