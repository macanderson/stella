//! **Witness (#3832, finding 1).** `stella goal --pipeline <variant>` used to
//! accept an arbiter-grade wrapper plugin exactly like any other and only
//! discover the shape was wrong after `WrapperDispatch`'s own hold loop
//! (`1 + DEFAULT_HOST_MAX_HOLDS` billed worker turns) ran *inside* one
//! already-judged goal round — `run_goal_wrapped_turn` then saw
//! `report.rounds != 1`, discarded everything, and errored out the whole
//! goal run after burning the cost of every one of those turns. Two
//! round-holders judging the same round is exactly the doubled-supervisor
//! shape the wrapper design forbids.
//!
//! This file fails on the pre-fix tree — no pre-flight arbiter check exists,
//! so `resolve`/`bind` succeed and the run proceeds to dispatch a real
//! (mocked-nowhere, so hanging or erroring for an unrelated reason) provider
//! call — and passes on this one: the arbiter grade is refused before the
//! provider is ever built, on the same pre-flight rung
//! `reject_verification_flags_without_pipeline` uses — before any paid model
//! call, though not before config load and catalog bootstrap, which still
//! run ahead of it. No mock server is stood up and no real API key is used —
//! deliberately, matching `verification_flags_require_pipeline_cli.rs`'s
//! discipline: if this refusal ever stopped firing before dispatch, the
//! process would hang or fail for a completely different (network) reason,
//! not print this exact text.
//!
//! `crates/stella-cli/src/wrapper_plugin.rs`'s
//! `reject_arbiter_wrapper_on_goal` unit tests already cover the decision
//! itself exhaustively; this file's only job is proving `stella goal`'s door
//! actually calls it before dispatch. The companion witness that a
//! steering-grade wrapper is unaffected and still drives every round is
//! `goal_wrapped_dispatch_cli.rs`'s
//! `a_goal_run_dispatches_each_round_through_the_bound_wrapper`.

use std::path::Path;
use std::process::{Command, Output, Stdio};

/// An arbiter-grade wrapper, shaped like `plugins/stella-goal`'s real
/// manifest (`[loop] participation = "arbiter"`, the `Stop` hook it
/// requires, one requirement, and a `flip = "not-applicable"` oracle
/// deciding it) — the exact grade `reject_arbiter_wrapper_on_goal` exists to
/// catch on this door. `[runtime]` names a script that is never spawned: the
/// refusal fires at resolve/bind time, before `ResolvedWrapper::serving`
/// would ever touch the process.
const ARBITER_PLUGIN_TOML: &str = r#"
name = "goal-arbiter-fixture"
[loop]
participation = "arbiter"
hooks = ["Stop"]
points = ["after_turn"]
max_holds = 3
[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh"]
timeout_secs = 30
env = ["PATH"]
[wrapper]
id = "goal-arbiter-fixture-v1"
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

fn install_arbiter_fixture(workspace: &Path) {
    let dir = workspace
        .join(".stella")
        .join("plugins")
        .join("goal-arbiter-fixture");
    std::fs::create_dir_all(&dir).expect("plugin dir");
    std::fs::write(dir.join("plugin.toml"), ARBITER_PLUGIN_TOML).expect("plugin.toml");
    // Never spawned by this test (the refusal fires before `serving()`), but
    // written anyway so a future regression that DID reach dispatch would
    // fail on the fixture doing something observable rather than a bare
    // "file not found" that could be misread as this witness's own passing
    // signal.
    std::fs::write(
        dir.join("main.sh"),
        "#!/bin/sh\necho 'must never run' >&2\nexit 1\n",
    )
    .expect("main.sh");
}

fn run_stella(workspace: &Path, data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stella"))
        .args([
            "--model",
            "openrouter/z-ai/glm-5.1",
            "--api-key",
            "sk-not-a-real-key",
        ])
        .args(args)
        .current_dir(workspace)
        .env("STELLA_DATA_DIR", data)
        // Never read the developer's project env, and never inherit a real
        // key: this test must not be able to reach a provider.
        .env("STELLA_NO_ENV_FILE", "1")
        // The project plugin tier must be trusted for the fixture to load at
        // all — the same flag `goal_wrapped_dispatch_cli.rs` sets — so a
        // failure to load never masquerades as the arbiter refusal.
        .env("STELLA_TRUST_PROJECT", "1")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(Stdio::null())
        .output()
        .expect("run stella")
}

fn fresh_dirs() -> (tempfile::TempDir, tempfile::TempDir) {
    (
        tempfile::tempdir().expect("workspace"),
        tempfile::tempdir().expect("data dir"),
    )
}

#[test]
fn an_arbiter_grade_wrapper_is_refused_on_goal_before_any_paid_call() {
    let (workspace, data) = fresh_dirs();
    install_arbiter_fixture(workspace.path());

    let out = run_stella(
        workspace.path(),
        data.path(),
        &[
            "goal",
            "do the thing, then stop",
            "--pipeline",
            "goal-arbiter-fixture-v1",
        ],
    );

    assert!(
        !out.status.success(),
        "expected a non-zero exit — stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("goal-arbiter-fixture-v1"),
        "the refusal must name the refused variant: {stderr}"
    );
    assert!(stderr.contains("#3832"), "{stderr}");
    assert!(
        stderr.contains("stella run --pipeline goal-arbiter-fixture-v1"),
        "the refusal must name the remedy — the wrapper's designed home: {stderr}"
    );
    assert!(
        !stderr.contains("must never run"),
        "the fixture script's own stderr must never appear — the refusal must fire before the \
         plugin process is ever spawned: {stderr}"
    );
    // No `store.db` execution row at all: the refusal fires before
    // `begin_execution` is ever called, so nothing was billed and nothing
    // was recorded — the strongest available proxy for "zero provider calls"
    // this end-to-end test can observe without a mock server.
    let store_db = workspace
        .path()
        .join(".stella")
        .join("private")
        .join("store.db");
    assert!(
        !store_db.exists(),
        "a pre-flight refusal must open no execution row at all: {store_db:?} exists"
    );
}

/// The companion half, scoped to this file's own fixture rather than
/// `goal_wrapped_dispatch_cli.rs`'s steering one: swapping `participation =
/// "arbiter"` for `"steering"` on the exact same manifest shape must no
/// longer be refused by THIS gate — it fails later for its own reasons (no
/// reachable provider), which is what proves the refusal is scoped to the
/// grade, not to having a `[wrapper]` variant at all.
#[test]
fn the_same_fixture_at_steering_grade_is_not_refused_by_the_arbiter_gate() {
    let (workspace, data) = fresh_dirs();
    let dir = workspace
        .path()
        .join(".stella")
        .join("plugins")
        .join("goal-steering-fixture");
    std::fs::create_dir_all(&dir).expect("plugin dir");
    std::fs::write(
        dir.join("plugin.toml"),
        r#"
name = "goal-steering-fixture"
[loop]
participation = "steering"
points = ["before_turn"]
[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh"]
timeout_secs = 30
env = ["PATH"]
[wrapper]
id = "goal-steering-fixture-v1"
[[wrapper.stages]]
name = "execute"
"#,
    )
    .expect("plugin.toml");
    std::fs::write(
        dir.join("main.sh"),
        "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"point\":\"before_turn\",\"body\":{\"protocol_version\":1}}'\n",
    )
    .expect("main.sh");

    let out = run_stella(
        workspace.path(),
        data.path(),
        &[
            "goal",
            "do the thing, then stop",
            "--pipeline",
            "goal-steering-fixture-v1",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("#3832"),
        "a steering-grade wrapper must never hit the arbiter refusal: {stderr}"
    );
    assert!(
        !stderr.contains("is arbiter-grade and cannot run on `stella goal`"),
        "{stderr}"
    );
}
