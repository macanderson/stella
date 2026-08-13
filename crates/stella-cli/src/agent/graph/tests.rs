//! Witness: `stella init` embeds the tree it just indexed, before any turn
//! runs and without a semantic search having been issued.
//!
//! The whole point of moving the embedding pass forward is that a single-turn
//! run in a fresh checkout never gets a second chance at it: the lazy pass
//! fires after the agent has already started grepping, and never at all if the
//! agent does not reach for `semantic_code_search`. So the assertion that
//! matters is not "vectors exist eventually" — it is that they exist with
//! **no query embedded**, which is exactly what the mock's received bodies can
//! tell apart.

use super::*;

use wiremock::matchers::{method, path as path_matcher};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FIXTURE: &[(&str, &str)] = &[
    (
        "src/scrub.rs",
        "pub fn clean(record: &mut Record) { record.password = None; }\n",
    ),
    (
        "src/wire.rs",
        "pub fn parse_header(socket: &Socket) -> Response { todo!() }\n",
    ),
];

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (rel, body) in FIXTURE {
        let target = dir.path().join(rel);
        std::fs::create_dir_all(target.parent().expect("parent")).expect("mkdir");
        std::fs::write(&target, body).expect("write");
    }
    dir
}

/// A two-dimensional embeddings endpoint. It answers anything, because what
/// this test reads off it is *what was sent*, not what came back.
async fn embeddings_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_matcher("/v1/embeddings"))
        .respond_with(|request: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json");
            let inputs = body["input"].as_array().expect("input array").len();
            let data: Vec<serde_json::Value> = (0..inputs)
                .map(|index| serde_json::json!({ "index": index, "embedding": [1.0, 0.0] }))
                .collect();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": data }))
        })
        .mount(&server)
        .await;
    server
}

fn env_for(server: &MockServer) -> stella_embed::EmbedderEnv {
    stella_embed::EmbedderEnv {
        url: Some(format!("{}/v1", server.uri())),
        model: Some("concept-2".to_string()),
        dims: Some("2".to_string()),
        ..Default::default()
    }
}

/// Every text this pass sent to the backend, flattened across requests.
async fn sent_texts(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .expect("recorded requests")
        .iter()
        .flat_map(|request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json");
            body["input"]
                .as_array()
                .expect("input array")
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn open_graph(root: &std::path::Path) -> stella_graph::CodeGraph {
    let db = stella_store::workspace_private_sqlite_path(root, "codegraph.db").expect("db path");
    stella_graph::CodeGraph::open(root, &db).expect("open graph")
}

/// THE WITNESS. On `main` the vector table is empty until a semantic query
/// asks; here it is populated by `stella init` itself.
#[tokio::test]
async fn init_embeds_the_index_without_any_semantic_query_being_issued() {
    use stella_embed::Embedder;

    let ws = workspace();
    let root = ws.path().canonicalize().expect("canonicalize");
    let server = embeddings_server().await;

    let mut lines: Vec<String> = Vec::new();
    build_code_graph_with(&root, &env_for(&server), &mut |line| lines.push(line)).await;

    let fingerprint = stella_embed::HttpEmbedder::new(
        &format!("{}/v1", server.uri()),
        "concept-2",
        None,
        2,
        0.25,
    )
    .fingerprint()
    .id();
    let graph = open_graph(&root);
    let embedded = graph.embedded_file_count(&fingerprint).expect("count");
    graph.shutdown();

    assert_eq!(
        embedded,
        FIXTURE.len(),
        "`stella init` must leave every indexed file embedded; lines were {lines:?}"
    );

    // The half that makes this a witness for EAGER embedding rather than for
    // embedding at all: a query would have been sent as a bare sentence, and
    // every text here is either a file render (`render_file_text` opens with
    // `file: `) or a chunk render (`render_chunk_text`'s `path :: name (kind)`
    // header) — the chunk pass runs against this same mock server (#3098), so
    // both shapes are expected traffic now, and neither is a bare query.
    let texts = sent_texts(&server).await;
    assert!(!texts.is_empty(), "the pass sent nothing");
    for text in &texts {
        assert!(
            text.starts_with("file: ") || (text.contains(" :: ") && text.contains(")\n\n")),
            "only file or chunk renders may be embedded by init — this looks like a query: \
             {text:?}"
        );
    }
    assert!(
        lines.iter().any(|line| line.contains("semantic index")),
        "the pass must report what it did: {lines:?}"
    );
}

/// The normal case on a benchmark container and on most laptops: nothing
/// configured. Init must still build the graph and must not fail.
#[tokio::test]
async fn init_still_builds_the_graph_with_no_embedder_configured() {
    let ws = workspace();
    let root = ws.path().canonicalize().expect("canonicalize");

    let mut lines: Vec<String> = Vec::new();
    build_code_graph_with(&root, &stella_embed::EmbedderEnv::default(), &mut |line| {
        lines.push(line)
    })
    .await;

    let graph = open_graph(&root);
    let files = graph.file_count().expect("file count");
    graph.shutdown();
    assert_eq!(files, FIXTURE.len(), "the graph must still be built");
    assert!(
        lines.iter().any(|line| line.contains("code graph:")),
        "the index summary must still print: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("no embedding backend configured")),
        "an unconfigured workspace must be told how to turn semantic search on: {lines:?}"
    );
}

/// A half-set configuration is neither off nor working, and must not read as a
/// deliberate opt-out — nor cost init its success.
#[tokio::test]
async fn a_half_configured_embedder_names_what_is_missing_and_init_survives() {
    let ws = workspace();
    let root = ws.path().canonicalize().expect("canonicalize");

    let env = stella_embed::EmbedderEnv {
        url: Some("http://127.0.0.1:1/v1".to_string()),
        ..Default::default()
    };
    let mut lines: Vec<String> = Vec::new();
    build_code_graph_with(&root, &env, &mut |line| lines.push(line)).await;

    let graph = open_graph(&root);
    let files = graph.file_count().expect("file count");
    graph.shutdown();
    assert_eq!(files, FIXTURE.len());
    assert!(
        lines.iter().any(|line| line.contains("STELLA_EMBED_MODEL")),
        "the missing piece must be named: {lines:?}"
    );
}

/// A backend that cannot be reached costs the index, never the init.
#[tokio::test]
async fn a_broken_backend_reports_and_leaves_init_successful() {
    let ws = workspace();
    let root = ws.path().canonicalize().expect("canonicalize");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_matcher("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream is down"))
        .mount(&server)
        .await;

    let mut lines: Vec<String> = Vec::new();
    build_code_graph_with(&root, &env_for(&server), &mut |line| lines.push(line)).await;

    let graph = open_graph(&root);
    let files = graph.file_count().expect("file count");
    graph.shutdown();
    assert_eq!(files, FIXTURE.len(), "the graph survives a dead embedder");
    assert!(
        lines
            .iter()
            .any(|line| line.contains("semantic index: stopped")),
        "a failed pass must say so: {lines:?}"
    );
}

/// The cap is a stated partial index, not a silent one.
#[test]
fn a_capped_pass_names_what_it_left_unembedded() {
    let rendered = format_warm_outcome(
        &stella_tools::graph::semantic::WarmOutcome::Warmed {
            embedded: 2_000,
            remaining: 3_231,
            unreadable: 0,
        },
        "voyage-code-3",
    );
    assert!(rendered.contains("2000 files embedded"), "{rendered}");
    assert!(rendered.contains("3231 left unembedded"), "{rendered}");
    assert!(
        rendered.contains("embed on demand"),
        "the cap's leftovers are picked up by the lazy pass: {rendered}"
    );
}

/// #3016: a file the pass could not read is a different leftover from one the
/// cap deferred — no later pass picks it up — so it may not be reported in the
/// sentence that promises it will be.
#[test]
fn an_unreadable_file_is_named_as_a_problem_not_as_the_cap() {
    let rendered = format_warm_outcome(
        &stella_tools::graph::semantic::WarmOutcome::Warmed {
            embedded: 68,
            remaining: 32,
            unreadable: 32,
        },
        "voyage-code-3",
    );
    assert!(rendered.starts_with('!'), "{rendered}");
    assert!(rendered.contains("68 files embedded"), "{rendered}");
    assert!(rendered.contains("32 left unembedded"), "{rendered}");
    assert!(rendered.contains("could not be read"), "{rendered}");
    assert!(
        !rendered.contains("embed on demand"),
        "an unreadable file will not embed on demand — do not promise it: {rendered}"
    );
}

/// THE WITNESS for #3098's fix: on `main` the chunk vector table is empty
/// until `search` has already been called several times (each call capped at
/// `MAX_FILES_PER_CHUNK_PASS`, 64 files); here `stella init` alone leaves
/// every symbol in the fixture chunk-embedded, with no semantic query issued.
#[tokio::test]
async fn init_embeds_every_chunk_without_any_semantic_query_being_issued() {
    use stella_embed::Embedder;

    let ws = workspace();
    let root = ws.path().canonicalize().expect("canonicalize");
    let server = embeddings_server().await;

    let mut lines: Vec<String> = Vec::new();
    build_code_graph_with(&root, &env_for(&server), &mut |line| lines.push(line)).await;

    let fingerprint = stella_embed::HttpEmbedder::new(
        &format!("{}/v1", server.uri()),
        "concept-2",
        None,
        2,
        0.25,
    )
    .fingerprint()
    .id();
    let graph = open_graph(&root);
    let chunks_remaining = graph
        .pending_chunk_file_count(&fingerprint)
        .expect("pending count");
    let chunks_embedded = graph.embedded_chunk_count(&fingerprint).expect("count");
    graph.shutdown();

    assert_eq!(
        chunks_remaining, 0,
        "`stella init` must leave no file with an un-chunked symbol; lines were {lines:?}"
    );
    assert_eq!(
        chunks_embedded,
        FIXTURE.len(),
        "one function per fixture file"
    );
    assert!(
        lines.iter().any(|line| line.contains("chunk index")),
        "the pass must report what it did: {lines:?}"
    );
}

/// Same discipline as `a_capped_pass_names_what_it_left_unembedded`, one rung
/// sharper.
#[test]
fn a_capped_chunk_pass_names_what_it_left_unembedded() {
    let rendered = format_chunk_warm_outcome(
        &stella_tools::search::ChunkWarmOutcome::Warmed {
            files_embedded: 2_000,
            files_remaining: 1_231,
            unreadable: 0,
        },
        "voyage-code-3",
    );
    assert!(rendered.contains("2000 file(s) embedded"), "{rendered}");
    assert!(rendered.contains("1231 left unembedded"), "{rendered}");
    assert!(
        rendered.contains("embed on demand"),
        "the cap's leftovers are picked up by the lazy pass: {rendered}"
    );
}
