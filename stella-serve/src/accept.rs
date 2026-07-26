// ┌───────────────────────────────────────────────────────────────────────────┐
// │ THIS FILE IS INTENTIONALLY DUPLICATED. Byte-identical copies live at      │
// │   stella-observatory/src/accept.rs                                        │
// │   stella-serve/src/accept.rs                                              │
// │ and both servers must apply the same accept() policy (issue #637).        │
// │                                                                           │
// │ Why a copy and not a shared crate: `stella-observatory` makes "no new     │
// │ dependencies" a numbered design constraint in its own module docs and     │
// │ links zero `stella-*` crates, so there is no shared home to put this in   │
// │ that does not break a documented architectural decision.                  │
// │                                                                           │
// │ EDIT BOTH FILES, ALWAYS. `the_two_copies_of_this_policy_have_not_drifted` │
// │ below runs in both crates and fails the moment the two texts differ, so   │
// │ a one-sided edit is a red test, never a silent divergence.                │
// └───────────────────────────────────────────────────────────────────────────┘

//! One shared policy for classifying `accept()` failures.
//!
//! # The policy
//!
//! An `accept()` failure is not one thing, and both blanket policies are wrong:
//!
//! - **Everything fatal** — where both of this workspace's servers started —
//!   tears down the whole listener because *one* peer hung up between its SYN
//!   and our `accept()`. `ECONNABORTED` is a routine event on any port a scanner
//!   touches; a server that dies on it is a server that dies at random.
//! - **Everything transient** turns a permanently broken listener into a hot
//!   busy loop, spinning a core forever on a condition that will never clear.
//!
//! So: three classes, plus a bounded retry streak that makes *misclassifying*
//! survivable in either direction.
//!
//! ## [`AcceptAction::Retry`] — try again at once
//!
//! Two groups, both of which are structurally incapable of spinning:
//!
//! - **The syscall did not complete.** `Interrupted` (EINTR), `WouldBlock`
//!   (EAGAIN/EWOULDBLOCK). Nothing failed; the call was cut short by a signal or
//!   readiness was spurious. Neither can recur without the kernel handing us a
//!   fresh wakeup first.
//! - **A connection died; the listener is fine.** `ConnectionAborted`
//!   (ECONNABORTED), `ConnectionReset` (ECONNRESET), `PermissionDenied` (EPERM —
//!   "firewall rules forbid connection", a property of that connection, not of
//!   our socket). Each one consumed a real accept-queue entry that a real peer
//!   had to create, so the rate is bounded by inbound connections rather than by
//!   our loop. Retrying immediately is the same call nginx and Go's `net/http`
//!   make, and it is why no sleep is needed here.
//!
//! ## [`AcceptAction::Backoff`] — sleep, then try again
//!
//! Resource exhaustion: `EMFILE`/`ENFILE` (the process or system fd table is
//! full), `ENOBUFS`/`ENOMEM` (kernel memory). These *do* recur instantly — an
//! fd-exhausted process gets EMFILE on every call until something closes — so
//! this is the one class where sleeping is mandatory. It is equally the class
//! where dying would be wrong: the cause is usually elsewhere in the process,
//! and it clears.
//!
//! Rust cannot name most of them. `io::ErrorKind` has no EMFILE, ENFILE or
//! ENOBUFS variant, so they arrive as the unmatchable `Uncategorized`.
//! Discriminating them would take a hardcoded per-platform errno table — exactly
//! the guessing this policy exists to remove. So the unnamed bucket is *treated*
//! as exhaustion. That is the deliberately safe direction: `Uncategorized` also
//! holds genuinely fatal errnos (EBADF, ENOTSOCK), but retrying those costs at
//! most the streak cap below, whereas dying on a recoverable EMFILE is an
//! outage.
//!
//! ## [`AcceptAction::Fatal`] — the listener is not a listener
//!
//! `InvalidInput` (EINVAL: the descriptor is not accepting connections) and
//! `Unsupported` (EOPNOTSUPP: not a socket that can accept). Both describe our
//! own socket's state rather than any connection's, so every later call returns
//! the same thing. Retrying is provably pointless.
//!
//! ## Why the streak cap is what makes this safe
//!
//! A `Backoff` that never clears is a process that is up and accepting nothing:
//! from outside, indistinguishable from a crash, but silent. After
//! [`GIVE_UP_STREAK`] consecutive backoffs — roughly a minute once the delay
//! caps — the loop surfaces the error and stops, so a supervisor restarts it.
//! Any successful accept, and any `Retry`-class failure, resets the streak, so a
//! busy server that trips EMFILE occasionally never accumulates toward it.
//!
//! # Why this file exists twice
//!
//! `stella-observatory` and `stella-serve` are deliberately leaf crates with no
//! shared dependency that could host this: the observatory's own module docs make
//! "**No new dependencies**" a numbered design constraint, and it links no
//! `stella-*` crate at all. Adding one purely to share ~40 lines of policy would
//! trade a documented architectural decision for a code-duplication nit.
//!
//! So the module is duplicated verbatim in `stella-observatory/src/accept.rs` and
//! `stella-serve/src/accept.rs` — and duplication is only acceptable here because
//! it cannot happen quietly. The two servers must not diverge (that is the whole
//! point of writing the policy down), so
//! [`tests::the_two_copies_of_this_policy_have_not_drifted`] compares the two
//! files byte-for-byte at compile time. It is compiled into *both* crates, so
//! `cargo test` on either one fails the instant they differ. Change one, change
//! the other.

use std::io;
use std::time::Duration;

/// First sleep after a [`AcceptAction::Backoff`] failure. Short enough that a
/// one-off exhaustion blip costs nothing observable.
const BACKOFF_START: Duration = Duration::from_millis(10);

/// Ceiling on the backoff sleep. Bounds how stale the listener's view of the
/// accept queue gets while it waits for fds to free up.
const BACKOFF_MAX: Duration = Duration::from_secs(1);

/// Consecutive [`AcceptAction::Backoff`] failures tolerated before the loop
/// gives up and surfaces the error. At [`BACKOFF_MAX`] this is a little over a
/// minute of accepting nothing — long enough to ride out real fd pressure,
/// short enough that a wedged listener does not sit there silently forever.
const GIVE_UP_STREAK: u32 = 64;

/// What an accept loop should do about one `accept()` failure. See the module
/// docs for the reasoning behind each class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AcceptAction {
    /// Nothing about the listener is wrong — retry immediately. Cannot spin:
    /// every error in this class needs a fresh kernel event to recur.
    Retry,
    /// Something is exhausted, or unnameable by `io::ErrorKind` — sleep, then
    /// retry. Gives up via [`AcceptBackoff::next_delay`] if it never clears.
    Backoff,
    /// The listener itself is unusable; retrying cannot ever succeed.
    Fatal,
}

/// Classify one `accept()` error. Pure and total — every `io::Error` lands in
/// exactly one class, with the unnameable ones defaulting to [`AcceptAction::Backoff`].
pub(crate) fn classify(err: &io::Error) -> AcceptAction {
    match err.kind() {
        // The syscall was cut short, or readiness was spurious.
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => AcceptAction::Retry,
        // The pending connection died, or a firewall refused it. Per-connection,
        // never a property of our socket.
        io::ErrorKind::ConnectionAborted
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::PermissionDenied => AcceptAction::Retry,
        // Nameable exhaustion (ENOMEM). EMFILE/ENFILE/ENOBUFS have no
        // `ErrorKind` and fall through to the same class below.
        io::ErrorKind::OutOfMemory => AcceptAction::Backoff,
        // Our own socket is not in a state that can accept.
        io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported => AcceptAction::Fatal,
        // Unnameable: treated as exhaustion on purpose — see the module docs.
        _ => AcceptAction::Backoff,
    }
}

/// Backoff state for one accept loop: the current sleep and how many
/// consecutive [`AcceptAction::Backoff`] failures have gone unresolved.
pub(crate) struct AcceptBackoff {
    delay: Duration,
    streak: u32,
}

impl AcceptBackoff {
    pub(crate) fn new() -> Self {
        Self {
            delay: BACKOFF_START,
            streak: 0,
        }
    }

    /// Record evidence that the listener works — a successful accept, or a
    /// [`AcceptAction::Retry`]-class failure (which only a real peer can cause).
    /// Resets both the delay and the streak, so occasional pressure on a busy
    /// server never accumulates toward [`GIVE_UP_STREAK`].
    pub(crate) fn succeeded(&mut self) {
        self.delay = BACKOFF_START;
        self.streak = 0;
    }

    /// Record a backoff-class failure and yield how long to sleep, or `None`
    /// once [`GIVE_UP_STREAK`] consecutive failures have gone unresolved — at
    /// which point the caller must treat the error as fatal and stop the loop.
    pub(crate) fn next_delay(&mut self) -> Option<Duration> {
        self.streak += 1;
        if self.streak > GIVE_UP_STREAK {
            return None;
        }
        let delay = self.delay;
        self.delay = (self.delay * 2).min(BACKOFF_MAX);
        Some(delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The drift guard: the two copies of this policy must stay byte-identical.
    ///
    /// `stella-observatory` and `stella-serve` deliberately share no crate that
    /// could host this module — the observatory's design constraints forbid it a
    /// `stella-*` dependency — so the policy is duplicated instead. That is only
    /// safe if a one-sided edit is impossible to miss, which is this test's whole
    /// job: it fails loudly the moment the two files differ by a single byte.
    ///
    /// Both paths are spelled from the workspace root rather than as "this file"
    /// and "the other one", so this test's own source text is identical in both
    /// copies — a self-referential guard has to be, or it would itself be the
    /// first difference. Each copy therefore includes itself *and* its twin, and
    /// the assertion runs in both crates' suites.
    ///
    /// `include_str!` reaching outside the crate directory is fine here: it is a
    /// compile-time file read, not a dependency, and it is confined to
    /// `#[cfg(test)]`, so it never enters a normal build. Neither crate is
    /// published (`publish = false` workspace-wide), so there is no packaged
    /// artifact for it to affect.
    ///
    /// If this test fails: do not "fix" it by editing one side to match. Decide
    /// what the policy should be, apply that to **both** files, and keep the two
    /// servers' behaviour identical.
    #[test]
    fn the_two_copies_of_this_policy_have_not_drifted() {
        const OBSERVATORY: &str = include_str!("../../stella-observatory/src/accept.rs");
        const SERVE: &str = include_str!("../../stella-serve/src/accept.rs");

        assert_eq!(
            OBSERVATORY, SERVE,
            "the accept() policy has drifted between stella-observatory and \
             stella-serve.\n\n\
             `stella-observatory/src/accept.rs` and `stella-serve/src/accept.rs` \
             are deliberately byte-identical duplicates: issue #637 requires both \
             servers to apply one written policy, and the observatory takes no \
             `stella-*` dependency (a numbered constraint in its own module docs), \
             so there is no shared crate to hold this.\n\n\
             Whatever you just changed in one file, make the same change in the \
             other — or if the two servers genuinely need to differ, delete this \
             guard and say why in both files."
        );
    }

    /// Synthesised kinds, not real syscall conditions: EMFILE and ECONNABORTED
    /// cannot be provoked locally, but the classification is a pure function of
    /// the kind, so the table is what needs pinning.
    #[test]
    fn transient_kinds_retry_without_sleeping() {
        for kind in [
            io::ErrorKind::Interrupted,
            io::ErrorKind::WouldBlock,
            io::ErrorKind::ConnectionAborted,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::PermissionDenied,
        ] {
            assert_eq!(
                classify(&io::Error::from(kind)),
                AcceptAction::Retry,
                "{kind:?} is per-connection or a non-completion: killing the \
                 listener over it is the bug this policy fixes"
            );
        }
    }

    #[test]
    fn exhaustion_and_unnameable_kinds_back_off() {
        assert_eq!(
            classify(&io::Error::from(io::ErrorKind::OutOfMemory)),
            AcceptAction::Backoff
        );
        // EMFILE (24) / ENFILE (23) / ENOBUFS have no `ErrorKind`, so they
        // arrive raw and must still land in the backoff class rather than
        // spinning or killing the server.
        for errno in [23, 24] {
            assert_eq!(
                classify(&io::Error::from_raw_os_error(errno)),
                AcceptAction::Backoff,
                "errno {errno} must back off, not spin"
            );
        }
        // Anything else we cannot name defaults to backoff, bounded by the
        // streak cap.
        assert_eq!(
            classify(&io::Error::other("something new")),
            AcceptAction::Backoff
        );
    }

    #[test]
    fn broken_listener_kinds_are_fatal() {
        for kind in [io::ErrorKind::InvalidInput, io::ErrorKind::Unsupported] {
            assert_eq!(
                classify(&io::Error::from(kind)),
                AcceptAction::Fatal,
                "{kind:?} describes our own socket: retrying cannot succeed"
            );
        }
    }

    #[test]
    fn backoff_grows_to_the_ceiling_and_never_returns_zero() {
        let mut backoff = AcceptBackoff::new();
        let first = backoff.next_delay().expect("first failure sleeps");
        assert_eq!(first, BACKOFF_START);
        assert_eq!(backoff.next_delay(), Some(BACKOFF_START * 2));
        // Walk up to the ceiling and confirm it stays there rather than
        // overflowing or growing unbounded.
        let mut last = Duration::ZERO;
        for _ in 0..10 {
            last = backoff.next_delay().expect("still under the streak cap");
            assert!(last <= BACKOFF_MAX, "delay {last:?} exceeded the ceiling");
            assert!(last > Duration::ZERO, "a backoff of zero would busy-spin");
        }
        assert_eq!(last, BACKOFF_MAX);
    }

    #[test]
    fn a_successful_accept_resets_the_streak_and_the_delay() {
        let mut backoff = AcceptBackoff::new();
        for _ in 0..GIVE_UP_STREAK {
            assert!(backoff.next_delay().is_some());
        }
        backoff.succeeded();
        assert_eq!(
            backoff.next_delay(),
            Some(BACKOFF_START),
            "one good accept must fully clear the streak, so a busy server that \
             trips exhaustion now and then never dies of accumulated history"
        );
    }

    #[test]
    fn an_unclearing_condition_gives_up_instead_of_looping_forever() {
        let mut backoff = AcceptBackoff::new();
        for i in 0..GIVE_UP_STREAK {
            assert!(
                backoff.next_delay().is_some(),
                "attempt {i} is still within the cap"
            );
        }
        assert_eq!(
            backoff.next_delay(),
            None,
            "a listener that has accepted nothing for the whole streak must \
             surface the error so a supervisor can restart it"
        );
    }
}
