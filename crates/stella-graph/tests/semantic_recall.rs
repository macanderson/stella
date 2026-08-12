//! Witness: a natural-language question finds the right file when no
//! substring of the question appears in that file's name.
//!
//! This is the behaviour the whole vector plane exists for, and the one a
//! lexical index structurally cannot deliver. The fixture is built so that
//! grep, a filename match, and a hashed-n-gram projection all fail: the
//! question is "removing sensitive data before logging" and the answer is
//! `src/scrub.rs`, whose name shares no word with the question and whose body
//! never uses the question's words either. The only route from the question to
//! the file is meaning.
//!
//! The embedder is a tiny concept-lexicon model rather than a network call, so
//! the test is deterministic and offline. It is a real semantic mapping —
//! different surface words that mean the same thing project onto the same
//! dimension — which is precisely the property `HashEmbedder` lacks and the
//! property a hosted `voyage-code-3` / `text-embedding-3` has at scale. The
//! last assertion makes that contrast the test's subject: the same fixture,
//! ranked with the lexical embedder the tree shipped before this change, does
//! **not** put `scrub.rs` first.

use std::fs;
use std::path::Path;

use stella_embed::rank::Candidate;
use stella_embed::{Embedder, HashEmbedder};
use stella_graph::vectors::{FileVector, render_file_text};
use stella_graph::{CodeGraph, MAX_FILES_PER_PASS};

/// Four concepts, each a dimension. Every listed word projects onto its
/// concept, so words that share no characters ("redact", "sensitive") land in
/// the same place and words that share many ("logging", "logic") do not.
const LEXICON: &[(&str, usize)] = &[
    // 0 — handling secret material
    ("redact", 0),
    ("secret", 0),
    ("credential", 0),
    ("password", 0),
    ("sensitive", 0),
    ("removing", 0),
    ("remove", 0),
    ("strip", 0),
    ("private", 0),
    // 1 — logging
    ("logger", 1),
    ("logging", 1),
    ("log", 1),
    ("emit", 1),
    ("diagnostic", 1),
    // 2 — HTTP wire handling
    ("http", 2),
    ("request", 2),
    ("response", 2),
    ("header", 2),
    ("socket", 2),
    // 3 — numeric work
    ("matrix", 3),
    ("multiply", 3),
    ("numeric", 3),
    ("arithmetic", 3),
    ("sum", 3),
];

const DIMS: usize = 4;

/// A four-dimensional concept embedder. Deterministic, offline, and genuinely
/// semantic over its vocabulary: it maps meaning, not spelling.
struct ConceptEmbedder;

impl ConceptEmbedder {
    fn project(text: &str) -> Vec<f32> {
        let mut acc = vec![0.0f32; DIMS];
        let lowered = text.to_lowercase();
        for word in lowered.split(|c: char| !c.is_ascii_alphabetic()) {
            if word.is_empty() {
                continue;
            }
            for (term, dim) in LEXICON {
                // Stem-insensitive containment, so `redacts`/`redaction` count
                // as `redact` without a stemmer.
                if word.contains(term) {
                    acc[*dim] += 1.0;
                }
            }
        }
        stella_embed::l2_normalize(&mut acc);
        acc
    }
}

#[async_trait::async_trait]
impl Embedder for ConceptEmbedder {
    fn fingerprint(&self) -> stella_embed::EmbedderFingerprint {
        stella_embed::EmbedderFingerprint {
            model_id: "concept-lexicon".into(),
            revision: "1".into(),
            dims: DIMS,
            normalization: "l2".into(),
        }
    }

    async fn embed(
        &self,
        texts: &[String],
    ) -> Result<Vec<stella_embed::Embedding>, stella_embed::EmbedError> {
        if texts.is_empty() {
            return Err(stella_embed::EmbedError::EmptyInput);
        }
        let fingerprint = self.fingerprint().id();
        Ok(texts
            .iter()
            .map(|text| stella_embed::Embedding {
                fingerprint: fingerprint.clone(),
                vector: Self::project(text),
            })
            .collect())
    }

    fn similarity_posture(&self) -> stella_embed::SimilarityPosture {
        stella_embed::SimilarityPosture::Semantic {
            admission_floor: 0.2,
        }
    }
}

/// The question. Note what it does *not* contain: the string "scrub", any
/// prefix of it, and any identifier that appears in `scrub.rs`.
const QUESTION: &str = "removing sensitive data before logging";

const FIXTURE: &[(&str, &str)] = &[
    (
        "src/scrub.rs",
        // The right answer. Its filename shares nothing with the question and
        // neither does its body — it talks about credentials and a logger,
        // the question talks about sensitive data and logging.
        "/// Drops credential material from a record on its way to the logger.\n\
         pub fn clean(record: &mut Record) {\n\
         \x20   record.password = None;\n\
         \x20   record.secret_token = None;\n\
         }\n",
    ),
    (
        "src/wire.rs",
        "/// Parses an inbound http request header off the socket.\n\
         pub fn parse_header(socket: &Socket) -> Response { todo!() }\n",
    ),
    (
        "src/tally.rs",
        "/// Numeric helpers: matrix multiply and arithmetic sum.\n\
         pub fn multiply(m: &Matrix) -> Matrix { todo!() }\n",
    ),
];

fn build_fixture(root: &Path) {
    for (rel, body) in FIXTURE {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, body).expect("write");
    }
}

/// Embed everything pending and write it back — the exact loop a caller runs.
async fn embed_all(graph: &CodeGraph, embedder: &dyn Embedder) -> String {
    let fingerprint = embedder.fingerprint().id();
    let pending = graph
        .files_pending_embedding(&fingerprint, MAX_FILES_PER_PASS)
        .expect("pending")
        .files;
    assert!(!pending.is_empty(), "the fixture must have indexed");

    let texts: Vec<String> = pending.iter().map(|p| p.text.clone()).collect();
    let vectors = embedder.embed(&texts).await.expect("embed");
    let rows: Vec<FileVector> = pending
        .iter()
        .zip(vectors)
        .map(|(p, embedding)| FileVector {
            path: p.path.clone(),
            content_sha256: p.content_sha256.clone(),
            vector: embedding.vector,
        })
        .collect();
    graph
        .store_file_vectors(&fingerprint, &rows)
        .expect("store");
    fingerprint
}

#[tokio::test]
async fn a_natural_language_question_finds_the_file_whose_name_it_does_not_share() {
    let workspace = tempfile::tempdir().expect("tempdir");
    build_fixture(workspace.path());
    let root = workspace.path().canonicalize().expect("canonicalize");

    // The premise that makes this test worth anything: no lexical route
    // exists from the question to the answer.
    let question_words: Vec<&str> = QUESTION.split_whitespace().collect();
    for word in &question_words {
        assert!(
            !"src/scrub.rs".contains(word),
            "the question word `{word}` appears in the answer's path — the fixture \
             would be solvable by a filename match and proves nothing"
        );
        let body = fs::read_to_string(root.join("src/scrub.rs")).expect("read");
        assert!(
            !body.to_lowercase().contains(word),
            "the question word `{word}` appears in the answer's body — grep would \
             find it and this test proves nothing"
        );
    }

    let graph = CodeGraph::open(&root, &root.join("codegraph.db")).expect("open");
    graph.index_all().expect("index");

    let embedder = ConceptEmbedder;
    let fingerprint = embed_all(&graph, &embedder).await;
    assert_eq!(
        graph.embedded_file_count(&fingerprint).expect("count"),
        FIXTURE.len()
    );

    let query = &embedder
        .embed(&[QUESTION.to_string()])
        .await
        .expect("embed query")[0]
        .vector;
    let floor = embedder
        .similarity_posture()
        .admission_floor()
        .expect("a semantic backend declares a floor");

    let ranked = graph
        .rank_files_by_vector(&fingerprint, query, floor, 5)
        .expect("rank");

    assert!(!ranked.is_empty(), "the semantic query returned nothing");
    assert_eq!(
        ranked[0].key, "src/scrub.rs",
        "expected the redaction file first; got {ranked:?}"
    );

    // Deterministic: the identical query ranks identically, which is what
    // invariant 7 needs from output that reaches the prompt.
    let again = graph
        .rank_files_by_vector(&fingerprint, query, floor, 5)
        .expect("rank");
    assert_eq!(ranked, again);

    graph.shutdown();
}

/// The contrast that makes the witness above a proof rather than a
/// coincidence: the lexical embedder that was the tree's only option before
/// this change cannot answer the same question. If this ever starts passing,
/// the fixture has become grep-solvable and the witness above needs a harder
/// one.
#[tokio::test]
async fn the_lexical_embedder_cannot_answer_the_same_question() {
    let workspace = tempfile::tempdir().expect("tempdir");
    build_fixture(workspace.path());

    let hash = HashEmbedder::default();
    let mut rows: Vec<(String, Vec<f32>)> = Vec::new();
    for (rel, body) in FIXTURE {
        let text = render_file_text(rel, &[], body);
        let vector = hash.embed(&[text]).await.expect("embed")[0].vector.clone();
        rows.push(((*rel).to_string(), vector));
    }
    let query = hash.embed(&[QUESTION.to_string()]).await.expect("embed")[0]
        .vector
        .clone();

    let candidates: Vec<Candidate<'_>> = rows
        .iter()
        .map(|(key, vector)| Candidate {
            key: key.as_str(),
            vector: vector.as_slice(),
        })
        .collect();
    let ranked = stella_embed::rank::top_k(&query, &candidates, f32::NEG_INFINITY, 5);

    assert_ne!(
        ranked[0].key, "src/scrub.rs",
        "the hashing projection ranked the right answer first, which means this \
         fixture is solvable by surface overlap and the semantic witness proves \
         nothing: {ranked:?}"
    );
}
