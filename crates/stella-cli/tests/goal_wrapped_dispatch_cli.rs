//! **Witness (#3695, goal half).** `stella goal --pipeline <variant>` used to
//! be refused unconditionally by `wrapper_plugin::reject_plugin_variant_for_door`
//! before this change — this file fails on the old binary for the plainest
//! possible reason (the process exits nonzero with "is not supported on
//! `stella goal` yet") and passes on this one: the door binds the installed
//! wrapper once and dispatches every judged round's worker turn through it,
//! and `executions.pipeline_variant` names it for the whole run.
//!
//! Deliberately a real subprocess against a real (mocked) HTTP endpoint,
//! matching `run_exits_cli.rs`'s hermeticity discipline — no ambient
//! credentials, `--base-url` pointed at a `wiremock` server this test drives
//! itself — because the property under test (the wrapper's `before_turn`
//! firing once per goal round, and the store row it lands on) only exists
//! once the goal loop has genuinely run two rounds end to end; nothing about
//! it is visible from a unit test that never builds a real `Engine::run_turn`
//! transcript.
//!
//! `cfg(unix)`: the fixture wrapper is a `/bin/sh` script, same reason
//! `stella-runtime`'s `tests/wrapper_dispatch.rs` is unix-only.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

/// The `[wrapper] id` the fixture declares, and what must land in
/// `executions.pipeline_variant`.
const VARIANT: &str = "goal-fixture-v1";

/// No `[requirements]`/`[oracle]` at all: `judge` is `Verdict::Met` for an
/// empty rule (`crates/stella-runtime/src/wrapper/verdict.rs`'s own doc,
/// "a rule with no requirements is `Verdict::Met`"), so each call into
/// `WrapperDispatch::run` drives exactly one internal turn — the property
/// `run_goal_wrapped_turn` depends on (`DispatchReport::rounds == 1`) rather
/// than guesses at.
///
/// It is also the only shape this door accepts, which is worth knowing before
/// reading the `--test-command` assertions below: `[requirements]` is refused
/// below `participation = "arbiter"`
/// (`ManifestError::RequirementsRequireArbiter`) and `stella goal` refuses
/// arbiter grade outright (#3832), so no plugin on this door can carry an
/// `[oracle]` for the host's tamper finding to qualify. What #3835 wired here
/// is the grant and the watch that feed such an oracle; a run that *decides*
/// on one is not reachable from this door until those two rules stop
/// contradicting.
const PLUGIN_TOML: &str = r#"
name = "goal-fixture"
[loop]
participation = "steering"
points = ["before_turn", "after_turn"]
[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh", "${plugin_dir}/rounds.log", "${plugin_dir}/after.log"]
timeout_secs = 30
env = ["PATH"]
[wrapper]
id = "goal-fixture-v1"
[[wrapper.stages]]
name = "execute"
"#;

/// Every call appends the exact request it was asked (one line — the wire
/// framing is compact JSON, never pretty-printed) to its point's own log:
/// `before_turn` to `$1`, so the test can count how many rounds actually
/// dispatched through it, and `after_turn` to `$2`, which is where the turn
/// facts the host reports back (#3834) are read from.
///
/// `after_turn` answers with an empty, `flip = "not-attempted"` observation
/// (nothing in this fixture's manifest declares an oracle that would read
/// it).
const PLUGIN_SCRIPT: &str = r#"#!/bin/sh
input=$(cat)
case "$input" in
  *'"point":"after_turn"'*)
    printf '%s\n' "$input" >> "$2"
    printf '%s\n' '{"point":"after_turn","body":{"protocol_version":1,"evidence":{"flip":"not-attempted"}}}'
    ;;
  *)
    printf '%s\n' "$input" >> "$1"
    printf '%s\n' '{"point":"before_turn","body":{"protocol_version":1}}'
    ;;
esac
"#;

/// A [`Match`] wiremock has no built-in for: "the body does NOT contain this
/// substring". Needed to route the worker's calls away from the verifier's
/// mocks without matching on the (large, prompt-engineering-internal, and
/// therefore fragile) worker system prompt instead.
struct NotContains(&'static str);

impl Match for NotContains {
    fn matches(&self, request: &Request) -> bool {
        !String::from_utf8_lossy(&request.body).contains(self.0)
    }
}

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
/// the watch entirely rather than pinned as missing — which would leave the
/// watch empty, the finding `NotChecked`, and the verdict undecided for a
/// reason that has nothing to do with what this test is about.
fn install_witness(workspace: &Path) {
    let tests = workspace.join("tests");
    std::fs::create_dir_all(&tests).expect("tests dir");
    std::fs::write(tests.join("witness_flip.sh"), "#!/bin/sh\nexit 1\n").expect("the witness");
}

/// The oracle `--test-command` arms, and the one artifact the host pins from
/// it. `sh <path>` names its file syntactically, which is the only shape
/// `wrapper_candidate::named_artifacts` will watch — `cargo test --test x`
/// names `x`, not `tests/x.rs`, and the host does not guess.
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

/// A mock chat-completions endpoint standing in for the real provider, wired
/// to make the goal loop run exactly two rounds: the round-1 verifier call
/// answers `met: false` (forcing a second round), the round-2 verifier call
/// answers `met: true` (ending the loop).
///
/// The two verifier responses are distinguished by whether the request body
/// already carries `stella_core::goal::verifier_feedback_text`'s "NOT yet
/// met" wording — present only once the round-1 verdict has been pushed onto
/// the transcript the round-2 verifier reads. Verifier calls themselves are
/// distinguished from worker calls by `goal::VERIFIER_SYSTEM_PROMPT`'s
/// opening words, which never reach the worker's own (unrelated) system
/// prompt.
async fn mock_two_round_goal_loop() -> MockServer {
    let server = MockServer::start().await;

    // The very first worker call answers with a tool call rather than text,
    // so round 1's turn genuinely dispatches something and the facts the host
    // folds are non-trivial rather than merely present (#3834).
    //
    // `get_environment` is the tool chosen for what it is *not*: it takes no
    // arguments, changes nothing, and its `Always` authority means no approval
    // gate stands between this fixture and a dispatched call — a headless goal
    // run has no human to answer one.
    //
    // Higher priority (a lower number) than the general worker mock below,
    // and capped at one use, so the second worker call of round 1 falls
    // through to the text answer and the turn ends.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(NotContains("impartial verifier"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(sse_tool_call(), "text/event-stream"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;

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
        .and(NotContains("NOT yet met"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse_completion(r#"{"met": false, "reasoning": "not yet", "feedback": "keep going"}"#),
            "text/event-stream",
        ))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("impartial verifier"))
        .and(body_string_contains("NOT yet met"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            sse_completion(r#"{"met": true, "reasoning": "done", "feedback": ""}"#),
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
async fn a_goal_run_dispatches_each_round_through_the_bound_wrapper() {
    let workspace = tempfile::tempdir().expect("workspace");
    let data = tempfile::tempdir().expect("data dir");
    // The user tier, which `STELLA_DATA_DIR` does not move.
    //
    // Removing every `*_API_KEY` below is not hermeticity on its own: an
    // environment variable is one of two ways a credential reaches the
    // process, and `~/.stella/credentials.toml` is the other. On a developer
    // machine with providers configured, this test discovered them through
    // that file, and the comment below is exactly right about why it matters —
    // the goal loop's cross-family verifier takes *whatever* is configured. So
    // the run bound a real OpenRouter verifier against a real zai worker,
    // spent $0.08 of the developer's money per invocation, and then failed,
    // because a live model does not answer like the wiremock fixture this test
    // drives. `STELLA_HOME` moves the whole user tier, so there is no
    // credentials file to find.
    let home = tempfile::tempdir().expect("stella home");
    let (rounds_log, after_log) = install_fixture_wrapper(workspace.path());
    install_witness(workspace.path());
    let server = mock_two_round_goal_loop().await;

    let child = Command::new(env!("CARGO_BIN_EXE_stella"))
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
        // Every other family's credential is removed too, not only the two
        // `run_exits_cli.rs` guards against: the goal loop's cross-family
        // verifier routing (`resolve_cross_family_verifier`) discovers
        // *whatever* is configured, and this sandbox's own tooling injects
        // AWS credentials for an unrelated proxy — which resolved a
        // "bedrock" verifier and made a real (unmocked, failing) network
        // call the first time this test ran. A single configured family
        // (zai, via `--api-key` above) is what keeps the verifier routed to
        // the SAME mock server the worker uses.
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

    assert!(
        output.status.success(),
        "stella goal --pipeline {VARIANT} did not exit 0 — stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // `before_turn` fired once per goal round — the wrapper was genuinely
    // dispatched twice, not once, which is the only way two rounds happened
    // at all under a manifest with no `[loop] max_holds` of its own (the
    // wire's own `round` field cannot show this: it restarts at 0 on every
    // `WrapperDispatch::run` call, one per goal round, by design — see
    // `crates/stella-cli/src/agent/goal/goal_wrapped.rs`'s module doc).
    let log = std::fs::read_to_string(&rounds_log).unwrap_or_default();
    let calls: Vec<&str> = log.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(
        calls.len(),
        2,
        "expected exactly one before_turn call per goal round (2 rounds): {log}"
    );
    for call in &calls {
        assert!(
            call.contains("\"point\":\"before_turn\""),
            "logged call was not a before_turn request: {call}"
        );
        // **Witness (#3835).** Every round carries the grant over the tree it
        // runs in, with `--test-command` parsed into the plan the plugin's
        // oracle would run. This door sent `candidate: None` before, and
        // `Option::None` is skipped on the wire — so the field is simply
        // absent in the old binary's request, and a plugin had no root to read
        // and no test to run.
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
    // `GoalRoundDriver` drove `Engine::run_turn` directly and folded nothing,
    // so it sent `tools: None, changed_files: None` on every round — and both
    // are `skip_serializing_if = "Option::is_none"`, so the old binary's
    // request simply has no such keys. That is honest under the wire contract
    // ("this host does not report it") and strictly less than `stella run`'s
    // wrapped door already gave the same plugin.
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
        "expected one after_turn call per goal round (2 rounds): {after}"
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

    // The tamper finding the host now pins beside that grant is not
    // observable from here, and the reason is a fact about this door rather
    // than about the wiring: a finding only changes a verdict through an
    // `[oracle]`, an oracle needs `[requirements]`, `[requirements]` needs
    // `participation = "arbiter"`, and this door refuses arbiter grade
    // (#3832). `crates/stella-cli/src/wrapper_candidate.rs`'s own tests are
    // where `Clean` and `Tampered` are witnessed.

    // The execution row this goal run opened names the wrapper that drove
    // it, and the store shows two rounds' worth of turn_instance under that
    // one row — the goal loop's own round math (`stella_core::turn_slots`'
    // worker lane, with the verifier lane beside it), advancing exactly as it
    // does on the raw arm.
    let conn = rusqlite::Connection::open(store_path(workspace.path())).expect("open store.db");
    let (execution_id, variant): (i64, Option<String>) = conn
        .query_row(
            "SELECT id, pipeline_variant FROM executions WHERE kind = 'goal' ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("one goal execution row");
    assert_eq!(
        variant.as_deref(),
        Some(VARIANT),
        "executions.pipeline_variant must name the wrapper that drove every round"
    );

    let mut stmt = conn
        .prepare("SELECT DISTINCT turn_instance FROM step_receipt WHERE execution_id = ?1 ORDER BY turn_instance")
        .expect("prepare");
    let turn_instances: Vec<i64> = stmt
        .query_map([execution_id], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert!(
        turn_instances.len() >= 4,
        "two goal rounds (worker + verifier each) must claim four distinct turn_instance \
         slots under the one execution row the wrapper drove: {turn_instances:?}"
    );
    // Round 1 is worker lane 0 / verifier lane 1; round 2 is the next slot of
    // each, which is four along rather than two — the two slots between them
    // are the host's own lanes, reserved so a wrapper plugin's child turns can
    // run beside this loop without overwriting a round (#3833).
    assert!(
        turn_instances.starts_with(&[0, 1, 4, 5]),
        "goal rounds must allocate through `stella_core::turn_slots`' worker and verifier \
         lanes: {turn_instances:?}"
    );
}
