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

/// One `stella run`, returning its exit status and stderr.
///
/// The hermeticity discipline is `goal_wrapped_dispatch_cli.rs`'s, and for the
/// same reason: `STELLA_HOME` moves the user tier so no
/// `~/.stella/credentials.toml` is discoverable, and every family's key is
/// removed from the environment, so this test cannot reach a real provider
/// with the developer's money.
fn run(workspace: &Path, base_url: &str, extra: &[&str]) -> (Option<i32>, String) {
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
        "run",
        "do the thing, then stop",
    ];
    args.extend_from_slice(extra);

    let output = Command::new(env!("CARGO_BIN_EXE_stella"))
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
        // The session's code-graph build warms a semantic index, and
        // `stella_embed::EmbedderEnv::from_process` resolves an HTTP backend
        // from whichever of these is set (`stella-embed/src/http.rs`). Left
        // inherited, a developer with `VOYAGE_API_KEY` exported has this test
        // calling api.voyageai.com on their account — which is what happened
        // the first time it ran here.
        .env_remove("VOYAGE_API_KEY")
        .env_remove("STELLA_EMBED_URL")
        .env_remove("STELLA_EMBED_MODEL")
        .env_remove("STELLA_EMBED_API_KEY")
        .output()
        .expect("spawn stella");

    (
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
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
