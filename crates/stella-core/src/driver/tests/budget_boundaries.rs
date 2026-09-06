//! Budget-boundary witnesses: an over-cap call is settled spend but the next
//! provider call never starts, an already-breached turn never pays for
//! compaction, a billed completion and its usage envelope land exactly once
//! even when a speculation is still in flight, both top-of-step aborts
//! hand back a transcript the next provider call still accepts, and a step
//! with a slow tool stops the turn before the deadline instead of after it.

use super::*;

/// The call id the two pairing witnesses below start their transcript with and
/// then look for an answer to.
const DANGLING_CALL_ID: &str = "dangling-call";

/// A transcript whose tail is an assistant `tool_use` nothing answered.
///
/// A caller reaches this shape without doing anything wrong: a hard-dropped
/// turn hands its history back mid-step, and `Checkpoint::from_json` restores
/// one after checking the version and never the pairing. Whatever produced it,
/// the next provider call rejects it outright unless the abort that returned it
/// closed the pairing first.
fn transcript_with_an_unanswered_tool_call() -> Vec<CompletionMessage> {
    vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("the task"),
        CompletionMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                call_id: DANGLING_CALL_ID.into(),
                name: "bash".into(),
                input: serde_json::json!({}),
            }],
            tool_results: Vec::new(),
            attachments: Vec::new(),
        },
    ]
}

/// Whether some `Tool` message answers [`DANGLING_CALL_ID`] — the pairing rule
/// every provider enforces, asked of the vector the caller gets back.
fn answers_the_dangling_call(messages: &[CompletionMessage]) -> bool {
    messages.iter().any(|message| {
        message.role == MessageRole::Tool
            && message
                .tool_results
                .iter()
                .any(|result| result.call_id == DANGLING_CALL_ID)
    })
}

/// A provider that fails the test if the turn ever calls it — both witnesses
/// below abort at the top of the step, before any model call.
fn provider_that_must_not_be_called() -> ScriptedProvider {
    ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Ok(text_result("must never be called"))]),
        calls: Arc::new(AtomicU32::new(0)),
    }
}

#[tokio::test]
async fn an_over_cap_budget_abort_hands_back_a_well_paired_transcript() {
    // The dollar arm of `settlement::check_budget` fires at
    // the top of the step, before any model call, on a transcript that is
    // still exactly what the caller handed in. Every other exit at this
    // boundary closes the pairing; this one returned the open `tool_use`
    // untouched, and the caller's next turn was rejected by the vendor with
    // nothing pointing back at the abort that caused it.
    let provider = provider_that_must_not_be_called();
    let provider_calls = provider.calls.clone();
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let seams = TurnCapabilities::none();
    let engine = Engine::assemble(&provider, &tools, EngineConfig::default(), &sleeper, seams);
    let mut messages = transcript_with_an_unanswered_tool_call();
    let mut budget = BudgetGuard::new(BudgetMode::Enforced, None, Some(0.05));
    budget.reseed_session_spend(0.10);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(
        matches!(outcome, TurnOutcome::Aborted { ref reason, .. } if reason.contains("budget")),
        "expected the over-cap abort, got {outcome:?}"
    );
    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        0,
        "the abort must land before the model call, which is what makes the transcript the \
         caller's own"
    );
    assert!(
        answers_the_dangling_call(&messages),
        "a budget abort must answer the open tool_use it hands back, or the caller's next \
         provider call is rejected: {messages:?}"
    );
}

#[tokio::test]
async fn a_past_deadline_abort_hands_back_a_well_paired_transcript() {
    // The same witness for the deadline arm, which is checked first and has
    // its own reason string — and had the same missing repair.
    let provider = provider_that_must_not_be_called();
    let provider_calls = provider.calls.clone();
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let seams = TurnCapabilities::none();
    let engine = Engine::assemble(&provider, &tools, EngineConfig::default(), &sleeper, seams);
    let mut messages = transcript_with_an_unanswered_tool_call();
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    budget.set_task_deadline(Some(
        std::time::Instant::now() - std::time::Duration::from_secs(1),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(
        matches!(outcome, TurnOutcome::Aborted { ref reason, .. } if reason.contains("deadline")),
        "expected the deadline abort, got {outcome:?}"
    );
    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        0,
        "a task past its deadline stops before the next call"
    );
    assert!(
        answers_the_dangling_call(&messages),
        "a deadline abort must answer the open tool_use it hands back: {messages:?}"
    );
}

struct BilledResultWithBlockedSpeculation {
    provider_completed: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Provider for BilledResultWithBlockedSpeculation {
    fn id(&self) -> &str {
        "billed-blocked-speculation"
    }

    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResultAlias, ProviderError> {
        unreachable!("the test requires complete_observed")
    }

    async fn complete_observed_ref(
        &self,
        _req: CompletionRequestRef<'_>,
        observer: &dyn stella_protocol::ToolCallObserver,
    ) -> Result<CompletionResultAlias, ProviderError> {
        let call = ToolCall {
            call_id: "blocked-read".into(),
            name: "read_forever".into(),
            input: serde_json::json!({}),
        };
        observer.tool_call_streamed(&call);
        self.provider_completed.notify_one();
        Ok(CompletionResultAlias {
            upstream_provider: None,
            text: String::new(),
            tool_calls: vec![call],
            usage: CompletionUsage {
                reported: true,
                ..CompletionUsage::default()
            },
            model: self.id().into(),
            cost_usd: 0.25,
            finish_reason: None,
        })
    }
}

struct ForeverRead;

#[async_trait]
impl ToolExecutor for ForeverRead {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "read_forever".into(),
            description: "a deterministic blocked read".into(),
            input_schema: serde_json::json!({"type": "object"}),
            read_only: true,
            speculation_safe: false,
        }]
    }

    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        std::future::pending().await
    }
}

#[tokio::test]
async fn summary_induced_budget_breach_aborts_with_cost_before_next_provider_call() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(text_result("SUMMARY: earlier steps established the plan")),
            Ok(text_result("must never be called")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let provider_calls = provider.calls.clone();
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let seams = TurnCapabilities::none();
    let engine = Engine::assemble(&provider, &tools, overflow_config(), &sleeper, seams);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("the task"),
    ];
    for i in 0..6 {
        messages.push(big_assistant_text(&format!("t{i}")));
    }
    let mut budget = BudgetGuard::new(BudgetMode::Enforced, Some(0.00005), None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    match outcome {
        TurnOutcome::Aborted {
            reason,
            kind,
            cost_usd,
        } => {
            assert!(reason.contains("budget"));
            assert_eq!(
                kind,
                AbortKind::DeliberateStop,
                "a budget stop is the engine's own policy, not a crash"
            );
            assert!(
                (cost_usd - 0.0001).abs() < 1e-9,
                "the abort must retain the settled summary call: {cost_usd}"
            );
        }
        other => panic!("expected a budget abort, got {other:?}"),
    }
    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        1,
        "the summary call may cross the cap, but the next provider call must not start"
    );
}

#[tokio::test]
async fn an_existing_budget_breach_stops_before_paid_compaction() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Ok(text_result("must never be called"))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let provider_calls = provider.calls.clone();
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let seams = TurnCapabilities::none();
    let engine = Engine::assemble(&provider, &tools, overflow_config(), &sleeper, seams);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("the task"),
    ];
    for i in 0..6 {
        messages.push(big_assistant_text(&format!("t{i}")));
    }
    let mut budget = BudgetGuard::new(BudgetMode::Enforced, None, Some(0.05));
    budget.reseed_session_spend(0.10);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(matches!(
        outcome,
        TurnOutcome::Aborted { cost_usd, .. } if cost_usd == 0.0
    ));
    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        0,
        "an already-over-cap turn must not pay for compaction"
    );
}

#[tokio::test]
async fn a_past_task_deadline_stops_the_turn_before_the_next_call_with_partial_work() {
    // The witness for #1481: the dollar budget is per turn/session, but a
    // benchmark's limit is per TASK — several turns that each honestly fit
    // their own dollar budget can still blow the task's wall clock. A task
    // deadline closes that gap: checked at the exact same safe boundary as
    // the dollar budget (never mid-tool), a task already past its deadline
    // must stop before paying for the next call — exactly like the
    // already-over-cap dollar budget above.
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Ok(text_result(
            "must never be called — the task deadline already passed",
        ))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let provider_calls = provider.calls.clone();
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let seams = TurnCapabilities::none();
    let engine = Engine::assemble(&provider, &tools, EngineConfig::default(), &sleeper, seams);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("the task"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    // The same shape a benchmark harness computes once at task start and
    // threads into every turn's guard for that task (`crate::budget` module
    // docs) — already in the past here, simulating a task whose wall clock
    // ran out mid-session.
    budget.set_task_deadline(Some(
        std::time::Instant::now() - std::time::Duration::from_secs(1),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    match outcome {
        TurnOutcome::Aborted {
            reason,
            kind,
            cost_usd,
        } => {
            assert_eq!(
                kind,
                AbortKind::DeliberateStop,
                "a deadline stop is the engine's own policy, not a crash"
            );
            assert!(
                reason.contains("deadline"),
                "reason should name the deadline: {reason}"
            );
            assert_eq!(cost_usd, 0.0, "must stop before paying for any call");
        }
        other => panic!("expected a deadline abort with partial (zero) work, got {other:?}"),
    }
    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        0,
        "a task past its deadline must stop at the safe boundary before the next provider \
         call — never mid-tool, and never by finishing the script anyway"
    );
}

#[tokio::test]
async fn cancellation_after_billed_completion_before_speculation_finishes_keeps_the_cost() {
    let provider_completed = Arc::new(tokio::sync::Notify::new());
    let provider = BilledResultWithBlockedSpeculation {
        provider_completed: provider_completed.clone(),
    };
    let tools = ForeverRead;
    let sleeper = NoopSleeper;
    let seams = TurnCapabilities::none();
    let engine = Engine::assemble(&provider, &tools, EngineConfig::default(), &sleeper, seams);
    let mut messages = vec![CompletionMessage::user("read")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    {
        let turn = engine.run_turn(&mut messages, &mut budget, &tx);
        tokio::pin!(turn);
        tokio::select! {
            outcome = &mut turn => panic!("blocked speculation must keep the turn pending: {outcome:?}"),
            _ = provider_completed.notified() => {}
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                panic!("provider did not complete")
            }
        }
    }

    assert!(
        (budget.session_spent_usd() - 0.25).abs() < 1e-9,
        "a settled provider result stays billed after cancellation: {}",
        budget.session_spent_usd()
    );
    assert_eq!(
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|event| matches!(event, AgentEvent::StepUsage { .. }))
            .count(),
        1,
        "the no-await billed boundary must also retain exactly one usage envelope"
    );
}

#[tokio::test]
async fn a_normal_completion_charges_the_budget_exactly_once() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Ok(text_result("done"))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let seams = TurnCapabilities::none();
    let engine = Engine::assemble(&provider, &tools, EngineConfig::default(), &sleeper, seams);
    let mut messages = vec![CompletionMessage::user("answer")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    assert!(
        (budget.session_spent_usd() - 0.0001).abs() < 1e-9,
        "normal completion must charge exactly once: {}",
        budget.session_spent_usd()
    );
    assert_eq!(
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|event| matches!(event, AgentEvent::StepUsage { .. }))
            .count(),
        1,
        "the normal path must not re-emit the success envelope"
    );
}

/// A `ToolExecutor` whose one tool sleeps, so the step around it costs far
/// more than the model call inside it.
///
/// Real time, because the reserve is real time: the driver reads
/// `Instant::now`, and a paused tokio clock would leave every step measured
/// at zero.
struct SlowTool {
    took: std::time::Duration,
}

#[async_trait]
impl ToolExecutor for SlowTool {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "bash".into(),
            description: "run a command".into(),
            input_schema: serde_json::json!({"type": "object"}),
            read_only: false,
            speculation_safe: false,
        }]
    }

    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        tokio::time::sleep(self.took).await;
        ToolOutput::Ok {
            content: "ok".into(),
            data: None,
        }
    }
}

#[tokio::test]
async fn a_slow_tool_stops_the_turn_before_the_deadline() {
    // The scripted model answers at once and its tool then runs for a
    // second, so the step costs a second and the call inside it costs
    // nothing. With 600ms left after that step, a reserve measured on the
    // model call alone reads about zero and opens step two, which runs the
    // same slow tool and crosses the deadline. A reserve measured on the
    // whole step reads about a second, and the turn stops with what it has.
    let tool_time = std::time::Duration::from_millis(1_000);
    let deadline_in = std::time::Duration::from_millis(1_600);

    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Ok(tool_call_result("slow-call", "bash"))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let provider_calls = provider.calls.clone();
    let tools = SlowTool { took: tool_time };
    let sleeper = NoopSleeper;
    let seams = TurnCapabilities::none();
    let engine = Engine::assemble(&provider, &tools, EngineConfig::default(), &sleeper, seams);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("the task"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    budget.set_task_deadline(Some(std::time::Instant::now() + deadline_in));
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    let TurnOutcome::Aborted { reason, kind, .. } = outcome else {
        panic!("expected the deadline stop, got {outcome:?}");
    };
    assert_eq!(
        kind,
        AbortKind::DeliberateStop,
        "stopping early is the engine's own policy, not a crash"
    );
    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        1,
        "step two must never start: its tool alone takes {tool_time:?} of a \
         {deadline_in:?} deadline"
    );
    assert!(
        reason.contains("cannot finish"),
        "the turn must stop while the deadline is still ahead, not report an \
         overrun after crossing it: {reason}"
    );
}
