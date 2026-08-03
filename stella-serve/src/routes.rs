// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The endpoint handlers, and the wire types they parse.
//!
//! Split out of `server.rs`, which keeps the transport: the listener, the
//! connection fold, the turn registry and the 401 throttle. The cut follows the
//! concern — everything here answers *one route*, everything there is about
//! connections and shared state — and it is what keeps both files clear of the
//! 1500-line ratchet (`scripts/check-file-size.sh`) now that every response also
//! carries a record.
//!
//! Every handler writes through [`Responder`], never to the socket directly, so
//! status and `bytes_out` are captured for the record by construction rather
//! than by each handler remembering to report them.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use stella_core::{BudgetGuard, EngineConfig};
use stella_protocol::{BudgetMode, CompletionMessage, ToolSchema};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::engine_overrides::{ClampedKnob, EngineOverrides, apply_engine_overrides};
use crate::frame::{ProviderDeltaIn, ProviderOutcomeIn, ProviderResultIn, ToolResultIn};
use crate::history::{FrameHistory, Replay};
use crate::http::{Request, discard_body, write_sse_event, write_sse_frame};
use crate::observe::event::{ServeEvent, StreamEndReason, TurnRef};
use crate::observe::record::{RequestRecord, Responder};
use crate::server::ServerState;
use crate::session::{Session, SessionSpec};

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
    /// Per-turn engine knobs (#1167), lowered onto the server's defaults.
    /// Omitted — or an empty object — reproduces today's behavior exactly.
    #[serde(default)]
    engine: Option<EngineOverrides>,
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
    /// Engine knobs the server lowered below what was asked (#1167). Absent
    /// when nothing was clamped, so pre-existing clients parse unchanged.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    clamped: Vec<ClampedKnob>,
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

/// How long a rejected caller is told to wait before retrying, in seconds.
///
/// Turns end when the host finishes streaming them, so the queue drains on the
/// order of a turn's length; 5 seconds is short enough to keep a well-behaved
/// host responsive without inviting a tight retry loop.
const RETRY_AFTER_SECS: &str = "5";

/// `405` naming the methods this path accepts, per RFC 9110 §15.5.6 (the
/// `Allow` header is mandatory on a 405 — a status that says "wrong verb"
/// without saying which verb is right is half an answer).
pub(crate) async fn method_not_allowed(
    res: &mut Responder<'_>,
    req: &mut Request,
    allow: &str,
) -> std::io::Result<()> {
    discard_body(res.stream_mut(), req).await;
    res.json_with_headers(
        "405 Method Not Allowed",
        &[("Allow", allow)],
        &error_body(&format!("method not allowed; this path accepts {allow}")),
    )
    .await
}

/// `GET /healthz` — liveness. Is this process alive?
///
/// Unauthenticated, and unconditionally `200` while the process can answer at
/// all. It deliberately says nothing about whether the server should be sent
/// work: that is [`handle_ready`]'s question, and conflating the two is what
/// makes an orchestrator either restart a draining instance (reading
/// not-ready as dead) or keep routing to it (having no way to ask).
pub(crate) async fn handle_health(res: &mut Responder<'_>) -> std::io::Result<()> {
    res.json("200 OK", br#"{"status":"ok"}"#).await
}

/// `GET /readyz` — readiness. Is it safe to send this process new work?
///
/// `200` only while serving; `503` while starting up and while draining. The
/// status code is the part an orchestrator acts on, so it carries the whole
/// signal — the `state` field is for the human reading the probe by hand,
/// which is why it names the phase rather than repeating the boolean.
pub(crate) async fn handle_ready(
    res: &mut Responder<'_>,
    state: &ServerState,
) -> std::io::Result<()> {
    let readiness = state.lifecycle().readiness();
    let body = format!(r#"{{"state":"{}"}}"#, readiness.as_str());
    let status = if readiness.is_ready() {
        "200 OK"
    } else {
        "503 Service Unavailable"
    };
    res.json(status, body.as_bytes()).await
}

/// `GET /v1/metrics` — the counters, behind the same bearer token as everything
/// else.
///
/// Authenticated on purpose (#930 asks for exactly this): occupancy, 401 counts
/// and reverse-request timings describe the host's traffic, and an open metrics
/// endpoint is a free reconnaissance surface. Pull, never push — nothing here
/// dials out, per AGENTS.md invariant 3.
pub(crate) async fn handle_metrics(
    res: &mut Responder<'_>,
    state: &ServerState,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(&state.metrics().snapshot()).unwrap_or_default();
    res.json("200 OK", &body).await
}

/// `GET /v1/calibration` — the cost-drift report (#1298).
///
/// What the engine estimated a request's input tokens to be, against what the
/// provider said it actually billed, per `(provider_id, model)`. A host that
/// cannot read this believes its own cost numbers until the invoice disagrees.
///
/// Read-only and authenticated, like `/v1/metrics` and for the same reason:
/// token volumes and model ids describe the host's traffic. It carries no
/// transcript content — see `crate::calibration` for the shape and for what
/// the numbers do and do not claim.
pub(crate) async fn handle_calibration(
    res: &mut Responder<'_>,
    state: &ServerState,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(&state.calibration().report()).unwrap_or_default();
    res.json("200 OK", &body).await
}

pub(crate) async fn handle_create(
    res: &mut Responder<'_>,
    state: &Arc<ServerState>,
    body: &[u8],
) -> std::io::Result<()> {
    let turn: TurnRequest = match serde_json::from_slice(body) {
        Ok(turn) => turn,
        Err(err) => {
            return res
                .json(
                    "400 Bad Request",
                    &error_body(&format!("invalid turn request: {err}")),
                )
                .await;
        }
    };

    let mut config = EngineConfig::default();
    let clamped = match &turn.engine {
        Some(engine) => match apply_engine_overrides(&mut config, engine) {
            Ok(clamped) => clamped,
            Err(message) => {
                return res.json("400 Bad Request", &error_body(&message)).await;
            }
        },
        None => Vec::new(),
    };
    if let Some(max_steps) = turn.max_steps {
        let Some(effective) = validate_max_steps(max_steps) else {
            return res
                .json(
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
            return res
                .json(
                    "400 Bad Request",
                    &error_body("reverse_request_timeout_ms must be at least 1"),
                )
                .await;
        };
        reverse_request_timeout = effective;
    }

    // Admission, reclamation and registration all happen under one lock hold
    // inside `register_turn` — see there for why. The closure receives the
    // minted id so the session can carry its own `TurnRef` into every record it
    // will emit: the id has to exist before the session starts, and only the
    // registry can mint it.
    let observer = state.observer().clone();
    // A stateless turn keys on its own id: the only name it will ever have,
    // and one the host is holding as soon as this call returns. Minted inside
    // the closure because only `register_turn` — under the registry lock —
    // can mint it.
    let checkpoint_state = Arc::clone(state);
    let extensions = state.extensions();
    // Keyed on the declared provider so two providers serving the same model
    // name never blend their drift — see `crate::calibration`.
    let calibration = Some(state.calibration().for_provider(&turn.provider_id));
    let registered = state.register_turn(move |turn_id| {
        Session::start(SessionSpec {
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
            turn: TurnRef::new(turn_id),
            observer,
            on_settled: None,
            checkpoint: checkpoint_state.checkpoint_for(turn_id),
            extensions,
            calibration,
        })
    });
    let Some(id) = registered else {
        return res
            .json_with_headers(
                "429 Too Many Requests",
                &[("Retry-After", RETRY_AFTER_SECS)],
                &error_body("too many live turns; retry after in-flight turns finish"),
            )
            .await;
    };

    let body = serde_json::to_vec(&TurnCreated {
        turn_id: &id,
        clamped,
    })
    .unwrap_or_default();
    res.json("200 OK", &body).await
}

pub(crate) async fn handle_events(
    res: &mut Responder<'_>,
    record: &mut RequestRecord,
    state: &Arc<ServerState>,
    id: &str,
    after: Option<u64>,
) -> std::io::Result<()> {
    let Some(entry) = state.lookup(id) else {
        return res.json("404 Not Found", &error_body("unknown turn")).await;
    };
    // Take the session out in its own scope so the (non-`Send`) mutex guard is
    // dropped before any `.await` — the connection future must stay `Send`.
    // The generation bump rides the same critical section: it is what a reaper
    // armed by an earlier disconnect compares against to know this turn has
    // been resumed and must not be cancelled.
    let taken = {
        let mut slot = entry.session.lock().unwrap_or_else(|p| p.into_inner());
        let taken = slot.take();
        if taken.is_some() {
            entry.stream_generation.fetch_add(1, Ordering::AcqRel);
        }
        taken
    };
    let generation = entry.stream_generation.load(Ordering::Acquire);
    let Some(mut session) = taken else {
        // No session to stream. A turn that already finished still has its
        // retained tail, so a client reconnecting a moment too late gets the
        // frames it missed instead of a bare conflict — which is the whole
        // point of retaining them.
        return match after {
            Some(after) => replay_only(res, record, state, id, &entry.history, after).await,
            None => {
                res.json(
                    "409 Conflict",
                    &error_body("events are already being streamed for this turn"),
                )
                .await
            }
        };
    };

    // From here the session is out of the registry entry and owned by this
    // connection, so every exit path must also drop the registry entry —
    // otherwise a turn whose stream never opened lingers in the map forever.
    if let Err(err) = res.sse_head().await {
        state.turns().remove(id);
        return Err(err);
    }

    let turn = TurnRef::new(id);
    let mut frames_sent = 0_u64;
    let mut bytes_out = 0_u64;

    // Replay before live. A resuming client must see the frames it missed in
    // stream order before any new one, or its own reconstruction of the turn
    // interleaves wrongly.
    if let Some(after) = after
        && let Some(reason) =
            write_replay(res, &entry.history, after, &mut frames_sent, &mut bytes_out).await
    {
        res.add_bytes(bytes_out);
        record.set_turn(id);
        state.observer().emit(&ServeEvent::StreamEnded {
            turn,
            frames_sent,
            reason,
        });
        state.park_for_resume(id, &entry, session, generation);
        return res.stream_mut().shutdown().await;
    }

    let reason = stream_frames(
        res,
        state,
        &mut session,
        &turn,
        &mut frames_sent,
        &mut bytes_out,
    )
    .await;
    res.add_bytes(bytes_out);
    record.set_turn(id);

    // A peer that vanished may be back: park the session so a reconnect can
    // resume from its retained tail, and let the grace window — not this
    // connection — decide when the turn is definitively abandoned. Every other
    // ending is final, so the session drops here and `Drop for Session`
    // cancels the turn, releasing its thread now rather than at end of scope.
    //
    // Asked of the reason rather than compared against one variant: a hang-up
    // surfaces as an EOF *or* as the failed write that follows it, whichever
    // the `select!` in `stream_frames` happens to see first, and both have to
    // land here. See `StreamEndReason::leaves_the_turn_resumable`.
    if reason.leaves_the_turn_resumable() {
        state.park_for_resume(id, &entry, session, generation);
    } else {
        drop(session);
        state.turns().remove(id);
    }

    // Announced *after* the turn has been disposed of, not before. The event is
    // the only signal an observer gets that a subscriber went away, and the
    // obvious thing to do with it is reconnect — so emitting it first published
    // a moment in which the turn was neither streamed nor yet parked, and a
    // reconnect arriving inside that window took the `replay_only` path: it got
    // the retained tail and an immediately-closed stream, with no live source
    // behind it. Ordering the emit last makes the event describe a state the
    // server is already in, which is what makes acting on it deterministic.
    state.observer().emit(&ServeEvent::StreamEnded {
        turn,
        frames_sent,
        reason,
    });
    res.stream_mut().shutdown().await
}

/// Serve a reconnect for a turn whose session is gone — finished, or being
/// streamed by someone else — from its retained tail alone.
///
/// The stream ends immediately after the replay: there is no live source to
/// follow it with. A client whose turn had already completed therefore sees
/// its `turn_complete` and stops, which is exactly right.
async fn replay_only(
    res: &mut Responder<'_>,
    record: &mut RequestRecord,
    state: &Arc<ServerState>,
    id: &str,
    history: &FrameHistory,
    after: u64,
) -> std::io::Result<()> {
    res.sse_head().await?;
    let mut frames_sent = 0_u64;
    let mut bytes_out = 0_u64;
    let reason = write_replay(res, history, after, &mut frames_sent, &mut bytes_out)
        .await
        .unwrap_or(StreamEndReason::TurnComplete);
    res.add_bytes(bytes_out);
    record.set_turn(id);
    state.observer().emit(&ServeEvent::StreamEnded {
        turn: TurnRef::new(id),
        frames_sent,
        reason,
    });
    res.stream_mut().shutdown().await
}

/// Write every retained frame after `after`.
///
/// Returns `None` when the replay completed and the caller should continue
/// with the live stream, or `Some(reason)` when the stream ended during it.
///
/// A resume point that has fallen out of the retention window is answered with
/// an explicit error frame rather than by quietly starting from the oldest
/// frame still held. A silent jump is undetectable by the client, and the
/// frames it would skip are the tool requests and completions the host
/// reconciles its own state against — so a hole there is worse than a failure.
async fn write_replay(
    res: &mut Responder<'_>,
    history: &FrameHistory,
    after: u64,
    frames_sent: &mut u64,
    bytes_out: &mut u64,
) -> Option<StreamEndReason> {
    let frames = match history.replay_after(after) {
        Replay::Frames(frames) => frames,
        Replay::Truncated { oldest } => {
            let json = serde_json::to_string(&ReplayTruncated {
                kind: "replay_truncated",
                requested_after: after,
                oldest_retained: oldest,
            })
            .unwrap_or_else(|_| {
                r#"{"type":"replay_truncated","requested_after":0,"oldest_retained":0}"#.to_string()
            });
            let (_, mut out) = res.stream_mut().split();
            return match write_sse_frame(&mut out, &json).await {
                Ok(written) => {
                    *frames_sent += 1;
                    *bytes_out = bytes_out.saturating_add(written);
                    Some(StreamEndReason::TurnComplete)
                }
                Err(_) => Some(StreamEndReason::WriteFailed),
            };
        }
    };
    let (_, mut out) = res.stream_mut().split();
    for (seq, json) in frames {
        match write_sse_event(&mut out, seq, &json).await {
            Ok(written) => {
                *frames_sent += 1;
                *bytes_out = bytes_out.saturating_add(written);
            }
            Err(_) => return Some(StreamEndReason::WriteFailed),
        }
    }
    None
}

/// The frame sent when a client's resume point has already been evicted.
///
/// Its own `type`, not a `ServerFrame` variant: this is a statement about the
/// *transport* — what the server can no longer supply — not something the
/// engine produced, and folding it into the engine's frame vocabulary would
/// oblige every non-resuming consumer to handle a case it can never see.
#[derive(Serialize)]
struct ReplayTruncated {
    #[serde(rename = "type")]
    kind: &'static str,
    /// The `seq` the client asked to resume after.
    requested_after: u64,
    /// The oldest `seq` the server still holds.
    oldest_retained: u64,
}

/// The SSE frame loop, split out so `handle_events` can report *why* it ended.
///
/// Watching the read half while waiting is not an optimization. A turn parked on
/// a reverse request produces nothing to write, so a host that vanished would
/// otherwise go unnoticed until the reverse-request deadline expired — up to an
/// hour of an OS thread and a registry slot held for a client that is provably
/// gone, with the host billed for whatever the engine went on to ask for.
async fn stream_frames(
    res: &mut Responder<'_>,
    state: &Arc<ServerState>,
    session: &mut Session,
    turn: &TurnRef,
    frames_sent: &mut u64,
    bytes_out: &mut u64,
) -> StreamEndReason {
    let (mut peer, mut out) = res.stream_mut().split();
    let mut scratch = [0_u8; 1024];
    // A GET on a `Connection: close` stream has nothing left to send, so
    // inbound bytes are a protocol violation. Tolerate a little (a client
    // library that pipelines a probe) but never spin discarding an unbounded
    // stream of them.
    let mut stray = 0_usize;
    const MAX_STRAY_BYTES: usize = 8 * 1024;
    loop {
        let frame = tokio::select! {
            read = peer.read(&mut scratch) => match read {
                // EOF or a reset: the subscriber is gone. Stop streaming;
                // dropping `session` cancels the turn.
                Ok(0) | Err(_) => return StreamEndReason::PeerDisconnected,
                Ok(n) => {
                    stray += n;
                    if stray > MAX_STRAY_BYTES {
                        return StreamEndReason::StrayBytes;
                    }
                    continue;
                }
            },
            frame = session.next_seq_frame() => match frame {
                Some(frame) => frame,
                None => return StreamEndReason::SessionEnded,
            },
        };
        if let Some(error) = frame.unencodable {
            // The turn just died of a bug in our own serialization. It used to
            // say nothing at all — the host got a synthesized terminal frame
            // and the server kept no record that it had happened.
            state.observer().emit(&ServeEvent::FrameUnencodable {
                turn: turn.clone(),
                error,
            });
        }
        match write_sse_event(&mut out, frame.seq, &frame.json).await {
            Ok(written) => {
                *frames_sent += 1;
                *bytes_out = bytes_out.saturating_add(written);
            }
            Err(_) => return StreamEndReason::WriteFailed,
        }
        if frame.terminal {
            return StreamEndReason::TurnComplete;
        }
    }
}

pub(crate) async fn handle_tool_result(
    res: &mut Responder<'_>,
    state: &Arc<ServerState>,
    id: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let Some(entry) = state.lookup(id) else {
        return res.json("404 Not Found", &error_body("unknown turn")).await;
    };
    let result: ToolResultIn = match serde_json::from_slice(body) {
        Ok(result) => result,
        Err(err) => {
            return res
                .json(
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
        Ok(()) => res.json("200 OK", br#"{"status":"ok"}"#).await,
        Err(err) => {
            res.json("409 Conflict", &error_body(&err.to_string()))
                .await
        }
    }
}

pub(crate) async fn handle_provider_result(
    res: &mut Responder<'_>,
    state: &Arc<ServerState>,
    id: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let Some(entry) = state.lookup(id) else {
        return res.json("404 Not Found", &error_body("unknown turn")).await;
    };
    let posted: ProviderResultIn = match serde_json::from_slice(body) {
        Ok(posted) => posted,
        Err(err) => {
            return res
                .json(
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
        Ok(()) => res.json("200 OK", br#"{"status":"ok"}"#).await,
        Err(err) => {
            res.json("409 Conflict", &error_body(&err.to_string()))
                .await
        }
    }
}

/// `POST /v1/turns/{id}/provider-delta` — streamed fragments for an in-flight
/// provider request (#1165), ahead of its terminating `provider-result`.
///
/// Optional: a host that cannot stream never calls this and keeps its old
/// behavior. Batched: one POST carries any number of fragments, because a
/// per-token HTTP request would cost more than the latency it buys. The
/// fragments surface on `/events` as `TextDelta` / `Reasoning` frames, so a
/// second subscriber sees text as it arrives and a resuming client replays it
/// — and each batch resets the reverse-request deadline, which is what makes
/// that deadline an idle bound for a host that streams.
pub(crate) async fn handle_provider_delta(
    res: &mut Responder<'_>,
    state: &Arc<ServerState>,
    id: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let Some(entry) = state.lookup(id) else {
        return res.json("404 Not Found", &error_body("unknown turn")).await;
    };
    let posted: ProviderDeltaIn = match serde_json::from_slice(body) {
        Ok(posted) => posted,
        Err(err) => {
            return res
                .json(
                    "400 Bad Request",
                    &error_body(&format!("invalid provider delta: {err}")),
                )
                .await;
        }
    };
    if posted.deltas.is_empty() {
        // Refused rather than accepted as a keepalive: an empty batch sends
        // nothing through the feed, so it would NOT reset the idle deadline —
        // accepting it would let a host believe it was proving liveness while
        // the clock ran out.
        return res
            .json(
                "400 Bad Request",
                &error_body("deltas must carry at least one fragment"),
            )
            .await;
    }
    match entry
        .pending
        .resolve_provider_delta(&posted.request_id, posted.deltas)
    {
        Ok(()) => res.json("200 OK", br#"{"status":"ok"}"#).await,
        Err(err) => {
            res.json("409 Conflict", &error_body(&err.to_string()))
                .await
        }
    }
}

/// `POST /v1/turns/{id}/cancel` — end an in-flight turn.
///
/// Answers once the turn is *signalled*, not once it has unwound: the parked
/// engine step wakes immediately, but the turn still needs a moment to produce
/// its terminal frame, and a host streaming `/events` is the one that observes
/// that. Blocking this response on it would deadlock a single-connection client.
pub(crate) async fn handle_cancel(
    res: &mut Responder<'_>,
    state: &Arc<ServerState>,
    id: &str,
) -> std::io::Result<()> {
    // Remove and signal, so a second cancel — or a late result POST — gets an
    // honest 404 rather than silently doing nothing. Scoped so the (non-`Send`)
    // guard is dropped before the await below.
    let removed = { state.turns().remove(id) };
    let Some(entry) = removed else {
        return res.json("404 Not Found", &error_body("unknown turn")).await;
    };
    // The step-boundary stop (#1129). Before this existed, cancelling a turn
    // that was mid-compaction or mid-loop-detection did nothing until it next
    // parked on a reverse request — the turn ended because the *host* stopped
    // answering, not because the engine was asked to stop. Now the engine
    // reads this latch at the top of every step and unwinds there, keeping
    // every completed step and leaving a transcript valid to hand back.
    entry.cancel.cancel();
    entry.pending.cancel();
    // A paused turn is parked on its gate, not on a reverse request, so the
    // cancel alone cannot wake it — and while a stream holds this entry alive,
    // neither can the sender-drop fallback. The release is what lets a
    // paused-then-cancelled turn observe the latch and unwind (`crate::controls`).
    entry.controls.resume();
    // Dropping our `Arc` here is what reclaims a turn whose stream never opened:
    // the registry no longer holds it, so this may be the last handle, and
    // `Drop for Session` releases the engine thread. A turn that *is* streaming
    // keeps its own handle and unwinds through `handle_events` as usual.
    drop(entry);
    res.json("200 OK", br#"{"status":"cancelled"}"#).await
}

/// Whether this turn is known to have finished — its session is parked in the
/// entry and its thread is done. `false` when the turn is live *or* currently
/// being streamed (the session is checked out, so there is nothing to ask);
/// a control request in that window is accepted and simply arrives too late
/// to matter, which is inherent to steering a running turn.
fn known_finished(entry: &crate::server::Entry) -> bool {
    match entry.session.try_lock() {
        Ok(slot) => slot.as_ref().is_some_and(Session::is_finished),
        Err(std::sync::TryLockError::WouldBlock) => false,
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned
            .into_inner()
            .as_ref()
            .is_some_and(Session::is_finished),
    }
}

/// Body of `POST /v1/turns/{id}/steer`.
#[derive(Debug, Deserialize)]
struct SteerIn {
    /// The user message to inject at the turn's next step boundary.
    message: String,
}

/// `POST /v1/turns/{id}/steer` — queue a mid-turn user message (#932).
///
/// The engine injects it at the next step boundary and echoes it on the event
/// stream as `AgentEvent::Steered`, so a streaming host sees exactly when its
/// message landed. Best-effort by nature: a turn that completes before its
/// next boundary never observes the message — the same race the CLI's `>`
/// steer has, and one the wire cannot close.
pub(crate) async fn handle_steer(
    res: &mut Responder<'_>,
    state: &Arc<ServerState>,
    id: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let Some(entry) = state.lookup(id) else {
        return res.json("404 Not Found", &error_body("unknown turn")).await;
    };
    let steer: SteerIn = match serde_json::from_slice(body) {
        Ok(steer) => steer,
        Err(err) => {
            return res
                .json(
                    "400 Bad Request",
                    &error_body(&format!("invalid steer request: {err}")),
                )
                .await;
        }
    };
    if steer.message.is_empty() {
        return res
            .json("400 Bad Request", &error_body("message must not be empty"))
            .await;
    }
    if known_finished(&entry) {
        return res
            .json("409 Conflict", &error_body("turn already finished"))
            .await;
    }
    entry.controls.steer(steer.message);
    res.json("200 OK", br#"{"status":"queued"}"#).await
}

/// Body of `POST /v1/turns/{id}/pause`. Optional in its entirety — an empty
/// body is the plain "just hold" the route shipped with.
#[derive(Debug, Default, Deserialize)]
struct PauseIn {
    /// Why the turn is being held, carried onto the `turn_held` frame.
    ///
    /// The client that reconnects mid-hold need not be the process that
    /// paused, so the reason has to live on the stream rather than in the
    /// pausing caller's memory.
    #[serde(default)]
    reason: Option<String>,
}

/// `POST /v1/turns/{id}/pause` — hold the turn at its next step boundary
/// (#932). Idempotent. Step granularity, never mid-tool: a step already in
/// flight (a parked reverse request included) finishes first.
///
/// The turn announces the hold on its event stream as a `turn_held` frame when
/// it actually reaches the boundary — which is later than this response, and
/// is the frame a reconnecting subscriber replays to re-learn that the turn is
/// waiting on it (`crate::controls`).
pub(crate) async fn handle_pause(
    res: &mut Responder<'_>,
    state: &Arc<ServerState>,
    id: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let Some(entry) = state.lookup(id) else {
        return res.json("404 Not Found", &error_body("unknown turn")).await;
    };
    // Empty is the documented shape and stays free: only a client that wrote
    // something is held to it being JSON.
    let pause: PauseIn = if body.iter().all(u8::is_ascii_whitespace) {
        PauseIn::default()
    } else {
        match serde_json::from_slice(body) {
            Ok(pause) => pause,
            Err(err) => {
                return res
                    .json(
                        "400 Bad Request",
                        &error_body(&format!("invalid pause request: {err}")),
                    )
                    .await;
            }
        }
    };
    if known_finished(&entry) {
        return res
            .json("409 Conflict", &error_body("turn already finished"))
            .await;
    }
    entry.controls.pause(pause.reason);
    res.json("200 OK", br#"{"status":"paused"}"#).await
}

/// `POST /v1/turns/{id}/resume` — release a held turn (#932). Idempotent:
/// resuming a turn that was never paused is a no-op, not an error.
pub(crate) async fn handle_resume(
    res: &mut Responder<'_>,
    state: &Arc<ServerState>,
    id: &str,
) -> std::io::Result<()> {
    let Some(entry) = state.lookup(id) else {
        return res.json("404 Not Found", &error_body("unknown turn")).await;
    };
    if known_finished(&entry) {
        return res
            .json("409 Conflict", &error_body("turn already finished"))
            .await;
    }
    entry.controls.resume();
    res.json("200 OK", br#"{"status":"resumed"}"#).await
}

// ── /v1/sessions: server-owned conversations (#931) ──────────────────────────
//
// The handlers are the thin part; the state machine (checkout, settle-back,
// retention) lives in `crate::sessions` where it is unit-tested without a
// socket. What belongs *here* is the wire shape and the status codes.

/// Body of `POST /v1/sessions`.
#[derive(Debug, Deserialize)]
struct SessionCreateRequest {
    /// Message index 0 for every turn of this session — minted once, held
    /// byte-identical thereafter. That is the prompt-cache stability contract:
    /// the engine never touches index 0, so every turn reopens the same cached
    /// prefix. (The CLI re-mints the system prompt on `stella resume` because
    /// a new process has a cold cache anyway; a hot server session must not.)
    system_prompt: String,
    /// Spend policy for the whole session. `session_limit_usd` finally binds
    /// across turns here — the session owns one guard and threads it through
    /// every turn, unlike the stateless route's fresh-guard-per-turn.
    #[serde(default)]
    budget: BudgetSpec,
}

/// Response to `POST /v1/sessions`.
#[derive(Debug, Serialize)]
struct SessionCreated<'a> {
    session_id: &'a str,
}

/// Body of `POST /v1/sessions/{id}/turns` — the stateless `TurnRequest`,
/// minus `messages` (the session owns the history) and minus `budget` (the
/// session owns the guard).
#[derive(Debug, Deserialize)]
struct SessionTurnRequest {
    provider_id: String,
    #[serde(default)]
    tools: Vec<ToolSchema>,
    /// This turn's new input, appended to the session history before the turn
    /// runs — typically one user message.
    input: Vec<CompletionMessage>,
    #[serde(default)]
    max_steps: Option<usize>,
    #[serde(default)]
    reverse_request_timeout_ms: Option<u64>,
    /// Per-turn engine knobs (#1167), applied on top of the defaults exactly
    /// as on the stateless route. Safe for the session's prompt-cache
    /// contract: none of these knobs touch the transcript, so the byte-stable
    /// prefix survives a turn that runs at a different temperature.
    #[serde(default)]
    engine: Option<EngineOverrides>,
}

/// Response to `POST /v1/sessions/{id}/turns`. The turn is an ordinary member
/// of `/v1/turns/{id}/...` — events, results, cancel, steer and pause all
/// address it by `turn_id` exactly as they would a stateless turn.
#[derive(Debug, Serialize)]
struct SessionTurnCreated<'a> {
    turn_id: &'a str,
    session_id: &'a str,
    /// Engine knobs the server lowered below what was asked (#1167). Absent
    /// when nothing was clamped, so pre-existing clients parse unchanged.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    clamped: Vec<ClampedKnob>,
}

/// Response to `GET /v1/sessions/{id}` — the last settled state, served even
/// while a turn is running (see `crate::sessions` on checkout semantics).
#[derive(Debug, Serialize)]
struct SessionInfo<'a> {
    session_id: &'a str,
    turns_completed: u64,
    turns_aborted: u64,
    /// Session-axis spend to date, aborted turns included.
    cost_usd: f64,
    /// The id of the turn currently running in this session, if any — the
    /// handle a reconnecting host needs to rejoin its event stream.
    live_turn: Option<String>,
    /// Whether that live turn has been asked to hold (#932).
    ///
    /// The point of reporting it here is that a host does not have to replay
    /// a stream to find out. A reconnecting process reads one `GET` and knows
    /// whether the silence on `/events` is a turn thinking or a turn waiting
    /// on *it* — the difference between waiting longer and posting a resume.
    /// `false` when there is no live turn.
    held: bool,
    /// The retained conversation, compaction rewrites included.
    messages: Vec<CompletionMessage>,
}

/// `POST /v1/sessions` — open a server-owned conversation.
pub(crate) async fn handle_session_create(
    res: &mut Responder<'_>,
    state: &Arc<ServerState>,
    body: &[u8],
) -> std::io::Result<()> {
    let request: SessionCreateRequest = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(err) => {
            return res
                .json(
                    "400 Bad Request",
                    &error_body(&format!("invalid session request: {err}")),
                )
                .await;
        }
    };
    let history = vec![CompletionMessage::system(request.system_prompt)];
    let budget = BudgetGuard::new(
        request.budget.mode,
        request.budget.turn_limit_usd,
        request.budget.session_limit_usd,
    );
    let registered =
        state
            .sessions()
            .register(history, budget, state.session_idle_ttl(), state.observer());
    let Some(id) = registered else {
        return res
            .json_with_headers(
                "429 Too Many Requests",
                &[("Retry-After", RETRY_AFTER_SECS)],
                &error_body("too many sessions; delete sessions you are done with"),
            )
            .await;
    };
    let body = serde_json::to_vec(&SessionCreated { session_id: &id }).unwrap_or_default();
    res.json("200 OK", &body).await
}

/// `POST /v1/sessions/{id}/turns` — run the next turn of a session.
///
/// The reservation-token choreography (reserve → snapshot → register → bind,
/// with release/settle racing all of it) is documented in `crate::sessions`.
/// What matters here: no session lock is held across `register_turn`, and the
/// turn itself is a first-class member of the turn registry, so the live-turn
/// cap applies and every `/v1/turns/{id}/...` verb works on it.
pub(crate) async fn handle_session_turn(
    res: &mut Responder<'_>,
    state: &Arc<ServerState>,
    id: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let Some(sess) = state.sessions().lookup(id) else {
        return res
            .json("404 Not Found", &error_body("unknown session"))
            .await;
    };
    let request: SessionTurnRequest = match serde_json::from_slice(body) {
        Ok(request) => request,
        Err(err) => {
            return res
                .json(
                    "400 Bad Request",
                    &error_body(&format!("invalid turn request: {err}")),
                )
                .await;
        }
    };
    if request.input.is_empty() {
        return res
            .json(
                "400 Bad Request",
                &error_body("input must carry at least one message"),
            )
            .await;
    }
    let mut config = EngineConfig::default();
    let clamped = match &request.engine {
        Some(engine) => match apply_engine_overrides(&mut config, engine) {
            Ok(clamped) => clamped,
            Err(message) => {
                return res.json("400 Bad Request", &error_body(&message)).await;
            }
        },
        None => Vec::new(),
    };
    if let Some(max_steps) = request.max_steps {
        let Some(effective) = validate_max_steps(max_steps) else {
            return res
                .json(
                    "400 Bad Request",
                    &error_body("max_steps must be at least 1"),
                )
                .await;
        };
        config.max_steps = effective;
    }
    let mut reverse_request_timeout = SessionSpec::DEFAULT_REVERSE_REQUEST_TIMEOUT;
    if let Some(requested_ms) = request.reverse_request_timeout_ms {
        let Some(effective) = validate_reverse_request_timeout(requested_ms) else {
            return res
                .json(
                    "400 Bad Request",
                    &error_body("reverse_request_timeout_ms must be at least 1"),
                )
                .await;
        };
        reverse_request_timeout = effective;
    }

    let Some(token) = sess.reserve() else {
        return res
            .json(
                "409 Conflict",
                &error_body("a turn is already running in this session"),
            )
            .await;
    };
    let (mut messages, budget) = sess.snapshot();
    messages.extend(request.input);

    let observer = state.observer().clone();
    // Keyed on the **session** id, not the turn's. Two reasons, and both are
    // about who is asking after the crash: the session id is the name the host
    // kept, and a session admits one live turn at a time, so the key has one
    // writer and each turn's resume point cleanly supersedes the last.
    let checkpoint = state.checkpoint_for(id);
    // The settle hook is minted *inside* the closure, not before the
    // `register_turn` call: a capacity-refused registration drops the closure
    // uncalled, and a hook minted early would fire its died-without-settling
    // path for a turn that never existed — miscounting an abort and racing
    // the `release` below.
    let hook_sess = Arc::clone(&sess);
    let extensions = state.extensions();
    let calibration = Some(state.calibration().for_provider(&request.provider_id));
    let registered = state.register_turn(move |turn_id| {
        Session::start(SessionSpec {
            provider_id: request.provider_id,
            tools: request.tools,
            messages,
            config,
            budget,
            reverse_request_timeout,
            turn: TurnRef::new(turn_id),
            observer,
            on_settled: Some(hook_sess.settle_hook(token)),
            checkpoint,
            extensions,
            calibration,
        })
    });
    let Some(turn_id) = registered else {
        // The turn registry is full — the reservation ends without a turn
        // ever having existed, so the session is immediately usable again.
        sess.release(token);
        return res
            .json_with_headers(
                "429 Too Many Requests",
                &[("Retry-After", RETRY_AFTER_SECS)],
                &error_body("too many live turns; retry after in-flight turns finish"),
            )
            .await;
    };
    sess.bind_turn(token, &turn_id);
    let body = serde_json::to_vec(&SessionTurnCreated {
        turn_id: &turn_id,
        session_id: id,
        clamped,
    })
    .unwrap_or_default();
    res.json("200 OK", &body).await
}

/// `GET /v1/sessions/{id}` — history, cost to date, and the live turn if any.
pub(crate) async fn handle_session_get(
    res: &mut Responder<'_>,
    state: &Arc<ServerState>,
    id: &str,
) -> std::io::Result<()> {
    let Some(sess) = state.sessions().lookup(id) else {
        return res
            .json("404 Not Found", &error_body("unknown session"))
            .await;
    };
    let view = sess.view();
    // A live turn that is no longer in the registry (cancelled, evicted) is
    // not held — there is nothing left to release.
    let held = view
        .live_turn
        .as_deref()
        .and_then(|turn_id| state.lookup(turn_id))
        .is_some_and(|entry| entry.controls.is_paused());
    let body = serde_json::to_vec(&SessionInfo {
        session_id: id,
        turns_completed: view.turns_completed,
        turns_aborted: view.turns_aborted,
        cost_usd: view.cost_usd,
        live_turn: view.live_turn,
        held,
        messages: view.history,
    })
    .unwrap_or_default();
    res.json("200 OK", &body).await
}

/// `DELETE /v1/sessions/{id}` — end a session, cancelling its live turn.
///
/// The removal happens first, so a concurrent `POST .../turns` racing this
/// delete loses cleanly (its lookup finds nothing and answers `404`). A turn
/// already registered keeps running only long enough to unwind: it is
/// cancelled here exactly the way `POST /v1/turns/{id}/cancel` would, and its
/// settlement lands on the detached entry, harmlessly.
pub(crate) async fn handle_session_delete(
    res: &mut Responder<'_>,
    state: &Arc<ServerState>,
    id: &str,
) -> std::io::Result<()> {
    let Some(sess) = state.sessions().remove(id, state.observer()) else {
        return res
            .json("404 Not Found", &error_body("unknown session"))
            .await;
    };
    if let Some(turn_id) = sess.live_turn_id() {
        // Scoped so the registry guard drops before the awaits below; the
        // session's own locks are not held here (`live_turn_id` returned).
        let removed = { state.turns().remove(&turn_id) };
        if let Some(entry) = removed {
            // Same three signals as `handle_cancel`: without the step-boundary
            // latch (#1129), a turn deleted mid-compaction kept computing until
            // it next parked on a reverse request and only unwound when
            // `Pending` refused the registration.
            entry.cancel.cancel();
            entry.pending.cancel();
            entry.controls.resume();
        }
    }
    // The conversation is gone, so its resume point is unreachable work: the
    // key is the session id, and that id now answers `404` everywhere else.
    // Leaving it would make `DELETE` the one operation that grows the store.
    if let Some(store) = state.checkpoints()
        && let Ok(key) = crate::checkpoint::CheckpointKey::new(id)
    {
        let _ = store.remove(&key);
    }
    res.json("200 OK", br#"{"status":"deleted"}"#).await
}

/// `GET /v1/sessions/{id}/checkpoint` and `GET /v1/turns/{id}/checkpoint` —
/// read back a resume point (#1198).
///
/// # Why this does not consult either registry
///
/// The id is looked up in the **store**, not in `state.sessions()` or
/// `state.turns()`, and the difference is the entire point: the interesting
/// caller is one whose session object died with the process it was living in.
/// A `GET` gated on the registry would answer `404` exactly when the answer
/// matters and `200` only while the in-memory copy made it redundant.
///
/// So `200` here does not mean "this session exists" — it means "this id has a
/// resume point", and those are deliberately different questions. A live
/// session and a crashed one are indistinguishable on this route, which is
/// what lets a host recover without first knowing whether it needs to.
///
/// # The body is the stored bytes, verbatim
///
/// Not re-encoded, not wrapped in an envelope. `stella_core::step::Checkpoint`
/// is a struct precisely so its JSON key order is its declaration order —
/// this workspace's `serde_json` has no `preserve_order`, so parsing the
/// stored text into a `Value` and re-serializing it would come back
/// **alphabetized**, breaking the byte-identity that
/// `checkpoint_round_trips_byte_identically` pins in `stella-core`. Passing
/// the bytes through is both cheaper and the only version that cannot corrupt
/// a resume point on the way out.
pub(crate) async fn handle_checkpoint_get(
    res: &mut Responder<'_>,
    state: &Arc<ServerState>,
    id: &str,
) -> std::io::Result<()> {
    match read_checkpoint(state, id) {
        Ok(Some(json)) => res.json("200 OK", json.as_bytes()).await,
        Ok(None) => {
            res.json("404 Not Found", &error_body("no checkpoint for this id"))
                .await
        }
        Err(err) => {
            res.json(
                "500 Internal Server Error",
                &error_body(&format!("checkpoint could not be read: {err}")),
            )
            .await
        }
    }
}

/// `DELETE /v1/sessions/{id}/checkpoint` and `DELETE /v1/turns/{id}/checkpoint`
/// — reclaim a resume point a host is finished with.
///
/// This exists because nothing else can do it. A turn that reaches a terminal
/// outcome discards its own resume point, so the only entries a store retains
/// are the turns that died *without* ending — and no timer can tell one whose
/// host is coming back from one whose host is gone. That judgment belongs to
/// the host, so it gets a verb.
///
/// Idempotent, like the `discard` half of the sink contract it mirrors:
/// deleting what is not there is a `200`, because a host recovering from a
/// crash cannot be expected to know which of its turns got far enough to write
/// one.
pub(crate) async fn handle_checkpoint_delete(
    res: &mut Responder<'_>,
    state: &Arc<ServerState>,
    id: &str,
) -> std::io::Result<()> {
    let Some(store) = state.checkpoints() else {
        return res
            .json(
                "404 Not Found",
                &error_body("this server is not storing checkpoints"),
            )
            .await;
    };
    // An unkeyable id has no entry by construction, so "deleted" is true.
    let Ok(key) = crate::checkpoint::CheckpointKey::new(id) else {
        return res.json("200 OK", br#"{"status":"deleted"}"#).await;
    };
    match store.remove(&key) {
        Ok(()) => res.json("200 OK", br#"{"status":"deleted"}"#).await,
        Err(err) => {
            res.json(
                "500 Internal Server Error",
                &error_body(&format!("checkpoint could not be removed: {err}")),
            )
            .await
        }
    }
}

/// The store read behind [`handle_checkpoint_get`], with "durability is off"
/// and "that is not a key" both folded into "nothing stored" — neither is a
/// server fault, and both are honestly `404`.
fn read_checkpoint(
    state: &Arc<ServerState>,
    id: &str,
) -> Result<Option<String>, crate::checkpoint::CheckpointStoreError> {
    let Some(store) = state.checkpoints() else {
        return Ok(None);
    };
    let Ok(key) = crate::checkpoint::CheckpointKey::new(id) else {
        return Ok(None);
    };
    store.get(&key)
}

pub(crate) fn error_body(message: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "error": message })).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zero step cap produces a turn that aborts before it runs, blaming a
    /// cap the caller never meant to set. Refusing it names the real problem.
    #[test]
    fn zero_steps_is_refused_rather_than_silently_accepted() {
        assert_eq!(validate_max_steps(0), None);
        assert_eq!(validate_max_steps(1), Some(1));
    }

    #[test]
    fn step_cap_is_clamped_to_the_ceiling() {
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
