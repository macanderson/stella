// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **Witness (#3695, fleet half).** `stella fleet --pipeline <variant>` used
//! to be refused unconditionally by
//! `wrapper_plugin::reject_plugin_variant_for_door` before this change — this
//! file fails on the old binary for the plainest possible reason (the process
//! exits nonzero with "is not supported on `stella fleet` yet", the fixture's
//! log is never written and no execution row is opened at all) and passes on
//! this one: each worker attempt binds the installed wrapper in its own tree
//! and dispatches its turn through it, and `executions.pipeline_variant` names
//! that wrapper on the attempt's row.
//!
//! Deliberately a real subprocess against a real (mocked) HTTP endpoint,
//! matching `goal_wrapped_dispatch_cli.rs`'s hermeticity discipline — no
//! ambient credentials, `--base-url` pointed at a `wiremock` server this test
//! drives itself — because the property under test (a wrapper's `before_turn`
//! firing inside a fleet worker, on the worker's own OS thread and its own
//! current-thread runtime, and the store row that worker opens) only exists
//! once the fan-out has genuinely run a task end to end.
//!
//! The plan is one **shared-tree** task (`Task::new`'s default, which is what
//! a positional prompt builds), so the worker's tree is the workspace itself
//! and the assertions can read one store. The fixture plugin lives in the
//! invocation workspace's project tier, which is where
//! `fleet_cmd::wrapped::bind_for_attempt` reads the roster from for every
//! attempt — including an isolated one, whose fresh worktree would not carry
//! an untracked `.stella/plugins/` at all.
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
const VARIANT: &str = "fleet-fixture-v1";

/// No `[requirements]`/`[oracle]` at all: `judge` is `Verdict::Met` for an
/// empty rule (`crates/stella-runtime/src/wrapper/verdict.rs`'s own doc, "a
/// rule with no requirements is `Verdict::Met`"), so the dispatch drives
/// exactly one internal turn — one attempt, one worker turn, as the raw arm
/// does.
///
/// The `witness` stage is conditional on the `test-command` signal, which is
/// what makes the per-task oracle (#3884) observable from outside: it is
/// programmed away for a task that declares none and dispatched for one that
/// does.
const PLUGIN_TOML: &str = r#"
name = "fleet-fixture"
[loop]
participation = "steering"
points = ["before_turn", "after_turn"]
[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh", "${plugin_dir}/rounds.log"]
timeout_secs = 30
env = ["PATH"]
[wrapper]
id = "fleet-fixture-v1"
[[wrapper.stages]]
name = "witness"
if = "test-command"
[[wrapper.stages]]
name = "execute"
"#;

/// Every `before_turn` call appends the exact request it was asked (one line
/// — the wire framing is compact JSON, never pretty-printed) to `$1`, so the
/// test can see the worker genuinely dispatched through the plugin.
/// `after_turn` answers with an empty, `flip = "not-attempted"` observation
/// (nothing in this fixture's manifest declares an oracle that would read
/// it).
const PLUGIN_SCRIPT: &str = r#"#!/bin/sh
input=$(cat)
case "$input" in
  *'"point":"after_turn"'*)
    printf '%s\n' '{"point":"after_turn","body":{"protocol_version":1,"evidence":{"flip":"not-attempted"}}}'
    ;;
  *)
    printf '%s\n' "$input" >> "$1"
    printf '%s\n' '{"point":"before_turn","body":{"protocol_version":1}}'
    ;;
esac
"#;

/// Write the fixture wrapper into the project plugin tier
/// (`<workspace>/.stella/plugins/…/plugin.toml`) and return the path its
/// script logs every `before_turn` call to.
fn install_fixture_wrapper(workspace: &Path) -> PathBuf {
    let dir = workspace
        .join(".stella")
        .join("plugins")
        .join("fleet-fixture");
    std::fs::create_dir_all(&dir).expect("plugin dir");
    std::fs::write(dir.join("plugin.toml"), PLUGIN_TOML).expect("plugin.toml");
    let script = dir.join("main.sh");
    std::fs::write(&script, PLUGIN_SCRIPT).expect("main.sh");
    let mut perms = std::fs::metadata(&script)
        .expect("script metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).expect("chmod +x");
    dir.join("rounds.log")
}

/// The witness a plan file's `test_command` names, and the artifact the host
/// pins before the attempt runs.
fn install_witness_script(workspace: &Path) {
    std::fs::create_dir_all(workspace.join("tests")).expect("tests dir");
    std::fs::write(
        workspace.join("tests").join("witness_flip.sh"),
        "#!/bin/sh\nexit 1\n",
    )
    .expect("witness script");
}

/// A real repository with one commit: `stella fleet` pins its base to a sha
/// (`git rev-parse --verify HEAD`) before it dispatches anything, so a tree
/// with no commit never reaches a worker at all.
fn init_repo(workspace: &Path) {
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(workspace)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    };
    git(&["init", "--quiet"]);
    git(&["config", "user.email", "fixture@example.invalid"]);
    git(&["config", "user.name", "Fixture"]);
    std::fs::write(workspace.join("README.md"), "fixture\n").expect("seed file");
    git(&["add", "README.md"]);
    git(&["commit", "--quiet", "-m", "seed"]);
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

/// The mock provider every worker turn talks to: one completion, no tool
/// calls, so the attempt finishes in a single step.
async fn mock_worker_provider() -> MockServer {
    let server = MockServer::start().await;
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

/// `<workspace>/.stella/private/store.db` — the shared-tree worker's own
/// store, which is the workspace's because its tree is.
fn store_path(workspace: &Path) -> PathBuf {
    workspace.join(".stella").join("private").join("store.db")
}

/// Run one hermetic `stella fleet …` against `server_uri`, with `fleet_args`
/// appended after the subcommand — the whole no-ambient-credentials envelope
/// in one place, so a second case cannot quietly inherit a real key by
/// forgetting one `env_remove`.
async fn run_fleet(
    workspace: &Path,
    data: &Path,
    server_uri: &str,
    fleet_args: &[&str],
) -> std::process::Output {
    let child = Command::new(env!("CARGO_BIN_EXE_stella"))
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
            "fleet",
        ])
        .args(fleet_args)
        .current_dir(workspace)
        .env("STELLA_HOME", data)
        .env("STELLA_DATA_DIR", data)
        // Same hermeticity discipline as `goal_wrapped_dispatch_cli.rs`: no
        // `.env`, no inherited real key, and the project plugin tier trusted
        // so the fixture wrapper actually loads.
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
        .expect("spawn stella fleet");

    tokio::task::spawn_blocking(move || child.wait_with_output())
        .await
        .expect("join")
        .expect("wait on stella")
}

/// The same run, held to exiting 0 — every case but the refusal one, where a
/// failed task is the assertion.
async fn run_fleet_ok(
    workspace: &Path,
    data: &Path,
    server_uri: &str,
    fleet_args: &[&str],
) -> std::process::Output {
    let output = run_fleet(workspace, data, server_uri, fleet_args).await;
    assert!(
        output.status.success(),
        "stella fleet --pipeline {VARIANT} did not exit 0 — stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

#[tokio::test]
async fn a_fleet_worker_dispatches_its_attempt_through_the_bound_wrapper() {
    let workspace = tempfile::tempdir().expect("workspace");
    let data = tempfile::tempdir().expect("data dir");
    init_repo(workspace.path());
    let rounds_log = install_fixture_wrapper(workspace.path());
    let server = mock_worker_provider().await;

    run_fleet_ok(
        workspace.path(),
        data.path(),
        &server.uri(),
        &["do the thing, then stop", "--pipeline", VARIANT],
    )
    .await;

    // The wrapper was genuinely asked about this attempt: one `before_turn`,
    // for the one task the plan carries.
    let log = std::fs::read_to_string(&rounds_log).unwrap_or_default();
    let calls: Vec<&str> = log.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(
        calls.len(),
        1,
        "expected exactly one before_turn call for the one dispatched task: {log}"
    );
    assert!(
        calls[0].contains("\"point\":\"before_turn\""),
        "logged call was not a before_turn request: {}",
        calls[0]
    );
    // …and it was handed the tree the attempt runs in, not `None` — the
    // candidate grant `crate::wrapper_candidate` mints per attempt.
    assert!(
        calls[0].contains("\"candidate\""),
        "the wrapper must receive a candidate grant naming this attempt's tree: {}",
        calls[0]
    );

    // The attempt's own execution row names the wrapper that drove it. NULL
    // here is what every `fleet` row could ever hold before this change.
    let conn = rusqlite::Connection::open(store_path(workspace.path())).expect("open store.db");
    let variant: Option<String> = conn
        .query_row(
            "SELECT pipeline_variant FROM executions WHERE kind = 'fleet' ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("one fleet execution row");
    assert_eq!(
        variant.as_deref(),
        Some(VARIANT),
        "executions.pipeline_variant must name the wrapper that drove this attempt"
    );
    // A positional prompt builds `Task::new`, which declares no oracle — so
    // the grant carries no test plan and the stage program says so, rather
    // than the host inventing a command nobody asked for (#3884).
    assert!(
        !calls[0].contains("\"program\""),
        "a task with no declared test command must be granted no test plan: {}",
        calls[0]
    );
    assert!(
        !log.contains("\"stage\":\"witness\""),
        "the `test-command` signal must be false, so the conditional stage is \
         programmed away: {log}"
    );
}

/// **Witness (#3884).** A plan file's per-task `test_command` reaches the
/// plugin as the attempt's `TestPlan`, and as the `test-command` signal the
/// stage program is written against — so a witness-flavoured wrapper has a
/// flip to observe on this door at all. Before this change
/// `bind_for_attempt` passed `None` unconditionally: the requests logged
/// below carried no `test` under their candidate whatever the plan said, the
/// conditional stage was programmed away every time, and every
/// witness-flavoured plugin reported `Undecided` on every fleet attempt.
#[tokio::test]
async fn a_plan_files_per_task_test_command_reaches_the_plugin_as_a_test_plan() {
    let workspace = tempfile::tempdir().expect("workspace");
    let data = tempfile::tempdir().expect("data dir");
    init_repo(workspace.path());
    install_witness_script(workspace.path());
    let rounds_log = install_fixture_wrapper(workspace.path());
    let server = mock_worker_provider().await;

    let plan = workspace.path().join("plan.json");
    std::fs::write(
        &plan,
        r#"{"tasks":[{"id":"witnessed","title":"decide me",
            "prompt":"do the thing, then stop",
            "test_command":"sh tests/witness_flip.sh"}]}"#,
    )
    .expect("plan file");

    run_fleet_ok(
        workspace.path(),
        data.path(),
        &server.uri(),
        &["--plan", "plan.json", "--pipeline", VARIANT],
    )
    .await;

    let log = std::fs::read_to_string(&rounds_log).unwrap_or_default();
    let calls: Vec<&str> = log.lines().filter(|line| !line.is_empty()).collect();
    // Two stages, because the `test-command` signal admitted the conditional
    // one — the signal and the grant are the same fact, read twice.
    assert_eq!(
        calls.len(),
        2,
        "expected a `witness` and an `execute` before_turn for the planned task: {log}"
    );
    assert!(
        log.contains("\"stage\":\"witness\""),
        "the stage conditional on `test-command` must be dispatched: {log}"
    );
    // The parsed argv, not the raw line: the command crossed the host's closed
    // runner vocabulary before anything reached the wire.
    for call in &calls {
        assert!(
            call.contains(r#""test":{"program":"sh","args":["tests/witness_flip.sh"]"#),
            "the plan's test command must ride the candidate grant as a parsed plan: {call}"
        );
    }
}

/// A `test_command` the host's runner vocabulary refuses fails **that task's**
/// dispatch by name — the fan-out reports it rather than swallowing it, and
/// the wrapper is never handed an invocation this host would not run.
#[tokio::test]
async fn a_refused_test_command_fails_that_tasks_dispatch_by_name() {
    let workspace = tempfile::tempdir().expect("workspace");
    let data = tempfile::tempdir().expect("data dir");
    init_repo(workspace.path());
    let rounds_log = install_fixture_wrapper(workspace.path());
    let server = mock_worker_provider().await;

    let plan = workspace.path().join("plan.json");
    std::fs::write(
        &plan,
        r#"{"tasks":[{"id":"refused","title":"not a runner",
            "prompt":"do the thing, then stop",
            "test_command":"rm -rf /"}]}"#,
    )
    .expect("plan file");

    let output = run_fleet(
        workspace.path(),
        data.path(),
        &server.uri(),
        &["--plan", "plan.json", "--pipeline", VARIANT],
    )
    .await;

    let seen = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        seen.contains("rm -rf /"),
        "the refusal must quote the command the plan declared: {seen}"
    );
    assert!(
        std::fs::read_to_string(&rounds_log)
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "no attempt may be dispatched on an oracle the host refused"
    );
}
