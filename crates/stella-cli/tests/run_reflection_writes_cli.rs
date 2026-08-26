// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **Witness (#4936).** A non-interactive `stella run --output-format json`
//! writes a lesson to `.stella/private/reflections.jsonl`.
//!
//! Nothing proved that before this file. The chain was covered in three
//! disconnected pieces — the gate admits a machine format
//! (`agent::tests::a_machine_format_turn_reflects_like_a_text_one`), the
//! spawned child does not inherit the opt-out
//! (`self_driving_cmd::work`'s `the_turn_does_not_inherit_the_reflection_opt_out`,
//! #4914), and the writer appends a line when called directly
//! (`memory::tests::reflection_mining`'s
//! `reflect_and_record_writes_lessons_to_log_and_store`) — and none of them
//! joined. The third is invoked directly and bypasses the gate entirely, so no
//! test reached the writer *through* `run_raw_one_shot`.
//!
//! That gap cost two bugs with the same symptom, neither caught by CI:
//!
//! - **#4130** — a `format == OutputFormat::Text` clause at the call site made
//!   the three conditions after it unreachable for every machine format. The
//!   gate's own test asserted the opposite and passed throughout, because the
//!   fourth condition lived where that test could not see it. 22 turns, 16
//!   merged pull requests, no reflections.
//! - **#4362** — the drive child inherited `STELLA_DISABLE_REFLECTION` from an
//!   unrelated shell. 143 `role=worker` model calls, zero `role=reflection`
//!   ones, and `reflections_logged: 0` with nothing distinguishing it from
//!   "learned nothing".
//!
//! # Why this is a spawned binary and not an in-process test
//!
//! `run_turn` spawns `std::env::current_exe()`, which under `cargo test` is
//! the test binary rather than `stella`, so a turn cannot be driven
//! in-process at all. Cargo's binary-path macro against a `wiremock` provider
//! is the only shape that reaches the writer through the real gate;
//! `goal_wrapped_dispatch_cli.rs` is the worked example this copies its
//! hermeticity and its mock routing from.
//!
//! The macro is named once, in [`run_one_shot`], and never spelled in prose:
//! `embedder_backend_sealed_cli.rs`'s guard counts occurrences of that literal
//! as spawn sites and requires a seal for each, so a doc comment mentioning it
//! reads as an unsealed spawn. That guard is right to count conservatively —
//! an unsealed child inherits `VOYAGE_API_KEY` and bills whoever runs the
//! suite (#4542) — so the prose moves, not the count.
//!
//! # Both directions, or neither is evidence
//!
//! The positive case alone is satisfied by a build that always reflects, so
//! [`the_opt_out_stops_the_lesson_being_written`] is the other half: the same
//! run with `STELLA_DISABLE_REFLECTION=1` **set on the child's `Command`**
//! must write nothing. On the child rather than in this process, for the
//! reason #4914 gives — `std::env` is process-global and this suite runs in
//! parallel, so a test that set the variable would decide other tests'
//! answers — and because the child's environment is the thing #4362 was
//! about.
//!
//! # Routing by body, never by call order
//!
//! The three calls a passing run makes (worker tool call, worker answer,
//! reflection) are told apart by what is *in* the request, not by how many
//! came before it: the reflection call is the only one carrying
//! [`REFLECTION_SYSTEM_PROMPT`], which `memory::reflection::reflect_on_turn`
//! puts at the head of its request and which reaches no worker prompt. A
//! change in dispatch order therefore cannot silently turn a two-call
//! assertion into a one-call one.

use std::path::Path;
use std::process::{Command, Stdio};

use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

mod common;
use common::SealsEmbedderBackend;

/// The opening words of the system message
/// `memory::reflection::reflect_on_turn` sends, and the only string that
/// separates the reflection call from a worker call on the wire.
const REFLECTION_SYSTEM_PROMPT: &str = "You are a self-reflection module";

/// The lesson the mocked reflection call returns. Asserted verbatim out of
/// the log, so the line under test is provably the one this mock produced and
/// not something else that happened to write the file.
const LESSON: &str = "the fixture provider answers on /chat/completions only";

/// A [`Match`] wiremock has no built-in for: "the body does NOT contain this
/// substring". Needed to route the worker's calls away from the reflection
/// mock without matching on the (large, prompt-engineering-internal, and
/// therefore fragile) worker system prompt instead. Same shape as
/// `goal_wrapped_dispatch_cli.rs`'s.
struct NotContains(&'static str);

impl Match for NotContains {
    fn matches(&self, request: &Request) -> bool {
        !String::from_utf8_lossy(&request.body).contains(self.0)
    }
}

/// One SSE completion, in the shape `stella-model`'s shared chat-completions
/// adapter parses: one content delta carrying the whole answer, a trailing
/// usage frame, then `[DONE]`.
fn sse_completion(text: &str) -> String {
    let content = serde_json::to_string(text).expect("text encodes");
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{content}}}}}]}}\n\n\
         data: {{\"choices\":[{{\"delta\":{{}}}}],\"usage\":{{\"prompt_tokens\":8,\"completion_tokens\":3}}}}\n\n\
         data: [DONE]\n\n"
    )
}

/// One SSE completion carrying a single tool call and no text, in the
/// index-keyed fragment dialect the shared chat-completions adapter parses.
///
/// The turn has to dispatch a tool for this test to mean anything:
/// `memory::turn_warrants_reflection` is `true` only when some message in the
/// turn carries a tool call, so a text-only turn would take the gate's
/// "nothing to mine" arm and write no line — and the test would then be
/// asserting the absence of a lesson in both directions.
///
/// `get_environment` is chosen for what it is *not*: it takes no arguments,
/// changes nothing, and its `Always` authority means no approval gate stands
/// between this fixture and a dispatched call — a run in non-interactive mode
/// has no human to answer one.
fn sse_tool_call() -> String {
    concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_env\",\
         \"function\":{\"name\":\"get_environment\",\"arguments\":\"{}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":8,\
         \"completion_tokens\":3}}\n\n",
        "data: [DONE]\n\n",
    )
    .to_string()
}

/// The reflection response, in the object shape
/// `memory::reflection::parse_lessons_checked` reads: one lesson with no
/// domain tags (a fresh workspace has no `domains.toml`, so every tag would
/// be dropped as invented), plus the self-review that rides in the same call.
fn sse_reflection() -> String {
    let body = serde_json::json!({
        "lessons": [{
            "lesson": LESSON,
            "trigger": "any turn in this fixture workspace",
            "saves": "the wrong base URL, which costs a whole failed turn",
            "kind": "domain",
            "domains": [],
        }],
        "self_review": {
            "delivered": true,
            "rating": 7,
            "went_well": "the tool call dispatched",
            "to_improve": "nothing",
            "critique": "a fixture turn",
        },
    });
    sse_completion(&body.to_string())
}

/// The mocked provider a reflecting run talks to: a tool call, then an
/// answer, then the reflection — each selected by request body.
async fn mock_reflecting_provider() -> MockServer {
    let server = MockServer::start().await;

    // Higher priority (a lower number) than the general worker mock below,
    // and capped at one use, so the *second* worker call falls through to the
    // text answer and the turn ends.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(NotContains(REFLECTION_SYSTEM_PROMPT))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_tool_call(), "text/event-stream"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(NotContains(REFLECTION_SYSTEM_PROMPT))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse_completion("did the requested work"),
            "text/event-stream",
        ))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(REFLECTION_SYSTEM_PROMPT))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(sse_reflection(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    server
}

/// `<workspace>/.stella/private/reflections.jsonl` — the mining log
/// `self_driving_cmd::learning::reflection_lines` counts, and the file whose
/// emptiness was #4130's and #4362's whole visible symptom.
fn reflection_log(workspace: &Path) -> std::path::PathBuf {
    workspace
        .join(".stella")
        .join("private")
        .join("reflections.jsonl")
}

/// Every non-empty line of the mining log, or none if it was never written.
fn logged_lessons(workspace: &Path) -> Vec<String> {
    std::fs::read_to_string(reflection_log(workspace))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// Run one hermetic `stella run --output-format json` against `server_uri`.
///
/// `opt_out` sets `STELLA_DISABLE_REFLECTION=1` **on this child**, which is
/// both the safe way to pin it (`std::env` is process-global and this suite
/// runs in parallel) and the thing #4362 was actually about.
///
/// `STELLA_HOME` as well as `STELLA_DATA_DIR`: the narrower variable moves
/// only the data tier, so a test that set it alone would still read the real
/// `~/.stella` — including `credentials.toml`, which is the second way a
/// provider key reaches this process and would make the run billable.
fn run_one_shot(
    workspace: &Path,
    data: &Path,
    server_uri: &str,
    opt_out: bool,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_stella"));
    command
        .without_embedder_backend()
        .args([
            "--model",
            "zai/glm-5.2",
            "--api-key",
            "sk-test-zai",
            "--base-url",
            server_uri,
            "--spend-limit",
            "5.0",
            "run",
            "--output-format",
            "json",
            "read the environment, then stop",
        ])
        .current_dir(workspace)
        .env("STELLA_HOME", data)
        .env("STELLA_DATA_DIR", data)
        .env("STELLA_NO_ENV_FILE", "1")
        .env_remove("STELLA_DISABLE_REFLECTION")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ZAI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("XAI_API_KEY")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("GOOGLE_API_KEY")
        .env_remove("GOOGLE_APPLICATION_CREDENTIALS")
        .env_remove("VERTEX_ACCESS_TOKEN")
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .env_remove("AWS_SESSION_TOKEN")
        .env_remove("AWS_REGION")
        .env_remove("AWS_DEFAULT_REGION")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if opt_out {
        command.env("STELLA_DISABLE_REFLECTION", "1");
    }
    command.output().expect("spawn stella run")
}

/// A workspace with one source file, so the session code-graph build has
/// something real to index — the same seed `run_exits_cli.rs` uses.
fn seed_workspace(workspace: &Path) {
    std::fs::write(workspace.join("lib.rs"), "pub fn hello() {}\n").expect("source file");
}

/// How many requests the mock actually served carrying the reflection
/// prompt. Zero here and a written log would mean the line came from
/// somewhere other than this turn's reflection call.
async fn reflection_calls(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| String::from_utf8_lossy(&request.body).contains(REFLECTION_SYSTEM_PROMPT))
        .count()
}

#[tokio::test]
async fn a_non_interactive_json_run_writes_its_lesson_to_the_mining_log() {
    let workspace = tempfile::tempdir().expect("workspace");
    let data = tempfile::tempdir().expect("data dir");
    seed_workspace(workspace.path());
    let server = mock_reflecting_provider().await;

    let output = tokio::task::spawn_blocking({
        let workspace = workspace.path().to_path_buf();
        let data = data.path().to_path_buf();
        let uri = server.uri();
        move || run_one_shot(&workspace, &data, &uri, false)
    })
    .await
    .expect("join");
    assert!(
        output.status.success(),
        "`stella run --output-format json` did not exit 0 — stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    assert_eq!(
        reflection_calls(&server).await,
        1,
        "the turn must make exactly one reflection model call — stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let lines = logged_lessons(workspace.path());
    assert_eq!(
        lines.len(),
        1,
        "`{}` must have gained exactly one line — stderr: {}",
        reflection_log(workspace.path()).display(),
        String::from_utf8_lossy(&output.stderr),
    );
    // The line is this reflection's, not an artifact of some other writer.
    let logged: serde_json::Value = serde_json::from_str(&lines[0]).expect("the line is JSON");
    assert_eq!(
        logged["lesson"].as_str(),
        Some(LESSON),
        "the logged lesson must be the one the reflection call returned: {}",
        lines[0]
    );
}

/// The negative direction. Without it the case above is satisfied by a build
/// that reflects unconditionally, which is a different bug from the one it is
/// meant to catch.
#[tokio::test]
async fn the_opt_out_stops_the_lesson_being_written() {
    let workspace = tempfile::tempdir().expect("workspace");
    let data = tempfile::tempdir().expect("data dir");
    seed_workspace(workspace.path());
    let server = mock_reflecting_provider().await;

    let output = tokio::task::spawn_blocking({
        let workspace = workspace.path().to_path_buf();
        let data = data.path().to_path_buf();
        let uri = server.uri();
        move || run_one_shot(&workspace, &data, &uri, true)
    })
    .await
    .expect("join");
    assert!(
        output.status.success(),
        "the opted-out run did not exit 0 — stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    assert_eq!(
        reflection_calls(&server).await,
        0,
        "`STELLA_DISABLE_REFLECTION=1` must buy no reflection model call"
    );
    assert!(
        logged_lessons(workspace.path()).is_empty(),
        "the opted-out run wrote to the mining log: {:?}",
        logged_lessons(workspace.path())
    );
}
