//! Complete per-call accounting for pipeline roles that call providers directly.

use std::sync::atomic::Ordering;
use std::time::Duration;

use stella_core::retry::RetryPolicy;
use stella_core::{
    AccountedCall, AccountedCallError, BudgetGuard, ReceiptContext, run_accounted_call,
};
use stella_protocol::{
    CompletionMessage, CompletionRequest, CompletionResult, ModelCallRole, ReasoningEffort,
};

use super::stage_budget::{PipelineBudgetAbort, budget_abort};
use super::{Pipeline, ResolvedRole, RoleCallOverrides};

pub(super) struct RawCall<'r, 'a> {
    pub(super) role: ModelCallRole,
    pub(super) resolved: &'r ResolvedRole<'a>,
    pub(super) messages: Vec<CompletionMessage>,
    pub(super) policy: RetryPolicy,
    pub(super) overrides: &'r RoleCallOverrides,
    /// The per-call wall clock, normally `EngineConfig::model_timeout` — the
    /// same posture-keyed ceiling the worker path got in #1211/#1277.
    ///
    /// **Every management role passes it.** `None` means "no clock at all",
    /// and the retry policy is not one: a provider that accepts the request
    /// and then dribbles never trips a retry, so an unbounded management call
    /// parks a headless run with no exit short of a hard cancel.
    ///
    /// The reason this is a field on the shared call rather than a default
    /// inside [`Pipeline::metered_raw_call`] is that the *consequence* of the
    /// deadline is per-role, and each role already has one written down:
    /// Verdict falls back to the deterministic heuristic, DistressGuidance to
    /// evidence-only revision, Plan and PlanRepair to the single-step plan.
    /// Those fallbacks are the point. Leaving `timeout: None` did not just
    /// risk a hang — it made the fallback arm unreachable code, so the run
    /// lost the degraded result it was designed to take (#1483, #1501).
    pub(super) timeout: Option<Duration>,
}

pub(super) enum RawCallError {
    Provider,
    Timeout,
    Budget(PipelineBudgetAbort),
}

/// Role-shaped request bounds `(max_output_tokens, effort)` for the
/// management chokepoint, applied between the caller's explicit overrides
/// (which always win) and the engine config's worker-tier base.
///
/// The base is the wrong default here: `engine.max_output_tokens` is seeded
/// from the model catalog's full ceiling (64k on current Claude rows) and an
/// unset `effort` leaves the provider's own default (high) reasoning
/// allowance in force — dispatching a role whose written output contract is
/// two to six lines with a 64k allowance and unbounded thinking. The bounded
/// shape is the one the engine's own overflow summarizer already pins
/// (`stella-core`'s `run_compaction_pass`: 1,200 tokens, `effort: Low`).
///
/// Exhaustive over [`ModelCallRole`] on purpose: a new role dispatched
/// through this chokepoint must decide its bounds here, not inherit the
/// worker ceiling by omission.
fn management_bounds(role: ModelCallRole) -> (Option<u32>, Option<ReasoningEffort>) {
    match role {
        // Three-line classification, already under the L-M4 decision-latency
        // ceiling: pinned-low effort is not only cheaper, it keeps the call
        // inside the ceiling so the fast route actually gets taken.
        ModelCallRole::Triage => (Some(512), Some(ReasoningEffort::Low)),
        // A small ordered-JSON plan. Output is bounded; effort is inherited —
        // plan quality rides the session's own reasoning posture.
        ModelCallRole::Plan | ModelCallRole::PlanRepair => (Some(4096), None),
        // "PASS or FAIL, then one line" / "at most 6 lines" by their own
        // prompts. Effort inherited: both are judgment calls on evidence.
        ModelCallRole::Verdict | ModelCallRole::DistressGuidance => (Some(1024), None),
        // The only raw `Worker` call is the conversational fast path, whose
        // instructions say "reply briefly and warmly in plain prose".
        ModelCallRole::Worker => (Some(2048), Some(ReasoningEffort::Low)),
        // Never dispatched through this chokepoint today; if one ever is,
        // inheriting the engine base is exactly the pre-existing behavior.
        ModelCallRole::Unknown
        | ModelCallRole::WitnessAuthor
        | ModelCallRole::WitnessRepair
        | ModelCallRole::AgentAuthor
        | ModelCallRole::SkillAuthor
        | ModelCallRole::DomainInference
        | ModelCallRole::Reflection
        | ModelCallRole::Summarization => (None, None),
    }
}

impl<'a> Pipeline<'a> {
    /// One metered raw provider completion. Successful calls emit exactly one
    /// `StepUsage` before budget enforcement can return; failures/timeouts emit
    /// one content-free `UsageIncomplete`. All raw roles use this chokepoint.
    pub(super) async fn metered_raw_call(
        &self,
        call: RawCall<'_, 'a>,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<CompletionResult, RawCallError> {
        // Park BEFORE dispatch while a supervisor holds the pause gate — the
        // engines park at their own step boundaries, and this chokepoint is
        // what extends the same discipline to every management call (triage,
        // verifier, guidance, conversational). Pre-spend, never mid-call: the
        // boundary contract is the budget guard's (L-E6).
        if let Some(gate) = self.turn_gate {
            gate.wait_if_paused().await;
        }
        // Destructured, so the caller-built message list is MOVED into the
        // request instead of cloned. The conversational fast path hands this
        // the whole running transcript, so the clone was a per-call copy of
        // the entire history for no benefit — nothing reads `call` afterwards.
        let RawCall {
            role,
            resolved,
            messages,
            policy,
            overrides,
            timeout,
        } = call;
        let messages = match &overrides.prompt {
            Some(prompt) => {
                let mut with_system = Vec::with_capacity(messages.len() + 1);
                with_system.push(CompletionMessage::system(prompt.clone()));
                with_system.extend(messages);
                with_system
            }
            None => messages,
        };
        let engine = &self.config.engine;
        // The real pre-call estimate, not 0: this pairs with the provider's
        // reported usage on `StepUsage` as an estimator drift sample, and a
        // hardcoded zero recorded "estimated 0, actual N" against every
        // management call — poisoning calibration and hiding a 10k-token
        // verifier prompt from anything reading the estimate.
        let estimated_input_tokens =
            stella_core::estimator::estimate_conversation_tokens(&messages);
        let (role_cap, role_effort) = management_bounds(role);
        let req = CompletionRequest {
            messages,
            max_output_tokens: overrides
                .max_output_tokens
                .or(role_cap)
                .or(engine.max_output_tokens),
            temperature: overrides.temperature.or(engine.temperature),
            effort: overrides.effort.or(role_effort).or(engine.effort),
            reasoning: overrides.reasoning.or(engine.reasoning),
            params: overrides.params.or(engine.params),
            tools: Vec::new(),
        };
        match run_accounted_call(
            AccountedCall {
                provider: resolved.provider,
                role,
                model_hint: resolved.model_ref.model_id.clone(),
                request: req,
                retry_policy: policy,
                timeout,
                estimated_input_tokens,
                // Management roles assemble their own prompts — the role's task
                // prompt, an optional settings-supplied system override, and a
                // rendered transcript that exists nowhere else. This receipt is
                // the only record of what any of them actually sent.
                receipt: Some(ReceiptContext {
                    turn_instance: 0,
                    step: 0,
                    call_seq: self.raw_call_seq.fetch_add(1, Ordering::Relaxed),
                    // Phase 2 (#713): a management role rides the session's
                    // engine config, so it inherits the same lifecycle switch.
                    lifecycle_enabled: engine.lifecycle_enabled,
                }),
            },
            budget,
            &self.events,
            self.sleeper,
        )
        .await
        {
            Ok(result) => {
                *total += result.cost_usd;
                Ok(result)
            }
            Err(AccountedCallError::Provider(_)) => Err(RawCallError::Provider),
            Err(AccountedCallError::Timeout) => Err(RawCallError::Timeout),
            Err(AccountedCallError::Budget { result, outcome }) => {
                *total += result.cost_usd;
                Err(RawCallError::Budget(
                    budget_abort(outcome).expect("budget error carries abort outcome"),
                ))
            }
        }
    }
}
