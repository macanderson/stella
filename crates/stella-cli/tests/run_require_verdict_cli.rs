//! **Witness (#3554, the exit-status half).** A wrapper plugin's `Unmet`
//! outcome decides `stella run`'s exit status — but only when the caller asks
//! for it.
//!
//! Before this change `wrapper_plugin::run_wrapped` returned the last round's
//! turn result and nothing else, so a `DispatchReport` whose outcome was
//! `Unmet` printed to stderr and exited `0`: there was no way to make a
//! plugin's refusal fail a delivery gate. `--require-verified`, the flag that
//! shape of user reaches for, was wired to the deleted staged pipeline's
//! ladder and is refused unconditionally (#3865).
//!
//! Three directions, because the default is as much of the decision as the
//! flag is — installing a third party's manifest must not, by itself, gain
//! the power to fail somebody's build:
//!
//! - the same unmet run without the flag still exits `0`;
//! - with `--require-verdict` it exits non-zero and says which flag did it;
//! - `--require-verdict` with no `--pipeline` is refused rather than
//!   accepted and silently ignored.
//!
//! #4543 took the flag to the other two wrapper-driving doors, witnessed
//! here in the same three directions where each door can reach them:
//! `stella fleet` gates PER ATTEMPT (an unmet verdict fails that attempt,
//! and a failed attempt fails the run), and `stella goal` reads the LAST
//! round's verdict — vacuously `Met` today, since this door refuses the
//! arbiter grade that could carry a rule (#3832), so its witnesses pin the
//! refusal without `--pipeline` and the accepted arm's exit-by-the-goal's-
//! own-result, both of which fail on a binary without the flag.
//!
//! Deliberately a real subprocess against a real (mocked) HTTP endpoint,
//! matching `run_exits_cli.rs` and `goal_wrapped_dispatch_cli.rs`: the
//! property is the process's exit status, which no unit test can observe.
//!
//! `cfg(unix)`: the fixture wrapper is a `/bin/sh` script, same reason
//! `goal_wrapped_dispatch_cli.rs` is unix-only.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::SealsEmbedderBackend;

/// The `[wrapper] id` the fixture declares, and what `--pipeline` names.
const VARIANT: &str = "verdict-fixture-v1";

/// An arbiter with one requirement its oracle decides on a witness flip, and
/// no allowance to hold the completion open — so the dispatch reaches a
/// verdict after exactly one turn and that verdict is `Unmet`.
///
/// Arbiter grade is what makes `[requirements]` admissible at all
/// (`ManifestError::RequirementsRequireArbiter`), and `stella run` is the door
/// that accepts it — `stella goal` refuses arbiter outright (#3832).
const PLUGIN_TOML: &str = r#"
name = "verdict-fixture"
[loop]
participation = "arbiter"
# The grade's own requirement: a completion verdict is what arbiter grants, and
# an undeclared hook is never invoked.
hooks = ["Stop"]
points = ["after_turn"]
# The floor: "an arbiter that can never hold is not an arbiter". One hold, then
# the allowance is spent and the loop stops with `proven` still unmet.
max_holds = 1

[requirements]
proven = "a witness test failed before the change and passes after it"

[oracle]
flip = "required"

[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh"]
timeout_secs = 30
env = ["PATH"]

[wrapper]
id = "verdict-fixture-v1"
[[wrapper.stages]]
name = "execute"
"#;

/// The plugin answers `after_turn` with a witness that ran on both sides and
/// did not flip — the observation that leaves `proven` genuinely **unmet**
/// rather than undecided. (`not-attempted` and `unobservable` both make
/// `judge` abstain instead, because each is a claim about the instrument.) It
/// reports honestly and the host believes it; nothing here is a failure of the
/// plugin.
const PLUGIN_SCRIPT: &str = r#"#!/bin/sh
cat >/dev/null
printf '%s\n' '{"point":"after_turn","body":{"protocol_version":1,"evidence":{"flip":"not-achieved"}}}'
"#;

/// One SSE completion in the shape `stella-model`'s shared chat-completions
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

/// Write the fixture wrapper into the project plugin tier.
fn install_fixture_wrapper(workspace: &Path) {
    let dir = workspace
        .join(".stella")
        .join("plugins")
        .join("verdict-fixture");
    std::fs::create_dir_all(&dir).expect("plugin dir");
    std::fs::write(dir.join("plugin.toml"), PLUGIN_TOML).expect("plugin.toml");
    let script = dir.join("main.sh");
    std::fs::write(&script, PLUGIN_SCRIPT).expect("main.sh");
    let mut perms = std::fs::metadata(&script)
        .expect("script metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).expect("chmod +x");
}

/// Every worker call answers with text, so the turn ends on its first step and
/// the run's whole content is the wrapper's verdict over it.
async fn mock_worker() -> MockServer {
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

/// One hermetic `stella` invocation: the door's own arguments after the
/// session flags, exit status plus both streams back.
///
/// The hermeticity discipline is `goal_wrapped_dispatch_cli.rs`'s, and for the
/// same reason: `STELLA_HOME` moves the user tier so no
/// `~/.stella/credentials.toml` is discoverable, and every family's key is
/// removed from the environment, so this test cannot reach a real provider
/// with the developer's money. The embedding backend the session's code-graph
/// build warms is sealed by `without_embedder_backend` — this test spelled
/// that removal out by hand until the shared helper landed (#4542).
fn stella(workspace: &Path, base_url: &str, door: &[&str]) -> (Option<i32>, String, String) {
    let data = tempfile::tempdir().expect("data dir");
    let home = tempfile::tempdir().expect("stella home");
    let mut args: Vec<&str> = vec![
        "--model",
        "zai/glm-5.2",
        "--api-key",
        "sk-test-zai",
        "--base-url",
        base_url,
        "--spend-limit",
        "5.0",
    ];
    args.extend_from_slice(door);

    let output = Command::new(env!("CARGO_BIN_EXE_stella"))
        .without_embedder_backend()
        .args(&args)
        .current_dir(workspace)
        .env("STELLA_HOME", home.path())
        .env("STELLA_DATA_DIR", data.path())
        .env("STELLA_NO_ENV_FILE", "1")
        .env("STELLA_TRUST_PROJECT", "1")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("ZAI_API_KEY")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("XAI_API_KEY")
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .env_remove("AWS_SESSION_TOKEN")
        .output()
        .expect("spawn stella");

    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// One `stella run`, returning its exit status and stderr.
fn run(workspace: &Path, base_url: &str, extra: &[&str]) -> (Option<i32>, String) {
    let mut door: Vec<&str> = vec!["run", "do the thing, then stop"];
    door.extend_from_slice(extra);
    let (code, _, stderr) = stella(workspace, base_url, &door);
    (code, stderr)
}

/// A workspace with the fixture wrapper installed and one source file, so the
/// session's code-graph build has something real to index.
fn workspace() -> tempfile::TempDir {
    let workspace = tempfile::tempdir().expect("workspace");
    install_fixture_wrapper(workspace.path());
    std::fs::write(workspace.path().join("lib.rs"), "pub fn hello() {}\n").expect("source file");
    workspace
}

#[tokio::test]
async fn an_unmet_verdict_fails_the_run_only_when_the_caller_asks() {
    let server = mock_worker().await;

    // Without the flag: the wrapper's refusal is reported and the process
    // still exits on the turn's own result. This is the half a third party's
    // manifest must never be able to change by itself.
    let ws = workspace();
    let (code, stderr) = run(ws.path(), &server.uri(), &["--pipeline", VARIANT]);
    assert_eq!(
        code,
        Some(0),
        "an unmet verdict is reported, not fatal, without --require-verdict: {stderr}"
    );

    // With it: the same run fails, and says which flag decided that.
    let ws = workspace();
    let (code, stderr) = run(
        ws.path(),
        &server.uri(),
        &["--pipeline", VARIANT, "--require-verdict"],
    );
    assert_ne!(
        code,
        Some(0),
        "--require-verdict must turn an unmet verdict into a failing exit: {stderr}"
    );
    assert!(
        stderr.contains("--require-verdict"),
        "the failure names the flag that caused it: {stderr}"
    );
    assert!(
        stderr.contains("proven"),
        "and the requirement that was left unmet: {stderr}"
    );
}

/// On the raw loop nothing declares a verdict, so the flag has nothing to
/// read. Accepting it there would be the silent drop CLAUDE.md forbids: the
/// caller asked for a delivery gate and would get an unconditional exit 0.
#[test]
fn require_verdict_is_refused_without_a_wrapper() {
    let ws = workspace();
    let (code, stderr) = run(ws.path(), "http://127.0.0.1:1", &["--require-verdict"]);
    assert_ne!(code, Some(0), "the refusal is a failure: {stderr}");
    assert!(
        stderr.contains("--require-verdict"),
        "and it names the flag: {stderr}"
    );
}

/// [`workspace`] as a git repository with one commit — `stella fleet` pins
/// its base to a sha before dispatching anything.
fn fleet_workspace() -> tempfile::TempDir {
    let ws = workspace();
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.email=fleet@test",
            "-c",
            "user.name=fleet",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        let status = Command::new("git")
            .args(&args)
            .current_dir(ws.path())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }
    ws
}

/// **Witness (#4543, the fleet door).** The same arbiter fixture, driven as
/// one fleet attempt: without the flag its unmet verdict is reported and the
/// run exits on the attempt's own result; with `--require-verdict` the
/// attempt fails by name — and a failed attempt fails the run, the rule
/// every failed task already follows.
#[tokio::test]
async fn an_unmet_verdict_fails_a_fleet_attempt_only_when_the_caller_asks() {
    let server = mock_worker().await;

    let ws = fleet_workspace();
    let (code, stdout, stderr) = stella(
        ws.path(),
        &server.uri(),
        &["fleet", "--pipeline", VARIANT, "do the thing, then stop"],
    );
    assert_eq!(
        code,
        Some(0),
        "an unmet verdict is reported, not fatal, without --require-verdict: \
         {stdout}\n{stderr}"
    );

    let ws = fleet_workspace();
    let (code, stdout, stderr) = stella(
        ws.path(),
        &server.uri(),
        &[
            "fleet",
            "--pipeline",
            VARIANT,
            "--require-verdict",
            "do the thing, then stop",
        ],
    );
    assert_ne!(
        code,
        Some(0),
        "--require-verdict must turn an unmet attempt verdict into a failing \
         run: {stdout}\n{stderr}"
    );
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("--require-verdict"),
        "the failure names the flag that caused it: {combined}"
    );
    assert!(
        combined.contains("proven"),
        "and the requirement that was left unmet: {combined}"
    );
}

/// The fleet door's raw arm refuses the flag exactly as `stella run`'s does:
/// no `--pipeline`, no verdict, no silent drop.
#[test]
fn fleet_refuses_require_verdict_without_a_wrapper() {
    let ws = fleet_workspace();
    let (code, stdout, stderr) = stella(
        ws.path(),
        "http://127.0.0.1:1",
        &["fleet", "--require-verdict", "do the thing, then stop"],
    );
    assert_ne!(
        code,
        Some(0),
        "the refusal is a failure: {stdout}\n{stderr}"
    );
    assert!(
        stderr.contains("--require-verdict"),
        "and it names the flag: {stderr}"
    );
}

/// A steering-grade fixture for the goal door, which refuses the arbiter
/// grade [`PLUGIN_TOML`] declares (#3832). No `[requirements]`, no
/// `[oracle]` — the only shape this door admits — so `judge` answers `Met`
/// on an empty rule and the gate below can only ever pass today. The wiring
/// is still the witnessable half: the flag parses, reads the last round's
/// report, and exits by the goal's own result.
const GOAL_PLUGIN_TOML: &str = r#"
name = "goal-verdict-fixture"
[loop]
participation = "steering"
points = ["before_turn", "after_turn"]
[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh"]
timeout_secs = 30
env = ["PATH"]
[wrapper]
id = "goal-verdict-fixture-v1"
[[wrapper.stages]]
name = "execute"
"#;

/// Answer each point with the smallest valid body; `after_turn` reports an
/// observation nothing in the manifest reads.
const GOAL_PLUGIN_SCRIPT: &str = r#"#!/bin/sh
input=$(cat)
case "$input" in
  *'"point":"after_turn"'*)
    printf '%s\n' '{"point":"after_turn","body":{"protocol_version":1,"evidence":{"flip":"not-attempted"}}}'
    ;;
  *)
    printf '%s\n' '{"point":"before_turn","body":{"protocol_version":1}}'
    ;;
esac
"#;

/// Install the goal fixture beside the arbiter one.
fn install_goal_fixture(workspace: &Path) {
    let dir = workspace
        .join(".stella")
        .join("plugins")
        .join("goal-verdict-fixture");
    std::fs::create_dir_all(&dir).expect("plugin dir");
    std::fs::write(dir.join("plugin.toml"), GOAL_PLUGIN_TOML).expect("plugin.toml");
    let script = dir.join("main.sh");
    std::fs::write(&script, GOAL_PLUGIN_SCRIPT).expect("main.sh");
    let mut perms = std::fs::metadata(&script)
        .expect("script metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).expect("chmod +x");
}

/// A goal loop that ends after one round: the worker answers text, the
/// verifier — told apart by its own system prompt's wording, which never
/// reaches the worker's — answers met on the first ask.
async fn mock_one_round_goal_loop() -> MockServer {
    use wiremock::matchers::body_string_contains;

    /// wiremock's missing negation: route worker calls away from the
    /// verifier's mock without matching on the (fragile) worker prompt.
    struct NotContains(&'static str);
    impl wiremock::Match for NotContains {
        fn matches(&self, request: &wiremock::Request) -> bool {
            !String::from_utf8_lossy(&request.body).contains(self.0)
        }
    }

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(NotContains("impartial verifier"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse_completion("did the requested work"),
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("impartial verifier"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse_completion(r#"{"met": true, "reasoning": "done", "feedback": ""}"#),
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    server
}

/// **Witness (#4543, the goal door).** `stella goal` takes the flag and, with
/// a wrapper bound, exits by the goal's own result — the last round's verdict
/// is `Met` for every wrapper this door admits (see [`GOAL_PLUGIN_TOML`]), so
/// the gate passes and a met goal still exits `0`. Fails on a binary without
/// the flag: clap refuses `--require-verdict` outright.
#[tokio::test]
async fn goal_takes_the_flag_and_exits_by_the_goals_own_result() {
    let server = mock_one_round_goal_loop().await;
    let ws = tempfile::tempdir().expect("workspace");
    install_goal_fixture(ws.path());
    std::fs::write(ws.path().join("lib.rs"), "pub fn hello() {}\n").expect("source file");

    let (code, stdout, stderr) = stella(
        ws.path(),
        &server.uri(),
        &[
            "goal",
            "--pipeline",
            "goal-verdict-fixture-v1",
            "--require-verdict",
            "say the work is done, then stop",
        ],
    );
    assert_eq!(
        code,
        Some(0),
        "a met goal whose last-round verdict is Met exits 0 under the flag: \
         {stdout}\n{stderr}"
    );
}

/// The goal door's raw arm refuses the flag exactly as `stella run`'s does.
/// Fails on a binary without the flag too, but for clap's reason rather than
/// the gate's — so the assertion pins the refusal's own wording.
#[test]
fn goal_refuses_require_verdict_without_a_wrapper() {
    let ws = tempfile::tempdir().expect("workspace");
    let (code, stdout, stderr) = stella(
        ws.path(),
        "http://127.0.0.1:1",
        &[
            "goal",
            "--require-verdict",
            "say the work is done, then stop",
        ],
    );
    assert_ne!(
        code,
        Some(0),
        "the refusal is a failure: {stdout}\n{stderr}"
    );
    assert!(
        stderr.contains("--require-verdict has no verdict to read on the raw loop"),
        "the refusal is the gate's, not the parser's: {stderr}"
    );
}
