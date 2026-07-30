//! `stella run` must exit on its own (#960).
//!
//! It did not. The turn finished, the terminal event went out, and the process
//! then sat there until something killed it — because the renderer task ends
//! when its channel closes, and the registry was still holding a sender clone
//! that nobody dropped. Every non-interactive use of the primary surface waits
//! on process exit, so this read as a hang to every caller: CI steps, wrapper
//! scripts, and the Terminal-Bench harness, which recorded even reward-1.0
//! trials as `AgentTimeoutError` and burned each task's whole wall-clock
//! budget.
//!
//! It went unnoticed for as long as it did precisely because nothing in the
//! suite ever *waited* on the process. That is what these tests do, and the
//! only thing they assert.
//!
//! Deliberately hermetic — no API key, no network, no model. `--base-url`
//! points at a closed loopback port, so the provider call fails immediately
//! and the run reaches its shutdown path in seconds. The shutdown path is the
//! subject; how the turn ended is not. (A failed turn is if anything the
//! *harder* case: it takes the error arm, which is the one the pipeline's own
//! terminal-event handling does not run.)

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Generous on purpose. The property under test is the difference between
/// "finishes" and "never finishes", not a latency budget — a bound tight
/// enough to be a performance assertion would be a flake on loaded CI.
const EXIT_BUDGET: Duration = Duration::from_secs(120);

/// How often to check. Cheap: `try_wait` does not block.
const POLL: Duration = Duration::from_millis(100);

/// Run `stella run` to completion, or give up at [`EXIT_BUDGET`]. Returns how
/// long it took; panics (after killing the child) if it never exited.
fn time_to_exit(workspace: &Path, data: &Path, format: &str) -> Duration {
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_stella"))
        .args([
            "--model",
            "openrouter/z-ai/glm-5.1",
            "--api-key",
            "sk-not-a-real-key",
            // Closed port on loopback: connection refused, at once.
            "--base-url",
            "http://127.0.0.1:1",
            "--budget",
            "0.05",
            "--output-format",
            format,
            "run",
            "say hi and stop",
        ])
        .current_dir(workspace)
        .env("STELLA_DATA_DIR", data)
        // Never read the developer's project env, and never inherit a real
        // key: this test must not be able to reach a provider.
        .env("STELLA_NO_ENV_FILE", "1")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        // Immediate EOF. The hang reproduced with stdin closed and with it
        // held open; closed is the one a CI step actually has.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn stella");

    loop {
        match child.try_wait().expect("wait on stella") {
            Some(_) => return started.elapsed(),
            None if started.elapsed() >= EXIT_BUDGET => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "`stella run --output-format {format}` never exited \
                     (killed at {EXIT_BUDGET:?}) — see #960"
                );
            }
            None => std::thread::sleep(POLL),
        }
    }
}

fn assert_exits(format: &str) {
    let workspace = tempfile::tempdir().expect("workspace");
    let data = tempfile::tempdir().expect("data dir");
    // One source file, so the session code-graph build has something real to
    // index and the watcher it then arms is genuinely running at shutdown.
    std::fs::write(workspace.path().join("lib.rs"), "pub fn hello() {}\n").expect("source file");

    let elapsed = time_to_exit(workspace.path(), data.path(), format);
    assert!(
        elapsed < EXIT_BUDGET,
        "{format} took {elapsed:?}, which is the whole budget"
    );
}

/// The machine-readable surface a benchmark harness or CI step consumes, and
/// the one #960 was found on.
#[test]
fn run_exits_on_its_own_with_stream_json() {
    assert_exits("stream-json");
}

#[test]
fn run_exits_on_its_own_with_json() {
    assert_exits("json");
}

#[test]
fn run_exits_on_its_own_with_text() {
    assert_exits("text");
}
