//! Shutdown-signal handling for the CLI's top-level futures.
//!
//! Nothing in this workspace installed a SIGINT or SIGTERM handler, so a
//! headless Ctrl-C killed the process outright: no unwinding, no `Drop`, and
//! therefore none of the RAII guards that exist precisely to clean up after
//! an interrupted turn.
//!
//! That mattered most for child processes. Every long-running child is
//! spawned with `setsid()`, which puts it in its own session — and the tty
//! delivers Ctrl-C's SIGINT only to the *foreground* process group. So the
//! child never saw the signal either. `GroupKillGuard`
//! (`stella-tools/src/exec.rs`) is the only thing that reaps that tree, and
//! it could not fire. A cancelled build or test suite kept running,
//! unattached, still mutating the workspace.
//!
//! # Why a `select!` and not a signal handler
//!
//! `Drop` cannot await, and `tokio::spawn` from inside a `Drop` running
//! during runtime shutdown silently does nothing — exactly the case a
//! teardown handler would be trying to serve. Every guard in this workspace
//! is already synchronous (`GroupKillGuard` is one `libc::kill`,
//! `ClaimGuard` is one DELETE), so none of them need to await; they only
//! need the future that owns them to be **dropped while the runtime is still
//! alive**.
//!
//! So the signal races the work future, and losing that race drops the work
//! — on the thread that owns the `block_on`, inside a live runtime. That
//! drop *is* the teardown.
//!
//! # The contract (#613)
//!
//! What a SIGINT or SIGTERM now **guarantees**:
//!
//! - every RAII guard on the interrupted work future's stack runs, inside a
//!   live runtime — that is what reaps `setsid` child process groups
//!   (`GroupKillGuard`), releases fleet claim locks (`ClaimGuard`), removes a
//!   `verify_done` shadow worktree (`ShadowDirGuard`), and stops a detached
//!   context warm;
//! - the process exits `128 + signum` (130 for SIGINT, 143 for SIGTERM), so a
//!   script wrapping `stella run` can tell "the user stopped this" from "this
//!   failed";
//! - shutdown is *bounded*: the runtime gets two seconds to retire blocking
//!   tasks and then goes anyway, so one wedged `spawn_blocking` cannot make
//!   Ctrl-C look ignored;
//! - a **second** signal is a hard kill. The default disposition is restored
//!   the moment the first one is caught, so a guard that wedges can never trap
//!   the user in an uninterruptible process.
//!
//! What it does **not** guarantee:
//!
//! - nothing awaits during teardown. A cleanup that genuinely needs an `await`
//!   — `verify_done`'s `git worktree` unregistration, `run_best_of_n`'s
//!   candidate-workspace removal — does not run, by construction; those
//!   resources are reclaimed by a prune on the next start instead;
//! - it only covers work driven through [`block_on_interruptible`]. Short
//!   local commands that build their own runtime (`stella models refresh`)
//!   create no reapable resources and are deliberately not wrapped;
//! - it is not a `kill -9` substitute for the child of a child that
//!   `setsid`s itself out of the group Stella knows about.

use std::future::Future;

/// Why a top-level future stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Interrupt {
    Int,
    Term,
}

impl Interrupt {
    /// Shell convention: 128 + the signal number, so `stella run …; echo $?`
    /// reads the same as any other Unix tool a script might wrap.
    pub(crate) fn exit_code(self) -> u8 {
        match self {
            Interrupt::Int => 130,
            Interrupt::Term => 143,
        }
    }

    pub(crate) fn reason(self) -> &'static str {
        match self {
            Interrupt::Int => "interrupted",
            Interrupt::Term => "terminated",
        }
    }
}

/// Resolve on the first shutdown signal.
#[cfg(unix)]
async fn first_signal() -> Interrupt {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        // If SIGTERM cannot be registered, still honour Ctrl-C rather than
        // giving up on both.
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
            return Interrupt::Int;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => Interrupt::Int,
        _ = terminate.recv() => Interrupt::Term,
    }
}

#[cfg(not(unix))]
async fn first_signal() -> Interrupt {
    let _ = tokio::signal::ctrl_c().await;
    Interrupt::Int
}

/// Run `work` until it finishes, or until a shutdown signal arrives.
///
/// On a signal the work future is dropped here, which runs every RAII guard
/// on its stack. Nothing waits on those guards: none of them await, and one
/// that needed to could not have run from `Drop` at all.
pub(crate) async fn until_interrupted<F: Future>(work: F) -> Result<F::Output, Interrupt> {
    race(work, first_signal()).await
}

/// The coordinator itself, over an arbitrary `signal` future.
///
/// Split out of [`until_interrupted`] purely so the state transition is
/// testable: [`first_signal`] can only be driven by raising a real signal at
/// the *process*, which in a test harness would take down every other test in
/// the binary. Production has exactly one caller and it passes `first_signal`.
async fn race<F: Future>(
    work: F,
    signal: impl Future<Output = Interrupt>,
) -> Result<F::Output, Interrupt> {
    let mut work = Box::pin(work);
    tokio::select! {
        // A work future that has already completed wins the race — a signal
        // arriving in the same tick must not discard a finished turn.
        biased;
        output = &mut work => Ok(output),
        signal = signal => {
            // Restore the default disposition before running teardown, so a
            // second Ctrl-C is a hard kill. A guard that wedges must never
            // be able to trap the user in an uninterruptible process.
            #[cfg(unix)]
            unsafe {
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::signal(libc::SIGTERM, libc::SIG_DFL);
            }
            // This line is the teardown. Explicit rather than an implicit
            // scope exit, because it is the whole point of the module.
            drop(work);
            Err(signal)
        }
    }
}

/// Set when a top-level future was cut short, so `main` can return the
/// shell-conventional exit code rather than a generic failure. A static
/// because `run` threads a plain `Result<(), String>` and widening that
/// error type would touch every `?` in it.
static INTERRUPTED: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Record that a signal cut this process short.
///
/// [`block_on_interruptible`] is the usual caller. The supervisor
/// ([`crate::daemon`]) is the other: it cannot use that wrapper, because a
/// signal there must stop the *child* and keep streaming rather than drop the
/// work — but the exit code it owes the shell is the same one.
pub(crate) fn note_interrupt(signal: Interrupt) {
    INTERRUPTED.store(signal.exit_code(), std::sync::atomic::Ordering::SeqCst);
}

/// The exit code to use, if a signal cut this process short.
pub(crate) fn interrupted_exit_code() -> Option<u8> {
    match INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst) {
        0 => None,
        code => Some(code),
    }
}

/// Drive `work` to completion on `rt`, honouring shutdown signals.
///
/// Takes the runtime by value so an interrupt can bound its shutdown:
/// dropping a multi-thread runtime blocks until every `spawn_blocking` task
/// finishes, and one wedged indexing or embedding call would make Ctrl-C
/// look ignored *after* the guards had already fired.
pub(crate) fn block_on_interruptible<F>(rt: tokio::runtime::Runtime, work: F) -> Result<(), String>
where
    F: Future<Output = Result<(), String>>,
{
    match rt.block_on(until_interrupted(work)) {
        Ok(inner) => {
            // The same bound on the success path. Everything the run awaited
            // has settled by here; what can still be running is *detached*
            // blocking work — the background code-graph indexer over a large
            // tree is the recorded case — and letting the runtime's implicit
            // drop wait for it held a finished benchmark trial hostage until
            // Harbor's wall clock expired: reward 1.0, terminal `complete`
            // emitted, and the process still alive at 300.06s of a 300s
            // budget (#960). A completed run owes stray blocking tasks the
            // same two seconds an interrupted one gets, and no more.
            rt.shutdown_timeout(std::time::Duration::from_secs(2));
            inner
        }
        Err(signal) => {
            note_interrupt(signal);
            rt.shutdown_timeout(std::time::Duration::from_secs(2));
            Err(signal.reason().to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_future_that_completes_is_not_disturbed() {
        let out = until_interrupted(async { 7u32 }).await;
        assert_eq!(out, Ok(7));
    }

    /// The success-path half of the shutdown bound (#960): a run that
    /// FINISHED must not be held hostage by a detached `spawn_blocking` task
    /// it never awaited — the shape of the background code-graph indexer
    /// over a large tree. Before the bound, the runtime's implicit drop
    /// waited for the blocking task without limit, and a benchmark trial
    /// with reward 1.0 sat alive until Harbor's wall clock expired.
    ///
    /// The wedged task sleeps 60s; the assertion window is 20s. On a
    /// regression this fails in 20s rather than hanging the suite.
    #[test]
    fn a_completed_run_is_not_held_hostage_by_a_wedged_blocking_task() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = block_on_interruptible(rt, async {
                let started = Arc::new(AtomicBool::new(false));
                let flag = started.clone();
                // Detached on purpose — the handle is dropped, exactly like
                // the fire-and-forget indexing task a one-shot run leaves
                // behind when the work finishes first.
                drop(tokio::task::spawn_blocking(move || {
                    flag.store(true, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_secs(60));
                }));
                // The wedge must actually be RUNNING when the work
                // completes: a merely queued blocking task is discarded by
                // shutdown without ever starting, which would prove nothing.
                while !started.load(Ordering::SeqCst) {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                Ok(())
            });
            let _ = done_tx.send(result);
        });

        let outcome = done_rx.recv_timeout(std::time::Duration::from_secs(20));
        assert_eq!(
            outcome.expect(
                "block_on_interruptible must return once the work future completes, \
                 bounded by the shutdown grace — not wait out a detached blocking task (#960)"
            ),
            Ok(())
        );
    }

    /// A guard that records its own drop — the synchronous shape every RAII
    /// guard in this workspace has (`GroupKillGuard` is one `libc::kill`,
    /// `ClaimGuard` one DELETE, `ShadowDirGuard` one `remove_dir_all`).
    struct Guard(std::sync::Arc<std::sync::atomic::AtomicBool>);

    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// The property the whole module exists for: when the work future is
    /// dropped, its guards run.
    #[tokio::test]
    async fn dropping_the_work_future_runs_its_guards() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let fired = Arc::new(AtomicBool::new(false));
        let flag = fired.clone();
        let work = async move {
            let _guard = Guard(flag);
            // Never resolves, so only a drop can end it.
            std::future::pending::<()>().await;
        };

        // Model the signal arm directly: `first_signal` cannot be triggered
        // from a test without actually signalling the test binary, which
        // would take down the whole harness.
        let mut work = Box::pin(work);
        tokio::select! {
            biased;
            _ = &mut work => unreachable!("the work future never completes"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => drop(work),
        }

        assert!(
            fired.load(Ordering::SeqCst),
            "dropping the work future must run the guards on its stack"
        );
    }

    /// The coordinator transition, end to end over a stand-in signal source
    /// (#613): a signal must abandon the in-flight work, run its guards, and
    /// report *which* signal it was so `main` can pick the exit code. The
    /// signal itself is never raised at the process level — that would kill
    /// the whole test binary — so `race` is driven with an explicit future.
    #[tokio::test]
    async fn a_signal_abandons_the_work_and_names_itself() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        for signal in [Interrupt::Int, Interrupt::Term] {
            let fired = Arc::new(AtomicBool::new(false));
            let flag = fired.clone();
            let work = async move {
                let _guard = Guard(flag);
                std::future::pending::<u32>().await
            };

            let outcome = race(work, async move { signal }).await;

            assert_eq!(outcome, Err(signal), "the signal must be reported as-is");
            assert!(
                fired.load(Ordering::SeqCst),
                "{signal:?} must run the guards on the abandoned work's stack"
            );
        }
    }

    /// The `biased` arm ordering, which is not cosmetic: a signal landing in
    /// the same tick as a completed turn must not discard the turn's result.
    #[tokio::test]
    async fn work_that_already_finished_beats_a_simultaneous_signal() {
        let outcome = race(async { 7u32 }, async { Interrupt::Int }).await;
        assert_eq!(outcome, Ok(7));
    }

    #[test]
    fn exit_codes_follow_the_shell_convention() {
        assert_eq!(Interrupt::Int.exit_code(), 130);
        assert_eq!(Interrupt::Term.exit_code(), 143);
    }
}
