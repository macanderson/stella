// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Portable witnesses for `crate::exec::GroupKillGuard` at the three spawn
//! planes that are not the plugin transport: `bash`, a custom script tool,
//! and a hook (#4698).
//!
//! #4456 gave the guard a Windows Job Object arm and #4675 proved it for the
//! plugin transport (`stella-runtime`'s `wrapper_transport_limits.rs`, which
//! this file's shape is copied from). The equivalents that lived beside these
//! three call sites — `bash.rs`'s and `custom/tests/execution.rs`'s
//! `#[cfg(unix)]` tests — read a pid with `libc::kill`, which has no Windows
//! answer, so nothing here ever exercised the guard's Windows arm. `hook_runner.rs`
//! had no such test at all. This file replaces the two and adds the third,
//! all three heartbeating to a file instead of polling a pid, exactly as the
//! plugin witness does: `ps -o state=` on a recorded pid cannot be ported (a
//! killed orphan lingers as a zombie until reaped, which Windows has no
//! state column for), but a file that stops growing reports the same fact —
//! that the process is still *doing* something — on both platforms.
//!
//! The custom-tool witness's heartbeat writer is `group-kill-fixture`, a
//! portable binary (`tests/fixtures/`, the same pattern
//! `wrapper-plugin-fixture` established for the plugin transport): the tool
//! runs `command[0]` with no shell, so it needs none, and it is the one
//! witness here that runs unconditionally on Windows. The `bash` and hook
//! witnesses have no such escape — both call sites hardcode
//! `Command::new("bash")` — and on `windows-latest` that resolves to
//! Windows' own WSL launcher stub, not a real shell, regardless of `PATH`
//! (`CreateProcessW` checks `System32` before `PATH` at all). They are
//! `#[cfg_attr(windows, ignore)]` until #4861 fixes that, rather than forced
//! green against an environment neither call site can see through.
//!
//! An integration test on purpose, not a `#[cfg(test)] mod` beside the
//! production code: `custom/tests.rs` and several other `stella-tools`
//! modules import `std::os::unix::fs::PermissionsExt` unconditionally inside
//! `#[cfg(test)]`, so the crate's `--lib` test binary does not compile on
//! Windows today (`windows-check.yml`'s own comment names this). A file
//! under `tests/` links only the compiled library, sidestepping that
//! altogether, and it is why `windows-check.yml` runs it by name rather than
//! folding it into `--lib`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use stella_core::hooks::{HookAction, HookExecError, HookRunner};
use stella_core::ports::ToolExecutor;
use stella_tools::ToolRegistry;
use stella_tools::bash::Bash;
use stella_tools::ctx::ToolCtx;
use stella_tools::custom::{CustomTool, CustomToolSet, MAX_TIMEOUT_MS};
use stella_tools::hook_runner::HostHookRunner;
use stella_tools::registry::Tool;

/// How long one observation of a heartbeat file lasts. The script below
/// appends every 20ms, so a running writer grows the file by roughly ten
/// bytes here and a killed one by none.
const OBSERVATION: Duration = Duration::from_millis(200);

/// Bytes written to a heartbeat file so far; `0` for one that does not exist
/// yet.
fn beats(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

/// Whether whatever is writing `path` is still writing it.
///
/// An `async fn` sleeping on the Tokio clock rather than blocking the OS
/// thread — the two dropped-future witnesses below poll this while a
/// `tokio::spawn`ed task is still in flight on the same single-threaded test
/// runtime, and a `std::thread::sleep` here would starve that task of the
/// chance to ever call `spawn` at all.
async fn still_beating(path: &Path) -> bool {
    let before = beats(path);
    tokio::time::sleep(OBSERVATION).await;
    beats(path) != before
}

/// Poll [`still_beating`] until it answers `false`, up to `budget`.
///
/// The kill is delivered asynchronously and the writer's last append may
/// already be in flight, so a single immediate check would be a race against
/// the kernel rather than a check on the guard. Answering `true` after the
/// whole budget is a real leak: nothing is coming to stop it.
async fn outlived(path: &Path, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if !still_beating(path).await {
            return false;
        }
    }
    still_beating(path).await
}

/// Wait until `path` has been written at all, so a test cannot pass because
/// the grandchild it watches never started — the premise every witness below
/// asserts before trusting a still file.
async fn started_beating(path: &Path, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if beats(path) > 0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// A shell fragment that backgrounds a heartbeat writer and then hangs, so
/// the group the caller kills has a real grandchild to fail to reach.
/// Single-quoted so a Windows temp path's backslashes reach `printf`
/// literally rather than as shell escapes.
fn backgrounding_command(pulse: &Path) -> String {
    format!(
        "( while true; do printf '.' >> '{}'; sleep 0.02; done ) & sleep 30",
        pulse.display()
    )
}

/// **Witness 1** (ported from `bash.rs`'s `#[cfg(unix)]` test). Dropping the
/// future driving a `bash` call — Esc during a long call — must kill the
/// whole group, not just the shell that fronts it.
///
/// Ignored on Windows, not skipped silently: `Bash::execute` hardcodes
/// `Command::new("bash")`, and on `windows-latest` that resolves to Windows'
/// own WSL launcher stub — with no distribution installed it prints an
/// install prompt and exits nonzero instead of running anything, ahead of
/// Git for Windows' real shell regardless of `PATH` order (`CreateProcessW`
/// checks `System32` before consulting `PATH` at all). #4861 tracks fixing
/// that; until then this witness cannot prove `GroupKillGuard` reaches a
/// backgrounded child here.
#[cfg_attr(
    windows,
    ignore = "blocked on #4861: bash resolves to the WSL launcher stub here, not a real shell"
)]
#[tokio::test]
async fn a_dropped_bash_call_kills_the_process_group() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pulse = dir.path().join("bash.pulse");
    let root = dir.path().to_path_buf();
    let command = backgrounding_command(&pulse);

    let handle = tokio::spawn(async move {
        Bash::new(None)
            .execute(
                &serde_json::json!({"command": command, "timeout_secs": 60}),
                &ToolCtx::bare(root),
            )
            .await
    });
    assert!(
        started_beating(&pulse, Duration::from_secs(5)).await,
        "the backgrounded heartbeat never started, so this test would prove nothing"
    );
    handle.abort();
    let _ = handle.await;

    assert!(
        !outlived(&pulse, Duration::from_secs(5)).await,
        "a dropped bash call left its backgrounded child running"
    );
}

/// **Witness 2** (ported from `custom/tests/execution.rs`'s `#[cfg(unix)]`
/// test). Dropping the future driving a custom tool must kill its whole
/// group the same way — the same guard, a different spawn site.
///
/// The tool's `command` runs `group-kill-fixture` directly, not `bash`:
/// `run_custom` spawns `command[0]` as the program with no shell in between
/// (a `#!/bin/sh` script — the shape the file's other fixtures use — never
/// executes on Windows at all), and a portable binary sidesteps needing a
/// shell to exist on the runner in the first place, unlike Witnesses 1 and 3.
#[tokio::test]
async fn a_dropped_custom_tool_kills_the_process_group() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pulse = dir.path().join("custom.pulse");
    let root = dir.path().to_path_buf();
    let tool = CustomTool {
        name: "bg".into(),
        description: "backgrounds a heartbeat writer".into(),
        command: vec![
            env!("CARGO_BIN_EXE_group-kill-fixture").to_string(),
            "background".to_string(),
            pulse.display().to_string(),
        ],
        timeout_ms: MAX_TIMEOUT_MS,
        input_schema: serde_json::json!({ "type": "object" }),
        env: HashMap::new(),
        source: dir.path().join("bg.toml"),
        foundry: None,
        claimed_read_only: false,
        claimed_risk: None,
        claimed_idempotent: false,
        output_schema: None,
        contributed_by: None,
    };
    let registry: Arc<dyn ToolExecutor> = Arc::new(ToolRegistry::new(root.clone()));
    let set = CustomToolSet::new_owned(registry, vec![tool], root);

    let handle = tokio::spawn(async move { set.execute("bg", &serde_json::json!({})).await });
    assert!(
        started_beating(&pulse, Duration::from_secs(5)).await,
        "the backgrounded heartbeat never started, so this test would prove nothing"
    );
    handle.abort();
    let _ = handle.await;

    assert!(
        !outlived(&pulse, Duration::from_secs(5)).await,
        "a dropped custom tool call left its backgrounded child running"
    );
}

/// **Witness 3** (new — `hook_runner.rs` had no group-kill test at all). A
/// hook that backgrounds work and then runs out of budget leaves nothing
/// behind, the same property `wrapper_transport_limits.rs`'s
/// `a_timed_out_plugin_leaves_no_surviving_grandchild` proves for the plugin
/// transport: `kill_now` on the timeout path reaches the grandchild, not
/// only the hook process that fronted it.
///
/// Ignored on Windows for the same reason as Witness 1, tracked by the same
/// #4861: the hook runner's operator path (an action with no `[plugin]`
/// origin) also hardcodes `Command::new("bash")`.
#[cfg_attr(
    windows,
    ignore = "blocked on #4861: bash resolves to the WSL launcher stub here, not a real shell"
)]
#[tokio::test]
async fn a_timed_out_hook_leaves_no_surviving_grandchild() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pulse = dir.path().join("hook.pulse");
    let mut hung = HookAction::new(backgrounding_command(&pulse));
    hung.timeout_ms = Some(300);

    let err = HostHookRunner
        .run(&hung, "{}", &dir.path().display().to_string())
        .await
        .expect_err("the hook never answers inside its budget");
    assert!(
        matches!(err, HookExecError::TimedOut { .. }),
        "the budget is what ended it: {err}"
    );
    assert!(
        started_beating(&pulse, Duration::from_secs(5)).await,
        "no grandchild ever ran, so this test would prove nothing about killing one"
    );
    assert!(
        !outlived(&pulse, Duration::from_secs(5)).await,
        "the backgrounded grandchild outlived the hook's own timeout"
    );
}
