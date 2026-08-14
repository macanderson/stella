//! Terminal-outcome witnesses: a red final verdict reports
//! `VerificationFailed` rather than `Completed`, an enforced budget breach in
//! triage settles its spend and starts no further paid stage, an expired task
//! deadline stops the run before it dispatches anything at all, and an
//! unresolvable independent witness author degrades the run instead of
//! aborting it.

use super::*;

/// Every role resolves except the verifier — "no independent witness author is
/// available", stated by identity rather than by call count so the fixture
/// survives a change in the order roles are resolved.
struct NoVerifierProvider<'a> {
    provider: &'a ScriptedProvider,
}

impl ProviderResolver for NoVerifierProvider<'_> {
    fn provider_for(&self, model: &ModelRef) -> Option<&dyn Provider> {
        (model.model_id != "verifier").then_some(self.provider as &dyn Provider)
    }
}

impl ScriptedProvider {
    async fn remaining(&self) -> usize {
        self.script.lock().await.len()
    }
}

/// An identical no-progress tool call, for driving the engine's stuck-loop
/// ladder end-to-end through the pipeline (steer on the first detection,
/// abort on the second).
fn looping_tool_result() -> CompletionResult {
    CompletionResult {
        upstream_provider: None,
        text: String::new(),
        tool_calls: vec![ToolCall {
            call_id: "same-call".into(),
            name: "read_output".into(),
            input: serde_json::json!({"handle": "proc-5"}),
        }],
        usage: CompletionUsage::default(),
        model: "scripted".into(),
        cost_usd: 0.0001,
        finish_reason: None,
    }
}

/// #1524 witness: one loop abort is exactly ONE `error` event on the stream.
/// The driver publishes the abort when it stops the turn; the pipeline's
/// terminal path used to publish the identical record a second time when the
/// aborted candidate was adopted, so every nightly loop-abort trial carried
/// two byte-identical `error` rows. The status must also carry the typed
/// kind that says this stop was the engine's own policy, not a crash.
#[tokio::test]
async fn a_loop_abort_reaches_the_stream_as_exactly_one_error_event() {
    // Triage, then three identical no-progress calls (detect + steer) and a
    // fourth that escalates to the abort.
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        looping_tool_result(),
        looping_tool_result(),
        looping_tool_result(),
        looping_tool_result(),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false], "");
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
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);

    let outcome = pipeline
        .run(
            "keep reading the process output",
            &mut messages,
            &mut budget,
        )
        .await
        .expect("a loop abort is a clean pipeline outcome");

    let PipelineStatus::Aborted { reason, kind } = &outcome.status else {
        panic!(
            "the stuck loop must abort the run, got {:?}",
            outcome.status
        );
    };
    assert!(reason.contains("stuck-loop"), "{reason}");
    assert_eq!(
        *kind,
        AbortKind::DeliberateStop,
        "a loop escalation is the engine stopping on purpose, not a crash"
    );

    let events = drain(&mut rx);
    // Terminal errors only: `retryable: true` records are warnings (the deck
    // folds them), and this run legitimately warns once when its isolation-
    // free setup degrades to a bare worker turn.
    let errors: Vec<&String> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Error {
                message,
                retryable: false,
            } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(
        errors.len(),
        1,
        "one abort is one terminal error event on the pipeline path: {errors:?}"
    );
    assert!(errors[0].contains("stuck-loop"), "{}", errors[0]);
}

#[tokio::test]
async fn red_final_verdict_is_verification_failed_not_completed() {
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, false], "@@ -1 +1 @@\n-old\n+new");
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
            test_command: Some("cargo test -p x".into()),
            max_revisions: 0,
            ..PipelineConfig::default()
        },
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);

    let outcome = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("verification failure is a typed outcome");

    let verdict = outcome
        .verdict
        .clone()
        .expect("failed evidence is retained");
    assert!(!verdict.passed);
    assert_eq!(
        outcome.status,
        PipelineStatus::VerificationFailed { verdict }
    );
    assert!(
        (outcome.total_cost_usd - 0.0002).abs() < 1e-9,
        "triage and worker spend are retained"
    );
    let events = drain(&mut rx);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Complete { .. })),
        "a failed verification must never emit the success terminal event"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Error { message, retryable: false }
                if message.contains("verification failed")
                    && message.contains(&outcome.verdict.as_ref().unwrap().summary)
        )),
        "the terminal failure event must retain verdict evidence: {events:?}"
    );
}

#[tokio::test]
async fn enforced_budget_breach_in_triage_stops_before_the_next_paid_stage() {
    let provider = ScriptedProvider::new(vec![
        text_result("multi"),
        text_result(r#"["plan must never run"]"#),
        text_result("worker must never run"),
    ]);
    let resolver = OneProvider(&provider);
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
    let mut budget = BudgetGuard::new(BudgetMode::Enforced, Some(0.00005), None);

    let outcome = pipeline
        .run(
            "Refactor the parser and update all callers",
            &mut messages,
            &mut budget,
        )
        .await
        .expect("budget breach is a typed outcome");

    assert!(matches!(outcome.status, PipelineStatus::Aborted { .. }));
    assert!(
        (outcome.total_cost_usd - 0.0001).abs() < 1e-9,
        "the over-cap triage call is settled spend"
    );
    assert_eq!(
        provider.remaining().await,
        2,
        "the next paid stage must not start after triage crosses the cap"
    );
}

/// #2238 witness: an already-expired task deadline stops the run before it
/// dispatches a single paid stage call.
///
/// Fails on `main`, where `BudgetGuard::set_task_deadline` was honoured only
/// by the engine's step loop (`driver::settlement::check_budget`): every raw
/// stage went through `Pipeline::dispatch_raw`, whose one budget rung matched
/// `BudgetOutcome::AbortTurn` — a dollar rung — so triage dispatched here
/// regardless of the clock, and research, plan, scope, witness, verify and
/// verdict followed it against a harness already about to SIGKILL the
/// container.
///
/// The dollar axes are deliberately unarmed (`BudgetMode::Off`, no limits):
/// the only ceiling in this fixture is the wall clock, so nothing but the
/// clock can be what stopped it.
#[tokio::test]
async fn an_expired_task_deadline_stops_the_run_before_any_paid_stage() {
    let provider = ScriptedProvider::new(vec![
        text_result("multi"),
        text_result(r#"["plan must never run"]"#),
        text_result("worker must never run"),
    ]);
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
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    budget.set_task_deadline(Some(
        std::time::Instant::now() - std::time::Duration::from_secs(7),
    ));

    let outcome = pipeline
        .run(
            "Refactor the parser and update all callers",
            &mut messages,
            &mut budget,
        )
        .await
        .expect("an expired deadline is a typed outcome, not a run error");

    assert!(
        matches!(
            outcome.status,
            PipelineStatus::Aborted {
                kind: AbortKind::DeliberateStop,
                ..
            }
        ),
        "stopping on the clock is a deliberate stop, not a failure: {:?}",
        outcome.status
    );
    assert_eq!(
        outcome.total_cost_usd, 0.0,
        "nothing was dispatched, so nothing was spent"
    );
    assert_eq!(
        provider.remaining().await,
        3,
        "not one scripted completion may be consumed after the deadline has passed"
    );
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Error { message, .. } if message.contains("task deadline exceeded by")
        )),
        "the stream must name the clock as the reason, not leave it to argv: {events:?}"
    );
}

/// An unresolvable verifier costs the run its authored witness, not the task.
/// The pipeline warns once and falls through to the unauthored verify ladder
/// rather than aborting with no work done.
#[tokio::test]
async fn unavailable_independent_witness_degrades_instead_of_aborting() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("TEST_COMMAND: cargo test --test witness witness -- --exact"),
    ]);
    let resolver = NoVerifierProvider {
        provider: &provider,
    };
    let runner = ScriptedRunner::new(vec![false], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let workspace = FakeWorkspace::new(0, vec![false], Ok(vec![]), log.clone()).with_repo_status(
        SeqRepoStatus::new(vec![vec![], vec![("tests/witness.rs", "sha256:test")]]),
    );
    let _candidate_workspaces = FakeWorkspacePort::new(vec![Ok(workspace)], log);
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
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);

    let outcome = pipeline
        .run("Fix the parser", &mut messages, &mut budget)
        .await
        .expect("an unresolvable witness author is a degradation, not a failure");

    assert!(
        !matches!(
            outcome.status,
            PipelineStatus::Aborted { ref reason, .. }
                if reason.contains("independent witness author")
        ),
        "losing the author must not abort the task: {outcome:?}"
    );
    let events = drain(&mut rx);
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Error { message, retryable: true }
                if message.contains("no model independent of the worker")
        )),
        "the degradation is announced once: {events:?}"
    );
    assert!(
        !stages(&events).contains(&StageKind::Witness),
        "witness authoring is skipped, never attempted without an author"
    );
}

/// A provider whose FIRST completion burns real wall clock and comes back
/// starved (#2128's signature: stopped at the token limit with nothing to
/// show), so the management chokepoint retries the SAME role. That second
/// dispatch is the only shape that can exercise an anticipatory rung built on
/// measurement — the first call of any role has no history and is never
/// refused, by design.
struct SlowThenStarved {
    first: std::time::Duration,
    calls: std::sync::atomic::AtomicUsize,
}

impl SlowThenStarved {
    fn new(first: std::time::Duration) -> Self {
        Self {
            first,
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl Provider for SlowThenStarved {
    fn id(&self) -> &str {
        "slow-then-starved"
    }
    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            tokio::time::sleep(self.first).await;
            return Ok(CompletionResult {
                finish_reason: Some(stella_protocol::FinishReason::Length),
                ..text_result("")
            });
        }
        Ok(text_result("multi"))
    }
}

/// Hands [`SlowThenStarved`] to every role — the single-model wiring
/// `OneProvider` gives the scripted provider, which is typed to it.
struct SlowResolver<'p>(&'p SlowThenStarved);
impl ProviderResolver for SlowResolver<'_> {
    fn provider_for(&self, _model: &ModelRef) -> Option<&dyn Provider> {
        Some(self.0)
    }
}

/// #2432 witness: a task deadline that has NOT passed, but is too near to fit
/// a call the pipeline has already measured, stops that call before dispatch.
///
/// The first triage call takes 300ms and comes back starved, so the chokepoint
/// retries the same role — and by then the pipeline knows what a triage call
/// costs. With 400ms of deadline armed, 100ms remains against a 300ms forecast,
/// so the retry is refused before it reaches the provider.
///
/// Fails on `main`, where the rung consults `check_deadline` —
/// `check_deadline_with_reserve(now, ZERO)` — so `DeadlineOutcome::Closing` is
/// unreachable and the retry is dispatched anyway, to overrun the task clock by
/// the difference. That is #2278's shape one layer up: the seam refuses only
/// work that is already too late, never work it can see it cannot finish.
///
/// Dollar axes deliberately unarmed, as in the #2238 witness above: the only
/// ceiling in this fixture is the wall clock.
#[tokio::test]
async fn a_deadline_too_near_to_fit_a_measured_call_stops_it_before_dispatch() {
    let provider = SlowThenStarved::new(std::time::Duration::from_millis(300));
    let resolver = SlowResolver(&provider);
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
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    // Still ahead — this run has time left, just not enough of it for the call
    // it is about to start.
    budget.set_task_deadline(Some(
        std::time::Instant::now() + std::time::Duration::from_millis(400),
    ));

    let outcome = pipeline
        .run(
            "Refactor the parser and update all callers",
            &mut messages,
            &mut budget,
        )
        .await
        .expect("declining on the clock is a typed outcome, not a run error");

    assert!(
        matches!(
            outcome.status,
            PipelineStatus::Aborted {
                kind: AbortKind::DeliberateStop,
                ..
            }
        ),
        "declining work the clock cannot fit is a deliberate stop: {:?}",
        outcome.status
    );
    assert_eq!(
        provider.calls(),
        1,
        "the anticipatory rung refuses BEFORE dispatch: the retry must never \
         reach the provider"
    );
    let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let anticipatory = events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::Error { message, .. }
                if message.contains("declining to start work") && !message.contains("exceeded")
        )
    });
    assert!(
        anticipatory,
        "the stop must read as anticipation, never as an overrun that never \
         happened — the distinction `driver::settlement` draws for the same \
         rung one layer down: {events:?}"
    );
}
