// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `/v1/sessions` endpoint handlers, and the wire types they parse.
//!
//! Split out of `routes.rs` to keep both files clear of the 1500-line ratchet
//! (`scripts/check-file-size.sh`), which `routes.rs` crossed once the goal-run
//! and sub-agent routes landed alongside the hook and calibration ones. The cut
//! follows the same concern the parent module documents: these four handlers
//! are the whole of the server-owned-conversation surface, and nothing outside
//! them reads the request shapes they parse.
//!
//! Re-exported by the parent, so callers stay `routes::handle_session_*`.

use super::*;

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
    /// A judged multi-round goal run inside a persistent session (#1297) —
    /// the same block as the stateless route. The rounds' messages join the
    /// session history like any other turn's, so a follow-up turn sees what
    /// the goal run did.
    #[serde(default)]
    goal: Option<GoalSpec>,
    /// Sub-agents for this turn (#1297), same block and same operator caps as
    /// the stateless route.
    #[serde(default)]
    sub_agents: Option<SubAgentsSpec>,
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
    // Same clamping as the stateless path (#1297) — a session turn is a turn.
    let goal = request.goal.and_then(goal_run);
    let sub_agents = request
        .sub_agents
        .and_then(|spec| state.sub_agent_policy().clamp(spec.into()));
    let extensions = state.extensions();
    // `None` when the registry's bounds refuse this id — see
    // `crate::calibration` for why a host-supplied key gets a bounded map.
    let calibration = state.calibration().for_provider(&request.provider_id);
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
            goal,
            pipeline: None,
            sub_agents,
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
