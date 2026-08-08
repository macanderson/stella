//! The witness repair wall-clock bound (#2141) — the end-to-end witness and
//! the stalling provider only it scripts.
//!
//! The defect this pins: the witness stage's repair rung issued its engine
//! turn with no bound of its own, and a reasoning-heavy repair model that
//! never stops streaming is invisible to every idle-based timeout — observed
//! as one 965-second, 36k-token `witness_repair` call that rode a
//! Terminal-Bench trial into the harness kill, discarding the whole
//! verification record of work the fast worker had already finished. The fix
//! is a stage latency ceiling in the shape the triage and research stages
//! already use: derive a bound from the run's remaining declared clock, drop
//! the in-flight turn past it, and degrade honestly to
//! `WitnessUnavailable` so verify/verdict proceed with what they have.

use super::*;

/// The one sentence only the repair prompt carries
/// ([`crate::witness::witness_repair_prompt`]) — how this provider
/// recognizes the repair call without depending on call order, so the test
/// stays correct whether the bound trips in flight or refuses pre-dispatch.
const REPAIR_PROMPT_MARKER: &str = "witness test PASSED on the current, unmodified code";

/// A provider that answers every call from its ordered script EXCEPT the
/// witness repair call, which never resolves — #2141's runaway reasoning
/// stream, which keeps producing and therefore never goes idle. Only a
/// wall-clock bound can end it.
struct StallsOnRepair {
    script: TokioMutex<VecDeque<CompletionResult>>,
}

impl StallsOnRepair {
    fn new(script: Vec<CompletionResult>) -> Self {
        Self {
            script: TokioMutex::new(script.into_iter().collect()),
        }
    }
}

#[async_trait]
impl Provider for StallsOnRepair {
    fn id(&self) -> &str {
        "stalls-on-repair"
    }
    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        if req
            .messages
            .iter()
            .any(|message| message.content.contains(REPAIR_PROMPT_MARKER))
        {
            std::future::pending::<()>().await;
        }
        let next = self.script.lock().await.pop_front();
        next.ok_or_else(|| ProviderError::Terminal("scripted provider exhausted".into()))
    }
}

/// A resolver handing the stalling provider to every role — the same
/// single-model wiring `OneProvider` gives the scripted one.
struct StallingResolver<'p>(&'p StallsOnRepair);
impl ProviderResolver for StallingResolver<'_> {
    fn provider_for(&self, _model: &ModelRef) -> Option<&dyn Provider> {
        Some(self.0)
    }
}

/// #2141's witness: a witness repair whose model streams forever cannot
/// spend the run's clock — the turn is dropped at its derived bound and the
/// stage degrades honestly, on the rail, while the run still reaches a
/// verdict. On `main` the repair turn is unbounded, nothing above it ends a
/// stream that never goes idle, and this test times out.
#[tokio::test]
async fn a_stalled_witness_repair_is_bounded_and_degrades_honestly() {
    let provider = StallsOnRepair::new(vec![
        // Triage, then the worker's execute turn.
        text_result("single"),
        text_result("done"),
        // The witness author names a command that will PASS on the pristine
        // baseline — the one precondition that reaches the repair rung.
        text_result("TEST_COMMAND: cargo test --test witness always_green -- --exact"),
        // The unauthored ladder's verdict, reachable only past the bound.
        text_result("PASS looks right"),
    ]);
    let resolver = StallingResolver(&provider);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone());
    // The authored command passes on the unmodified baseline, so the stage
    // asks for the one bounded repair — and the repair model stalls.
    let baseline = FakeWorkspace::new(1, vec![true], Ok(vec![]), log.clone());
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log);
    let session_runner = NeverRunner;
    let session_status = NeverRepoStatus;
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
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
            repo_status: &session_status,
            touches: &NoFileTouches,
            diagnostics: &session_runner,
            tests: &session_runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: Some(&port),
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig {
            // A short declared allowance derives a sub-second repair bound
            // (`witness_repair_bound` grants half of what remains), so the
            // trip is observable in test time without faking clocks. Should
            // a slow machine exhaust it before the repair dispatches, the
            // pre-dispatch refusal degrades with the same honesty — the
            // assertions below hold on either path.
            run_budget: Some(Duration::from_millis(400)),
            ..PipelineConfig::default()
        },
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    // The outer timeout is the DETECTOR, not the bound under test: with the
    // repair unbounded, the stalled stream holds `run` open until an
    // external kill — the exact #2141 failure — and this is what fails
    // instead of hanging the suite.
    let outcome = tokio::time::timeout(
        Duration::from_secs(30),
        pipeline.run("Fix the retry bug", &mut messages, &mut budget),
    )
    .await
    .expect(
        "the witness repair turn must be bounded — unbounded, a stalled \
         repair stream spends the whole trial (#2141)",
    )
    .expect("a bounded repair is a degradation, never a run-ending error");

    assert!(
        !matches!(outcome.status, PipelineStatus::Aborted { .. }),
        "the pipeline proceeds to verify/verdict with what it has: {outcome:?}"
    );
    let events = drain(&mut rx);
    let on_the_rail = events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::Proof {
                step: ProofStep::WitnessUnavailable { reason }
            } if reason.contains("witness repair") && reason.contains("wall-clock")
        )
    });
    assert!(
        on_the_rail,
        "the breach is recorded on the rail as an honest degradation, \
         never ridden into an external timeout: {events:?}"
    );
}
