//! The two capabilities that used to be CLI-only (#1297): a judged
//! multi-round goal run, and a turn that delegates to sub-agents.
//!
//! Driven through [`Session`] directly, like `bridge.rs` — a mock host reads
//! frames and answers the reverse-RPC requests. If the capability holds here
//! it holds over HTTP, because the socket layer is the same frame stream for
//! every turn and is witnessed separately.

use serde_json::json;
use stella_core::{BudgetGuard, EngineConfig, GoalConfig};
use stella_protocol::{
    AgentEvent, BudgetMode, CompletionMessage, CompletionResult, CompletionUsage, ModelCallRole,
    ToolCall, ToolOutput, ToolSchema,
};
use stella_serve::observe::TurnRef;
use stella_serve::{
    EffectiveSubAgents, GoalRun, ServerFrame, Session, SessionSpec, SubAgentPolicy,
    SubAgentRequest, TurnOutcomeWire,
};

fn answer(text: &str) -> CompletionResult {
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

fn read_tool() -> ToolSchema {
    ToolSchema {
        name: "read_file".to_string(),
        description: "read a file".to_string(),
        input_schema: json!({ "type": "object" }),
        read_only: true,
        speculation_safe: true,
    }
}

fn base_spec(prompt: &str) -> SessionSpec {
    SessionSpec {
        provider_id: "worker-model".to_string(),
        tools: vec![read_tool()],
        messages: vec![CompletionMessage::user(prompt)],
        config: EngineConfig::default(),
        budget: BudgetGuard::new(BudgetMode::Off, None, None),
        reverse_request_timeout: SessionSpec::DEFAULT_REVERSE_REQUEST_TIMEOUT,
        turn: TurnRef::new("turn-goaltest0000"),
        observer: stella_serve::observe::null_observer(),
        on_settled: None,
        checkpoint: None,
        goal: None,
        sub_agents: None,
    }
}

/// #1297 acceptance, goal half: a judged multi-round run can be *requested*
/// over the wire, its rounds arrive on the ordinary event stream, and the
/// judge is addressable as a different model than the worker.
///
/// Round 1: the worker answers, the judge says NOT met with feedback. Round 2:
/// the worker answers again, the judge says met — and the turn completes
/// carrying the judge's reasoning. Four model calls, two of them announcing
/// `role: judge` and the judge's own provider id, which is what lets a host
/// route the judge to another family: the property the goal loop exists for
/// and the one the engine cannot enforce, because the host owns the calls.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_goal_run_is_requestable_over_the_wire_and_streams_its_rounds() {
    let mut session = Session::start(SessionSpec {
        goal: Some(GoalRun {
            goal: "make the widget work".to_string(),
            config: GoalConfig {
                max_rounds: 4,
                ..GoalConfig::default()
            },
            judge_provider_id: Some("judge-model".to_string()),
        }),
        ..base_spec("ignored — the goal seeds its own first message")
    });

    let mut worker_calls = 0usize;
    let mut judge_calls = 0usize;
    let mut verdicts: Vec<(usize, bool)> = Vec::new();
    let mut outcome = None;

    while let Some(frame) = session.next_frame().await {
        match frame {
            ServerFrame::Event { event } => {
                if let AgentEvent::GoalVerdict { round, met, .. } = event {
                    verdicts.push((round, met));
                }
            }
            ServerFrame::ProviderRequest {
                request_id,
                provider_id,
                role,
                ..
            } => {
                let result = match role {
                    ModelCallRole::Judge => {
                        judge_calls += 1;
                        assert_eq!(
                            provider_id, "judge-model",
                            "a judge call must announce the provider the caller chose for it"
                        );
                        if judge_calls == 1 {
                            answer(
                                r#"{"met": false, "reasoning": "no evidence yet",
                                    "feedback": "run the tests"}"#,
                            )
                        } else {
                            answer(
                                r#"{"met": true, "reasoning": "the tests pass", "feedback": ""}"#,
                            )
                        }
                    }
                    _ => {
                        worker_calls += 1;
                        assert_eq!(
                            provider_id, "worker-model",
                            "worker calls stay on the turn's own provider"
                        );
                        answer("did some work")
                    }
                };
                session.resolve_provider(&request_id, result).unwrap();
            }
            ServerFrame::ToolRequest { request_id, .. } => {
                session
                    .resolve_tool(
                        &request_id,
                        ToolOutput::Ok {
                            content: String::new(),
                        },
                    )
                    .unwrap();
            }
            ServerFrame::TurnComplete { outcome: done } => outcome = Some(done),
            ServerFrame::TurnHeld { .. } | ServerFrame::TurnReleased => {}
        }
    }

    assert_eq!(worker_calls, 2, "two working rounds");
    assert_eq!(judge_calls, 2, "each round was judged");
    assert_eq!(
        verdicts,
        vec![(1, false), (2, true)],
        "every round's verdict reaches the ordinary event stream, in order — this is how a \
         caller follows a run that takes minutes, with nothing new to poll"
    );
    match outcome {
        Some(TurnOutcomeWire::Completed { text, .. }) => assert!(
            text.contains("the tests pass"),
            "the terminal frame carries the judge's reasoning: {text}"
        ),
        other => panic!("a met goal completes the turn, got {other:?}"),
    }
}

/// A goal run that never satisfies its judge ends at the round cap as an
/// `aborted` outcome with a named reason — never a silent success, and never
/// an unbounded loop. The cap is the backstop the mode flag is safe behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unmet_goal_stops_at_the_round_cap_with_a_named_reason() {
    let mut session = Session::start(SessionSpec {
        goal: Some(GoalRun {
            goal: "an impossible thing".to_string(),
            config: GoalConfig {
                max_rounds: 2,
                ..GoalConfig::default()
            },
            judge_provider_id: None,
        }),
        ..base_spec("unused")
    });

    let mut rounds = 0usize;
    let mut outcome = None;
    while let Some(frame) = session.next_frame().await {
        match frame {
            ServerFrame::ProviderRequest {
                request_id,
                provider_id,
                role,
                ..
            } => {
                let result = match role {
                    ModelCallRole::Judge => {
                        // No judge provider named: the judge rides the turn's
                        // own model, which is the single-model deployment and
                        // must keep working.
                        assert_eq!(provider_id, "worker-model");
                        answer(
                            r#"{"met": false, "reasoning": "not yet", "feedback": "keep going"}"#,
                        )
                    }
                    _ => answer("tried"),
                };
                session.resolve_provider(&request_id, result).unwrap();
            }
            ServerFrame::Event { event } => {
                if matches!(event, AgentEvent::GoalVerdict { .. }) {
                    rounds += 1;
                }
            }
            ServerFrame::TurnComplete { outcome: done } => outcome = Some(done),
            _ => {}
        }
    }

    assert_eq!(rounds, 2, "the cap bounds the run");
    match outcome {
        Some(TurnOutcomeWire::Aborted { reason, .. }) => {
            assert!(
                reason.contains("round cap") && reason.contains("goal not met"),
                "an unmet run must say what ended it: {reason}"
            );
        }
        other => panic!("an unmet goal is an aborted turn, got {other:?}"),
    }
}

/// #1297 acceptance, sub-agent half: with the operator's policy allowing it,
/// a served turn advertises `task`, a child runs on the same remoted ports,
/// and only the child's *finding* crosses back into the parent's transcript.
///
/// The child's own model call is a `provider_request` like any other — the
/// sidecar still executes nothing — and it announces the provider id the
/// caller chose for children, which is how "which models" is expressed on
/// this surface.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_served_turn_can_delegate_to_a_sub_agent() {
    let effective = SubAgentPolicy::allowed()
        .clamp(SubAgentRequest {
            enabled: true,
            pool_limit_usd: Some(0.5),
            max_steps: Some(4),
            provider_id: Some("child-model".to_string()),
        })
        .expect("the policy allows children");
    assert_eq!(
        effective,
        EffectiveSubAgents {
            pool_usd: 0.5,
            child_steps: 4,
            provider_id: Some("child-model".to_string()),
        }
    );

    let mut session = Session::start(SessionSpec {
        sub_agents: Some(effective),
        ..base_spec("find out how retries work, then answer")
    });

    let mut parent_calls = 0usize;
    let mut child_calls = 0usize;
    let mut saw_task_schema = false;
    let mut child_saw_task_schema = false;
    let mut outcome = None;

    while let Some(frame) = session.next_frame().await {
        match frame {
            ServerFrame::ProviderRequest {
                request_id,
                provider_id,
                request,
                ..
            } => {
                let names: Vec<&str> = request.tools.iter().map(|t| t.name.as_str()).collect();
                let result = if provider_id == "child-model" {
                    child_calls += 1;
                    child_saw_task_schema |= names.contains(&"task");
                    answer("retries use exponential backoff, capped at 5 attempts")
                } else {
                    parent_calls += 1;
                    saw_task_schema |= names.contains(&"task");
                    if parent_calls == 1 {
                        wants_tool(
                            "call-1",
                            "task",
                            json!({
                                "description": "find retry policy",
                                "prompt": "How does the retry policy work?"
                            }),
                        )
                    } else {
                        answer("done")
                    }
                };
                session.resolve_provider(&request_id, result).unwrap();
            }
            ServerFrame::ToolRequest {
                request_id, name, ..
            } => {
                assert_ne!(
                    name, "task",
                    "`task` is executed by the engine, never remoted to the host"
                );
                session
                    .resolve_tool(
                        &request_id,
                        ToolOutput::Ok {
                            content: String::new(),
                        },
                    )
                    .unwrap();
            }
            ServerFrame::TurnComplete { outcome: done } => outcome = Some(done),
            _ => {}
        }
    }

    assert!(
        saw_task_schema,
        "an opted-in turn must advertise `task`, or the model cannot delegate"
    );
    assert_eq!(child_calls, 1, "the child ran on the remoted provider port");
    assert!(
        !child_saw_task_schema,
        "a child must not see `task`: nesting is capped at one level by construction, not by \
         a depth counter"
    );
    assert_eq!(parent_calls, 2, "the parent called again with the finding");
    assert!(matches!(outcome, Some(TurnOutcomeWire::Completed { .. })));
}

/// The operator's veto: with the default policy — the shipped one — a turn
/// asking for sub-agents gets none, and the model is never told the tool
/// exists. A capability that spends money on the host's account must not be
/// reachable by a request body alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deployment_that_has_not_opted_in_advertises_no_task_tool() {
    assert!(
        SubAgentPolicy::default()
            .clamp(SubAgentRequest {
                enabled: true,
                ..SubAgentRequest::default()
            })
            .is_none(),
        "the default policy refuses children whatever the caller asked for"
    );

    let mut session = Session::start(base_spec("do the thing"));
    let mut advertised_task = false;
    while let Some(frame) = session.next_frame().await {
        if let ServerFrame::ProviderRequest {
            request_id,
            request,
            ..
        } = frame
        {
            advertised_task |= request.tools.iter().any(|t| t.name == "task");
            session
                .resolve_provider(&request_id, answer("done"))
                .unwrap();
        }
    }
    assert!(
        !advertised_task,
        "a turn with no sub-agent grant must not advertise `task`"
    );
}
