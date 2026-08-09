//! Durable adapter for paid one-shot calls that are not part of an engine turn.
//!
//! This is the standalone counterpart to `stella-pipeline`'s `metered_raw_call`
//! chokepoint, and it carries the same reasoning-starvation recovery for the
//! same reason. Every call through here is a bounded call with a written output
//! contract — a JSON lessons array, an authored manifest, a domain list — and a
//! reasoning model bills its thinking against the one `max_output_tokens`
//! number on the wire. #2128 taught the pipeline to notice that and left this
//! path untouched, so post-turn reflection kept dispatching a reasoning model
//! against a bare 2,048-token cap; it came back empty with
//! `finish_reason: length`, which parses to zero lessons exactly like a turn
//! with nothing to learn, and the context lifecycle sat frozen for nine days
//! (#2174).

use std::path::Path;
use std::time::Duration;

use stella_core::starvation::{starved_of_output, starved_retry_cap};
use stella_core::{
    AccountedCall, AccountedCallError, BudgetGuard, RetryPolicy, run_accounted_call,
};
use stella_protocol::{AgentEvent, CompletionRequest, CompletionResult, ModelCallRole, Provider};
use stella_store::Store;
use tokio::sync::mpsc;

use crate::agent;
use crate::runtime::TokioSleeper;

#[derive(Debug)]
pub(crate) struct StandaloneCompletion {
    pub(crate) result: CompletionResult,
    pub(crate) cost_usd: f64,
    pub(crate) events: Vec<AgentEvent>,
}

#[derive(Debug)]
pub(crate) struct StandaloneCallError {
    pub(crate) message: String,
    pub(crate) cost_usd: f64,
    pub(crate) events: Vec<AgentEvent>,
}

pub(crate) async fn complete_standalone(
    workspace_root: &Path,
    provider: &dyn Provider,
    role: ModelCallRole,
    kind: &str,
    model_hint: &str,
    budget_limit: Option<f64>,
    request: CompletionRequest,
) -> Result<StandaloneCompletion, StandaloneCallError> {
    let store = Store::open(workspace_root).map_err(|error| StandaloneCallError {
        message: format!("accounting store unavailable before model dispatch: {error}"),
        cost_usd: 0.0,
        events: Vec::new(),
    })?;
    let execution_id = store
        .begin_execution(
            kind,
            "content-free system operation",
            provider.id(),
            model_hint,
        )
        .map_err(|error| StandaloneCallError {
            message: format!("accounting execution unavailable before model dispatch: {error}"),
            cost_usd: 0.0,
            events: Vec::new(),
        })?;
    let mut budget: BudgetGuard = agent::build_budget_guard(budget_limit);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let events = stella_core::EventSender::new(tx.clone());
    // Kept for the starvation retry, and ONLY when one could change the
    // outcome — the same trade `metered_raw_call` makes: one `Vec` copy of a
    // bounded prompt against a provider round trip whose result would
    // otherwise be discarded as "the model had nothing to say".
    let raised = starved_retry_cap(request.max_output_tokens);
    let retry = raised.map(|raised| {
        // The retry is the SAME request with one number changed. Every tuning
        // field rides along: a retry that quietly dropped the pinned effort
        // would be a different call answering a different question.
        (
            raised,
            request.messages.clone(),
            request.tools.clone(),
            (request.temperature, request.effort, request.reasoning),
            request.params,
        )
    });
    let mut outcome = dispatch(provider, role, model_hint, request, &mut budget, &events).await;
    // What a superseded attempt already spent. Every arm below adds it, because
    // the retry is a real paid call: reporting only the surviving attempt's
    // cost would understate the turn by exactly the amount the starvation cost
    // us, which is the number an operator most wants to see.
    let mut superseded_cost = 0.0;
    // A provider that stopped at the token limit with nothing visible to show
    // for it has stated that the budget ran out before the first answer token.
    // Every other outcome falls through unchanged. The retry emits its own
    // `StepUsage` — the honest record, since it is not free and the telemetry
    // must not imply it was.
    if let (Ok(result), Some((raised, messages, tools, (temperature, effort, reasoning), params))) =
        (&outcome, retry)
        && starved_of_output(result)
    {
        superseded_cost = result.cost_usd;
        outcome = dispatch(
            provider,
            role,
            model_hint,
            CompletionRequest {
                messages,
                max_output_tokens: Some(raised),
                temperature,
                effort,
                tools,
                reasoning,
                params,
            },
            &mut budget,
            &events,
        )
        .await;
    }
    // BOTH senders, and the order is not cosmetic: `events` holds a clone of
    // `tx`, so dropping only `tx` leaves the channel open and the drain below
    // blocks forever on a `recv()` that can never return `None`. Before the
    // starvation retry the sender was built inline at the single dispatch and
    // died with that statement; naming it so two dispatches can share it is
    // what made the lifetime explicit — and this the explicit end of it.
    drop(events);
    drop(tx);
    let mut persistence_complete = true;
    let mut seq = 0;
    let mut settled_events = Vec::new();
    while let Some(event) = rx.recv().await {
        persistence_complete &=
            agent::persist_event(&store, execution_id, seq, &event, provider.id());
        settled_events.push(event);
        seq += 1;
    }
    match outcome {
        Ok(result) => {
            let cost_usd = superseded_cost + result.cost_usd;
            let complete = persistence_complete
                && store
                    .finish_execution_accounted(
                        execution_id,
                        "completed",
                        cost_usd,
                        persistence_complete,
                    )
                    .is_ok();
            if !complete {
                return Err(StandaloneCallError {
                    message: "model call settled but its accounting closeout failed".into(),
                    cost_usd,
                    events: settled_events,
                });
            }
            Ok(StandaloneCompletion {
                result,
                cost_usd,
                events: settled_events,
            })
        }
        Err(AccountedCallError::Budget { result, .. }) => {
            let cost_usd = superseded_cost + result.cost_usd;
            let _ = store.finish_execution_accounted(
                execution_id,
                "aborted",
                cost_usd,
                persistence_complete,
            );
            Err(StandaloneCallError {
                message: "model call settled over the configured budget".into(),
                cost_usd,
                events: settled_events,
            })
        }
        Err(AccountedCallError::Provider(error)) => {
            let _ =
                store.finish_execution_accounted(execution_id, "failed", superseded_cost, false);
            Err(StandaloneCallError {
                message: error.to_string(),
                cost_usd: superseded_cost,
                events: settled_events,
            })
        }
        Err(AccountedCallError::Timeout) => {
            let _ =
                store.finish_execution_accounted(execution_id, "failed", superseded_cost, false);
            Err(StandaloneCallError {
                message: "model call timed out".into(),
                cost_usd: superseded_cost,
                events: settled_events,
            })
        }
    }
}

/// One metered dispatch, so the starvation retry re-sends through exactly the
/// accounting seam the first attempt used.
async fn dispatch(
    provider: &dyn Provider,
    role: ModelCallRole,
    model_hint: &str,
    request: CompletionRequest,
    budget: &mut BudgetGuard,
    events: &stella_core::EventSender,
) -> Result<CompletionResult, AccountedCallError> {
    run_accounted_call(
        AccountedCall {
            provider,
            role,
            model_hint: model_hint.to_string(),
            request,
            retry_policy: RetryPolicy::deterministic(),
            timeout: Some(Duration::from_secs(120)),
            estimated_input_tokens: 0,
            receipt: None,
        },
        budget,
        events,
        &TokioSleeper,
    )
    .await
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use stella_protocol::{CompletionRequestRef, CompletionUsage, ProviderError};

    use super::*;

    struct PaidProvider;

    #[async_trait]
    impl Provider for PaidProvider {
        fn id(&self) -> &str {
            "paid-test"
        }

        async fn complete_ref(
            &self,
            _request: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            Ok(CompletionResult {
                text: "[]".into(),
                tool_calls: Vec::new(),
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 10,
                    output_tokens: 2,
                    ..CompletionUsage::default()
                },
                model: "paid-model".into(),
                cost_usd: 0.0125,
                finish_reason: None,
            })
        }
    }

    fn request() -> CompletionRequest {
        CompletionRequest {
            messages: Vec::new(),
            max_output_tokens: Some(32),
            temperature: None,
            effort: None,
            tools: Vec::new(),
            reasoning: None,
            params: None,
        }
    }

    #[tokio::test]
    async fn all_four_standalone_paid_call_sites_return_and_persist_exact_cost() {
        let root = tempfile::tempdir().expect("root");
        for (role, kind) in [
            (ModelCallRole::AgentAuthor, "agent_author"),
            (ModelCallRole::SkillAuthor, "skill_author"),
            (ModelCallRole::DomainInference, "domain_inference"),
            (ModelCallRole::Reflection, "reflection"),
        ] {
            let outcome = complete_standalone(
                root.path(),
                &PaidProvider,
                role,
                kind,
                "paid-model",
                None,
                request(),
            )
            .await
            .expect("accounted call");
            assert_eq!(outcome.cost_usd, 0.0125);
            assert!(outcome.events.iter().any(|event| matches!(
                event,
                AgentEvent::StepUsage { role: actual, .. } if actual == &role
            )));
        }

        let store = Store::open(root.path()).expect("store");
        assert_eq!(store.count("telemetry").expect("telemetry count"), 4);
        let json = store
            .export_all_json()
            .expect("export")
            .into_iter()
            .find_map(|(table, json)| (table == "telemetry").then_some(json))
            .expect("telemetry");
        for role in [
            "agent_author",
            "skill_author",
            "domain_inference",
            "reflection",
        ] {
            assert!(json.contains(role), "missing persisted role {role}: {json}");
        }
    }

    /// #2174 witness: a call whose whole budget went to reasoning is retried
    /// with real room, and both attempts are paid for honestly.
    ///
    /// The provider here reproduces the shape execution 63 actually returned:
    /// empty text with `finish_reason: length`, which every caller reads as
    /// "the model had nothing to say". `metered_raw_call` learned to recognise
    /// it in #2128; this chokepoint did not, so post-turn reflection kept
    /// discarding a recoverable call and the context lifecycle sat frozen for
    /// nine days with every surface reporting health.
    #[tokio::test]
    async fn a_call_starved_by_its_own_reasoning_is_retried_with_room() {
        use std::sync::atomic::{AtomicU32, Ordering};

        /// Starves on the first attempt, answers on the second — and records
        /// the cap it was asked for each time.
        #[derive(Default)]
        struct StarvingProvider {
            attempts: AtomicU32,
            caps: std::sync::Mutex<Vec<Option<u32>>>,
        }

        #[async_trait]
        impl Provider for StarvingProvider {
            fn id(&self) -> &str {
                "starving"
            }

            async fn complete_ref(
                &self,
                request: CompletionRequestRef<'_>,
            ) -> Result<CompletionResult, ProviderError> {
                self.caps
                    .lock()
                    .expect("caps lock")
                    .push(request.max_output_tokens);
                let first = self.attempts.fetch_add(1, Ordering::SeqCst) == 0;
                Ok(CompletionResult {
                    text: if first { String::new() } else { "[]".into() },
                    tool_calls: Vec::new(),
                    usage: CompletionUsage {
                        reported: true,
                        output_tokens: 2,
                        ..CompletionUsage::default()
                    },
                    model: "starving-model".into(),
                    cost_usd: 0.01,
                    finish_reason: first.then_some(stella_protocol::FinishReason::Length),
                })
            }
        }

        let root = tempfile::tempdir().expect("root");
        let provider = StarvingProvider::default();
        let outcome = complete_standalone(
            root.path(),
            &provider,
            ModelCallRole::Reflection,
            "reflection",
            "starving-model",
            None,
            CompletionRequest {
                max_output_tokens: Some(6_144),
                ..request()
            },
        )
        .await
        .expect("the retry answers");

        assert_eq!(
            outcome.result.text, "[]",
            "the caller must receive the RETRY's answer, not the empty first \
             one — an empty lessons array and an unread one are the same bytes"
        );
        assert_eq!(
            *provider.caps.lock().expect("caps lock"),
            vec![
                Some(6_144),
                Some(stella_core::starvation::STARVED_RETRY_CAP)
            ],
            "the retry must buy real room; re-sending the same cap would only \
             starve again"
        );
        assert!(
            (outcome.cost_usd - 0.02).abs() < f64::EPSILON,
            "both attempts are paid calls and the reported cost must say so — \
             charging for one understates the turn by exactly what the \
             starvation cost: {}",
            outcome.cost_usd
        );
    }

    #[tokio::test]
    async fn over_limit_call_persists_exact_cost_before_model_output_can_apply() {
        let root = tempfile::tempdir().expect("root");
        let error = complete_standalone(
            root.path(),
            &PaidProvider,
            ModelCallRole::SkillAuthor,
            "skill_author",
            "paid-model",
            Some(0.001),
            request(),
        )
        .await
        .expect_err("settled call exceeds guard");
        assert_eq!(error.cost_usd, 0.0125);
        let store = Store::open(root.path()).expect("store");
        let rollup = store
            .execution_rollup(1, root.path())
            .expect("rollup")
            .expect("execution");
        assert_eq!(rollup.cost_usd, 0.0125);
        assert_eq!(rollup.outcome, "aborted");
    }
}
