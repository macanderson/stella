//! Supervisor tests (#1552).
//!
//! The decision half is pure and tested as arithmetic. The mechanical half —
//! `setsid`, the liveness lock, the split console, both rungs of the stop
//! escalation — is tested against **real child processes**, because every one
//! of those is a syscall whose behaviour is the thing under test: a fake would
//! assert only that the fake was written to match the assertion.
//!
//! What is NOT covered here, so the list is not mistaken for the whole
//! surface: `Supervised::follow` and `interrupt_and_drain` write to this
//! process's real stdout and stderr, which an in-process test cannot capture,
//! so their streaming is exercised through [`Tail`] — which is where the
//! truncation and lost-tail bugs actually live — rather than end to end. The
//! end-to-end property (a `stella run` in a terminal survives that terminal's
//! hangup) needs a pty and a live run; it is verified by hand, and the
//! procedure is in the PR that added this module.

use super::*;

/// An isolated registry per test. Not a shared temp dir: these tests spawn
/// processes and signal process groups, and a registry two tests could both
/// see is a registry where one test's `stop` can reach the other's child.
fn temp_registry(tag: &str) -> (PathBuf, SessionRegistry) {
    let dir = std::env::temp_dir().join(format!(
        "stella-daemon-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    (dir.clone(), SessionRegistry::open(dir))
}

/// Spawn `script` under the supervisor as `/bin/sh -c <script>`.
fn spawn_sh(registry: &SessionRegistry, title: &str, script: &str) -> Supervised {
    spawn(
        registry,
        "/tmp/workspace",
        title,
        Path::new("/bin/sh"),
        &["-c".into(), script.into()],
        b"",
    )
    .expect("supervised spawn")
}

/// Wait for `predicate`, up to `budget`. Answers whether it came true.
///
/// Generous budgets throughout: these tests assert the difference between
/// "happens" and "never happens", and a bound tight enough to be a latency
/// assertion would be a flake on loaded CI.
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

fn session_of(pid: i32) -> i32 {
    // SAFETY: `getsid` reads kernel state for a pid and cannot fail
    // destructively; -1 on a dead pid is a value, not an error to handle.
    unsafe { libc::getsid(pid) }
}

fn group_of(pid: i32) -> i32 {
    // SAFETY: as above, for the process group.
    unsafe { libc::getpgid(pid) }
}

#[test]
fn a_terminal_run_is_supervised_and_everything_else_is_not() {
    // The case the issue is about: a human typed `stella run` in a terminal.
    assert!(should_supervise(false, false, true));

    // --foreground / STELLA_FOREGROUND is the escape hatch, and wins.
    assert!(!should_supervise(true, false, true));

    // No terminal to lose: a pipe, CI, a container, the Terminal-Bench
    // harness. Supervising here would add a process and a copy of every byte
    // to buy immunity from a hangup that cannot arrive.
    assert!(!should_supervise(false, false, false));

    // A supervised child re-parses the same argv. If it ever reached this
    // decision with `--foreground` lost, it must still refuse — one process
    // does the work, and this is the backstop that keeps it one.
    assert!(!should_supervise(false, true, true));
}

/// The witness for the whole feature: a supervised run leaves the terminal's
/// session, so the `SIGHUP` a closing window sends to that session cannot
/// reach it.
///
/// This is the property stated precisely. A test cannot close this process's
/// terminal — the direct proof would take the test runner down with it — and
/// "the child ignores SIGHUP" would be the wrong property anyway: the child
/// does not ignore anything, it is simply no longer in the session being hung
/// up. That is what `setsid` buys and what this asserts.
#[test]
fn a_supervised_child_leaves_this_process_session_and_group() {
    let (dir, registry) = temp_registry("detached");
    let run = spawn_sh(&registry, "detachment", "sleep 30");
    let child = i32::try_from(run.child.id()).expect("pid fits");

    let ours = std::process::id() as i32;
    assert_ne!(
        session_of(child),
        session_of(ours),
        "a child in our session dies with our terminal"
    );
    assert_ne!(group_of(child), group_of(ours));
    assert_eq!(
        group_of(child),
        child,
        "the child must LEAD its group, or a signal to it misses the tools it spawned"
    );
    assert_eq!(
        run.pgid, child,
        "the recorded group must be the child's own"
    );

    stop(&registry, &run.id).expect("stop");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The registry must name the process doing the work, not the one watching it
/// — the third bug `scripts/self-driving.sh` paid for. Everything downstream reads
/// this pid: the SESSIONS view's liveness, `daemon list`, and the signal
/// `daemon stop` sends.
#[test]
fn the_record_carries_the_childs_pid_not_the_supervisors() {
    let (dir, registry) = temp_registry("childpid");
    let run = spawn_sh(&registry, "pid", "sleep 30");

    let record = registry.get(&run.id).expect("record registered");
    assert_eq!(record.pid, run.child.id());
    assert_ne!(record.pid, std::process::id());
    assert_eq!(
        record.supervisor.as_ref().map(|s| s.pgid),
        Some(i32::try_from(run.child.id()).unwrap())
    );
    assert_eq!(record.status, SessionStatus::InProgress);

    stop(&registry, &run.id).expect("stop");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Liveness is the lock, not the pid (see the module docs). The kernel
/// releases it when the process dies, so this is the one liveness answer a
/// recycled pid cannot forge.
#[test]
fn the_liveness_lock_is_held_while_the_run_lives_and_free_once_it_ends() {
    let (dir, registry) = temp_registry("lock");
    let mut run = spawn_sh(&registry, "lock", "sleep 0.2");
    let sidecar = registry.sidecar_dir(&run.id);

    assert_eq!(
        lock_is_held(&sidecar),
        Some(true),
        "a running child holds its lock"
    );
    let _ = run.child.wait();
    assert!(
        eventually(Duration::from_secs(10), || lock_is_held(&sidecar)
            == Some(false)),
        "the kernel must release the lock when the process exits"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Reattaching is reading the console from the beginning, which is only
/// possible because the streams were written to files — and they stay two
/// files, because a `--output-format json` run is piped into something that
/// would choke on a warning folded into stdout.
#[test]
fn the_console_is_replayable_and_the_two_streams_stay_apart() {
    let (dir, registry) = temp_registry("console");
    let mut run = spawn_sh(
        &registry,
        "console",
        "echo to-stdout; echo to-stderr >&2; sleep 0.1",
    );
    let sidecar = registry.sidecar_dir(&run.id);
    let _ = run.child.wait();

    let out_path = sidecar.join(supervised::STDOUT_LOG);
    let err_path = sidecar.join(supervised::STDERR_LOG);
    assert!(eventually(Duration::from_secs(10), || {
        std::fs::read_to_string(&out_path).is_ok_and(|t| t.contains("to-stdout"))
    }));

    let out = std::fs::read_to_string(&out_path).unwrap();
    let err = std::fs::read_to_string(&err_path).unwrap();
    assert!(!out.contains("to-stderr"), "stdout carried stderr: {out}");
    assert!(!err.contains("to-stdout"), "stderr carried stdout: {err}");

    // What `stella daemon attach` does: a reader arriving after the fact
    // replays everything, because the console is a file and not a pipe that
    // was only ever connected to a terminal that has since gone.
    let mut replay = Tail::open(&out_path).expect("console readable");
    let mut seen = Vec::new();
    replay.pump(&mut seen).expect("replay");
    assert!(String::from_utf8_lossy(&seen).contains("to-stdout"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A stop must reach the whole tree, and must record what it did.
///
/// Both halves are the bugs `scripts/self-driving.sh` paid for: a stop that
/// signals only the leader leaves the tools it spawned running, and a stop
/// that never writes a terminal status leaves a run the operator ended by hand
/// to be presented, forever, as a crash.
#[test]
fn stopping_ends_the_whole_group_and_records_it_as_deliberate() {
    let (dir, registry) = temp_registry("stop");
    // A child with a child: `sh` waits on `sleep`, so a signal that reaches
    // only `sh` leaves `sleep` behind. `sleep` also inherits the liveness
    // lock, which is what makes the lock assertion below a statement about
    // the whole group rather than only its leader.
    let mut run = spawn_sh(&registry, "stop", "sleep 120 & wait");
    let sidecar = registry.sidecar_dir(&run.id);
    let group = run.pgid;

    let id = run.id.clone();
    stop(&registry, &id).expect("stop");

    assert_eq!(
        lock_is_held(&sidecar),
        Some(false),
        "every holder of the lock — `sh` AND the `sleep` it spawned — must be gone"
    );

    // Reap the leader before probing the group: an exited-but-unreaped child
    // is a zombie, and a zombie stays IN its process group until it is reaped
    // — on Linux `kill(-pgid, 0)` on a group containing only a zombie returns
    // success, so this assertion would fail there while passing on macOS,
    // whose `killpg` skips zombies.
    let _ = run.child.wait();
    // The orphaned `sleep` is init's to reap, not ours, so "gone" is
    // eventually-true rather than instantly-true: launchd reaps before the
    // next statement on a laptop; a CI container's init can lose that race,
    // which is a fact about reap latency, not about `stop`.
    assert!(
        eventually(Duration::from_secs(10), || {
            // SAFETY: probing a group with signal 0 sends nothing. The
            // parentheses are load-bearing: a bare `unsafe { … }` opening a
            // statement ends the statement at the block, orphaning the `!=`.
            (unsafe { libc::kill(-group, 0) }) != 0
        }),
        "the whole process group must be gone"
    );

    assert_eq!(
        registry.get(&id).map(|r| r.status),
        Some(SessionStatus::Cancelled),
        "a hand-stopped run must not age into the registry as a crash"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Stopping something that already finished is not an error, and must not
/// signal anything: the pid in the record may by then belong to somebody else
/// entirely.
#[test]
fn stopping_a_finished_run_signals_nothing_and_still_records_the_stop() {
    let (dir, registry) = temp_registry("stop-finished");
    let mut run = spawn_sh(&registry, "finished", "true");
    let _ = run.child.wait();
    let sidecar = registry.sidecar_dir(&run.id);
    assert!(eventually(Duration::from_secs(10), || {
        lock_is_held(&sidecar) == Some(false)
    }));

    stop(&registry, &run.id).expect("stopping a finished run is not an error");
    assert_eq!(
        registry.get(&run.id).map(|r| r.status),
        Some(SessionStatus::Cancelled)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A stop that arrives after the run recorded its own answer must not
/// overwrite it: "it completed" is a fact the run established, and the stopper
/// only knows it was too late.
#[test]
fn a_late_stop_does_not_relabel_a_run_that_already_finished_cleanly() {
    let (dir, registry) = temp_registry("late-stop");
    let mut run = spawn_sh(&registry, "late", "true");
    let _ = run.child.wait();
    registry
        .set_status(&run.id, SessionStatus::Complete)
        .expect("record the run's own answer");

    stop(&registry, &run.id).expect("stop");
    assert_eq!(
        registry.get(&run.id).map(|r| r.status),
        Some(SessionStatus::Complete)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The fallback exists for a child that did NOT come through [`spawn`] — one
/// started by hand with the env var set, or by a future launchd/systemd unit.
/// Such a child starts with no sidecar and no lock file at all, so an
/// implementation that treats "no lock file" as a reason to do nothing is a
/// no-op in exactly the case it was written for.
#[test]
fn the_liveness_fallback_takes_a_lock_that_does_not_exist_yet() {
    let (dir, registry) = temp_registry("fallback-lock");
    let record = SessionRecord::new("/w", "hand-started");
    registry.upsert(&record).unwrap();
    let sidecar = registry.sidecar_dir(&record.id);
    assert_eq!(
        lock_is_held(&sidecar),
        None,
        "precondition: nothing has created a lock file yet"
    );

    hold_liveness_lock(&registry, &record.id);

    assert_eq!(
        lock_is_held(&sidecar),
        Some(true),
        "the fallback must leave this process holding the lock"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The escalation rung. A run that ignores `SIGTERM` — a shell script with a
/// `trap`, or an engine wedged below its own handler — must still end, and
/// must still be recorded as stopped rather than left to age into a crash.
///
/// Deliberately slow: it waits out the real [`STOP_GRACE`], because a test
/// that shortened the grace would be testing a different constant than the one
/// that ships.
#[test]
fn a_run_that_ignores_term_is_killed_after_the_grace_period() {
    let (dir, registry) = temp_registry("stop-ignores-term");
    let mut run = spawn_sh(&registry, "stubborn", "trap '' TERM; sleep 120");
    let sidecar = registry.sidecar_dir(&run.id);
    let id = run.id.clone();

    let started = Instant::now();
    stop(&registry, &id).expect("stop");
    let took = started.elapsed();

    assert!(
        took >= STOP_GRACE,
        "the child was killed after {took:?}, before its {STOP_GRACE:?} to shut down cleanly"
    );
    assert_eq!(
        lock_is_held(&sidecar),
        Some(false),
        "a child that ignores TERM must still be gone"
    );
    assert_eq!(
        registry.get(&id).map(|r| r.status),
        Some(SessionStatus::Cancelled),
        "the kill path must record the stop too — nothing else will"
    );

    let _ = run.child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every terminal read goes through `drain`, because `pump` is bounded: a
/// single bounded read where a run has just ended shows the first chunk of
/// what is left and silently drops the rest — and the rest, at that moment, is
/// the run's answer.
#[test]
fn draining_a_console_larger_than_one_chunk_delivers_all_of_it() {
    let dir = std::env::temp_dir().join(format!("stella-daemon-chunks-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("console");
    // Deliberately not a multiple of the chunk: an off-by-one in the loop's
    // exit condition shows up in the remainder, not in the whole chunks.
    let written = vec![b'x'; PUMP_CHUNK * 2 + 7];
    std::fs::write(&path, &written).unwrap();

    let mut tail = Tail::open(&path).unwrap();
    let mut seen = Vec::new();
    tail.drain(&mut seen).unwrap();
    assert_eq!(seen.len(), written.len());

    // One `pump` is one chunk, which is what makes `drain` necessary.
    let mut once = Tail::open(&path).unwrap();
    let mut partial = Vec::new();
    assert_eq!(once.pump(&mut partial).unwrap(), PUMP_CHUNK);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ids_resolve_by_unique_prefix_and_refuse_an_ambiguous_one() {
    let (dir, registry) = temp_registry("resolve");
    let mut a = SessionRecord::new("/w", "a");
    a.id = "ses-100-1".into();
    a.supervisor = Some(SupervisorInfo { pgid: 1 });
    let mut b = SessionRecord::new("/w", "b");
    b.id = "ses-100-12".into();
    b.supervisor = Some(SupervisorInfo { pgid: 2 });
    let mut plain = SessionRecord::new("/w", "unsupervised");
    plain.id = "ses-200-3".into();
    for record in [&a, &b, &plain] {
        registry.upsert(record).unwrap();
    }

    assert_eq!(resolve(&registry, Some("ses-100-12")).unwrap().id, b.id);
    // An exact hit is never ambiguous, however many ids extend it.
    assert_eq!(resolve(&registry, Some("ses-100-1")).unwrap().id, a.id);
    assert!(resolve(&registry, Some("ses-100")).is_err());
    assert!(resolve(&registry, Some("ses-999")).is_err());
    // Unsupervised sessions are not this command's subject.
    assert!(resolve(&registry, Some("ses-200-3")).is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bare_pid_resolves_the_run_that_is_running_under_it() {
    let (dir, registry) = temp_registry("resolve-pid");
    let mut supervised = SessionRecord::new("/w", "supervised");
    supervised.id = "ses-100-4242".into();
    supervised.pid = 4242;
    supervised.supervisor = Some(SupervisorInfo { pgid: 4242 });
    let mut plain = SessionRecord::new("/w", "unsupervised");
    plain.id = "ses-200-777".into();
    plain.pid = 777;
    for record in [&supervised, &plain] {
        registry.upsert(record).unwrap();
    }

    // The address `ps`, Activity Monitor and an OOM-killer log hand you.
    assert_eq!(resolve(&registry, Some("4242")).unwrap().id, supervised.id);

    // A miss is reported in the address space the caller used: an id begins
    // `ses-`, so "9999" was never a prefix anything could have matched.
    let miss = resolve(&registry, Some("9999")).unwrap_err();
    assert!(miss.contains("pid 9999"), "{miss}");

    // Unsupervised sessions are not this command's subject by pid either.
    assert!(resolve(&registry, Some("777")).is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_pid_two_runs_have_held_is_refused_rather_than_guessed() {
    let (dir, registry) = temp_registry("resolve-pid-recycled");
    // The kernel recycles pids and a finished run keeps the one it ran under,
    // so two records sharing one is reachable, not hypothetical — and picking
    // whichever the registry listed first would stop the wrong run.
    for id in ["ses-100-4242", "ses-900-4242"] {
        let mut record = SessionRecord::new("/w", id);
        record.id = id.into();
        record.pid = 4242;
        record.supervisor = Some(SupervisorInfo { pgid: 4242 });
        registry.upsert(&record).unwrap();
    }

    let ambiguous = resolve(&registry, Some("4242")).unwrap_err();
    assert!(ambiguous.contains("ses-100-4242"), "{ambiguous}");
    assert!(ambiguous.contains("ses-900-4242"), "{ambiguous}");
    // "use more of the id" is useless advice for a pid: there is no more of it.
    assert!(ambiguous.contains("use the id"), "{ambiguous}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_exit_code_is_forwarded_the_way_a_shell_reports_it() {
    let status = |script: &str| {
        std::process::Command::new("/bin/sh")
            .args(["-c", script])
            .status()
            .expect("sh")
    };
    assert_eq!(exit_code(&status("true")), None, "success forwards nothing");
    assert_eq!(exit_code(&status("exit 3")), Some(3));
    // 128 + SIGTERM, the convention `stella run` already answers with.
    assert_eq!(exit_code(&status("kill -TERM $$")), Some(143));
}

#[test]
fn logs_start_the_requested_number_of_lines_back() {
    let dir = std::env::temp_dir().join(format!("stella-daemon-tail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("console");
    std::fs::write(&path, "one\ntwo\nthree\nfour\n").unwrap();

    let mut tail = Tail::open(&path).unwrap();
    tail.seek_back_lines(2).unwrap();
    let mut seen = Vec::new();
    tail.pump(&mut seen).unwrap();
    assert_eq!(String::from_utf8_lossy(&seen), "three\nfour\n");

    // Asking for more than there is starts at the beginning rather than
    // failing: a run that has printed two lines has printed two lines.
    let mut all = Tail::open(&path).unwrap();
    all.seek_back_lines(99).unwrap();
    let mut everything = Vec::new();
    all.pump(&mut everything).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&everything),
        "one\ntwo\nthree\nfour\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A console with no trailing newline is the normal case while a run is still
/// writing — the tail must not treat the unterminated last line as absent.
#[test]
fn a_partial_last_line_is_still_a_line() {
    let dir = std::env::temp_dir().join(format!("stella-daemon-partial-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("console");
    std::fs::write(&path, "one\ntwo\nthree").unwrap();

    let mut tail = Tail::open(&path).unwrap();
    tail.seek_back_lines(2).unwrap();
    let mut seen = Vec::new();
    tail.pump(&mut seen).unwrap();
    assert_eq!(String::from_utf8_lossy(&seen), "two\nthree");

    let _ = std::fs::remove_dir_all(&dir);
}

/// #1586: a resume relaunches under the SAME record — same id, same sidecar —
/// and preserves the killed attempt's console rather than truncating it. On
/// `main`, `launch` does not exist and every spawn mints a fresh record, so a
/// resumed run would strand its checkpoint, its sidecar, and its history
/// under an id nothing references.
#[test]
fn a_resumed_launch_keeps_the_record_and_the_crashed_console() {
    let (_dir, registry) = temp_registry("resume-console");
    let mut record = SessionRecord::new("/tmp/workspace", "resume");
    record.summary = "resume".into();
    // The killed attempt: a terminal-less status and a console tail a human
    // still wants to read.
    record.status = SessionStatus::Error;
    let id = record.id.clone();
    let sidecar = registry.prepare_sidecar(&id).unwrap();
    std::fs::write(
        sidecar.join(supervised::STDOUT_LOG),
        b"the killed attempt's tail\n",
    )
    .unwrap();

    let run = launch(
        &registry,
        record,
        Path::new("/bin/sh"),
        &["-c".into(), "echo the resumed attempt".into()],
        b"",
        Console::Preserve,
        None,
    )
    .expect("relaunch");
    assert_eq!(
        run.id, id,
        "resume must continue the session, not mint a second one"
    );
    assert!(
        eventually(Duration::from_secs(10), || {
            std::fs::read_to_string(sidecar.join(supervised::STDOUT_LOG))
                .is_ok_and(|s| s.contains("the resumed attempt"))
        }),
        "the relaunched child's console must land"
    );
    let console = std::fs::read_to_string(sidecar.join(supervised::STDOUT_LOG)).unwrap();
    assert!(
        console.starts_with("the killed attempt's tail\n"),
        "Console::Preserve must keep the crashed attempt's output: {console:?}"
    );
    let stored = registry.get(&id).unwrap();
    assert_eq!(
        stored.status,
        SessionStatus::InProgress,
        "a relaunch re-owns the record as live"
    );
}
