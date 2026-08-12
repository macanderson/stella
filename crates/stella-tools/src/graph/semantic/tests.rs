use super::*;
use stella_embed::HttpEmbedder;
use wiremock::matchers::{method, path as path_matcher};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The same fixture shape as `stella-graph`'s witness: the right answer for a
/// concept query is a file whose name shares nothing with the query.
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

fn open(root: &Path) -> stella_graph::CodeGraph {
    let graph = stella_graph::CodeGraph::open(root, &root.join("codegraph.db")).expect("open");
    graph.index_all().expect("index");
    graph
}

fn content(output: &ToolOutput) -> String {
    match output {
        ToolOutput::Ok { content } => content.clone(),
        ToolOutput::Error { message } => panic!("expected Ok, got error: {message}"),
    }
}

/// Every embed request answers with a vector chosen by whether the text
/// mentions a password — a stand-in for "the model understood this file is
/// about secrets". Deterministic and offline; the transport under test is the
/// real `HttpEmbedder`.
async fn concept_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_matcher("/v1/embeddings"))
        .respond_with(|request: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json body");
            let inputs = body["input"].as_array().expect("input array").clone();
            let data: Vec<serde_json::Value> = inputs
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    let text = value.as_str().unwrap_or_default().to_lowercase();
                    // "secrets" concept on dim 0, "wire" concept on dim 1.
                    let vector = if text.contains("password") || text.contains("credential") {
                        [1.0, 0.0]
                    } else if text.contains("socket") || text.contains("header") {
                        [0.0, 1.0]
                    } else {
                        [0.0, 0.0]
                    };
                    serde_json::json!({ "index": index, "embedding": vector })
                })
                .collect();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": data }))
        })
        .mount(&server)
        .await;
    server
}

fn embedder(server: &MockServer) -> HttpEmbedder {
    HttpEmbedder::new(&format!("{}/v1", server.uri()), "concept-2", None, 2, 0.2)
}

#[tokio::test]
async fn a_concept_query_ranks_the_file_it_describes_first() {
    let ws = workspace();
    let root = ws.path().canonicalize().expect("canonicalize");
    let graph = open(&root);
    let server = concept_server().await;

    let output =
        semantic_query_with(&graph, &embedder(&server), "handling credential material").await;
    let rendered = content(&output);

    assert!(rendered.contains("src/scrub.rs"), "{rendered}");
    let scrub = rendered.find("src/scrub.rs").expect("scrub listed");
    // `wire.rs` being absent entirely is also correct — it scores 0.0 against
    // the query and the floor drops it. What must never happen is it ranking
    // above the file the query describes.
    if let Some(wire) = rendered.find("src/wire.rs") {
        assert!(scrub < wire, "expected scrub.rs first:\n{rendered}");
    }
    assert!(rendered.contains("concept-2"), "the answer names the model");
    graph.shutdown();
}

#[tokio::test]
async fn the_second_query_re_embeds_nothing() {
    let ws = workspace();
    let root = ws.path().canonicalize().expect("canonicalize");
    let graph = open(&root);
    let server = concept_server().await;
    let embedder = embedder(&server);

    semantic_query_with(&graph, &embedder, "handling credential material").await;
    let after_first = server.received_requests().await.expect("requests").len();

    semantic_query_with(&graph, &embedder, "handling credential material").await;
    let after_second = server.received_requests().await.expect("requests").len();

    assert_eq!(
        after_second - after_first,
        1,
        "the second pass must embed only the query, not the corpus again"
    );
    graph.shutdown();
}

#[tokio::test]
async fn an_identical_query_renders_identical_bytes() {
    let ws = workspace();
    let root = ws.path().canonicalize().expect("canonicalize");
    let graph = open(&root);
    let server = concept_server().await;
    let embedder = embedder(&server);

    let first = content(&semantic_query_with(&graph, &embedder, "credential handling").await);
    let second = content(&semantic_query_with(&graph, &embedder, "credential handling").await);
    assert_eq!(
        first, second,
        "tool output that reaches the prompt must be byte-stable"
    );
    graph.shutdown();
}

#[tokio::test]
async fn a_broken_endpoint_degrades_to_names_and_says_so() {
    let ws = workspace();
    let root = ws.path().canonicalize().expect("canonicalize");
    let graph = open(&root);

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_matcher("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream is down"))
        .mount(&server)
        .await;

    let output = semantic_query_with(&graph, &embedder(&server), "parse header").await;
    let rendered = content(&output);
    assert!(
        rendered.starts_with(DEGRADED_NOTE_PREFIX),
        "a failed backend must announce the degradation: {rendered}"
    );
    assert!(rendered.contains("src/wire.rs"), "{rendered}");
    graph.shutdown();
}

#[test]
fn the_no_embedder_fallback_matches_names_and_labels_itself() {
    let ws = workspace();
    let root = ws.path().canonicalize().expect("canonicalize");
    let graph = open(&root);

    let rendered = content(&lexical_answer(&graph, "parse_header socket", None));
    assert!(rendered.starts_with(LEXICAL_FALLBACK_NOTE), "{rendered}");
    assert!(rendered.contains("src/wire.rs"), "{rendered}");
    // Deterministic, and never dressed up as a meaning match.
    assert_eq!(
        rendered,
        content(&lexical_answer(&graph, "parse_header socket", None))
    );
    graph.shutdown();
}

#[test]
fn the_fallback_reports_a_miss_in_the_classifiable_vocabulary() {
    let ws = workspace();
    let root = ws.path().canonicalize().expect("canonicalize");
    let graph = open(&root);

    let rendered = content(&lexical_answer(&graph, "quaternion tessellation", None));
    assert_eq!(
        crate::graph::classify_answer(&rendered),
        crate::graph::GraphAnswer::Unresolved,
        "{rendered}"
    );
    graph.shutdown();
}

#[test]
fn function_words_are_not_search_terms_but_short_identifiers_are() {
    // `is` is too short, `where`/`the` are noise — and `sql` is three
    // characters of pure signal, which is why a length floor alone will not do.
    assert_eq!(
        terms_of("where is the sql http header parsed"),
        vec![
            "header".to_string(),
            "http".to_string(),
            "parsed".to_string(),
            "sql".to_string(),
        ]
    );
}
