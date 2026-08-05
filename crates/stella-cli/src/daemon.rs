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
//! It is **not** engine-level checkpoint/resume. A supervised run survives a
//! closed terminal, a logout, and an `ssh` disconnect. It does not survive its
//! own process being killed: that loses the turn, and only the fact of it is
//! recorded. `stella-engine` exists for the stronger property (drive the step
//! loop from a durable host, checkpoint between steps) and is tracked
//! separately — the weaker property is worth shipping first because it is what
//! a closed laptop lid actually costs today.
//!
//! # Why supervision keys on a controlling terminal
//!
//! [`should_supervise`] answers yes only when this process has a controlling
//! terminal. The reasoning is symmetrical: a terminal is exactly what can
//! close, so a process without one is already immune and supervising it buys
//! nothing while adding a process, two files, and a copy to every byte of
//! output. Concretely that leaves every non-interactive caller — CI, a pipe,
//! `nohup`, and the Terminal-Bench harness, which runs `stella run` on pipes
//! inside a container — on byte-for-byte the process shape they run today.
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
//! means the run is over, and [`stop`] refuses to signal anything once it can
//! take that lock itself.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use colored::Colorize;
use stella_store::{SessionRecord, SessionRegistry, SessionStatus, SupervisorInfo, supervised};

use crate::DaemonCmd;

pub(crate) mod approval;

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

/// A live supervised child, owned by the parent that spawned it.
pub(crate) struct Supervised {
    /// The registry id — what `stella daemon attach` takes.
    pub(crate) id: String,
    /// The child's session sidecar, holding both console files.
    sidecar: PathBuf,
    /// The child's process group, which is also its pid ([`spawn`]).
    pgid: i32,
    child: std::process::Child,
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

/// This process's supervised session id, when it is itself a supervised child.
pub(crate) fn supervised_id() -> Option<String> {
    std::env::var(SUPERVISED_ENV)
        .ok()
        .filter(|id| !id.trim().is_empty())
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
pub(crate) fn supervise_this_invocation(
    rt: tokio::runtime::Runtime,
    workspace: &Path,
    title: &str,
    stdin: &[u8],
) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate the stella binary to supervise: {e}"))?;
    // This process's argv verbatim, plus the one flag that makes the child do
    // the work rather than supervise it again. Verbatim because every
    // alternative re-derives arguments that clap already parsed, and a
    // reconstruction is one forgotten flag away from running something the
    // user did not type. `SUPERVISED_ENV` is the backstop if it is ever lost.
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    args.push("--foreground".to_string());

    let registry = SessionRegistry::open_default();
    let mut run = spawn(
        &registry,
        &workspace.display().to_string(),
        title,
        &exe,
        &args,
        stdin,
    )?;
    run.announce();

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
            let drained = rt.block_on(run.interrupt_and_drain());
            mark_stopped(&registry, &run.id);
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
    args: &[String],
    stdin: &[u8],
) -> Result<Supervised, String> {
    // Minted before the spawn so the child can be told its own id, and so a
    // failed spawn leaves a directory rather than a half-registered session.
    let record = SessionRecord::new(workspace, title);
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
    let out_file = create_console(&out_path)?;
    let err_file = create_console(&err_path)?;

    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .env(SUPERVISED_ENV, &record.id)
        .stdin(std::process::Stdio::from(stdin_file))
        .stdout(std::process::Stdio::from(out_file))
        .stderr(std::process::Stdio::from(err_file));
    #[cfg(unix)]
    detach_before_exec(&mut command, sidecar.join(supervised::LOCK));

    let child = command
        .spawn()
        .map_err(|e| format!("cannot start the supervised run: {e}"))?;

    // The CHILD's pid, deliberately: the supervisor's says a supervisor
    // exists, this says the work is running. Every liveness check in the
    // registry, and every signal `stop` sends, reads it.
    let pid = child.id();
    let pgid = i32::try_from(pid)
        .map_err(|_| format!("supervised child pid {pid} does not fit a process group"))?;
    let mut record = SessionRecord {
        pid,
        supervisor: Some(SupervisorInfo { pgid }),
        ..record
    };
    record.summary = title.to_string();
    registry
        .upsert(&record)
        .map_err(|e| format!("cannot register the supervised run: {e}"))?;

    Ok(Supervised {
        id: record.id,
        sidecar,
        pgid,
        child,
    })
}

/// Create a console file that is readable only by its owner.
///
/// A run's stdout carries whatever the model said and whatever the tools
/// printed. `ensure_private_dir` already restricts the directory; this keeps
/// the file itself from being the exception if the directory's mode is ever
/// widened by hand.
fn create_console(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
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
/// Both calls are async-signal-safe, which is the bar for anything running in
/// this window. Either failing fails the spawn — a child that half-detached is
/// worse than one that never started, because it looks supervised in the
/// registry while still dying with the terminal.
#[cfg(unix)]
fn detach_before_exec(command: &mut std::process::Command, lock_path: PathBuf) {
    use std::os::unix::process::CommandExt;

    let lock = std::ffi::CString::new(lock_path.into_os_string().into_encoded_bytes())
        .unwrap_or_else(|_| c"".to_owned());
    // SAFETY: `setsid`, `open` and `flock` are async-signal-safe, and this
    // closure allocates nothing (the CString is built above, in the parent).
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if lock.as_bytes().is_empty() {
                return Err(std::io::Error::other(
                    "supervisor lock path is not a C string",
                ));
            }
            // Deliberately NOT `O_CLOEXEC`, and deliberately never closed: the
            // lock has to outlive both this function and the `exec` that
            // follows it. The kernel is what closes this descriptor, when the
            // process dies, which is precisely the event readers are asking
            // about.
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
        use std::io::IsTerminal;
        let mut out = Tail::open(&self.sidecar.join(supervised::STDOUT_LOG))?;
        let mut err = Tail::open(&self.sidecar.join(supervised::STDERR_LOG))?;
        let interactive = std::io::stdin().is_terminal();
        let mut approval_noted = false;
        loop {
            let moved = out.pump(&mut std::io::stdout())? + err.pump(&mut std::io::stderr())?;
            // The launching terminal is the first surface a parked scope
            // review reaches (#1585) — this is the exact capability the
            // supervisor used to take away.
            approval::forward_pending_approval(&self.sidecar, interactive, &mut approval_noted);
            match self
                .child
                .try_wait()
                .map_err(|e| format!("cannot wait on the supervised run: {e}"))?
            {
                Some(status) => {
                    // Ordered after the wait on purpose: this drain catches
                    // whatever the child wrote between the pump above and its
                    // exit, and once it has exited nothing more can appear.
                    out.pump(&mut std::io::stdout())?;
                    err.pump(&mut std::io::stderr())?;
                    return Ok(exit_code(&status));
                }
                None if moved == 0 => tokio::time::sleep(POLL).await,
                None => {}
            }
        }
    }

    /// Ask the child to stop the way `SIGTERM` from anywhere else would, then
    /// keep streaming until it is gone.
    ///
    /// Used when the *parent* is interrupted: Ctrl-C in a terminal reaches
    /// only the foreground group, which the child left, so without this a
    /// Ctrl-C would detach the run rather than stop it — the opposite of what
    /// the key means everywhere else.
    pub(crate) async fn interrupt_and_drain(&mut self) -> Result<(), String> {
        signal_group(self.pgid, libc::SIGTERM);
        let deadline = Instant::now() + STOP_GRACE;
        let mut out = Tail::open(&self.sidecar.join(supervised::STDOUT_LOG))?;
        let mut err = Tail::open(&self.sidecar.join(supervised::STDERR_LOG))?;
        out.seek_to_end()?;
        err.seek_to_end()?;
        while Instant::now() < deadline {
            out.pump(&mut std::io::stdout())?;
            err.pump(&mut std::io::stderr())?;
            if matches!(self.child.try_wait(), Ok(Some(_))) {
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
        signal_group(self.pgid, libc::SIGKILL);
        let _ = self.child.wait();
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

    /// Skip whatever is already there — for a reader that only wants what
    /// happens from now on.
    fn seek_to_end(&mut self) -> Result<(), String> {
        self.offset = self
            .file
            .seek(SeekFrom::End(0))
            .map_err(|e| format!("cannot seek the console: {e}"))?;
        Ok(())
    }

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

    /// Copy everything appended since the last call to `out`, and answer how
    /// many bytes that was.
    fn pump(&mut self, out: &mut impl Write) -> Result<usize, String> {
        self.file
            .seek(SeekFrom::Start(self.offset))
            .map_err(|e| format!("cannot seek the console: {e}"))?;
        let mut buf = Vec::new();
        let read = self
            .file
            .read_to_end(&mut buf)
            .map_err(|e| format!("cannot read the console: {e}"))?;
        if read > 0 {
            // A closed pipe on the reading side is a routine way to stop
            // watching (`stella daemon attach … | head`), not an error worth
            // failing a run over.
            let _ = out.write_all(&buf);
            let _ = out.flush();
            self.offset += read as u64;
        }
        Ok(read)
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
/// directory we cannot open — and callers treat that as "not live" rather than
/// guessing, because every consequence of guessing wrong points one way:
/// signalling something that is not ours.
fn lock_is_held(sidecar: &Path) -> Option<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let path = sidecar.join(supervised::LOCK);
        let file = std::fs::OpenOptions::new().read(true).open(path).ok()?;
        // SAFETY: `file` owns the descriptor for the whole call.
        let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if taken == 0 {
            // Nobody held it, so the run is over. Release immediately: this
            // process is a reader, not the new owner.
            // SAFETY: same descriptor, still owned by `file`.
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
            Some(false)
        } else {
            Some(true)
        }
    }
    #[cfg(not(unix))]
    {
        let _ = sidecar;
        None
    }
}

/// The child's side: hold the liveness lock for the rest of this process.
///
/// A no-op in the normal case — the lock was taken in `pre_exec` and the
/// descriptor is still open — and exists for the case where it was not: a
/// child started by hand with [`SUPERVISED_ENV`] set, or a future launcher
/// (launchd, systemd) that spawns the child without going through [`spawn`].
/// Without it those runs would read as already-finished to every other
/// process the moment they started.
pub(crate) fn hold_liveness_lock(registry: &SessionRegistry, id: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // Only a lock somebody already holds is a reason to stop. `None` — no
        // lock file, no sidecar — is not that: it is precisely the state a
        // child spawned outside `spawn` starts in, and returning on it would
        // make this whole function a no-op in the one case it exists for.
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
        // stays open — and the lock held — until the process exits.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            std::mem::forget(file);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (registry, id);
    }
}

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
    let status = match (ok, crate::signals::interrupted_exit_code()) {
        // A signal is not a failure: the run was stopped, and recording it as
        // an error would put a deliberate `stella daemon stop` in the registry
        // beside the runs that genuinely broke.
        (_, Some(_)) => SessionStatus::Cancelled,
        (true, None) => SessionStatus::Complete,
        (false, None) => SessionStatus::Error,
    };
    let _ = SessionRegistry::open_default().set_status(&id, status);
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

    signal_group(supervisor.pgid, libc::SIGTERM);
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
    signal_group(supervisor.pgid, libc::SIGKILL);
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
/// is a timestamp and a pid and nobody is going to type it. `None` picks the
/// most recent supervised run, which is what "the one I just started" means
/// nine times in ten.
fn resolve(registry: &SessionRegistry, id: Option<&str>) -> Result<SessionRecord, String> {
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

    let mut matches = supervised_runs.into_iter().filter(|r| r.id.starts_with(id));
    let Some(first) = matches.next() else {
        return Err(format!(
            "no supervised run matches `{id}` — `stella daemon list` shows what there is"
        ));
    };
    match matches.next() {
        None => Ok(first),
        Some(second) => Err(format!(
            "`{id}` matches more than one run ({} and {}) — use more of the id",
            first.id, second.id
        )),
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
    for run in runs {
        let live = lock_is_held(&registry.sidecar_dir(&run.id)) == Some(true);
        // The lock outranks the stored status for a live-looking record: the
        // status is what the child last wrote, and a child killed between two
        // writes never got to correct it.
        let status = match (live, run.status) {
            // Yellow, not green: a parked run is waiting on the human reading
            // this table (`stella daemon attach` answers it — #1585).
            (true, SessionStatus::NeedsInput) => SessionStatus::NeedsInput.label().yellow(),
            (true, status) if status.is_live() => status.label().green(),
            (true, _) => "Running".green(),
            (false, status) if status.is_live() => "Crashed".red(),
            (false, status) => status.label().normal(),
        };
        println!("{:<28} {:<12} {}", run.id, status, run.title);
    }
    Ok(())
}

fn attach(registry: &SessionRegistry, id: Option<&str>) -> Result<(), String> {
    use std::io::IsTerminal;
    let record = resolve(registry, id)?;
    let sidecar = registry.sidecar_dir(&record.id);
    let mut out = Tail::open(&sidecar.join(supervised::STDOUT_LOG))?;
    let mut err = Tail::open(&sidecar.join(supervised::STDERR_LOG))?;
    let interactive = std::io::stdin().is_terminal();
    let mut approval_noted = false;
    eprintln!(
        "{} {} — {}",
        "▸ attached".green().bold(),
        record.id.dimmed(),
        record.title
    );
    loop {
        let moved = out.pump(&mut std::io::stdout())? + err.pump(&mut std::io::stderr())?;
        // Attaching to a parked run is the issue's headline path (#1585):
        // `daemon list` says Needs Input, attach asks the question.
        approval::forward_pending_approval(&sidecar, interactive, &mut approval_noted);
        if lock_is_held(&sidecar) != Some(true) {
            // Same ordering as `follow`: drain after observing the end, so the
            // last thing the run wrote is never the thing attach misses.
            out.pump(&mut std::io::stdout())?;
            err.pump(&mut std::io::stderr())?;
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
    let mut out = Tail::open(&sidecar.join(supervised::STDOUT_LOG))?;
    let mut err = Tail::open(&sidecar.join(supervised::STDERR_LOG))?;
    out.seek_back_lines(lines)?;
    err.seek_back_lines(lines)?;
    out.pump(&mut std::io::stdout())?;
    err.pump(&mut std::io::stderr())?;
    Ok(())
}

#[cfg(test)]
mod tests;
