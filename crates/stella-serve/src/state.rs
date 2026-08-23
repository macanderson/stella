// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The turn registry and the state every connection shares.
//!
//! Split out of `src/server.rs` (#3734), which was eight lines under the
//! gate's 1500-line ceiling with no baseline entry — so the next non-trivial
//! change to it would have failed `make gate` outright, with no grandfather
//! available (`scripts/check-file-size.sh --update` refuses a first-time
//! crossing, #3441). `src/routes.rs` was carved out of the same file earlier
//! for the same reason and is the precedent this follows.
//!
//! The seam is what `server.rs`'s own module doc already drew: that file is
//! **the transport** — listener, connection fold, router, throttle — and this
//! is what the transport serves out of. [`ServerState`] is one process's whole
//! shared state, and the turn registry inside it is the part with rules:
//! admission under [`MAX_LIVE_TURNS`], reclamation of a finished turn nobody
//! ever streamed, and the unguessable id a registration mints. Those three
//! move together because they are one invariant — the cap is only a queue
//! rather than a one-way latch because reclamation runs inside the same lock
//! hold that admits.
//!
//! Everything here is `pub(crate)`: nothing in this module is part of the
//! crate's public surface, which is `serve`, `serve_until` and the types a
//! host builds a [`ServeConfig`](crate::ServeConfig) out of.

use std::collections::HashMap;
use std::collections::hash_map;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::Duration;

use rand::Rng as _;

use crate::history::FrameHistory;
use crate::lifecycle::Lifecycle;
use crate::observe::event::{RefusalReason, ServeEvent, StreamEndReason, TurnRef};
use crate::observe::{Metrics, SharedObserver};
use crate::pending::Pending;
use crate::server::{MAX_RESUME_GRACE, ServeConfig};
use crate::session::Session;
use crate::throttle::TokenBucket;

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
    /// Mid-turn controls (pause/resume/steer) — like `pending`, the shared,
    /// always-available handle: the session itself is exclusively owned by
    /// whichever stream is running, so control POSTs go through this instead.
    pub(crate) controls: crate::controls::Controls,
    /// The turn's step-boundary stop signal (#1129). Held here for the same
    /// reason as `pending` and `controls`: `handle_cancel` cannot reach the
    /// session object while a stream has it checked out, and this is the
    /// signal that interrupts a step which is computing rather than waiting.
    pub(crate) cancel: stella_engine::CancelToken,
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
    /// The server-owned conversations (`/v1/sessions`, #931). A separate
    /// registry from `turns` on purpose: a session is retained state, a turn
    /// is live work, and they are capped, reclaimed and locked independently.
    sessions: crate::sessions::SessionRegistry,
    /// See [`ServeConfig::session_idle_ttl`].
    session_idle_ttl: Duration,
    /// The compiled `Host` guard, consulted before any route dispatch (#1130).
    host_policy: crate::hostguard::HostPolicy,
    /// Ready/draining latches behind `GET /readyz` and the shutdown drain
    /// (#1131).
    lifecycle: Lifecycle,
    /// See [`ServeConfig::checkpoints`]. `None` leaves every turn's
    /// `checkpoint_sink` unset, which is exactly today's behavior.
    checkpoints: Option<Arc<dyn crate::checkpoint::CheckpointStore>>,
    /// See [`ServeConfig::sub_agents`] — the operator's ceilings on what a
    /// turn's children may do (#1297).
    sub_agents: crate::SubAgentPolicy,
    /// See [`ServeConfig::extensions`]. Cloned onto every `SessionSpec` — one
    /// `Arc` bump per turn, and the same handler set for every turn, which is
    /// what makes "the operator decides" checkable rather than per-request.
    extensions: crate::extensions::Extensions,
    /// Token-drift state, shared across every turn in the process (#1298).
    ///
    /// Server-owned rather than session-owned because calibration is only
    /// useful across turns — a per-turn map would be discarded before it
    /// cleared its sample floor — and because `GET /v1/calibration` has to
    /// read it from outside any turn.
    calibration: crate::calibration::CalibrationRegistry,
}

impl ServerState {
    /// Everything one process shares, built from the operator's config and the
    /// host guard compiled against the address actually bound.
    ///
    /// Takes the config by value because it is consumed: a second state built
    /// from the same config would be a second registry answering the same
    /// token, which is not a shape this server has. The two ceilings the
    /// config only *proposes* are clamped here rather than at the call site,
    /// so a host cannot raise them by constructing state another way.
    pub(crate) fn new(config: ServeConfig, host_policy: crate::hostguard::HostPolicy) -> Self {
        Self {
            token: config.token,
            turns: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(0),
            unauthorized: Mutex::new(TokenBucket::new()),
            observer: config.observer,
            metrics: config.metrics,
            resume_grace: config.resume_grace.min(MAX_RESUME_GRACE),
            sessions: crate::sessions::SessionRegistry::new(),
            session_idle_ttl: config.session_idle_ttl,
            checkpoints: config.checkpoints.clone(),
            sub_agents: config.sub_agents,
            extensions: config.extensions,
            calibration: crate::calibration::CalibrationRegistry::new(),
            host_policy,
            lifecycle: Lifecycle::new(),
        }
    }

    /// Whether `presented` is this server's bearer token, compared without an
    /// early exit ([`constant_time_eq`]).
    ///
    /// A method rather than a `token()` getter: handing the secret out is one
    /// careless `format!` away from a log line, and the only thing any caller
    /// wants is the answer.
    pub(crate) fn token_matches(&self, presented: &str) -> bool {
        constant_time_eq(presented.as_bytes(), self.token.as_bytes())
    }

    /// Whether the compiled `Host` guard admits this request's `Host` header
    /// (#1130). Consulted before any route dispatch.
    pub(crate) fn permits_host(&self, header: Option<&str>) -> bool {
        self.host_policy.permits(header)
    }

    pub(crate) fn turns(&self) -> MutexGuard<'_, HashMap<String, Arc<Entry>>> {
        self.turns.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn sessions(&self) -> &crate::sessions::SessionRegistry {
        &self.sessions
    }

    pub(crate) fn session_idle_ttl(&self) -> Duration {
        self.session_idle_ttl
    }

    /// This deployment's sub-agent ceilings (#1297). Every `sub_agents` block
    /// on a turn request passes through `SubAgentPolicy::clamp` before it can
    /// affect anything.
    pub(crate) fn sub_agent_policy(&self) -> crate::SubAgentPolicy {
        self.sub_agents
    }

    /// The configured checkpoint store, if this deployment has one.
    pub(crate) fn checkpoints(&self) -> Option<&Arc<dyn crate::checkpoint::CheckpointStore>> {
        self.checkpoints.as_ref()
    }

    /// The operator's hook extensions, for the `SessionSpec` of a turn about
    /// to start.
    pub(crate) fn extensions(&self) -> crate::extensions::Extensions {
        self.extensions.clone()
    }

    /// This process's token-drift state — read by `GET /v1/calibration`,
    /// written by every committed step of every turn.
    pub(crate) fn calibration(&self) -> &crate::calibration::CalibrationRegistry {
        &self.calibration
    }

    /// This turn's or session's durable identity, or `None` when durability is
    /// off or `id` is not a legal key.
    pub(crate) fn checkpoint_for(&self, id: &str) -> Option<crate::checkpoint::TurnCheckpoint> {
        crate::checkpoint::TurnCheckpoint::for_id(self.checkpoints()?, id)
    }

    pub(crate) fn lookup(&self, id: &str) -> Option<Arc<Entry>> {
        self.turns().get(id).cloned()
    }

    pub(crate) fn observer(&self) -> &SharedObserver {
        &self.observer
    }

    pub(crate) fn lifecycle(&self) -> &Lifecycle {
        &self.lifecycle
    }

    /// Clones of every live turn's controls, taken under one lock and handed
    /// back released.
    ///
    /// Cloning rather than acting under the lock is required: a settling turn
    /// takes this same lock on its way out, so a drain that held it while
    /// waiting for turns to settle would be waiting on work it was itself
    /// blocking.
    pub(crate) fn live_controls(&self) -> Vec<crate::controls::Controls> {
        self.turns()
            .values()
            .map(|entry| entry.controls.clone())
            .collect()
    }

    /// Every live turn's registry entry, same locking discipline as
    /// [`ServerState::live_controls`].
    pub(crate) fn live_entries(&self) -> Vec<Arc<Entry>> {
        self.turns().values().map(Arc::clone).collect()
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
                            controls: session.controls(),
                            cancel: session.cancel_token(),
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
    /// nobody does within the configured `resume_grace` (see [`DEFAULT_RESUME_GRACE`](crate::DEFAULT_RESUME_GRACE)).
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
    pub(crate) fn unauthorized_delay(&self) -> Duration {
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
    use crate::server::{DEFAULT_RESUME_GRACE, DEFAULT_SESSION_IDLE_TTL};
    use crate::session::SessionSpec;
    use stella_core::{BudgetGuard, EngineConfig};
    use stella_protocol::{BudgetMode, CompletionMessage};

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
            sessions: crate::sessions::SessionRegistry::new(),
            session_idle_ttl: DEFAULT_SESSION_IDLE_TTL,
            host_policy: crate::hostguard::HostPolicy::new(
                "127.0.0.1:8080"
                    .parse()
                    .expect("a literal loopback address"),
                &[],
            ),
            // Ready, not draining: the unit tests below exercise routing and
            // the turn registry, neither of which is about the drain.
            lifecycle: {
                let lifecycle = Lifecycle::new();
                lifecycle.mark_ready();
                lifecycle
            },
            checkpoints: None,
            sub_agents: crate::SubAgentPolicy::default(),
            extensions: crate::extensions::Extensions::new(),
            calibration: crate::calibration::CalibrationRegistry::new(),
        };
        (state, capture)
    }

    fn test_spec(turn_id: &str) -> SessionSpec {
        SessionSpec {
            provider_id: "mock".to_string(),
            tools: Vec::new(),
            principal: stella_core::ports::Principal::Host("test".to_string()),
            gate: SessionSpec::default_gate(),
            messages: vec![CompletionMessage::user("hi")],
            config: EngineConfig::default(),
            budget: BudgetGuard::new(BudgetMode::Off, None, None),
            reverse_request_timeout: SessionSpec::DEFAULT_REVERSE_REQUEST_TIMEOUT,
            turn: TurnRef::new(turn_id),
            observer: crate::observe::null_observer(),
            on_settled: None,
            goal: None,
            sub_agents: None,
            checkpoint: None,
            extensions: crate::extensions::Extensions::new(),
            calibration: None,
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
}
