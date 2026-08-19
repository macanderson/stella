//! The two resources a plugin process can spend that are not its own: the
//! host's memory, and the machine after the turn ended.
//!
//! Both are witnesses for #3380's transport audit, and both failed before the
//! transport took the hook plane's spawn policy whole:
//!
//! - `child.wait_with_output()` read both pipes to EOF into a `Vec<u8>`, so a
//!   plugin's `after_turn` — the documented place to run a test suite or a
//!   benchmark — could hand the host every byte it felt like producing before
//!   one of them was parsed. The `OUTPUT_EXCERPT_CHARS` bound applied only
//!   when *rendering an error*, which is after the whole buffer already
//!   existed.
//! - `kill_on_drop(true)` SIGKILLs the direct child and nothing else, so a
//!   plugin that backgrounded work left a tree running past the turn it was
//!   gathering evidence for.
//!
//! Each test asserts the *behaviour*, not the error's spelling: the first
//! observes that the read stopped while the plugin was still willing to write
//! and that the writer is gone; the second observes that a specific
//! grandchild pid is no longer running.
//!
//! `cfg(unix)` for `tests/wrapper_socket.rs`'s reason and tracked in the same
//! issue: the plugins here are `/bin/sh` scripts, and a portable in-tree
//! plugin binary is #3497. The process-group half of what is asserted is
//! unix-shaped anyway — the non-unix build has `kill_on_drop` and nothing
//! else, which is a declared gap rather than a covered one.

#![cfg(unix)]

use std::time::{Duration, Instant};

use stella_plugin::{BeforeTurnRequest, HostStage, PROTOCOL_VERSION, StageName};
use stella_runtime::wrapper::{SubprocessWrapper, TurnWrapper, WrapperError};
use stella_tools::exec::MAX_CAPTURE_BYTES;

/// A `sh` plugin with the given budget. No environment at all: these tests are
/// about the transport's own limits, not what it passes through.
fn plugin(script: &str, timeout: Duration) -> SubprocessWrapper {
    SubprocessWrapper::declare(
        vec!["/bin/sh".into(), "-c".into(), script.into()],
        Vec::new(),
        timeout,
    )
    .expect("the transport is declared with a program and a budget")
    .wrapper
}

fn before() -> BeforeTurnRequest {
    BeforeTurnRequest {
        protocol_version: PROTOCOL_VERSION,
        wrapper: "limits-v1".into(),
        stage: StageName::Host(HostStage::Triage),
        round: 0,
        goal: "prove the transport bounds what a plugin can spend".into(),
        candidate: None,
        published: Vec::new(),
    }
}

/// Whether `pid` names a process that is still running.
///
/// `kill -0` cannot answer this on its own: a SIGKILLed orphan stays in the
/// process table until someone reaps it, and under a pid 1 that does not reap
/// (every minimal container) it stays there for good. `ps` reports the state,
/// so a `Z` is read as gone — which is the honest reading, since a zombie
/// holds no memory, no file descriptors, and cannot run.
fn still_running(pid: i32) -> bool {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-o", "state=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    let state = String::from_utf8_lossy(&output.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

/// Poll [`still_running`] until it answers `false`, up to `budget`.
///
/// A SIGKILL is delivered asynchronously and the reap that follows is someone
/// else's scheduling decision, so an immediate single check would be a race
/// against the kernel rather than a check on the transport. Answering `true`
/// after the whole budget is a real leak: nothing is coming to clean it up.
fn outlived(pid: i32, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if !still_running(pid) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    still_running(pid)
}

/// Read a pid the plugin recorded for itself.
fn recorded_pid(path: &std::path::Path) -> i32 {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("the plugin recorded a pid at {}: {e}", path.display()));
    text.trim()
        .parse()
        .unwrap_or_else(|e| panic!("`{}` is a pid: {e}", text.trim()))
}

/// **Witness 1.** A plugin that writes past the ceiling is refused, and the
/// refusal is a *stopped read*: the call comes back while the plugin is still
/// producing output, and the plugin is killed rather than left to fill a pipe
/// nobody is draining.
///
/// The script writes one MiB past the ceiling as fast as the pipe takes it and
/// then trickles forever. Before the cap, that meant the host held all of it
/// and then sat on the trickle until the budget expired — the assertions below
/// fail as a `Timeout` ten seconds later. The trickle is what makes this test
/// safe to run against the old binary: an unbounded writer would have proven
/// the same thing by exhausting the machine's memory instead.
///
/// **What the overflow is made of matters since the host-call channel landed**
/// (#3540). The transport reads messages incrementally now, so it can tell an
/// *incomplete* document from a malformed one: bytes that are not even a JSON
/// prefix are refused by the parser on the first read
/// ([`WrapperError::Decode`], the case below), and it is the still-incomplete
/// document that accumulates. So the flood here is one enormous string inside a
/// well-formed answer that never closes — which is exactly the shape a host
/// must bound, since it is the only one it cannot refuse on sight.
#[tokio::test]
async fn a_plugin_that_writes_past_the_ceiling_is_refused_and_the_read_stops() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pid_file = dir.path().join("writer.pid");
    let budget = Duration::from_secs(10);
    let over_the_line = MAX_CAPTURE_BYTES + 1024 * 1024;
    let script = format!(
        "echo $$ > {pid}\n\
         printf '%s' '{{\"point\":\"before_turn\",\"body\":{{\"context\":[{{\"label\":\"x\",\"text\":\"'\n\
         yes stella | tr -d '\\n' | head -c {over_the_line}\n\
         while : ; do printf tick ; sleep 0.05 ; done\n",
        pid = pid_file.display(),
    );

    let started = Instant::now();
    let err = plugin(&script, budget)
        .before_turn(before())
        .await
        .expect_err("a plugin past the ceiling is refused, not buffered");

    let WrapperError::OutputCap { stream, cap, .. } = &err else {
        panic!("the refusal is named, not collapsed into another failure: {err}");
    };
    assert_eq!(*stream, "stdout", "stdout is the stream that overflowed");
    assert_eq!(
        *cap, MAX_CAPTURE_BYTES,
        "the transport shares the subprocess plane's one capture ceiling"
    );

    // The read stopped: the plugin still had output to give and the host
    // answered anyway, well inside a budget it never used.
    assert!(
        started.elapsed() < budget / 2,
        "refused after {:?} of a {budget:?} budget — the read ran on to the timeout",
        started.elapsed(),
    );
    // And it cannot resume: the writer is gone, so no further byte can arrive
    // and nothing is blocked on a pipe the host stopped draining.
    let writer = recorded_pid(&pid_file);
    assert!(
        !outlived(writer, Duration::from_secs(5)),
        "the refused plugin (pid {writer}) is still writing after the call returned",
    );
}

/// **Witness 2.** A plugin that backgrounds a child and then runs out of
/// budget leaves nothing behind.
///
/// `kill_on_drop` reaches the `sh` and stops there; the `sleep` it backgrounded
/// is a grandchild, and before the transport `setsid`'d its children into their
/// own process group there was nothing that could reach it. It outlived the
/// turn by its full five minutes — on a machine whose user had been told the
/// turn was over.
#[tokio::test]
async fn a_timed_out_plugin_leaves_no_surviving_grandchild() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pid_file = dir.path().join("grandchild.pid");
    let script = format!(
        "sleep 300 &\n\
         echo $! > {pid}\n\
         sleep 300\n",
        pid = pid_file.display(),
    );

    let err = plugin(&script, Duration::from_millis(500))
        .before_turn(before())
        .await
        .expect_err("the plugin never answers inside its budget");
    assert!(
        matches!(err, WrapperError::Timeout { .. }),
        "the budget is what ended it: {err}"
    );

    let grandchild = recorded_pid(&pid_file);
    assert!(
        !outlived(grandchild, Duration::from_secs(5)),
        "the backgrounded grandchild (pid {grandchild}) outlived the turn it was gathering \
         evidence for",
    );
}

/// **Witness 3.** A plugin that keeps writing to stdout after it has already
/// answered the point does not wedge the exchange.
///
/// `converse` stops reading stdout the instant it finds the point response —
/// there is nothing left in the wire contract to parse — so anything the
/// child writes after that sits in the OS pipe (about 64 KiB on Linux)
/// nobody is draining. Before `settle` also drained stdout, a child that
/// filled that pipe blocked in `write(2)` and could never reach `exit(2)`, so
/// `child.wait()` hung until the outer budget below fired and the process
/// group was killed — turning a plugin that had already answered correctly
/// into a `Timeout`. The script writes comfortably past one pipe buffer
/// (three times over) after its response, which is enough to reproduce the
/// deadlock on the old code without needing anywhere near the capture
/// ceiling.
#[tokio::test]
async fn a_plugin_that_talks_after_answering_does_not_wedge_the_exchange() {
    let budget = Duration::from_secs(8);
    let trailing_past_one_pipe_buffer = 3 * 64 * 1024;
    let script = format!(
        "printf '%s\\n' '{{\"point\":\"before_turn\",\"body\":{{\"protocol_version\":1,\"context\":[]}}}}'\n\
         yes stella | tr -d '\\n' | head -c {trailing_past_one_pipe_buffer}\n",
    );

    let started = Instant::now();
    let response = plugin(&script, budget).before_turn(before()).await.expect(
        "a plugin that answered correctly and then kept talking is not lost to a pipe \
             deadlock",
    );
    let elapsed = started.elapsed();

    assert!(
        response.context.is_empty(),
        "the point response itself is unaffected by what came after it"
    );
    // The old code did not fail differently here — it hung for the whole
    // budget and then failed as `Timeout`. This bound is what tells the two
    // apart: an exchange that drains the trailing output returns in well
    // under a second, not most of an 8-second budget.
    assert!(
        elapsed < budget / 2,
        "answered in {elapsed:?} of a {budget:?} budget — settle() waited on the child's exit \
         without draining its trailing stdout, exactly the deadlock this test exists to catch",
    );
}
