use super::*;

// ---- the out-of-band spend fold (tool-dispatched children) ------------

/// An executor whose tools "spawn" sub-agents: it reports a fixed spend per
/// `execute`, exactly as a real `delegate` tool would after its child settled.
struct SpendingTools {
    ledger: SubAgentSpendLedger,
    per_call_usd: f64,
    drains: AtomicUsize,
}

#[async_trait]
impl ToolExecutor for SpendingTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "delegate".into(),
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
            data: None,
        }
    }

    fn drain_sub_agent_spend_usd(&self) -> f64 {
        self.drains.fetch_add(1, Ordering::SeqCst);
        drain_sub_agent_spend(&self.ledger)
    }
}

/// A child dispatched from *inside* a tool call spends money the engine's
/// guard cannot see — the engine holds it mutably for the whole turn. Folding
/// that spend in at the next step boundary is what keeps `--spend-limit` a hard
/// ceiling once turns nest; deferring to end-of-turn would let the parent and
/// its children each run to the cap independently.
#[tokio::test]
async fn tool_dispatched_child_spend_aborts_the_parent_at_the_next_step_boundary() {
    let provider = ScriptedProvider::new(vec![
        // The parent's own calls are free; every dollar here is the child's.
        Ok(tool_call_result("delegate", "c1", 0.0)),
        Ok(tool_call_result("delegate", "c2", 0.0)),
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
        TurnOutcome::Aborted {
            reason, cost_usd, ..
        } => {
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
        Ok(tool_call_result("delegate", "c1", 0.0)),
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

// ---- cancellation (#1954) --------------------------------------------

/// Serves its script, then hangs forever — and says so on `hang_reached`,
/// which is what lets a test cancel the child at a *deterministic* point
/// instead of racing a wall-clock timeout.
struct HangAfterScript {
    script: Mutex<Vec<Result<CompletionResult, ProviderError>>>,
    hang_reached: std::sync::Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Provider for HangAfterScript {
    fn id(&self) -> &str {
        "hanging"
    }

    async fn complete_ref(
        &self,
        _request: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        let next = self.script.lock().unwrap().pop();
        match next {
            Some(result) => result,
            None => {
                self.hang_reached.notify_one();
                std::future::pending().await
            }
        }
    }
}

/// #1954 witness: a caller that drops the sub-agent future mid-flight still
/// gets a **balanced** bracket whose `Finished` carries the committed step
/// count and cost, after the abandoned call's `UsageIncomplete { Cancelled }`
/// envelope — and the money still settles into the parent's guard. Before
/// `CancelBracket`, the `Started` bracket stayed open forever and every
/// ceiling-bearing caller had to forge a `Finished` it could only fill with
/// `steps: 0`.
#[tokio::test]
async fn a_cancelled_child_closes_its_bracket_with_committed_steps_and_cost() {
    let parent_provider = ScriptedProvider::new(vec![]);
    let hang_reached = std::sync::Arc::new(tokio::sync::Notify::new());
    // One committed step (a tool call), then the second model call hangs.
    let child_provider = HangAfterScript {
        script: Mutex::new(vec![Ok(tool_call_result("read_file", "c1", 0.002))]),
        hang_reached: hang_reached.clone(),
    };
    let tools = MixedTools::default();
    let parent = Engine::with_sleeper(&parent_provider, &tools, EngineConfig::default(), &NoSleep);
    let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let spec = SubAgentSpec::read_only("search-1", "find it");

    {
        let fut = parent.run_sub_agent(SubAgentHost::new(&child_provider), &spec, &mut budget, &tx);
        let mut fut = std::pin::pin!(fut);
        tokio::select! {
            _ = &mut fut => unreachable!("a hanging child cannot complete"),
            _ = hang_reached.notified() => {}
        }
        // `fut` drops here: the cancel every latency-ceiling caller performs.
    }

    let events = drain(&mut rx);
    let started = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::SubAgent {
                    phase: SubAgentPhase::Started { .. }
                }
            )
        })
        .count();
    let finished: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SubAgent {
                phase: phase @ SubAgentPhase::Finished { .. },
            } => Some(phase.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(started, 1);
    assert_eq!(
        finished.len(),
        1,
        "the bracket must close exactly once on a cancel: {events:?}"
    );
    match &finished[0] {
        SubAgentPhase::Finished {
            status,
            steps,
            cost_usd,
            reason,
            ..
        } => {
            assert_eq!(*status, SubAgentStatus::Incomplete);
            assert_eq!(
                *steps, 1,
                "the committed step count, not a forged zero (#1954)"
            );
            assert!(
                (*cost_usd - 0.002).abs() < 1e-9,
                "the committed cost: {cost_usd}"
            );
            let reason = reason.as_deref().unwrap_or_default();
            assert!(reason.contains("cancelled"), "the close says why: {reason}");
        }
        SubAgentPhase::Started { .. } => unreachable!(),
    }
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::UsageIncomplete {
                reason: stella_protocol::UsageIncompleteReason::Cancelled,
                ..
            }
        )),
        "the abandoned in-flight call owes its envelope: {events:?}"
    );
    assert!(
        (budget.session_spent_usd() - 0.002).abs() < 1e-9,
        "the committed spend still settles on the drop path: {}",
        budget.session_spent_usd()
    );
}
