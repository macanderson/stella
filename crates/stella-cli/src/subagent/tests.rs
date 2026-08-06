// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Session dispatcher tests (#922), and the one that matters most: the
//! shipped tool stack forwards sub-agent spend end to end.

use serde_json::{Value, json};
use stella_core::ports::ToolExecutor;
use stella_core::subagent::{SubAgentSpendLedger, push_sub_agent_spend};
use stella_protocol::{ToolOutput, ToolSchema};

use super::*;

/// A leaf executor standing in for the registry: it holds the spend ledger
/// the `task` tool writes to, and reports it exactly as the registry does.
struct LedgerBase(SubAgentSpendLedger);

#[async_trait]
impl ToolExecutor for LedgerBase {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "read_file".into(),
            description: "read".into(),
            input_schema: json!({"type": "object"}),
            read_only: true,
            speculation_safe: false,
        }]
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: String::new(),
        }
    }
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        stella_core::subagent::drain_sub_agent_spend(&self.0)
    }
}

struct NeverProvider;

#[async_trait]
impl Provider for NeverProvider {
    fn id(&self) -> &str {
        "never"
    }
    async fn complete_ref(
        &self,
        _request: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<stella_protocol::CompletionResult, stella_protocol::ProviderError> {
        Err(stella_protocol::ProviderError::Terminal(
            "no provider in tests".into(),
        ))
    }
}

fn registry() -> Arc<ToolRegistry> {
    Arc::new(ToolRegistry::with_issue_backend(
        std::path::PathBuf::from("."),
        None,
    ))
}

/// **The decorator-forwarding witness.**
///
/// The deck stacks several executors between the engine and the registry
/// (`CustomToolSet → InteractiveToolSet → PolicyToolSet → DiscoveryToolSet`,
/// plus the taps). `drain_sub_agent_spend_usd` has a `0.0` default, so any
/// one of them forgetting to forward would silently drop a child's cost out
/// of the parent's budget — and no compiler would say so.
///
/// This asserts through the *shipped* composition rather than a hypothetical
/// one: a future decorator inserted into the real stack fails here, which is
/// the only place that can catch it.
#[tokio::test]
async fn the_production_tool_stack_forwards_sub_agent_spend() {
    let ledger: SubAgentSpendLedger = Arc::default();
    let base = LedgerBase(ledger.clone());
    push_sub_agent_spend(&ledger, 0.42);

    let customs =
        stella_tools::custom::CustomToolSet::new(&base, Vec::new(), std::path::PathBuf::from("."));
    let (stub_tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let interactive = crate::interactive::InteractiveToolSet::new(
        &customs,
        stub_tx,
        crate::interactive::default_ask_io(false),
    );
    let permitted = crate::agent::PolicyToolSet::new(&interactive, Default::default());
    let discovery =
        crate::discovery::DiscoveryToolSet::new(&permitted, std::path::PathBuf::from("."));

    assert!(
        (discovery.drain_sub_agent_spend_usd() - 0.42).abs() < 1e-9,
        "a child's cost must survive every decorator between the engine and \
         the registry — one that swallows it silently under-bills the turn"
    );
    assert_eq!(
        discovery.drain_sub_agent_spend_usd(),
        0.0,
        "and the drain stays destructive through the stack"
    );
}

/// The wait-request twin of the spend witness above: `drain_wait_request`
/// has a `None` default, so any one decorator forgetting to forward would
/// silently turn parked waits (#1471) back into model-step polling for
/// every session composed through it — and no compiler would say so.
#[tokio::test]
async fn the_production_tool_stack_forwards_wait_requests() {
    /// A leaf holding one deposited request, standing in for the registry.
    struct WaitingBase(std::sync::Mutex<Option<stella_core::WaitRequest>>);

    #[async_trait]
    impl ToolExecutor for WaitingBase {
        fn schemas(&self) -> Vec<ToolSchema> {
            Vec::new()
        }
        async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
            ToolOutput::Ok {
                content: String::new(),
            }
        }
        fn drain_wait_request(&self) -> Option<stella_core::WaitRequest> {
            self.0.lock().unwrap().take()
        }
    }

    let request = stella_core::WaitRequest {
        description: "CI for branch main settles".into(),
        probe: stella_core::WaitCall {
            name: "ci_status".into(),
            input: json!({ "probe": true, "branch": "main" }),
        },
        baseline: "pending".into(),
        on_wake: None,
        poll_interval_secs: 15,
        timeout_secs: 0,
    };
    let base = WaitingBase(std::sync::Mutex::new(Some(request.clone())));

    let customs =
        stella_tools::custom::CustomToolSet::new(&base, Vec::new(), std::path::PathBuf::from("."));
    let (stub_tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let interactive = crate::interactive::InteractiveToolSet::new(
        &customs,
        stub_tx,
        crate::interactive::default_ask_io(false),
    );
    let permitted = crate::agent::PolicyToolSet::new(&interactive, Default::default());
    let discovery =
        crate::discovery::DiscoveryToolSet::new(&permitted, std::path::PathBuf::from("."));

    assert_eq!(
        discovery.drain_wait_request(),
        Some(request),
        "a deposited wait request must survive every decorator between the \
         engine and the registry — one that swallows it re-enables polling"
    );
    assert_eq!(
        discovery.drain_wait_request(),
        None,
        "and the drain stays destructive through the stack"
    );
}

/// A dispatcher whose registry has been dropped reports a refusal rather
/// than panicking a torn-down session.
#[tokio::test]
async fn a_dropped_registry_refuses_instead_of_panicking() {
    let registry = registry();
    let dispatcher = SessionSubAgents::new(
        Arc::new(NeverProvider),
        &registry,
        EngineConfig::default(),
        stella_protocol::BudgetMode::Observed,
    );
    drop(registry);

    let outcome = dispatcher.dispatch(SubAgentSpec::read_only("x", "y")).await;
    assert!(
        matches!(outcome, SubAgentOutcome::Refused { .. }),
        "got {outcome:?}"
    );
}

/// The pool is a second bound on sub-agent spend, not the hard ceiling —
/// but it must actually bind, and a child that never billed must not charge
/// it.
#[tokio::test]
async fn the_pool_binds_and_a_failed_child_charges_nothing() {
    let registry = registry();
    let dispatcher = SessionSubAgents::new(
        Arc::new(NeverProvider),
        &registry,
        EngineConfig::default(),
        stella_protocol::BudgetMode::Enforced,
    )
    .with_pool_limit(Some(0.05));

    let outcome = dispatcher.dispatch(SubAgentSpec::read_only("a", "q")).await;
    assert!(
        !matches!(outcome, SubAgentOutcome::Completed(_)),
        "a dead provider cannot complete"
    );
    let pool = *dispatcher.pool.lock().unwrap();
    assert_eq!(pool.session_limit_usd(), Some(0.05));
    assert_eq!(
        pool.session_spent_usd(),
        0.0,
        "a failed child that never billed must not charge the pool"
    );
}

#[test]
fn the_task_tool_is_always_advertised_and_never_read_only() {
    let registry = registry();
    // Registered whether or not a dispatcher is attached — an unattached one
    // yields a truthful "unavailable" tool result, which the model can act
    // on, rather than a missing tool it has to infer from absence.
    let advertised: Vec<_> = registry
        .schemas()
        .into_iter()
        .filter(|schema| schema.name == "task")
        .collect();
    assert_eq!(advertised.len(), 1, "registered exactly once");
    assert!(
        !advertised[0].read_only,
        "read_only would let children spawn children"
    );

    SessionSubAgents::install(
        Arc::new(NeverProvider),
        &registry,
        EngineConfig::default(),
        stella_protocol::BudgetMode::Observed,
        None,
    );
    assert_eq!(
        registry
            .schemas()
            .iter()
            .filter(|schema| schema.name == "task")
            .count(),
        1,
        "installing a dispatcher does not duplicate the tool"
    );
}

// ---- turn controls: pause and stop reach a dispatched child ------------

/// Counts how many times a child got as far as asking for a completion. A
/// child that made a model call is a child the controls failed to hold.
#[derive(Default)]
struct CountingProvider(std::sync::atomic::AtomicUsize);

impl CountingProvider {
    fn calls(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl Provider for CountingProvider {
    fn id(&self) -> &str {
        "counting"
    }
    async fn complete_ref(
        &self,
        _request: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<stella_protocol::CompletionResult, stella_protocol::ProviderError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(stella_protocol::ProviderError::Terminal(
            "no provider in tests".into(),
        ))
    }
}

/// The deck's `WatchGate` in miniature — private to its driver, so the shape
/// is reproduced here rather than exported for a test. Counts entries so a
/// parked child is distinguishable from one that has not started.
struct PausableGate {
    rx: tokio::sync::watch::Receiver<bool>,
    polls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl stella_core::ports::TurnGate for PausableGate {
    async fn wait_if_paused(&self) {
        self.polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut rx = self.rx.clone();
        while *rx.borrow() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

fn dispatcher_over(
    provider: Arc<CountingProvider>,
    registry: &Arc<ToolRegistry>,
) -> SessionSubAgents {
    SessionSubAgents::new(
        provider,
        registry,
        EngineConfig::default(),
        stella_protocol::BudgetMode::Observed,
    )
}

/// Spin until `cond` holds, or fail. Used to observe the child's *own*
/// thread reaching a boundary without asserting on wall-clock timing — a
/// fixed sleep would either be flaky or slow, and both are worse than a
/// bounded poll on an atomic.
async fn until(label: &str, cond: impl Fn() -> bool) {
    for _ in 0..2_000 {
        if cond() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    panic!("timed out waiting for {label}");
}

/// **The gap this closes.** A child dispatched from a tool call runs on its
/// own thread against a session-scoped dispatcher, so before the turn
/// published its seams there was nothing connecting the two: Esc ended the
/// parent and the child kept spending.
#[tokio::test]
async fn a_turns_soft_stop_reaches_the_children_it_dispatched() {
    let provider = Arc::new(CountingProvider::default());
    let registry = registry();
    let dispatcher = dispatcher_over(provider.clone(), &registry);

    let tap: Arc<crate::subsession::SteeringTap> = Arc::default();
    tap.request_soft_stop();
    let _controls = registry
        .attach_turn_controls(stella_core::ports::TurnControls::none().with_steering(tap.clone()));

    let outcome = dispatcher
        .dispatch(SubAgentSpec::read_only(
            "stoppable",
            "find the retry policy",
        ))
        .await;

    assert!(
        matches!(&outcome, SubAgentOutcome::Incomplete { reason, .. }
            if reason.contains(stella_core::driver::SOFT_STOP_REASON)),
        "got {outcome:?}"
    );
    assert_eq!(
        provider.calls(),
        0,
        "a stop latched before the child's first boundary stops it there — \
         it must not pay for one more call first"
    );
}

/// The stop crosses; the messages must not. `drain_steering` is destructive
/// by contract, so a child that inherited the tap directly would swallow
/// something the user typed to the parent.
#[tokio::test]
async fn a_dispatched_child_never_eats_the_parents_queued_steering() {
    let provider = Arc::new(CountingProvider::default());
    let registry = registry();
    let dispatcher = dispatcher_over(provider.clone(), &registry);

    let tap: Arc<crate::subsession::SteeringTap> = Arc::default();
    tap.push("actually, check the timeout instead".to_string());
    let _controls = registry
        .attach_turn_controls(stella_core::ports::TurnControls::none().with_steering(tap.clone()));

    dispatcher
        .dispatch(SubAgentSpec::read_only("reader", "read something"))
        .await;

    assert_eq!(
        stella_core::ports::TurnSteering::drain_steering(tap.as_ref()),
        vec!["actually, check the timeout instead".to_string()],
        "the message must still be there for the parent's next boundary"
    );
}

/// Pause means pause, including inside delegated work. The parked child
/// holds the parent's `task` call open, and both continue on resume.
#[tokio::test]
async fn a_paused_turn_parks_its_children_and_resume_releases_them() {
    let provider = Arc::new(CountingProvider::default());
    let registry = registry();
    let dispatcher = Arc::new(dispatcher_over(provider.clone(), &registry));

    let (pause_tx, pause_rx) = tokio::sync::watch::channel(true);
    let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let _controls = registry.attach_turn_controls(
        stella_core::ports::TurnControls::none().with_gate(Arc::new(PausableGate {
            rx: pause_rx,
            polls: polls.clone(),
        })),
    );

    let running = dispatcher.clone();
    let child = tokio::spawn(async move {
        running
            .dispatch(SubAgentSpec::read_only("pausable", "investigate"))
            .await
    });

    until("the child to reach its first step boundary", || {
        polls.load(std::sync::atomic::Ordering::SeqCst) > 0
    })
    .await;
    assert_eq!(
        provider.calls(),
        0,
        "parked at the boundary, not spending through the pause"
    );
    assert!(!child.is_finished(), "and still parked, not finished");

    pause_tx.send(false).expect("the gate is still listening");
    child.await.expect("the child thread reports back");
    assert_eq!(
        provider.calls(),
        1,
        "resume releases it into the very same turn it was parked in"
    );
}

/// The counterpart every turn owes: a stop latched by one turn must not
/// stop the next turn's children. The guard is what makes that structural
/// rather than a call every driver has to remember on every exit path.
#[tokio::test]
async fn controls_come_down_with_the_turn_that_published_them() {
    let provider = Arc::new(CountingProvider::default());
    let registry = registry();
    let dispatcher = dispatcher_over(provider.clone(), &registry);

    {
        let tap: Arc<crate::subsession::SteeringTap> = Arc::default();
        tap.request_soft_stop();
        let _controls = registry
            .attach_turn_controls(stella_core::ports::TurnControls::none().with_steering(tap));
    }
    assert!(registry.turn_controls().is_empty());

    let outcome = dispatcher
        .dispatch(SubAgentSpec::read_only("next-turn", "work"))
        .await;
    assert!(
        !matches!(&outcome, SubAgentOutcome::Incomplete { reason, .. }
            if reason.contains(stella_core::driver::SOFT_STOP_REASON)),
        "last turn's Esc must not stop this turn's children, got {outcome:?}"
    );
    assert_eq!(provider.calls(), 1, "and the child actually ran");
}

// ---- a cancelled parent: bounded, and still billed ---------------------

/// Returns a tool call every time, so a child left to itself runs to its step
/// cap — which is what makes "how many calls did it make" a discriminating
/// question. The first call parks until released, giving a test a window in
/// which the child is provably mid-flight.
struct LoopingPaidProvider {
    calls: std::sync::atomic::AtomicUsize,
    hold: tokio::sync::watch::Receiver<bool>,
    cost_usd: f64,
}

impl LoopingPaidProvider {
    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl Provider for LoopingPaidProvider {
    fn id(&self) -> &str {
        "looping-paid"
    }
    async fn complete_ref(
        &self,
        _request: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<stella_protocol::CompletionResult, stella_protocol::ProviderError> {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n == 0 {
            let mut rx = self.hold.clone();
            while *rx.borrow() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
        }
        Ok(stella_protocol::CompletionResult {
            text: String::new(),
            // Distinct arguments per call: identical calls are a stuck loop,
            // and `loop_detect` is right to abort one.
            tool_calls: vec![stella_protocol::ToolCall {
                call_id: format!("c{n}"),
                name: "read_file".into(),
                input: json!({ "path": format!("no-such-file-{n}.txt") }),
            }],
            usage: stella_protocol::CompletionUsage {
                reported: true,
                ..stella_protocol::CompletionUsage::default()
            },
            model: "looping-paid".into(),
            cost_usd: self.cost_usd,
            finish_reason: None,
        })
    }
}

fn paid_dispatcher(
    cost_usd: f64,
) -> (
    Arc<SessionSubAgents>,
    Arc<ToolRegistry>,
    Arc<LoopingPaidProvider>,
    tokio::sync::watch::Sender<bool>,
) {
    let (hold_tx, hold) = tokio::sync::watch::channel(true);
    let provider = Arc::new(LoopingPaidProvider {
        calls: std::sync::atomic::AtomicUsize::new(0),
        hold,
        cost_usd,
    });
    let registry = registry();
    let dispatcher = Arc::new(SessionSubAgents::new(
        provider.clone(),
        &registry,
        EngineConfig::default(),
        stella_protocol::BudgetMode::Observed,
    ));
    (dispatcher, registry, provider, hold_tx)
}

fn looping_spec() -> SubAgentSpec {
    SubAgentSpec {
        // Comfortably more than one, so a child that ignored the orphan stop
        // would be visibly distinguishable from one that took it.
        max_steps: 6,
        ..SubAgentSpec::read_only("orphan", "investigate")
    }
}

/// **Cascade the intent, not the mechanism.** A hard cancel cannot propagate
/// as a hard cancel — the child is an OS thread, not a future in the parent's
/// graph, and killing a thread mid-tool is exactly what the step-boundary rule
/// forbids. So the parent's cancel becomes the child's *soft stop*, and the
/// orphan is bounded to the call already in flight instead of its whole step
/// cap.
#[tokio::test]
async fn cancelling_the_parent_stops_the_child_at_its_next_boundary() {
    let (dispatcher, _registry, provider, hold_tx) = paid_dispatcher(0.01);

    let running = dispatcher.clone();
    let child = tokio::spawn(async move { running.dispatch(looping_spec()).await });

    until("the child to be inside its first model call", || {
        provider.calls() >= 1
    })
    .await;

    // Awaiting the aborted handle is what makes this deterministic: it
    // resolves only once the task has actually been dropped, which is when
    // `ParentGone` runs.
    child.abort();
    assert!(child.await.is_err(), "the dispatch future was cancelled");

    hold_tx
        .send(false)
        .expect("the provider is still listening");
    until("the orphaned child to settle", || {
        dispatcher.pool.lock().unwrap().session_spent_usd() > 0.0
    })
    .await;

    assert_eq!(
        provider.calls(),
        1,
        "the child must stop at the boundary after the call already in \
         flight — a child that ignored the cancel would run to its step cap"
    );
}

/// The sharper half of the same defect. Settlement used to happen after
/// `wait.await` in the dispatcher and after `dispatch().await` in the tool —
/// neither of which runs when the parent is cancelled mid-`task`, so a child's
/// real spend landed in NO ledger. Charging late to the session is the
/// ledger's doctrine; never charging is not.
#[tokio::test]
async fn a_cancelled_parents_child_still_charges_what_it_spent() {
    let (dispatcher, registry, provider, hold_tx) = paid_dispatcher(0.01);
    let ledger = registry.sub_agent_spend_ledger();

    let running = dispatcher.clone();
    let child = tokio::spawn(async move { running.dispatch(looping_spec()).await });

    until("the child to be inside its first model call", || {
        provider.calls() >= 1
    })
    .await;
    child.abort();
    assert!(child.await.is_err());

    hold_tx
        .send(false)
        .expect("the provider is still listening");
    until("the orphaned child to settle", || {
        dispatcher.pool.lock().unwrap().session_spent_usd() > 0.0
    })
    .await;

    let charged = stella_core::subagent::drain_sub_agent_spend(&ledger);
    assert!(
        (charged - 0.01).abs() < 1e-9,
        "the engine drains this into the parent's guard at the next step \
         boundary; got {charged}"
    );
    assert!(
        (dispatcher.pool.lock().unwrap().session_spent_usd() - 0.01).abs() < 1e-9,
        "and the session pool is charged the same dollars, exactly once"
    );
}

/// The ordinary path settles once, through the same single writer. Two
/// writers (the tool AND the dispatcher) would double-bill every child.
#[tokio::test]
async fn an_uncancelled_child_is_charged_exactly_once() {
    let (dispatcher, registry, _provider, hold_tx) = paid_dispatcher(0.01);
    let ledger = registry.sub_agent_spend_ledger();
    hold_tx.send(false).expect("nothing is holding yet");

    dispatcher.dispatch(looping_spec()).await;

    let charged = stella_core::subagent::drain_sub_agent_spend(&ledger);
    assert!(
        (charged - 0.06).abs() < 1e-9,
        "six steps at a cent each, counted once; got {charged}"
    );
}
