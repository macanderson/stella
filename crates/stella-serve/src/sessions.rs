// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Server-owned conversations: the `/v1/sessions` resource (#931).
//!
//! Without this, `stella-serve`'s only resource is a single turn, so a host
//! running a multi-turn interaction re-sends the entire message history on
//! every create — which makes prompt-cache stability the host's problem,
//! makes the engine's compaction unreachable, and fragments cost accounting
//! into unrelated turns. A session moves the transcript to the server side:
//! the host sends only each turn's new input, and the server threads the one
//! `Vec<CompletionMessage>` through every turn.
//!
//! # What a session is
//!
//! A [`SessionEntry`] owns three things between turns:
//!
//! - **The history.** Message index 0 is the system prompt, minted once at
//!   `POST /v1/sessions` and never rewritten — that is the byte-stable prefix
//!   every provider's cache breakpoint sits behind, and the whole reason a
//!   server-owned transcript earns the cache discount an API consumer cannot
//!   get by re-sending messages itself. Compaction needs nothing extra: it
//!   runs inside `Engine::run_turn`, so threading the same vec through each
//!   turn is sufficient.
//! - **The budget guard.** One [`BudgetGuard`] per session, `begin_turn()` at
//!   each turn, so `session_limit_usd` and `session_spent_usd` finally mean
//!   what they say — the stateless turn route builds a fresh guard per turn,
//!   which reduces the session axis to a second turn limit.
//! - **The counts.** Turns completed and aborted, for `GET /v1/sessions/{id}`.
//!
//! A turn *inside* a session is an ordinary turn in the turn registry: it
//! counts against `MAX_LIVE_TURNS`, streams over `/v1/turns/{id}/events`, and
//! answers to cancel/steer/pause like any other. The session only decides what
//! the turn starts from and what happens to what it produced.
//!
//! # Checkout, not sharing
//!
//! A turn runs on a **clone** of the history and a **copy** of the guard, and
//! only a `Completed` outcome writes the transcript back. That one decision
//! carries three properties:
//!
//! - `GET` always answers, from the last settled state, even mid-turn — no
//!   reader ever waits on a running engine.
//! - An aborted turn leaves the session exactly as it was. This is not just
//!   tidiness: a hard-cancelled turn can leave a dangling `tool_use` with no
//!   `tool_result`, and a transcript retaining that pair-break makes the
//!   *next* model call fail. Discarding the aborted clone is the same posture
//!   as the CLI's truncate-on-cancel.
//! - Spend still lands. The settled guard comes back on **every** outcome —
//!   an aborted turn's tokens were paid for, so its cost joins the session
//!   total even though its transcript does not.
//!
//! # The live-turn token
//!
//! One turn at a time per session, enforced by a reservation token rather
//! than a boolean, because the settle signal can race the reservation: a turn
//! whose thread could not even be spawned settles (by dropping its hook)
//! *inside* `register_turn`, before the creating request has resumed. A bare
//! flag cleared by that early settle would then be re-set by the creator and
//! wedge the session forever. The token makes every clear conditional on
//! naming the reservation it ends.
//!
//! # Retention
//!
//! [`MAX_SESSIONS`] bounds the count, and reclamation is driven by that cap —
//! the same shape as the turn registry (`server.rs`), for the same reason: a
//! separate retention ceiling could never bind before the admission cap does.
//! The difference is what eviction costs. A reclaimed *turn* had already
//! finished; a reclaimed *session* is a conversation destroyed, its id an
//! honest `404`. So a session is only evicted under pressure **and** past the
//! idle deadline ([`crate::server::ServeConfig::session_idle_ttl`]), oldest
//! idle first, and every eviction is reported. When nothing qualifies, the
//! create is refused instead — `429` tells the host to `DELETE` what it is
//! done with, which is recoverable in a way a destroyed transcript is not.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use rand::Rng as _;
use stella_core::BudgetGuard;
use stella_protocol::CompletionMessage;

use crate::observe::SharedObserver;
use crate::observe::event::{ServeEvent, SessionRef};
use crate::session::{SettleHook, TurnSettlement};

/// Ceiling on retained sessions.
///
/// Each one holds a full transcript in memory, so the cap bounds the *count*
/// of transcripts an authenticated caller can pin (their individual size is
/// bounded by the engine's own compaction). A `const` rather than a config
/// field for the same reason as `MAX_LIVE_TURNS`: a host that could raise the
/// cap could remove it. 64 is twice the live-turn cap — a session is idle
/// most of its life, so more of them coexist than turns do.
const MAX_SESSIONS: usize = 64;

/// The session registry: id → retained conversation.
///
/// Lock order, where both are ever taken: **sessions map → entry locks**, and
/// entry locks are only ever taken one at a time (`live` and `core` are
/// sequential, never nested). The turn registry's lock is never taken while
/// any of these are held — `routes::handle_session_turn` releases every
/// session lock before it registers the turn, and `handle_session_delete`
/// reads the live turn id in its own scope before touching the turn registry.
pub(crate) struct SessionRegistry {
    sessions: Mutex<HashMap<String, Arc<SessionEntry>>>,
    /// Registration order for [`SessionEntry::seq`] — diagnostics only.
    counter: AtomicU64,
}

impl SessionRegistry {
    pub(crate) fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(0),
        }
    }

    fn sessions(&self) -> MutexGuard<'_, HashMap<String, Arc<SessionEntry>>> {
        self.sessions.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Admit a session and hand back its id, or `None` when the registry is
    /// full and nothing idle had aged past `idle_ttl`.
    ///
    /// Admission, reclamation and insertion happen under one lock hold, the
    /// same discipline as `ServerState::register_turn` and for the same race.
    /// Events are emitted after the lock is released — an observer may do I/O.
    pub(crate) fn register(
        &self,
        history: Vec<CompletionMessage>,
        budget: BudgetGuard,
        idle_ttl: Duration,
        observer: &SharedObserver,
    ) -> Option<String> {
        let (outcome, reclaimed, live_sessions) = {
            let mut sessions = self.sessions();
            let reclaimed = reclaim_idle_expired(&mut sessions, MAX_SESSIONS, idle_ttl);
            let outcome = if sessions.len() >= MAX_SESSIONS {
                None
            } else {
                let id = new_session_id();
                let entry = Arc::new(SessionEntry {
                    seq: self.counter.fetch_add(1, Ordering::Relaxed),
                    idle_since: Mutex::new(Instant::now()),
                    live: Mutex::new(None),
                    next_token: AtomicU64::new(0),
                    core: Mutex::new(SessionCore {
                        history,
                        budget,
                        turns_completed: 0,
                        turns_aborted: 0,
                    }),
                });
                sessions.insert(id.clone(), entry);
                Some(id)
            };
            (outcome, reclaimed, sessions.len())
        };
        for (id, idle_ms) in reclaimed {
            observer.emit(&ServeEvent::SessionReclaimed {
                session: SessionRef::new(&id),
                idle_ms,
            });
        }
        match &outcome {
            Some(id) => observer.emit(&ServeEvent::SessionCreated {
                session: SessionRef::new(id),
                live_sessions,
            }),
            None => observer.emit(&ServeEvent::SessionRefused { live_sessions }),
        }
        outcome
    }

    pub(crate) fn lookup(&self, id: &str) -> Option<Arc<SessionEntry>> {
        self.sessions().get(id).cloned()
    }

    /// Remove a session, reporting the deletion. `None` when the id is
    /// unknown — already deleted, reclaimed, or never real.
    pub(crate) fn remove(&self, id: &str, observer: &SharedObserver) -> Option<Arc<SessionEntry>> {
        let (removed, live_sessions) = {
            let mut sessions = self.sessions();
            let removed = sessions.remove(id);
            (removed, sessions.len())
        };
        if removed.is_some() {
            observer.emit(&ServeEvent::SessionDeleted {
                session: SessionRef::new(id),
                live_sessions,
            });
        }
        removed
    }
}

/// Evict idle-expired sessions, longest idle first, until the registry is
/// below `target` — or until nothing left qualifies. Returns what was evicted,
/// for the caller to report outside the lock.
///
/// A session qualifies only when no turn is live in it *and* it has sat idle
/// past `ttl`. Entry locks are taken with `try_lock` because this runs under
/// the map lock: a contended lock means the session is being used right now,
/// which is exactly a reason not to reclaim it.
fn reclaim_idle_expired(
    sessions: &mut HashMap<String, Arc<SessionEntry>>,
    target: usize,
    ttl: Duration,
) -> Vec<(String, u64)> {
    if sessions.len() < target {
        return Vec::new();
    }
    let now = Instant::now();
    let mut expired: Vec<(Duration, String)> = sessions
        .iter()
        .filter_map(|(id, entry)| entry.idle_for(now, ttl).map(|idle| (idle, id.clone())))
        .collect();
    // Longest idle first: the sessions most plausibly abandoned go first, and
    // a recently-used one is only taken when nothing older can make room.
    expired.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    let excess = sessions.len() - target + 1;
    expired
        .into_iter()
        .take(excess)
        .map(|(idle, id)| {
            sessions.remove(&id);
            (id, crate::observe::event::millis(idle))
        })
        .collect()
}

/// One retained conversation.
pub(crate) struct SessionEntry {
    /// Registration order. Diagnostics only — eviction is by idle time.
    #[allow(dead_code)]
    seq: u64,
    /// When this session last started or settled a turn. Reads (`GET`) do not
    /// touch it: observation must not extend a lease, or a dashboard polling
    /// its sessions would make every one of them immortal.
    idle_since: Mutex<Instant>,
    /// The live-turn reservation — see the module docs for why a token.
    live: Mutex<Option<LiveTurn>>,
    next_token: AtomicU64,
    core: Mutex<SessionCore>,
}

/// The state a session carries between turns.
struct SessionCore {
    history: Vec<CompletionMessage>,
    budget: BudgetGuard,
    turns_completed: u64,
    turns_aborted: u64,
}

struct LiveTurn {
    token: u64,
    /// `None` between the reservation and `bind_turn` — the moment where the
    /// turn id has not been minted yet.
    turn_id: Option<String>,
}

/// A read of a session for `GET /v1/sessions/{id}`: always the last settled
/// state, never blocked on a running turn.
pub(crate) struct SessionView {
    pub history: Vec<CompletionMessage>,
    pub cost_usd: f64,
    pub turns_completed: u64,
    pub turns_aborted: u64,
    pub live_turn: Option<String>,
}

impl SessionEntry {
    /// How long this session has been idle beyond `ttl`, or `None` when it is
    /// live, recently used, or momentarily contended.
    fn idle_for(&self, now: Instant, ttl: Duration) -> Option<Duration> {
        let live = match self.live.try_lock() {
            Ok(live) => live,
            Err(TryLockError::WouldBlock) => return None,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        if live.is_some() {
            return None;
        }
        let idle_since = match self.idle_since.try_lock() {
            Ok(idle_since) => *idle_since,
            Err(TryLockError::WouldBlock) => return None,
            Err(TryLockError::Poisoned(poisoned)) => *poisoned.into_inner(),
        };
        let idle = now.saturating_duration_since(idle_since);
        (idle >= ttl).then_some(idle)
    }

    fn touch(&self) {
        *self.idle_since.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
    }

    fn live_lock(&self) -> MutexGuard<'_, Option<LiveTurn>> {
        self.live.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn core_lock(&self) -> MutexGuard<'_, SessionCore> {
        self.core.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Reserve the session's one live-turn slot. `None` when a turn is
    /// already running — the caller answers `409`.
    pub(crate) fn reserve(&self) -> Option<u64> {
        let mut live = self.live_lock();
        if live.is_some() {
            return None;
        }
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        *live = Some(LiveTurn {
            token,
            turn_id: None,
        });
        self.touch();
        Some(token)
    }

    /// The starting state for a reserved turn: the history cloned, and the
    /// session's guard with its turn axis reset.
    pub(crate) fn snapshot(&self) -> (Vec<CompletionMessage>, BudgetGuard) {
        let core = self.core_lock();
        let mut budget = core.budget;
        budget.begin_turn();
        (core.history.clone(), budget)
    }

    /// Record which turn id the reservation became — unless the turn already
    /// settled (a spawn failure settles inside `register_turn`, before the
    /// creator gets here), in which case there is nothing left to name.
    pub(crate) fn bind_turn(&self, token: u64, turn_id: &str) {
        let mut live = self.live_lock();
        if let Some(reserved) = live.as_mut()
            && reserved.token == token
        {
            reserved.turn_id = Some(turn_id.to_string());
        }
    }

    /// Give a reservation back without a turn ever existing — registration
    /// was refused (the turn registry is at capacity).
    pub(crate) fn release(&self, token: u64) {
        let mut live = self.live_lock();
        if live.as_ref().is_some_and(|l| l.token == token) {
            *live = None;
        }
    }

    /// The id of the turn currently running in this session, if any.
    pub(crate) fn live_turn_id(&self) -> Option<String> {
        self.live_lock().as_ref().and_then(|l| l.turn_id.clone())
    }

    /// Read the last settled state — see [`SessionView`].
    pub(crate) fn view(&self) -> SessionView {
        let (history, cost_usd, turns_completed, turns_aborted) = {
            let core = self.core_lock();
            (
                core.history.clone(),
                core.budget.session_spent_usd(),
                core.turns_completed,
                core.turns_aborted,
            )
        };
        SessionView {
            history,
            cost_usd,
            turns_completed,
            turns_aborted,
            live_turn: self.live_turn_id(),
        }
    }

    /// The settlement hook to install on this session's turn
    /// ([`crate::session::SessionSpec::on_settled`]).
    ///
    /// Called → the turn's transcript and spend are folded in. Merely dropped
    /// (the session thread never ran) → the reservation is released and the
    /// turn counted aborted, so a spawn failure cannot wedge the session.
    pub(crate) fn settle_hook(self: &Arc<Self>, token: u64) -> SettleHook {
        let mut ticket = Ticket {
            entry: Some(Arc::clone(self)),
            token,
        };
        Box::new(move |settlement| {
            if let Some(entry) = ticket.entry.take() {
                entry.settle(ticket.token, settlement);
            }
        })
    }

    /// Fold a settled turn into the session. The guard comes back on every
    /// outcome (spend is real either way); the transcript only on completion
    /// (an aborted clone may hold a dangling `tool_use` — see module docs).
    ///
    /// The core is written **before** the live slot clears, and the ordering
    /// is load-bearing: clearing first opens a window where a concurrent
    /// `POST .../turns` can reserve the freed slot and snapshot the *previous*
    /// turn's history — losing this one from the next turn's transcript. With
    /// the core written first, whoever wins the freed slot reads settled
    /// state. (A host that waits for `turn_complete` never races this — the
    /// settle also runs before that frame is emitted — but the wire cannot
    /// make hosts wait.)
    fn settle(&self, token: u64, settlement: TurnSettlement) {
        {
            let mut core = self.core_lock();
            core.budget = settlement.budget;
            if settlement.completed {
                core.history = settlement.messages;
                core.turns_completed += 1;
            } else {
                core.turns_aborted += 1;
            }
        }
        let mut live = self.live_lock();
        if live.as_ref().is_some_and(|l| l.token == token) {
            *live = None;
        }
        drop(live);
        self.touch();
    }

    /// A turn that died without settling — its hook was dropped uncalled.
    /// Same core-before-live ordering as [`SessionEntry::settle`].
    fn settle_died(&self, token: u64) {
        self.core_lock().turns_aborted += 1;
        let mut live = self.live_lock();
        if live.as_ref().is_some_and(|l| l.token == token) {
            *live = None;
        }
        drop(live);
        self.touch();
    }
}

/// The drop half of [`SessionEntry::settle_hook`]: if the closure is torn
/// down without being called, the reservation still ends.
struct Ticket {
    entry: Option<Arc<SessionEntry>>,
    token: u64,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if let Some(entry) = self.entry.take() {
            entry.settle_died(self.token);
        }
    }
}

/// Mint a session id that cannot be guessed from another's — same construction
/// and same reasoning as `new_turn_id` in `server.rs`. Deliberately *not* the
/// CLI registry's `ses-<ms>-<pid>` format: those ids never leave the machine
/// and are guessable by design; these are a capability presented over a wire.
fn new_session_id() -> String {
    let mut bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    let mut id = String::with_capacity(8 + 32);
    id.push_str("session-");
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(id, "{byte:02x}");
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::null_observer;
    use stella_protocol::BudgetMode;

    fn registry_with_one(idle_ttl: Duration) -> (SessionRegistry, String) {
        let registry = SessionRegistry::new();
        let id = registry
            .register(
                vec![CompletionMessage::system("sys")],
                BudgetGuard::new(BudgetMode::Off, None, None),
                idle_ttl,
                &null_observer(),
            )
            .expect("register");
        (registry, id)
    }

    fn settled(completed: bool, messages: Vec<CompletionMessage>) -> TurnSettlement {
        TurnSettlement {
            completed,
            messages,
            budget: BudgetGuard::new(BudgetMode::Off, None, None),
        }
    }

    /// One turn at a time: a second reserve while the first is outstanding is
    /// refused, and settling frees the slot.
    #[test]
    fn a_session_admits_one_live_turn_at_a_time() {
        let (registry, id) = registry_with_one(Duration::from_secs(3600));
        let entry = registry.lookup(&id).expect("lookup");
        let token = entry.reserve().expect("first reserve");
        assert!(entry.reserve().is_none(), "the slot is single-occupancy");
        entry.settle(token, settled(true, vec![CompletionMessage::system("sys")]));
        assert!(entry.reserve().is_some(), "settling frees the slot");
    }

    /// Only a completed turn rewrites history; an aborted one leaves it — but
    /// its spend still lands via the returned guard.
    #[test]
    fn an_aborted_turn_keeps_the_history_and_the_spend() {
        let (registry, id) = registry_with_one(Duration::from_secs(3600));
        let entry = registry.lookup(&id).expect("lookup");

        let token = entry.reserve().expect("reserve");
        let mut spent = BudgetGuard::new(BudgetMode::Off, None, None);
        let _ = spent.record_spend(0.25);
        entry.settle(
            token,
            TurnSettlement {
                completed: false,
                messages: vec![
                    CompletionMessage::system("sys"),
                    CompletionMessage::user("half-finished"),
                ],
                budget: spent,
            },
        );

        let view = entry.view();
        assert_eq!(view.history.len(), 1, "an aborted turn writes nothing back");
        assert_eq!(view.turns_aborted, 1);
        assert_eq!(view.turns_completed, 0);
        assert!(
            (view.cost_usd - 0.25).abs() < f64::EPSILON,
            "aborted spend is still spend"
        );
    }

    /// A hook dropped uncalled — the session thread never ran — must free the
    /// reservation rather than wedge the session forever.
    #[test]
    fn a_dropped_hook_frees_the_reservation() {
        let (registry, id) = registry_with_one(Duration::from_secs(3600));
        let entry = registry.lookup(&id).expect("lookup");
        let token = entry.reserve().expect("reserve");
        drop(entry.settle_hook(token));
        assert!(
            entry.reserve().is_some(),
            "the reservation must not outlive its dropped hook"
        );
        assert_eq!(entry.view().turns_aborted, 1);
    }

    /// An early settle (before `bind_turn`) must not be resurrected by the
    /// creator's late bind — the token protocol from the module docs.
    #[test]
    fn a_late_bind_after_settle_does_not_wedge_the_session() {
        let (registry, id) = registry_with_one(Duration::from_secs(3600));
        let entry = registry.lookup(&id).expect("lookup");
        let token = entry.reserve().expect("reserve");
        entry.settle(token, settled(true, vec![CompletionMessage::system("sys")]));
        entry.bind_turn(token, "turn-dead");
        assert!(
            entry.reserve().is_some(),
            "a bind after settle must find nothing to name"
        );
    }

    /// At the cap, an idle-expired session is evicted (longest idle first) —
    /// and when nothing qualifies, the create is refused instead of taking a
    /// conversation someone is still using.
    #[test]
    fn the_cap_evicts_expired_idle_sessions_and_otherwise_refuses() {
        let registry = SessionRegistry::new();
        let observer = null_observer();
        let budget = || BudgetGuard::new(BudgetMode::Off, None, None);
        // Fill to the cap with a TTL of zero: everything idle is instantly
        // expired, so each create past the cap evicts the longest-idle one.
        for _ in 0..MAX_SESSIONS {
            assert!(
                registry
                    .register(Vec::new(), budget(), Duration::ZERO, &observer)
                    .is_some()
            );
        }
        assert!(
            registry
                .register(Vec::new(), budget(), Duration::ZERO, &observer)
                .is_some(),
            "an expired-idle session makes room"
        );
        // With an infinite TTL nothing qualifies, so the cap refuses.
        assert!(
            registry
                .register(Vec::new(), budget(), Duration::MAX, &observer)
                .is_none(),
            "no eviction of sessions inside their idle window"
        );
    }

    /// A session with a live turn is never reclaimed, whatever its age.
    #[test]
    fn a_live_session_is_never_reclaimed() {
        let registry = SessionRegistry::new();
        let observer = null_observer();
        let budget = || BudgetGuard::new(BudgetMode::Off, None, None);
        let mut ids = Vec::new();
        for _ in 0..MAX_SESSIONS {
            ids.push(
                registry
                    .register(Vec::new(), budget(), Duration::ZERO, &observer)
                    .expect("register"),
            );
        }
        // Every session holds a live reservation: all expired by TTL, none
        // reclaimable.
        let entries: Vec<_> = ids
            .iter()
            .map(|id| registry.lookup(id).expect("lookup"))
            .collect();
        for entry in &entries {
            entry.reserve().expect("reserve");
        }
        assert!(
            registry
                .register(Vec::new(), budget(), Duration::ZERO, &observer)
                .is_none(),
            "a running conversation must never be evicted"
        );
    }
}
