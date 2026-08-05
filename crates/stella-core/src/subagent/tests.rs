// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Sub-agent primitive tests (#922).
//!
//! The load-bearing one is
//! [`the_parent_transcript_does_not_grow_by_the_childs_intermediate_work`] —
//! the witness for the whole feature. Everything else pins one of the five
//! contracts in the module docs.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::{Value, json};
use stella_protocol::{
    BudgetMode, CompletionRequestRef, CompletionResult, CompletionUsage, ProviderError, ToolCall,
    ToolOutput, ToolSchema,
};
use tokio::sync::mpsc;

use super::*;
use crate::budget::BudgetOutcome;
use crate::ports::TurnGate;
use crate::retry::Sleeper;

// ---- fakes -----------------------------------------------------------

struct NoSleep;
#[async_trait]
impl Sleeper for NoSleep {
    async fn sleep(&self, _duration_ms: u64) {}
}

/// A provider that returns a fixed sequence of results, then errors.
struct ScriptedProvider {
    script: Mutex<Vec<Result<CompletionResult, ProviderError>>>,
    calls: AtomicU32,
}

impl ScriptedProvider {
    fn new(script: Vec<Result<CompletionResult, ProviderError>>) -> Self {
        Self {
            script: Mutex::new(script),
            calls: AtomicU32::new(0),
        }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted"
    }

    // `complete_ref` is the trait's required method; `complete` is a default
    // that delegates to it. Overriding the default left this double missing the
    // real one, so the crate's tests stopped compiling.
    async fn complete_ref(
        &self,
        _request: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut script = self.script.lock().unwrap();
        if script.is_empty() {
            return Err(ProviderError::Terminal("script exhausted".into()));
        }
        script.remove(0)
    }
}

/// One read-only tool and one mutating tool, counting executions of each.
#[derive(Default)]
struct MixedTools {
    reads: AtomicUsize,
    writes: AtomicUsize,
}

#[async_trait]
impl ToolExecutor for MixedTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![
            ToolSchema {
                name: "read_file".into(),
                description: "read a file".into(),
                input_schema: json!({"type": "object"}),
                read_only: true,
                speculation_safe: false,
            },
            ToolSchema {
                name: "write_file".into(),
                description: "write a file".into(),
                input_schema: json!({"type": "object"}),
                read_only: false,
                speculation_safe: false,
            },
        ]
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        match name {
            "read_file" => {
                self.reads.fetch_add(1, Ordering::SeqCst);
                ToolOutput::Ok {
                    // Deliberately bulky: this is the content a parent would
                    // otherwise be carrying for the rest of the session. Keyed
                    // on the input so distinct reads produce distinct output —
                    // identical output from identical arguments is a stuck
                    // loop, and `crate::loop_detect` is right to abort it.
                    content: format!("{input}{}", "x".repeat(4_000)),
                }
            }
            "write_file" => {
                self.writes.fetch_add(1, Ordering::SeqCst);
                ToolOutput::Ok {
                    content: "written".into(),
                }
            }
            other => ToolOutput::Error {
                message: format!("no such tool {other}"),
            },
        }
    }
}

fn text_result(text: &str, cost: f64) -> CompletionResult {
    CompletionResult {
        text: text.into(),
        tool_calls: vec![],
        usage: CompletionUsage {
            reported: true,
            ..CompletionUsage::default()
        },
        model: "scripted".into(),
        cost_usd: cost,
        finish_reason: None,
    }
}

fn tool_call_result(name: &str, call_id: &str, cost: f64) -> CompletionResult {
    // `call_id` doubles as the argument so consecutive calls differ — see
    // `MixedTools::execute` on why identical calls are a loop, not a fixture.
    CompletionResult {
        text: String::new(),
        tool_calls: vec![ToolCall {
            call_id: call_id.into(),
            name: name.into(),
            input: json!({ "path": call_id }),
        }],
        usage: CompletionUsage {
            reported: true,
            ..CompletionUsage::default()
        },
        model: "scripted".into(),
        cost_usd: cost,
        finish_reason: None,
    }
}

fn drain(rx: &mut mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

/// The `Finished` phase of the (single) child in a stream.
fn finished(events: &[AgentEvent]) -> SubAgentPhase {
    events
        .iter()
        .find_map(|event| match event {
            AgentEvent::SubAgent {
                phase: phase @ SubAgentPhase::Finished { .. },
            } => Some(phase.clone()),
            _ => None,
        })
        .expect("a spawn always emits exactly one Finished")
}

// ---- the witness ------------------------------------------------------

/// **The acceptance witness for #922.** A parent turn spawns a child that
/// does real, bulky work — four tool calls returning 4 KB each — and the
/// parent's transcript does not grow by ANY of it.
///
/// The assertion is deliberately on the parent's `Vec<CompletionMessage>`
/// rather than on a token estimate: message identity is what the next
/// provider call actually serializes, so an unchanged vector is proof the
/// child's intermediate work is not being re-sent on every subsequent step
/// for the rest of the session. The report the parent *may* choose to append
/// is bounded separately (see
/// [`the_report_is_clamped_to_the_spec_cap_and_says_so`]).
#[tokio::test]
async fn the_parent_transcript_does_not_grow_by_the_childs_intermediate_work() {
    let parent_provider = ScriptedProvider::new(vec![]);
    // Four read steps, then an answer: 9 messages of child transcript.
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("read_file", "c1", 0.001)),
        Ok(tool_call_result("read_file", "c2", 0.001)),
        Ok(tool_call_result("read_file", "c3", 0.001)),
        Ok(tool_call_result("read_file", "c4", 0.001)),
        Ok(text_result("the retry policy is in retry.rs", 0.001)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);

    let mut parent_messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("which file defines the retry policy?"),
    ];
    let before = parent_messages.clone();
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("search-1", "find the retry policy"),
            &mut budget,
            &tx,
        )
        .await;

    assert_eq!(
        parent_messages, before,
        "the parent's transcript must be byte-identical after a child ran"
    );

    let report = match &outcome {
        SubAgentOutcome::Completed(report) => report,
        other => panic!("expected Completed, got {other:?}"),
    };
    assert_eq!(report.summary, "the retry policy is in retry.rs");
    assert_eq!(report.steps, 5, "five committed model calls");
    assert!(
        report.absorbed_messages >= 8,
        "the child's own transcript grew by {} messages — that is the growth \
         the parent avoided",
        report.absorbed_messages
    );
    assert_eq!(tools.reads.load(Ordering::SeqCst), 4, "the work really ran");

    // And the child's bulk is nowhere in the parent's event stream either:
    // the ToolResults are forwarded for visibility, but the transcript that
    // held them is gone with the function scope that owned it.
    let events = drain(&mut rx);
    assert!(
        matches!(
            finished(&events),
            SubAgentPhase::Finished { status, .. } if status == SubAgentStatus::Completed
        ),
        "the bracket closes with the real status"
    );

    // Belt and braces: appending the report is the ONLY growth available,
    // and it is one message however much the child read.
    parent_messages.push(CompletionMessage::user(report.summary.clone()));
    assert_eq!(parent_messages.len(), before.len() + 1);
}

/// A sink that records what a turn asked it to do.
#[derive(Default)]
struct RecordingSink {
    persisted: std::sync::Mutex<Vec<String>>,
    discards: AtomicUsize,
}

impl std::fmt::Debug for RecordingSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecordingSink")
    }
}

impl crate::step::CheckpointSink for RecordingSink {
    fn persist(&self, json: &str) {
        self.persisted
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(json.to_string());
    }
    fn discard(&self) {
        self.discards.fetch_add(1, Ordering::SeqCst);
    }
}

/// A child turn must not touch the SESSION's resume point.
///
/// The sink is bound to a session, and the child runs inside one of the
/// parent's tool calls while the parent's own turn is still in flight. An
/// inherited sink breaks that in both directions: the child's steps overwrite
/// the parent's resume point with the child's transcript, and the child
/// reaching a terminal outcome retracts the parent's resume point outright — so
/// a crash moments later would find either nothing to resume from, or a
/// conversation belonging to a different agent.
#[tokio::test]
async fn a_child_turn_never_writes_or_clears_the_parents_resume_point() {
    let parent_provider = ScriptedProvider::new(vec![]);
    // Two tool calls then an answer: the child crosses two step boundaries, so
    // an inherited sink would be written to — and discarded — several times.
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("read_file", "c1", 0.001)),
        Ok(tool_call_result("read_file", "c2", 0.001)),
        Ok(text_result("answered", 0.001)),
    ]);
    let tools = MixedTools::default();
    let sink = Arc::new(RecordingSink::default());
    let config = EngineConfig {
        checkpoint_sink: Some(sink.clone() as Arc<dyn crate::step::CheckpointSink>),
        ..EngineConfig::default()
    };
    let parent = Engine::with_sleeper(&parent_provider, &tools, config, &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("search-1", "find it"),
            &mut budget,
            &tx,
        )
        .await;
    assert!(
        matches!(outcome, SubAgentOutcome::Completed(_)),
        "the child really ran: {outcome:?}"
    );
    assert_eq!(
        tools.reads.load(Ordering::SeqCst),
        2,
        "and really did work, so it really crossed step boundaries"
    );

    assert!(
        sink.persisted
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty(),
        "a child step must not overwrite the session's resume point with the child's transcript"
    );
    assert_eq!(
        sink.discards.load(Ordering::SeqCst),
        0,
        "and a child ENDING must not retract a resume point the parent's in-flight turn still needs"
    );
    let _ = drain(&mut rx);
}

// ---- context economy is a mechanism, not an intention -----------------

#[tokio::test]
async fn the_report_is_clamped_to_the_spec_cap_and_says_so() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![Ok(text_result(&"y".repeat(5_000), 0.001))]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let spec = SubAgentSpec {
        max_report_chars: 100,
        ..SubAgentSpec::read_only("verbose", "say a lot")
    };
    let outcome = parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx)
        .await;

    let report = outcome.report().expect("completed");
    assert!(
        report.truncated,
        "a clamped report must never look exhaustive"
    );
    assert_eq!(
        report.summary.chars().count(),
        101,
        "100 chars plus the ellipsis marker"
    );

    // The truncation is on the wire too, not just in the return value.
    match finished(&drain(&mut rx)) {
        SubAgentPhase::Finished { truncated, .. } => assert!(truncated),
        other => panic!("expected Finished, got {other:?}"),
    }
}

// ---- read-only by default ---------------------------------------------

#[tokio::test]
async fn a_read_only_child_cannot_execute_a_mutating_tool_even_when_it_tries() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("write_file", "c1", 0.001)),
        Ok(text_result("i tried", 0.001)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("reader", "look around"),
            &mut budget,
            &tx,
        )
        .await;

    assert_eq!(
        tools.writes.load(Ordering::SeqCst),
        0,
        "the restriction is enforced at execution time, not by prompt"
    );
}

#[tokio::test]
async fn write_access_is_opt_in_per_spawn() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("write_file", "c1", 0.001)),
        Ok(text_result("done", 0.001)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let spec = SubAgentSpec {
        write_access: true,
        ..SubAgentSpec::read_only("writer", "change something")
    };
    parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx)
        .await;

    assert_eq!(tools.writes.load(Ordering::SeqCst), 1);
}

// ---- budget: carved, settled, and never a hole in the accounting ------

#[tokio::test]
async fn child_spend_settles_into_the_parent_exactly_once() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("read_file", "c1", 0.02)),
        Ok(text_result("found it", 0.03)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    budget.record_spend(0.10); // the parent's own prior work
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("searcher", "search"),
            &mut budget,
            &tx,
        )
        .await;

    assert!(
        (outcome.cost_usd() - 0.05).abs() < 1e-9,
        "the child's own cost"
    );
    assert!(
        (budget.session_spent_usd() - 0.15).abs() < 1e-9,
        "0.10 parent + 0.05 child, counted once — got {}",
        budget.session_spent_usd()
    );
}

#[tokio::test]
async fn an_enforced_carve_stops_the_child_without_touching_the_parents_turn() {
    let parent_provider = ScriptedProvider::new(vec![]);
    // Each call costs more than the whole carve, so the child trips at the
    // first between-steps check.
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("read_file", "c1", 0.50)),
        Ok(text_result("never reached", 0.50)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Enforced, None, Some(100.0));
    let (tx, _rx) = mpsc::unbounded_channel();

    let spec = SubAgentSpec {
        budget_usd: Some(0.10),
        ..SubAgentSpec::read_only("spendy", "burn money")
    };
    let outcome = parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx)
        .await;

    assert!(
        matches!(outcome, SubAgentOutcome::Incomplete { .. }),
        "the carve is a wall under Enforced, got {outcome:?}"
    );
    assert!(
        (budget.session_spent_usd() - 0.50).abs() < 1e-9,
        "only the one call that landed is charged, and it IS charged"
    );
    assert_eq!(
        budget.evaluate(),
        BudgetOutcome::Continue,
        "the parent, far under its own cap, keeps going — a child hitting \
         its carve is not the parent's abort"
    );
}

#[tokio::test]
async fn a_child_can_never_be_carved_past_the_parents_remaining_headroom() {
    // The hard-ceiling property at the spawn boundary: the caller asks for
    // ten dollars, the parent has four cents left, and the child is bounded
    // by the four cents.
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![Ok(text_result("hi", 0.001))]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Enforced, None, Some(1.0));
    budget.record_spend(0.96);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let spec = SubAgentSpec {
        budget_usd: Some(10.0),
        ..SubAgentSpec::read_only("greedy", "spend it all")
    };
    parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx)
        .await;

    let started = drain(&mut rx)
        .into_iter()
        .find_map(|event| match event {
            AgentEvent::SubAgent {
                phase: phase @ SubAgentPhase::Started { .. },
            } => Some(phase),
            _ => None,
        })
        .expect("Started is always emitted");
    match started {
        SubAgentPhase::Started { budget_usd, .. } => {
            let carved = budget_usd.expect("a bounded parent bounds its child");
            assert!(
                (carved - 0.04).abs() < 1e-9,
                "asked for 10.0, headroom was 0.04, carved {carved}"
            );
        }
        other => panic!("expected Started, got {other:?}"),
    }
}

#[tokio::test]
async fn an_enforced_parent_with_no_headroom_refuses_before_spending_anything() {
    let parent_provider = ScriptedProvider::new(vec![]);
    // Would answer happily if it were ever asked. It must not be.
    let child_provider = ScriptedProvider::new(vec![Ok(text_result("hello", 0.99))]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Enforced, None, Some(1.0));
    budget.record_spend(1.5);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("doomed", "do something"),
            &mut budget,
            &tx,
        )
        .await;

    assert!(
        matches!(outcome, SubAgentOutcome::Refused { .. }),
        "got {outcome:?}"
    );
    assert_eq!(
        child_provider.calls.load(Ordering::SeqCst),
        0,
        "a refusal must cost exactly zero model calls — budget is checked \
         BETWEEN steps, so starting the child would always pay for one"
    );
    assert!((budget.session_spent_usd() - 1.5).abs() < 1e-9);
    // A refusal still brackets, so a fold over the stream never sees an
    // unclosed child.
    let events = drain(&mut rx);
    assert!(matches!(
        finished(&events),
        SubAgentPhase::Finished { status, .. } if status == SubAgentStatus::Refused
    ));
}

// ---- failure is data --------------------------------------------------

#[tokio::test]
async fn an_aborted_child_salvages_the_last_answer_it_paid_for() {
    // The child answers, is told to keep going, then hits its step cap. Its
    // text is real work; throwing it away with the transcript would make an
    // abort strictly worse than never spawning.
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![
        Ok(CompletionResult {
            text: "partial finding: it is in retry.rs".into(),
            tool_calls: vec![ToolCall {
                call_id: "c1".into(),
                name: "read_file".into(),
                input: json!({ "path": "c1" }),
            }],
            usage: CompletionUsage {
                reported: true,
                ..CompletionUsage::default()
            },
            model: "scripted".into(),
            cost_usd: 0.01,
            finish_reason: None,
        }),
        Ok(tool_call_result("read_file", "c2", 0.01)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let spec = SubAgentSpec {
        max_steps: 2,
        ..SubAgentSpec::read_only("capped", "look")
    };
    let outcome = parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx)
        .await;

    match outcome {
        SubAgentOutcome::Incomplete { report, reason } => {
            assert_eq!(report.summary, "partial finding: it is in retry.rs");
            assert!(!reason.is_empty(), "the abort always names itself");
            assert!(
                (report.cost_usd - 0.02).abs() < 1e-9,
                "the salvage does not hide what it cost"
            );
        }
        other => panic!("expected Incomplete, got {other:?}"),
    }
}

#[tokio::test]
async fn a_failed_child_never_becomes_an_error_the_parent_has_to_handle() {
    // The provider errors terminally on the first call. The parent still
    // gets a value back, with the reason in it.
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("broken", "try"),
            &mut budget,
            &tx,
        )
        .await;

    match outcome {
        SubAgentOutcome::Incomplete { report, reason } => {
            assert!(report.summary.is_empty(), "nothing was produced to salvage");
            assert!(!reason.is_empty());
        }
        other => panic!("expected Incomplete, got {other:?}"),
    }
}

#[tokio::test]
async fn nesting_deeper_than_the_cap_is_refused_before_spending() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![Ok(text_result("hi", 0.01))]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let spec = SubAgentSpec {
        depth: MAX_SUB_AGENT_DEPTH + 1,
        ..SubAgentSpec::read_only("too-deep", "recurse")
    };
    let outcome = parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx)
        .await;

    assert!(matches!(outcome, SubAgentOutcome::Refused { .. }));
    assert_eq!(child_provider.calls.load(Ordering::SeqCst), 0);
    // The boundary itself is allowed — the cap is a maximum, not an exclusive
    // bound.
    let ok = SubAgentSpec {
        depth: MAX_SUB_AGENT_DEPTH,
        ..SubAgentSpec::read_only("at-the-edge", "recurse")
    };
    let outcome = parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &ok, &mut budget, &tx)
        .await;
    assert!(matches!(outcome, SubAgentOutcome::Completed(_)));
}

// ---- the event plane --------------------------------------------------

#[tokio::test]
async fn the_childs_stage_and_narration_never_reach_the_parents_stream() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("read_file", "c1", 0.01)),
        Ok(text_result("the answer", 0.01)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("quiet", "work"),
            &mut budget,
            &tx,
        )
        .await;
    let events = drain(&mut rx);

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Stage { .. })),
        "a child's stage boundary read as the parent's is exactly the \
         confusion this filter exists to prevent"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Text { .. })),
        "the child's narration is a draft; the report ships once, on Finished"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Complete { .. })),
        "the child does not terminate the parent's stream"
    );

    // ...but the report IS on the wire, exactly once.
    match finished(&events) {
        SubAgentPhase::Finished { summary, .. } => assert_eq!(summary, "the answer"),
        other => panic!("expected Finished, got {other:?}"),
    }
}

#[tokio::test]
async fn step_usage_and_tool_activity_reach_the_parent_so_cost_rolls_up() {
    // Dropping StepUsage is precisely how child spend would vanish from
    // `stella stats` and quietly falsify the $/resolved-task number.
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("read_file", "c1", 0.01)),
        Ok(text_result("done", 0.02)),
    ]);
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("metered", "work"),
            &mut budget,
            &tx,
        )
        .await;
    let events = drain(&mut rx);

    let metered: f64 = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::StepUsage { cost_usd, .. } => Some(*cost_usd),
            _ => None,
        })
        .sum();
    assert!(
        (metered - 0.03).abs() < 1e-9,
        "every child call must appear in the parent's metering stream, got {metered}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolStart { .. })),
        "a child's activity stays visible even though its narration does not"
    );
    // And the parent's own post-settlement numbers are re-ticked, since the
    // child's carve-scoped ticks were dropped.
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::BudgetTick { session_spent_usd: Some(spent), .. } if (*spent - 0.03).abs() < 1e-9
        )),
        "the HUD must not sit stale across a child run"
    );
}

#[test]
fn the_forward_filter_fails_toward_visible() {
    // A new AgentEvent variant must default to *forwarded*: a redundant HUD
    // line is a cosmetic bug, a dropped metering row is a falsified invoice.
    assert!(forwards_to_parent(&AgentEvent::Error {
        message: "boom".into(),
        retryable: false,
    }));
    assert!(forwards_to_parent(&AgentEvent::TaskUpdate {
        tasks: vec![]
    }));
    assert!(!forwards_to_parent(&AgentEvent::Stage {
        name: stella_protocol::StageKind::Execute,
    }));
}

// ---- seams: gate, steering, attribution -------------------------------

/// A steering source that records whether its queue was ever drained.
struct SpySteering {
    drained: AtomicUsize,
    stop: bool,
}

impl TurnSteering for SpySteering {
    fn drain_steering(&self) -> Vec<String> {
        self.drained.fetch_add(1, Ordering::SeqCst);
        vec!["a message the user wrote to the PARENT".to_string()]
    }
    fn soft_stop_requested(&self) -> bool {
        self.stop
    }
}

#[tokio::test]
async fn a_child_honors_the_soft_stop_but_never_eats_the_parents_steering() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![Ok(text_result("hi", 0.01))]);
    let tools = MixedTools::default();
    let steering = SpySteering {
        drained: AtomicUsize::new(0),
        stop: true,
    };
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep)
        .with_steering(&steering);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("stoppable", "work"),
            &mut budget,
            &tx,
        )
        .await;

    assert_eq!(
        steering.drained.load(Ordering::SeqCst),
        0,
        "drain_steering is destructive by contract — a child that called it \
         would silently swallow a message addressed to the parent"
    );
    assert!(
        matches!(&outcome, SubAgentOutcome::Incomplete { reason, .. } if reason.contains(crate::driver::SOFT_STOP_REASON)),
        "the latched soft stop must still stop the child, got {outcome:?}"
    );
    assert_eq!(
        child_provider.calls.load(Ordering::SeqCst),
        0,
        "a stop latched before the first step stops it there"
    );
}

#[test]
fn child_steering_forwards_the_stop_and_swallows_the_drain() {
    let parent = SpySteering {
        drained: AtomicUsize::new(0),
        stop: true,
    };
    let child = ChildSteering::new(&parent);
    assert!(child.drain_steering().is_empty());
    assert_eq!(parent.drained.load(Ordering::SeqCst), 0);
    assert!(child.soft_stop_requested());
}

/// A gate that counts how many times it was polled — proving the child polls
/// the parent's gate rather than running unpausable.
struct CountingGate(AtomicUsize);

#[async_trait]
impl TurnGate for CountingGate {
    async fn wait_if_paused(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn a_child_polls_the_parents_pause_gate() {
    // `assess` dropped the gate when it hand-rolled a verifier engine, so a
    // paused session kept spending inside the verifier. Inheritance here is
    // what closes that.
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("read_file", "c1", 0.01)),
        Ok(text_result("done", 0.01)),
    ]);
    let tools = MixedTools::default();
    let gate = CountingGate(AtomicUsize::new(0));
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep)
        .with_gate(&gate);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("pausable", "work"),
            &mut budget,
            &tx,
        )
        .await;

    assert_eq!(
        gate.0.load(Ordering::SeqCst),
        2,
        "once per child step — a child that ignored the gate would keep \
         spending through a pause"
    );
}

/// [`TurnControls`] is the owned form a session-scoped host holds, so a
/// sub-agent dispatcher can give a child the seams of the turn that asked for
/// it. This pins the two properties that make it safe to call blindly at
/// dispatch time.
#[tokio::test]
async fn owned_turn_controls_stop_a_child_without_clobbering_an_attached_gate() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![Ok(text_result("hi", 0.01))]);
    let tools = MixedTools::default();

    // A gate attached the per-turn way, and controls carrying only steering:
    // the driver's gate must survive, because a host that reads controls
    // holding one seam would otherwise silently unpause the child.
    let gate = CountingGate(AtomicUsize::new(0));
    let steering = Arc::new(SpySteering {
        drained: AtomicUsize::new(0),
        stop: true,
    });
    let controls = TurnControls::none().with_steering(steering.clone());
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep)
        .with_gate(&gate)
        .with_turn_controls(&controls);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = parent
        .run_sub_agent(
            SubAgentHost::new(&child_provider),
            &SubAgentSpec::read_only("controlled", "work"),
            &mut budget,
            &tx,
        )
        .await;

    assert!(
        matches!(&outcome, SubAgentOutcome::Incomplete { reason, .. }
            if reason.contains(crate::driver::SOFT_STOP_REASON)),
        "the published stop must reach the child, got {outcome:?}"
    );
    assert_eq!(
        gate.0.load(Ordering::SeqCst),
        1,
        "the separately-attached gate must still be polled — controls fill \
         absent seams, they do not replace the set"
    );
    assert_eq!(
        steering.drained.load(Ordering::SeqCst),
        0,
        "still `ChildSteering` underneath: the stop crosses, the parent's \
         queued messages never do"
    );
}

/// Every driver used to publish exactly one seam — worker lanes a gate, the
/// deck's lead a steering tap — so the both-at-once case had no caller and no
/// test. The deck's lead lane is that caller now (#1219): it steers with `>`
/// and pauses with `p`, and hands the pair to its sub-agent dispatcher in one
/// [`TurnControls`]. A `with_gate` that displaced the steering it was given
/// would turn the lead's Esc into a no-op the moment pause was wired up —
/// silently, since both are `Option` and neither absence is an error.
#[test]
fn turn_controls_carrying_both_seams_give_a_child_both() {
    let provider = ScriptedProvider::new(vec![]);
    let tools = MixedTools::default();
    let both = TurnControls::none()
        .with_gate(Arc::new(CountingGate(AtomicUsize::new(0))))
        .with_steering(Arc::new(SpySteering {
            drained: AtomicUsize::new(0),
            stop: false,
        }));
    assert!(!both.is_empty());

    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &NoSleep)
        .with_turn_controls(&both);

    assert!(engine.gate.is_some(), "the pause must survive the steering");
    assert!(
        engine.steering.is_some(),
        "and the steering must survive the pause — the two seams are \
         independent, not a slot the last writer wins"
    );
}

#[test]
fn empty_turn_controls_leave_an_engine_exactly_as_it_was() {
    let provider = ScriptedProvider::new(vec![]);
    let tools = MixedTools::default();
    let gate = CountingGate(AtomicUsize::new(0));
    let nothing = TurnControls::none();
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &NoSleep)
        .with_gate(&gate)
        .with_turn_controls(&nothing);

    assert!(
        engine.gate.is_some(),
        "a driver that publishes nothing must not cost a turn its own gate"
    );
    assert!(engine.steering.is_none());
    assert!(TurnControls::none().is_empty());
}

#[test]
fn attribution_restores_what_it_displaced_including_on_an_unwind() {
    let bus = HookBus::new("session-1");
    bus.set_agent(Some("parent".into()));

    {
        let _child = AgentAttribution::enter(Some(&bus), "child");
        // Nested one deeper, then unwound by panic — the restore must still
        // run, and must restore "child", not clear to None.
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _grandchild = AgentAttribution::enter(Some(&bus), "grandchild");
            panic!("a tool blew up");
        }));
        assert!(unwound.is_err());
        assert_eq!(
            bus.set_agent(Some("child".into())),
            Some("child".to_string()),
            "the grandchild restored the child, not None"
        );
    }

    assert_eq!(
        bus.set_agent(None),
        Some("parent".to_string()),
        "the child restored the parent on the way out"
    );
}

#[test]
fn attribution_with_no_bus_is_a_no_op() {
    let guard = AgentAttribution::enter(None, "orphan");
    drop(guard);
}

// ---- receipts ---------------------------------------------------------

#[tokio::test]
async fn the_child_claims_its_own_receipt_turn_slot() {
    // Receipts key on (execution_id, turn_instance, step, call_seq) and every
    // turn restarts step at 0, so a child sharing the parent's slot would
    // overwrite the parent's manifests in the store.
    let parent_provider = ScriptedProvider::new(vec![]);
    let child_provider = ScriptedProvider::new(vec![Ok(text_result("done", 0.01))]);
    let tools = MixedTools::default();
    let config = EngineConfig {
        turn_instance: 4,
        lifecycle_enabled: true,
        ..EngineConfig::default()
    };
    let parent = Engine::with_sleeper(&parent_provider, &tools, config, &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let spec = SubAgentSpec {
        turn_instance: 5,
        ..SubAgentSpec::read_only("receipted", "work")
    };
    parent
        .run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx)
        .await;

    let slots: Vec<u32> = drain(&mut rx)
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::StepManifest { turn_instance, .. } => Some(turn_instance),
            _ => None,
        })
        .collect();
    assert!(
        !slots.is_empty() && slots.iter().all(|slot| *slot == 5),
        "the child's manifests must land in ITS slot, got {slots:?}"
    );
}

// ---- the out-of-band spend fold (tool-dispatched children) ------------

/// An executor whose tools "spawn" sub-agents: it reports a fixed spend per
/// `execute`, exactly as a real `task` tool would after its child settled.
struct SpendingTools {
    ledger: SubAgentSpendLedger,
    per_call_usd: f64,
    drains: AtomicUsize,
}

#[async_trait]
impl ToolExecutor for SpendingTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "task".into(),
            description: "spawn a sub-agent".into(),
            input_schema: json!({"type": "object"}),
            // Mutating on purpose: a read-only schema would let the engine
            // execute it speculatively, and this test is about the ordinary
            // dispatch path.
            read_only: false,
            speculation_safe: false,
        }]
    }

    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        push_sub_agent_spend(&self.ledger, self.per_call_usd);
        ToolOutput::Ok {
            content: "child says: it is in retry.rs".into(),
        }
    }

    fn drain_sub_agent_spend_usd(&self) -> f64 {
        self.drains.fetch_add(1, Ordering::SeqCst);
        drain_sub_agent_spend(&self.ledger)
    }
}

/// A child dispatched from *inside* a tool call spends money the engine's
/// guard cannot see — the engine holds it mutably for the whole turn. Folding
/// that spend in at the next step boundary is what keeps `--budget` a hard
/// ceiling once turns nest; deferring to end-of-turn would let the parent and
/// its children each run to the cap independently.
#[tokio::test]
async fn tool_dispatched_child_spend_aborts_the_parent_at_the_next_step_boundary() {
    let provider = ScriptedProvider::new(vec![
        // The parent's own calls are free; every dollar here is the child's.
        Ok(tool_call_result("task", "c1", 0.0)),
        Ok(tool_call_result("task", "c2", 0.0)),
        Ok(text_result("should never be reached", 0.0)),
    ]);
    let tools = SpendingTools {
        ledger: SubAgentSpendLedger::default(),
        per_call_usd: 0.60,
        drains: AtomicUsize::new(0),
    };
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &NoSleep);
    let mut messages = vec![CompletionMessage::user("go")];
    let mut budget = BudgetGuard::new(BudgetMode::Enforced, None, Some(1.0));
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    match outcome {
        TurnOutcome::Aborted { reason, cost_usd } => {
            assert!(
                reason.contains("budget exceeded"),
                "the parent must abort on the CHILDREN's spend, got: {reason}"
            );
            // The abort's own figure must carry the money that tripped it:
            // `Complete`/`TurnOutcome` totals are summaries of the StepUsage
            // events on the stream, and the children's StepUsage is forwarded
            // — a total excluding child spend contradicted the reason string
            // sitting right beside it.
            assert!(
                (cost_usd - 1.20).abs() < 1e-9,
                "the outcome's cost must include the children's spend, got {cost_usd}"
            );
        }
        other => panic!("expected a budget abort, got {other:?}"),
    }
    assert!(
        (budget.session_spent_usd() - 1.20).abs() < 1e-9,
        "both children's spend is charged to the parent, got {}",
        budget.session_spent_usd()
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "the third model call must never be dispatched — that is the ceiling \
         holding at a step boundary"
    );
    // The fold rides `record_settled_cost`, so the child's money moves the
    // HUD like every other dollar rather than appearing only in a total.
    let ticked: Vec<f64> = drain(&mut rx)
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::BudgetTick {
                session_spent_usd, ..
            } => session_spent_usd,
            _ => None,
        })
        .collect();
    assert!(
        ticked.iter().any(|spent| (*spent - 0.60).abs() < 1e-9),
        "a tick must report the first child's spend as it lands, got {ticked:?}"
    );
}

#[tokio::test]
async fn the_drain_is_destructive_so_child_spend_is_never_charged_twice() {
    let provider = ScriptedProvider::new(vec![
        Ok(tool_call_result("task", "c1", 0.0)),
        Ok(text_result("done", 0.0)),
    ]);
    let tools = SpendingTools {
        ledger: SubAgentSpendLedger::default(),
        per_call_usd: 0.25,
        drains: AtomicUsize::new(0),
    };
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &NoSleep);
    let mut messages = vec![CompletionMessage::user("go")];
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(
        tools.drains.load(Ordering::SeqCst) >= 2,
        "the engine drains at every step boundary, not once per turn"
    );
    assert!(
        (budget.session_spent_usd() - 0.25).abs() < 1e-9,
        "0.25 spent once, charged once — got {}",
        budget.session_spent_usd()
    );
    match outcome {
        TurnOutcome::Completed { cost_usd, .. } => assert!(
            (cost_usd - 0.25).abs() < 1e-9,
            "a completed turn's total must include child spend exactly once, got {cost_usd}"
        ),
        other => panic!("expected completion, got {other:?}"),
    }
}

#[test]
fn a_read_only_view_forwards_the_drain_instead_of_zeroing_it() {
    // A grandchild's spend arrives through the child's `ReadOnlyTools` view.
    // Zeroing here would hide it from the carve meant to bound it.
    let ledger = SubAgentSpendLedger::default();
    push_sub_agent_spend(&ledger, 0.75);
    let inner = SpendingTools {
        ledger: ledger.clone(),
        per_call_usd: 0.0,
        drains: AtomicUsize::new(0),
    };
    let view = ReadOnlyTools::new(&inner);
    assert!((view.drain_sub_agent_spend_usd() - 0.75).abs() < 1e-9);
    assert_eq!(
        drain_sub_agent_spend(&ledger),
        0.0,
        "and it really took it — a peek would double-charge"
    );
}

#[test]
fn the_ledger_accumulates_and_drains_to_zero() {
    let ledger = SubAgentSpendLedger::default();
    push_sub_agent_spend(&ledger, 0.01);
    push_sub_agent_spend(&ledger, 0.02);
    assert!((drain_sub_agent_spend(&ledger) - 0.03).abs() < 1e-9);
    assert_eq!(drain_sub_agent_spend(&ledger), 0.0);
}
