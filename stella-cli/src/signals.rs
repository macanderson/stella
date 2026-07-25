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
    let mut work = Box::pin(work);
    tokio::select! {
        // A work future that has already completed wins the race — a signal
        // arriving in the same tick must not discard a finished turn.
        biased;
        output = &mut work => Ok(output),
        signal = first_signal() => {
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
        Ok(inner) => inner,
        Err(signal) => {
            INTERRUPTED.store(signal.exit_code(), std::sync::atomic::Ordering::SeqCst);
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

    /// The property the whole module exists for: when the work future is
    /// dropped, its guards run. `GroupKillGuard` is synchronous, so a plain
    /// `Drop` witness models it exactly.
    #[tokio::test]
    async fn dropping_the_work_future_runs_its_guards() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct Guard(Arc<AtomicBool>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

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

    #[test]
    fn exit_codes_follow_the_shell_convention() {
        assert_eq!(Interrupt::Int.exit_code(), 130);
        assert_eq!(Interrupt::Term.exit_code(), 143);
    }
}
