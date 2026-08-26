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

mod common;
use common::SealsEmbedderBackend;

/// Generous on purpose. The property under test is the difference between
/// "finishes" and "never finishes", not a latency budget — a bound tight
/// enough to be a performance assertion would be a flake on loaded CI.
const EXIT_BUDGET: Duration = Duration::from_secs(120);

/// How often to check. Cheap: `try_wait` does not block.
const POLL: Duration = Duration::from_millis(100);

/// clap's exit code for a usage error. The one status that must never satisfy
/// these tests: a process that died in the argument parser reached no
/// shutdown path at all, and "it exited" is then a fact about clap.
const USAGE_ERROR: i32 = 2;

/// Run `stella run` to completion, or give up at [`EXIT_BUDGET`]. Returns how
/// long it took; panics (after killing the child) if it never exited, or if
/// it exited without ever starting.
///
/// The exit *code* is checked as well as the fact of exiting, because these
/// tests were vacuous for as long as the argument order was wrong.
/// `--output-format` used to be a root-position flag and became a `run` flag
/// (`crates/stella-cli/src/tests.rs`'s `run_and_fleet_declare_output_format`,
/// and its sibling asserting the root spelling is now a parse error); this
/// file kept the old order and was never updated. Every run therefore exited
/// in milliseconds with `error: unexpected argument '--output-format' found`,
/// which satisfies "it exited" perfectly and says nothing whatever about
/// #960's shutdown path.
fn time_to_exit(workspace: &Path, data: &Path, format: &str) -> Duration {
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_stella"))
        .without_embedder_backend()
        .args([
            "--model",
            "openrouter/z-ai/glm-5.1",
            "--api-key",
            "sk-not-a-real-key",
            // Closed port on loopback: connection refused, at once.
            "--base-url",
            "http://127.0.0.1:1",
            "--spend-limit",
            "0.05",
            "run",
            "--output-format",
            format,
            "say hi and stop",
        ])
        .current_dir(workspace)
        .env("STELLA_HOME", data)
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
        // Piped rather than discarded so a usage error can be quoted back
        // when the assertion below fires.
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stella");

    loop {
        match child.try_wait().expect("wait on stella") {
            Some(status) => {
                assert_ne!(
                    status.code(),
                    Some(USAGE_ERROR),
                    "`stella run --output-format {format}` died in the argument \
                     parser, so it never reached the shutdown path this test is \
                     about — stderr: {}",
                    read_stderr(&mut child),
                );
                return started.elapsed();
            }
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

/// Drain the child's piped stderr, for the failure message above.
fn read_stderr(child: &mut std::process::Child) -> String {
    use std::io::Read;
    let mut buffer = String::new();
    if let Some(stderr) = child.stderr.as_mut() {
        let _ = stderr.read_to_string(&mut buffer);
    }
    buffer
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
