// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella run --resume <id>` goes on with one named session, and
//! `stella run` names its session as it starts.
//!
//! A cloud sandbox can hit its time limit, or its process can exit. The
//! launcher then starts Stella again with the run's id. Each test runs the
//! real binary against a `wiremock` model. A turn cannot run inside
//! `cargo test` itself, so `run_reflection_writes_cli.rs` works the same
//! way. The model's saved requests show what it was sent.

use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::SealsEmbedderBackend;

const FIRST_PROMPT: &str = "fix the flaky parser test";
const NEXT_PROMPT: &str = "now add a regression test";

/// A model's no to one request. It ends the turn before the answer.
const REFUSAL: &str =
    r#"{"error":{"message":"the request was refused","type":"invalid_request_error"}}"#;

/// One streamed answer, sent in one piece.
fn sse_completion(text: &str) -> String {
    let content = serde_json::to_string(text).expect("text encodes");
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{content}}}}}]}}\n\n\
         data: {{\"choices\":[{{\"delta\":{{}}}}],\"usage\":{{\"prompt_tokens\":8,\"completion_tokens\":3}}}}\n\n\
         data: [DONE]\n\n"
    )
}

/// A model that says `text` to every call.
async fn answering(server: &MockServer, text: &str) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(sse_completion(text), "text/event-stream"),
        )
        .mount(server)
        .await;
}

/// A model that says `text` when a call holds `needle`. This rule wins over
/// the rest.
async fn answering_when(server: &MockServer, needle: &str, text: &str) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(needle))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(sse_completion(text), "text/event-stream"),
        )
        .with_priority(1)
        .mount(server)
        .await;
}

/// A folder with one source file and a trusted `SessionStart` hook. The
/// hook adds each payload it gets to `session-start.jsonl`.
fn seed_workspace(workspace: &Path) {
    std::fs::write(workspace.join("lib.rs"), "pub fn hello() {}\n").expect("source file");
    let log = workspace.join("session-start.jsonl");
    let command = format!("cat >> '{0}'; echo >> '{0}'", log.display());
    let settings = serde_json::json!({
        "hooks": { "SessionStart": [{ "hooks": [{ "command": command }] }] }
    });
    std::fs::create_dir_all(workspace.join(".stella")).expect(".stella");
    std::fs::write(
        workspace.join(".stella").join("settings.json"),
        settings.to_string(),
    )
    .expect("settings.json");
}

/// Every `SessionStart` payload the hook saw, in order.
fn session_starts(workspace: &Path) -> Vec<Value> {
    std::fs::read_to_string(workspace.join("session-start.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a payload is JSON"))
        .collect()
}

/// Run one sealed `stella run` in `workspace` against `base_url`.
///
/// It sets `STELLA_HOME` and `STELLA_DATA_DIR`, so the list of sessions and
/// every key file belong to the test.
fn stella_run(workspace: &Path, data: &Path, base_url: &str, run_args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_stella"));
    command
        .without_embedder_backend()
        .args([
            "--model",
            "zai/glm-5.2",
            "--api-key",
            "sk-test-zai",
            "--base-url",
            base_url,
            "--spend-limit",
            "5.0",
            "--foreground",
            "run",
        ])
        .args(run_args)
        .current_dir(workspace)
        .env("STELLA_HOME", data)
        .env("STELLA_DATA_DIR", data)
        .env("STELLA_NO_ENV_FILE", "1")
        .env("STELLA_TRUST_PROJECT", "1")
        .env("STELLA_DISABLE_REFLECTION", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in [
        "OPENROUTER_API_KEY",
        "ANTHROPIC_API_KEY",
        "ZAI_API_KEY",
        "OPENAI_API_KEY",
        "XAI_API_KEY",
        "DEEPSEEK_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "VERTEX_ACCESS_TOKEN",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_REGION",
        "AWS_DEFAULT_REGION",
        "STELLA_OUTPUT_FORMAT",
    ] {
        command.env_remove(key);
    }
    command.output().expect("spawn stella run")
}

/// Run `stella run` on its own thread, so the mock model keeps answering.
async fn run_blocking(workspace: &Path, data: &Path, base_url: &str, args: &[&str]) -> Output {
    let (workspace, data) = (workspace.to_path_buf(), data.to_path_buf());
    let base_url = base_url.to_string();
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        stella_run(&workspace, &data, &base_url, &args)
    })
    .await
    .expect("the run thread joins")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The id the run named on stderr as it started.
fn started_session(output: &Output) -> String {
    let stderr = stderr(output);
    stderr
        .lines()
        .find_map(|line| line.strip_prefix("stella: session "))
        .unwrap_or_else(|| panic!("the run named no session at start: {stderr}"))
        .trim()
        .to_string()
}

/// The JSON summary a `--output-format json` run prints on stdout.
fn summary(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is one JSON summary ({e}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            stderr(output)
        )
    })
}

/// Each request the model got, oldest first.
async fn requests(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect()
}

/// The `messages` list of one request.
fn messages_of(body: &str) -> Vec<Value> {
    let request: Value = serde_json::from_str(body).expect("a request is JSON");
    request["messages"]
        .as_array()
        .cloned()
        .expect("a request carries messages")
}

/// Whether a message holds `text`, in any shape.
fn says(message: &Value, text: &str) -> bool {
    message["content"].to_string().contains(text)
}

/// The run names its session on stderr as it starts, in each output
/// format. The `json` summary names it as `session_id` too.
#[tokio::test]
async fn a_run_names_its_session_as_it_starts() {
    let workspace = tempfile::tempdir().expect("workspace");
    let data = tempfile::tempdir().expect("data dir");
    seed_workspace(workspace.path());
    let server = MockServer::start().await;
    answering(&server, "done").await;
    let uri = server.uri();

    let text = run_blocking(workspace.path(), data.path(), &uri, &[FIRST_PROMPT]).await;
    assert!(text.status.success(), "{}", stderr(&text));
    assert!(started_session(&text).starts_with("ses-"));

    let json_args = ["--output-format", "json", FIRST_PROMPT];
    let json = run_blocking(workspace.path(), data.path(), &uri, &json_args).await;
    assert!(json.status.success(), "{}", stderr(&json));
    let id = started_session(&json);
    assert_eq!(summary(&json)["session_id"], id.as_str());

    let stream_args = ["--output-format", "stream-json", FIRST_PROMPT];
    let stream = run_blocking(workspace.path(), data.path(), &uri, &stream_args).await;
    assert!(stream.status.success(), "{}", stderr(&stream));
    assert!(started_session(&stream).starts_with("ses-"));
}

/// A resumed run with a prompt sends the saved turns, then the prompt as
/// the next turn. It keeps the session id. `SessionStart` fires with
/// `source: "resume"`.
#[tokio::test]
async fn a_resumed_run_sends_the_saved_turns_and_the_next_prompt() {
    let workspace = tempfile::tempdir().expect("workspace");
    let data = tempfile::tempdir().expect("data dir");
    seed_workspace(workspace.path());
    let server = MockServer::start().await;
    answering_when(&server, NEXT_PROMPT, "added the test").await;
    answering(&server, "fixed the parser").await;
    let uri = server.uri();

    let first_args = ["--output-format", "json", FIRST_PROMPT];
    let first = run_blocking(workspace.path(), data.path(), &uri, &first_args).await;
    assert!(first.status.success(), "{}", stderr(&first));
    let id = started_session(&first);

    let resume_args = [
        "--resume",
        id.as_str(),
        "--output-format",
        "json",
        "--",
        NEXT_PROMPT,
    ];
    let resumed = run_blocking(workspace.path(), data.path(), &uri, &resume_args).await;
    assert!(resumed.status.success(), "{}", stderr(&resumed));
    assert_eq!(started_session(&resumed), id, "the same session goes on");
    let report = summary(&resumed);
    assert_eq!(report["session_id"], id.as_str());
    assert_eq!(report["text"], "added the test");

    let bodies = requests(&server).await;
    let sent = messages_of(bodies.last().expect("the resumed run called the model"));
    let at = |role: &str, text: &str| {
        sent.iter()
            .position(|message| message["role"] == role && says(message, text))
            .unwrap_or_else(|| panic!("no {role} turn says {text:?}: {sent:?}"))
    };
    let asked = at("user", FIRST_PROMPT);
    let answered = at("assistant", "fixed the parser");
    let next = at("user", NEXT_PROMPT);
    assert!(asked < answered, "the saved turn keeps its order: {sent:?}");
    assert_eq!(
        next,
        sent.len() - 1,
        "the prompt is the next turn: {sent:?}"
    );

    let starts = session_starts(workspace.path());
    let sources: Vec<&str> = starts
        .iter()
        .map(|start| start["source"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(sources, ["startup", "resume"], "{starts:?}");
    assert!(
        starts
            .iter()
            .all(|start| start["session_id"] == id.as_str()),
        "{starts:?}"
    );
}

/// A turn that stopped before its answer goes on with no prompt. The model
/// gets the saved turn as it was, with no new turn. That includes the recall
/// block the first run put before the prompt, which is a user message too.
#[tokio::test]
async fn a_resumed_run_with_no_prompt_continues_the_stopped_turn() {
    let workspace = tempfile::tempdir().expect("workspace");
    let data = tempfile::tempdir().expect("data dir");
    seed_workspace(workspace.path());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_string(REFUSAL))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    answering(&server, "fixed the parser").await;
    let uri = server.uri();

    let first_args = ["--output-format", "json", FIRST_PROMPT];
    let first = run_blocking(workspace.path(), data.path(), &uri, &first_args).await;
    assert!(!first.status.success(), "the refused turn fails");
    let id = started_session(&first);

    let resume_args = ["--resume", id.as_str(), "--output-format", "json"];
    let resumed = run_blocking(workspace.path(), data.path(), &uri, &resume_args).await;
    assert!(resumed.status.success(), "{}", stderr(&resumed));
    assert_eq!(summary(&resumed)["text"], "fixed the parser");

    let bodies = requests(&server).await;
    let refused = messages_of(bodies.first().expect("the first run called the model"));
    let sent = messages_of(bodies.last().expect("the resumed run called the model"));
    // Each run builds its own system prompt, so the turns after it are what
    // must match. They include the first run's recall block: a resumed run
    // recalls nothing new.
    assert_eq!(
        sent[1..],
        refused[1..],
        "the model got the saved turn as it was, with no new turn"
    );
    let last = sent.last().expect("the request carries a turn");
    assert_eq!(last["role"], "user", "{sent:?}");
    assert!(says(last, FIRST_PROMPT), "{sent:?}");

    let again = run_blocking(workspace.path(), data.path(), &uri, &resume_args).await;
    assert!(
        !again.status.success(),
        "a finished turn has nothing to go on"
    );
    assert!(
        stderr(&again).contains("finished its last turn"),
        "{}",
        stderr(&again)
    );
}

/// An id no session has fails and names the id. It starts nothing. No
/// session is added and no model call is made.
#[tokio::test]
async fn an_unknown_id_fails_naming_it_and_starts_nothing() {
    let workspace = tempfile::tempdir().expect("workspace");
    let data = tempfile::tempdir().expect("data dir");
    seed_workspace(workspace.path());
    let server = MockServer::start().await;
    answering(&server, "never sent").await;
    let uri = server.uri();

    let args = [
        "--resume",
        "ses-missing",
        "--output-format",
        "json",
        "--",
        NEXT_PROMPT,
    ];
    let output = run_blocking(workspace.path(), data.path(), &uri, &args).await;
    assert!(!output.status.success(), "an unknown id is a failure");
    assert!(
        stderr(&output).contains("ses-missing"),
        "{}",
        stderr(&output)
    );
    assert!(
        summary(&output)["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("ses-missing")),
        "the JSON error names the id too"
    );
    assert!(requests(&server).await.is_empty(), "no model call was made");
    let registered = std::fs::read_dir(data.path().join("sessions"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(registered, 0, "no session was registered");
    assert!(
        session_starts(workspace.path()).is_empty(),
        "no session started"
    );
}
