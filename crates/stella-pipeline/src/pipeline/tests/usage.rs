//! Paid-call accounting witnesses for every raw pipeline role.

use super::*;
use stella_protocol::ToolCallObserver;

fn usage_events(events: &[AgentEvent]) -> Vec<serde_json::Value> {
    events
        .iter()
        .filter_map(|event| serde_json::to_value(event).ok())
        .filter(|event| {
            matches!(
                event.get("type").and_then(serde_json::Value::as_str),
                Some("step_usage" | "usage_incomplete")
            )
        })
        .collect()
}

pub(super) fn usage_roles(events: &[AgentEvent]) -> Vec<String> {
    usage_events(events)
        .into_iter()
        .filter_map(|event| event.get("role")?.as_str().map(str::to_owned))
        .collect()
}

struct AnyProvider<'a>(&'a dyn Provider);

impl ProviderResolver for AnyProvider<'_> {
    fn provider_for(&self, _model: &ModelRef) -> Option<&dyn Provider> {
        Some(self.0)
    }
}

struct ErrorProvider;

#[async_trait]
impl Provider for ErrorProvider {
    fn id(&self) -> &str {
        "paid-error"
    }

    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        Err(ProviderError::Terminal("upstream failed".into()))
    }
}

struct SlowProvider;

#[async_trait]
impl Provider for SlowProvider {
    fn id(&self) -> &str {
        "paid-timeout"
    }

    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        tokio::time::sleep(Duration::from_secs(60)).await;
        Ok(text_result("lookup"))
    }
}

/// A resolver that routes nothing, so every responsibility comes back
/// [`Assigned::Unresolvable`] — a missing credential or an unknown model slug,
/// as seen from the call site.
struct NoProviderResolver;

impl ProviderResolver for NoProviderResolver {
    fn provider_for(&self, _model: &ModelRef) -> Option<&dyn Provider> {
        None
    }
}

async fn run_triage_only(
    provider: &dyn Provider,
    config: PipelineConfig,
    budget: &mut BudgetGuard,
) -> (
    Result<TaskAssessment, PipelineStageAbort>,
    f64,
    Vec<AgentEvent>,
) {
    run_triage_with_resolver(&AnyProvider(provider), config, budget).await
}

async fn run_triage_with_resolver(
    resolver: &dyn ProviderResolver,
    config: PipelineConfig,
    budget: &mut BudgetGuard,
) -> (
    Result<TaskAssessment, PipelineStageAbort>,
    f64,
    Vec<AgentEvent>,
) {
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let runner = ScriptedRunner::new(vec![], "");
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: resolver,
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
        config,
    );
    let mut total = 0.0;
    let result = pipeline
        .triage("inspect the repository", budget, &mut total)
        .await;
    (
        result.map(|(assessment, _research)| assessment),
        total,
        drain(&mut rx),
    )
}

#[tokio::test]
async fn triage_success_emits_usage_before_budget_abort() {
    let provider = ScriptedProvider::new(vec![text_result("lookup")]);
    let mut budget = BudgetGuard::new(BudgetMode::Enforced, Some(0.00001), None);
    let (result, total, events) =
        run_triage_only(&provider, PipelineConfig::default(), &mut budget).await;

    assert!(result.is_err(), "the settled call crosses the tiny budget");
    assert_eq!(total, 0.0001);
    let serialized: Vec<_> = events
        .iter()
        .filter_map(|event| serde_json::to_value(event).ok())
        .collect();
    let usage = serialized
        .iter()
        .position(|event| event["type"] == "step_usage")
        .expect("the paid call must emit usage");
    let tick = serialized
        .iter()
        .position(|event| event["type"] == "budget_tick")
        .expect("the paid call must settle the budget");
    assert!(
        usage < tick,
        "usage must be durable before an abort can return"
    );
    assert_eq!(serialized[usage]["role"], "triage");
    assert_eq!(serialized[usage]["provider"], "scripted");
    assert_eq!(serialized[usage]["model"], "scripted");
    // The provider call itself succeeded with a real, trustworthy usage
    // envelope (`text_result`'s `reported: true`) — `complete` tracks that,
    // not the turn's outcome. The *subsequent* budget check aborting the
    // turn is a separate concern from whether this call's own accounting
    // record can be trusted; it can.
    assert_eq!(serialized[usage]["complete"], true);
}

#[tokio::test]
async fn triage_provider_error_emits_content_free_incompleteness() {
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (result, _, events) =
        run_triage_only(&ErrorProvider, PipelineConfig::default(), &mut budget).await;
    // `SingleTask`, not `SimpleLookup`: a triage that produced no
    // classification cannot route the turn onto the path that skips the
    // planner and the verifier (`triage_outage_floor`). The subject of this
    // test is the recorded reason, not the class.
    assert_eq!(result.unwrap().class, TaskClass::SingleTask);
    let usage = usage_events(&events);
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0]["type"], "usage_incomplete");
    assert_eq!(usage[0]["role"], "triage");
    assert_eq!(usage[0]["reason"], "provider_error");
    assert!(
        usage[0].get("message").is_none(),
        "no provider content leaks"
    );
}

#[tokio::test]
async fn triage_timeout_emits_content_free_incompleteness() {
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let config = PipelineConfig {
        triage_latency_ceiling: Duration::from_millis(1),
        ..PipelineConfig::default()
    };
    let (result, _, events) = run_triage_only(&SlowProvider, config, &mut budget).await;
    // `SingleTask`, not `SimpleLookup`: a triage that produced no
    // classification cannot route the turn onto the path that skips the
    // planner and the verifier (`triage_outage_floor`). The subject of this
    // test is the recorded reason, not the class.
    assert_eq!(result.unwrap().class, TaskClass::SingleTask);
    let usage = usage_events(&events);
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0]["type"], "usage_incomplete");
    assert_eq!(usage[0]["reason"], "timeout");
}

/// Streams a fragment immediately, then goes quiet for longer than the
/// ceiling before resolving — OpenRouter's `usage.include` gateway
/// accounting shape (#1467): the answer streams promptly, but the routed
/// call's usage/cost is reported in one final SSE frame that trails it,
/// once the gateway has settled the price.
struct DelayedUsageFrameProvider {
    trailing_delay: Duration,
}

#[async_trait]
impl Provider for DelayedUsageFrameProvider {
    fn id(&self) -> &str {
        "openrouter"
    }

    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        panic!("this test dispatches through complete_observed_ref, not complete_ref");
    }

    async fn complete_observed_ref(
        &self,
        _req: CompletionRequestRef<'_>,
        observer: &dyn ToolCallObserver,
    ) -> Result<CompletionResult, ProviderError> {
        observer.text_delta("lookup");
        tokio::time::sleep(self.trailing_delay).await;
        Ok(text_result("lookup"))
    }
}

/// Witness for #1467: a triage call that streamed real content (one
/// fragment, at dispatch) but whose result — carrying OpenRouter's
/// usage/cost accounting — only resolves 80ms later, longer than the 50ms
/// `triage_latency_ceiling` below. A ceiling measured as flat wall clock
/// from dispatch start abandons the call at 50ms, before the 80ms result
/// lands, and its usage/cost go unaccounted (`agent.step.usage_incomplete`,
/// `reason=timeout`) even though the call was actively answering the whole
/// time. Measured as idle time instead — since the last streamed fragment —
/// the tick at dispatch re-arms the window and the 80ms result lands inside
/// it, so the call is billed like any other successful one.
#[tokio::test]
async fn triage_survives_a_usage_frame_that_trails_the_content() {
    let provider = DelayedUsageFrameProvider {
        trailing_delay: Duration::from_millis(80),
    };
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let config = PipelineConfig {
        triage_latency_ceiling: Duration::from_millis(50),
        ..PipelineConfig::default()
    };
    let (result, total, events) = run_triage_only(&provider, config, &mut budget).await;

    assert!(
        result.is_ok(),
        "the trailing gap must not abandon a call that was actively answering: {result:?}"
    );
    assert_eq!(
        total, 0.0001,
        "the delayed usage frame's cost must still be billed"
    );
    let usage = usage_events(&events);
    assert_eq!(usage.len(), 1, "{usage:?}");
    assert_eq!(usage[0]["type"], "step_usage");
    assert_eq!(usage[0]["role"], "triage");
    assert_eq!(usage[0]["complete"], true);
}

#[tokio::test]
async fn plan_and_plan_repair_each_emit_one_paid_call_envelope() {
    let provider = ScriptedProvider::new(vec![
        text_result("multi"),
        text_result("not-json"),
        text_result(r#"["s1","s2","s3","s4","s5","s6"]"#),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = FixedGate(ScopeDecision::Approve);
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
            headless: true,
            headless_bypass_scope_review: false,
            ..PipelineConfig::default()
        },
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let error = pipeline
        .run(
            "Refactor across the codebase and update every caller",
            &mut messages,
            &mut budget,
        )
        .await
        .expect_err("large headless plan stops after planning");
    assert!((error.total_cost_usd - 0.0003).abs() < f64::EPSILON * 4.0);
    assert_eq!(
        usage_roles(&drain(&mut rx)),
        ["triage", "plan", "plan_repair"]
    );
}

#[tokio::test]
async fn witness_author_and_repair_are_individually_metered_before_degrade() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"), // worker executes before the author is asked
        text_result("TEST_COMMAND: cargo test --test witness always_green -- --exact"),
        text_result("TEST_COMMAND: cargo test --test witness still_green -- --exact"),
        // The useless witness leaves the candidate on the unauthored ladder.
        text_result("PASS looks right"),
    ]);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let candidate = FakeWorkspace::new(0, vec![true], Ok(vec![]), log.clone());
    let baseline =
        FakeWorkspace::new(1, vec![true, true], Ok(vec![]), log.clone()).with_repo_status(
            SeqRepoStatus::new(vec![vec![], vec![("tests/witness.rs", "w1")]]),
        );
    let port = FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log);
    let (outcome, events, _) = run_isolated(
        &provider,
        &port,
        PipelineConfig::default(),
        "Fix the retry bug",
    )
    .await;
    // The witness author and its repair are each metered individually, even
    // though the useless witness then degrades rather than aborts. The worker
    // now sits between triage and the author, because authoring is demand-
    // driven: it is only bought once there is a diff worth proving.
    let roles = usage_roles(&events);
    assert_eq!(
        &roles[..4],
        ["triage", "worker", "witness_author", "witness_repair"],
        "author and repair are individually metered: {roles:?}"
    );
    assert!(
        !matches!(outcome.unwrap().status, PipelineStatus::Aborted { .. }),
        "a useless witness degrades rather than aborting"
    );
}

/// Every [`ProofStep::TriageDegraded`] reason in a stream, in order.
fn triage_degraded_reasons(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Proof {
                step: stella_protocol::ProofStep::TriageDegraded { reason },
            } => Some(reason.clone()),
            _ => None,
        })
        .collect()
}

/// #2414's witness. A triage call that burns its whole ceiling and returns
/// nothing is *correctly* handled — the class falls to the deterministic
/// keyword floor and the run proceeds — and was completely invisible: 27 of
/// 34 triage calls across three Terminal-Bench arm runs took this path,
/// about four and a half minutes of wall clock purchasing zero bits, with
/// nothing in the summary layer saying so. A bench conclusion must not be
/// drawable from a triage that never ran.
///
/// The record names the ceiling, because "triage timed out" and "triage timed
/// out at 30s" are different facts to whoever is deciding whether the ceiling
/// is the problem.
#[tokio::test]
async fn a_timed_out_triage_records_that_the_class_came_from_the_floor() {
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let config = PipelineConfig {
        triage_latency_ceiling: Duration::from_millis(1),
        ..PipelineConfig::default()
    };
    let (result, _, events) = run_triage_only(&SlowProvider, config, &mut budget).await;

    // The fallback itself is unchanged and load-bearing: never fail a run on
    // triage.
    // `SingleTask`, not `SimpleLookup`: a triage that produced no
    // classification cannot route the turn onto the path that skips the
    // planner and the verifier (`triage_outage_floor`). The subject of this
    // test is the recorded reason, not the class.
    assert_eq!(result.unwrap().class, TaskClass::SingleTask);

    let reasons = triage_degraded_reasons(&events);
    assert_eq!(reasons.len(), 1, "exactly one record per degraded triage");
    assert!(
        reasons[0].contains("timed out") && reasons[0].contains("1ms"),
        "the record must name the ceiling it hit: {reasons:?}"
    );
    // Both channels, like `unproven`/`unverifiable`: the prose account a
    // human reads and the structured record a census counts.
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Error { message, .. } if message.contains("deterministic keyword floor")
        )),
        "the degradation is also stated in prose: {events:?}"
    );
}

/// A provider error is the same outcome — no class from a model — but a
/// different fact about the run, and it costs no dead air. Naming them apart
/// is what makes a census of these records mean anything.
#[tokio::test]
async fn a_failed_triage_call_records_a_provider_failure_not_a_timeout() {
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (result, _, events) =
        run_triage_only(&ErrorProvider, PipelineConfig::default(), &mut budget).await;
    // `SingleTask`, not `SimpleLookup`: a triage that produced no
    // classification cannot route the turn onto the path that skips the
    // planner and the verifier (`triage_outage_floor`). The subject of this
    // test is the recorded reason, not the class.
    assert_eq!(result.unwrap().class, TaskClass::SingleTask);
    let reasons = triage_degraded_reasons(&events);
    assert_eq!(reasons.len(), 1);
    assert!(
        reasons[0].contains("failed at the provider") && !reasons[0].contains("timed out"),
        "{reasons:?}"
    );
}

/// A response that arrived and said nothing the protocol recognizes lands on
/// the same floor, so it is the same record — and exactly one, not one for
/// the call plus one for the parse.
#[tokio::test]
async fn an_off_protocol_triage_response_records_the_same_degradation() {
    let provider = ScriptedProvider::new(vec![text_result("Sure — happy to help.")]);
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (result, _, events) =
        run_triage_only(&provider, PipelineConfig::default(), &mut budget).await;
    // `SingleTask`, not `SimpleLookup`: a triage that produced no
    // classification cannot route the turn onto the path that skips the
    // planner and the verifier (`triage_outage_floor`). The subject of this
    // test is the recorded reason, not the class.
    assert_eq!(result.unwrap().class, TaskClass::SingleTask);
    let reasons = triage_degraded_reasons(&events);
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(reasons[0].contains("did not follow the classification protocol"));
}

/// The silence that matters: a triage that answered on protocol records
/// nothing at all. A degradation marker on a healthy turn would make the
/// census useless in the other direction.
#[tokio::test]
async fn a_triage_that_answers_records_no_degradation() {
    let provider = ScriptedProvider::new(vec![text_result("lookup")]);
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (_result, _, events) =
        run_triage_only(&provider, PipelineConfig::default(), &mut budget).await;
    assert!(triage_degraded_reasons(&events).is_empty(), "{events:?}");
}

/// The fourth degradation path, and the one with no paid call behind it:
/// triage resolved to no provider at all (a missing credential, an unknown
/// model slug). The class comes from the deterministic floor exactly as it
/// does for a timeout, so it is the same kind of record — and it was silent.
///
/// **Witness.** #2430 added this emit site; #2462's rewrite of the branch to
/// introduce `Assigned` carried a version without it over the top, so on
/// `main` an unroutable triage falls to the floor recording nothing. A census
/// of `triage_degraded` (#2414, #2429) would attribute those turns to a triage
/// that ran and answered, which is the one reading the record exists to
/// prevent.
#[tokio::test]
async fn an_unroutable_triage_records_that_it_could_not_be_routed() {
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (result, total, events) =
        run_triage_with_resolver(&NoProviderResolver, PipelineConfig::default(), &mut budget).await;

    // The fallback is unchanged and load-bearing: never fail a run on triage.
    // `SingleTask`, not `SimpleLookup`: a triage that produced no
    // classification cannot route the turn onto the path that skips the
    // planner and the verifier (`triage_outage_floor`). The subject of this
    // test is the recorded reason, not the class.
    assert_eq!(result.unwrap().class, TaskClass::SingleTask);
    assert_eq!(total, 0.0, "an unroutable triage buys nothing");

    let reasons = triage_degraded_reasons(&events);
    assert_eq!(reasons.len(), 1, "exactly one record, got {reasons:?}");
    assert!(
        reasons[0].contains("could not be routed"),
        "the record must name routing, not a timeout it never waited for: {reasons:?}"
    );
}

/// **Witness for the ceiling itself.** #2414 moved this from 10s to 30s
/// because the old bound sat *inside* the answering distribution: 27 of 34
/// calls burned the full 10,000ms and returned nothing, while the 7 that
/// answered took 4,684-8,587ms — so the bound was converting slow-but-correct
/// answers into no answer at all, and paying the full ceiling to do it.
///
/// It is asserted here because it was silently reverted once already. #2462
/// rewrote `PipelineConfig::default` from a branch that predated #2430 and
/// carried the stale `10` over the top, leaving the field's own doc comment —
/// which explains the move in full, and names the 7-sample caveat — describing
/// a value the struct no longer had. A merge that does that again fails this
/// test instead of quietly halving the ceiling.
///
/// This asserts the number, not its correctness: 30s is sized from 7 answering
/// samples and #2429 is open to re-measure it. Moving it deliberately means
/// editing this line and saying which side of the distribution the new bound
/// buys.
#[test]
fn the_default_triage_ceiling_is_the_one_the_doc_comment_describes() {
    assert_eq!(
        PipelineConfig::default().triage_latency_ceiling,
        Duration::from_secs(30),
        "triage's ceiling regressed away from the measured value (#2414, #2429)"
    );
}

/// A triage outage must not route the turn onto the path that skips the
/// planner and the verifier.
///
/// The measured defect. On the 2026-08-10 TB2.1 panel, four of eight pipeline
/// trials met a rate-limited triage call, fell to the keyword floor, and ran
/// with NO management roles at all — `SimpleLookup` skips both the planner and
/// the verifier, so a provider hiccup silently turned the staged pipeline into
/// a bare loop that still paid the pipeline's overhead. The trials still
/// scored; they simply were not measuring the pipeline, and nothing in the
/// solve count could show it.
///
/// The goal here carries no keyword the floor recognises, which is the whole
/// hazard: the floor's evidence is absent, not cheap, and absence of a
/// classification is not evidence that a task is simple. `resolve_task_class`
/// errs toward planning everywhere else; the outage path is where it did not.
#[tokio::test]
async fn a_triage_outage_never_routes_the_turn_below_single_task() {
    let off_protocol = ScriptedProvider::new(vec![text_result("Sure — happy to help.")]);
    let cases: [(&str, &dyn stella_protocol::Provider); 2] = [
        ("a provider error", &ErrorProvider),
        ("an off-protocol answer", &off_protocol),
    ];
    for (label, provider) in cases {
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        let (result, _, _) =
            run_triage_only(provider, PipelineConfig::default(), &mut budget).await;
        let class = result.expect("triage never fails the run").class;

        assert!(
            class >= TaskClass::SingleTask,
            "{label}: an outage routed the turn to {class:?}, which skips the \
             verifier — the pipeline would run as a bare loop"
        );
    }
}
