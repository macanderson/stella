//! How the loop is asked to stop — and why the two ways of asking end
//! differently.
//!
//! A **stop file** under the durable loop root parks the run. `step` returns
//! `Blocked { OperatorStop }`, the driver waits and re-asks, and deleting the
//! file puts it back to work with no other input. That is the self-resume
//! property the whole design rests on: an operator who drops the flag has
//! already said everything that needs saying, and nothing here latches the
//! answer, so the block stops being returned on the very next poll.
//!
//! A **signal** ends the run. SIGTERM or SIGINT is the operating system saying
//! this process is finishing; there is nothing to resume into and nothing to
//! wait for, so the loop records what happened and returns.
//!
//! # Why a signal is latched and a stop file is not
//!
//! They are answers to different questions. The file answers *should this loop
//! be working right now*, which changes while the process lives and must
//! therefore be re-read every poll. The signal answers *is this process
//! ending*, which is true exactly once and cannot be untrue afterwards — and
//! it arrives on a thread the loop does not own, so something has to hold it
//! until the loop reaches a boundary where honouring it is safe.
//!
//! # What the record buys
//!
//! `audit.jsonl` for `oxagen-platform` held twenty `session_started` records
//! and four `session_stopped` ones: the other sixteen runs were signalled, and
//! `drive` wrote its stop line only on the exits it chose for itself. A reader
//! had to infer the ending from the journal going quiet — which the Observatory
//! labels `lost` for a legacy session and probes the pid for a stamped one
//! (#4360), both inference where a record belongs (#4361).
//!
//! SIGKILL cannot be caught and stays inferred. That is the only gap, and the
//! Observatory's `liveness` field says so.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};

use crate::signals::Interrupt;

/// The file whose presence parks the loop, under the durable loop root.
///
/// A file rather than a flag in this process, because the operator asking is
/// not this process: they are at a shell, and the loop may be mid-turn or on a
/// different machine's clone of the same state root.
pub(super) const STOP_FILE: &str = "stop";

/// Where [`STOP_FILE`] lives for a given loop.
pub(super) fn stop_file(dir: &Path) -> PathBuf {
    dir.join(STOP_FILE)
}

/// Whether an operator has asked this loop to park.
///
/// Read on every poll and never cached. A latch here would be the thing that
/// stops the loop resuming when the flag is dropped, which is the one property
/// `Blocked` exists to guarantee.
///
/// An unreadable path reads as *not parked*: a loop that stopped working
/// because it could not stat a file would be worse than one that kept going,
/// and the operator's next `stop` still lands.
pub(super) fn parked(durable: &super::state::LoopState) -> bool {
    stop_file(&durable.dir).exists()
}

/// No signal (0), [`Interrupt::Int`] (1), or [`Interrupt::Term`] (2).
///
/// An integer rather than an `Option<Interrupt>` behind a lock, because the
/// reader is a synchronous loop polling it between steps and the writer is a
/// watcher thread that stores exactly once.
static CAUGHT: AtomicU8 = AtomicU8::new(0);

/// The signal that has ended this process, once one has arrived.
pub(super) fn caught() -> Option<Interrupt> {
    match CAUGHT.load(Ordering::SeqCst) {
        1 => Some(Interrupt::Int),
        2 => Some(Interrupt::Term),
        _ => None,
    }
}

/// `stella self-driving stop` — write the flag [`parked`] reads.
///
/// The verb exists because the file did and nothing wrote it: an operator had
/// to `touch` a path they learned from a parked run's audit line, which is a
/// path they can only learn *after* the loop is already parked. `root` is the
/// loop's durable state directory — the one `LoopState` resolved, unless a
/// caller names another because they are stopping a loop rooted somewhere
/// else (a second clone, another machine's shared state root).
///
/// **Idempotent, and it says which case it was.** Asking a parked loop to park
/// is not an error, and reporting it as one would make a supervisor script
/// treat a successful stop as a failure. The distinction is still worth
/// printing: "already asked" tells an operator whose loop is still working
/// that the flag is not what is holding it.
///
/// Resuming is deleting the file, and the output names it. That is a verb this
/// does not have, deliberately: nothing latches the flag, so `rm` is the whole
/// operation and a second subcommand would only be a spelling of it.
pub(super) fn request(root: &Path) -> Result<(), String> {
    let path = stop_file(root);
    let already = path.exists();
    if !already {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!("cannot create the loop state dir {}: {e}", parent.display())
            })?;
        }
        std::fs::write(&path, "")
            .map_err(|e| format!("cannot write the stop flag {}: {e}", path.display()))?;
    }
    let display = path.display();
    if already {
        println!("stop already requested: {display}");
    } else {
        println!("stop requested: {display}");
    }
    println!("  the loop parks at its next boundary; it does not abandon a turn in flight");
    println!("  delete that file to put it back to work — nothing latches the request");
    Ok(())
}

/// Start watching for SIGINT and SIGTERM, and latch the first one.
///
/// A thread with a runtime of its own, because `drive` is synchronous: there is
/// no work future here to race and drop the way [`crate::signals`] does for
/// `stella run`, and the loop must reach a boundary of its own choosing before
/// it honours the signal. Killing a turn where it stands would leave a
/// worktree, a branch and possibly a pull request that nothing has recorded.
///
/// The thread is detached and outlives nothing: the process is ending either
/// way, and a signal caught after the loop has already returned changes no
/// answer.
///
/// **Returns only once the handlers are installed.** A watcher that is merely
/// starting is not watching, and a SIGTERM in that window would take the
/// default disposition and kill the run with no record — the exact failure
/// this exists to end, moved to the first moments of a run rather than
/// removed. The wait is bounded, because a run that cannot arm its watcher
/// should still work.
///
/// **The default disposition is restored the moment the first signal is
/// caught**, so a second one is a hard kill. An operator whose loop is wedged
/// inside a long turn must never be trapped in an uninterruptible process —
/// the same guarantee `crate::signals::race` gives every other door.
pub(super) fn watch() {
    let (armed_tx, armed_rx) = std::sync::mpsc::sync_channel::<()>(1);
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            // No runtime, no watcher. The loop still runs and still stops on a
            // stop file; what is lost is the record a signal would have
            // written, which is worth neither a panic nor a refusal to start.
            drop(armed_tx);
            return;
        };
        let signal = runtime.block_on(crate::signals::first_signal_armed(move || {
            let _ = armed_tx.send(());
        }));
        // SAFETY: restoring the default disposition is a signal-handler
        // registration, not a data race; the same call `crate::signals::race`
        // makes for the same reason.
        #[cfg(unix)]
        unsafe {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
        }
        CAUGHT.store(
            match signal {
                Interrupt::Int => 1,
                Interrupt::Term => 2,
            },
            Ordering::SeqCst,
        );
    });
    // A closed channel answers as well as a send does: the watcher thread gave
    // up before arming, and waiting out the timeout for a thread that has
    // already returned would delay every run for nothing.
    let _ = armed_rx.recv_timeout(std::time::Duration::from_secs(5));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stop file is read, not remembered: dropping it must put the loop
    /// back to work with no other input.
    #[test]
    fn a_dropped_stop_file_is_visible_on_the_very_next_read() {
        let dir = tempfile::tempdir().expect("state dir");
        let durable = super::super::state::LoopState {
            dir: dir.path().to_path_buf(),
            repo_root: dir.path().to_path_buf(),
        };

        assert!(!parked(&durable), "a fresh loop is not parked");

        std::fs::write(stop_file(&durable.dir), "").expect("write the stop file");
        assert!(parked(&durable), "the flag must park the loop");

        std::fs::remove_file(stop_file(&durable.dir)).expect("drop the stop file");
        assert!(
            !parked(&durable),
            "dropping the flag must resume the loop with no resume signal"
        );
    }

    /// **Witness (#4457).** The verb writes the flag the loop reads, so an
    /// operator asking a loop to stop and the loop noticing are one operation
    /// rather than two spellings that can drift.
    ///
    /// Fails on the base, where the flag had a reader and no writer: #3942
    /// taught `observe` to see the file and left `touch` — against a path an
    /// operator could only learn from a parked run's audit line, which is a
    /// line they can only read once the loop is already parked — as the way to
    /// create it.
    #[test]
    fn the_stop_verb_writes_the_flag_the_loop_reads() {
        let dir = tempfile::tempdir().expect("state dir");
        let durable = super::super::state::LoopState {
            dir: dir.path().to_path_buf(),
            repo_root: dir.path().to_path_buf(),
        };
        assert!(!parked(&durable), "the control: a fresh loop is not parked");

        request(&durable.dir).expect("the verb writes the flag");
        assert!(
            parked(&durable),
            "what the verb wrote must be what `observe` reads"
        );

        // Asking twice is not an error. A supervisor that re-issues the stop
        // must not read a successful stop as a failure.
        request(&durable.dir).expect("asking a parked loop to park is not an error");
        assert!(parked(&durable));

        std::fs::remove_file(stop_file(&durable.dir)).expect("drop the flag");
        assert!(
            !parked(&durable),
            "and the verb latches nothing the deletion cannot undo"
        );
    }

    /// **Witness (#4361).** A real SIGTERM at this process is caught, latched,
    /// and named — so the driver has something to record instead of dying
    /// where it stands.
    ///
    /// The signal is raised at the test binary itself, which is safe here for
    /// one reason and only one: [`watch`] returns after the handler is
    /// installed, so the raise below cannot land on the default disposition.
    /// Were the arming asynchronous, this test would kill the whole binary
    /// some fraction of the time.
    ///
    /// It also asserts the *pre*-state, so the latch cannot pass by having
    /// been set by something else: nothing else in this binary arms a watcher.
    #[cfg(unix)]
    #[test]
    fn a_sigterm_is_caught_latched_and_named() {
        assert_eq!(
            caught(),
            None,
            "nothing may be latched before a signal is raised"
        );

        watch();

        // SAFETY: raising a signal at this process is not a data race, and the
        // handler installed by `watch` above is what receives it.
        unsafe {
            libc::raise(libc::SIGTERM);
        }

        // The handler runs on the watcher's thread, so the store is not
        // instantaneous. Five seconds is a bound for a failure, not a wait a
        // passing run pays: the loop below returns on the first observation.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while caught().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert_eq!(
            caught(),
            Some(Interrupt::Term),
            "SIGTERM must be caught and named, not left for a reader to infer from a journal \
             that went quiet (#4361)"
        );
    }
}
