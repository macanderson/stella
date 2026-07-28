//! Proves the `!Send` concurrency bridge in isolation, with no HTTP: a mock
//! "host" reads [`ServerFrame`]s off a live [`Session`] and answers the
//! reverse-RPC requests, exactly as the real host (Oxagen) will over HTTP. If
//! this holds, wrapping a socket around it is transport plumbing.

use std::time::{Duration, Instant};

use serde_json::json;
use stella_core::{BudgetGuard, EngineConfig, TurnOutcome};
use stella_protocol::{
    BudgetMode, CompletionMessage, CompletionResult, CompletionUsage, ToolCall, ToolOutput,
    ToolSchema,
};
use stella_serve::{ServerFrame, Session, SessionSpec, TurnOutcomeWire};

/// Build a mock model result carrying a final text answer and no tool calls —
/// the engine treats this as "the turn is done."
fn final_answer(text: &str) -> CompletionResult {
    CompletionResult {
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
    }
}

fn spec_for(prompt: &str) -> SessionSpec {
    SessionSpec {
        provider_id: "mock".to_string(),
        tools: vec![echo_tool()],
        messages: vec![CompletionMessage::user(prompt)],
        config: EngineConfig::default(),
        budget: BudgetGuard::new(BudgetMode::Off, None, None),
        reverse_request_timeout: SessionSpec::DEFAULT_REVERSE_REQUEST_TIMEOUT,
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
                        },
                    )
                    .unwrap();
            }
            ServerFrame::TurnComplete { outcome: done } => outcome = Some(done),
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

    // Read frames but deliberately never resolve the provider request.
    let mut outcome = None;
    let mut provider_requests = 0usize;
    while let Some(frame) = session.next_frame().await {
        match frame {
            ServerFrame::ProviderRequest { .. } => provider_requests += 1,
            ServerFrame::TurnComplete { outcome: done } => outcome = Some(done),
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
            _ => {}
        }
    }

    match outcome {
        Some(TurnOutcomeWire::Aborted { .. }) => {}
        other => panic!("expected a clean abort, got {other:?}"),
    }
}
