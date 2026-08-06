// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The supervisor: how a run outlives the terminal that started it (#1552).
//!
//! Closing a terminal window sends `SIGHUP` to its foreground process group,
//! and a `stella run` in that group dies mid-turn — no answer, no record of
//! why, and no way back to it. That is a property of *how the process was
//! started*, not of anything stella decided, so the fix belongs here rather
//! than anywhere in the engine.
//!
//! # What this module does, and what it deliberately does not
//!
//! This is **process-level supervision**: the work is re-launched as a
//! detached child in its own session, its console is two files in the
//! session's sidecar, and the parent stays only to stream those files back to
//! the terminal. Close the terminal and the parent dies; the child does not
//! notice, because it left that session before it began.
//!
//! Process-level supervision is the weaker of the two properties #1552 named,
//! and the engine-level one now stands behind it (#1586): every turn already
//! checkpoints into the workspace's work journal at each committed step
//! boundary, so a supervised run whose *process* is killed — OOM, `kill -9`,
//! power loss — leaves a resume point where a mere closed terminal never
//! needed one. [`resume_supervised`] is the verb that picks it up: it
//! relaunches the same session under a fresh supervised child, which
//! continues the interrupted turn from that boundary instead of restarting
//! it (`crate::agent::resume`).
//!
//! # Why supervision keys on a controlling terminal
//!
//! [`should_supervise`] answers yes only when this process has a controlling
//! terminal. The reasoning is symmetrical: a terminal is exactly what can
//! close, so a process without one is already immune and supervising it buys
//! nothing while adding a process, two files, and a copy to every byte of
//! output. Concretely that leaves every non-interactive caller — CI, a pipe,
//! a container, and the Terminal-Bench harness, which runs `stella run` on
//! pipes with no terminal — on exactly the process shape they run today.
//!
//! `nohup` is deliberately NOT in that list. It redirects the standard
//! streams but never detaches the controlling terminal, so
//! `nohup stella run &` still opens `/dev/tty` and still gets supervised. It
//! loses nothing by that — a supervised run is at least as durable as a
//! nohup'd one — but its output is written to the sidecar consoles and copied
//! to `nohup.out`, rather than only the latter.
//!
//! # The three bugs this inherits rather than rediscovers
//!
//! `scripts/fullauto.sh` has supervised its own cycles for long enough to pay
//! for three, and each one is load-bearing here:
//!
//! 1. **The child must lead its own process group**, or stopping the
//!    supervisor orphans the work. [`spawn`] calls `setsid` before `exec` and
//!    treats a failure as a failed spawn, so a supervised child is *always* a
//!    session leader and `pgid == pid` is an invariant rather than a hope.
//! 2. **Stopping must not race the child's own shutdown.** The engine aborts
//!    at safe boundaries (invariant 6), which takes seconds; a stop that
//!    escalates to `SIGKILL` after one second lands mid-teardown, the terminal
//!    status is never written, and a run the operator stopped by hand ages
//!    into the registry as a crash. [`stop`] waits [`STOP_GRACE`] and records
//!    the transition on every path, including the one where it had to kill.
//! 3. **Record the child's pid, not the supervisor's.** The supervisor's pid
//!    says a supervisor exists; only the child's says the work is running, and
//!    it is the sole handle anything outside this process has on it.
//!
//! # Liveness is a lock, not a pid
//!
//! Every other reader of the session registry answers "is it alive?" with
//! `kill(pid, 0)`, which is right for *display*: the worst a recycled pid does
//! there is show a dead session as live. This module signals process groups,
//! where the worst a recycled pgid does is take down a stranger's processes.
//!
//! So a supervised child holds an advisory lock
//! ([`stella_store::supervised::LOCK`]) for its entire life, taken in the same
//! `pre_exec` as `setsid` and never closed. The kernel releases it when the
//! process dies — `SIGKILL`, panic and power loss included — so a free lock
//! means the run is over, and [`stop`] does not signal once it can take that
//! lock itself.
//!
//! Two things that discipline requires, both easy to get wrong:
//!
//! - The lock belongs to the **stella process**, not to its descendants.
//!   `flock` is held by an open file description, which `fork`+`exec`
//!   inherits, so [`hold_liveness_lock`] marks it `FD_CLOEXEC` the moment the
//!   child is running. Without that a backgrounded dev server would go on
//!   holding a finished run's lock, and `stop` would kill it on that run's
//!   behalf.
//! - "Does not signal once it can take the lock" is a check, not an
//!   interlock. Nothing holds the lock between [`stop`]'s last poll and its
//!   `kill`, so a run that ends inside that window is signalled anyway. It
//!   takes full pid-space wraparound in tens of milliseconds to matter, which
//!   is why this is a caveat rather than a design; POSIX offers no way to
//!   close it.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use colored::{ColoredString, Colorize};
use stella_store::{SessionRecord, SessionRegistry, SessionStatus, SupervisorInfo, supervised};

use crate::DaemonCmd;

/// `install` / `uninstall`: the service-manager half (#1587) — what makes a
/// registered invocation come back after the logout and reboot that
/// supervision alone cannot cross.
mod service;

/// `resume-all` and `install --resume-all`: the boot-time sweep (#1627) that
/// makes a registered service *continue* the turns a reboot killed rather
/// than restart them.
mod boot;

pub(crate) mod approval;
pub(crate) mod console;

/// `--detach`: the same supervised launch, without the launcher staying to
/// stream it (#1607).
pub(crate) mod detach;

/// Carries the supervised session's registry id into the child.
///
/// Two jobs. It tells the child which record to stamp on its way out — the
/// child is the only process guaranteed to still be there at the end, so a
/// terminal status written by anyone else is a status that goes missing
/// exactly when the terminal was closed. And it is the recursion backstop: a
/// child that somehow reached [`should_supervise`] with an argv that lost
/// `--foreground` still refuses to supervise itself.
pub(crate) const SUPERVISED_ENV: &str = "STELLA_SUPERVISED";

/// How long [`stop`] lets a child shut down before escalating to `SIGKILL`.
///
/// Sized for what the child is being asked to do, not for how long a human
/// will wait: `SIGTERM` reaches the engine's own handler, which unwinds the
/// turn's RAII guards — reaping tool process groups, releasing fleet claims,
/// removing shadow worktrees — and only then writes the terminal status. The
/// value comes from `scripts/fullauto.sh`, where a shorter one was measured
/// killing the handler mid-flight and recording hand-stopped runs as crashes.
const STOP_GRACE: Duration = Duration::from_secs(8);

/// How often the follow loop and [`stop`] re-check. Cheap: both poll a file
/// offset or a lock, never a process.
const POLL: Duration = Duration::from_millis(80);

/// The two signals this module sends, named once so the call sites are not
/// each a `cfg`. `libc`'s Windows module defines neither, and the rest of the
/// file carries `#[cfg(not(unix))]` stubs — an ungated `libc::SIGKILL` would
/// make that stated portability a compile error waiting for the first Windows
/// build. Where these are unreachable, [`signal_group`] is already a no-op.
#[cfg(unix)]
use libc::{SIGKILL, SIGTERM};
#[cfg(not(unix))]
const SIGTERM: i32 = 15;
#[cfg(not(unix))]
const SIGKILL: i32 = 9;

/// A live supervised child, owned by the parent that spawned it.
pub(crate) struct Supervised {
    /// The registry id — what `stella daemon attach` takes.
    pub(crate) id: String,
    /// The child's process group, which is also its pid ([`spawn`]).
    pgid: i32,
    child: std::process::Child,
    /// The two consoles, held across the whole supervision rather than
    /// reopened per phase, so an interrupt resumes streaming from the byte the
    /// terminal has actually seen. Reopening and seeking to the end instead
    /// silently drops everything written since the last poll — which on the
    /// interrupt path is the run's own shutdown output, the part a user who
    /// just pressed Ctrl-C is most likely to want.
    out: Tail,
    err: Tail,
}

/// Whether this invocation should be handed to the supervisor.
///
/// Pure over already-observed booleans so the decision is directly testable;
/// the caller does the observing. `foreground` is the user's `--foreground`
/// (or `STELLA_FOREGROUND`), `already_supervised` is [`SUPERVISED_ENV`] being
/// set, and `has_controlling_terminal` is the module doc's rule.
pub(crate) fn should_supervise(
    foreground: bool,
    already_supervised: bool,
    has_controlling_terminal: bool,
) -> bool {
    !foreground && !already_supervised && has_controlling_terminal
}

/// This process's supervised session id, captured once at startup.
static SUPERVISED: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// Take [`SUPERVISED_ENV`] out of the environment and remember it.
///
/// **Consumed, not read.** Left in the environment it is inherited by every
/// process the run spawns, and every one of those is a `stella` away from
/// claiming to be this session. The repo's own workflow makes that concrete:
/// a supervised run whose agent executes `make smoke` runs `stella models`,
/// which on exit would stamp a terminal status onto its *parent's* still-live
/// record. A nested long-running verb is worse — it reaches
/// `agent::presence`, adopts the outer record, rewrites its title, and
/// finishes it.
///
/// Takes the startup token for the same reason `credential_handoff` does:
/// writing the process environment is only safe before the first thread, and
/// the token is what bounds the window in code rather than in a comment.
pub(crate) fn consume_supervised_env(_startup: &crate::startup::StartupPhase) {
    let id = std::env::var(SUPERVISED_ENV)
        .ok()
        .filter(|id| !id.trim().is_empty());
    if id.is_some() {
        // SAFETY: single-threaded startup, holding the one token that permits
        // an environment write (#1140).
        unsafe { std::env::remove_var(SUPERVISED_ENV) };
    }
    let _ = SUPERVISED.set(id);
}

/// This process's supervised session id, when it is itself a supervised child.
///
/// Answers `None` in any process that never called [`consume_supervised_env`]
/// — which is every test binary, and is the correct answer for them.
pub(crate) fn supervised_id() -> Option<String> {
    SUPERVISED.get().cloned().flatten()
}

/// The exit code a supervised child answered with, for `main` to forward.
///
/// A static for the same reason [`crate::signals`] uses one: `run` threads a
/// plain `Result<(), String>`, and widening that error type to carry a number
/// would touch every `?` in the binary to serve one caller.
static FORWARDED: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(u16::MAX);

/// The exit code this process owes the shell on behalf of its supervised
/// child, if it had one that exited nonzero.
///
/// Fidelity here is the difference between a supervisor and a wrapper: a
/// script that reads `stella run …; echo $?` must get the run's answer, not
/// the answer to "did the supervisor manage to stream a log".
pub(crate) fn forwarded_exit_code() -> Option<u8> {
    match FORWARDED.load(std::sync::atomic::Ordering::SeqCst) {
        u16::MAX => None,
        code => u8::try_from(code).ok(),
    }
}

/// Hand this entire invocation to a supervised child, and stream it back here
/// until it finishes or this terminal goes away.
///
/// The child re-parses the same argv with `--foreground` appended, so the
/// division of labour is exactly one process doing the work and one process
/// watching — no argument is re-derived, re-quoted, or dropped on the way.
///
/// `posture` decides whether this process then stays: [`detach::Posture::Attached`]
/// streams the child back here until it ends, [`detach::Posture::Detached`]
/// returns the moment the child is registered (#1607). Both spawn through the
/// same [`spawn`], because "the launcher does not stay" is the only difference
/// between them and a second launch path is how the two would drift.
pub(crate) fn supervise_this_invocation(
    rt: tokio::runtime::Runtime,
    workspace: &Path,
    title: &str,
    stdin: &[u8],
    posture: detach::Posture,
) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate the stella binary to supervise: {e}"))?;
    // This process's argv **verbatim**: every alternative re-derives arguments
    // clap already parsed, and a reconstruction is one forgotten flag away
    // from running something the user did not type.
    //
    // Verbatim means verbatim — nothing is appended. `--foreground` used to
    // be, and after a `--` separator clap treats everything as positional, so
    // it silently became data: `stella run -- --my-prompt` (the invocation
    // `--prompt`'s own help documents) got a second positional and died with a
    // usage error, and `stella fleet -- 'fix a' 'fix b'` gained a THIRD task
    // whose prompt was the word `--foreground`. The child is told through its
    // environment instead, which no argument can be mistaken for.
    //
    // `args_os`, not `args`: the latter panics on argv that is not UTF-8, and
    // clap accepts non-UTF-8 in any `PathBuf` argument — so a `--log-file`
    // clap was happy with would abort the supervisor.
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();

    let registry = SessionRegistry::open_default();
    let run = spawn(
        &registry,
        &workspace.display().to_string(),
        title,
        &exe,
        &args,
        stdin,
    )?;
    if posture == detach::Posture::Detached {
        return detach::release(run);
    }
    run.announce();
    watch(rt, &registry, run)
}

/// Stream a just-spawned child to this terminal until it finishes — or until
/// this terminal's human interrupts, which stops the child rather than
/// abandoning it.
///
/// Shared by the two ways a supervised child comes to exist: a fresh
/// [`supervise_this_invocation`] and a [`resume_supervised`] relaunch. One
/// implementation because the interrupt path is the subtle half — Ctrl-C must
/// reach the child as a stop, and a divergent copy here is how a resumed run
/// would come to treat it as a detach.
fn watch(
    rt: tokio::runtime::Runtime,
    registry: &SessionRegistry,
    mut run: Supervised,
) -> Result<(), String> {
    match rt.block_on(crate::signals::until_interrupted(run.follow())) {
        Ok(followed) => {
            rt.shutdown_timeout(Duration::from_secs(2));
            if let Some(code) = followed? {
                FORWARDED.store(u16::from(code), std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        }
        Err(signal) => {
            // Ctrl-C means stop, here as everywhere else — so the child is
            // asked to stop and this process stays to watch it happen, rather
            // than dropping the work future the way `block_on_interruptible`
            // would. The work is not on this stack to drop.
            crate::signals::note_interrupt(signal);
            // The child writes its own machine-readable error envelope, and
            // this process copies it through verbatim. `main`'s catch-all
            // would print a SECOND one describing the supervisor's view of
            // the same interruption, leaving two JSON documents on the stdout
            // of a `--output-format json | jq`. Claiming the slot the child
            // already filled is what suppresses it.
            crate::note_json_summary_emitted();
            let drained = rt.block_on(run.interrupt_and_drain());
            mark_stopped(registry, &run.id);
            rt.shutdown_timeout(Duration::from_secs(2));
            drained?;
            Err(signal.reason().to_string())
        }
    }
}

/// Whether this process has a controlling terminal — something that can close
/// and take it down with a `SIGHUP`.
///
/// Opening `/dev/tty` is the question itself rather than a proxy for it:
/// `isatty(0)` answers "is stdin a terminal", which is a different and weaker
/// thing (`stella run … | tee log` redirects stdout, keeps the terminal, and
/// still dies when the window closes).
pub(crate) fn has_controlling_terminal() -> bool {
    #[cfg(unix)]
    {
        std::fs::OpenOptions::new()
            .read(true)
            .open("/dev/tty")
            .is_ok()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Launch `program args…` as a detached child and register it as a supervised
/// session.
///
/// `program` is a parameter rather than `current_exe()` so the detachment
/// itself — `setsid`, the liveness lock, the split console, the stop
/// escalation — can be tested against a real child process instead of against
/// a second copy of stella that would need a provider, a key and a workspace
/// to say anything. Production has one caller and it passes the running
/// binary.
///
/// `stdin` is whatever the parent already consumed (an empty slice when it
/// consumed nothing) — see [`stella_store::supervised::STDIN`] for why it
/// travels as a file rather than as an argument.
pub(crate) fn spawn(
    registry: &SessionRegistry,
    workspace: &str,
    title: &str,
    program: &Path,
    args: &[std::ffi::OsString],
    stdin: &[u8],
) -> Result<Supervised, String> {
    // Minted before the launch so a failed spawn leaves a directory rather
    // than a half-registered session.
    let mut record = SessionRecord::new(workspace, title);
    record.summary = title.to_string();
    launch(registry, record, program, args, stdin, Console::Fresh, None)
}

/// How a launched child's console files begin life.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Console {
    /// A fresh run: truncate whatever a previous life left behind.
    Fresh,
    /// A resumed run (#1586): append, keeping the killed attempt's output —
    /// that tail is the context a human reads to understand why there was
    /// something to resume, and `attach --from-start` should show one
    /// continuous story.
    Preserve,
}

/// The mechanical half of [`spawn`], over a caller-supplied record: detach,
/// take the liveness lock, wire the console, register. A fresh spawn mints
/// its record; a resume ([`resume_supervised`]) re-launches under the record
/// — and therefore the session id — it already has, because the checkpoint,
/// the sidecar, and the registry row are all keyed by that id and a new one
/// would strand every piece of state this exists to pick back up.
///
/// `cwd` pins the child's working directory. A fresh spawn inherits this
/// process's (the child re-parses the same argv, and relative paths in it
/// must keep meaning what the user meant); a resume sets the record's
/// workspace, because the resuming parent may be run from anywhere.
fn launch(
    registry: &SessionRegistry,
    mut record: SessionRecord,
    program: &Path,
    args: &[impl AsRef<std::ffi::OsStr>],
    stdin: &[u8],
    console: Console,
    cwd: Option<&Path>,
) -> Result<Supervised, String> {
    // Forced back to in-progress up front: a fresh record is born live, but a
    // resume's adopted record still carries the killed attempt's terminal
    // reading, and the registration below must never show it.
    record.status = SessionStatus::InProgress;
    let sidecar = registry
        .prepare_sidecar(&record.id)
        .map_err(|e| format!("cannot create the session directory: {e}"))?;

    let stdin_path = sidecar.join(supervised::STDIN);
    stella_store::write_sensitive_file_atomic(&stdin_path, stdin)
        .map_err(|e| format!("cannot stage the prompt for the supervised run: {e}"))?;
    let stdin_file = std::fs::File::open(&stdin_path)
        .map_err(|e| format!("cannot reopen the staged prompt: {e}"))?;

    let out_path = sidecar.join(supervised::STDOUT_LOG);
    let err_path = sidecar.join(supervised::STDERR_LOG);
    let out_file = create_console(&out_path, console)?;
    let err_file = create_console(&err_path, console)?;

    // Registered BEFORE the spawn. The child looks its own record up by id and
    // continues it (`agent::presence`), and it can reach that lookup before a
    // descheduled parent gets back to writing — on a loaded machine the parent
    // has a file write to do and the child only has to finish `exec`. A miss
    // there does not fail loudly; it mints a SECOND record, splitting the
    // session into the two halves the adoption exists to prevent, with
    // `daemon stop` holding one id and the journal accruing under the other.
    //
    // The pid is this process's until the spawn returns, and is corrected
    // below. That is the safe direction to be briefly wrong in: a live record
    // naming a live pid reads as running, which it is.
    registry
        .upsert(&record)
        .map_err(|e| format!("cannot register the supervised run: {e}"))?;

    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .env(SUPERVISED_ENV, &record.id)
        // The child does the work rather than supervising it again. Through
        // the environment, not argv, so a `--` separator cannot turn it into a
        // prompt or a fleet task — see `supervise_this_invocation`.
        .env("STELLA_FOREGROUND", "1")
        .stdin(std::process::Stdio::from(stdin_file))
        .stdout(std::process::Stdio::from(out_file))
        .stderr(std::process::Stdio::from(err_file));
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    #[cfg(unix)]
    detach_before_exec(&mut command, sidecar.join(supervised::LOCK));

    let child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = registry.remove(&record.id);
            return Err(format!("cannot start the supervised run: {e}"));
        }
    };

    // The CHILD's pid, deliberately: the supervisor's says a supervisor
    // exists, this says the work is running. Every liveness check in the
    // registry, and every signal `stop` sends, reads it. Status is forced
    // back to in-progress for the resume path, whose adopted record still
    // carries the killed attempt's terminal reading.
    let pid = child.id();
    let Ok(pgid) = i32::try_from(pid) else {
        // The child is already running and detached. Leaving it would be
        // leaving a process nothing can name, find, or stop.
        abandon(registry, &record.id, &mut { child });
        return Err(format!(
            "supervised child pid {pid} does not fit a process group"
        ));
    };
    record.pid = pid;
    record.supervisor = Some(SupervisorInfo { pgid });
    if let Err(e) = registry.upsert(&record) {
        abandon(registry, &record.id, &mut { child });
        return Err(format!("cannot register the supervised run: {e}"));
    }

    let out = Tail::open(&out_path)?;
    let err = Tail::open(&err_path)?;
    Ok(Supervised {
        id: record.id,
        pgid,
        child,
        out,
        err,
    })
}

/// Take down a child that was spawned but could not be registered, and drop
/// its record.
///
/// The alternative is returning an error while a detached process keeps
/// running with no id, no row in `daemon list`, and no way to stop it short of
/// `ps`. Signalling the pid rather than the group because the group is exactly
/// the number we failed to establish.
fn abandon(registry: &SessionRegistry, id: &str, child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
    let _ = registry.remove(id);
}

/// Create a console file that is readable only by its owner.
///
/// A run's stdout carries whatever the model said and whatever the tools
/// printed. `ensure_private_dir` already restricts the directory; this keeps
/// the file itself from being the exception if the directory's mode is ever
/// widened by hand.
fn create_console(path: &Path, console: Console) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true);
    match console {
        Console::Fresh => options.truncate(true),
        Console::Preserve => options.append(true),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))
}

/// Everything that must happen between `fork` and `exec` for the child to be
/// genuinely detached: leave the terminal's session, and take the liveness
/// lock.
///
/// This runs in the post-`fork` window, so it does only what is safe there:
/// three raw syscalls (`setsid` and `open` are on POSIX's async-signal-safe
/// list; `flock` is not on the list but is a bare syscall with no library
/// state), no allocation, and no lock any other thread of the forked parent
/// could have held. The path is turned into a `CString` in the parent, above.
///
/// Either failing fails the spawn — a child that half-detached is worse than
/// one that never started, because it looks supervised in the registry while
/// still dying with the terminal.
#[cfg(unix)]
fn detach_before_exec(command: &mut std::process::Command, lock_path: PathBuf) {
    use std::os::unix::process::CommandExt;

    // An empty string cannot be a real lock path (`sidecar.join(LOCK)` is
    // never empty), so it doubles as the "path had an interior NUL" signal —
    // and lets the post-fork closure report it with a plain errno instead of
    // allocating an error message where it must not allocate.
    let lock = std::ffi::CString::new(lock_path.into_os_string().into_encoded_bytes())
        .unwrap_or_else(|_| c"".to_owned());
    // SAFETY: see the doc comment — allocation-free, async-signal-safe calls
    // only, on data prepared before the fork.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if lock.as_bytes().is_empty() {
                return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
            }
            // Deliberately NOT `O_CLOEXEC`: the lock must survive the `exec`
            // that follows, and only an inherited descriptor can. The child
            // makes it non-inheritable again the moment it is running — see
            // `hold_liveness_lock`, and the note there about why leaving it
            // inheritable is not an option.
            let fd = libc::open(lock.as_ptr(), libc::O_RDWR | libc::O_CREAT, 0o600);
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

impl Supervised {
    /// The banner the terminal sees, once, before the child's first byte.
    ///
    /// It is printed rather than assumed because supervision changes two
    /// things a user can otherwise only discover the hard way: the run is no
    /// longer this terminal's to lose, and there is an id to come back to.
    ///
    /// There is deliberately no scope-review caveat here any more: a
    /// supervised plan that expands scope parks and asks through the sidecar
    /// (#1585) — this terminal while it stays, any `stella daemon attach`
    /// after it goes — so supervision no longer takes that answer away.
    pub(crate) fn announce(&self) {
        eprintln!(
            "{} {} — survives this terminal closing",
            "▸ supervised".green().bold(),
            self.id.dimmed()
        );
        eprintln!(
            "  reattach with {}",
            format!("stella daemon attach {}", self.id).cyan()
        );
    }

    /// Stream the child's console to this terminal until it exits, and answer
    /// its exit code.
    ///
    /// stdout and stderr are replayed onto stdout and stderr, never merged:
    /// see [`stella_store::supervised::STDOUT_LOG`].
    pub(crate) async fn follow(&mut self) -> Result<Option<u8>, String> {
        loop {
            let moved = self.pump()?;
            match self
                .child
                .try_wait()
                .map_err(|e| format!("cannot wait on the supervised run: {e}"))?
            {
                Some(status) => {
                    // Ordered after the wait on purpose: this drain catches
                    // whatever the child wrote between the pump above and its
                    // exit, and once it has exited nothing more can appear.
                    self.drain()?;
                    return Ok(exit_code(&status));
                }
                None if moved == 0 => tokio::time::sleep(POLL).await,
                None => {}
            }
        }
    }

    /// Move one chunk of each console onto the matching stream, and answer how
    /// many bytes that was. For the live loop, which comes straight back.
    fn pump(&mut self) -> Result<usize, String> {
        let out = self.out.pump(&mut std::io::stdout())?;
        let err = self.err.pump(&mut std::io::stderr())?;
        Ok(out + err)
    }

    /// Move everything both consoles hold. For the reads that will not come
    /// back — see [`Tail::drain`].
    fn drain(&mut self) -> Result<(), String> {
        self.out.drain(&mut std::io::stdout())?;
        self.err.drain(&mut std::io::stderr())
    }

    /// Ask the child to stop the way `SIGTERM` from anywhere else would, then
    /// keep streaming until it is gone.
    ///
    /// Used when the *parent* is interrupted: Ctrl-C in a terminal reaches
    /// only the foreground group, which the child left, so without this a
    /// Ctrl-C would detach the run rather than stop it — the opposite of what
    /// the key means everywhere else.
    /// The consoles continue from wherever [`Self::follow`] left them. They
    /// used to be reopened and seeked to the end here, which threw away
    /// everything written since the last poll — on this path that is the run's
    /// own shutdown output, which is exactly what somebody who just pressed
    /// Ctrl-C is waiting to read.
    pub(crate) async fn interrupt_and_drain(&mut self) -> Result<(), String> {
        signal_group(self.pgid, SIGTERM);
        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline {
            self.pump()?;
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                self.drain()?;
                return Ok(());
            }
            tokio::time::sleep(POLL).await;
        }
        eprintln!(
            "{} supervised run {} did not stop within {}s; killing it",
            "⚠".yellow(),
            self.id.dimmed(),
            STOP_GRACE.as_secs()
        );
        signal_group(self.pgid, SIGKILL);
        let _ = self.child.wait();
        self.drain()?;
        Ok(())
    }
}

/// A file being read forward as something else appends to it.
struct Tail {
    file: std::fs::File,
    offset: u64,
}

impl Tail {
    fn open(path: &Path) -> Result<Self, String> {
        let file = std::fs::File::open(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Ok(Self { file, offset: 0 })
    }

    // There is deliberately no `seek_to_end` here, for the same reason
    // `console::Follower` carries no `skip_to_now`: that pair was the only
    // caller, and #1632 removed it as a defect — seeking past everything
    // already written threw away the run's shutdown output, which on the
    // Ctrl-C path is precisely what the person who pressed the key is
    // waiting to read.

    /// Start `lines` lines back from the end, or at the beginning if the file
    /// is shorter than that.
    ///
    /// Reads the whole file rather than scanning backwards in blocks: a
    /// console is bounded by one run's output, and a block-wise reverse scan
    /// is three times the code for a saving nobody can perceive.
    fn seek_back_lines(&mut self, lines: usize) -> Result<(), String> {
        let mut all = Vec::new();
        self.file
            .read_to_end(&mut all)
            .map_err(|e| format!("cannot read the console: {e}"))?;
        let start = all
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, byte)| **byte == b'\n')
            // The trailing newline ends the last line rather than starting
            // one, so it is not a boundary this may stop at.
            .skip(usize::from(all.last() == Some(&b'\n')))
            .nth(lines.saturating_sub(1))
            .map(|(at, _)| at + 1)
            .unwrap_or(0);
        self.offset = start as u64;
        Ok(())
    }

    /// Copy up to [`PUMP_CHUNK`] bytes appended since the last call to `out`,
    /// and answer how many that was.
    ///
    /// Bounded rather than "everything to EOF": attaching to a run that has
    /// been writing `stream-json` for a day would otherwise read a console of
    /// arbitrary size into one allocation before printing a byte of it. The
    /// callers all loop until a terminal condition, so a partial pump is a
    /// smaller step, never a lost one — and a nonzero return already means
    /// "come straight back", so a backlog drains at memory speed.
    fn pump(&mut self, out: &mut impl Write) -> Result<usize, String> {
        self.file
            .seek(SeekFrom::Start(self.offset))
            .map_err(|e| format!("cannot seek the console: {e}"))?;
        let mut buf = vec![0u8; PUMP_CHUNK];
        let read = self
            .file
            .read(&mut buf)
            .map_err(|e| format!("cannot read the console: {e}"))?;
        if read > 0 {
            // A closed pipe on the reading side is a routine way to stop
            // watching (`stella daemon attach … | head`), not an error worth
            // failing a run over.
            let _ = out.write_all(&buf[..read]);
            let _ = out.flush();
            self.offset += read as u64;
        }
        Ok(read)
    }
}

/// The most one [`Tail::pump`] moves. Large enough that a normal run is one
/// read, small enough that a huge console cannot be a huge allocation.
const PUMP_CHUNK: usize = 64 * 1024;

impl Tail {
    /// Copy everything appended so far, however many [`PUMP_CHUNK`]s that
    /// takes.
    ///
    /// This is what every *terminal* read must use. A single bounded `pump`
    /// where the run has just ended shows the first 64KiB of what is left and
    /// silently drops the rest — and the rest, at that moment, is the run's
    /// answer.
    fn drain(&mut self, out: &mut impl Write) -> Result<(), String> {
        while self.pump(out)? > 0 {}
        Ok(())
    }
}

/// Send `signal` to the whole process group `pgid`.
///
/// The group, not the pid: the child leads a session whose members include
/// every tool process it spawned, and a `SIGTERM` to the leader alone leaves a
/// running build attached to nothing.
fn signal_group(pgid: i32, signal: i32) {
    #[cfg(unix)]
    // SAFETY: `kill` with a negative pid targets a process group; `pgid` is
    // the child's own, recorded at spawn and confirmed live by the caller.
    unsafe {
        libc::kill(-pgid, signal);
    }
    #[cfg(not(unix))]
    {
        let _ = (pgid, signal);
    }
}

/// The exit code to forward, as the shell would report it.
fn exit_code(status: &std::process::ExitStatus) -> Option<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return u8::try_from(128 + signal).ok();
        }
    }
    match status.code() {
        Some(0) | None => None,
        Some(code) => u8::try_from(code).ok().or(Some(1)),
    }
}

/// Whether a supervised run is still going, asked of its lock rather than its
/// pid (see the module docs).
///
/// `None` means the question could not be answered — no lock file, or a
/// filesystem that does not implement `flock` (some NFS and FUSE mounts) —
/// and callers treat that as "not live" rather than guessing, because every
/// consequence of guessing wrong points one way: signalling something that is
/// not ours.
///
/// Only `EWOULDBLOCK` means held. Reporting *every* `flock` failure as held
/// would make a home directory on such a filesystem report every run as
/// running forever, and `stop` would then signal on that basis — the exact
/// inversion this function's caution is meant to prevent.
fn lock_is_held(sidecar: &Path) -> Option<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let path = sidecar.join(supervised::LOCK);
        let file = std::fs::OpenOptions::new().read(true).open(path).ok()?;
        // SAFETY: `file` owns the descriptor for the whole call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            // Nobody held it, so the run is over. Release immediately: this
            // process is a reader, not the new owner.
            // SAFETY: same descriptor, still owned by `file`.
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
            return Some(false);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EWOULDBLOCK) => Some(true),
            _ => None,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = sidecar;
        None
    }
}

/// The child's side of the liveness lock, run once at startup: claim the
/// descriptor inherited from `pre_exec` so it stops being inherited any
/// further, or take the lock outright if there is none.
///
/// # Why claiming matters more than taking
///
/// `flock` is held by an **open file description**, not by a process, and an
/// inherited descriptor is the same description. Left alone, the lock
/// `pre_exec` took is inherited by every process the run spawns — every
/// `bash` tool, every build, every dev server the agent starts in the
/// background — and stays held until the *last* of them exits.
///
/// That turns the liveness answer inside out. A run that finished an hour ago
/// still reads as running because some backgrounded `npm run dev` holds the
/// description; `stella daemon list` shows `Running` forever, and
/// `stella daemon stop` believes it and `SIGKILL`s that process group.
/// Setting `FD_CLOEXEC` here is what makes the module's claim — the lock is
/// released when the process dies — actually true of the stella process.
///
/// The taking half is the fallback for a child that did not come through
/// [`spawn`]: started by hand with [`SUPERVISED_ENV`] set, or by a future
/// launchd/systemd unit. Those start with no sidecar and no lock file at all.
pub(crate) fn hold_liveness_lock(registry: &SessionRegistry, id: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let lock_path = registry.sidecar_dir(id).join(supervised::LOCK);
        if let Some(inherited) = inherited_lock_fd(&lock_path) {
            // SAFETY: `inherited` is an open descriptor of this process,
            // identified by comparing its `fstat` identity to the lock file's.
            // `F_SETFD` alters only the descriptor's inheritance flag.
            unsafe { libc::fcntl(inherited, libc::F_SETFD, libc::FD_CLOEXEC) };
            return;
        }
        // Nothing inherited. Either this child came from somewhere else, or
        // the lock is genuinely free — but never steal one somebody holds.
        if lock_is_held(&registry.sidecar_dir(id)) == Some(true) {
            return;
        }
        let Ok(sidecar) = registry.prepare_sidecar(id) else {
            return;
        };
        let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(sidecar.join(supervised::LOCK))
        else {
            return;
        };
        // SAFETY: `file` owns the descriptor, and it is leaked below so it
        // stays open — and the lock held — until the process exits. Rust opens
        // files `O_CLOEXEC`, so this one is not inherited by tools either.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            std::mem::forget(file);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (registry, id);
    }
}

/// The descriptor this process inherited for `lock_path`, if any.
///
/// Found by identity rather than by number: `pre_exec` opens the lock after
/// the standard streams are in place, so the number it lands on is whatever
/// was free and is not ours to predict. Comparing `(st_dev, st_ino)` against
/// the lock file answers exactly the question — is one of my open descriptors
/// this file — without `/proc`, which macOS does not have.
///
/// The scan stops at [`LOCK_FD_SCAN_LIMIT`]: the descriptor is opened during
/// startup, when only the standard streams and std's own spawn plumbing are
/// open, so it is always a small number. Missing it is not a correctness
/// failure — the caller falls through to taking the lock afresh.
#[cfg(unix)]
fn inherited_lock_fd(lock_path: &Path) -> Option<i32> {
    use std::os::unix::fs::MetadataExt;

    let lock = std::fs::metadata(lock_path).ok()?;
    for fd in 3..LOCK_FD_SCAN_LIMIT {
        // The identity is read through `MetadataExt`, not a raw `libc::stat`,
        // because `dev_t` is unsigned on Linux and signed on macOS: no single
        // conversion of `st_dev` compiles on both, while `dev()`/`ino()` are
        // declared `u64` on every Unix.
        //
        // SAFETY: the descriptor is borrowed, never owned — `ManuallyDrop`
        // keeps `File`'s destructor from closing an fd this only looks at.
        // `metadata` is an `fstat`, which reports EBADF for a descriptor that
        // is not open rather than doing anything with it.
        let probe = std::mem::ManuallyDrop::new(unsafe {
            <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd)
        });
        let Ok(stat) = probe.metadata() else {
            continue;
        };
        if stat.dev() == lock.dev() && stat.ino() == lock.ino() {
            return Some(fd);
        }
    }
    None
}

/// How far [`inherited_lock_fd`] looks. Startup has the three standard
/// streams and a handful of transient descriptors; the lock lands among them.
#[cfg(unix)]
const LOCK_FD_SCAN_LIMIT: i32 = 64;

/// The child's side: stamp the terminal status on its way out.
///
/// The child does this, and not the supervisor, because the supervisor is the
/// process that is *expected* to be gone — the whole feature is about the
/// terminal closing. A record whose live status is never replaced reads as a
/// crash to every viewer, so a supervised run that completed perfectly would
/// be indistinguishable from one that died the moment its window closed.
///
/// A no-op in an unsupervised process, and harmless where a surface already
/// wrote its own answer: `SessionPresence::finish` runs first on the two paths
/// that have one, and writes the same value. This is what covers the paths
/// that do not — `stella fleet`, and any future long-running verb that is
/// handed to the supervisor before it grows a session presence.
pub(crate) fn record_outcome_if_supervised(ok: bool) {
    let Some(id) = supervised_id() else {
        return;
    };
    let _ = SessionRegistry::open_default().set_status(&id, outcome_status(ok));
}

/// The terminal status a finished run records.
///
/// A signal is not a failure: the run was stopped, and recording it as an
/// error would put a deliberate `stella daemon stop` in the registry beside
/// the runs that genuinely broke. Shared with the resume driver
/// (`crate::agent::resume`), which writes its own terminal status for the
/// hand-run `--foreground` case no supervised env var covers.
pub(crate) fn outcome_status(ok: bool) -> SessionStatus {
    match (ok, crate::signals::interrupted_exit_code()) {
        (_, Some(_)) => SessionStatus::Cancelled,
        (true, None) => SessionStatus::Complete,
        (false, None) => SessionStatus::Error,
    }
}

/// `stella daemon resume` — the parent half (#1586).
///
/// Verifies there is genuinely something to resume — the run is over, and it
/// left a resume point this build can read — then relaunches the SAME
/// session as a fresh supervised child running `daemon resume <id>
/// --foreground`, and stays to stream it exactly as the original launch
/// would have. The child continues the interrupted turn from its last
/// committed step boundary (`crate::agent::resume`); completed steps are
/// already in the checkpointed transcript, so nothing re-runs and nothing is
/// double-applied.
pub(crate) fn resume_supervised(
    rt: tokio::runtime::Runtime,
    id: Option<&str>,
) -> Result<(), String> {
    let registry = SessionRegistry::open_default();
    let record = resolve(&registry, id)?;
    let sidecar = registry.sidecar_dir(&record.id);
    if lock_is_held(&sidecar) == Some(true) {
        return Err(format!(
            "{} is still running — `stella daemon attach {}` streams it, `stella daemon \
             stop {}` ends it",
            record.id, record.id, record.id
        ));
    }
    let workspace = PathBuf::from(&record.workspace);
    if !workspace.is_dir() {
        return Err(format!(
            "{}'s workspace {} no longer exists, and a resumed turn must run where its \
             work is",
            record.id, record.workspace
        ));
    }
    let checkpoint = resume_point(&record)?;
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate the stella binary to resume with: {e}"))?;
    let args: Vec<std::ffi::OsString> = vec![
        "daemon".into(),
        "resume".into(),
        record.id.clone().into(),
        "--foreground".into(),
    ];
    let id = record.id.clone();
    let run = launch(
        &registry,
        record,
        &exe,
        &args,
        b"",
        Console::Preserve,
        Some(&workspace),
    )?;
    eprintln!(
        "{} {} — continuing its interrupted turn at step {}",
        "▸ resuming".green().bold(),
        id.dimmed(),
        checkpoint.step
    );
    watch(rt, &registry, run)
}

/// `stella daemon resume-all` — the boot-time sweep (#1627).
///
/// Thin on purpose: the decision about *which* runs a boot continues, and the
/// bound that stops it continuing them forever, live in [`boot`] where they
/// are pure enough to witness.
pub(crate) fn resume_all<F>(dry_run: bool, runtime: F) -> Result<(), String>
where
    F: FnMut() -> Result<tokio::runtime::Runtime, String>,
{
    boot::resume_all(dry_run, runtime)
}

/// The resume point `record` left behind, decoded — or the precise reason
/// there is nothing to resume.
///
/// Decoded HERE, in the parent, so an absent or version-skewed checkpoint
/// refuses before a child is spawned: the child would only discover the same
/// thing after the operator has been told the run was resuming.
fn resume_point(record: &SessionRecord) -> Result<stella_core::step::Checkpoint, String> {
    let journal =
        stella_store::work_journal::WorkJournal::open(Path::new(&record.workspace), &record.id)
            .map_err(|e| format!("cannot open {}'s durable record: {e}", record.id))?;
    let Some(json) = journal.checkpoint() else {
        return Err(format!(
            "{} has no resume point. A turn leaves one only when its process was killed \
             mid-turn; a run that completed, was stopped, or aborted discards it on the \
             way out",
            record.id
        ));
    };
    stella_core::step::Checkpoint::from_json(&json).map_err(|e| {
        format!(
            "{}'s resume point cannot be read by this build ({e}); refusing to resume a \
             turn from a shape this build half-recognizes",
            record.id
        )
    })
}

/// Stop a supervised run from another process — `stella daemon stop`.
pub(crate) fn stop(registry: &SessionRegistry, id: &str) -> Result<(), String> {
    let record = resolve(registry, Some(id))?;
    let Some(supervisor) = record.supervisor.as_ref() else {
        return Err(format!(
            "{} is not a supervised run — there is no separate process to stop",
            record.id
        ));
    };
    let sidecar = registry.sidecar_dir(&record.id);

    if lock_is_held(&sidecar) != Some(true) {
        // Already over. Still record the transition: a run whose terminal
        // status was never written reads as a crash, and "the operator
        // stopped it" is the one thing we know for certain here.
        mark_stopped(registry, &record.id);
        println!("{} {} was already finished", "▸".dimmed(), record.id);
        return Ok(());
    }

    signal_group(supervisor.pgid, SIGTERM);
    let deadline = Instant::now() + STOP_GRACE;
    while Instant::now() < deadline {
        if lock_is_held(&sidecar) != Some(true) {
            mark_stopped(registry, &record.id);
            println!("{} stopped {}", "✓".green(), record.id);
            return Ok(());
        }
        std::thread::sleep(POLL);
    }

    eprintln!(
        "{} {} did not stop within {}s; killing it",
        "⚠".yellow(),
        record.id,
        STOP_GRACE.as_secs()
    );
    signal_group(supervisor.pgid, SIGKILL);
    // Written here as well as on the graceful path, because on this one the
    // child's own shutdown never ran and nothing else will write it.
    mark_stopped(registry, &record.id);
    println!("{} killed {}", "✓".green(), record.id);
    Ok(())
}

/// Record a stop as deliberate.
///
/// Only where the run never recorded an answer of its own: one that completed
/// a second before the stop landed must keep its own, rather than be
/// relabelled by the process that arrived too late to change it.
///
/// Read through [`SessionRegistry::get`], never from a record that came out of
/// `list`. `list` presents a live status whose pid is gone as `Error`, so a
/// guard reading that value sees "it already has an answer" for the one case
/// that most needs writing — a supervised run whose child is over but whose
/// status was never replaced. The stored status is the only one that says
/// whether anybody wrote anything.
fn mark_stopped(registry: &SessionRegistry, id: &str) {
    let stored_status = registry.get(id).map(|record| record.status);
    if stored_status.is_some_and(|status| status.is_live()) {
        let _ = registry.set_status(id, SessionStatus::Cancelled);
    }
}

/// Resolve a user-typed id to a record.
///
/// Accepts a unique prefix, because the full form (`ses-1754431200000-84213`)
/// is a timestamp and a pid and nobody is going to type it. It also accepts a
/// bare **pid**, because that is the address `ps`, `top`, Activity Monitor and
/// an OOM-killer log hand you, and without this rung translating one back to a
/// run means eyeballing `stella daemon list` (#1690). `None` picks the most
/// recent supervised run, which is what "the one I just started" means nine
/// times in ten.
pub(crate) fn resolve(
    registry: &SessionRegistry,
    id: Option<&str>,
) -> Result<SessionRecord, String> {
    let supervised_runs: Vec<SessionRecord> = registry
        .list()
        .into_iter()
        .filter(|r| r.supervisor.is_some())
        .collect();
    let Some(id) = id else {
        return supervised_runs
            .into_iter()
            .next()
            .ok_or_else(|| "no supervised runs on this machine".to_string());
    };

    // An exact hit is never ambiguous, however many ids extend it — and it is
    // tested for across the whole set rather than on whichever candidate came
    // out first, because the list is ordered by start time and has no reason
    // to put the exact match anywhere in particular.
    if let Some(exact) = supervised_runs.iter().find(|r| r.id == id) {
        return Ok(exact.clone());
    }

    // An id begins `ses-`, so an argument that parses as a number cannot be a
    // prefix of one: the two address spaces are disjoint, and a pid that hits
    // nothing is reported as a pid rather than falling through to a prefix
    // scan that could not have matched either. The ambiguity rule is the same
    // one — pids are recycled, and a finished run keeps the one it ran under —
    // but the advice is not, since there are no further digits to type.
    if let Ok(pid) = id.parse::<u32>() {
        return only(
            supervised_runs.into_iter().filter(|r| r.pid == pid),
            || {
                format!(
                    "no supervised run has pid {pid} — `stella daemon list` shows what there is"
                )
            },
            |first, second| {
                format!(
                    "pid {pid} matches more than one run ({} and {}) — use the id `stella daemon list` prints",
                    first.id, second.id
                )
            },
        );
    }

    only(
        supervised_runs.into_iter().filter(|r| r.id.starts_with(id)),
        || format!("no supervised run matches `{id}` — `stella daemon list` shows what there is"),
        |first, second| {
            format!(
                "`{id}` matches more than one run ({} and {}) — use more of the id",
                first.id, second.id
            )
        },
    )
}

/// The one record in `matches`, or the error for none and for several.
///
/// Shared by [`resolve`]'s prefix and pid rungs: the rule ("exactly one, or
/// say so") is the same on both, while the sentences are not — what to type
/// instead depends on which address space the caller was using.
fn only(
    mut matches: impl Iterator<Item = SessionRecord>,
    miss: impl FnOnce() -> String,
    ambiguous: impl FnOnce(&SessionRecord, &SessionRecord) -> String,
) -> Result<SessionRecord, String> {
    let Some(first) = matches.next() else {
        return Err(miss());
    };
    match matches.next() {
        None => Ok(first),
        Some(second) => Err(ambiguous(&first, &second)),
    }
}

/// `stella daemon <cmd>`.
pub(crate) fn run(cmd: &DaemonCmd) -> Result<(), String> {
    let registry = SessionRegistry::open_default();
    match cmd {
        DaemonCmd::List => list(&registry),
        DaemonCmd::Attach { id } => attach(&registry, id.as_deref()),
        DaemonCmd::Logs { id, lines } => logs(&registry, id.as_deref(), *lines),
        DaemonCmd::Stop { id } => stop(&registry, id),
        // Routed in `main` before this is reached: the parent half spawns
        // from the registry alone ([`resume_supervised`]), while the
        // `--foreground` child half needs the provider resolution this
        // registry-only dispatcher exists to run before.
        DaemonCmd::Resume { .. } => unreachable!("daemon resume is dispatched in main"),
        // Same routing, same reason: the sweep spawns and streams the runs it
        // continues, so it needs the runtime `main` owns (#1627).
        DaemonCmd::ResumeAll { .. } => unreachable!("daemon resume-all is dispatched in main"),
        DaemonCmd::Install {
            label,
            keep_alive,
            resume_all,
            command,
        } => service::install(label.as_deref(), *keep_alive, *resume_all, command),
        DaemonCmd::Uninstall { label } => service::uninstall(label),
    }
}

fn list(registry: &SessionRegistry) -> Result<(), String> {
    let runs: Vec<SessionRecord> = registry
        .list()
        .into_iter()
        .filter(|r| r.supervisor.is_some())
        .collect();
    if runs.is_empty() {
        println!("No supervised runs. Every `stella run` in a terminal starts one.");
        return Ok(());
    }
    println!(
        "{:<28} {:<12} {}",
        "ID".bold(),
        "STATUS".bold(),
        "WHAT".bold()
    );
    let mut any_resumable = false;
    for run in runs {
        let live = lock_is_held(&registry.sidecar_dir(&run.id)) == Some(true);
        // The lock outranks the stored status for a live-looking record: the
        // status is what the child last wrote, and a child killed between two
        // writes never got to correct it.
        let (label, paint): (&str, fn(&str) -> ColoredString) = match (live, run.status) {
            (true, status) if status.is_live() => (status.label(), |s| s.green()),
            (true, _) => ("Running", |s| s.green()),
            // Probed only on this rare arm: a crashed run holding a resume
            // point is exactly the row `daemon resume` exists for (#1586),
            // and a killed run's journal already exists, so the probe opens
            // rather than creates.
            (false, status) if status.is_live() => {
                if has_resume_point(&run) {
                    any_resumable = true;
                    ("Crashed ↩", |s| s.red())
                } else {
                    ("Crashed", |s| s.red())
                }
            }
            (false, status) => (status.label(), |s| s.normal()),
        };
        // A crashed run earns the resume hint only if it actually left a
        // resume point behind; a clean exit discarded its own on the way out.
        if !live && run.status.is_live() && !any_resumable {
            any_resumable = has_resume_point(&run);
        }
        // Padded before it is painted. A width applied to a `ColoredString`
        // counts the ANSI escapes as characters, so every coloured cell comes
        // out its escape-length too narrow and the last column ragged.
        println!(
            "{:<28} {} {}",
            run.id,
            paint(&format!("{label:<12}")),
            run.title
        );
    }
    if any_resumable {
        println!(
            "\n↩ killed mid-turn with a resume point — {} continues it",
            "stella daemon resume <id>".cyan()
        );
    }
    Ok(())
}

/// Whether `record` left a resume point — the cheap presence probe behind
/// `list`'s `↩`, deliberately not decoding what [`resume_point`] will.
fn has_resume_point(record: &SessionRecord) -> bool {
    stella_store::work_journal::WorkJournal::open(Path::new(&record.workspace), &record.id)
        .ok()
        .and_then(|journal| journal.checkpoint())
        .is_some()
}

fn attach(registry: &SessionRegistry, id: Option<&str>) -> Result<(), String> {
    use std::io::IsTerminal;
    let record = resolve(registry, id)?;
    let sidecar = registry.sidecar_dir(&record.id);
    let mut console = console::Follower::open(&sidecar)?;
    let interactive = std::io::stdin().is_terminal();
    let mut approval_noted = false;
    eprintln!(
        "{} {} — {}",
        "▸ attached".green().bold(),
        record.id.dimmed(),
        record.title
    );
    loop {
        let moved = console.pump(&mut std::io::stdout(), &mut std::io::stderr())?;
        // Attaching to a parked run is the issue's headline path (#1585):
        // `daemon list` says Needs Input, attach asks the question.
        approval::forward_pending_approval(&sidecar, interactive, &mut approval_noted);
        if lock_is_held(&sidecar) != Some(true) {
            // Same ordering as `follow`: drain after observing the end, so the
            // last thing the run wrote is never the thing attach misses. The
            // pump is bounded, so draining is pumping until nothing moves.
            while console.pump(&mut std::io::stdout(), &mut std::io::stderr())? > 0 {}
            eprintln!("{} {} has finished", "▸".dimmed(), record.id.dimmed());
            return Ok(());
        }
        if moved == 0 {
            std::thread::sleep(POLL);
        }
    }
}

fn logs(registry: &SessionRegistry, id: Option<&str>, lines: usize) -> Result<(), String> {
    let record = resolve(registry, id)?;
    let sidecar = registry.sidecar_dir(&record.id);
    // An indexed console replays in true cross-stream order (#1588). A
    // session recorded before the index existed cannot be ordered — the raw
    // files carry no timestamps — so it keeps the old two-block tail, which
    // stays the honest answer for exactly those sessions.
    if console::replay_logs(
        &sidecar,
        lines,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )? {
        return Ok(());
    }
    let mut out = Tail::open(&sidecar.join(supervised::STDOUT_LOG))?;
    let mut err = Tail::open(&sidecar.join(supervised::STDERR_LOG))?;
    out.seek_back_lines(lines)?;
    err.seek_back_lines(lines)?;
    out.drain(&mut std::io::stdout())?;
    err.drain(&mut std::io::stderr())?;
    Ok(())
}

#[cfg(test)]
mod tests;
