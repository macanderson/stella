//! Complete per-call accounting for pipeline roles that call providers directly.

use std::sync::atomic::Ordering;
use std::time::Duration;

use stella_core::retry::RetryPolicy;
use stella_core::{
    AccountedCall, AccountedCallError, BudgetGuard, ReceiptContext, run_accounted_call,
};
use stella_protocol::{CompletionMessage, CompletionRequest, CompletionResult, ModelCallRole};

use super::stage_budget::{PipelineBudgetAbort, budget_abort};
use super::{Pipeline, ResolvedRole, RoleCallOverrides};

pub(super) struct RawCall<'r, 'a> {
    pub(super) role: ModelCallRole,
    pub(super) resolved: &'r ResolvedRole<'a>,
    pub(super) messages: Vec<CompletionMessage>,
    pub(super) policy: RetryPolicy,
    pub(super) overrides: &'r RoleCallOverrides,
    pub(super) timeout: Option<Duration>,
}

pub(super) enum RawCallError {
    Provider,
    Timeout,
    Budget(PipelineBudgetAbort),
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
        // judge, guidance, conversational). Pre-spend, never mid-call: the
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
        let req = CompletionRequest {
            messages,
            max_output_tokens: overrides.max_output_tokens.or(engine.max_output_tokens),
            temperature: overrides.temperature.or(engine.temperature),
            effort: overrides.effort.or(engine.effort),
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
                estimated_input_tokens: 0,
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
