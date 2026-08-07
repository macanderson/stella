//! Management-call deadline and budget-accounting witnesses.

use super::*;

struct DelayedProvider {
    result: TokioMutex<Option<CompletionResult>>,
    delay: Duration,
}

#[async_trait]
impl Provider for DelayedProvider {
    fn id(&self) -> &str {
        "delayed"
    }

    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        tokio::time::sleep(self.delay).await;
        self.result
            .lock()
            .await
            .take()
            .ok_or_else(|| ProviderError::Terminal("delayed provider exhausted".into()))
    }
}

struct AnyProvider<'p>(&'p dyn Provider);

impl ProviderResolver for AnyProvider<'_> {
    fn provider_for(&self, _model: &ModelRef) -> Option<&dyn Provider> {
        Some(self.0)
    }
}

/// A triage call that misses its decision deadline must not silently vanish
/// from the accounting record. The pipeline abandons the answer and falls back
/// to the deterministic floor, but emits an explicit `UsageIncomplete` so the
/// unknowable envelope is visible rather than guessed at (fail closed).
#[tokio::test]
async fn late_triage_is_abandoned_and_reported_incomplete() {
    let mut result = text_result("multi");
    result.cost_usd = 0.05;
    result.usage.input_tokens = 123;
    result.usage.output_tokens = 7;
    let provider = DelayedProvider {
        result: TokioMutex::new(Some(result)),
        delay: Duration::from_millis(20),
    };
    let resolver = AnyProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig {
            triage_latency_ceiling: Duration::from_millis(1),
            ..PipelineConfig::default()
        },
    );
    let mut budget = BudgetGuard::new(BudgetMode::Enforced, Some(1.0), None);
    let mut total = 0.0;

    let (class, _research) = pipeline
        .triage("What is two plus two?", &mut budget, &mut total)
        .await
        .expect("a missed triage deadline is never a run-ending failure");

    // The abandoned answer never classifies, so triage lands on the
    // deterministic floor rather than a guess from a call it did not await.
    assert_eq!(
        class.class,
        resolve_task_class(None, "What is two plus two?")
    );
    // Nothing settled, so nothing is charged — an unknowable envelope is
    // reported as incomplete instead of being invented.
    assert_eq!(total, 0.0);
    assert_eq!(budget.spent_usd(), 0.0);
    let events = drain(&mut rx);
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::UsageIncomplete {
                role: ModelCallRole::Triage,
                reason: stella_protocol::UsageIncompleteReason::Timeout,
                ..
            }
        )),
        "the missed deadline must surface as an explicit incomplete-usage record"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::StepUsage { .. })),
        "an abandoned call must not emit a settled metering record"
    );
}

/// A management call that crosses an enforced turn budget is the last paid
/// call. Its usage is retained, then the pipeline aborts before planning or
/// worker execution.
#[tokio::test]
async fn triage_budget_crossing_aborts_before_a_second_provider_call() {
    let mut triage = text_result("single");
    triage.cost_usd = 0.20;
    let provider = ScriptedProvider::new(vec![triage, text_result("must not run")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig::default(),
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Enforced, Some(0.17), None);
    let outcome = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("budget abort is a normal pipeline outcome");

    assert!(matches!(outcome.status, PipelineStatus::Aborted { .. }));
    assert_eq!(outcome.total_cost_usd, 0.20);
    assert_eq!(outcome.candidates_run, 0);
    assert_eq!(provider.script.lock().await.len(), 1);

    let events = drain(&mut rx);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::StepUsage { .. }))
            .count(),
        1
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Complete { .. }))
    );
}

/// #1483: `Pipeline::verifier` and `verifier_guidance` used to dispatch with
/// `RawCall.timeout: None`, so a hung verifier had nothing bounding the call
/// short of the retry policy — it would sit waiting on the clock that scores
/// the run (bench timeouts) rather than degrading onto the documented cheap
/// fallback. Both now thread `EngineConfig::model_timeout` — the same
/// posture-keyed ceiling the worker path got in #1211/#1277 — into the raw
/// call, the same shape `triage_latency_ceiling` already gave triage.
///
/// A tiny `model_timeout` against a provider that answers well after it
/// proves the call is abandoned at the ceiling: the verdict falls back to
/// the deterministic heuristic (`Verdict::heuristic == true`) instead of the
/// model's real "PASS verified" answer, and the abandonment is recorded as
/// an explicit `UsageIncomplete { role: Verdict, reason: Timeout }` rather
/// than silently waiting the call out.
#[tokio::test]
async fn late_verdict_is_abandoned_and_falls_back_to_the_heuristic() {
    let provider = DelayedProvider {
        result: TokioMutex::new(Some(text_result("PASS verified"))),
        delay: Duration::from_millis(20),
    };
    let resolver = AnyProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig {
            engine: EngineConfig {
                model_timeout: Some(Duration::from_millis(1)),
                ..EngineConfig::default()
            },
            ..PipelineConfig::default()
        },
    );
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let mut total = 0.0;
    let prompt = ManagementPrompt {
        instructions: "verdict?",
        payload: "goal, diff, evidence".into(),
    };
    let inputs = LadderInputs {
        touched_tests_passed: Some(true),
        ..LadderInputs::default()
    };

    let verdict = pipeline
        .verifier(prompt, &inputs, &mut budget, &mut total)
        .await
        .expect("a wedged verifier is never a run-ending failure");

    assert!(
        verdict.heuristic,
        "the abandoned call must fall back to the deterministic heuristic, \
         not wait out the model's real answer"
    );
    assert_eq!(total, 0.0, "nothing settled, so nothing is charged");
    let events = drain(&mut rx);
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::UsageIncomplete {
                role: ModelCallRole::Verdict,
                reason: stella_protocol::UsageIncompleteReason::Timeout,
                ..
            }
        )),
        "the missed deadline must surface as an explicit incomplete-usage record: {events:?}"
    );
}

/// The distress-guidance sibling of the witness above: a hung guidance call
/// now abandons onto its own documented best-effort fallback (`Ok(None)`,
/// evidence-only revision) at the same `model_timeout` ceiling, instead of
/// stalling the revision loop on a wedged verifier.
#[tokio::test]
async fn late_guidance_is_abandoned_and_reported_incomplete() {
    let provider = DelayedProvider {
        result: TokioMutex::new(Some(text_result("try a different approach"))),
        delay: Duration::from_millis(20),
    };
    let resolver = AnyProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig {
            engine: EngineConfig {
                model_timeout: Some(Duration::from_millis(1)),
                ..EngineConfig::default()
            },
            ..PipelineConfig::default()
        },
    );
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let mut total = 0.0;
    let prompt = ManagementPrompt {
        instructions: "guidance?",
        payload: "goal, diff, evidence".into(),
    };

    let guidance = pipeline
        .verifier_guidance(prompt, &mut budget, &mut total)
        .await
        .expect("a wedged guidance call is never a run-ending failure");

    assert_eq!(
        guidance, None,
        "the abandoned call degrades to evidence-only revision, not the \
         model's real (late) steering text"
    );
    assert_eq!(total, 0.0, "nothing settled, so nothing is charged");
    let events = drain(&mut rx);
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::UsageIncomplete {
                role: ModelCallRole::DistressGuidance,
                reason: stella_protocol::UsageIncompleteReason::Timeout,
                ..
            }
        )),
        "the missed deadline must surface as an explicit incomplete-usage record: {events:?}"
    );
}

/// #1501, the residual of #1483: `verifier`/`verifier_guidance` got their
/// clock, but `plan_stage` — which is on the ordinary `stella run` path, not
/// a verification-only one — still dispatched with `RawCall.timeout: None`.
///
/// The Plan case is the same shape as the Verdict one and worse for the same
/// reason: the arm below it *already* degrades `RawCallError::Timeout` to the
/// single-step fallback plan, so without a clock that arm was unreachable
/// code. A dribbling provider parked a headless run indefinitely next to a
/// written-down fallback that could never fire.
///
/// A tiny `model_timeout` against a provider that answers a real multi-step
/// plan well after it proves the call is abandoned at the ceiling: the stage
/// returns the one-step fallback (the goal itself) rather than the model's
/// three parsed steps, and the abandonment is recorded as an explicit
/// `UsageIncomplete { role: Plan, reason: Timeout }`.
#[tokio::test]
async fn a_late_plan_is_abandoned_and_falls_back_to_the_single_step_plan() {
    let provider = DelayedProvider {
        result: TokioMutex::new(Some(text_result(
            r#"["read the failing test", "fix the parser", "run the suite"]"#,
        ))),
        delay: Duration::from_millis(20),
    };
    let resolver = AnyProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig {
            engine: EngineConfig {
                model_timeout: Some(Duration::from_millis(1)),
                ..EngineConfig::default()
            },
            ..PipelineConfig::default()
        },
    );
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let mut total = 0.0;

    let steps = pipeline
        .plan_stage(
            "make the parser stop panicking",
            &[],
            &[],
            "",
            None,
            &mut budget,
            &mut total,
        )
        .await
        .expect("a wedged planner is never a run-ending failure");

    assert_eq!(
        steps,
        vec![PlanStep::new("make the parser stop panicking")],
        "the abandoned call must take the written-down single-step fallback, \
         not wait out the model's real (late) three-step plan"
    );
    assert_eq!(total, 0.0, "nothing settled, so nothing is charged");
    let events = drain(&mut rx);
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::UsageIncomplete {
                role: ModelCallRole::Plan,
                reason: stella_protocol::UsageIncompleteReason::Timeout,
                ..
            }
        )),
        "the missed deadline must surface as an explicit incomplete-usage record: {events:?}"
    );
}

/// Serves one scripted result per call while recording the request-shaping
/// fields that reached it, so a test can assert the *allowance* a management
/// call was dispatched with — `ScriptedProvider` records prompts, not bounds.
struct BoundsProvider {
    script: TokioMutex<VecDeque<CompletionResult>>,
    bounds: std::sync::Mutex<Vec<(Option<u32>, Option<stella_protocol::ReasoningEffort>)>>,
}

impl BoundsProvider {
    fn new(results: Vec<CompletionResult>) -> Self {
        Self {
            script: TokioMutex::new(results.into_iter().collect()),
            bounds: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Each request's `(max_output_tokens, effort)`, in call order.
    fn bounds(&self) -> Vec<(Option<u32>, Option<stella_protocol::ReasoningEffort>)> {
        self.bounds.lock().unwrap().clone()
    }
}

#[async_trait]
impl Provider for BoundsProvider {
    fn id(&self) -> &str {
        "bounds"
    }

    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.bounds
            .lock()
            .unwrap()
            .push((req.max_output_tokens, req.effort));
        let mut q = self.script.lock().await;
        q.pop_front()
            .ok_or_else(|| ProviderError::Terminal("bounds provider exhausted".into()))
    }
}

/// Triage answers in three lines, but with no role bounds it inherited the
/// worker's whole allowance: `engine.max_output_tokens` (the model catalog's
/// full ceiling once the CLI seeds it) and an unset effort, which leaves the
/// provider's default (high) reasoning allowance burning thinking tokens on a
/// classification. The chokepoint must pin the bounded shape instead.
#[tokio::test]
async fn triage_dispatches_with_pinned_management_bounds() {
    let provider = BoundsProvider::new(vec![text_result("single")]);
    let resolver = AnyProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, _rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig::default(),
    );
    let mut budget = BudgetGuard::new(BudgetMode::Enforced, Some(1.0), None);
    let mut total = 0.0;

    pipeline
        .triage("fix the failing parser test", &mut budget, &mut total)
        .await
        .expect("a served triage call must classify");

    assert_eq!(
        provider.bounds(),
        vec![(Some(512), Some(stella_protocol::ReasoningEffort::Low))],
        "the triage dispatch must carry the pinned management bounds, not \
         fall through to the worker-tier allowance"
    );
}

/// A settings-supplied role override must keep winning over the pinned role
/// bounds — the bounds are a default between the caller's explicit overrides
/// and the engine base, never a cap on operator intent.
#[tokio::test]
async fn explicit_triage_overrides_win_over_the_role_bounds() {
    let provider = BoundsProvider::new(vec![text_result("single")]);
    let resolver = AnyProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, _rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &runner,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig {
            role_overrides: PipelineRoleOverrides {
                triage: RoleCallOverrides {
                    max_output_tokens: Some(9000),
                    effort: Some(stella_protocol::ReasoningEffort::High),
                    ..RoleCallOverrides::default()
                },
                ..PipelineRoleOverrides::default()
            },
            ..PipelineConfig::default()
        },
    );
    let mut budget = BudgetGuard::new(BudgetMode::Enforced, Some(1.0), None);
    let mut total = 0.0;

    pipeline
        .triage("fix the failing parser test", &mut budget, &mut total)
        .await
        .expect("a served triage call must classify");

    assert_eq!(
        provider.bounds(),
        vec![(Some(9000), Some(stella_protocol::ReasoningEffort::High))],
        "explicit role overrides must not be shadowed by the role bounds"
    );
}
