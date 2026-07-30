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

use crate::frame::{ProviderOutcomeIn, ProviderResultIn, ToolResultIn};
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

/// `GET /healthz` — the only unauthenticated route.
pub(crate) async fn handle_health(res: &mut Responder<'_>) -> std::io::Result<()> {
    res.json("200 OK", br#"{"status":"ok"}"#).await
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

    let body = serde_json::to_vec(&TurnCreated { turn_id: &id }).unwrap_or_default();
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
    state.observer().emit(&ServeEvent::StreamEnded {
        turn,
        frames_sent,
        reason,
    });

    // A peer that vanished may be back: park the session so a reconnect can
    // resume from its retained tail, and let the grace window — not this
    // connection — decide when the turn is definitively abandoned. Every other
    // ending is final, so the session drops here and `Drop for Session`
    // cancels the turn, releasing its thread now rather than at end of scope.
    if reason == StreamEndReason::PeerDisconnected {
        state.park_for_resume(id, &entry, session, generation);
    } else {
        drop(session);
        state.turns().remove(id);
    }
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
    entry.pending.cancel();
    // Dropping our `Arc` here is what reclaims a turn whose stream never opened:
    // the registry no longer holds it, so this may be the last handle, and
    // `Drop for Session` releases the engine thread. A turn that *is* streaming
    // keeps its own handle and unwinds through `handle_events` as usual.
    drop(entry);
    res.json("200 OK", br#"{"status":"cancelled"}"#).await
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
