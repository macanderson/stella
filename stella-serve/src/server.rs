//! The HTTP/SSE transport over [`Session`].
//!
//! Endpoints (all under a bearer token except `/healthz`):
//!
//! | Method + path | Purpose |
//! |---|---|
//! | `GET /healthz` | liveness |
//! | `POST /v1/turns` | start a turn ([`TurnRequest`] body) → `{ "turn_id": … }` |
//! | `GET /v1/turns/{id}/events` | SSE stream of [`ServerFrame`]s until `turn_complete` |
//! | `POST /v1/turns/{id}/tool-result` | answer a `tool_request` ([`ToolResultIn`]) |
//! | `POST /v1/turns/{id}/provider-result` | answer a `provider_request` ([`ProviderResultIn`]) |
//! | `POST /v1/turns/{id}/cancel` | end an in-flight turn → `{ "status": "cancelled" }` |
//!
//! Any other method on one of those paths is a `405` carrying `Allow`; any
//! other path is a `404`.
//!
//! The SSE stream is the engine → host direction; the two result POSTs are the
//! host → engine direction. Together they are the reverse tool-call protocol —
//! the engine never runs a model or tool call itself.
//!
//! # A stream is a subscription, not a fire-and-forget
//!
//! `GET /v1/turns/{id}/events` owns the turn for its lifetime: the session is
//! taken out of the registry (a second subscriber gets `409`), and the stream
//! ending — for any reason, including the client hanging up — cancels the turn
//! and reclaims its thread. That is why the stream watches its own read half
//! while it waits for frames: a turn parked on a reverse request produces
//! nothing to write, so a disconnect would otherwise go unnoticed until the
//! reverse-request deadline expired minutes later.
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
use serde::{Deserialize, Serialize};
use stella_core::{BudgetGuard, EngineConfig};
use stella_protocol::{BudgetMode, CompletionMessage, ToolSchema};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::accept::{self, AcceptAction, AcceptBackoff};
use crate::frame::{
    ProviderOutcomeIn, ProviderResultIn, ServerFrame, ToolResultIn, TurnOutcomeWire,
};
use crate::http::{
    BodyOutcome, ReadOutcome, Request, discard_body, read_body, read_head, write_json,
    write_json_with_headers, write_sse_frame, write_sse_head,
};
use crate::pending::Pending;
use crate::session::{Session, SessionSpec};

/// How to bind and authenticate the server.
pub struct ServeConfig {
    /// Address to bind. `127.0.0.1:0` picks a free loopback port (tests); a
    /// containerized deployment binds `0.0.0.0:<port>`.
    pub bind: SocketAddr,
    /// The bearer token every request (except `/healthz`) must present. This is
    /// the auth gate — the server may bind a non-loopback address behind the
    /// host's private network.
    pub token: String,
}

/// Body of `POST /v1/turns`. The host owns prompt assembly, model selection, and
/// the tool set; engine knobs are optional overrides on top of the defaults.
#[derive(Debug, Deserialize)]
struct TurnRequest {
    provider_id: String,
    #[serde(default)]
    tools: Vec<ToolSchema>,
    messages: Vec<CompletionMessage>,
    #[serde(default)]
    budget: BudgetSpec,
    #[serde(default)]
    max_steps: Option<usize>,
    /// Per-reverse-request deadline override, in milliseconds. Omitted means
    /// [`SessionSpec::DEFAULT_REVERSE_REQUEST_TIMEOUT`].
    #[serde(default)]
    reverse_request_timeout_ms: Option<u64>,
}

/// Spend policy for a turn — the serializable projection of a [`BudgetGuard`].
#[derive(Debug, Deserialize)]
struct BudgetSpec {
    #[serde(default = "budget_mode_off")]
    mode: BudgetMode,
    #[serde(default)]
    turn_limit_usd: Option<f64>,
    #[serde(default)]
    session_limit_usd: Option<f64>,
}

impl Default for BudgetSpec {
    fn default() -> Self {
        Self {
            mode: BudgetMode::Off,
            turn_limit_usd: None,
            session_limit_usd: None,
        }
    }
}

fn budget_mode_off() -> BudgetMode {
    BudgetMode::Off
}

/// Response to `POST /v1/turns`.
#[derive(Debug, Serialize)]
struct TurnCreated<'a> {
    turn_id: &'a str,
}

/// Ceiling on a host-supplied [`TurnRequest::max_steps`].
///
/// The step cap is the engine's belt-and-suspenders backstop against a turn
/// that never terminates, and the host also supplies the budget mode (which may
/// be `Off`), so an unclamped `max_steps` lets a caller remove the last bound on
/// a turn that already holds an OS thread. `EngineConfig::default` uses 200;
/// 10 000 is fifty times that — far above any turn a real agentic task runs,
/// while still terminating in bounded time.
const MAX_SERVED_STEPS: usize = 10_000;

/// Validate a host-supplied step cap: `None` when it is unusable (`0` produces
/// a zero-iteration turn that aborts with the misleading "reached the step cap
/// (0)"), otherwise the value clamped down to [`MAX_SERVED_STEPS`].
fn validate_max_steps(requested: usize) -> Option<usize> {
    (requested > 0).then(|| requested.min(MAX_SERVED_STEPS))
}

/// Ceiling on a host-supplied reverse-request deadline: one hour.
///
/// Same reasoning as [`MAX_SERVED_STEPS`]. The deadline is what bounds a turn
/// holding an OS thread on a host that never answers, so letting a caller set it
/// to `u64::MAX` would hand back the unbounded wait it exists to remove. An hour
/// is far past any legitimate model call or tool run.
const MAX_REVERSE_REQUEST_TIMEOUT: Duration = Duration::from_secs(3600);

/// Validate a host-supplied reverse-request deadline: `None` when it is unusable
/// (`0` would expire every reverse request before the host could possibly
/// answer, making the turn fail rather than run), otherwise clamped down to
/// [`MAX_REVERSE_REQUEST_TIMEOUT`].
fn validate_reverse_request_timeout(requested_ms: u64) -> Option<Duration> {
    (requested_ms > 0).then(|| Duration::from_millis(requested_ms).min(MAX_REVERSE_REQUEST_TIMEOUT))
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
/// This is a const rather than a [`ServeConfig`] field on purpose, matching
/// [`MAX_SERVED_STEPS`] and [`MAX_REVERSE_REQUEST_TIMEOUT`]: these are the
/// server's own safety backstops, not host-tunable policy. A host that could
/// raise the cap could remove it, which is exactly what it exists to prevent.
const MAX_LIVE_TURNS: usize = 32;

/// How long a rejected caller is told to wait before retrying, in seconds.
///
/// Turns end when the host finishes streaming them, so the queue drains on the
/// order of a turn's length; 5 seconds is short enough to keep a well-behaved
/// host responsive without inviting a tight retry loop.
const RETRY_AFTER_SECS: &str = "5";

/// One registered turn. `pending` answers reverse requests (shared, always
/// available); `session` is taken exactly once by the SSE stream.
struct Entry {
    /// Registration order, so reclamation can name the *oldest* abandoned turn.
    ///
    /// Deliberately not derived from the turn id: ids are 128 random bits (see
    /// [`new_turn_id`]) precisely so that nothing can be inferred from one, and
    /// that includes age. This counter is internal and never leaves the process.
    seq: u64,
    pending: Pending,
    session: Mutex<Option<Session>>,
}

/// Shared server state across connections.
struct ServerState {
    token: String,
    turns: Mutex<HashMap<String, Arc<Entry>>>,
    /// Monotonic source of [`Entry::seq`]. Registration ordering only — the
    /// turn id is random and owes nothing to this.
    counter: AtomicU64,
    unauthorized: Mutex<TokenBucket>,
}

impl ServerState {
    fn turns(&self) -> MutexGuard<'_, HashMap<String, Arc<Entry>>> {
        self.turns.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn lookup(&self, id: &str) -> Option<Arc<Entry>> {
        self.turns().get(id).cloned()
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
    /// The session is started by `start` *inside* the lock, and only once a slot
    /// is known to be free. `Session::start` is synchronous (it spawns a thread
    /// and returns), so holding the lock across it is sound — there is no await
    /// in this block. Starting it before the cap check would spawn, then
    /// immediately drop, a thread for every refused request, which hands a
    /// caller the very thread churn the cap exists to prevent.
    fn register_turn(&self, start: impl FnOnce() -> Session) -> Option<String> {
        let mut turns = self.turns();
        reclaim_finished_unstreamed(&mut turns, MAX_LIVE_TURNS);
        if turns.len() >= MAX_LIVE_TURNS {
            return None;
        }
        let id = new_turn_id();
        // Insert through the vacant entry rather than `insert`, which would
        // *replace* on a collision: the displaced turn's `Session` would drop
        // (cancelling a turn its owner is still using) and two callers would
        // hold the same id. 128 random bits make this unreachable in practice;
        // going through the entry API makes it unreachable by construction, so
        // the guarantee does not rest on the id width alone. Refusing is
        // fail-closed — the caller renders it as the cap's 429, which is the
        // wrong reason for the right answer, and is preferable to either
        // silently cancelling a live turn or panicking a request thread.
        let slot = match turns.entry(id.clone()) {
            hash_map::Entry::Occupied(_) => return None,
            hash_map::Entry::Vacant(slot) => slot,
        };
        let session = start();
        slot.insert(Arc::new(Entry {
            seq: self.counter.fetch_add(1, Ordering::Relaxed),
            pending: session.pending(),
            session: Mutex::new(Some(session)),
        }));
        Some(id)
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
fn reclaim_finished_unstreamed(turns: &mut HashMap<String, Arc<Entry>>, target: usize) {
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
        turns.remove(&id);
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
/// from its own copy.
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
///   drain buffer rather than [`crate::http`]'s body cap. That matters
///   precisely because connections are uncapped: without it, every anonymous
///   connection could cost megabytes for as long as the read deadline allows.
///
/// Reverse requests *are* bounded (see [`SessionSpec::reverse_request_timeout`]),
/// and a turn can be ended early with `POST /v1/turns/{id}/cancel`.
pub async fn serve(config: ServeConfig, on_ready: impl FnOnce(SocketAddr)) -> std::io::Result<()> {
    let listener = TcpListener::bind(config.bind).await?;
    on_ready(listener.local_addr()?);
    let state = Arc::new(ServerState {
        token: config.token,
        turns: Mutex::new(HashMap::new()),
        counter: AtomicU64::new(0),
        unauthorized: Mutex::new(TokenBucket::new()),
    });
    let mut backoff = AcceptBackoff::new();
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => {
                backoff.succeeded();
                stream
            }
            Err(err) => match accept::classify(&err) {
                // A dead pending connection or an interrupted syscall: the
                // listener is fine, and this cannot spin (see `accept`).
                AcceptAction::Retry => {
                    backoff.succeeded();
                    continue;
                }
                // Exhaustion, or a condition `io::ErrorKind` cannot name. Sleep
                // so it cannot busy-loop, and give up if it never clears.
                AcceptAction::Backoff => match backoff.next_delay() {
                    Some(delay) => {
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    None => return Err(err),
                },
                AcceptAction::Fatal => return Err(err),
            },
        };
        // Nagle off. Every frame on this wire gates the next engine step — a
        // `provider_request` the host has not seen yet is a step that cannot
        // proceed — so coalescing a small write with the next one trades
        // nothing for up to a delayed-ACK's worth of added latency per step.
        let _ = stream.set_nodelay(true);
        let state = Arc::clone(&state);
        // Per-connection errors (client hangup, bad request) stay local to the
        // connection; the accept loop keeps serving.
        tokio::spawn(async move {
            let _ = handle_conn(stream, state).await;
        });
    }
}

/// Serve one connection: read the head, authenticate, then — and only then —
/// read the body and route.
///
/// The two-phase read is the whole point of the ordering here. Buffering the
/// body first meant an *unauthenticated* peer could make the server hold up to
/// `MAX_BODY_BYTES` for it, once per connection, and connections are
/// deliberately uncapped (see [`serve`]'s operational limits) — so the 401 was
/// answered only after paying for the request that earned it. Now a refused
/// request costs one 8 KiB drain buffer. The body is still taken off the socket
/// before the response is written: closing with unread bytes in flight makes the
/// kernel RST, and a peer that gets an RST mid-send never reads its 401.
async fn handle_conn(mut stream: TcpStream, state: Arc<ServerState>) -> std::io::Result<()> {
    let mut req = match read_head(&mut stream).await? {
        ReadOutcome::Request(req) => req,
        // A peer that never sent anything is owed nothing.
        ReadOutcome::Hangup => return Ok(()),
        ReadOutcome::TooLarge => {
            return write_json(
                &mut stream,
                "413 Payload Too Large",
                &error_body("request exceeded the server's size limit"),
            )
            .await;
        }
        ReadOutcome::Malformed => {
            return write_json(
                &mut stream,
                "400 Bad Request",
                &error_body("malformed HTTP request"),
            )
            .await;
        }
        ReadOutcome::UnsupportedTransferEncoding => {
            return write_json(
                &mut stream,
                "501 Not Implemented",
                &error_body(
                    "chunked transfer-encoding is not supported; send a Content-Length body",
                ),
            )
            .await;
        }
        ReadOutcome::Timeout => {
            return write_json(
                &mut stream,
                "408 Request Timeout",
                &error_body("request was not completed in time"),
            )
            .await;
        }
    };
    let path = req.path.split('?').next().unwrap_or(&req.path).to_string();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    if req.method == "GET" && segs.as_slice() == ["healthz"] {
        discard_body(&mut stream, &mut req).await;
        return write_json(&mut stream, "200 OK", br#"{"status":"ok"}"#).await;
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
        discard_body(&mut stream, &mut req).await;
        // Rate-limited by holding the response, never by changing it: the
        // body and status a guesser sees are identical whether or not the
        // bucket was empty, so the throttle leaks nothing about its own state.
        // The sleep is `tokio::time::sleep` on this connection's task, so a
        // held 401 costs one task, not a runtime thread.
        let delay = state.unauthorized_delay();
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        return write_json(
            &mut stream,
            "401 Unauthorized",
            br#"{"error":"missing or invalid bearer token"}"#,
        )
        .await;
    }

    // Routes that carry no body are matched before the body is read, so a GET
    // with a stray `Content-Length` is drained rather than parsed.
    match (req.method.as_str(), segs.as_slice()) {
        ("GET", ["v1", "turns", id, "events"]) => {
            let id = (*id).to_string();
            discard_body(&mut stream, &mut req).await;
            return handle_events(&mut stream, &state, &id).await;
        }
        ("POST", ["v1", "turns", id, "cancel"]) => {
            let id = (*id).to_string();
            discard_body(&mut stream, &mut req).await;
            return handle_cancel(&mut stream, &state, &id).await;
        }
        // The body-bearing routes fall through to the read below.
        ("POST", ["v1", "turns"] | ["v1", "turns", _, "tool-result" | "provider-result"]) => {}
        // A known resource reached with the wrong method is a 405 naming what
        // it does accept, not a 404 claiming the route does not exist — the
        // difference between "you typed the path wrong" and "you used the
        // wrong verb" is the whole diagnostic value of the status code.
        (_, ["healthz"]) => return method_not_allowed(&mut stream, &mut req, "GET").await,
        (_, ["v1", "turns"]) => return method_not_allowed(&mut stream, &mut req, "POST").await,
        (_, ["v1", "turns", _, "events"]) => {
            return method_not_allowed(&mut stream, &mut req, "GET").await;
        }
        (
            _,
            [
                "v1",
                "turns",
                _,
                "tool-result" | "provider-result" | "cancel",
            ],
        ) => {
            return method_not_allowed(&mut stream, &mut req, "POST").await;
        }
        _ => {
            discard_body(&mut stream, &mut req).await;
            return write_json(&mut stream, "404 Not Found", br#"{"error":"not found"}"#).await;
        }
    }

    if let BodyOutcome::Timeout = read_body(&mut stream, &mut req).await? {
        return write_json(
            &mut stream,
            "408 Request Timeout",
            &error_body("request was not completed in time"),
        )
        .await;
    }

    match segs.as_slice() {
        ["v1", "turns"] => handle_create(&mut stream, &state, &req.body).await,
        ["v1", "turns", id, "tool-result"] => {
            handle_tool_result(&mut stream, &state, id, &req.body).await
        }
        ["v1", "turns", id, "provider-result"] => {
            handle_provider_result(&mut stream, &state, id, &req.body).await
        }
        // Unreachable: the match above admits exactly the three body-bearing
        // routes. Answering rather than panicking keeps a future edit that
        // widens that match from taking down a request thread.
        _ => write_json(&mut stream, "404 Not Found", br#"{"error":"not found"}"#).await,
    }
}

/// `405` naming the methods this path accepts, per RFC 9110 §15.5.6 (the
/// `Allow` header is mandatory on a 405 — a status that says "wrong verb"
/// without saying which verb is right is half an answer).
async fn method_not_allowed(
    stream: &mut TcpStream,
    req: &mut Request,
    allow: &str,
) -> std::io::Result<()> {
    discard_body(stream, req).await;
    write_json_with_headers(
        stream,
        "405 Method Not Allowed",
        &[("Allow", allow)],
        &error_body(&format!("method not allowed; this path accepts {allow}")),
    )
    .await
}

async fn handle_create(
    stream: &mut TcpStream,
    state: &ServerState,
    body: &[u8],
) -> std::io::Result<()> {
    let turn: TurnRequest = match serde_json::from_slice(body) {
        Ok(turn) => turn,
        Err(err) => {
            return write_json(
                stream,
                "400 Bad Request",
                &error_body(&format!("invalid turn request: {err}")),
            )
            .await;
        }
    };

    let mut config = EngineConfig::default();
    if let Some(max_steps) = turn.max_steps {
        let Some(effective) = validate_max_steps(max_steps) else {
            return write_json(
                stream,
                "400 Bad Request",
                &error_body("max_steps must be at least 1"),
            )
            .await;
        };
        config.max_steps = effective;
    }
    let mut reverse_request_timeout = SessionSpec::DEFAULT_REVERSE_REQUEST_TIMEOUT;
    if let Some(requested_ms) = turn.reverse_request_timeout_ms {
        let Some(effective) = validate_reverse_request_timeout(requested_ms) else {
            return write_json(
                stream,
                "400 Bad Request",
                &error_body("reverse_request_timeout_ms must be at least 1"),
            )
            .await;
        };
        reverse_request_timeout = effective;
    }
    let spec = SessionSpec {
        provider_id: turn.provider_id,
        tools: turn.tools,
        messages: turn.messages,
        config,
        budget: BudgetGuard::new(
            turn.budget.mode,
            turn.budget.turn_limit_usd,
            turn.budget.session_limit_usd,
        ),
        reverse_request_timeout,
    };

    // Admission, reclamation and registration all happen under one lock hold
    // inside `register_turn` — see there for why.
    let Some(id) = state.register_turn(|| Session::start(spec)) else {
        return write_json_with_headers(
            stream,
            "429 Too Many Requests",
            &[("Retry-After", RETRY_AFTER_SECS)],
            &error_body("too many live turns; retry after in-flight turns finish"),
        )
        .await;
    };

    let body = serde_json::to_vec(&TurnCreated { turn_id: &id }).unwrap_or_default();
    write_json(stream, "200 OK", &body).await
}

/// Mint a turn id that cannot be guessed from another turn's id.
///
/// These were `turn-0`, `turn-1`, … from a process counter. The bearer token is
/// still the actual auth gate, but a sequential id makes every *other* live turn
/// addressable to anyone who learns one — a turn id in a log line, an error
/// report, or a proxy access log hands over the whole namespace. 128 bits from
/// the OS CSPRNG removes that second, accidental way in.
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

async fn handle_events(
    stream: &mut TcpStream,
    state: &ServerState,
    id: &str,
) -> std::io::Result<()> {
    let Some(entry) = state.lookup(id) else {
        return write_json(stream, "404 Not Found", &error_body("unknown turn")).await;
    };
    // Take the session out in its own scope so the (non-`Send`) mutex guard is
    // dropped before any `.await` — the connection future must stay `Send`.
    let taken = {
        entry
            .session
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
    };
    let mut session = match taken {
        Some(session) => session,
        None => {
            return write_json(
                stream,
                "409 Conflict",
                &error_body("events are already being streamed for this turn"),
            )
            .await;
        }
    };

    // From here the session is out of the registry entry and owned by this
    // connection, so every exit path must also drop the registry entry —
    // otherwise a turn whose stream never opened lingers in the map forever.
    if let Err(err) = write_sse_head(stream).await {
        state.turns().remove(id);
        return Err(err);
    }
    {
        // Split so the frame loop can watch the *read* half while it waits.
        // Without that, a host that vanishes mid-turn is only noticed at the
        // next write — and the engine may be parked on a reverse request for
        // the whole `reverse_request_timeout` (up to an hour) before it
        // produces one. The turn would keep an OS thread and a registry slot
        // for a client that is provably gone, and the host would be billed for
        // whatever the engine went on to ask for. Watching for EOF turns that
        // into a cancellation within one poll.
        let (mut peer, mut out) = stream.split();
        let mut scratch = [0_u8; 1024];
        // A GET on a `Connection: close` stream has nothing left to send, so
        // inbound bytes are a protocol violation. Tolerate a little (a client
        // library that pipelines a probe) but never spin discarding an
        // unbounded stream of them.
        let mut stray = 0_usize;
        const MAX_STRAY_BYTES: usize = 8 * 1024;
        loop {
            let frame = tokio::select! {
                read = peer.read(&mut scratch) => match read {
                    // EOF or a reset: the subscriber is gone. Stop streaming;
                    // dropping `session` below cancels the turn.
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        stray += n;
                        if stray > MAX_STRAY_BYTES {
                            break;
                        }
                        continue;
                    }
                },
                frame = session.next_frame() => match frame {
                    Some(frame) => frame,
                    None => break,
                },
            };
            let done = matches!(frame, ServerFrame::TurnComplete { .. });
            if write_sse_frame(&mut out, &sse_json(&frame)).await.is_err() {
                break;
            }
            if done {
                break;
            }
        }
    }
    // Drop the session *before* the socket teardown: `Drop for Session`
    // cancels the turn, so a stream that ended early (client gone, stray
    // bytes) releases the engine thread now rather than at the end of scope.
    drop(session);
    // The turn is finished streaming; drop it so its thread and registry entry
    // are reclaimed.
    state.turns().remove(id);
    stream.shutdown().await
}

/// Render one frame as the JSON payload of an SSE `data:` line.
///
/// Every [`ServerFrame`] is built from serde-clean types, so the failure arm is
/// unreachable in practice — but "unreachable" is not "harmless". It used to
/// emit `{}`, a frame with no `type` at all: a host would skip it silently, and
/// if the frame it replaced was the terminal `TurnComplete`, the stream would
/// simply end with the turn's outcome and settled cost never reported. A
/// synthesized terminal frame keeps the stream's contract — every turn ends with
/// exactly one `turn_complete` — and names the cause instead of losing it.
fn sse_json(frame: &ServerFrame) -> String {
    encode_or_abort(frame)
}

fn encode_or_abort<T: Serialize>(value: &T) -> String {
    match serde_json::to_string(value) {
        Ok(json) => json,
        Err(err) => serde_json::to_string(&ServerFrame::TurnComplete {
            outcome: TurnOutcomeWire::Aborted {
                reason: format!("the engine produced a frame the server could not encode: {err}"),
                cost_usd: 0.0,
            },
        })
        // The fallback's fallback: a literal that is valid on the wire, so the
        // stream still terminates with a `turn_complete` no matter what.
        .unwrap_or_else(|_| {
            r#"{"type":"turn_complete","outcome":{"status":"aborted","reason":"frame encoding failed","cost_usd":0.0}}"#
                .to_string()
        }),
    }
}

async fn handle_tool_result(
    stream: &mut TcpStream,
    state: &ServerState,
    id: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let Some(entry) = state.lookup(id) else {
        return write_json(stream, "404 Not Found", &error_body("unknown turn")).await;
    };
    let result: ToolResultIn = match serde_json::from_slice(body) {
        Ok(result) => result,
        Err(err) => {
            return write_json(
                stream,
                "400 Bad Request",
                &error_body(&format!("invalid tool result: {err}")),
            )
            .await;
        }
    };
    match entry
        .pending
        .resolve_tool(&result.request_id, result.output)
    {
        Ok(()) => write_json(stream, "200 OK", br#"{"status":"ok"}"#).await,
        Err(err) => write_json(stream, "409 Conflict", &error_body(&err.to_string())).await,
    }
}

async fn handle_provider_result(
    stream: &mut TcpStream,
    state: &ServerState,
    id: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let Some(entry) = state.lookup(id) else {
        return write_json(stream, "404 Not Found", &error_body("unknown turn")).await;
    };
    let posted: ProviderResultIn = match serde_json::from_slice(body) {
        Ok(posted) => posted,
        Err(err) => {
            return write_json(
                stream,
                "400 Bad Request",
                &error_body(&format!("invalid provider result: {err}")),
            )
            .await;
        }
    };
    let result = match posted.outcome {
        ProviderOutcomeIn::Ok { result } => Ok(result),
        ProviderOutcomeIn::Error { error } => Err(error.into()),
    };
    match entry.pending.resolve_provider(&posted.request_id, result) {
        Ok(()) => write_json(stream, "200 OK", br#"{"status":"ok"}"#).await,
        Err(err) => write_json(stream, "409 Conflict", &error_body(&err.to_string())).await,
    }
}

/// `POST /v1/turns/{id}/cancel` — end an in-flight turn.
///
/// Answers once the turn is *signalled*, not once it has unwound: the parked
/// engine step wakes immediately, but the turn still needs a moment to produce
/// its terminal frame, and a host streaming `/events` is the one that observes
/// that. Blocking this response on it would deadlock a single-connection client.
async fn handle_cancel(
    stream: &mut TcpStream,
    state: &ServerState,
    id: &str,
) -> std::io::Result<()> {
    // Remove and signal, so a second cancel — or a late result POST — gets an
    // honest 404 rather than silently doing nothing. Scoped so the (non-`Send`)
    // guard is dropped before the await below.
    let removed = { state.turns().remove(id) };
    let Some(entry) = removed else {
        return write_json(stream, "404 Not Found", &error_body("unknown turn")).await;
    };
    entry.pending.cancel();
    // Dropping our `Arc` here is what reclaims a turn whose stream never opened:
    // the registry no longer holds it, so this may be the last handle, and
    // `Drop for Session` releases the engine thread. A turn that *is* streaming
    // keeps its own handle and unwinds through `handle_events` as usual.
    drop(entry);
    write_json(stream, "200 OK", br#"{"status":"cancelled"}"#).await
}

fn error_body(message: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "error": message })).unwrap_or_default()
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

    /// A brute-force run against the only credential this service has must
    /// stop being free after the burst. Driven over an injected `Instant` so
    /// the refill is asserted exactly, with no sleeping in the test.
    #[test]
    fn sustained_401s_are_throttled_once_the_burst_is_spent() {
        let start = std::time::Instant::now();
        let mut bucket = TokenBucket::new();

        for i in 0..UNAUTHORIZED_BURST as usize {
            assert_eq!(
                bucket.take(start),
                Duration::ZERO,
                "401 #{i} is within the burst and must not be delayed"
            );
        }
        assert_eq!(
            bucket.take(start),
            UNAUTHORIZED_PENALTY,
            "the first 401 past the burst pays the penalty"
        );
        assert_eq!(
            bucket.take(start),
            UNAUTHORIZED_PENALTY,
            "and it keeps paying while the bucket is empty"
        );

        // One second later the bucket has refilled by UNAUTHORIZED_REFILL_PER_SEC.
        let later = start + Duration::from_secs(1);
        for _ in 0..UNAUTHORIZED_REFILL_PER_SEC as usize {
            assert_eq!(bucket.take(later), Duration::ZERO);
        }
        assert_eq!(bucket.take(later), UNAUTHORIZED_PENALTY);
    }

    /// The bucket must not bank credit indefinitely: an idle server does not
    /// hand a later attacker an unbounded free run.
    #[test]
    fn refill_is_capped_at_the_burst_size() {
        let start = std::time::Instant::now();
        let mut bucket = TokenBucket::new();
        for _ in 0..UNAUTHORIZED_BURST as usize {
            assert_eq!(bucket.take(start), Duration::ZERO);
        }
        let much_later = start + Duration::from_secs(3600);
        for i in 0..UNAUTHORIZED_BURST as usize {
            assert_eq!(bucket.take(much_later), Duration::ZERO, "burst slot {i}");
        }
        assert_eq!(
            bucket.take(much_later),
            UNAUTHORIZED_PENALTY,
            "an hour idle buys one burst, not an hour's worth of attempts"
        );
    }

    #[test]
    fn zero_steps_is_refused_rather_than_silently_accepted() {
        // `for step in 0..0` runs no iterations and aborts with "reached the
        // step cap (0)" — a turn that never called the model reporting the
        // backstop that never fired.
        assert_eq!(validate_max_steps(0), None);
    }

    #[test]
    fn step_cap_is_clamped_to_the_ceiling() {
        assert_eq!(validate_max_steps(1), Some(1));
        assert_eq!(validate_max_steps(200), Some(200));
        assert_eq!(validate_max_steps(MAX_SERVED_STEPS), Some(MAX_SERVED_STEPS));
        assert_eq!(
            validate_max_steps(MAX_SERVED_STEPS + 1),
            Some(MAX_SERVED_STEPS)
        );
        assert_eq!(
            validate_max_steps(usize::MAX),
            Some(MAX_SERVED_STEPS),
            "an unbounded step loop must not be reachable from the wire"
        );
    }

    fn test_state() -> ServerState {
        ServerState {
            token: String::new(),
            turns: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(0),
            unauthorized: Mutex::new(TokenBucket::new()),
        }
    }

    fn test_spec() -> SessionSpec {
        SessionSpec {
            provider_id: "mock".to_string(),
            tools: Vec::new(),
            messages: vec![CompletionMessage::user("hi")],
            config: EngineConfig::default(),
            budget: BudgetGuard::new(BudgetMode::Off, None, None),
            reverse_request_timeout: SessionSpec::DEFAULT_REVERSE_REQUEST_TIMEOUT,
        }
    }

    /// A session that has already produced its terminal frame: cancelled
    /// before it can park, then waited on until its thread exits.
    fn finished_session() -> Session {
        let session = Session::start(test_spec());
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
        let state = test_state();
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
    }

    /// Reclamation must never touch a live turn: a still-running turn parked on
    /// its reverse request outlives any number of finished ones, even as the
    /// oldest entry in the registry.
    #[test]
    fn a_live_turn_is_never_reclaimed() {
        let state = test_state();
        let live = state
            .register_turn(|| Session::start(test_spec()))
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
    /// host that is still using it.
    #[test]
    fn a_registry_full_of_live_turns_refuses_the_next() {
        let state = test_state();
        for _ in 0..MAX_LIVE_TURNS {
            state
                .register_turn(|| Session::start(test_spec()))
                .expect("the registry has room");
        }
        assert!(
            state
                .register_turn(|| Session::start(test_spec()))
                .is_none(),
            "no live turn may be reclaimed, so the cap must refuse"
        );
    }

    /// A value whose `Serialize` always fails, standing in for the frame
    /// encoding failure that real [`ServerFrame`]s cannot produce. Without it
    /// the fallback would be untestable — and an untested fallback on the one
    /// path that carries a turn's outcome is how `{}` sat on the wire.
    struct Unserializable;

    impl Serialize for Unserializable {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("nope"))
        }
    }

    /// A frame that cannot be encoded must still leave the stream terminated:
    /// the old `{}` fallback was a frame with no `type`, so a host skipped it
    /// silently and — when the lost frame was the terminal one — never learned
    /// the turn's outcome or its settled cost.
    #[test]
    fn an_unencodable_frame_becomes_a_terminal_aborted_frame_not_an_empty_object() {
        let json = encode_or_abort(&Unserializable);
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON on the wire");
        assert_eq!(value["type"], "turn_complete", "{json}");
        assert_eq!(value["outcome"]["status"], "aborted", "{json}");
        assert!(
            value["outcome"]["reason"]
                .as_str()
                .is_some_and(|r| r.contains("nope")),
            "the cause must survive, not be swallowed: {json}"
        );
    }

    #[test]
    fn an_ordinary_frame_encodes_unchanged() {
        let json = sse_json(&ServerFrame::TurnComplete {
            outcome: TurnOutcomeWire::Completed {
                text: "done".to_string(),
                cost_usd: 0.5,
            },
        });
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["outcome"]["text"], "done");
        assert_eq!(value["outcome"]["cost_usd"], 0.5);
    }

    #[test]
    fn zero_reverse_request_deadline_is_refused() {
        // A 0 ms deadline expires before the host could possibly answer, so
        // every turn would fail on its first reverse request.
        assert_eq!(validate_reverse_request_timeout(0), None);
    }

    #[test]
    fn reverse_request_deadline_is_clamped_to_the_ceiling() {
        assert_eq!(
            validate_reverse_request_timeout(50),
            Some(Duration::from_millis(50))
        );
        assert_eq!(
            validate_reverse_request_timeout(300_000),
            Some(Duration::from_secs(300)),
            "the default is expressible from the wire"
        );
        assert_eq!(
            validate_reverse_request_timeout(MAX_REVERSE_REQUEST_TIMEOUT.as_millis() as u64),
            Some(MAX_REVERSE_REQUEST_TIMEOUT)
        );
        assert_eq!(
            validate_reverse_request_timeout(u64::MAX),
            Some(MAX_REVERSE_REQUEST_TIMEOUT),
            "a caller must not be able to restore the unbounded wait the \
             deadline exists to remove"
        );
    }
}
