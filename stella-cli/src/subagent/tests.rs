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
