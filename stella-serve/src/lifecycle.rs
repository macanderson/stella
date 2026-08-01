// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Process lifecycle: accepting connections, the three states `GET /readyz`
//! reports, and the drain that ends them (#1131).
//!
//! [`accept_loop`] and [`drain`] live here together because they are two
//! halves of one decision — see the "listener stays open" note on
//! [`crate::serve_until`]. Keeping them in the same file makes it hard to
//! change one without reading why the other cannot simply be stopped first.
//!
//! # Why `/readyz` is not `/healthz`
//!
//! They answer different questions and an orchestrator uses them for different
//! things. `/healthz` answers *is this process alive* — a `false` there means
//! restart me. `/readyz` answers *is it safe to send me new work* — a `false`
//! there means route elsewhere, and says nothing about whether the process
//! should live.
//!
//! Collapsing them is what makes a rolling deploy drop requests. Without a
//! readiness signal, the only way for a load balancer to learn that a
//! shutting-down instance should stop receiving traffic is to send it a
//! request and have it fail. With one, the instance says so first, the balancer
//! stops routing, and the drain then has nothing new arriving to fight.
//!
//! # Ready is not the same as running
//!
//! [`Lifecycle::mark_ready`] fires after the listener is bound and the server
//! state exists, immediately before the accept loop begins. Today that window
//! is narrow — nothing expensive happens between bind and accept — so a probe
//! is unlikely to observe not-ready at startup. It is still modeled rather
//! than assumed, because "startup does no work" is a property of today's
//! construction sequence and not a guarantee: the moment a warm-up lands
//! (a session registry to restore, a catalog to load), the endpoint is already
//! telling the truth instead of needing to be retrofitted at the point where
//! it is first wrong.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::net::TcpListener;

use crate::accept::{self, AcceptAction, AcceptBackoff};
use crate::observe::event::{AcceptKind, ServeEvent, ShutdownReason, millis};
use crate::server::{ServerState, handle_conn};

/// How often the drain re-checks whether the turn registry has emptied.
///
/// A turn settles on its own OS thread, so there is no single future to await
/// — the registry emptying is the condition, and it has to be sampled. Short
/// enough that a drain finishing in 200ms is not rounded up to a second, long
/// enough that a 25-second drain costs a couple of hundred wakeups rather
/// than a busy loop.
const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// What `GET /readyz` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Readiness {
    /// Bound and accepting; new turns and sessions are welcome.
    Ready,
    /// Bound but not yet past construction — see the module docs.
    Starting,
    /// A shutdown drain has begun. The process is alive and finishing what it
    /// has, and must not be sent anything new.
    Draining,
}

impl Readiness {
    /// The wire string. Stable — an orchestrator's probe may match on it.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Starting => "starting",
            Self::Draining => "draining",
        }
    }

    /// Whether this state should answer `200`. Only [`Readiness::Ready`] does;
    /// both other states are a `503`, which is what a load balancer reads as
    /// "take me out of rotation" without concluding the process is dead.
    pub(crate) fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// The process's ready/draining latches, shared with every connection task.
///
/// Two booleans rather than one tri-state enum because they are set by
/// different actors at different times and neither ever goes back: startup
/// sets `ready`, a signal sets `draining`, and `draining` wins. Modeling that
/// as one atomic would invite a compare-and-swap whose failure case has no
/// sensible answer.
#[derive(Debug, Default)]
pub(crate) struct Lifecycle {
    ready: AtomicBool,
    draining: AtomicBool,
}

impl Lifecycle {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Construction is finished and the accept loop is about to begin.
    pub(crate) fn mark_ready(&self) {
        self.ready.store(true, Ordering::SeqCst);
    }

    /// Begin a drain. Returns `false` if one was already in progress, so a
    /// second signal does not restart the grace clock — a `SIGTERM` followed
    /// by a `SIGINT` from an impatient operator is one shutdown, not two.
    pub(crate) fn begin_drain(&self) -> bool {
        !self.draining.swap(true, Ordering::SeqCst)
    }

    /// Whether new turns and sessions must be refused.
    pub(crate) fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// The current state, draining first: a process that is both ready and
    /// draining is draining.
    pub(crate) fn readiness(&self) -> Readiness {
        if self.draining.load(Ordering::SeqCst) {
            Readiness::Draining
        } else if self.ready.load(Ordering::SeqCst) {
            Readiness::Ready
        } else {
            Readiness::Starting
        }
    }
}

/// Accept connections until the listener is structurally unusable.
///
/// Split from [`crate::serve_until`] so the drain can run alongside it rather than
/// after it. Returns only on a fatal accept error — the ordinary exit is its
/// caller dropping this future once the drain is done.
pub(crate) async fn accept_loop(
    state: &Arc<ServerState>,
    listener: TcpListener,
) -> std::io::Result<()> {
    let mut backoff = AcceptBackoff::new();
    // Mirrors `AcceptBackoff`'s own streak. Kept here rather than exposed from
    // `accept.rs` because that file is guarded byte-identical against
    // `stella-observatory`'s copy — reporting is this server's concern, not the
    // shared policy's.
    let mut streak = 0_u32;
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => {
                backoff.succeeded();
                streak = 0;
                stream
            }
            Err(err) => match accept::classify(&err) {
                // A dead pending connection or an interrupted syscall: the
                // listener is fine, and this cannot spin (see `accept`).
                AcceptAction::Retry => {
                    backoff.succeeded();
                    streak = 0;
                    continue;
                }
                // Exhaustion, or a condition `io::ErrorKind` cannot name. Sleep
                // so it cannot busy-loop, and give up if it never clears.
                AcceptAction::Backoff => {
                    streak += 1;
                    let kind = accept_kind(&err);
                    match backoff.next_delay() {
                        Some(delay) => {
                            state.observer().emit(&ServeEvent::AcceptBackoff {
                                kind,
                                delay_ms: millis(delay),
                                streak,
                            });
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                        None => {
                            state
                                .observer()
                                .emit(&ServeEvent::AcceptGaveUp { kind, streak });
                            shutting_down(state);
                            return Err(err);
                        }
                    }
                }
                AcceptAction::Fatal => {
                    shutting_down(state);
                    return Err(err);
                }
            },
        };
        // Nagle off. Every frame on this wire gates the next engine step — a
        // `provider_request` the host has not seen yet is a step that cannot
        // proceed — so coalescing a small write with the next one trades
        // nothing for up to a delayed-ACK's worth of added latency per step.
        let _ = stream.set_nodelay(true);
        let state = Arc::clone(state);
        // Per-connection errors (client hangup, bad request) stay local to the
        // connection; the accept loop keeps serving. The error is no longer
        // discarded — `RequestRecord::close` names it before this returns.
        tokio::spawn(async move {
            let _ = handle_conn(stream, state).await;
        });
    }
}

/// Which exhaustion class an accept error belongs to, mirroring `accept.rs`'s
/// own split without modifying that (drift-guarded) file.
fn accept_kind(err: &std::io::Error) -> AcceptKind {
    match err.kind() {
        std::io::ErrorKind::OutOfMemory => AcceptKind::Exhaustion,
        // EMFILE/ENFILE/ENOBUFS have no `ErrorKind`, so they land here too —
        // "unnameable" is the honest label for what we can actually tell.
        _ => AcceptKind::Unnameable,
    }
}

fn shutting_down(state: &Arc<ServerState>) {
    state.observer().emit(&ServeEvent::ShuttingDown {
        reason: ShutdownReason::ListenerFailed,
        live_turns: state.turns().len(),
    });
}

/// The process's termination signals: `SIGTERM` (what an orchestrator sends
/// before killing a container on redeploy or scale-down) and `SIGINT` (what an
/// operator sends from a terminal). Whichever arrives first resolves.
///
/// On a non-Unix target only `Ctrl-C` exists, so that is the whole of it.
/// Returning a future rather than installing anything eagerly keeps [`serve`]
/// the only place with a process-wide side effect.
pub(crate) async fn termination_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        // A failed registration must not become a server that cannot start.
        // The honest degrade is to keep serving without a drain — the process
        // still dies on the signal, exactly as it did before this existed —
        // rather than to refuse to boot over a shutdown nicety.
        let mut term = match signal(SignalKind::terminate()) {
            Ok(term) => term,
            Err(_) => return std::future::pending().await,
        };
        let mut interrupt = match signal(SignalKind::interrupt()) {
            Ok(interrupt) => interrupt,
            Err(_) => return std::future::pending().await,
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = interrupt.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Bring every in-flight turn to a stop, gracefully first and forcibly second.
///
/// Returns once the registry is empty or `grace` has elapsed, whichever comes
/// first. The two-phase shape is the whole point: phase one costs nothing (an
/// in-flight model call lands and is kept), phase two is bounded (a turn whose
/// host stopped answering has no step boundary to reach, so only a cancel can
/// end it).
///
/// Every path here still lets a turn *emit* its terminal frame — a subscriber
/// streaming `/events` sees `turn_complete` with a settled cost. That is the
/// difference between draining and exiting: dropping the process would leave
/// the host with a stream that simply stopped, and no record of what the turn
/// had already spent.
pub(crate) async fn drain(state: &Arc<ServerState>, grace: Duration) {
    if !state.lifecycle().begin_drain() {
        return;
    }
    let live_turns = state.turns().len();
    state.observer().emit(&ServeEvent::ShuttingDown {
        reason: ShutdownReason::SignalReceived,
        live_turns,
    });
    if live_turns == 0 {
        state.observer().emit(&ServeEvent::DrainCompleted {
            drained: 0,
            cancelled: 0,
            timed_out: false,
        });
        return;
    }

    // Phase one: ask. A clone of the handles, not a held lock — a settling
    // turn takes the same registry lock on its way out, and holding it across
    // the wait below would deadlock the very drain it is meant to observe.
    for controls in state.live_controls() {
        controls.soft_stop();
    }

    // Poll rather than await a notification: a turn settles on its own OS
    // thread through a `SettleHook`, so there is no single future to wait on,
    // and the registry emptying is the one condition that means "all done".
    // The interval is short enough that a fast drain is not padded by it and
    // long enough that a slow one costs a handful of wakeups.
    let deadline = tokio::time::Instant::now() + grace;
    while tokio::time::Instant::now() < deadline && !state.turns().is_empty() {
        tokio::time::sleep(DRAIN_POLL_INTERVAL).await;
    }
    let remaining = state.turns().len();

    // Phase two: whatever is left did not reach a boundary inside the grace
    // period. Cancel it — still a step-boundary unwind that emits a terminal
    // frame, just one that also wakes a reverse request nobody is going to
    // answer.
    let timed_out = remaining > 0;
    if timed_out {
        for entry in state.live_entries() {
            entry.pending.cancel();
            entry.controls.resume();
        }
    }
    state.observer().emit(&ServeEvent::DrainCompleted {
        drained: live_turns.saturating_sub(remaining),
        cancelled: remaining,
        timed_out,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_lifecycle_is_starting_not_ready() {
        let lifecycle = Lifecycle::new();
        assert_eq!(lifecycle.readiness(), Readiness::Starting);
        assert!(!lifecycle.readiness().is_ready());
        assert!(!lifecycle.is_draining());
    }

    #[test]
    fn marking_ready_makes_it_ready() {
        let lifecycle = Lifecycle::new();
        lifecycle.mark_ready();
        assert_eq!(lifecycle.readiness(), Readiness::Ready);
        assert!(lifecycle.readiness().is_ready());
    }

    /// Draining beats ready. A process mid-drain is alive and must not be
    /// sent new work, which is precisely the state a single "healthy" boolean
    /// cannot express.
    #[test]
    fn draining_wins_over_ready() {
        let lifecycle = Lifecycle::new();
        lifecycle.mark_ready();
        assert!(lifecycle.begin_drain());
        assert_eq!(lifecycle.readiness(), Readiness::Draining);
        assert!(!lifecycle.readiness().is_ready());
        assert!(lifecycle.is_draining());
    }

    /// A drain that begins before startup finished is still a drain — a
    /// signal during construction must not be swallowed and then report
    /// `ready` once `mark_ready` runs.
    #[test]
    fn a_drain_during_startup_is_not_overwritten_by_mark_ready() {
        let lifecycle = Lifecycle::new();
        assert!(lifecycle.begin_drain());
        lifecycle.mark_ready();
        assert_eq!(lifecycle.readiness(), Readiness::Draining);
    }

    /// Only the first signal owns the drain. A second one must not restart
    /// the grace clock.
    #[test]
    fn a_second_drain_request_reports_that_one_is_already_running() {
        let lifecycle = Lifecycle::new();
        assert!(lifecycle.begin_drain(), "the first caller owns the drain");
        assert!(!lifecycle.begin_drain(), "a second signal is not a restart");
        assert!(!lifecycle.begin_drain());
    }
}
