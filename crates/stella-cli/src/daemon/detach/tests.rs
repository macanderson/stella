// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `--detach` tests (#1607).
//!
//! The decision half is pure and tested as arithmetic; the mechanical half is
//! tested against a **real child process**, for the reason the supervisor's
//! own tests give: `setsid`, the liveness lock and the split console are
//! syscalls, and a fake would only assert that the fake matched the assertion.
//!
//! The scaffolding here (a per-test registry, a `/bin/sh` child, a bounded
//! wait) deliberately does not reach into `super::super::tests` for its
//! equivalents: those are private to that module, and widening them would put
//! this change inside hunks that two open PRs are editing. It is a dozen lines.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use stella_store::{SessionRegistry, SessionStatus};

use super::*;
use crate::daemon::{lock_is_held, resolve, spawn, stop};

/// An isolated registry per test — these tests signal process groups, and a
/// shared registry is one where a test's `stop` can reach another's child.
fn temp_registry(tag: &str) -> (PathBuf, SessionRegistry) {
    let dir = std::env::temp_dir().join(format!(
        "stella-detach-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    (dir.clone(), SessionRegistry::open(dir))
}

/// Wait for `predicate`, up to `budget`. Answers whether it came true. Budgets
/// are generous on purpose: the assertion is "happens" versus "never happens",
/// and a bound tight enough to measure latency would flake on loaded CI.
fn eventually(budget: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    predicate()
}

/// Whether the run behind `id` is still alive, asked the way the supervisor
/// itself asks it.
///
/// Deliberately not `kill(pid, 0)`. Nothing reaps the child here — production's
/// launcher exits and init does it, but this test *is* the launcher and stays —
/// so a finished child lingers as a zombie whose pid still signals. The
/// liveness lock has no such ambiguity: the kernel releases it when the process
/// dies, zombie or not, which is exactly why `stop` reads it rather than a pid.
fn alive(registry: &SessionRegistry, id: &str) -> bool {
    lock_is_held(&registry.sidecar_dir(id)) == Some(true)
}

/// The witness for `--detach`: a run handed to the supervisor keeps running
/// once the launcher has let go of it, and everything needed to come back to
/// it — the registry row, the console, the stop — still works from a process
/// that holds no handle on it at all.
///
/// "The launcher let go" is modelled exactly as production models it:
/// [`release`] consumes the [`Supervised`] handle, which is the only thing
/// this process ever held. After that line there is no `Child`, no console
/// tail and no pgid in scope, which is the same state the shell is in after
/// `stella run --detach` returns. The remaining hop — this *process* also
/// exiting — is what `setsid` covers, and the supervisor's own
/// `a_supervised_child_leaves_this_process_session_and_group` is its witness;
/// a test cannot exit its own runner to prove it a second time.
#[test]
fn a_detached_run_outlives_its_launcher_and_stays_discoverable_and_stoppable() {
    let (dir, registry) = temp_registry("outlives");

    let id = {
        let run = spawn(
            &registry,
            "/tmp/workspace",
            "detached run",
            Path::new("/bin/sh"),
            &["-c".into(), "echo working; sleep 30".into()],
            b"",
        )
        .expect("supervised spawn");
        let id = run.id.clone();
        // The launcher returns here. Everything below runs with no handle.
        release(run, crate::OutputFormat::Text).expect("release");
        id
    };

    // It outlived the handle.
    assert!(
        alive(&registry, &id),
        "the detached run died when its launcher let go of it"
    );

    // It is discoverable — by full id and by the prefix a human would type.
    let found = resolve(&registry, Some(&id)).expect("resolve by id");
    assert_eq!(found.id, id);
    assert!(found.status.is_live(), "status was {:?}", found.status);
    let prefix = &id[..id.len() - 3];
    assert_eq!(
        resolve(&registry, Some(prefix))
            .expect("resolve by prefix")
            .id,
        id
    );

    // Its output reached the console with no terminal attached to receive it.
    let out = registry
        .sidecar_dir(&id)
        .join(stella_store::supervised::STDOUT_LOG);
    assert!(
        eventually(Duration::from_secs(10), || {
            std::fs::read_to_string(&out).is_ok_and(|s| s.contains("working"))
        }),
        "the detached run's stdout never reached {}",
        out.display()
    );

    // And it is stoppable from here, where nothing holds it.
    stop(&registry, &id).expect("stop the detached run");
    assert!(
        eventually(Duration::from_secs(10), || !alive(&registry, &id)),
        "the detached run survived stop"
    );
    assert_eq!(
        registry.get(&id).map(|r| r.status),
        Some(SessionStatus::Cancelled),
        "a stop somebody asked for must not age into the registry as a crash"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// `--detach` is asked for a different property than supervision is, so it
/// answers a different question about the terminal.
///
/// Supervision declines without a controlling terminal because there is no
/// hangup to survive. "Give me the prompt back" is wanted just as much from a
/// pipe, a CI step or a `cron` job — a shell script that launches a run and
/// exits is the whole point — so the flag forces it.
#[test]
fn detach_forces_supervision_where_a_bare_run_would_decline_it() {
    // No terminal: supervision declines, --detach does not.
    assert!(!crate::daemon::should_supervise(false, false, false));
    assert_eq!(posture(false, true, false, false), Posture::Detached);

    // With a terminal, --detach only changes whether the launcher stays.
    assert_eq!(posture(false, false, false, true), Posture::Attached);
    assert_eq!(posture(false, true, false, true), Posture::Detached);

    // And with neither flag nor terminal, nothing changed.
    assert_eq!(posture(false, false, false, false), Posture::Foreground);
}

/// The two ways `--detach` must lose, and the second one is load-bearing.
///
/// A supervised child re-parses its parent's argv verbatim, `--detach`
/// included, and is told to do the work through `STELLA_FOREGROUND=1`. If
/// either rule here failed, that child would supervise itself again — one
/// process per turn, forever.
#[test]
fn foreground_and_the_recursion_backstop_both_beat_detach() {
    assert_eq!(posture(true, true, false, true), Posture::Foreground);
    assert_eq!(posture(false, true, true, true), Posture::Foreground);
    // The child's real shape: both, since the env sets one and argv the other.
    assert_eq!(posture(true, true, true, true), Posture::Foreground);
}

#[test]
fn only_the_foreground_posture_keeps_the_work_in_this_process() {
    assert!(!Posture::Foreground.supervises());
    assert!(Posture::Attached.supervises());
    assert!(Posture::Detached.supervises());
}

/// **Witness (#1688).** A detached launch under a machine-readable format
/// names its session on stdout.
///
/// `stella run "…" --detach --output-format json` wrote **nothing at all** to
/// stdout, so a script piping it into `jq` got an empty stream and could not
/// learn the id it needs to poll — for the flag whose main use is exactly that
/// script. The id was recoverable only by scraping the stderr banner or racing
/// `stella daemon list`.
#[test]
fn a_detached_launch_names_its_session_on_stdout_under_json() {
    let summary = super::summary_json(crate::OutputFormat::Json, "sess-1688")
        .expect("json asks for a machine-readable answer");
    let value: serde_json::Value = serde_json::from_str(&summary).expect("parseable by a consumer");

    assert_eq!(
        value["session_id"], "sess-1688",
        "the one value a script cannot proceed without: {summary}"
    );
    assert_eq!(value["schema_version"], crate::SUMMARY_SCHEMA_VERSION);
    assert_eq!(value["status"], "detached");
    assert_eq!(value["attach"], "stella daemon attach sess-1688");
    assert_eq!(value["stop"], "stella daemon stop sess-1688");
}

/// `stream-json` gets the object compact, so the line-delimited contract holds.
///
/// A pretty-printed object here would be a multi-line document on a stream
/// whose consumers split on newlines — the failure would land in the consumer,
/// not in Stella, which is why it is pinned rather than left to the formatter.
#[test]
fn the_stream_json_launch_summary_occupies_exactly_one_line() {
    let summary = super::summary_json(crate::OutputFormat::StreamJson, "sess-1688")
        .expect("stream-json asks for a machine-readable answer");

    assert!(
        !summary.contains('\n'),
        "one JSON document per line: {summary:?}"
    );
    let value: serde_json::Value = serde_json::from_str(&summary).expect("parseable");
    assert_eq!(value["session_id"], "sess-1688");
}

/// A human run keeps stdout clean — the banner is the answer, on stderr.
///
/// The inverse of the witness above, and the reason the fix is gated on the
/// format rather than always printing: a text-mode `stella run --detach` that
/// started emitting JSON would be a regression for every interactive user.
#[test]
fn a_text_launch_writes_nothing_to_stdout() {
    assert!(super::summary_json(crate::OutputFormat::Text, "sess-1688").is_none());
}
