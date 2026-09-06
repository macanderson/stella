//! Proves the `!Send` concurrency bridge in isolation, with no HTTP: a mock
//! "host" reads [`ServerFrame`]s off a live [`Session`] and answers the
//! reverse-RPC requests, exactly as the real host (Oxagen) will over HTTP. If
//! this holds, wrapping a socket around it is transport plumbing.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use stella_core::{BudgetGuard, EngineConfig, TurnOutcome};
use stella_protocol::{
    BudgetMode, CompletionMessage, CompletionResult, CompletionUsage, MessageRole, ToolCall,
    ToolOutput, ToolSchema,
};
use stella_serve::observe::TurnRef;
use stella_serve::{ProviderDelta, ServerFrame, Session, SessionSpec, TurnOutcomeWire};

/// Build a mock model result carrying a final text answer and no tool calls —
/// the engine treats this as "the turn is done."
fn final_answer(text: &str) -> CompletionResult {
    CompletionResult {
        upstream_provider: None,
        text: text.to_string(),
        tool_calls: vec![],
        usage: CompletionUsage {
            reported: true,
            ..CompletionUsage::default()
        },
        model: "mock".to_string(),
        cost_usd: 0.0,
        finish_reason: None,
    }
}

/// Build a mock model result that asks for one tool call.
fn wants_tool(call_id: &str, name: &str, input: serde_json::Value) -> CompletionResult {
    CompletionResult {
        upstream_provider: None,
        text: String::new(),
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: name.to_string(),
            input,
        }],
        usage: CompletionUsage {
            reported: true,
            ..CompletionUsage::default()
        },
        model: "mock".to_string(),
        cost_usd: 0.0,
        finish_reason: None,
    }
}

fn echo_tool() -> ToolSchema {
    ToolSchema {
        name: "echo".to_string(),
        description: "echo its input".to_string(),
        input_schema: json!({ "type": "object" }),
        read_only: false,
        speculation_safe: false,
    }
}

fn spec_for(prompt: &str) -> SessionSpec {
    SessionSpec {
        provider_id: "mock".to_string(),
        principal: stella_core::ports::Principal::Host("test".to_string()),
        gate: SessionSpec::default_gate(),
        tools: vec![stella_protocol::ToolContract::declared(echo_tool())],
        messages: vec![CompletionMessage::user(prompt)],
        config: EngineConfig::default(),
        budget: BudgetGuard::new(BudgetMode::Off, None, None),
        reverse_request_timeout: SessionSpec::DEFAULT_REVERSE_REQUEST_TIMEOUT,
        turn: TurnRef::new("turn-bridgetest0"),
        observer: stella_serve::observe::null_observer(),
        on_settled: None,
        checkpoint: None,
        goal: None,
        sub_agents: None,
        extensions: stella_serve::Extensions::new(),
        calibration: None,
        steering_requery: false,
    }
}

/// The same spec with a deadline short enough to assert against in a test. The
/// production default is five minutes; nothing here should sleep for that long,
/// so every deadline test injects its own.
fn spec_with_deadline(prompt: &str, deadline: Duration) -> SessionSpec {
    SessionSpec {
        reverse_request_timeout: deadline,
        ..spec_for(prompt)
    }
}

#[test]
fn aborted_outcome_preserves_incurred_cost_on_wire() {
    let wire = TurnOutcomeWire::from(TurnOutcome::Aborted {
        reason: "budget exhausted".to_string(),
        kind: stella_core::AbortKind::DeliberateStop,
        cost_usd: 1.25,
    });

    assert_eq!(
        wire,
        TurnOutcomeWire::Aborted {
            reason: "budget exhausted".to_string(),
            cost_usd: 1.25,
        }
    );
    assert_eq!(
        serde_json::to_value(wire).unwrap(),
        json!({
            "status": "aborted",
            "reason": "budget exhausted",
            "cost_usd": 1.25,
        })
    );
}

/// The full loop: model asks for a tool, the host runs it and answers, the model
/// then produces a final answer, and the turn completes. Proves both reverse-RPC
/// ports (provider + tool) round-trip across the thread boundary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_round_trip_completes_the_turn() {
    let mut session = Session::start(spec_for("use the echo tool then answer"));

    let mut provider_calls = 0usize;
    let mut tool_calls = 0usize;
    let mut events = 0usize;
    let mut outcome = None;

    while let Some(frame) = session.next_frame().await {
        match frame {
            ServerFrame::Event { .. } => events += 1,
            ServerFrame::ProviderRequest { request_id, .. } => {
                provider_calls += 1;
                let result = if provider_calls == 1 {
                    wants_tool("call-1", "echo", json!({ "text": "hi" }))
                } else {
                    final_answer("done")
                };
                session.resolve_provider(&request_id, result).unwrap();
            }
            ServerFrame::ToolRequest {
                request_id, name, ..
            } => {
                tool_calls += 1;
                assert_eq!(name, "echo");
                session
                    .resolve_tool(
                        &request_id,
                        ToolOutput::Ok {
                            content: "echoed".to_string(),
                            data: None,
                        },
                    )
                    .unwrap();
            }
            ServerFrame::TurnComplete { outcome: done } => outcome = Some(done),
            ServerFrame::RequeryRequest { .. } => {
                panic!("a turn that did not opt in must not ask for context")
            }
            // An unpaused turn crosses every step boundary freely, so the
            // pause gate must stay silent — a hold nobody asked for would
            // be a frame every host has to learn to ignore.
            ServerFrame::TurnHeld { .. } | ServerFrame::TurnReleased => {
                panic!("an unpaused turn must not announce a hold")
            }
        }
    }

    assert_eq!(provider_calls, 2, "model called before and after the tool");
    assert_eq!(tool_calls, 1, "the one requested tool call round-tripped");
    assert!(events > 0, "the turn emitted agent events for the UI");
    assert_eq!(
        outcome,
        Some(TurnOutcomeWire::Completed {
            text: "done".to_string(),
            cost_usd: 0.0,
        }),
    );
}

/// A turn whose model answers immediately needs no tool round-trip and still
/// completes — the minimal path through the bridge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn immediate_completion_needs_no_tools() {
    let mut session = Session::start(spec_for("just answer"));

    let mut provider_calls = 0usize;
    let mut tool_calls = 0usize;
    let mut outcome = None;

    while let Some(frame) = session.next_frame().await {
        match frame {
            ServerFrame::Event { .. } => {}
            ServerFrame::ProviderRequest { request_id, .. } => {
                provider_calls += 1;
                session
                    .resolve_provider(&request_id, final_answer("hello"))
                    .unwrap();
            }
            ServerFrame::ToolRequest { .. } => tool_calls += 1,
            ServerFrame::TurnComplete { outcome: done } => outcome = Some(done),
            ServerFrame::RequeryRequest { .. } => {
                panic!("a turn that did not opt in must not ask for context")
            }
            // An unpaused turn crosses every step boundary freely, so the
            // pause gate must stay silent — a hold nobody asked for would
            // be a frame every host has to learn to ignore.
            ServerFrame::TurnHeld { .. } | ServerFrame::TurnReleased => {
                panic!("an unpaused turn must not announce a hold")
            }
        }
    }

    assert_eq!(provider_calls, 1);
    assert_eq!(tool_calls, 0);
    assert_eq!(
        outcome,
        Some(TurnOutcomeWire::Completed {
            text: "hello".to_string(),
            cost_usd: 0.0,
        }),
    );
}

/// A reverse request the host never answers must fail on its deadline instead of
/// parking the engine step — and its thread — forever. The deadline is injected
/// short so this is a fast test; production defaults to five minutes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unanswered_provider_request_fails_on_the_deadline() {
    let deadline = Duration::from_millis(50);
    let mut session = Session::start(spec_with_deadline("nobody will answer", deadline));
    let started = Instant::now();

    // Read frames but never resolve the provider request.
    let mut outcome = None;
    let mut provider_requests = 0usize;
    while let Some(frame) = session.next_frame().await {
        match frame {
            ServerFrame::ProviderRequest { .. } => provider_requests += 1,
            ServerFrame::TurnComplete { outcome: done } => outcome = Some(done),
            ServerFrame::RequeryRequest { .. } => {
                panic!("a turn that did not opt in must not ask for context")
            }
            // An unpaused turn crosses every step boundary freely, so the
            // pause gate must stay silent — a hold nobody asked for would
            // be a frame every host has to learn to ignore.
            ServerFrame::TurnHeld { .. } | ServerFrame::TurnReleased => {
                panic!("an unpaused turn must not announce a hold")
            }
            _ => {}
        }
    }
    let elapsed = started.elapsed();

    assert_eq!(provider_requests, 1, "the model was asked exactly once");
    let reason = match outcome {
        Some(TurnOutcomeWire::Aborted { reason, .. }) => reason,
        other => panic!("expected the deadline to abort the turn, got {other:?}"),
    };
    assert!(
        reason.contains("deadline"),
        "the abort must name the deadline as the cause: {reason}"
    );
    assert!(
        elapsed >= deadline,
        "the turn cannot fail before its own deadline: {elapsed:?}"
    );
    // The deadline must be terminal, not retryable: a `Transport` error would be
    // retried, handing the unresponsive host the same wait once per attempt.
    assert!(
        elapsed < Duration::from_secs(5),
        "the turn failed on the deadline rather than retrying its way there: {elapsed:?}"
    );
}

/// An unanswered *tool* request is model-visible data, not an engine error, so it
/// times out into a `ToolOutput::Error` the turn can carry on from.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unanswered_tool_request_times_out_into_a_tool_error() {
    let mut session = Session::start(spec_with_deadline(
        "use the echo tool then answer",
        Duration::from_millis(50),
    ));

    let mut provider_calls = 0usize;
    let mut tool_requests = 0usize;
    let mut outcome = None;
    while let Some(frame) = session.next_frame().await {
        match frame {
            ServerFrame::ProviderRequest { request_id, .. } => {
                provider_calls += 1;
                let result = if provider_calls == 1 {
                    wants_tool("call-1", "echo", json!({ "text": "hi" }))
                } else {
                    final_answer("carried on without the tool")
                };
                session.resolve_provider(&request_id, result).unwrap();
            }
            // Never answered — the port's deadline must resolve it instead.
            ServerFrame::ToolRequest { .. } => tool_requests += 1,
            ServerFrame::TurnComplete { outcome: done } => outcome = Some(done),
            ServerFrame::RequeryRequest { .. } => {
                panic!("a turn that did not opt in must not ask for context")
            }
            // An unpaused turn crosses every step boundary freely, so the
            // pause gate must stay silent — a hold nobody asked for would
            // be a frame every host has to learn to ignore.
            ServerFrame::TurnHeld { .. } | ServerFrame::TurnReleased => {
                panic!("an unpaused turn must not announce a hold")
            }
            _ => {}
        }
    }

    assert_eq!(
        tool_requests, 1,
        "the tool was requested and left unanswered"
    );
    assert_eq!(
        outcome,
        Some(TurnOutcomeWire::Completed {
            text: "carried on without the tool".to_string(),
            cost_usd: 0.0,
        }),
        "a timed-out tool is an error the model sees, never a failed turn",
    );
}

/// Cancelling a turn wakes its parked reverse request at once and unwinds the
/// turn to a terminal frame, rather than killing the thread or waiting out the
/// (five-minute) deadline. The default deadline is left in place on purpose: if
/// cancellation did not work, this test would hang rather than pass slowly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_parked_turn_unwinds_it_promptly() {
    let mut session = Session::start(spec_for("this turn gets cancelled"));
    let started = Instant::now();

    let mut outcome = None;
    while let Some(frame) = session.next_frame().await {
        match frame {
            // Cancel instead of answering, while the step is parked on us.
            ServerFrame::ProviderRequest { .. } => session.cancel(),
            ServerFrame::TurnComplete { outcome: done } => outcome = Some(done),
            ServerFrame::RequeryRequest { .. } => {
                panic!("a turn that did not opt in must not ask for context")
            }
            // An unpaused turn crosses every step boundary freely, so the
            // pause gate must stay silent — a hold nobody asked for would
            // be a frame every host has to learn to ignore.
            ServerFrame::TurnHeld { .. } | ServerFrame::TurnReleased => {
                panic!("an unpaused turn must not announce a hold")
            }
            _ => {}
        }
    }

    match outcome {
        Some(TurnOutcomeWire::Aborted { .. }) => {}
        other => panic!("expected a cancelled turn to abort, got {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "cancellation must not wait out the reverse-request deadline"
    );
}

/// Tearing a session down (drop, not an explicit cancel) must *cancel* the
/// turn, not merely drop its parked one-shots: a bare drop wakes the parked
/// step with a retryable transport error, and the engine would burn its
/// provider retries — and their backoff sleeps — against a registry nobody
/// answers anymore. The detached thread cannot be joined, so the latched
/// cancel flag is the observable contract: it is what the woken step reads as
/// the non-retryable `Cancelled` instead of a retryable failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_a_session_cancels_the_turn_rather_than_letting_it_retry() {
    let mut session = Session::start(spec_for("teardown mid-step"));
    let pending = session.pending();

    // Park the turn on a reverse request first, so the drop happens mid-step.
    loop {
        match session.next_frame().await {
            Some(ServerFrame::ProviderRequest { .. }) => break,
            Some(_) => continue,
            None => panic!("the turn ended before it parked on the provider"),
        }
    }

    drop(session);
    assert!(
        pending.is_cancelled(),
        "teardown must latch the cancel flag so the woken step aborts instead of retrying"
    );
}

/// The streaming half of the provider answer (#1165): fragments the host
/// feeds to an in-flight provider request surface on the frame stream as
/// `TextDelta` / `Reasoning` events **before** the turn completes — which is
/// what a second `/events` subscriber sees live and a resuming client
/// replays. Before this, a served turn's stream carried no partial text at
/// all: the engine only ever saw one assembled result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamed_provider_deltas_surface_as_events_before_the_completion() {
    let mut session = Session::start(spec_for("stream the answer"));

    let mut text_deltas: Vec<String> = Vec::new();
    let mut reasoning = 0usize;
    let mut final_text_at: Option<usize> = None;
    let mut frames_seen = 0usize;
    let mut outcome = None;

    while let Some(frame) = session.next_frame().await {
        frames_seen += 1;
        match frame {
            ServerFrame::ProviderRequest { request_id, .. } => {
                // The host streams its model call: two batches of fragments,
                // then the aggregated result — exactly the wire choreography
                // of POST provider-delta, provider-delta, provider-result.
                session
                    .resolve_provider_delta(
                        &request_id,
                        vec![
                            ProviderDelta::Reasoning {
                                text: "weighing…".to_string(),
                            },
                            ProviderDelta::Text {
                                text: "Hel".to_string(),
                            },
                        ],
                    )
                    .expect("first batch lands on the in-flight request");
                session
                    .resolve_provider_delta(
                        &request_id,
                        vec![ProviderDelta::Text {
                            text: "lo".to_string(),
                        }],
                    )
                    .expect("a second batch still finds the request in flight");
                session
                    .resolve_provider(&request_id, final_answer("Hello"))
                    .unwrap();
            }
            ServerFrame::Event { event } => match event {
                stella_protocol::AgentEvent::TextDelta { delta: text } => {
                    text_deltas.push(text);
                }
                stella_protocol::AgentEvent::Reasoning { .. } => reasoning += 1,
                stella_protocol::AgentEvent::Text { .. } => {
                    final_text_at.get_or_insert(frames_seen);
                }
                _ => {}
            },
            ServerFrame::TurnComplete { outcome: done } => outcome = Some(done),
            ServerFrame::RequeryRequest { .. } => {
                panic!("a turn that did not opt in must not ask for context")
            }
            // An unpaused turn crosses every step boundary freely, so the
            // pause gate must stay silent — a hold nobody asked for would
            // be a frame every host has to learn to ignore.
            ServerFrame::TurnHeld { .. } | ServerFrame::TurnReleased => {
                panic!("an unpaused turn must not announce a hold")
            }
            ServerFrame::ToolRequest { .. } => {}
        }
    }

    assert_eq!(
        text_deltas,
        vec!["Hel".to_string(), "lo".to_string()],
        "the host's text fragments must surface as TextDelta events, in order"
    );
    assert!(
        reasoning >= 1,
        "the thinking fragment must surface as Reasoning, never as answer text"
    );
    assert!(
        final_text_at.is_some(),
        "the authoritative Text event still lands after the previews"
    );
    assert_eq!(
        outcome,
        Some(TurnOutcomeWire::Completed {
            text: "Hello".to_string(),
            cost_usd: 0.0,
        }),
        "streaming must not change the turn's outcome"
    );
}

/// A host that streams nothing keeps exactly its old behavior — the whole
/// route is optional, which is the compatibility half of #1165's acceptance.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_that_never_streams_is_unchanged() {
    let mut session = Session::start(spec_for("just answer"));

    let mut text_deltas = 0usize;
    let mut outcome = None;
    while let Some(frame) = session.next_frame().await {
        match frame {
            ServerFrame::ProviderRequest { request_id, .. } => {
                session
                    .resolve_provider(&request_id, final_answer("hello"))
                    .unwrap();
            }
            ServerFrame::Event {
                event: stella_protocol::AgentEvent::TextDelta { .. },
            } => text_deltas += 1,
            ServerFrame::TurnComplete { outcome: done } => outcome = Some(done),
            ServerFrame::RequeryRequest { .. } => {
                panic!("a turn that did not opt in must not ask for context")
            }
            // An unpaused turn crosses every step boundary freely, so the
            // pause gate must stay silent — a hold nobody asked for would
            // be a frame every host has to learn to ignore.
            ServerFrame::TurnHeld { .. } | ServerFrame::TurnReleased => {
                panic!("an unpaused turn must not announce a hold")
            }
            _ => {}
        }
    }

    assert_eq!(text_deltas, 0, "no fragments were fed, so none may appear");
    assert_eq!(
        outcome,
        Some(TurnOutcomeWire::Completed {
            text: "hello".to_string(),
            cost_usd: 0.0,
        }),
    );
}

/// Fragments count as progress against the reverse-request deadline: a host
/// that keeps streaming past the configured window must not have its turn cut
/// as "unanswered" — the deadline measures silence, not elapsed time.
///
/// This test races two real clocks against each other. `Session::start`
/// drives the turn on its own OS thread with its own `tokio` runtime (see
/// that fn's doc comment for why). So a paused clock in this test cannot
/// reach the deadline's sleep, which ticks on the other thread. The margin
/// below only needs to survive scheduler delay, not the whole deadline, so a
/// generous margin is enough.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_streaming_host_resets_the_reverse_request_deadline() {
    // 300ms gaps against a 1s deadline: 3.3x slack per gap. A loaded CI
    // runner delaying either side of a gap by a few tens of milliseconds
    // must not false-fail this test — a 60ms gap against a 120ms deadline
    // gave that delay only 60ms to hide in; 300ms against 1s gives it 700ms.
    // `gap < deadline` still holds, so a single gap can never trip the
    // deadline on its own; `6 * gap` (1.8s) still clears `deadline` (1s), so
    // the turn only survives past the deadline if resetting it on progress
    // is real.
    let deadline = Duration::from_millis(1000);
    let mut session = Session::start(spec_with_deadline("keep streaming", deadline));

    let mut outcome = None;
    while let Some(frame) = session.next_frame().await {
        match frame {
            ServerFrame::ProviderRequest { request_id, .. } => {
                // Stream fragments for ~1.8x the deadline, each gap well
                // inside it, then answer. Under a fixed total window this
                // turn would have been killed after `deadline`.
                for n in 0..6 {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    session
                        .resolve_provider_delta(
                            &request_id,
                            vec![ProviderDelta::Text {
                                text: format!("chunk-{n} "),
                            }],
                        )
                        .expect("a live stream must keep its request in flight");
                }
                session
                    .resolve_provider(&request_id, final_answer("streamed to the end"))
                    .unwrap();
            }
            ServerFrame::TurnComplete { outcome: done } => outcome = Some(done),
            ServerFrame::RequeryRequest { .. } => {
                panic!("a turn that did not opt in must not ask for context")
            }
            // An unpaused turn crosses every step boundary freely, so the
            // pause gate must stay silent — a hold nobody asked for would
            // be a frame every host has to learn to ignore.
            ServerFrame::TurnHeld { .. } | ServerFrame::TurnReleased => {
                panic!("an unpaused turn must not announce a hold")
            }
            _ => {}
        }
    }

    assert_eq!(
        outcome,
        Some(TurnOutcomeWire::Completed {
            text: "streamed to the end".to_string(),
            cost_usd: 0.0,
        }),
        "a host that keeps producing must never be timed out as silent",
    );
}

/// A classified provider failure aborts the turn cleanly (no panic, a terminal
/// frame). Uses a retryable transport error the engine will retry then give up
/// on — the point is the bridge surfaces the error path, not the retry count.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_error_aborts_cleanly() {
    let mut session = Session::start(spec_for("this will fail"));

    let mut outcome = None;
    while let Some(frame) = session.next_frame().await {
        match frame {
            ServerFrame::ProviderRequest { request_id, .. } => {
                session
                    .fail_provider(
                        &request_id,
                        stella_protocol::ProviderError::Terminal("mock outage".to_string()),
                    )
                    .unwrap();
            }
            ServerFrame::TurnComplete { outcome: done } => outcome = Some(done),
            ServerFrame::RequeryRequest { .. } => {
                panic!("a turn that did not opt in must not ask for context")
            }
            // An unpaused turn crosses every step boundary freely, so the
            // pause gate must stay silent — a hold nobody asked for would
            // be a frame every host has to learn to ignore.
            ServerFrame::TurnHeld { .. } | ServerFrame::TurnReleased => {
                panic!("an unpaused turn must not announce a hold")
            }
            _ => {}
        }
    }

    match outcome {
        Some(TurnOutcomeWire::Aborted { .. }) => {}
        other => panic!("expected a clean abort, got {other:?}"),
    }
}

// ── the mid-turn context re-query ───────────────────────────────────────────

/// The system message a re-query must not disturb: the byte-stable prefix a
/// turn's prompt cache depends on (AGENTS.md rule 7).
const STABLE_PREFIX: &str = "you are a careful engineer";

/// The block a scripted host returns whenever the server asks for context.
const CONTEXT_BLOCK: &str = "the provider adapters share one SSE parser";

/// What one scripted re-query turn produced, for the tests that read it.
struct RequeryRun {
    outcome: Option<TurnOutcomeWire>,
    /// The transcript as the engine left it.
    messages: Vec<CompletionMessage>,
    /// How many times the server asked the host for context.
    asks: usize,
    /// The touched paths carried on the first ask.
    signal_paths: Vec<String>,
    /// Whether a model call made after the ask saw the block.
    block_reached_the_model: bool,
}

/// Run a three-step turn whose first two steps each touch `src/adapter.rs`, so
/// the third boundary has both a drifted signal and the spacing the port
/// requires, and answer whatever it asks for with `context`.
async fn scripted_requery_turn(context: Option<&str>) -> RequeryRun {
    let settled: Arc<Mutex<Vec<CompletionMessage>>> = Arc::new(Mutex::new(Vec::new()));
    let write_back = Arc::clone(&settled);
    let mut session = Session::start(SessionSpec {
        messages: vec![
            CompletionMessage::system(STABLE_PREFIX),
            CompletionMessage::user("fix the flaky test"),
        ],
        steering_requery: true,
        on_settled: Some(Box::new(move |settlement| {
            *write_back.lock().expect("settled transcript") = settlement.messages;
        })),
        ..spec_for("unused — the messages above are this turn's transcript")
    });

    let mut run = RequeryRun {
        outcome: None,
        messages: Vec::new(),
        asks: 0,
        signal_paths: Vec::new(),
        block_reached_the_model: false,
    };
    let mut provider_calls = 0usize;

    while let Some(frame) = session.next_frame().await {
        match frame {
            ServerFrame::ProviderRequest {
                request_id,
                request,
                ..
            } => {
                provider_calls += 1;
                if request
                    .messages
                    .iter()
                    .any(|message| message.content.contains(CONTEXT_BLOCK))
                {
                    run.block_reached_the_model = true;
                }
                let result = if provider_calls <= 2 {
                    wants_tool(
                        &format!("call-{provider_calls}"),
                        "echo",
                        json!({ "path": "src/adapter.rs" }),
                    )
                } else {
                    final_answer("done")
                };
                session.resolve_provider(&request_id, result).unwrap();
            }
            ServerFrame::ToolRequest { request_id, .. } => {
                session
                    .resolve_tool(
                        &request_id,
                        ToolOutput::Ok {
                            content: "read it".to_string(),
                            data: None,
                        },
                    )
                    .unwrap();
            }
            ServerFrame::RequeryRequest { request_id, signal } => {
                if run.asks == 0 {
                    run.signal_paths = signal.touched_paths.clone();
                }
                run.asks += 1;
                session
                    .resolve_requery(&request_id, context.map(str::to_string))
                    .unwrap();
            }
            ServerFrame::TurnComplete { outcome: done } => run.outcome = Some(done),
            ServerFrame::Event { .. } => {}
            ServerFrame::TurnHeld { .. } | ServerFrame::TurnReleased => {
                panic!("an unpaused turn must not announce a hold")
            }
        }
    }
    run.messages = settled.lock().expect("settled transcript").clone();
    run
}

/// **The witness for `turn.steering_requery` on the API surface.**
///
/// A served turn whose work moves onto a path its opening prompt never named
/// asks the host for context at a step boundary, and what the host answers
/// reaches the next model call.
///
/// It fails on `main` by construction, twice over: `stella-serve` assembles
/// no re-query source there (`served_capabilities` passes `requery: None`),
/// and there is no `requery_request` frame for a host to answer even if it
/// did.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_served_turn_requeries_the_steering_plane_after_its_work_moves_to_a_new_path() {
    let run = scripted_requery_turn(Some(CONTEXT_BLOCK)).await;

    assert_eq!(run.asks, 1, "the drifted boundary asked exactly once");
    assert_eq!(
        run.signal_paths,
        vec!["src/adapter.rs".to_string()],
        "the host is told what the turn has touched, not just its prompt"
    );
    assert!(
        run.block_reached_the_model,
        "the host's block must reach the model call after it, or the re-query bought nothing"
    );
    assert_eq!(
        run.outcome,
        Some(TurnOutcomeWire::Completed {
            text: "done".to_string(),
            cost_usd: 0.0,
        }),
    );
}

/// The re-query appends and nothing else: the system message is byte-identical
/// after it, and the block rides the volatile tail as a marked user message
/// (AGENTS.md rule 7).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_served_requery_leaves_the_byte_stable_prefix_untouched() {
    let run = scripted_requery_turn(Some(CONTEXT_BLOCK)).await;

    let prefix = run.messages.first().expect("the turn settled a transcript");
    assert_eq!(prefix.role, MessageRole::System);
    assert_eq!(
        prefix.content, STABLE_PREFIX,
        "a re-query must not move one byte of the cached prefix"
    );

    let injected: Vec<&CompletionMessage> = run
        .messages
        .iter()
        .filter(|message| message.content.contains(CONTEXT_BLOCK))
        .collect();
    assert_eq!(
        injected.len(),
        1,
        "the block is injected once, not per step"
    );
    let block = injected[0];
    assert_eq!(block.role, MessageRole::User);
    assert!(
        block
            .content
            .starts_with(stella_core::receipts::RECALL_MARKER),
        "an injected block must carry the recall marker or the engine reads it \
         as a user turn: {:?}",
        block.content
    );
}

/// The cheap answer. A host with nothing worth the tokens answers `null`, and
/// the turn runs on with the context it already had.
///
/// The control for the witness above: both turns ask, and only the answered
/// one grows a message.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_that_declines_a_requery_injects_nothing() {
    let run = scripted_requery_turn(None).await;

    assert_eq!(run.asks, 1, "the boundary still drifted, so it still asked");
    assert!(
        !run.block_reached_the_model,
        "a declined re-query must put nothing in front of the model"
    );
    assert!(
        run.messages.iter().all(|message| !message
            .content
            .starts_with(stella_core::receipts::RECALL_MARKER)),
        "a declined re-query must leave no injected block behind"
    );
    assert_eq!(
        run.outcome,
        Some(TurnOutcomeWire::Completed {
            text: "done".to_string(),
            cost_usd: 0.0,
        }),
    );
}
