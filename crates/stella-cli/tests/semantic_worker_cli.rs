//! The private worker command fills the shared graph from a pipe-only config.

use std::io::Write;
use std::process::{Command, Stdio};

use stella_embed::{Embedder, EmbedderEnv, Resolution};
use stella_graph::CodeGraph;
use wiremock::matchers::{header, method};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

mod common;
use common::SealsEmbedderBackend;

#[tokio::test]
async fn worker_embeds_the_shared_index_with_pipe_only_credentials() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer fixture-pipe-key"))
        .respond_with(|request: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let inputs = body["input"].as_array().unwrap();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": inputs.iter().enumerate().map(|(index, _)|
                    serde_json::json!({"index": index, "embedding": [1.0, 0.0]})
                ).collect::<Vec<_>>()
            }))
        })
        .expect(1..)
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path().canonicalize().unwrap();
    std::fs::write(root.join("sample.rs"), "pub fn search_fixture() {}\n").unwrap();
    let db = stella_store::workspace_private_sqlite_path(&root, "codegraph.db").unwrap();
    let graph = CodeGraph::open(&root, &db).unwrap();
    graph.index_all().unwrap();
    let config = EmbedderEnv {
        url: Some(server.uri()),
        model: Some("fixture".into()),
        api_key: Some("fixture-pipe-key".into()),
        dims: Some("2".into()),
        ..EmbedderEnv::default()
    };
    let Resolution::Configured(embedder) = stella_embed::resolve(&config) else {
        panic!("fixture resolves")
    };
    let fingerprint = embedder.fingerprint().id();
    assert_eq!(graph.embedded_file_count(&fingerprint).unwrap(), 0);
    let mut child = Command::new(env!("CARGO_BIN_EXE_stella"))
        .without_embedder_backend()
        .arg("index-worker")
        .current_dir(&root)
        .env("STELLA_NO_ENV_FILE", "1")
        .env_remove("STELLA_CREDENTIAL_HANDOFF_FD")
        .env_remove("STELLA_CREDENTIAL_HANDOFF_TARGET")
        .env_remove("STELLA_SUPERVISED")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let request = serde_json::json!({"embedder": {
        "url": config.url, "model": config.model, "api_key": config.api_key,
        "dims": config.dims, "floor": null, "voyage_api_key": null, "openai_api_key": null
    }});
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let output = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(graph.embedded_file_count(&fingerprint).unwrap() > 0);
    assert_eq!(graph.pending_chunk_file_count(&fingerprint).unwrap(), 0);
    graph.shutdown();
}
