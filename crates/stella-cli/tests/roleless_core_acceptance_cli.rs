//! `doc:roleless-core` §9's acceptance script, as a test that runs it.
//!
//! §2 states the sentence the epic is measured by:
//!
//! > Install a triage plugin. Activate it. With no other configuration, the
//! > only thing that changes is that the turn now has a triage stage, and the
//! > default model performs it.
//!
//! §9 turns that into steps. This file runs them against the real binary:
//!
//! 1. install a plugin with one stage and one role. Run a turn with the
//!    plugin switched **off**. This is the control.
//! 2. switch it on in `active_plugins`. Run the same turn.
//! 3. assign a model to `<plugin>/<role>` in `seat_models`. Run it again.
//!
//! Two comparisons decide it:
//!
//! - step 2 against step 1. The stage list gained the plugin's stage. The
//!   count of distinct models did not change.
//! - step 3 against step 2. The count of distinct models changed. The stage
//!   list did not.
//!
//! # What each observation is read from
//!
//! **The stage list** is the `stage` field of each `before_turn` request the
//! host sent the plugin. The fixture appends every request to a log. The
//! plugin declares no `[loop] before_turn_stages`, so the host asks it at
//! every stage the program holds. The log is the whole stage list. The
//! control has no line at all: a plugin that is installed and not switched on
//! is never spawned (§8.1).
//!
//! Not the `stage` **events**. No host emits one for a stage a plugin
//! contributed, so the event stream cannot answer this yet. `#6261` tracks
//! that gap.
//!
//! **The model census** is `SELECT DISTINCT call_role, model FROM telemetry`
//! over the run's own store. That table is the store's copy of `step_usage`,
//! and the census AGENTS.md sends a bench reader to run. Each step runs in a
//! workspace of its own, so the whole table is that step's run.
//!
//! # No live model and no spend
//!
//! One `wiremock` server answers every call. It serves the worker turn and
//! the plugin's child turn alike. A scripted provider still answers both of
//! §9's questions: did the composition change, and how many distinct models
//! were asked. Neither one is about what a model said.
//!
//! `cfg(unix)`: the fixture plugin is a `/bin/sh` script. That is why
//! `goal_wrapped_dispatch_cli.rs` beside it is unix-only too.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::SealsEmbedderBackend;

/// The plugin's name — the namespace half of every seat key it contributes.
const PLUGIN: &str = "acceptance-fixture";

/// The `[wrapper] id`, and what `executions.pipeline_variant` must name once
/// the plugin is switched on.
const VARIANT: &str = "acceptance-v1";

/// The contributed stage. Not a word the host answers to. That is what makes
/// it the plugin's own, and not a reshuffle of stages the host already had.
const STAGE: &str = "acceptance-review";

/// The plugin's own word for the participant its process needs.
const ROLE: &str = "reviewer";

/// The session's model. Seeded, so no catalog lookup can refuse it.
const SESSION_MODEL: &str = "glm-5.2";

/// The model assigned to the seat in step 3. Also seeded, and on the same
/// provider, so one mock server serves both. The assignment is then the only
/// difference between the two runs.
const SEAT_MODEL: &str = "glm-4.5-air";

/// The seat key the user writes and the host asks for: `<plugin>/<role>`
/// (`doc:roleless-core` §8.4). The plugin never spells the prefix.
fn seat_key() -> String {
    format!("{PLUGIN}/{ROLE}")
}

/// One stage, one role, one host call — the smallest manifest §9's sentence
/// can be run against.
///
/// `participation = "steering"` is the grade a `[wrapper]` needs.
/// `calls = ["child_turn"]` lets the plugin ask the host to run a turn at its
/// declared role. Without that call, nothing ever spends at the role, and
/// step 3 would have nothing to change.
const PLUGIN_TOML: &str = r#"
name = "acceptance-fixture"

[loop]
participation = "steering"
points = ["before_turn"]
calls = ["child_turn"]

[roles.reviewer]
tier = "research"

[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh", "${plugin_dir}/stages.log", "${plugin_dir}/child.log"]
timeout_secs = 60
env = ["PATH"]

[wrapper]
id = "acceptance-v1"

[[wrapper.stages]]
name = "acceptance-review"
band = "early"
"#;

/// The plugin's whole program: log the request, ask the host for one child
/// turn at its declared role, log the answer, and end the point.
///
/// `$1` is the stage log and `$2` the child-turn log. The test reads both.
/// The first is the stage list. The second turns a refused child turn into a
/// named failure, rather than a census that quietly came out one model short.
const PLUGIN_SCRIPT: &str = r#"#!/bin/sh
read -r request
printf '%s\n' "$request" >> "$1"
printf '%s\n' '{"call":"child_turn","id":1,"args":{"role":"reviewer","instruction":"reply with the word ok"}}'
read -r answer
printf '%s\n' "$answer" >> "$2"
printf '%s\n' '{"point":"before_turn","body":{"protocol_version":1}}'
"#;

/// What one step of §9 observed.
#[derive(Debug)]
struct Observed {
    /// Which of the four steps this is, for the printed report.
    label: &'static str,
    /// The stage names the turn dispatched the plugin at, in order.
    stages: Vec<String>,
    /// The distinct models this run asked, sorted.
    models: Vec<String>,
    /// The `(call_role, model)` census, sorted.
    census: Vec<(String, String)>,
    /// What `executions.pipeline_variant` recorded for the turn.
    wrapper_id: Option<String>,
    /// The host's answer to each `child_turn` the plugin asked for.
    child_answers: Vec<String>,
}

impl Observed {
    /// Print what this step saw, so a reader gets the numbers and not just
    /// the verdict.
    fn report(&self) {
        println!("--- {} ---", self.label);
        println!("  stage list      : {:?}", self.stages);
        println!("  wrapper id      : {:?}", self.wrapper_id);
        println!("  distinct models : {:?}", self.models);
        println!("  (role, model)   : {:?}", self.census);
        for answer in &self.child_answers {
            println!("  child turn      : {answer}");
        }
    }
}

/// The seat map a step runs with.
enum Seats {
    /// No `seat_models` key at all — every seat runs on the session's model.
    Unassigned,
    /// One model assigned to this plugin's declared seat.
    Assigned,
}

/// One SSE completion, in the shape the shared chat-completions adapter
/// parses. One content delta with the whole answer, a usage frame, then
/// `[DONE]`. Copied from `crates/stella-model/src/zai/tests.rs`.
fn sse_completion(text: &str) -> String {
    let content = serde_json::to_string(text).expect("text encodes");
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{content}}}}}]}}\n\n\
         data: {{\"choices\":[{{\"delta\":{{}}}}],\"usage\":{{\"prompt_tokens\":8,\
         \"completion_tokens\":3}}}}\n\n\
         data: [DONE]\n\n"
    )
}

/// A provider that answers every call with the same short text.
///
/// One mock for the worker turn and the plugin's child turn alike. The census
/// reads which model each of them asked for. The seat map decides that, not
/// anything the server says.
async fn mock_provider() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(sse_completion("ok"), "text/event-stream"),
        )
        .mount(&server)
        .await;
    server
}

/// Write the fixture plugin into the project plugin tier and return its
/// directory.
fn install_plugin(workspace: &Path) -> PathBuf {
    let dir = workspace.join(".stella").join("plugins").join(PLUGIN);
    std::fs::create_dir_all(&dir).expect("plugin dir");
    std::fs::write(dir.join("plugin.toml"), PLUGIN_TOML).expect("plugin.toml");
    let script = dir.join("main.sh");
    std::fs::write(&script, PLUGIN_SCRIPT).expect("main.sh");
    let mut perms = std::fs::metadata(&script)
        .expect("script metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).expect("chmod +x");
    dir
}

/// The settings this step runs under.
///
/// A seat resolves through the provider entry. `resolve_seat_models` builds
/// its adapter from that provider's own base URL, not from `--base-url`.
/// Without the entry the seat would call the real endpoint. Both point at the
/// mock here.
fn settings_json(base_url: &str, active: bool, seats: &Seats) -> String {
    let mut settings = serde_json::json!({
        "providers": {
            "zai": {"api_key": "sk-acceptance-fixture", "base_url": base_url}
        }
    });
    if active {
        settings["active_plugins"] = serde_json::json!([PLUGIN]);
    }
    if matches!(seats, Seats::Assigned) {
        // Built up rather than written with `json!`, because the seat key is a
        // value this file computes and an object key there has to be a literal.
        let mut assignments = serde_json::Map::new();
        assignments.insert(
            seat_key(),
            serde_json::Value::String(format!("zai/{SEAT_MODEL}")),
        );
        let mut engine = serde_json::Map::new();
        engine.insert(
            "seat_models".to_owned(),
            serde_json::Value::Object(assignments),
        );
        settings["agent_engine_config"] = serde_json::Value::Object(engine);
    }
    serde_json::to_string_pretty(&settings).expect("settings encode")
}

/// `<workspace>/.stella/private/store.db` — this workspace's own store, which
/// `STELLA_DATA_DIR` does not move.
fn store_path(workspace: &Path) -> PathBuf {
    workspace.join(".stella").join("private").join("store.db")
}

/// Every line of a log the fixture appended to, or an empty list when the
/// plugin never ran.
fn log_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

/// Run one step of §9 end to end and read back what it did.
async fn observe(label: &'static str, server: &MockServer, active: bool, seats: Seats) -> Observed {
    let workspace = tempfile::tempdir().expect("workspace");
    let home = tempfile::tempdir().expect("stella home");
    let data = tempfile::tempdir().expect("data dir");
    let plugin_dir = install_plugin(workspace.path());
    // One source file, so the session code-graph build has something real to
    // index — `run_exits_cli.rs`'s reason for the same line.
    std::fs::write(workspace.path().join("lib.rs"), "pub fn hello() {}\n").expect("source file");
    std::fs::write(
        workspace.path().join(".stella").join("settings.json"),
        settings_json(&server.uri(), active, &seats),
    )
    .expect("settings.json");

    let child = Command::new(env!("CARGO_BIN_EXE_stella"))
        .without_embedder_backend()
        .args([
            "--model",
            &format!("zai/{SESSION_MODEL}"),
            "--api-key",
            "sk-acceptance-fixture",
            "--base-url",
            &server.uri(),
            "--spend-limit",
            "5.0",
            "run",
            "say ok and stop",
        ])
        .current_dir(workspace.path())
        // `STELLA_HOME` moves the whole user tier, so no credentials file on
        // the developer's machine can reach this run — the hermeticity
        // `goal_wrapped_dispatch_cli.rs` learned the hard way.
        .env("STELLA_HOME", home.path())
        .env("STELLA_DATA_DIR", data.path())
        .env("NO_COLOR", "1")
        .env("STELLA_NO_ENV_FILE", "1")
        // The project tier holds both the plugin and the settings that switch
        // it on, and both sit behind the code-execution boundary.
        .env("STELLA_TRUST_PROJECT", "1")
        .env("STELLA_CATALOG_AUTO_REFRESH", "0")
        .env(
            "STELLA_MANAGED_SETTINGS",
            home.path().join("no-managed.json"),
        )
        // A configured family other than the mocked one would give the run a
        // second real endpoint to reach for.
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
        .expect("spawn stella run");

    // On a blocking thread so the mock server keeps serving while the child
    // runs — the runtime this test is on is the one answering its calls.
    let output = tokio::task::spawn_blocking(move || child.wait_with_output())
        .await
        .expect("join")
        .expect("wait on stella");
    assert!(
        output.status.success(),
        "`{label}`: stella run did not exit 0 — stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stages: Vec<String> = log_lines(&plugin_dir.join("stages.log"))
        .iter()
        .map(|line| {
            let request: serde_json::Value =
                serde_json::from_str(line).expect("a before_turn request is a wire message");
            request["stage"]
                .as_str()
                .expect("every before_turn request names its stage")
                .to_owned()
        })
        .collect();
    let child_answers = log_lines(&plugin_dir.join("child.log"));

    let conn = rusqlite::Connection::open(store_path(workspace.path())).expect("open store.db");
    let models: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT DISTINCT model FROM telemetry ORDER BY model")
            .expect("prepare");
        let rows = stmt.query_map([], |row| row.get(0)).expect("query");
        rows.collect::<Result<_, _>>().expect("model rows")
    };
    let census: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare("SELECT DISTINCT call_role, model FROM telemetry ORDER BY call_role, model")
            .expect("prepare");
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query");
        rows.collect::<Result<_, _>>().expect("census rows")
    };
    let wrapper_id: Option<String> = conn
        .query_row(
            "SELECT pipeline_variant FROM executions WHERE kind = 'run' ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("one run execution row");

    let observed = Observed {
        label,
        stages,
        models,
        census,
        wrapper_id,
        child_answers,
    };
    observed.report();
    observed
}

/// **`doc:roleless-core` §9's acceptance script.**
///
/// It fails in the four ways §9 names. The stage list did not change when the
/// plugin was switched on. The model count did change then. The model count
/// did not change when a seat was assigned. The stage list changed then.
#[tokio::test]
async fn a_plugin_adds_a_stage_without_adding_a_model_and_a_seat_adds_a_model_without_a_stage() {
    // The manifest is a string literal. These four checks keep the constants
    // the assertions read from naming the plugin that actually runs.
    assert!(
        PLUGIN_TOML.contains(&format!("name = \"{PLUGIN}\"")),
        "{PLUGIN_TOML}"
    );
    assert!(
        PLUGIN_TOML.contains(&format!("id = \"{VARIANT}\"")),
        "{PLUGIN_TOML}"
    );
    assert!(
        PLUGIN_TOML.contains(&format!("name = \"{STAGE}\"")),
        "{PLUGIN_TOML}"
    );
    assert!(
        PLUGIN_TOML.contains(&format!("[roles.{ROLE}]")),
        "{PLUGIN_TOML}"
    );

    let server = mock_provider().await;

    // Step 1 and 2 of §9: installed, and not yet switched on.
    let control = observe("installed, switched off", &server, false, Seats::Unassigned).await;
    // Step 3: switched on, with no other configuration.
    let active = observe("switched on, no seat", &server, true, Seats::Unassigned).await;
    // Step 4: one model assigned to the seat the plugin declared.
    let assigned = observe("switched on, seat assigned", &server, true, Seats::Assigned).await;

    // ---- The premises the two comparisons rest on. ----
    assert!(
        control.stages.is_empty(),
        "a plugin that is installed and not switched on must not join the turn: {control:?}"
    );
    assert_eq!(
        control.wrapper_id, None,
        "the control turn is wrapped by nothing: {control:?}"
    );
    for step in [&active, &assigned] {
        assert_eq!(
            step.child_answers.len(),
            1,
            "the plugin asked the host for exactly one child turn: {step:?}"
        );
        assert!(
            step.child_answers[0].contains("\"ok\""),
            "the host ran the child turn rather than refusing it: {step:?}"
        );
    }

    // ---- Comparison one: the composition changed, the model count did not. ----
    assert_eq!(
        active.stages,
        vec![STAGE.to_string()],
        "switching the plugin on must add its stage to the turn: {active:?}"
    );
    assert_eq!(
        active.wrapper_id.as_deref(),
        Some(VARIANT),
        "the turn records the wrapper that composed it: {active:?}"
    );
    assert_eq!(
        active.models, control.models,
        "adding a stage must not add a model. That is §2's sentence, and the \
         point of a core that knows one role: {active:?} against {control:?}"
    );
    assert_eq!(
        active.models,
        vec![SESSION_MODEL.to_string()],
        "the plugin's own turn runs on the session's model until someone says \
         otherwise: {active:?}"
    );
    assert!(
        active
            .census
            .contains(&("plugin".to_string(), SESSION_MODEL.to_string())),
        "the plugin's call is booked at its own seat, on the session's model: {active:?}"
    );

    // ---- Comparison two: the model count changed, the composition did not. ----
    assert_eq!(
        assigned.stages, active.stages,
        "assigning a model must not change the turn's stages: {assigned:?} against {active:?}"
    );
    assert_eq!(
        assigned.wrapper_id, active.wrapper_id,
        "nor the wrapper that composed them: {assigned:?} against {active:?}"
    );
    assert_eq!(
        assigned.models,
        vec![SEAT_MODEL.to_string(), SESSION_MODEL.to_string()],
        "the assigned seat runs on the model the user named, beside the \
         session's own: {assigned:?}"
    );
    assert!(
        assigned
            .census
            .contains(&("plugin".to_string(), SEAT_MODEL.to_string())),
        "and the second model is the plugin's seat, not a second worker: {assigned:?}"
    );
    assert!(
        assigned
            .census
            .contains(&("worker".to_string(), SESSION_MODEL.to_string())),
        "while the session's own turn is unmoved: {assigned:?}"
    );
}
