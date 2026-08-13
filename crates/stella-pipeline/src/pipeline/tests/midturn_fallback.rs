// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Mid-turn provider fallback on the pipeline's execute stage (#2765) — the
//! pipeline half of the seam `stella-core`'s
//! `driver::tests::model_fallback` proves at the engine level (#2679).
//!
//! `stella-core` proves the engine swaps when a [`FallbackResolver`] hands it
//! a replacement. That says nothing about whether the pipeline — the default
//! `stella run` path — ever attaches one, and until #2765 it did not: a
//! retries-exhausted worker call aborted the run with every completed step's
//! work stranded, exactly as it did before #2679 shipped.
//!
//! These tests drive a REAL pipeline run through the full chain the fix
//! depends on, rather than calling the resolver directly:
//!
//! 1. the execute engine's failing calls feed the router's breaker through
//!    `with_provider_outcomes` (#2673),
//! 2. so the breaker opens on the sick provider,
//! 3. so the resolver's re-resolution of the engine's own role routes past it,
//! 4. so the resolver maps the new `ModelRef` through the run's own
//!    [`ProviderResolver`] to an adapter the run already owns,
//! 5. so the engine latches the swap and the turn finishes on the replacement.
//!
//! Every link is a place the wiring could be wrong, and a unit test of the
//! resolver alone would prove none of them.
//!
//! [`FallbackResolver`]: stella_core::ports::FallbackResolver

use super::*;

/// The tool set the execute engine advertises. One schema, never called —
/// its whole job is to make an engine turn distinguishable from a management
/// raw call on the wire, which is how [`FailsEngineTurns`] decides what to
/// fail without counting calls (a count would silently stop witnessing the
/// moment a stage added a model call).
struct OneSchemaTools;

#[async_trait]
impl ToolExecutor for OneSchemaTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "noop".into(),
            description: "does nothing".into(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
            read_only: true,
            speculation_safe: false,
        }]
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: String::new(),
        }
    }
}

/// Serves management calls (no tool schemas on the request) from a script and
/// fails every engine turn terminally — the sick worker this run has to
/// route around, without being sick at the stages that decide the run ever
/// reaches an execute turn.
struct FailsEngineTurns {
    id: &'static str,
    management: TokioMutex<VecDeque<CompletionResult>>,
    engine_calls: std::sync::atomic::AtomicU32,
}

impl FailsEngineTurns {
    fn new(id: &'static str, management: Vec<CompletionResult>) -> Self {
        Self {
            id,
            management: TokioMutex::new(management.into_iter().collect()),
            engine_calls: std::sync::atomic::AtomicU32::new(0),
        }
    }

    fn engine_calls(&self) -> u32 {
        self.engine_calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl Provider for FailsEngineTurns {
    fn id(&self) -> &str {
        self.id
    }
    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        if !req.tools.is_empty() {
            self.engine_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return Err(ProviderError::Terminal("worker provider is down".into()));
        }
        let mut queue = self.management.lock().await;
        queue
            .pop_front()
            .ok_or_else(|| ProviderError::Terminal("management script exhausted".into()))
    }
}

/// Completes every call, recording how many engine turns it served — the
/// replacement the swap must actually reach.
struct AlwaysHealthy {
    id: &'static str,
    engine_calls: std::sync::atomic::AtomicU32,
    goals: std::sync::Mutex<Vec<String>>,
}

impl AlwaysHealthy {
    fn new(id: &'static str) -> Self {
        Self {
            id,
            engine_calls: std::sync::atomic::AtomicU32::new(0),
            goals: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn engine_calls(&self) -> u32 {
        self.engine_calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The joined transcript of each engine turn this provider served.
    fn engine_transcripts(&self) -> Vec<String> {
        self.goals.lock().unwrap().clone()
    }
}

#[async_trait]
impl Provider for AlwaysHealthy {
    fn id(&self) -> &str {
        self.id
    }
    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        if !req.tools.is_empty() {
            self.engine_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.goals.lock().unwrap().push(
                req.messages
                    .iter()
                    .map(|m| m.content.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        Ok(text_result("rescued — the change is made"))
    }
}

/// Maps each provider id to its adapter, exactly as `stella-cli`'s
/// `RoleProviderResolver` maps the run's owned adapter set.
struct ByProviderId<'p>(Vec<(&'static str, &'p dyn Provider)>);

impl ProviderResolver for ByProviderId<'_> {
    fn provider_for(&self, model: &ModelRef) -> Option<&dyn Provider> {
        self.0
            .iter()
            .find(|(id, _)| *id == model.provider)
            .map(|(_, provider)| *provider)
    }
}

/// A profile per provider id, its three tiers all one model. Order is
/// preference order, so `sick` is what an unpinned role resolves to until its
/// breaker opens.
fn profiles(ids: &[&str]) -> Vec<ProviderProfile> {
    ids.iter()
        .map(|id| {
            let model = ModelRef::new(*id, "only");
            ProviderProfile::new(*id, model.clone(), model.clone(), model)
        })
        .collect()
}

/// A router over `ids` whose breaker opens after a SINGLE observed failure,
/// so one exhausted ladder is decisive. The engine's own call-outcome
/// feedback is what feeds it (`attach`, #2673); nothing here reports on the
/// engine's behalf.
fn router_over(ids: &[&str]) -> Router {
    Router::new(
        RoleTable::new(),
        profiles(ids),
        CircuitBreaker::with_thresholds(Box::new(ZeroClock), 1, 60_000),
    )
}

/// Enough management answers for one `single`-class run: the triage
/// classification first, then spares so a plan (or any later management
/// call) never exhausts the script and turns this into a test about
/// something else.
fn management_script() -> Vec<CompletionResult> {
    let mut script = vec![text_result("single")];
    script.extend(std::iter::repeat_with(|| text_result("proceed")).take(8));
    script
}

fn config() -> PipelineConfig {
    PipelineConfig {
        // No authored witness: this run is about the execute engine's
        // provider, and the witness stage would resolve a second role.
        roster: Roster::default().with_enabled(ModelCallRole::WitnessAuthor, false),
        candidates: Some(1),
        headless: true,
        headless_bypass_scope_review: true,
        ..PipelineConfig::default()
    }
}

/// Run one pipeline over the given router and adapter set, returning the
/// outcome and the event stream.
async fn run(
    router: &Router,
    providers: &dyn ProviderResolver,
) -> (Result<PipelineOutcome, PipelineRunError>, Vec<AgentEvent>) {
    let runner = ScriptedRunner::new(vec![], "@@ -1 +1 @@\n-a\n+b");
    let repo_status = SeqRepoStatus::new(vec![vec![]]);
    let tools = OneSchemaTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router,
            providers,
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
        config(),
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Fix the retry bug", &mut messages, &mut budget)
        .await;
    let events = drain(&mut rx);
    (outcome, events)
}

fn fallbacks(events: &[AgentEvent]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ProviderFallback { from, to, .. } => Some((from.clone(), to.clone())),
            _ => None,
        })
        .collect()
}

/// **The #2765 witness.** The execute engine's provider dies mid-turn; the
/// run re-resolves the worker role through its own router and finishes on the
/// healthy profile. On `main` the pipeline attaches no
/// [`FallbackResolver`](stella_core::ports::FallbackResolver), so the swap is
/// never announced and the healthy provider never serves an engine turn — the
/// run dies with the sick one.
#[tokio::test]
async fn an_exhausted_execute_turn_re_resolves_the_worker_and_finishes_on_the_fallback() {
    let sick = FailsEngineTurns::new("sick", management_script());
    let healthy = AlwaysHealthy::new("healthy");
    let providers = ByProviderId(vec![("sick", &sick), ("healthy", &healthy)]);
    let router = router_over(&["sick", "healthy"]);

    let (outcome, events) = run(&router, &providers).await;

    let outcome = outcome.expect("the run must not fail: a healthy fallback was configured");
    // **The #2918 witness.** No test command is configured here, so since
    // #2915 the run buys a continue round before settling `Unverified` — a
    // second, independent engine turn off the same candidate engine. The
    // latch is scoped to that engine (`driver::model_fallback`), so the
    // second turn starts on the replacement the first turn already settled
    // on: it never touches `sick`, and there is no second swap to announce.
    //
    // On `main` the latch lived in a `OnceLock` that `with_turn_halt`
    // COPIED per turn, so the swap died with the turn that made it and this
    // reads `[(sick, healthy), (sick, healthy)]` with `sick.engine_calls()
    // == 2` — one wasted round trip against a provider already known dead,
    // plus a duplicate user-visible `Error`.
    assert_eq!(
        fallbacks(&events),
        vec![("sick".to_string(), "healthy".to_string())],
        "exactly one announced swap for the whole run, sick -> healthy: a settled route is \
         settled for the candidate, and re-announcing it every turn is noise, not L-M7 \
         honesty: {events:?}"
    );
    assert_eq!(
        sick.engine_calls(),
        1,
        "the dead provider is paid ONCE: the first turn's ladder establishes it is down, and \
         no later turn on this engine may re-discover that at the cost of another round trip"
    );
    // The guard that stops the assertions above from being satisfied by the
    // run simply getting shorter. `sick` being called once is only evidence
    // of a surviving latch if a second engine turn actually happened — and
    // if it did, `healthy` is the provider that served it.
    assert!(
        healthy.engine_calls() >= 2,
        "this witness needs a MULTI-turn run to say anything; the replacement served only \
         {} engine turn(s)",
        healthy.engine_calls()
    );
    assert!(
        healthy
            .engine_transcripts()
            .first()
            .is_some_and(|seen| seen.contains("Fix the retry bug")),
        "the replacement must receive the run's own goal, not a fresh turn: {:?}",
        healthy.engine_transcripts()
    );
    assert!(
        outcome.final_text.contains("rescued"),
        "the run's answer comes from the replacement: {outcome:?}"
    );
    assert_eq!(
        outcome.candidates_run, 1,
        "the swapped turn still counts as the candidate that ran: {outcome:?}"
    );
}

/// The negative control, and the proof this harness can fail: with the sick
/// provider as the ONLY profile there is nothing to fail over to, so the
/// resolver refuses, nothing is announced, and the run surfaces the failure
/// exactly as it did before #2765. Without this, a fallback event asserted
/// above could be coming from anywhere.
#[tokio::test]
async fn with_no_second_profile_nothing_is_announced_and_the_failure_stands() {
    let sick = FailsEngineTurns::new("sick", management_script());
    let healthy = AlwaysHealthy::new("healthy");
    // The adapter set still HOLDS the healthy provider — only the router's
    // profiles are narrowed. A swap here could only come from something
    // reaching past the router, which is precisely what must not happen.
    let providers = ByProviderId(vec![("sick", &sick), ("healthy", &healthy)]);
    let router = router_over(&["sick"]);

    let (_outcome, events) = run(&router, &providers).await;

    assert!(
        fallbacks(&events).is_empty(),
        "a swap must never be announced when the router has nowhere to go: {events:?}"
    );
    assert_eq!(
        healthy.engine_calls(),
        0,
        "an adapter the router did not choose must never serve a turn"
    );
}
