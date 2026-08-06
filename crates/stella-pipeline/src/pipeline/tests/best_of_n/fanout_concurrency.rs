//! Best-of-N candidates executing *at the same time* (#1215) — the wall-clock
//! half of the isolation contract `best_of_n` pins the correctness half of.
//!
//! Every other test in this tree runs against doubles that resolve without
//! ever returning `Pending`, so a concurrent fan-out and a sequential one are
//! observationally identical there: `buffer_unordered` polls candidate 0 until
//! it completes before it ever reaches candidate 1. That is a feature for
//! those tests (they stay deterministic) and useless for this one, which has
//! to prove candidates genuinely overlap.
//!
//! [`OverlapProbe`] is the instrument: a provider that yields to the runtime
//! inside every completion and records how many completions were in flight at
//! the peak. One candidate at a time can never move that number past 1,
//! however much it yields — so the peak *is* the concurrency, and the scripted
//! per-call yield counts let a test make candidates finish in an order other
//! than their index.

use super::*;

use std::sync::atomic::AtomicU32;
use stella_protocol::ToolCallObserver;

/// A [`Provider`] that measures how many completions overlap.
///
/// Each call pops its scripted `(result, extra_yields)` pair on entry, so the
/// response a candidate receives is fixed by the order candidates *reach* the
/// model, while how long it stays there is scripted independently. That
/// separation is what lets a test hold candidate 0 inside the model while its
/// siblings run to completion.
struct OverlapProbe {
    script: std::sync::Mutex<VecDeque<(CompletionResult, usize)>>,
    in_flight: AtomicU32,
    peak: AtomicU32,
    /// `enter:<text>` / `exit:<text>` in real order — the record of who
    /// actually finished first.
    trace: std::sync::Mutex<Vec<String>>,
    /// A fragment streamed through the observer on every call, so a test can
    /// see whether live previews reach the consumer.
    stream_preview: bool,
}

impl OverlapProbe {
    /// `script` pairs each completion with the number of EXTRA runtime yields
    /// the call parks for. Every call yields at least once regardless: one
    /// yield is what gives a sibling the chance to be polled into its own
    /// call, which is the minimum any overlap needs.
    fn new(script: Vec<(CompletionResult, usize)>) -> Self {
        Self {
            script: std::sync::Mutex::new(script.into_iter().collect()),
            in_flight: AtomicU32::new(0),
            peak: AtomicU32::new(0),
            trace: std::sync::Mutex::new(Vec::new()),
            stream_preview: false,
        }
    }

    fn streaming(mut self) -> Self {
        self.stream_preview = true;
        self
    }

    /// The most completions that were ever in flight together.
    fn peak(&self) -> u32 {
        self.peak.load(Ordering::SeqCst)
    }

    fn trace(&self) -> Vec<String> {
        self.trace.lock().unwrap().clone()
    }

    /// The order calls *returned*, by response text.
    fn exit_order(&self) -> Vec<String> {
        self.trace()
            .into_iter()
            .filter_map(|line| line.strip_prefix("exit:").map(str::to_string))
            .collect()
    }
}

#[async_trait]
impl Provider for OverlapProbe {
    fn id(&self) -> &str {
        "overlap-probe"
    }

    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        let (result, extra_yields) = {
            let mut script = self.script.lock().unwrap();
            script
                .pop_front()
                .ok_or_else(|| ProviderError::Terminal("overlap probe exhausted".into()))?
        };
        let live = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(live, Ordering::SeqCst);
        self.trace
            .lock()
            .unwrap()
            .push(format!("enter:{}", result.text));
        for _ in 0..=extra_yields {
            tokio::task::yield_now().await;
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        self.trace
            .lock()
            .unwrap()
            .push(format!("exit:{}", result.text));
        Ok(result)
    }

    async fn complete_observed_ref(
        &self,
        req: CompletionRequestRef<'_>,
        observer: &dyn ToolCallObserver,
    ) -> Result<CompletionResult, ProviderError> {
        if self.stream_preview {
            // Two fragments, so an interleaved stream would visibly splice
            // rather than merely reorder.
            observer.text_delta("frag-a");
            observer.reasoning_delta("thinking");
        }
        self.complete_ref(req).await
    }
}

/// A resolver serving the probe for every role.
struct OneProbe<'p>(&'p OverlapProbe);
impl ProviderResolver for OneProbe<'_> {
    fn provider_for(&self, _model: &ModelRef) -> Option<&dyn Provider> {
        Some(self.0)
    }
}

/// [`super::run_isolated`] over the overlap probe, additionally returning the
/// turn's budget guard so a test can assert what the fan-out actually spent.
async fn run_probed(
    provider: &OverlapProbe,
    port: &FakeWorkspacePort,
    config: PipelineConfig,
    budget: BudgetGuard,
) -> (PipelineOutcome, Vec<AgentEvent>, BudgetGuard) {
    let resolver = OneProbe(provider);
    let diagnostics = NeverRunner;
    let repo_status = NeverRepoStatus;
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
            repo_status: &repo_status,
            touches: &NoFileTouches,
            diagnostics: &diagnostics,
            tests: &diagnostics,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: Some(port),
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        config,
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = budget;
    let outcome = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("a scripted fan-out completes");
    (outcome, drain(&mut rx), budget)
}

/// One workspace per candidate, every one of them flipping fail→pass so the
/// deterministic ladder submits without buying a verifier call — which keeps the
/// provider script to exactly one entry per candidate.
fn flipping_workspaces(n: usize, log: &Arc<std::sync::Mutex<Vec<String>>>) -> FakeWorkspacePort {
    FakeWorkspacePort::new(
        (0..n)
            .map(|id| {
                Ok(FakeWorkspace::new(
                    id,
                    vec![false, true],
                    Ok(vec![]),
                    log.clone(),
                ))
            })
            .collect(),
        log.clone(),
    )
}

/// `triage` plus one worker turn per candidate, each parking for `yields[i]`
/// extra runtime yields.
fn fanout_script(yields: &[usize]) -> Vec<(CompletionResult, usize)> {
    let mut script = vec![(text_result("single"), 0)];
    for (i, extra) in yields.iter().enumerate() {
        script.push((text_result(&format!("cand{i} done")), *extra));
    }
    script
}

fn off_budget() -> BudgetGuard {
    BudgetGuard::new(BudgetMode::Off, None, None)
}

/// The issue in one assertion: three isolated candidates are inside the model
/// **together**, so the fan-out costs the slowest candidate rather than the
/// sum of all three. Sequential dispatch pins the peak at one however long
/// each candidate parks.
#[tokio::test]
async fn isolated_candidates_execute_concurrently() {
    let provider = OverlapProbe::new(fanout_script(&[0, 0, 0]));
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = flipping_workspaces(3, &log);

    let (outcome, _events, _budget) =
        run_probed(&provider, &port, isolated_config(3), off_budget()).await;

    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert_eq!(outcome.candidates_run, 3);
    assert_eq!(
        provider.peak(),
        3,
        "all three candidates must be in flight together, not one at a time: {:?}",
        provider.trace()
    );
}

/// The knob an operator reaches for when they need one candidate's events to
/// arrive without a sibling's spliced in: `Some(1)` is exactly the sequential
/// dispatch that predates concurrency.
#[tokio::test]
async fn a_candidate_concurrency_of_one_restores_sequential_dispatch() {
    let provider = OverlapProbe::new(fanout_script(&[0, 0, 0]));
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = flipping_workspaces(3, &log);
    let config = PipelineConfig {
        candidate_concurrency: Some(1),
        ..isolated_config(3)
    };

    let (outcome, _events, _budget) = run_probed(&provider, &port, config, off_budget()).await;

    assert_eq!(outcome.candidates_run, 3);
    assert_eq!(
        provider.peak(),
        1,
        "width 1 must never overlap two candidates: {:?}",
        provider.trace()
    );
    assert_eq!(
        provider.exit_order(),
        vec!["single", "cand0 done", "cand1 done", "cand2 done"],
        "a sequential fan-out finishes in index order"
    );
}

/// Selection must read the same whoever finishes first. Candidate 0 is held
/// inside the model until both siblings have finished, and candidates 0 and 1
/// tie on evidence and diff size — so the winner is decided by the earliest
/// index, not by the earliest finish.
#[tokio::test]
async fn the_winner_does_not_depend_on_which_candidate_finishes_first() {
    // Candidate 0 parks for a long stretch of yields; 1 and 2 run straight
    // through, so the exit order is 1, 2, 0.
    let provider = OverlapProbe::new(fanout_script(&[24, 0, 0]));
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = FakeWorkspacePort::new(
        vec![
            // 0 and 1 both flip fail→pass on identical diffs: a genuine tie.
            Ok(FakeWorkspace::new(
                0,
                vec![false, true],
                Ok(vec![]),
                log.clone(),
            )),
            Ok(FakeWorkspace::new(
                1,
                vec![false, true],
                Ok(vec![]),
                log.clone(),
            )),
            // 2 never flips.
            Ok(FakeWorkspace::new(
                2,
                vec![false, false],
                Ok(vec![]),
                log.clone(),
            )),
        ],
        log.clone(),
    );

    let (outcome, _events, _budget) =
        run_probed(&provider, &port, isolated_config(3), off_budget()).await;

    let exits = provider.exit_order();
    assert!(
        exits.iter().position(|e| e == "cand1 done") < exits.iter().position(|e| e == "cand0 done"),
        "the test is only meaningful if candidate 1 really finished first: {exits:?}"
    );
    assert_eq!(
        outcome.final_text, "cand0 done",
        "a tie is broken by the earliest INDEX, never by the earliest finish"
    );
    let log = log.lock().unwrap().clone();
    assert_eq!(
        log.iter()
            .filter(|e| e.starts_with("adopt"))
            .collect::<Vec<_>>(),
        vec!["adopt:0"],
        "the adopted workspace must be the winner's, whoever finished first: {log:?}"
    );
}

/// Every candidate spends against a carve of the turn's budget and folds it
/// back on the way out, so a concurrent fan-out is not a hole in the
/// accounting: the guard and the reported total both see every candidate's
/// money exactly once.
#[tokio::test]
async fn concurrent_candidates_settle_every_dollar_back_into_the_turn_budget() {
    let provider = OverlapProbe::new(fanout_script(&[0, 0, 0]));
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = flipping_workspaces(3, &log);

    let (outcome, _events, budget) =
        run_probed(&provider, &port, isolated_config(3), off_budget()).await;

    // Triage plus one worker turn per candidate, at `text_result`'s cost.
    let expected = 4.0 * 0.0001;
    assert!(
        (outcome.total_cost_usd - expected).abs() < 1e-9,
        "the reported cost must be every candidate's spend, summed once: {}",
        outcome.total_cost_usd
    );
    assert!(
        (budget.spent_usd() - expected).abs() < 1e-9,
        "the caller's guard must see the same money the outcome reports: {}",
        budget.spent_usd()
    );
}

/// A candidate the fan-out cannot fund never reaches a model. Without the
/// pre-dispatch gate every remaining slot would buy one doomed call first and
/// discover the enforced cap over again.
///
/// The cap here funds triage and the first in-flight window and nothing more:
/// `width = 2`, so candidates 0 and 1 dispatch together, and by the time a
/// slot frees for candidate 2 their settled spend has crossed it.
#[tokio::test]
async fn an_exhausted_enforced_budget_stops_dispatching_further_candidates() {
    let provider = OverlapProbe::new(fanout_script(&[0, 0, 0]));
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = flipping_workspaces(3, &log);
    let config = PipelineConfig {
        candidate_concurrency: Some(2),
        ..isolated_config(3)
    };
    // Triage costs 0.0001 and leaves headroom; the first window's two
    // candidates cost 0.0001 each and do not.
    let budget = BudgetGuard::new(BudgetMode::Enforced, Some(0.00015), None);

    let (_outcome, _events, _budget) = run_probed(&provider, &port, config, budget).await;

    let entered: Vec<String> = provider
        .trace()
        .into_iter()
        .filter_map(|line| line.strip_prefix("enter:").map(str::to_string))
        .collect();
    assert!(
        entered.contains(&"cand0 done".to_string()),
        "the funded window still runs: {entered:?}"
    );
    assert!(
        !entered.contains(&"cand2 done".to_string()),
        "a candidate dispatched after the cap was crossed must never reach the model: {entered:?}"
    );
}

/// N models sharing one event stream mute the two event kinds the wire
/// contract already calls best-effort previews. Everything durable — each
/// candidate's authoritative `Text` — still arrives.
#[tokio::test]
async fn a_concurrent_fan_out_mutes_previews_but_never_the_authoritative_text() {
    let provider = OverlapProbe::new(fanout_script(&[0, 0, 0])).streaming();
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = flipping_workspaces(3, &log);

    let (_outcome, events, _budget) =
        run_probed(&provider, &port, isolated_config(3), off_budget()).await;

    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::TextDelta { .. } | AgentEvent::Reasoning { .. }
        )),
        "three models' fragments must not be spliced into one preview: {events:?}"
    );
    for i in 0..3 {
        let expected = format!("cand{i} done");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Text { text: delta } if *delta == expected)),
            "every candidate's authoritative text still reaches the consumer: {events:?}"
        );
    }
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Text { text: delta } if delta.contains("candidates at once")
        )),
        "the fan-out says why the stream went quiet: {events:?}"
    );
}

/// A sequential fan-out has nobody to interleave with, so it streams exactly
/// as it always did.
#[tokio::test]
async fn a_sequential_fan_out_still_streams_its_previews() {
    let provider = OverlapProbe::new(fanout_script(&[0, 0])).streaming();
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let port = flipping_workspaces(2, &log);
    let config = PipelineConfig {
        candidate_concurrency: Some(1),
        ..isolated_config(2)
    };

    let (_outcome, events, _budget) = run_probed(&provider, &port, config, off_budget()).await;

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { delta: text } if text == "frag-a")),
        "width 1 keeps the live preview: {events:?}"
    );
}
