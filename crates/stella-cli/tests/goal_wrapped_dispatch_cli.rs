//! **Witness (#3911).** `stella goal` binds an installed wrapper plugin and
//! hands it the turn. The plugin's own `again` decides how many rounds the
//! goal takes.
//!
//! It fails on the old binary for the plainest possible reason: the plugin
//! this file installs declares `participation = "arbiter"`, and
//! `wrapper_plugin::reject_arbiter_wrapper_on_goal` refused that grade on this
//! door before any provider was built — the process exited nonzero with "is
//! arbiter-grade and cannot run on `stella goal`" and the fixture's logs stayed
//! empty. It passes on this one: the door has no completion arbiter of its own
//! left to collide with, so arbiter is the grade it wants.
//!
//! Its subject before that was the other arrangement (#3695, goal half): a
//! *steering* plugin dispatched once per round of the built-in goal loop, with
//! `stella_core::Engine::assess` deciding met/unmet. That loop is gone from
//! this door, and with it the only reason a steering plugin saw more than one
//! round here.
//!
//! Deliberately a real subprocess against a real (mocked) HTTP endpoint,
//! matching `run_exits_cli.rs`'s hermeticity discipline — no ambient
//! credentials, `--base-url` pointed at a `wiremock` server this test drives
//! itself — because the property under test only exists once the dispatch has
//! genuinely held a turn open and run two worker turns end to end.
//!
//! `cfg(unix)`: the fixture wrapper is a `/bin/sh` script, same reason
//! `stella-runtime`'s `tests/wrapper_dispatch.rs` is unix-only.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::SealsEmbedderBackend;

/// The `[wrapper] id` the fixture declares, and what must land in
/// `executions.pipeline_variant`.
const VARIANT: &str = "goal-fixture-v1";

/// An arbiter-grade manifest shaped like `plugins/stella-goal`'s real one: the
/// `Stop` hook arbiter requires, one requirement, and a `flip =
/// "not-applicable"` oracle deciding it off a single `met` measurement.
///
/// `max_holds = 3` is an ask, not an authority — the host funds
/// `DEFAULT_HOST_MAX_HOLDS`, and one hold is all this fixture needs.
const PLUGIN_TOML: &str = r#"
name = "goal-fixture"
[loop]
participation = "arbiter"
hooks = ["Stop"]
points = ["before_turn", "after_turn"]
max_holds = 3
[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh", "${plugin_dir}/rounds.log", "${plugin_dir}/after.log", "${plugin_dir}/count"]
timeout_secs = 30
env = ["PATH"]
[wrapper]
id = "goal-fixture-v1"
[[wrapper.stages]]
name = "execute"
[requirements]
goal-met = "an independent verifier turn assessed the goal as accomplished"
[oracle]
flip = "not-applicable"
measurements = ["met"]
[[oracle.checks]]
requirement = "goal-met"
check = "met >= 1"
"#;

/// Every call appends the exact request it was asked (one line — the wire
/// framing is compact JSON, never pretty-printed) to its point's own log:
/// `before_turn` to `$1`, so the test can count how many rounds actually
/// dispatched through it, and `after_turn` to `$2`, which is where the turn
/// facts the host reports back (#3834) are read from.
///
/// `after_turn` reports `met = 0` the first time and `met = 1` the second,
/// counting through `$3`. That is what makes the dispatch hold the turn open
/// exactly once: `judge` reads the declared check `met >= 1` against the
/// evidence, `again` holds an unmet round open for an arbiter, and the second
/// round settles it.
///
/// The `Stop` hook `ManifestError::ArbiterMustDeclareStop` requires reaches
/// this same program, so its payload is answered and dropped rather than
/// appended: it is a hook event, not a wrapper-socket point, and counting it
/// as a round would make this file's central assertion wrong in the direction
/// that reads as a pass.
const PLUGIN_SCRIPT: &str = r#"#!/bin/sh
input=$(cat)
case "$input" in
  *'"point":"after_turn"'*)
    printf '%s\n' "$input" >> "$2"
    if [ -f "$3" ]; then met=1; else met=0; : > "$3"; fi
    printf '%s\n' '{"point":"after_turn","body":{"protocol_version":1,"evidence":{"flip":"not-attempted","measurements":{"met":'"$met"'}}}}'
    ;;
  *'"point":"before_turn"'*)
    printf '%s\n' "$input" >> "$1"
    printf '%s\n' '{"point":"before_turn","body":{"protocol_version":1}}'
    ;;
  *)
    printf '%s\n' '{}'
    ;;
esac
"#;

/// Write the fixture wrapper into the project plugin tier
/// (`<workspace>/.stella/plugins/…/plugin.toml`) and return the two paths its
/// script logs to: every `before_turn` call, then every `after_turn` call.
fn install_fixture_wrapper(workspace: &Path) -> (PathBuf, PathBuf) {
    let dir = workspace
        .join(".stella")
        .join("plugins")
        .join("goal-fixture");
    std::fs::create_dir_all(&dir).expect("plugin dir");
    std::fs::write(dir.join("plugin.toml"), PLUGIN_TOML).expect("plugin.toml");
    let script = dir.join("main.sh");
    std::fs::write(&script, PLUGIN_SCRIPT).expect("main.sh");
    let mut perms = std::fs::metadata(&script)
        .expect("script metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).expect("chmod +x");
    (dir.join("rounds.log"), dir.join("after.log"))
}

/// The witness the run is judged against, named by [`TEST_COMMAND`].
///
/// It has to exist on disk before the run: `TamperWatch::pin` records a
/// baseline of things that *were there*, and an absent artifact is left out of
/// the watch entirely rather than pinned as missing.
fn install_witness(workspace: &Path) {
    let tests = workspace.join("tests");
    std::fs::create_dir_all(&tests).expect("tests dir");
    std::fs::write(tests.join("witness_flip.sh"), "#!/bin/sh\nexit 1\n").expect("the witness");
}

/// The oracle `--test-command` arms, and the one artifact the host pins from
/// it. `sh <path>` names its file syntactically, which is the only shape
/// `wrapper_candidate::named_artifacts` will watch.
const TEST_COMMAND: &str = "sh tests/witness_flip.sh";

/// One SSE completion, in the shape `stella-model`'s shared chat-completions
/// adapter parses (`crates/stella-model/src/zai/tests.rs`'s
/// `complete_aggregates_text_deltas_from_a_mock_server` is the reference this
/// copies): one content delta carrying the whole answer, a trailing usage
/// frame, then `[DONE]`.
fn sse_completion(text: &str) -> String {
    let content = serde_json::to_string(text).expect("text encodes");
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{content}}}}}]}}\n\n\
         data: {{\"choices\":[{{\"delta\":{{}}}}],\"usage\":{{\"prompt_tokens\":8,\"completion_tokens\":3}}}}\n\n\
         data: [DONE]\n\n"
    )
}

/// One SSE completion carrying a single tool call and no text, in the
/// index-keyed fragment dialect the shared chat-completions adapter parses
/// (`crates/stella-model/src/zai/tests.rs`'s
/// `complete_reassembles_a_streamed_tool_call_split_across_many_chunks`).
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

/// A mock chat-completions endpoint standing in for the real provider.
///
/// No verifier arm: the plugin decides met/unmet from its own `after_turn`
/// evidence and spends no child turn, so every request this server sees is a
/// worker call. The first answers with a tool call rather than text, so round
/// 1's turn genuinely dispatches something and the facts the host folds are
/// non-trivial rather than merely present (#3834).
///
/// `get_environment` is the tool chosen for what it is *not*: it takes no
/// arguments, changes nothing, and its `Always` authority means no approval
/// gate stands between this fixture and a dispatched call — a headless goal
/// run has no human to answer one.
async fn mock_worker() -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_tool_call(), "text/event-stream"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse_completion("did the requested work"),
            "text/event-stream",
        ))
        .mount(&server)
        .await;

    server
}

/// `<workspace>/.stella/private/store.db` — a per-workspace path, unrelated
/// to `STELLA_DATA_DIR` (that env var moves the cross-project telemetry hub
/// under `~/.stella`, not this workspace's own store — AGENTS.md's `.stella/`
/// table).
fn store_path(workspace: &Path) -> PathBuf {
    workspace.join(".stella").join("private").join("store.db")
}

#[tokio::test]
async fn a_goal_run_binds_an_arbiter_plugin_and_takes_the_rounds_it_holds_open() {
    let workspace = tempfile::tempdir().expect("workspace");
    let data = tempfile::tempdir().expect("data dir");
    // The user tier, which `STELLA_DATA_DIR` does not move. Removing every
    // `*_API_KEY` below is not hermeticity on its own: an environment variable
    // is one of two ways a credential reaches the process, and
    // `~/.stella/credentials.toml` is the other. `STELLA_HOME` moves the whole
    // user tier, so there is no credentials file to find.
    let home = tempfile::tempdir().expect("stella home");
    let (rounds_log, after_log) = install_fixture_wrapper(workspace.path());
    install_witness(workspace.path());
    let server = mock_worker().await;

    let child = Command::new(env!("CARGO_BIN_EXE_stella"))
        .without_embedder_backend()
        .args([
            "--model",
            "zai/glm-5.2",
            "--api-key",
            "sk-test-zai",
            "--base-url",
            &server.uri(),
            "--spend-limit",
            "5.0",
            "goal",
            "do the thing, then stop",
            "--pipeline",
            VARIANT,
            "--test-command",
            TEST_COMMAND,
        ])
        .current_dir(workspace.path())
        .env("STELLA_HOME", home.path())
        .env("STELLA_DATA_DIR", data.path())
        // Same hermeticity discipline as `run_exits_cli.rs`: no `.env`, no
        // inherited real key, and the project plugin tier trusted so the
        // fixture wrapper actually loads.
        .env("STELLA_NO_ENV_FILE", "1")
        .env("STELLA_TRUST_PROJECT", "1")
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
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stella goal");

    let output = tokio::task::spawn_blocking(move || child.wait_with_output())
        .await
        .expect("join")
        .expect("wait on stella");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("is arbiter-grade and cannot run on `stella goal`"),
        "the door must accept the grade it needs: {stderr}"
    );
    assert!(
        output.status.success(),
        "stella goal --pipeline {VARIANT} did not exit 0 — stdout: {}\nstderr: {stderr}",
        String::from_utf8_lossy(&output.stdout),
    );

    // `before_turn` fired once per round the arbiter held open — two, because
    // its first `after_turn` reported `met = 0` and the declared check
    // `met >= 1` is what `judge` reads.
    let log = std::fs::read_to_string(&rounds_log).unwrap_or_default();
    let calls: Vec<&str> = log.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(
        calls.len(),
        2,
        "the arbiter held the turn open once, so two rounds ran: {log}"
    );
    for call in &calls {
        assert!(
            call.contains("\"point\":\"before_turn\""),
            "logged call was not a before_turn request: {call}"
        );
        // **Witness (#3835).** Every round carries the grant over the tree it
        // runs in, with `--test-command` parsed into the plan the plugin's
        // oracle would run.
        assert!(
            call.contains("\"candidate\":"),
            "a goal round must hand the plugin the tree it runs in: {call}"
        );
        assert!(
            call.contains("\"program\":\"sh\"") && call.contains("witness_flip.sh"),
            "the grant carries the parsed test plan the oracle observes with: {call}"
        );
    }

    // **Witness (#3834).** Every round reports what its worker turn actually
    // dispatched and changed, and the first round names the tool it called.
    //
    // The distinction the assertions below draw is the one the wire type
    // exists to draw: `[]` is "the turn dispatched none", an absent key is
    // "this host cannot look".
    let after = std::fs::read_to_string(&after_log).unwrap_or_default();
    let reports: Vec<serde_json::Value> = after
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("an after_turn request is a wire message"))
        .collect();
    assert_eq!(
        reports.len(),
        2,
        "one after_turn call per round the arbiter held open: {after}"
    );
    for report in &reports {
        let turn = &report["body"]["turn"];
        assert!(
            turn["tools"].is_array(),
            "a goal round must report the tools its turn dispatched, not omit the key: {report}"
        );
        assert!(
            turn["changed_files"].is_array(),
            "a goal round must report what its turn changed, not omit the key: {report}"
        );
    }
    assert_eq!(
        reports[0]["body"]["turn"]["tools"],
        serde_json::json!(["get_environment"]),
        "round 1's worker turn dispatched one tool and the fold must name it: {}",
        reports[0]
    );
    assert_eq!(
        reports[1]["body"]["turn"]["tools"],
        serde_json::json!([]),
        "round 2's worker turn dispatched none, which is a different claim from \
         `this host does not report tools`: {}",
        reports[1]
    );

    // The rows this run opened are the goal door's, not the one-shot's. The
    // driver takes the door name as a field for exactly this reason: both
    // doors run the same wrapped turn, and a goal run whose rows all said
    // `run` would be unfindable in the store under the verb the user typed.
    let conn = rusqlite::Connection::open(store_path(workspace.path())).expect("open store.db");
    let (rows, variant): (i64, Option<String>) = conn
        .query_row(
            "SELECT COUNT(*), MAX(pipeline_variant) FROM executions WHERE kind = 'goal'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("goal execution rows");
    assert_eq!(
        rows, 2,
        "each round the arbiter held open is a turn, and each turn opens its own row"
    );
    assert_eq!(
        variant.as_deref(),
        Some(VARIANT),
        "executions.pipeline_variant must name the wrapper that drove every round"
    );
}
