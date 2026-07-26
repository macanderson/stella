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
//! The SSE stream is the engine → host direction; the two result POSTs are the
//! host → engine direction. Together they are the reverse tool-call protocol —
//! the engine never runs a model or tool call itself.
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
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use stella_core::{BudgetGuard, EngineConfig};
use stella_protocol::{BudgetMode, CompletionMessage, ToolSchema};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

use crate::accept::{self, AcceptAction, AcceptBackoff};
use crate::frame::{ProviderOutcomeIn, ProviderResultIn, ServerFrame, ToolResultIn};
use crate::http::{read_request, write_json, write_sse_frame, write_sse_head};
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

/// One registered turn. `pending` answers reverse requests (shared, always
/// available); `session` is taken exactly once by the SSE stream.
struct Entry {
    pending: Pending,
    session: Mutex<Option<Session>>,
}

/// Shared server state across connections.
struct ServerState {
    token: String,
    turns: Mutex<HashMap<String, Arc<Entry>>>,
    counter: AtomicU64,
}

impl ServerState {
    fn turns(&self) -> MutexGuard<'_, HashMap<String, Arc<Entry>>> {
        self.turns.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn lookup(&self, id: &str) -> Option<Arc<Entry>> {
        self.turns().get(id).cloned()
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
/// - **No admission control.** Connections and turns are both unbounded; a
///   turn holds an OS thread from `POST /v1/turns` until its stream ends.
/// - **Turn ids are sequential**, so they are guessable by anyone holding the
///   token — the token is the only tenancy boundary, one process per tenant.
/// - **No read timeout**, so a peer that dribbles a request head holds a
///   connection open. Front it with a proxy or a private network.
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
        let state = Arc::clone(&state);
        // Per-connection errors (client hangup, bad request) stay local to the
        // connection; the accept loop keeps serving.
        tokio::spawn(async move {
            let _ = handle_conn(stream, state).await;
        });
    }
}

async fn handle_conn(mut stream: TcpStream, state: Arc<ServerState>) -> std::io::Result<()> {
    let Some(req) = read_request(&mut stream).await? else {
        return Ok(());
    };
    let path = req.path.split('?').next().unwrap_or(&req.path);
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    if req.method == "GET" && segs.as_slice() == ["healthz"] {
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
        return write_json(
            &mut stream,
            "401 Unauthorized",
            br#"{"error":"missing or invalid bearer token"}"#,
        )
        .await;
    }

    match (req.method.as_str(), segs.as_slice()) {
        ("POST", ["v1", "turns"]) => handle_create(&mut stream, &state, &req.body).await,
        ("GET", ["v1", "turns", id, "events"]) => handle_events(&mut stream, &state, id).await,
        ("POST", ["v1", "turns", id, "tool-result"]) => {
            handle_tool_result(&mut stream, &state, id, &req.body).await
        }
        ("POST", ["v1", "turns", id, "provider-result"]) => {
            handle_provider_result(&mut stream, &state, id, &req.body).await
        }
        ("POST", ["v1", "turns", id, "cancel"]) => handle_cancel(&mut stream, &state, id).await,
        _ => write_json(&mut stream, "404 Not Found", br#"{"error":"not found"}"#).await,
    }
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

    let session = Session::start(spec);
    let id = format!("turn-{}", state.counter.fetch_add(1, Ordering::Relaxed));
    let entry = Arc::new(Entry {
        pending: session.pending(),
        session: Mutex::new(Some(session)),
    });
    state.turns().insert(id.clone(), entry);

    let body = serde_json::to_vec(&TurnCreated { turn_id: &id }).unwrap_or_default();
    write_json(stream, "200 OK", &body).await
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
    while let Some(frame) = session.next_frame().await {
        let done = matches!(frame, ServerFrame::TurnComplete { .. });
        let json = serde_json::to_string(&frame).unwrap_or_else(|_| "{}".to_string());
        if write_sse_frame(stream, &json).await.is_err() {
            break;
        }
        if done {
            break;
        }
    }
    // The turn is finished streaming; drop it so its thread and registry entry
    // are reclaimed.
    state.turns().remove(id);
    stream.shutdown().await
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
