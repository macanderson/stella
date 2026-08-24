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
//! Both tests were `#![cfg(unix)]` until #3497, because both plugins were
//! `/bin/sh` scripts and the liveness check was `ps`. They now drive the
//! portable `wrapper-plugin-fixture`, which matters most for the second one:
//! the tree-reaching kill is a process group on unix and a Job Object on
//! Windows (#3550), and the Windows half had no test that could run at all.

use std::path::Path;
use std::time::{Duration, Instant};

use stella_plugin::{BeforeTurnRequest, HostStage, PROTOCOL_VERSION, StageName};
use stella_runtime::wrapper::{SubprocessWrapper, TurnWrapper, WrapperError};
use stella_tools::exec::MAX_CAPTURE_BYTES;

/// The portable plugin (#3497), located the way `stella-mcp`'s fixture server
/// is. No environment at all: these tests are about the transport's own limits,
/// not what it passes through.
fn plugin(mode: &[&str], timeout: Duration) -> SubprocessWrapper {
    let mut argv = vec![env!("CARGO_BIN_EXE_wrapper-plugin-fixture").to_string()];
    argv.extend(mode.iter().map(|part| (*part).to_string()));
    SubprocessWrapper::declare(argv, Vec::new(), timeout)
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

/// How long one observation of a heartbeat file lasts. The fixture appends
/// every 25ms, so a running writer grows the file by roughly eight bytes here
/// and a dead one by none.
const OBSERVATION: Duration = Duration::from_millis(200);

/// Bytes written to a heartbeat file so far; `0` for one that does not exist
/// yet.
fn beats(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

/// Whether whatever is writing `path` is still writing it.
///
/// This replaces a `ps -o state=` on a recorded pid, which could not be ported
/// (#3497): a SIGKILLed orphan lingers as a zombie until someone reaps it, so
/// the unix answer needed a state column Windows has no equivalent of. The file
/// is the stronger observation as well as the portable one — it reports what
/// the process is still *doing*, not merely that a table row outlived it, and a
/// writer wedged on a pipe nobody drains is the case a pid check gets wrong in
/// the flattering direction. The fixture heartbeats from a thread that touches
/// no pipe for exactly that reason.
fn still_beating(path: &Path) -> bool {
    let before = beats(path);
    std::thread::sleep(OBSERVATION);
    beats(path) != before
}

/// Poll [`still_beating`] until it answers `false`, up to `budget`.
///
/// The kill is delivered asynchronously and the writer's last append may
/// already be in flight, so a single immediate check would be a race against
/// the kernel rather than a check on the transport. Answering `true` after the
/// whole budget is a real leak: nothing is coming to stop it.
fn outlived(path: &Path, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if !still_beating(path) {
            return false;
        }
    }
    still_beating(path)
}

/// Wait until `path` has been written at all, so a test cannot pass because the
/// process it is watching never started.
fn started_beating(path: &Path, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if beats(path) > 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
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
    let pulse = dir.path().join("writer.pulse");
    let budget = Duration::from_secs(10);
    let over_the_line = MAX_CAPTURE_BYTES + 1024 * 1024;

    let started = Instant::now();
    let err = plugin(
        &[
            "flood",
            &over_the_line.to_string(),
            &pulse.display().to_string(),
        ],
        budget,
    )
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
    // The premise, asserted rather than assumed: the plugin really did run and
    // really was heartbeating, so a still file below means it was stopped and
    // not that it never started.
    assert!(
        beats(&pulse) > 0,
        "the flooding plugin never wrote its heartbeat, so the check below proves nothing"
    );
    // And it cannot resume: the writer is gone, so no further byte can arrive
    // and nothing is blocked on a pipe the host stopped draining.
    assert!(
        !outlived(&pulse, Duration::from_secs(5)),
        "the refused plugin is still running after the call returned",
    );
}

/// **Witness 2.** A plugin that backgrounds a child and then runs out of
/// budget leaves nothing behind.
///
/// `kill_on_drop` reaches the plugin process and stops there; what it started
/// is a grandchild, and before the transport put its children in a group of
/// their own there was nothing that could reach it. It outlived the turn by its
/// full five minutes — on a machine whose user had been told the turn was over.
///
/// **This is also #3550's witness.** That group is a `setsid` process group on
/// unix and a Job Object on Windows, and until this file stopped being a `sh`
/// script the Windows half had no test that could run at all: the mechanism
/// shipped with "it compiles" as its whole evidence.
#[tokio::test]
async fn a_timed_out_plugin_leaves_no_surviving_grandchild() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pulse = dir.path().join("grandchild.pulse");

    let err = plugin(
        &["background", &pulse.display().to_string()],
        Duration::from_millis(500),
    )
    .before_turn(before())
    .await
    .expect_err("the plugin never answers inside its budget");
    assert!(
        matches!(err, WrapperError::Timeout { .. }),
        "the budget is what ended it: {err}"
    );

    // The premise: there really was a grandchild to leak. Without this the
    // test would pass just as happily against a fixture that failed to start
    // one.
    assert!(
        started_beating(&pulse, Duration::from_secs(5)),
        "no grandchild ever ran, so this test would prove nothing about killing one"
    );
    assert!(
        !outlived(&pulse, Duration::from_secs(5)),
        "the backgrounded grandchild outlived the turn it was gathering evidence for",
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

    let started = Instant::now();
    let response = plugin(
        &["trailing", &trailing_past_one_pipe_buffer.to_string()],
        budget,
    )
    .before_turn(before())
    .await
    .expect(
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
