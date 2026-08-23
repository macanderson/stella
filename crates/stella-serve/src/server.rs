// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The HTTP/SSE transport over [`Session`].
//!
//! Endpoints (all under a bearer token except `/healthz`):
//!
//! | Method + path | Purpose |
//! |---|---|
//! | `GET /healthz` | liveness — is this process alive |
//! | `GET /readyz` | readiness — is it safe to send new work (#1131) |
//! | `GET /v1/metrics` | counters ([`crate::observe::Snapshot`]) — authenticated, pull-only |
//! | `POST /v1/turns` | start a turn (`TurnRequest` body) → `{ "turn_id": … }` |
//! | `GET /v1/turns/{id}/events` | SSE stream of [`ServerFrame`](crate::frame::ServerFrame)s until `turn_complete` |
//! | `POST /v1/turns/{id}/tool-result` | answer a `tool_request` (`ToolResultIn`) |
//! | `POST /v1/turns/{id}/provider-result` | answer a `provider_request` (`ProviderResultIn`) |
//! | `POST /v1/turns/{id}/cancel` | end an in-flight turn → `{ "status": "cancelled" }` |
//! | `POST /v1/turns/{id}/steer` | inject a mid-turn user message (#932) |
//! | `POST /v1/turns/{id}/pause` | hold the turn at its next step boundary; optional `{"reason"}` (#932) |
//! | `POST /v1/turns/{id}/resume` | release a held turn (#932) |
//! | `POST /v1/sessions` | open a server-owned conversation (#931) → `{ "session_id": … }` |
//! | `GET /v1/sessions/{id}` | history, cost to date, live turn |
//! | `POST /v1/sessions/{id}/turns` | run the next turn (turn semantics minus `messages`) |
//! | `DELETE /v1/sessions/{id}` | end the session, cancelling its live turn |
//! | `GET`/`DELETE` `/v1/{turns,sessions}/{id}/checkpoint` | read back or reclaim a resume point (#1198) |
//!
//! Any other method on one of those paths is a `405` carrying `Allow`; any
//! other path is a `404`. The handlers themselves live in `src/routes.rs` and
//! the turn registry they act on in `src/state.rs`; this module is the
//! transport — listener, connection fold, auth, route classification.
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
//! including when the connection failed. The router's twenty-odd exits do not know
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
//! ([`SessionSpec::reverse_request_timeout`](crate::session::SessionSpec::reverse_request_timeout)) are the two bounds on a turn that
//! stops making progress: the deadline is the automatic one, cancel the manual
//! one.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};

use crate::http::{BodyOutcome, ReadOutcome, discard_body, read_body, read_head};
use crate::lifecycle::{accept_loop, drain, termination_signal};
use crate::observe::event::{RequestId, Route, ServeEvent, host_for_record, millis};
use crate::observe::record::{RequestRecord, Responder};
use crate::observe::{Metrics, SharedObserver};
use crate::router::{allowed, classify, resume_point, route_addresses_a_turn};
use crate::routes::{self, error_body};
use crate::state::ServerState;

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
    /// How long a session must sit idle (no turn started or settled) before
    /// the registry may reclaim it to admit a new one.
    ///
    /// This is *not* an expiry clock — an idle session inside the cap is kept
    /// indefinitely. It only decides who is evictable when `POST /v1/sessions`
    /// finds the registry full: idle past this window → reclaimable, longest
    /// idle first; otherwise the create is refused with `429`. Memory is
    /// bounded by the session cap either way, which is why this dial has no
    /// ceiling: raising it trades `429`s for retention, never boundedness.
    pub session_idle_ttl: Duration,
    /// Hostnames this deployment answers to, for the `Host` guard (#1130).
    ///
    /// Only consulted on a **non-loopback** bind: a loopback bind already
    /// knows what it is called, and enforces loopback identities regardless.
    /// Leaving it empty on a `0.0.0.0` bind leaves the guard
    /// [`crate::HostMode::Inert`] — reported on `Listening` rather than
    /// silently assumed; see `crate::hostguard` for why that arm exists.
    pub allowed_hosts: Vec<String>,
    /// How long a shutdown drain waits for in-flight turns to reach a step
    /// boundary before cancelling what is left (#1131).
    ///
    /// Clamped to [`MAX_SHUTDOWN_GRACE`]. The number that matters is the
    /// orchestrator's own: Kubernetes sends `SIGKILL` once
    /// `terminationGracePeriodSeconds` (30 by default) elapses, and a drain
    /// budgeted past that is not a longer drain — it is the same drain with
    /// its tail replaced by a hard kill, which is the outcome the drain exists
    /// to avoid. [`DEFAULT_SHUTDOWN_GRACE`] deliberately finishes inside the
    /// default window.
    pub shutdown_grace: Duration,
    /// Where served turns write their resume points, if anywhere (#1198).
    ///
    /// `None` — the default — is the pre-#1198 behavior: `drive_turn` still
    /// reaches both checkpoint seams, finds no sink, and returns before
    /// serializing anything, so a non-durable deployment pays nothing per
    /// step. Durability is opt-in because *where* a checkpoint goes is a
    /// deployment decision (a mounted volume, a replicated table) that this
    /// crate cannot guess and must not invent — see `crate::checkpoint`.
    pub checkpoints: Option<Arc<dyn crate::checkpoint::CheckpointStore>>,
    /// What this deployment allows a turn's sub-agents to do (#1297).
    ///
    /// `Default` allows none: children spend money on the host's account, and
    /// a deployment that has not thought about that should not discover it
    /// from a request body. Turning them on is
    /// [`ServeConfig::with_sub_agents`], and every caller-settable knob is
    /// bounded by the policy — see [`crate::SubAgentPolicy`].
    pub sub_agents: crate::SubAgentPolicy,
    /// The operator's hook extensions, installed on every served turn (#1298).
    ///
    /// Empty — the default, and what the shipping binary ships — is the
    /// pre-#1298 behavior exactly: no bus is minted and no turn pays for a
    /// boundary nobody observes.
    ///
    /// **Set here and nowhere else.** No route registers, names or configures
    /// an extension, and that is a deliberate security boundary rather than an
    /// unfinished one: the bearer token authenticates a *host*, and a host
    /// that could install a handler could read every tool call's raw input and
    /// `Deny` its way into steering the engine. `crate::extensions` states the
    /// rule and what each hook kind may do.
    pub extensions: crate::extensions::Extensions,
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

/// The default [`ServeConfig::session_idle_ttl`]: one hour.
///
/// Long enough that a conversation paced by a human — minutes between turns —
/// is never the one evicted; short enough that a host leaking sessions finds
/// the registry self-healing under pressure rather than permanently full.
pub const DEFAULT_SESSION_IDLE_TTL: Duration = Duration::from_secs(3600);

/// The default [`ServeConfig::shutdown_grace`]: twenty-five seconds.
///
/// Chosen against Kubernetes' 30-second default
/// `terminationGracePeriodSeconds`, not picked as a round number: the drain
/// has to *finish* inside the orchestrator's window, because a drain still
/// running when `SIGKILL` lands has achieved nothing that a hard kill would
/// not have. Five seconds of headroom covers the signal delivery, the poll
/// granularity, and the final terminal frames.
pub const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(25);

/// Ceiling on [`ServeConfig::shutdown_grace`]: five minutes.
///
/// Same reasoning as [`MAX_RESUME_GRACE`]: a bound a caller could raise
/// without limit is not a bound. A drain that cannot end is a process that
/// cannot be redeployed.
pub const MAX_SHUTDOWN_GRACE: Duration = Duration::from_secs(300);

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
            session_idle_ttl: DEFAULT_SESSION_IDLE_TTL,
            allowed_hosts: Vec::new(),
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
            checkpoints: None,
            sub_agents: crate::SubAgentPolicy::default(),
            extensions: crate::extensions::Extensions::new(),
        }
    }

    /// Allow served turns to delegate to sub-agents, under `policy` (#1297).
    ///
    /// Off by default; see [`ServeConfig::sub_agents`] for why the safe
    /// posture is the default one.
    #[must_use]
    pub fn with_sub_agents(mut self, policy: crate::SubAgentPolicy) -> Self {
        self.sub_agents = policy;
        self
    }

    /// Install an operator hook extension on every turn this server runs
    /// (#1298).
    ///
    /// Order matters and is preserved: blocking policy handlers run in
    /// registration order and the chain stops at the first `Deny`, so the
    /// extension registered first is the one that gets to refuse first. See
    /// [`ServeConfig::extensions`] for who may call this and why nobody else
    /// can.
    #[must_use]
    pub fn with_extension(mut self, extension: Arc<dyn crate::extensions::ServeExtension>) -> Self {
        self.extensions.push(extension);
        self
    }

    /// Make served turns durable by writing their resume points to `store`.
    ///
    /// `FileCheckpointStore::open(dir)` is the ordinary production answer;
    /// `MemoryCheckpointStore` is for tests. See
    /// [`ServeConfig::checkpoints`].
    #[must_use]
    pub fn with_checkpoint_store(
        mut self,
        store: Arc<dyn crate::checkpoint::CheckpointStore>,
    ) -> Self {
        self.checkpoints = Some(store);
        self
    }

    /// Set how long a shutdown drain waits before cancelling what is left,
    /// clamped to [`MAX_SHUTDOWN_GRACE`].
    ///
    /// Clamped rather than rejected, for the same reason as
    /// [`ServeConfig::with_resume_grace`]: a tuning mistake in an operational
    /// dial must not become a failure to boot.
    #[must_use]
    pub fn with_shutdown_grace(mut self, grace: Duration) -> Self {
        self.shutdown_grace = grace.min(MAX_SHUTDOWN_GRACE);
        self
    }

    /// Name the hostnames this deployment answers to — see
    /// [`ServeConfig::allowed_hosts`].
    #[must_use]
    pub fn with_allowed_hosts(mut self, hosts: Vec<String>) -> Self {
        self.allowed_hosts = hosts;
        self
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

    /// Set how long a session must sit idle before it is reclaimable under
    /// pressure — see [`ServeConfig::session_idle_ttl`] for what this does
    /// and does not bound.
    #[must_use]
    pub fn with_session_idle_ttl(mut self, ttl: Duration) -> Self {
        self.session_idle_ttl = ttl;
        self
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
    serve_until(config, on_ready, termination_signal()).await
}

/// [`serve`], stopping when `shutdown` resolves instead of only on a fatal
/// listener error — the injectable form the drain is tested through (#1131).
///
/// `serve` is this with the process's own termination signals supplied. The
/// split exists because a signal is the one input a test cannot deliver
/// without affecting the whole test binary: `tokio::signal` replaces a
/// signal's default disposition process-wide, so a suite whose servers each
/// registered a `SIGTERM` handler would quietly become un-killable. A oneshot
/// exercises the identical drain path with none of that.
///
/// # What a drain does
///
/// 1. Report not-ready. `GET /readyz` answers `503` from this moment, so a
///    load balancer stops routing new traffic here.
/// 2. Refuse new work. `POST /v1/turns` and both session-creating routes
///    answer `503`; everything an in-flight turn needs keeps working.
/// 3. Ask every live turn to stop at its next step boundary. An in-flight
///    model call *completes* and its result is kept — see
///    `Controls::soft_stop` for why that is not the same as cancelling.
/// 4. Wait, up to [`ServeConfig::shutdown_grace`], for the turns to settle.
/// 5. Cancel whatever is left. A turn parked on a host that never answered has
///    no boundary to reach, so the graceful ask cannot bound it; the cancel
///    can, and still lets each turn emit its terminal frame rather than having
///    its future dropped.
///
/// # The listener stays open for the whole drain
///
/// It is tempting to close the port first — it reads like "stop accepting" —
/// and it is wrong twice over. The reverse-RPC protocol means an in-flight
/// turn is finished *by the host*, over new connections: a turn parked on a
/// `provider_request` reaches its step boundary only once a
/// `POST /v1/turns/{id}/provider-result` arrives. Close the port and every
/// parked turn is unreachable, so the drain waits out its whole grace period
/// and then cancels turns it wedged itself. `GET /readyz` is likewise
/// unanswerable, so the one signal telling a balancer to route elsewhere
/// disappears exactly when it is needed.
///
/// Refusing new work is done by *status code*, not by closing the socket. The
/// port is released when the process exits, which is the moment the
/// replacement instance actually needs it.
pub async fn serve_until(
    config: ServeConfig,
    on_ready: impl FnOnce(SocketAddr),
    shutdown: impl std::future::Future<Output = ()>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(config.bind).await?;
    let bound = listener.local_addr()?;
    // Compiled against the address actually bound, not the one requested: a
    // `127.0.0.1:0` test bind must be judged on the port it got.
    let host_policy = crate::hostguard::HostPolicy::new(bound, &config.allowed_hosts);
    let host_guard = host_policy.mode();
    // Read before the config is consumed by the state it builds.
    let shutdown_grace = config.shutdown_grace.min(MAX_SHUTDOWN_GRACE);
    let state = Arc::new(ServerState::new(config, host_policy));
    state.observer().emit(&ServeEvent::Listening {
        addr: bound.to_string(),
        host_guard,
    });
    // Construction is done: every later request finds a complete state, so
    // `/readyz` may now answer `200`.
    state.lifecycle().mark_ready();
    on_ready(bound);

    // The two run concurrently on purpose — see the "listener stays open"
    // note above. Whichever finishes first ends the server: a completed drain
    // is the orderly exit, a returning accept loop is the fatal-listener one.
    // Dropping the loser is safe either way; `TcpListener::accept` is
    // cancel-safe, and an in-flight connection lives in its own spawned task
    // rather than in this future.
    let accepting = accept_loop(&state, listener);
    let draining = async {
        shutdown.await;
        drain(&state, shutdown_grace).await;
    };
    tokio::select! {
        result = accepting => result,
        () = draining => Ok(()),
    }
}

/// Serve one connection, and report it exactly once.
///
/// The record is opened before the head is read and closed after the router
/// returns, so there is no path — malformed request, oversized head, timeout,
/// silent hangup, I/O failure mid-response — that produces no record. That is
/// the whole reason [`route`] exists as a separate function: distributing
/// "remember to report" across its twenty-odd exits is one refactor away from
/// being silently false, which is exactly how the PROOF rail (#901) shipped a
/// surface that reported only on the happy path.
pub(crate) async fn handle_conn(
    mut stream: TcpStream,
    state: Arc<ServerState>,
) -> std::io::Result<()> {
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
    let (route, member_id) = classify(&segs);
    record.set_route(route);
    let member_id = member_id.map(str::to_string);
    // The member id is a turn id on `/v1/turns/...` and a session id on
    // `/v1/sessions/...`; only the former belongs in the record's turn slot.
    if let Some(id) = member_id.as_deref()
        && route_addresses_a_turn(route)
    {
        record.set_turn(id);
    }

    // The `Host` guard runs before every dispatch, `/healthz` included. A
    // rebound request reaching the unauthenticated liveness endpoint leaks
    // only liveness, but "before any route dispatch" is a rule that stays
    // true under later edits precisely because it has no exceptions to
    // remember — and `/healthz` is the one route bearer auth does not cover,
    // so exempting it would carve the hole in the only place the second
    // control is absent. Loopback callers are always permitted, so a
    // container's own probe never needs an allow-list entry (#1130).
    if !state.permits_host(req.header("host")) {
        discard_body(res.stream_mut(), &mut req).await;
        state.observer().emit(&ServeEvent::HostRejected {
            request_id: record.request_id().clone(),
            route,
            host: host_for_record(req.header("host").unwrap_or_default()),
        });
        return res
            .json(
                "403 Forbidden",
                &error_body("Host header does not match this server's expected identity"),
            )
            .await;
    }

    if route == Route::Healthz && req.method == "GET" {
        discard_body(res.stream_mut(), &mut req).await;
        return routes::handle_health(res).await;
    }
    // Unauthenticated for the same reason `/healthz` is: a readiness probe is
    // infrastructure, run by a kubelet or a load balancer that has no bearer
    // token and no way to be given one. It reveals strictly less than
    // `/healthz` already does — that the process is alive is implied by any
    // answer at all, and "draining" is a state an operator's own orchestrator
    // put it in.
    if route == Route::Readyz && req.method == "GET" {
        discard_body(res.stream_mut(), &mut req).await;
        return routes::handle_ready(res, state).await;
    }
    // Compared in constant time: `==` on a `&str` stops at the first differing
    // byte, which leaks the shared secret one byte at a time to a caller that
    // can time its own 401s. A missing header stays a hard `false` so an
    // empty configured token cannot authorize an anonymous request.
    let authorized = match req.bearer() {
        Some(presented) => state.token_matches(presented),
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

    // A draining process finishes what it has and admits nothing new (#1131).
    //
    // The split is what makes a drain a drain rather than a stall: the three
    // routes below *start* work, so they are refused, while everything an
    // in-flight turn needs to reach its step boundary — `tool-result` and
    // `provider-result` above all, but also cancel/pause/resume/steer and the
    // `/events` stream watching it — keeps working. Refusing a
    // `provider-result` here would park every turn on a reverse request it
    // could no longer be answered on, and the drain would then time out
    // cancelling turns it had itself wedged.
    //
    // Placed after the auth check on purpose: an anonymous caller learns
    // "wrong token", not "this instance is going away".
    if state.lifecycle().is_draining()
        && matches!(
            route,
            Route::TurnsCreate | Route::SessionsCreate | Route::SessionTurns
        )
    {
        discard_body(res.stream_mut(), &mut req).await;
        // `Retry-After: 1` points a client at its *next* attempt, which a load
        // balancer that has seen `/readyz` go `503` will route to a different
        // instance. It is not a promise that this one will be back.
        return res
            .json_with_headers(
                "503 Service Unavailable",
                &[("Retry-After", "1")],
                &error_body("server is shutting down and is not accepting new work"),
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
        ("GET", Route::Calibration) => {
            discard_body(res.stream_mut(), &mut req).await;
            return routes::handle_calibration(res, state).await;
        }
        ("GET", Route::TurnEvents) => {
            discard_body(res.stream_mut(), &mut req).await;
            return routes::handle_events(
                res,
                record,
                state,
                member_id.as_deref().unwrap_or_default(),
                resume_point(&query, &req),
            )
            .await;
        }
        ("POST", Route::TurnCancel) => {
            discard_body(res.stream_mut(), &mut req).await;
            return routes::handle_cancel(res, state, member_id.as_deref().unwrap_or_default())
                .await;
        }
        ("POST", Route::TurnResume) => {
            discard_body(res.stream_mut(), &mut req).await;
            return routes::handle_resume(res, state, member_id.as_deref().unwrap_or_default())
                .await;
        }
        // One handler for both templates: a checkpoint is looked up by key,
        // and whether that key names a turn or a session is a routing fact the
        // store has no opinion about.
        ("GET", Route::TurnCheckpoint | Route::SessionCheckpoint) => {
            discard_body(res.stream_mut(), &mut req).await;
            return routes::handle_checkpoint_get(
                res,
                state,
                member_id.as_deref().unwrap_or_default(),
            )
            .await;
        }
        ("DELETE", Route::TurnCheckpoint | Route::SessionCheckpoint) => {
            discard_body(res.stream_mut(), &mut req).await;
            return routes::handle_checkpoint_delete(
                res,
                state,
                member_id.as_deref().unwrap_or_default(),
            )
            .await;
        }
        ("GET", Route::Session) => {
            discard_body(res.stream_mut(), &mut req).await;
            return routes::handle_session_get(
                res,
                state,
                member_id.as_deref().unwrap_or_default(),
            )
            .await;
        }
        ("DELETE", Route::Session) => {
            discard_body(res.stream_mut(), &mut req).await;
            return routes::handle_session_delete(
                res,
                state,
                member_id.as_deref().unwrap_or_default(),
            )
            .await;
        }
        // The body-bearing routes fall through to the read below.
        (
            "POST",
            Route::TurnsCreate
            | Route::TurnToolResult
            | Route::TurnProviderResult
            | Route::TurnProviderDelta
            | Route::TurnSteer
            // Body-bearing but not body-*requiring*: an empty pause is the
            // shape the route shipped with and stays valid (#932).
            | Route::TurnPause
            | Route::SessionsCreate
            | Route::SessionTurns,
        ) => {}
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

    let id = member_id.as_deref().unwrap_or_default();
    match route {
        Route::TurnsCreate => routes::handle_create(res, state, &req.body).await,
        Route::TurnToolResult => routes::handle_tool_result(res, state, id, &req.body).await,
        Route::TurnProviderResult => {
            routes::handle_provider_result(res, state, id, &req.body).await
        }
        Route::TurnProviderDelta => routes::handle_provider_delta(res, state, id, &req.body).await,
        Route::TurnSteer => routes::handle_steer(res, state, id, &req.body).await,
        Route::TurnPause => routes::handle_pause(res, state, id, &req.body).await,
        Route::SessionsCreate => routes::handle_session_create(res, state, &req.body).await,
        Route::SessionTurns => routes::handle_session_turn(res, state, id, &req.body).await,
        // Unreachable: the match above admits exactly the body-bearing
        // routes. Answering rather than panicking keeps a future edit that
        // widens that match from taking down a request thread.
        _ => res.json("404 Not Found", br#"{"error":"not found"}"#).await,
    }
}
