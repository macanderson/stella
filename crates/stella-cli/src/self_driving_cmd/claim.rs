//! The ledger claim this loop holds while it is working an issue (#4300).
//!
//! [`super::contention`] gathers four signals, and three of them are names:
//! a remote branch, a pull request title, a worktree path. A name cannot say
//! *when*. That is the whole difficulty #4300 names — at claim time the
//! loop's own crashed run and a live peer leave the same string at the same
//! path under the same root, so no filter over paths can separate them.
//!
//! This module supplies the signal that can: a **fenced lease** in the
//! workspace's fleet ledger, taken before a turn starts and released when it
//! settles, renewed by a heartbeat for as long as the turn runs. Liveness is
//! what distinguishes the two cases and a lease is what carries liveness:
//!
//! | Who left the worktree under the loop's root | Its lease | Claim time |
//! |---|---|---|
//! | this loop's own crashed run | lapsed — nothing renewed it | proceed, and `work::start` repairs it |
//! | a live peer self-driving process | held and beating | defer |
//!
//! So the own-root worktree exclusion in [`super::contention::worktrees_naming`]
//! is safe *because this exists*. Without a producer, `ledger_claims` is
//! always empty, the exclusion drops a live peer's worktree along with the
//! loop's own, and #4300's second half — "a live peer's still does" — is not
//! delivered.
//!
//! # Why a lease and not a lock file or a pid
//!
//! Because the crash case is the case. A lock file has to be cleaned up by
//! the process that died; a pid can be reused, and is meaningless across the
//! shared checkout of a machine that reboots. A lease expires with no reaper,
//! no daemon and no `--force`: `stella_fleet`'s claim statement evaluates
//! expiry inside the write, so a dead holder's key is reclaimed as a side
//! effect of the next claimant asking for it. `crates/stella-fleet/src/ledger/lease.rs`
//! carries the full argument; this module is a second dispatcher on the same
//! table, which is what the `issue:<n>` namespace was reserved for.
//!
//! # It also closes the race the read-only probe cannot
//!
//! [`super::contention::for_issue`] reads. Two loops polling the same clone
//! can both read "free" before either writes — the exact race
//! [`stella_fleet::Ledger::claim_dispatch`] exists to close, in one
//! conditional statement where the affected-row count decides the winner. So
//! the Claim arm asks for the lease *after* the verdict: the verdict is what
//! weighs branches and worktrees, and the lease is what makes two loops agree
//! about who won.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use stella_fleet::{ClaimOutcome, DispatchLease, Ledger, RenewOutcome, issue_claim_key};

/// How long an issue looks taken after this process stops beating.
///
/// A heartbeat renews well inside it, so this never has to cover the length
/// of a turn — only the longest pause a *live* loop can take between two
/// local SQLite writes without being dead. Five minutes is generous for that
/// and is the number that matters in the other direction: after a hard kill
/// it is how long the loop's own issue stays deferred before the recovery in
/// `work::start` becomes reachable again.
///
/// Deliberately shorter than `stella_fleet`'s 15-minute task lease. A fleet
/// worker's rival would duplicate a whole dispatched task; here the loser is
/// the loop's own next pass, and the thing being protected — one issue, one
/// poll interval — is cheaper than the delay a long TTL costs after a crash.
const LEASE_TTL: Duration = Duration::from_secs(5 * 60);

/// How often the heartbeat thread wakes to look at the stop flag.
///
/// Independent of the beat cadence, and small, because it is also what
/// dropping a [`Lease`] waits for: a loop that finishes an issue should
/// release the key immediately, not a heartbeat interval later.
const STOP_POLL: Duration = Duration::from_millis(250);

/// The answer to asking for an issue.
#[derive(Debug)]
pub(super) enum Claim {
    /// This run holds it. Dropping the [`Lease`] releases the key.
    Granted(Lease),
    /// A live peer holds it, named so the audit line can say who.
    HeldBy(String),
    /// The ledger could not answer.
    ///
    /// **Fails open**, like every other contention probe: an unopenable
    /// ledger is not a peer, and a loop meant to run for days cannot treat a
    /// local I/O error as somebody else's work. The cost is that this pass
    /// works the issue without a lease, so a peer starting in the same
    /// moment sees no claim — the weaker signals still apply, and the
    /// alternative is a loop that stops working because a file would not
    /// open.
    Unavailable,
}

/// A held claim, released when it drops.
///
/// Ownership is the release: every path out of the Work arm — the outcome,
/// an early `continue`, the run reaching its bound, a `?` — drops this, and
/// the key is free for the next pass rather than waiting out [`LEASE_TTL`].
#[derive(Debug)]
pub(super) struct Lease {
    /// The grant: key, owner and fence, which is what renew and release are
    /// conditional on.
    lease: DispatchLease,
    /// The ledger this grant lives in. Held as a path rather than a
    /// connection because the heartbeat runs on its own thread and
    /// `rusqlite::Connection` is not `Sync`.
    path: PathBuf,
    /// Set to stop the heartbeat.
    stop: Arc<AtomicBool>,
    /// The heartbeat, joined before the release so a renewal cannot land
    /// after it.
    beat: Option<JoinHandle<()>>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(beat) = self.beat.take() {
            let _ = beat.join();
        }
        // Fenced, so a lease this process already lost cannot delete the
        // rival row that superseded it. Best-effort: a ledger that will not
        // open on the way out leaves a row that expires on its own, which is
        // the whole reason the row has an expiry.
        if let Ok(ledger) = Ledger::open(&self.path) {
            let _ = ledger.release_dispatch(&self.lease);
        }
    }
}

/// Take the ledger claim on `key`, for as long as the returned lease lives.
///
/// Every failure is [`Claim::Unavailable`] rather than an error: see that
/// variant for why a probe that cannot answer must not defer.
pub(super) fn acquire(root: &Path, key: &str) -> Claim {
    acquire_as(root, key, &owner())
}

/// [`acquire`], with the holder's identity given rather than derived.
///
/// The identity is a parameter because a peer is *defined* by being a
/// different one, and a test that mints a peer's claim under this process's
/// own owner string would be testing the loop against itself — which
/// [`super::contention::claims_naming`] deliberately treats as no contention
/// at all.
pub(super) fn acquire_as(root: &Path, key: &str, owner: &str) -> Claim {
    acquire_with(root, key, owner, LEASE_TTL)
}

/// [`acquire_as`], with the lease's lifetime given rather than fixed.
///
/// The TTL is a parameter for the same reason the owner is one: the thing
/// worth proving about it cannot be reached otherwise. Renewal is a *timing*
/// behaviour, and nothing that finishes in milliseconds can observe it — every
/// test written about this module completed far inside [`LEASE_TTL`] and so
/// passed with the heartbeat thread deleted entirely, while a turn longer than
/// five minutes (the normal case for a `stella run` on a real issue, not the
/// edge case) would lose its lease mid-flight to a peer polling at that
/// moment (#4314).
///
/// Deliberately **not** a setting. Five minutes is the right default and an
/// operator has no reason to tune it — see [`LEASE_TTL`] for both directions
/// of that argument. This makes the constant observable to a test without
/// making it configurable by anyone.
pub(super) fn acquire_with(root: &Path, key: &str, owner: &str, ttl: Duration) -> Claim {
    let Ok(path) = stella_store::workspace_private_sqlite_path(root, "fleet.db") else {
        return Claim::Unavailable;
    };
    let Ok(ledger) = Ledger::open(&path) else {
        return Claim::Unavailable;
    };
    let Some(now_ms) = now_ms() else {
        return Claim::Unavailable;
    };

    let ttl_ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
    match ledger.claim_dispatch(&issue_claim_key(key), owner, now_ms, ttl_ms) {
        Ok(ClaimOutcome::Granted(lease)) => Claim::Granted(hold(path, lease)),
        Ok(ClaimOutcome::Held(who)) => Claim::HeldBy(
            who.map(|claim| claim.owner)
                .unwrap_or_else(|| "another worker".to_string()),
        ),
        Err(_) => Claim::Unavailable,
    }
}

/// Who this process is in the ledger.
///
/// The pid is here to make an audit line legible — "held by
/// `self-driving:8412`" tells an operator which process to look for — not to
/// be checked for liveness. Liveness is the lease's expiry, which is the one
/// answer that survives a reboot and a recycled pid.
#[must_use]
pub(super) fn owner() -> String {
    format!("self-driving:{}", std::process::id())
}

/// Wrap a granted lease in the guard, and start beating.
fn hold(path: PathBuf, lease: DispatchLease) -> Lease {
    let stop = Arc::new(AtomicBool::new(false));
    let beat = {
        let path = path.clone();
        let lease = lease.clone();
        let stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("self-driving-claim".to_string())
            .spawn(move || beat(&path, lease, &stop))
            .ok()
    };
    Lease {
        lease,
        path,
        stop,
        beat,
    }
}

/// Renew until told to stop, or until the key is somebody else's.
///
/// A failed renewal is retried on the next beat — a busy SQLite file is not a
/// lost lease, and giving up on one would let a live loop's key lapse
/// underneath it. [`RenewOutcome::Lost`] is different: a rival already
/// reclaimed the key, and beating on would be trying to extend a grant this
/// process no longer holds. The turn in flight is deliberately left running,
/// which is `stella_fleet::Fleet::dispatch`'s policy for the same event —
/// killing a worker mid-flight throws away real work to enforce a lease whose
/// own documentation calls this overlap the price of not wedging.
fn beat(path: &Path, mut lease: DispatchLease, stop: &AtomicBool) {
    let Ok(ledger) = Ledger::open(path) else {
        return;
    };
    let interval = Duration::from_millis(lease.heartbeat_interval_ms());
    let mut since = Duration::ZERO;

    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(STOP_POLL);
        since += STOP_POLL;
        if since < interval {
            continue;
        }
        since = Duration::ZERO;

        let Some(now_ms) = now_ms() else {
            continue;
        };
        match ledger.renew_dispatch(&lease, now_ms) {
            Ok(RenewOutcome::Renewed(renewed)) => lease = renewed,
            Ok(RenewOutcome::Lost(_)) => return,
            Err(_) => {}
        }
    }
}

/// Wall-clock milliseconds, or `None` on a clock before the epoch.
///
/// Shared with [`super::contention`] so the two halves of one question — who
/// holds a claim, and who is asking — read the same clock.
#[must_use]
pub(super) fn now_ms() -> Option<u64> {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    Some(since.as_millis().min(u128::from(u64::MAX)) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace root with nothing in it — `acquire` creates the ledger it
    /// needs, because a loop that has never fanned out still has peers.
    fn workspace() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    /// A second self-driving process, as far as the ledger can tell.
    const PEER: &str = "self-driving:99999";

    /// The producer half of #4300: taking an issue writes a claim the
    /// claim-time *reader* can see.
    ///
    /// Asserted through `contention::ledger_claims` rather than by querying
    /// the ledger directly, because the defect this closes was precisely that
    /// the reader existed and nothing ever fed it: a probe that always returns
    /// empty passes every test written about the probe.
    #[test]
    fn a_held_claim_is_visible_to_the_claim_time_reader() {
        let root = workspace();

        let held = acquire_as(root.path(), "4300", PEER);
        assert!(matches!(held, Claim::Granted(_)));

        assert_eq!(
            super::super::contention::ledger_claims(root.path(), "4300"),
            vec![format!("issue:4300 held by {PEER}")]
        );
    }

    /// Releasing is the drop, not the TTL: the next pass sees a free key
    /// immediately rather than waiting out `LEASE_TTL`.
    #[test]
    fn dropping_the_lease_frees_the_key_at_once() {
        let root = workspace();

        drop(acquire_as(root.path(), "4300", PEER));

        assert!(super::super::contention::ledger_claims(root.path(), "4300").is_empty());
        assert!(matches!(acquire(root.path(), "4300"), Claim::Granted(_)));
    }

    /// Two loops, one clone: the second is told who won rather than both
    /// working the issue. This is the race the read-only probe cannot close,
    /// because both can read "free" before either writes.
    #[test]
    fn a_second_claimant_loses_and_is_told_who_holds_it() {
        let root = workspace();

        let _peer = acquire_as(root.path(), "4300", PEER);
        let ours = acquire(root.path(), "4300");

        match ours {
            Claim::HeldBy(who) => assert_eq!(who, PEER),
            other => panic!("expected the key to be held, got {other:?}"),
        }
    }

    /// A claim on one issue says nothing about another.
    #[test]
    fn a_claim_is_scoped_to_its_own_issue() {
        let root = workspace();

        let _held = acquire_as(root.path(), "4300", PEER);

        assert!(super::super::contention::ledger_claims(root.path(), "4301").is_empty());
        assert!(matches!(acquire(root.path(), "4301"), Claim::Granted(_)));
    }

    /// **Witness (#4314).** The heartbeat actually renews: a lease held past
    /// its own TTL is still live.
    ///
    /// Every other test in this module completes in milliseconds and so passes
    /// with the `beat` thread deleted. This one fails without it, which is the
    /// only way to cover the case that matters — a turn longer than the TTL,
    /// which for a real `stella run` is the normal case.
    ///
    /// # The numbers, and why they survive a loaded CI box
    ///
    /// A 2s TTL gives a 666ms beat interval (`heartbeat_interval_ms` is
    /// TTL/3), which is comfortably coarser than the 250ms [`STOP_POLL`] the
    /// thread wakes on — so the TTL is chosen against that constant rather
    /// than the constant being lowered to suit a test. Beats therefore land at
    /// roughly 750ms, 1500ms, 2250ms and 3000ms, each pushing the expiry two
    /// seconds out.
    ///
    /// The assertion is at 3500ms. Without a heartbeat the row expired at
    /// 2000ms and is gone. With one, the last beat before the assertion sets
    /// expiry to ~5000ms — and the margin is two *consecutive* missed beats,
    /// which is exactly what a TTL/3 cadence is designed to absorb: even if
    /// only the 1500ms beat had landed the row is still live at 3500ms. A test
    /// green on a laptop and red on a loaded box is worse than none.
    #[test]
    fn the_heartbeat_keeps_a_lease_alive_past_its_own_ttl() {
        // Declared before the lease so it outlives it: dropping a `Lease`
        // joins the beat thread and writes to the ledger underneath.
        let root = workspace();
        let ttl = Duration::from_millis(2_000);

        let held = acquire_with(root.path(), "4314", PEER, ttl);
        assert!(matches!(held, Claim::Granted(_)), "the key was free");

        std::thread::sleep(Duration::from_millis(3_500));

        // Through the claim-time reader, which filters on liveness
        // (`live_dispatch_claims`) — so this asserts what a *peer* would see
        // rather than merely that a row exists.
        assert_eq!(
            super::super::contention::ledger_claims(root.path(), "4314"),
            vec![format!("issue:4314 held by {PEER}")],
            "a lease held past its TTL must still be live — without the \
             heartbeat a peer polling now takes an issue this loop is working"
        );
    }

    /// A lease a rival has already taken stops the heartbeat rather than
    /// spinning on a grant this process no longer holds.
    ///
    /// The `Lost` branch of [`beat`], which nothing covered. Asserted by
    /// bounded wait rather than a bare `join`: the failure being guarded
    /// against is a thread that never returns, and a `join` on one would hang
    /// the suite instead of failing it.
    #[test]
    fn the_heartbeat_stops_when_a_rival_takes_the_key() {
        let root = workspace();
        let path = stella_store::workspace_private_sqlite_path(root.path(), "fleet.db")
            .expect("a path under the workspace");
        let ledger = Ledger::open(&path).expect("the ledger opens");
        let key = issue_claim_key("4314");

        // A grant that lapses, so the rival's claim can take the key and bump
        // the fence — which is what makes the first grant unrenewable.
        let ours = match ledger
            .claim_dispatch(&key, PEER, now_ms().expect("clock"), 200)
            .expect("the key is free")
        {
            ClaimOutcome::Granted(lease) => lease,
            ClaimOutcome::Held(_) => panic!("the key was free"),
        };
        std::thread::sleep(Duration::from_millis(250));
        let rival = ledger.claim_dispatch(&key, "self-driving:1", now_ms().expect("clock"), 60_000);
        assert!(
            matches!(rival, Ok(ClaimOutcome::Granted(_))),
            "the lapsed key must be reclaimable: {rival:?}"
        );

        let (tx, rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                beat(&path, ours, &stop);
                let _ = tx.send(());
            });
        }

        let returned = rx.recv_timeout(Duration::from_secs(5)).is_ok();
        // Set whatever happened, so a spinning thread does not outlive the
        // test and keep writing into a tempdir that is about to go.
        stop.store(true, Ordering::Relaxed);
        assert!(
            returned,
            "`beat` must return on RenewOutcome::Lost — a rival holds the key, \
             and beating on would be trying to extend a grant this process no \
             longer has"
        );
    }

    /// #4309's third requirement: the loop never defers on its own lease.
    ///
    /// The claim this process holds is real, live, and names the issue — and
    /// reads as no contention, because contention is other people. Without
    /// this the producer would hand the loop a new way to defer forever on
    /// evidence only it produces, which is #4300 again through the one signal
    /// that is supposed to be authoritative.
    #[test]
    fn the_loop_never_defers_on_its_own_lease() {
        let root = workspace();

        let ours = acquire(root.path(), "4300");
        assert!(matches!(ours, Claim::Granted(_)));

        assert!(super::super::contention::ledger_claims(root.path(), "4300").is_empty());
    }
}
