// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Breaker feedback at the model-call boundary (#2673).
//!
//! The witness for wiring this crate went without: `Router` carried a
//! `CircuitBreaker` and the engine observed every call outcome, but nothing
//! connected them — `record_success`/`record_failure` had no production
//! caller, so a provider could fail every call for hours and `resolve` kept
//! routing to it. These tests drive real turns through
//! [`Engine::with_provider_outcomes`] and assert the router's *next*
//! resolution actually routes around the provider those turns watched fail.

use super::super::*;
use crate::ports::Clock;
use crate::router::{CircuitBreaker, ProviderProfile, RoleTable, Router};
use serde_json::Value;
use stella_protocol::{CompletionResult, ModelRef, Role, ToolSchema};

/// A provider whose every call fails unretryably — the transport-class
/// terminal failure the breaker counts.
struct FailingPrimary;

#[async_trait::async_trait]
impl Provider for FailingPrimary {
    fn id(&self) -> &str {
        "primary"
    }

    async fn complete_ref(
        &self,
        _req: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        Err(ProviderError::Auth("primary is down".to_string()))
    }
}

/// The same provider identity, healthy — the recovery half.
struct HealthyPrimary;

#[async_trait::async_trait]
impl Provider for HealthyPrimary {
    fn id(&self) -> &str {
        "primary"
    }

    async fn complete_ref(
        &self,
        _req: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        Ok(CompletionResult {
            upstream_provider: None,
            text: "done".to_string(),
            tool_calls: Vec::new(),
            usage: stella_protocol::CompletionUsage::default(),
            model: "primary-strong".to_string(),
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

struct NoTools;

#[async_trait::async_trait]
impl ToolExecutor for NoTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        Vec::new()
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: String::new(),
            data: None,
        }
    }
}

struct NoSleep;

#[async_trait::async_trait]
impl crate::retry::Sleeper for NoSleep {
    async fn sleep(&self, _duration_ms: u64) {}
}

/// Cooldown timing is irrelevant here (nothing advances), so a frozen clock
/// keeps the breaker's open state deterministic for the whole test.
struct FrozenClock;

impl Clock for FrozenClock {
    fn now_ms(&self) -> u64 {
        0
    }
}

fn tier(provider: &str) -> ModelRef {
    ModelRef::new(provider, format!("{provider}-strong"))
}

/// Two configured providers, `primary` preferred — the router every test
/// resolves against while turns feed its breaker.
fn router() -> Router {
    let profile = |id: &str| ProviderProfile::new(id, tier(id), tier(id), tier(id));
    Router::new(
        RoleTable::new(),
        vec![profile("primary"), profile("standby")],
        CircuitBreaker::new(Box::new(FrozenClock)),
    )
}

/// One real turn whose engine reports call outcomes into `router` — the
/// production wiring shape (`&Router`, shared, at the call site).
async fn run_turn_feeding(provider: &dyn Provider, router: &Router) -> TurnOutcome {
    let tools = NoTools;
    let sleeper = NoSleep;
    let engine = Engine::with_sleeper(provider, &tools, EngineConfig::default(), &sleeper)
        .with_provider_outcomes(router);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut messages = vec![CompletionMessage::user("hi")];
    let mut budget = BudgetGuard::new(stella_protocol::BudgetMode::Off, None, None);
    engine.run_turn(&mut messages, &mut budget, &tx).await
}

/// The #2673 witness: a provider failing the threshold of consecutive calls
/// is actually skipped by the next `resolve`, with the substitution reported
/// as a fallback rather than made silently (L-M7).
#[tokio::test]
async fn a_provider_failing_the_threshold_of_calls_is_skipped_by_the_next_resolve() {
    let router = router();
    let before = router.resolve(Role::Worker).expect("healthy resolution");
    assert_eq!(before.model_ref, tier("primary"));

    for _ in 0..CircuitBreaker::DEFAULT_FAILURE_THRESHOLD {
        let outcome = run_turn_feeding(&FailingPrimary, &router).await;
        assert!(matches!(outcome, TurnOutcome::Aborted { .. }));
    }

    let after = router.resolve(Role::Worker).expect("standby is healthy");
    assert_eq!(after.model_ref, tier("standby"));
    let fallback = after.fallback.expect("breaker fallback is never silent");
    assert_eq!(fallback.from, "primary");
    assert_eq!(fallback.to, "standby");
}

/// Below the threshold the breaker stays closed: one bad turn must not
/// reroute a provider that is merely having a moment.
#[tokio::test]
async fn one_failed_call_does_not_trip_failover() {
    let router = router();
    let outcome = run_turn_feeding(&FailingPrimary, &router).await;
    assert!(matches!(outcome, TurnOutcome::Aborted { .. }));

    let decision = router.resolve(Role::Worker).expect("still resolves");
    assert_eq!(decision.model_ref, tier("primary"));
    assert_eq!(decision.fallback, None);
}

/// A committed call reports success and fully closes the breaker, so
/// resolution returns to the preferred provider.
#[tokio::test]
async fn a_committed_call_closes_the_breaker_again() {
    let router = router();
    for _ in 0..CircuitBreaker::DEFAULT_FAILURE_THRESHOLD {
        run_turn_feeding(&FailingPrimary, &router).await;
    }
    assert_eq!(
        router.resolve(Role::Worker).expect("fallback").model_ref,
        tier("standby")
    );

    let outcome = run_turn_feeding(&HealthyPrimary, &router).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));

    let healed = router.resolve(Role::Worker).expect("healed resolution");
    assert_eq!(healed.model_ref, tier("primary"));
    assert_eq!(healed.fallback, None);
}
