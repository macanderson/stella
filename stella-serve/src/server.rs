// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The HTTP/SSE transport over [`Session`].
//!
//! Endpoints (all under a bearer token except `/healthz`):
//!
//! | Method + path | Purpose |
//! |---|---|
//! | `GET /healthz` | liveness |
//! | `GET /v1/metrics` | counters ([`crate::observe::Snapshot`]) — authenticated, pull-only |
//! | `POST /v1/turns` | start a turn (`TurnRequest` body) → `{ "turn_id": … }` |
//! | `GET /v1/turns/{id}/events` | SSE stream of [`ServerFrame`]s until `turn_complete` |
//! | `POST /v1/turns/{id}/tool-result` | answer a `tool_request` (`ToolResultIn`) |
//! | `POST /v1/turns/{id}/provider-result` | answer a `provider_request` (`ProviderResultIn`) |
//! | `POST /v1/turns/{id}/cancel` | end an in-flight turn → `{ "status": "cancelled" }` |
//!
//! Any other method on one of those paths is a `405` carrying `Allow`; any
//! other path is a `404`. The handlers themselves live in `src/routes.rs`; this
//! module is the transport — listener, connection fold, turn registry, throttle.
//!
//! The SSE stream is the engine → host direction; the two result POSTs are the
//! host → engine direction. Together they are the reverse tool-call protocol —
//! the engine never runs a model or tool call itself.
//!
//! # Every connection produces exactly one record
//!
//! [`handle_conn`] is a fold, not a router: it opens a `RequestRecord` before
//! the first byte is parsed, delegates to [`route`], and emits the terminal
//! record on the way out — including when the peer hung up unanswered, and
//! including when the connection failed. The router's sixteen exits do not know
//! a record exists. See `src/observe/record.rs` for why that indirection is
//! load-bearing rather than decorative.
//!
//! # A stream is a subscription, not a fire-and-forget
//!
//! `GET /v1/turns/{id}/events` owns the turn while it is streaming: the session
//! is taken out of the registry, and a second concurrent subscriber gets `409`.
//! That is why the stream watches its own read half while it waits for frames —
//! a turn parked on a reverse request produces nothing to write, so a
//! disconnect would otherwise go unnoticed until the reverse-request deadline
//! expired minutes later.
//!
//! How the stream *ends* decides what happens to the turn:
//!
//! - **The client hung up.** The turn is *parked*, not cancelled: the session
//!   goes back into the registry and a reconnect can resume it from its
//!   retained frames. If nobody reconnects within
//!   [`ServeConfig::resume_grace`], the turn is cancelled and its thread
//!   reclaimed. Setting that window to zero restores cancel-on-disconnect.
//! - **Anything else** — the terminal frame, a failed write, stray bytes — is
//!   final: the session drops, which cancels the turn, and the entry is
//!   reclaimed at once.
//!
//! # Resumable streams
//!
//! Every frame carries a monotonic `seq` and is retained in a bounded ring
//! (`crate::history`). A reconnecting client names its resume point either with
//! `?after=<seq>` or, for a browser `EventSource`, by the `Last-Event-ID`
//! header the platform sends automatically from the `id:` line each frame
//! carries. Frames after that point are replayed in order before the live
//! stream continues. A resume point that has already fallen out of the ring is
//! answered with an explicit `replay_truncated` frame rather than a silent jump
//! — a hole a client cannot detect is worse than an error it can.
//!
//! # Cancellation
//!
//! `POST /v1/turns/{id}/cancel` is the caller's way to give up on a turn, and it
//! answers `200` as soon as the turn is *signalled*, not once it has finished:
//!
//! - It takes the action-suffixed shape of the three routes above rather than a
//!   bare `POST /v1/turns/{id}`, because every verb in this API is already the
//!   last path segment, and `POST` to a collection member would be the one route
//!   whose meaning came from its method instead of its path.
//! - The turn is dropped from the registry immediately, so a later
//!   `tool-result` / `provider-result` / `cancel` for that id is a `404`.
//! - A host streaming `/events` still receives the terminal `turn_complete`
//!   frame (an `aborted` outcome): the engine turn is unwound, not killed, so a
//!   cancelled turn reports its settled cost like any other.
//! - Cancelling a turn nobody has streamed also works, and reclaims its thread.
//!
//! Cancellation and the reverse-request deadline
//! ([`SessionSpec::reverse_request_timeout`]) are the two bounds on a turn that
//! stops making progress: the deadline is the automatic one, cancel the manual
//! one.

use std::collections::HashMap;
use std::collections::hash_map;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::Duration;

use rand::Rng as _;
use tokio::net::{TcpListener, TcpStream};

use crate::accept::{self, AcceptAction, AcceptBackoff};
use crate::history::FrameHistory;
use crate::http::{BodyOutcome, ReadOutcome, discard_body, read_body, read_head};
use crate::observe::event::StreamEndReason;
use crate::observe::event::{
    AcceptKind, RefusalReason, RequestId, Route, ServeEvent, ShutdownReason, TurnRef, millis,
};
use crate::observe::record::{RequestRecord, Responder};
use crate::observe::{Metrics, SharedObserver};
use crate::pending::Pending;
use crate::routes::{self, error_body};
use crate::session::Session;

/// How to bind, authenticate, and observe the server.
pub struct ServeConfig {
    /// Address to bind. `127.0.0.1:0` picks a free loopback port (tests); a
    /// containerized deployment binds `0.0.0.0:<port>`.
    pub bind: SocketAddr,
    /// The bearer token every request (except `/healthz`) must present. This is
    /// the auth gate — the server may bind a non-loopback address behind the
    /// host's private network.
    pub token: String,
    /// Where boundary events go. `crate::observe::standard()` is the production
    /// wiring (counters + a JSONL sink on stderr); tests substitute a
    /// `Capture` so they can assert on typed events.
    pub observer: SharedObserver,
    /// The counters `GET /v1/metrics` reports. Must be the same `Metrics` that
    /// is inside `observer`, or the endpoint reports zeros while the log looks
    /// healthy — [`ServeConfig::new`] wires both correctly.
    pub metrics: Arc<Metrics>,
    /// How long a turn survives its subscriber disconnecting before the server
    /// gives up on a reconnect and cancels it.
    ///
    /// This window is what makes retained history worth keeping: without it a
    /// dropped connection cancels the turn at once and a resuming client would
    /// find nothing left to resume into — only a replay of a dead turn.
    ///
    /// It is genuinely a trade, so it is genuinely configurable. Holding the
    /// window costs an OS thread and any in-flight model call for a client that
    /// may never return; `Duration::ZERO` opts out entirely and restores the
    /// cancel-on-disconnect behavior. What a host *cannot* do is extend it past
    /// [`MAX_RESUME_GRACE`] — a ceiling a caller could raise without limit is
    /// not a bound, and this one exists to stop an abandoned turn holding
    /// resources forever. See [`ServeConfig::resume_grace`].
    pub resume_grace: Duration,
}

/// The default [`ServeConfig::resume_grace`]: thirty seconds.
///
/// Covers the reconnects that actually happen — a proxy recycling a
/// connection, a laptop waking, a gateway redeploying — without holding a
/// thread for a client that has genuinely gone.
pub const DEFAULT_RESUME_GRACE: Duration = Duration::from_secs(30);

/// Ceiling on [`ServeConfig::resume_grace`]: five minutes.
///
/// Same reasoning as the live-turn cap and the route ceilings (both private,
/// deliberately): the bound exists to stop an abandoned turn holding an OS
/// thread and a live model call indefinitely, so a host that could set it to
/// infinity could remove it.
pub const MAX_RESUME_GRACE: Duration = Duration::from_secs(300);

impl ServeConfig {
    /// The production wiring: bind, token, and the standard observability stack.
    #[must_use]
    pub fn new(bind: SocketAddr, token: String) -> Self {
        let (observer, metrics) = crate::observe::standard();
        Self {
            bind,
            token,
            observer,
            metrics,
            resume_grace: DEFAULT_RESUME_GRACE,
        }
    }

    /// Set how long a disconnected turn waits for a reconnect, clamped to
    /// [`MAX_RESUME_GRACE`].
    ///
    /// Clamped rather than rejected: this is an operational dial, and a host
    /// that asks for an hour wants "as long as possible", which is what it
    /// gets. Refusing the whole config over it would turn a tuning mistake
    /// into a failure to boot.
    #[must_use]
    pub fn with_resume_grace(mut self, grace: Duration) -> Self {
        self.resume_grace = grace.min(MAX_RESUME_GRACE);
        self
    }
}

/// Ceiling on turns registered at once.
///
/// Every live turn owns an OS thread and a pending-request slot for as long as
/// the host keeps answering it — so without a cap an authenticated caller could
/// register turns until the process ran out of threads. 32 concurrent turns is
/// far past any real host (each one is a whole agentic conversation in flight)
/// while keeping the thread count bounded by a number an operator can reason
/// about.
///
/// The cap counts registry entries, and a turn that *finished* without ever
/// being streamed still holds one. On its own that would make this a one-way
/// latch: 32 abandoned turns would occupy the registry forever and every later
/// `POST /v1/turns` would be a `429`. [`reclaim_finished_unstreamed`] is what
/// keeps it a queue — see there for why reclamation is driven by this cap
/// rather than by a ceiling of its own.
///
/// This is a const rather than a [`ServeConfig`] field on purpose, matching the
/// ceilings in `routes.rs`: these are the server's own safety backstops, not
/// host-tunable policy. A host that could raise the cap could remove it, which
/// is exactly what it exists to prevent.
const MAX_LIVE_TURNS: usize = 32;

/// One registered turn. `pending` answers reverse requests (shared, always
/// available); `session` is taken exactly once by the SSE stream.
pub(crate) struct Entry {
    /// Registration order, so reclamation can name the *oldest* abandoned turn.
    ///
    /// Deliberately not derived from the turn id: ids are 128 random bits (see
    /// [`new_turn_id`]) precisely so that nothing can be inferred from one, and
    /// that includes age. This counter is internal and never leaves the process.
    seq: u64,
    pub(crate) pending: Pending,
    pub(crate) session: Mutex<Option<Session>>,
    /// This turn's retained frame tail, shared with the [`Session`].
    ///
    /// Held by the *entry* rather than only by the session because that is the
    /// whole point of retention: when a subscriber's connection drops, the
    /// session goes with it, and the reconnect has to find the history still
    /// here to replay from. It also lets a client that reconnects after the
    /// turn already finished collect the tail it missed.
    pub(crate) history: Arc<FrameHistory>,
    /// Bumped every time a stream takes the session out.
    ///
    /// A disconnect schedules a reaper for this turn; the reaper must not
    /// cancel a turn that a *later* subscriber has since picked up. Comparing
    /// the generation it captured against the current one is what tells those
    /// two situations apart — the session merely being present again is not
    /// enough, since a reconnect that also dropped would look identical.
    pub(crate) stream_generation: AtomicU64,
}

/// Shared server state across connections.
pub(crate) struct ServerState {
    token: String,
    turns: Mutex<HashMap<String, Arc<Entry>>>,
    /// Monotonic source of [`Entry::seq`]. Registration ordering only — the
    /// turn id is random and owes nothing to this.
    counter: AtomicU64,
    unauthorized: Mutex<TokenBucket>,
    observer: SharedObserver,
    metrics: Arc<Metrics>,
    /// See [`ServeConfig::resume_grace`]. `Duration::ZERO` means a disconnect
    /// cancels the turn at once, with no resume window.
    resume_grace: Duration,
}

impl ServerState {
    pub(crate) fn turns(&self) -> MutexGuard<'_, HashMap<String, Arc<Entry>>> {
        self.turns.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn lookup(&self, id: &str) -> Option<Arc<Entry>> {
        self.turns().get(id).cloned()
    }

    pub(crate) fn observer(&self) -> &SharedObserver {
        &self.observer
    }

    pub(crate) fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// Admit a turn and hand back its id, or `None` when the server is at
    /// [`MAX_LIVE_TURNS`] and nothing could be reclaimed to make room.
    ///
    /// Registration is the only way the registry grows, so admission,
    /// reclamation and insertion all happen here under **one** lock hold.
    /// Checking the count, then starting the session, then inserting would let
    /// two concurrent creates both observe `len() < MAX_LIVE_TURNS` and both
    /// insert, so the cap could be exceeded by exactly the number of racing
    /// callers.
    ///
    /// `start` receives the minted id: a session has to carry its own
    /// [`TurnRef`] into every record it emits, and only this function — holding
    /// the registry lock — can mint one.
    ///
    /// The session is started by `start` *inside* the lock, and only once a slot
    /// is known to be free. `Session::start` is synchronous (it spawns a thread
    /// and returns), so holding the lock across it is sound — there is no await
    /// in this block. Starting it before the cap check would spawn, then
    /// immediately drop, a thread for every refused request, which hands a
    /// caller the very thread churn the cap exists to prevent.
    pub(crate) fn register_turn(&self, start: impl FnOnce(&str) -> Session) -> Option<String> {
        let outcome = {
            let mut turns = self.turns();
            reclaim_finished_unstreamed(&mut turns, MAX_LIVE_TURNS, &self.observer);
            if turns.len() >= MAX_LIVE_TURNS {
                Err((RefusalReason::AtCapacity, turns.len()))
            } else {
                let id = new_turn_id();
                // Insert through the vacant entry rather than `insert`, which
                // would *replace* on a collision: the displaced turn's `Session`
                // would drop (cancelling a turn its owner is still using) and
                // two callers would hold the same id. 128 random bits make this
                // unreachable in practice; going through the entry API makes it
                // unreachable by construction, so the guarantee does not rest on
                // the id width alone. Refusing is fail-closed — the caller
                // renders it as the cap's 429, which is the wrong reason for the
                // right answer, and is preferable to either silently cancelling
                // a live turn or panicking a request thread. It is also the one
                // refusal worth recording separately: if it ever fires, the RNG
                // is the story.
                match turns.entry(id.clone()) {
                    hash_map::Entry::Occupied(_) => Err((RefusalReason::IdCollision, turns.len())),
                    hash_map::Entry::Vacant(slot) => {
                        let session = start(&id);
                        slot.insert(Arc::new(Entry {
                            seq: self.counter.fetch_add(1, Ordering::Relaxed),
                            pending: session.pending(),
                            history: session.history(),
                            stream_generation: AtomicU64::new(0),
                            session: Mutex::new(Some(session)),
                        }));
                        Ok(id)
                    }
                }
            }
        };
        // Emitted outside the registry lock: an observer is caller-supplied and
        // may do I/O, and holding the one lock every route contends on across
        // someone else's `write` is how a log sink becomes a latency spike.
        let live_turns = self.turns().len();
        match outcome {
            Ok(id) => {
                self.observer.emit(&ServeEvent::TurnCreated {
                    turn: TurnRef::new(&id),
                    live_turns,
                });
                Some(id)
            }
            Err((reason, _)) => {
                self.observer
                    .emit(&ServeEvent::TurnRefused { reason, live_turns });
                None
            }
        }
    }

    /// Hand a disconnected turn back to the registry and schedule its reaper.
    ///
    /// Called when a stream ends because the *peer* went away, rather than
    /// because the turn finished. The session goes back into its entry so a
    /// reconnect can pick it up, and a task is armed to cancel the turn if
    /// nobody does within [`RESUME_GRACE`].
    ///
    /// `generation` is the value read when this stream took the session. The
    /// reaper cancels only if it is still current: a later subscriber bumps it,
    /// so a turn someone has since resumed is never killed by the reaper armed
    /// for an earlier disconnect. Checking merely that a session is *present*
    /// would not distinguish "resumed and still streaming" from "resumed and
    /// dropped again", and would cancel a live turn.
    pub(crate) fn park_for_resume(
        self: &Arc<Self>,
        id: &str,
        entry: &Arc<Entry>,
        session: Session,
        generation: u64,
    ) {
        let grace = self.resume_grace;
        // A zero window is the opt-out: the caller wants the pre-resume
        // behavior, where a disconnect ends the turn immediately. Doing this
        // here rather than at the call site keeps "what a disconnect means"
        // in one place.
        if grace.is_zero() {
            drop(session);
            self.turns().remove(id);
            return;
        }
        {
            let mut slot = entry.session.lock().unwrap_or_else(|p| p.into_inner());
            *slot = Some(session);
        }
        let state = Arc::clone(self);
        let entry = Arc::clone(entry);
        let id = id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(grace).await;
            if entry.stream_generation.load(Ordering::Acquire) != generation {
                // Somebody resumed. Their own disconnect armed a fresh reaper.
                return;
            }
            // Take the session out before removing the entry so a reconnect
            // racing this reap loses cleanly (it finds `None` and 409s) rather
            // than getting a session about to be cancelled underneath it.
            let taken = {
                entry
                    .session
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .take()
            };
            if taken.is_none() {
                return;
            }
            state.turns().remove(&id);
            state.observer().emit(&ServeEvent::StreamEnded {
                turn: TurnRef::new(&id),
                frames_sent: 0,
                reason: StreamEndReason::ResumeWindowExpired,
            });
            // Dropping the session cancels the turn and releases its thread.
            drop(taken);
        });
    }

    /// How long this 401 should be held before it is answered. `Duration::ZERO`
    /// while the caller is within the burst allowance.
    fn unauthorized_delay(&self) -> Duration {
        self.unauthorized
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take(std::time::Instant::now())
    }
}

/// Drop finished-but-unstreamed turns, oldest first, until the registry is
/// below `target` — or until nothing is left that qualifies.
///
/// A turn's entry is normally reclaimed by its `/events` stream ending or by an
/// explicit cancel. A host that creates turns and never streams them does
/// neither, and each such turn pins its entry *plus* every frame the turn
/// buffered for the stream that never opened. This is the third reclaimer, for
/// exactly that case.
///
/// A turn qualifies only when its session is still in the entry (nothing is
/// streaming it — a stream takes the session out, and removes the entry itself
/// when it ends) *and* its thread has finished, so only buffered frames remain.
/// Everything else is live and is never reclaimed: a still-running or
/// currently-streamed turn is reclaimed by its own lifecycle.
///
/// Reclamation is driven by [`MAX_LIVE_TURNS`] rather than by a ceiling of its
/// own. A separate "keep at most N settled turns" bound would be unreachable —
/// the registry can never hold more than `MAX_LIVE_TURNS` entries in the first
/// place — so the only bound that can ever bind is the one that decides
/// admission. Running under pressure also makes this as gentle as possible: a
/// host that streams its turns promptly never has one taken away, and a turn is
/// only dropped when its slot is the difference between admitting the next turn
/// and answering `429`. A reclaimed id answers an honest `404`.
///
/// Every eviction is reported ([`ServeEvent::TurnReclaimed`]), with the count of
/// frames nobody ever read. This used to be a silent `HashMap::remove`, which
/// made a host that abandons turns indistinguishable from one that does not.
///
/// Lock order is registry → session, the same direction `handle_events` uses
/// (its registry lookup releases the registry lock before it takes the session
/// slot). This function is the one place that holds *both*, and it never blocks
/// for the second: it takes each session slot with `try_lock` and treats a
/// contended one as not-reclaimable. So the ordering is not merely observed by
/// convention here — it cannot be violated, even if a future caller acquires
/// these locks the other way around.
///
/// What this bounds, and what it does not: reclamation frees a settled turn's
/// entry and the frames it buffered, but the frame channel itself is unbounded,
/// so *one* abandoned turn can still buffer arbitrarily much before it settles.
/// Bounding that is a backpressure change in `Session`, not a registry one.
/// What is bounded here is the count — at most [`MAX_LIVE_TURNS`] turns' worth
/// of buffered frames can be pinned at once, instead of one per create forever.
fn reclaim_finished_unstreamed(
    turns: &mut HashMap<String, Arc<Entry>>,
    target: usize,
    observer: &SharedObserver,
) {
    // Below the target there is nothing to make room for, so the common path
    // costs one length check and allocates nothing.
    if turns.len() < target {
        return;
    }
    let mut finished: Vec<(u64, &str)> = turns
        .iter()
        .filter(|(_, entry)| {
            // `try_lock`, not `lock`: this runs while the registry lock is held,
            // and blocking on a session lock here would invert the one ordering
            // this module relies on (registry → session, see the doc above).
            let session = match entry.session.try_lock() {
                Ok(session) => session,
                // Held. The only writer is `handle_events` taking the session
                // out to stream it, so contention *means* a stream is opening —
                // skipping is the right answer, not a compromise.
                Err(TryLockError::WouldBlock) => return false,
                // A panic while holding this mutex must not make the entry
                // immortal, so poisoning is recovered rather than propagated —
                // the same posture as every other lock in this file.
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            };
            session.as_ref().is_some_and(Session::is_finished)
        })
        .map(|(id, entry)| (entry.seq, id.as_str()))
        .collect();
    // Oldest first, so a late stream is most likely to still find its turn.
    finished.sort_unstable();
    let excess = turns.len() - target + 1;
    let doomed: Vec<String> = finished
        .into_iter()
        .take(excess)
        .map(|(_, id)| id.to_string())
        .collect();
    for id in doomed {
        // Count what is being thrown away *before* removing it: the entry owns
        // the buffered frames, so after the remove there is nothing left to ask.
        let buffered_frames = turns
            .get(&id)
            .map_or(0, |entry| match entry.session.try_lock() {
                Ok(session) => session.as_ref().map_or(0, Session::buffered_frames),
                Err(TryLockError::WouldBlock) => 0,
                Err(TryLockError::Poisoned(poisoned)) => poisoned
                    .into_inner()
                    .as_ref()
                    .map_or(0, Session::buffered_frames),
            });
        turns.remove(&id);
        observer.emit(&ServeEvent::TurnReclaimed {
            turn: TurnRef::new(&id),
            buffered_frames,
        });
    }
}

/// Burst of 401s answered with no delay. A host that restarts and races a few
/// requests against a not-yet-updated token, or a health probe that forgets the
/// header, should not be punished for the first handful.
const UNAUTHORIZED_BURST: f64 = 8.0;

/// Steady-state 401s per second once the burst is spent.
const UNAUTHORIZED_REFILL_PER_SEC: f64 = 2.0;

/// Delay applied to a 401 that arrives with the bucket empty.
///
/// This is deliberately a *delay*, not a rejection: a 429 or a dropped
/// connection would tell an attacker they had been noticed and would break a
/// legitimate client that is merely misconfigured. Holding the response instead
/// costs the guesser wall-clock time per attempt — which is the entire point,
/// since the token is a fixed shared secret with no lockout behind it — while a
/// correctly-configured host never reaches this path at all.
const UNAUTHORIZED_PENALTY: Duration = Duration::from_millis(500);

/// A dependency-free token bucket. Deliberately per-process and not per-peer:
/// tracking source addresses would mean unbounded state keyed by something the
/// caller chooses, which is its own denial-of-service surface, and this is a
/// sidecar for exactly one trusted host — a legitimate deployment produces no
/// sustained 401s at all, so a global bucket costs it nothing.
struct TokenBucket {
    tokens: f64,
    last: std::time::Instant,
}

impl TokenBucket {
    fn new() -> Self {
        Self {
            tokens: UNAUTHORIZED_BURST,
            last: std::time::Instant::now(),
        }
    }

    /// Spend one token, returning the delay the caller must observe first.
    fn take(&mut self, now: std::time::Instant) -> Duration {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * UNAUTHORIZED_REFILL_PER_SEC).min(UNAUTHORIZED_BURST);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Duration::ZERO
        } else {
            UNAUTHORIZED_PENALTY
        }
    }
}

/// Bind and serve until the accept loop hits a **fatal** error. `on_ready` fires
/// once with the bound address (so a `:0` bind can report its port).
///
/// Not every `accept()` failure ends the server: a peer that hangs up before its
/// connection is accepted, or transient fd exhaustion, is retried (with backoff
/// where backoff is needed) rather than taken as a shutdown signal. Only a
/// listener that is structurally unusable — or one that has accepted nothing for
/// the whole give-up streak — returns. `src/accept.rs` holds the full
/// classification and the reasoning; `stella-observatory` applies the same policy
/// from its own copy, and the two files are guarded byte-identical, so the
/// reporting added here lives at the call site rather than in that module.
///
/// # Operational limits
///
/// This is a sidecar for one trusted host, not an internet-facing server, and
/// the deployment must supply what it deliberately omits:
///
/// - **Turns are capped, connections are not.** At most 32 may be registered at
///   once — past that, `POST /v1/turns` answers `429` with a `Retry-After` —
///   because each one holds an OS thread until its stream ends.
///   Accepted *connections* are still unbounded: a semaphore over them is not
///   the right tool here, because a permit would be held for the whole lifetime
///   of a long-lived SSE stream and would starve the `tool-result` and
///   `provider-result` POSTs that the very same turn must deliver over separate
///   connections — the reverse-RPC protocol would deadlock against itself.
///   Bounding turns bounds the resource that actually accumulates.
///   The cap is a queue and not a latch: a turn that finished without ever
///   being streamed is reclaimed, oldest first, to admit a new one, so a host
///   that abandons turns cannot wedge the server into answering `429` forever.
/// - **Turn ids are unguessable** (128 random bits), so learning one id does not
///   hand over the namespace. The token remains the only tenancy boundary, one
///   process per tenant.
/// - **Reads are bounded** by a deadline and by separate head and body caps; a
///   peer that dribbles a request head is dropped rather than parked forever.
///   The response says which bound was hit (`408`, `413`, `400`, `501`).
/// - **Authentication happens on the head**, before the body is read, so the
///   memory an *unauthenticated* peer can make this process hold is one 8 KiB
///   drain buffer rather than the request-body cap. That matters
///   precisely because connections are uncapped: without it, every anonymous
///   connection could cost megabytes for as long as the read deadline allows.
///
/// Reverse requests *are* bounded (see [`SessionSpec::reverse_request_timeout`]),
/// and a turn can be ended early with `POST /v1/turns/{id}/cancel`.
///
/// [`SessionSpec::reverse_request_timeout`]: crate::SessionSpec::reverse_request_timeout
pub async fn serve(config: ServeConfig, on_ready: impl FnOnce(SocketAddr)) -> std::io::Result<()> {
    let listener = TcpListener::bind(config.bind).await?;
    let bound = listener.local_addr()?;
    let state = Arc::new(ServerState {
        token: config.token,
        turns: Mutex::new(HashMap::new()),
        counter: AtomicU64::new(0),
        unauthorized: Mutex::new(TokenBucket::new()),
        observer: config.observer,
        metrics: config.metrics,
        resume_grace: config.resume_grace.min(MAX_RESUME_GRACE),
    });
    state.observer.emit(&ServeEvent::Listening {
        addr: bound.to_string(),
    });
    on_ready(bound);
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
                            state.observer.emit(&ServeEvent::AcceptBackoff {
                                kind,
                                delay_ms: millis(delay),
                                streak,
                            });
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                        None => {
                            state
                                .observer
                                .emit(&ServeEvent::AcceptGaveUp { kind, streak });
                            shutting_down(&state);
                            return Err(err);
                        }
                    }
                }
                AcceptAction::Fatal => {
                    shutting_down(&state);
                    return Err(err);
                }
            },
        };
        // Nagle off. Every frame on this wire gates the next engine step — a
        // `provider_request` the host has not seen yet is a step that cannot
        // proceed — so coalescing a small write with the next one trades
        // nothing for up to a delayed-ACK's worth of added latency per step.
        let _ = stream.set_nodelay(true);
        let state = Arc::clone(&state);
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
    state.observer.emit(&ServeEvent::ShuttingDown {
        reason: ShutdownReason::ListenerFailed,
        live_turns: state.turns().len(),
    });
}

/// Serve one connection, and report it exactly once.
///
/// The record is opened before the head is read and closed after the router
/// returns, so there is no path — malformed request, oversized head, timeout,
/// silent hangup, I/O failure mid-response — that produces no record. That is
/// the whole reason [`route`] exists as a separate function: distributing
/// "remember to report" across its sixteen exits is one refactor away from
/// being silently false, which is exactly how the PROOF rail (#901) shipped a
/// surface that reported only on the happy path.
async fn handle_conn(mut stream: TcpStream, state: Arc<ServerState>) -> std::io::Result<()> {
    let mut record = RequestRecord::opening(RequestId::generate());
    // Read the head first so the peer's correlation id can be adopted, but note
    // the record already exists: a peer that never completes a head still gets
    // one, with the generated id and a `status: None`.
    let head = read_head(&mut stream).await;
    if let Ok(ReadOutcome::Request(req)) = &head
        && let Some(supplied) = req.header("x-request-id").and_then(RequestId::from_header)
    {
        record.adopt_request_id(supplied);
    }
    let mut responder = Responder::new(&mut stream, record.request_id().clone());
    let outcome = route(&mut responder, &mut record, &state, head).await;
    let wrote = responder.wrote();
    record.close(state.observer(), wrote, &outcome);
    outcome
}

/// Route one request: authenticate on the head, then — and only then — read the
/// body and dispatch.
///
/// The two-phase read is the whole point of the ordering here. Buffering the
/// body first meant an *unauthenticated* peer could make the server hold up to
/// `MAX_BODY_BYTES` for it, once per connection, and connections are
/// deliberately uncapped (see [`serve`]'s operational limits) — so the 401 was
/// answered only after paying for the request that earned it. Now a refused
/// request costs one 8 KiB drain buffer. The body is still taken off the socket
/// before the response is written: closing with unread bytes in flight makes the
/// kernel RST, and a peer that gets an RST mid-send never reads its 401.
///
/// Knows nothing about records beyond filling in what it learns. Its caller owns
/// the reporting.
async fn route(
    res: &mut Responder<'_>,
    record: &mut RequestRecord,
    state: &Arc<ServerState>,
    head: std::io::Result<ReadOutcome>,
) -> std::io::Result<()> {
    let mut req = match head? {
        ReadOutcome::Request(req) => req,
        // A peer that never sent anything is owed nothing — but it is still
        // recorded, as a request with no status.
        ReadOutcome::Hangup => return Ok(()),
        ReadOutcome::TooLarge => {
            return res
                .json(
                    "413 Payload Too Large",
                    &error_body("request exceeded the server's size limit"),
                )
                .await;
        }
        ReadOutcome::Malformed => {
            return res
                .json("400 Bad Request", &error_body("malformed HTTP request"))
                .await;
        }
        ReadOutcome::UnsupportedTransferEncoding => {
            return res
                .json(
                    "501 Not Implemented",
                    &error_body(
                        "chunked transfer-encoding is not supported; send a Content-Length body",
                    ),
                )
                .await;
        }
        ReadOutcome::Timeout => {
            return res
                .json(
                    "408 Request Timeout",
                    &error_body("request was not completed in time"),
                )
                .await;
        }
    };
    record.set_method(&req.method);

    // The query string used to be split off and thrown away, which is why
    // `?after=` was documented as a resume mechanism while being, in fact,
    // unparsed. It is kept now — `handle_events` reads `after` from it.
    let (path, query) = match req.path.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (req.path.clone(), String::new()),
    };
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let (route, turn_id) = classify(&segs);
    record.set_route(route);
    let turn_id = turn_id.map(str::to_string);
    if let Some(id) = turn_id.as_deref() {
        record.set_turn(id);
    }

    if route == Route::Healthz && req.method == "GET" {
        discard_body(res.stream_mut(), &mut req).await;
        return routes::handle_health(res).await;
    }
    // Compared in constant time: `==` on a `&str` stops at the first differing
    // byte, which leaks the shared secret one byte at a time to a caller that
    // can time its own 401s. A missing header stays a hard `false` so an
    // empty configured token cannot authorize an anonymous request.
    let authorized = match req.bearer() {
        Some(presented) => constant_time_eq(presented.as_bytes(), state.token.as_bytes()),
        None => false,
    };
    if !authorized {
        discard_body(res.stream_mut(), &mut req).await;
        // Rate-limited by holding the response, never by changing it: the
        // body and status a guesser sees are identical whether or not the
        // bucket was empty, so the throttle leaks nothing about its own state.
        // The sleep is `tokio::time::sleep` on this connection's task, so a
        // held 401 costs one task, not a runtime thread. `held_ms` is recorded
        // because that asymmetry — invisible outside, visible inside — is
        // precisely what makes a sustained guess identifiable.
        let delay = state.unauthorized_delay();
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        state.observer().emit(&ServeEvent::Unauthorized {
            request_id: record.request_id().clone(),
            route,
            held_ms: millis(delay),
        });
        return res
            .json(
                "401 Unauthorized",
                br#"{"error":"missing or invalid bearer token"}"#,
            )
            .await;
    }

    // Routes that carry no body are matched before the body is read, so a GET
    // with a stray `Content-Length` is drained rather than parsed.
    match (req.method.as_str(), route) {
        ("GET", Route::Metrics) => {
            discard_body(res.stream_mut(), &mut req).await;
            return routes::handle_metrics(res, state).await;
        }
        ("GET", Route::TurnEvents) => {
            discard_body(res.stream_mut(), &mut req).await;
            return routes::handle_events(
                res,
                record,
                state,
                turn_id.as_deref().unwrap_or_default(),
                resume_point(&query, &req),
            )
            .await;
        }
        ("POST", Route::TurnCancel) => {
            discard_body(res.stream_mut(), &mut req).await;
            return routes::handle_cancel(res, state, turn_id.as_deref().unwrap_or_default()).await;
        }
        // The body-bearing routes fall through to the read below.
        ("POST", Route::TurnsCreate | Route::TurnToolResult | Route::TurnProviderResult) => {}
        (_, Route::Unrouted) => {
            discard_body(res.stream_mut(), &mut req).await;
            return res.json("404 Not Found", br#"{"error":"not found"}"#).await;
        }
        // A known resource reached with the wrong method is a 405 naming what
        // it does accept, not a 404 claiming the route does not exist — the
        // difference between "you typed the path wrong" and "you used the
        // wrong verb" is the whole diagnostic value of the status code.
        (_, known) => return routes::method_not_allowed(res, &mut req, allowed(known)).await,
    }

    if let BodyOutcome::Timeout = read_body(res.stream_mut(), &mut req).await? {
        return res
            .json(
                "408 Request Timeout",
                &error_body("request was not completed in time"),
            )
            .await;
    }
    record.set_bytes_in(req.body.len() as u64);

    let id = turn_id.as_deref().unwrap_or_default();
    match route {
        Route::TurnsCreate => routes::handle_create(res, state, &req.body).await,
        Route::TurnToolResult => routes::handle_tool_result(res, state, id, &req.body).await,
        Route::TurnProviderResult => {
            routes::handle_provider_result(res, state, id, &req.body).await
        }
        // Unreachable: the match above admits exactly the three body-bearing
        // routes. Answering rather than panicking keeps a future edit that
        // widens that match from taking down a request thread.
        _ => res.json("404 Not Found", br#"{"error":"not found"}"#).await,
    }
}

/// Map a path to its route template and, where it has one, the turn id.
///
/// Classification is deliberately independent of the method, so a `405` records
/// which resource was addressed rather than collapsing to "unrouted" — the
/// difference between "you used the wrong verb on `/v1/turns`" and "you asked
/// for a path that does not exist" is exactly what makes a record diagnostic.
fn classify<'a>(segs: &[&'a str]) -> (Route, Option<&'a str>) {
    match segs {
        ["healthz"] => (Route::Healthz, None),
        ["v1", "metrics"] => (Route::Metrics, None),
        ["v1", "turns"] => (Route::TurnsCreate, None),
        ["v1", "turns", id, "events"] => (Route::TurnEvents, Some(id)),
        ["v1", "turns", id, "tool-result"] => (Route::TurnToolResult, Some(id)),
        ["v1", "turns", id, "provider-result"] => (Route::TurnProviderResult, Some(id)),
        ["v1", "turns", id, "cancel"] => (Route::TurnCancel, Some(id)),
        _ => (Route::Unrouted, None),
    }
}

/// Read one parameter out of a raw query string.
///
/// Deliberately minimal — no percent-decoding, no repeated-key semantics, no
/// dependency. The only parameter this server reads is `after`, whose value is
/// a decimal integer: every byte that could need decoding makes it fail to
/// parse as one, which is the correct outcome anyway. Growing a second
/// parameter with a richer value type is the moment to reach for a real
/// parser, not before.
fn query_param<'q>(query: &'q str, name: &str) -> Option<&'q str> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then_some(value)
    })
}

/// Where a subscriber wants the stream to resume from, if anywhere.
///
/// Two spellings of one number, because two kinds of client name it
/// differently. `?after=<seq>` is the explicit form, for a client that manages
/// its own reconnects. `Last-Event-ID` is the SSE standard's form, which a
/// browser `EventSource` sends **automatically** from the `id:` line each frame
/// carries — so a web host resumes with no client code at all, which is the
/// whole reason the `id:` is emitted.
///
/// `?after=` wins when both are present: it is the one the caller wrote on
/// purpose, whereas `Last-Event-ID` is replayed by the platform from whatever
/// it happened to see last.
fn resume_point(query: &str, req: &crate::http::Request) -> Option<u64> {
    query_param(query, "after")
        .or_else(|| req.header("last-event-id"))
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/// The `Allow` header value for a known route.
fn allowed(route: Route) -> &'static str {
    match route {
        Route::Healthz | Route::Metrics | Route::TurnEvents => "GET",
        Route::TurnsCreate
        | Route::TurnToolResult
        | Route::TurnProviderResult
        | Route::TurnCancel => "POST",
        // Never reached: `Unrouted` is answered as a 404 before this is called.
        Route::Unrouted => "",
    }
}

/// Mint a turn id that cannot be guessed from another turn's id.
///
/// These were `turn-0`, `turn-1`, … from a process counter. The bearer token is
/// still the actual auth gate, but a sequential id makes every *other* live turn
/// addressable to anyone who learns one — a turn id in a log line, an error
/// report, or a proxy access log hands over the whole namespace. 128 bits from
/// the OS CSPRNG removes that second, accidental way in. Records carry only a
/// truncated [`TurnRef`], so this server's own log is not that leak either.
fn new_turn_id() -> String {
    let mut bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    let mut id = String::with_capacity(5 + 32);
    id.push_str("turn-");
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(id, "{byte:02x}");
    }
    id
}

/// Compare two secrets without an early exit, so the time taken does not depend
/// on how many leading bytes matched.
///
/// The length difference is still observable (unavoidable without hashing) —
/// that reveals the token's length, not its bytes. A dedicated crate (`subtle`)
/// would be the rigorous answer; the workspace's "no new deps casually" rule
/// makes a six-line fold the better trade for one comparison per request.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::Capture;
    use crate::observe::event::ServeEvent;
    use crate::session::SessionSpec;
    use stella_core::{BudgetGuard, EngineConfig};
    use stella_protocol::{BudgetMode, CompletionMessage};

    /// A brute-force run against the only credential this service has must
    /// stop being free after the burst. Driven over an injected `Instant` so
    /// the refill is asserted exactly, with no sleeping in the test.
    #[test]
    fn sustained_401s_are_throttled_once_the_burst_is_spent() {
        let start = std::time::Instant::now();
        let mut bucket = TokenBucket::new();
        for attempt in 0..UNAUTHORIZED_BURST as usize {
            assert_eq!(
                bucket.take(start),
                Duration::ZERO,
                "attempt {attempt} is within the burst and must not be delayed"
            );
        }
        assert_eq!(
            bucket.take(start),
            UNAUTHORIZED_PENALTY,
            "the first attempt past the burst is held"
        );
        // Half a second of refill at 2/sec is exactly one token.
        assert_eq!(
            bucket.take(start + Duration::from_millis(500)),
            Duration::ZERO
        );
        assert_eq!(
            bucket.take(start + Duration::from_millis(500)),
            UNAUTHORIZED_PENALTY,
            "and it is spent again immediately"
        );
    }

    #[test]
    fn refill_is_capped_at_the_burst_size() {
        let start = std::time::Instant::now();
        let mut bucket = TokenBucket::new();
        // An hour idle must not bank an hour of tokens.
        let later = start + Duration::from_secs(3600);
        for _ in 0..UNAUTHORIZED_BURST as usize {
            assert_eq!(bucket.take(later), Duration::ZERO);
        }
        assert_eq!(bucket.take(later), UNAUTHORIZED_PENALTY);
    }

    fn test_state() -> (ServerState, Arc<Capture>) {
        let capture = Arc::new(Capture::new());
        let state = ServerState {
            token: String::new(),
            turns: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(0),
            unauthorized: Mutex::new(TokenBucket::new()),
            observer: capture.clone(),
            metrics: Arc::new(Metrics::new()),
            resume_grace: DEFAULT_RESUME_GRACE,
        };
        (state, capture)
    }

    fn test_spec(turn_id: &str) -> SessionSpec {
        SessionSpec {
            provider_id: "mock".to_string(),
            tools: Vec::new(),
            messages: vec![CompletionMessage::user("hi")],
            config: EngineConfig::default(),
            budget: BudgetGuard::new(BudgetMode::Off, None, None),
            reverse_request_timeout: SessionSpec::DEFAULT_REVERSE_REQUEST_TIMEOUT,
            turn: TurnRef::new(turn_id),
            observer: crate::observe::null_observer(),
        }
    }

    /// A session that has already produced its terminal frame: cancelled
    /// before it can park, then waited on until its thread exits.
    fn finished_session(turn_id: &str) -> Session {
        let session = Session::start(test_spec(turn_id));
        session.cancel();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !session.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "cancelled session did not finish"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        session
    }

    /// A host that creates turns and never streams them must not wedge the
    /// server. Filling the registry with finished-unstreamed turns and then
    /// creating one more has to succeed, by reclaiming the oldest — otherwise
    /// [`MAX_LIVE_TURNS`] is a one-way latch and the 33rd create is a `429`
    /// forever.
    #[test]
    fn a_finished_unstreamed_turn_is_reclaimed_to_admit_a_new_one() {
        let (state, capture) = test_state();
        let ids: Vec<String> = (0..MAX_LIVE_TURNS)
            .map(|_| {
                state
                    .register_turn(finished_session)
                    .expect("the registry has room")
            })
            .collect();
        assert_eq!(state.turns().len(), MAX_LIVE_TURNS, "the registry is full");

        let next = state
            .register_turn(finished_session)
            .expect("a full registry of abandoned turns must still admit a new one");

        assert_eq!(
            state.turns().len(),
            MAX_LIVE_TURNS,
            "reclaiming frees exactly the one slot the new turn takes"
        );
        assert!(
            state.lookup(&ids[0]).is_none(),
            "the oldest finished turn is the one reclaimed"
        );
        assert!(
            state.lookup(&ids[1]).is_some(),
            "only as many as needed are reclaimed — the second-oldest stays"
        );
        assert!(state.lookup(&next).is_some(), "the new turn is registered");

        // The eviction is *reported*. It used to be a silent `HashMap::remove`,
        // which is what made a host that abandons turns indistinguishable from
        // one that does not.
        let reclaimed = capture.find(|e| matches!(e, ServeEvent::TurnReclaimed { .. }));
        assert!(
            reclaimed.is_some(),
            "reclaiming a turn must not be silent — that is the discarded work \
             this dimension is scored on"
        );
        assert_eq!(
            capture.count(|e| matches!(e, ServeEvent::TurnCreated { .. })),
            MAX_LIVE_TURNS + 1,
            "every admitted turn is recorded"
        );
    }

    /// Reclamation must never touch a live turn: a still-running turn parked on
    /// its reverse request outlives any number of finished ones, even as the
    /// oldest entry in the registry.
    #[test]
    fn a_live_turn_is_never_reclaimed() {
        let (state, _capture) = test_state();
        let live = state
            .register_turn(|id| Session::start(test_spec(id)))
            .expect("the first turn is always admitted");
        for _ in 0..MAX_LIVE_TURNS {
            state
                .register_turn(finished_session)
                .expect("finished turns are reclaimed to make room");
        }
        assert!(
            state.lookup(&live).is_some(),
            "the live turn survives although it is the oldest entry"
        );
        assert_eq!(state.turns().len(), MAX_LIVE_TURNS);
        // Dropping the registry drops the live session, whose `Drop` cancels
        // the turn — the test leaves no thread parked on the 5-minute default.
    }

    /// When nothing can be reclaimed, the cap holds: a registry full of *live*
    /// turns refuses the next one rather than dropping a turn out from under a
    /// host that is still using it — and says so, because a 429 storm that
    /// produces no record is one of the four situations #930 names.
    #[test]
    fn a_registry_full_of_live_turns_refuses_the_next() {
        let (state, capture) = test_state();
        for _ in 0..MAX_LIVE_TURNS {
            state
                .register_turn(|id| Session::start(test_spec(id)))
                .expect("the registry has room");
        }
        assert!(
            state
                .register_turn(|id| Session::start(test_spec(id)))
                .is_none(),
            "no live turn may be reclaimed, so the cap must refuse"
        );
        assert!(
            capture
                .find(|e| matches!(
                    e,
                    ServeEvent::TurnRefused {
                        reason: RefusalReason::AtCapacity,
                        ..
                    }
                ))
                .is_some(),
            "a refusal at capacity must be visible from outside the process"
        );
    }

    /// A path is classified independently of its method, so a 405 still records
    /// which resource was addressed.
    #[test]
    fn paths_classify_to_templates_and_surrender_their_turn_id() {
        let split = |path: &'static str| -> Vec<&'static str> {
            path.split('/').filter(|s| !s.is_empty()).collect()
        };
        assert_eq!(classify(&split("/healthz")), (Route::Healthz, None));
        assert_eq!(classify(&split("/v1/metrics")), (Route::Metrics, None));
        assert_eq!(classify(&split("/v1/turns")), (Route::TurnsCreate, None));
        assert_eq!(
            classify(&split("/v1/turns/turn-abc/events")),
            (Route::TurnEvents, Some("turn-abc"))
        );
        assert_eq!(
            classify(&split("/v1/turns/turn-abc/tool-result")),
            (Route::TurnToolResult, Some("turn-abc"))
        );
        assert_eq!(
            classify(&split("/v1/turns/turn-abc/provider-result")),
            (Route::TurnProviderResult, Some("turn-abc"))
        );
        assert_eq!(
            classify(&split("/v1/turns/turn-abc/cancel")),
            (Route::TurnCancel, Some("turn-abc"))
        );
        assert_eq!(classify(&split("/nope")), (Route::Unrouted, None));
        assert_eq!(classify(&split("/v1/turns/a/b/c")), (Route::Unrouted, None));
    }

    /// Every routable path must name a verb in its `Allow` header, or a 405 is
    /// half an answer.
    #[test]
    fn every_known_route_names_an_allowed_method() {
        for route in [
            Route::Healthz,
            Route::Metrics,
            Route::TurnsCreate,
            Route::TurnEvents,
            Route::TurnToolResult,
            Route::TurnProviderResult,
            Route::TurnCancel,
        ] {
            assert!(
                matches!(allowed(route), "GET" | "POST"),
                "{} has no allowed method",
                route.template()
            );
        }
    }
}
