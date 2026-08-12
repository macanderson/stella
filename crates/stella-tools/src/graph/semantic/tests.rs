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

/// Index into the workspace-private path the tools use, the way `stella init`
/// leaves it — the eager pass opens `.stella/private/codegraph.db` and would
/// find nothing beside a db written to the fixture root.
fn index_as_init_does(root: &Path) {
    let db = stella_store::workspace_private_sqlite_path(root, "codegraph.db").expect("db path");
    let graph = stella_graph::CodeGraph::open(root, &db).expect("open");
    graph.index_all().expect("index");
    graph.shutdown();
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

/// The split (invariant #9): semantic search is its own tool with its own
/// verb, and `graph_query` no longer carries an operation selector for it.
#[test]
fn semantic_search_is_a_tool_of_its_own_and_graph_query_no_longer_offers_the_op() {
    let schema = SemanticCodeSearch.schema();
    assert_eq!(schema.name, "semantic_code_search");
    let properties = &schema.input_schema["properties"];
    assert!(
        properties.get("query").is_some(),
        "the input is the question: {properties}"
    );
    for selector in ["op", "mode", "kind"] {
        assert!(
            properties.get(selector).is_none(),
            "`{selector}` would select an operation, which invariant #9 forbids"
        );
    }

    let graph_query = crate::graph::CodeGraphQuery.schema();
    let ops = graph_query.input_schema["properties"]["op"]["enum"]
        .as_array()
        .expect("an op enum")
        .iter()
        .map(|value| value.as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(
        !ops.contains(&"semantic".to_string()),
        "graph_query kept the semantic op: {ops:?}"
    );
    // Discovery goes the other way instead: the structural tool points at the
    // one that answers a question it cannot.
    assert!(
        graph_query.description.contains("semantic_code_search"),
        "graph_query must name where a description-shaped question goes"
    );
}

#[tokio::test]
async fn an_empty_query_names_the_tool_that_takes_an_identifier() {
    let ws = workspace();
    let output = SemanticCodeSearch
        .execute(&serde_json::json!({ "query": "  " }), ws.path())
        .await;
    let ToolOutput::Error { message } = output else {
        panic!("an empty query must be an error");
    };
    assert!(message.contains("graph_query"), "{message}");
}

/// The eager pass: the index is populated with no query asked, which is what
/// makes semantic search available on the FIRST tool call of a one-turn run.
#[tokio::test]
async fn an_eager_pass_embeds_the_corpus_and_no_query() {
    let ws = workspace();
    let root = ws.path().canonicalize().expect("canonicalize");
    // The eager pass deliberately does not index; `stella init` has just run
    // `index_all`, which is what this stands in for.
    index_as_init_does(&root);
    let server = concept_server().await;

    let outcome = warm_file_vectors(&root, &embedder(&server), MAX_FILES_PER_EAGER_PASS).await;
    assert_eq!(
        outcome,
        WarmOutcome::Warmed {
            embedded: FIXTURE.len(),
            remaining: 0,
            unreadable: 0
        }
    );

    // Every text sent was a file render, never a question.
    for request in server.received_requests().await.expect("requests") {
        let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json");
        for input in body["input"].as_array().expect("inputs") {
            assert!(
                input.as_str().unwrap_or_default().starts_with("file: "),
                "the eager pass embedded something that is not a file: {input}"
            );
        }
    }

    // And the pass is idempotent: a second one has nothing left to do.
    let again = warm_file_vectors(&root, &embedder(&server), MAX_FILES_PER_EAGER_PASS).await;
    assert_eq!(
        again,
        WarmOutcome::Warmed {
            embedded: 0,
            remaining: 0,
            unreadable: 0
        }
    );
}

/// A repository past the cap gets a *stated* partial index — the number left
/// over is what the caller renders, so it can never be silent.
#[tokio::test]
async fn a_pass_that_hits_its_cap_reports_what_it_left() {
    let ws = workspace();
    let root = ws.path().canonicalize().expect("canonicalize");
    index_as_init_does(&root);
    let server = concept_server().await;

    let outcome = warm_file_vectors(&root, &embedder(&server), 1).await;
    assert_eq!(
        outcome,
        WarmOutcome::Warmed {
            embedded: 1,
            remaining: FIXTURE.len() - 1,
            unreadable: 0
        }
    );
}

/// #3016: an unreadable file at the head of the path order must not end the
/// pass. The window is sized in embeddable files, so the readable ones behind
/// it are still offered — and the leftover is reported as what it is rather
/// than as the cap's doing.
#[tokio::test]
async fn an_unreadable_file_does_not_stop_the_eager_pass_at_the_readable_ones() {
    let ws = workspace();
    let root = ws.path().canonicalize().expect("canonicalize");
    index_as_init_does(&root);
    // Indexed, then gone — `src/scrub.rs` sorts ahead of `src/wire.rs`, so it
    // is what a window of one lands on first.
    std::fs::remove_file(root.join("src/scrub.rs")).expect("remove");
    let server = concept_server().await;

    let outcome = warm_file_vectors(&root, &embedder(&server), 1).await;
    assert_eq!(
        outcome,
        WarmOutcome::Warmed {
            embedded: 1,
            remaining: 1,
            unreadable: 1
        },
        "the readable file behind the deleted one must still be embedded"
    );
}

/// A dead backend is a report, never a panic and never a lost graph.
#[tokio::test]
async fn a_broken_backend_makes_the_eager_pass_a_named_failure() {
    let ws = workspace();
    let root = ws.path().canonicalize().expect("canonicalize");
    index_as_init_does(&root);

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_matcher("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream is down"))
        .mount(&server)
        .await;

    let WarmOutcome::Failed { embedded, reason } =
        warm_file_vectors(&root, &embedder(&server), MAX_FILES_PER_EAGER_PASS).await
    else {
        panic!("a 500 must report a failure");
    };
    assert_eq!(embedded, 0);
    assert!(reason.contains("500"), "{reason}");
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
