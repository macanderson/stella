//! `stella run --keep-witness` / `--require-verified` / `--test-command`
//! silently no-op on the raw loop that #3381 made the default (#3696).
//!
//! `run_raw_one_shot` never accepted `keep_witness`/`require_verified` as
//! parameters at all, and only threads `test_command` to a *bound wrapper
//! plugin's* own oracle — so before this fix, `stella run --keep-witness
//! "…"` (the single most common invocation shape after the flip) would start
//! a paid model call and then simply drop the flag. These tests exercise the
//! real binary end to end: the refusal must fire on the process's own stderr
//! and exit code, before any provider is ever contacted — no real network
//! host is named, and no API key is real, so if this reached a provider it
//! would either hang or fail for an unrelated reason, not print this exact
//! refusal text.
//!
//! `crates/stella-cli/src/wrapper_plugin.rs`'s
//! `reject_verification_flags_without_pipeline` unit tests already cover the
//! decision exhaustively; this file's only job is proving `stella run`'s
//! door actually calls it before dispatch.

use std::path::Path;
use std::process::{Command, Output, Stdio};

mod common;
use common::SealsEmbedderBackend;

fn run_stella(workspace: &Path, data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stella"))
        .without_embedder_backend()
        .args([
            "--model",
            "openrouter/z-ai/glm-5.1",
            "--api-key",
            "sk-not-a-real-key",
        ])
        .args(args)
        .current_dir(workspace)
        .env("STELLA_HOME", data)
        .env("STELLA_DATA_DIR", data)
        // Never read the developer's project env, and never inherit a real
        // key: this test must not be able to reach a provider.
        .env("STELLA_NO_ENV_FILE", "1")
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

/// **Witness (#3696).** Fails on the pre-fix tree — no such refusal exists,
/// so this exact invocation reaches `run_one_shot` and attempts to start a
/// provider call rather than refusing outright — and passes on this one:
/// refused before dispatch, naming the remedy.
#[test]
fn keep_witness_alone_is_refused_without_pipeline() {
    let (workspace, data) = fresh_dirs();
    let out = run_stella(
        workspace.path(),
        data.path(),
        &["run", "--keep-witness", "say hi"],
    );
    assert!(!out.status.success(), "expected a non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--keep-witness"), "{stderr}");
    assert!(stderr.contains("stella plugin install"), "{stderr}");
}

#[test]
fn require_verified_alone_is_refused_without_pipeline() {
    let (workspace, data) = fresh_dirs();
    let out = run_stella(
        workspace.path(),
        data.path(),
        &["run", "--require-verified", "say hi"],
    );
    assert!(!out.status.success(), "expected a non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--require-verified"), "{stderr}");
    assert!(stderr.contains("stella plugin install"), "{stderr}");
}

#[test]
fn test_command_alone_is_refused_without_pipeline() {
    let (workspace, data) = fresh_dirs();
    let out = run_stella(
        workspace.path(),
        data.path(),
        &["run", "--test-command", "pytest", "say hi"],
    );
    assert!(!out.status.success(), "expected a non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--test-command"), "{stderr}");
    assert!(stderr.contains("stella plugin install"), "{stderr}");
}

/// The refusal is scoped to the three verification flags, not to
/// `--pipeline` being absent: with none of them passed, `stella run` never
/// hits the refusal's error text at all (it goes on to fail differently,
/// against the closed loopback port — the pre-existing #960 shutdown-path
/// shape `run_exits_cli.rs` already covers).
#[test]
fn a_bare_raw_run_never_prints_the_verification_refusal() {
    let (workspace, data) = fresh_dirs();
    let out = Command::new(env!("CARGO_BIN_EXE_stella"))
        .without_embedder_backend()
        .args([
            "--model",
            "openrouter/z-ai/glm-5.1",
            "--api-key",
            "sk-not-a-real-key",
            // Closed port on loopback: connection refused, at once — never a
            // real network call.
            "--base-url",
            "http://127.0.0.1:1",
            "--spend-limit",
            "0.05",
            "run",
            "say hi",
        ])
        .current_dir(workspace.path())
        .env("STELLA_HOME", data.path())
        .env("STELLA_DATA_DIR", data.path())
        .env("STELLA_NO_ENV_FILE", "1")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(Stdio::null())
        .output()
        .expect("run stella");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("staged pipeline's verification machinery"),
        "a bare run must never hit the verification-flag refusal: {stderr}"
    );
}

/// **Witness (#3696, arena door).** `stella arena` inherited the same gap:
/// it resolved `PipelineChoice::resolve(no_pipeline, None)` — always `Raw`
/// after #3381 — and handed `--test-command` to a driver with no verify
/// ladder to arm, so a benchmark runner asking for an oracle got a number
/// measured without one and no way to tell. Fails on the pre-fix tree (the
/// flag was accepted and inert; `--pipeline` did not even parse), passes on
/// this one.
#[test]
fn arena_test_command_is_refused_without_pipeline() {
    let (workspace, data) = fresh_dirs();
    let out = run_stella(
        workspace.path(),
        data.path(),
        &[
            "arena",
            "--task-dir",
            ".",
            "--journal",
            "journal.jsonl",
            "--state-dir",
            "state",
            "--test-command",
            "pytest",
        ],
    );
    assert!(!out.status.success(), "expected a non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--test-command"), "{stderr}");
    assert!(stderr.contains("stella plugin install"), "{stderr}");
}

/// **Witness (#3865).** `--pipeline classic` used to give `--test-command` a ladder to arm,
/// so this exact invocation used to run past the verification-flags gate
/// (see the previous commit's `arena_test_command_is_accepted_with_pipeline_classic`,
/// which this replaces). The built-in staged pipeline is gone now, so
/// `--pipeline classic` is refused at `PipelineChoice::resolve` itself,
/// before `arena`'s own verification-flags gate is ever reached — the same
/// refusal every other door gives it. Fails on the pre-slice-1 tree (which
/// still runs past this point); passes on this one.
#[test]
fn arena_pipeline_classic_is_refused_outright() {
    let (workspace, data) = fresh_dirs();
    let out = run_stella(
        workspace.path(),
        data.path(),
        &[
            "arena",
            "--task-dir",
            ".",
            "--journal",
            "journal.jsonl",
            "--state-dir",
            "state",
            "--pipeline",
            "classic",
            "--test-command",
            "pytest",
        ],
    );
    assert!(!out.status.success(), "expected a non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--pipeline classic no longer runs anything"),
        "{stderr}"
    );
    assert!(stderr.contains("stella plugin install"), "{stderr}");
}
