//! I/O-free one-shot provider accounting shared by non-engine callers.

use std::time::{Duration, Instant};

use stella_protocol::{
    AgentEvent, CompletionRequest, CompletionResult, ModelCallRole, Provider, ProviderError,
    UsageIncompleteReason,
};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::timeout;

use crate::budget::{BudgetGuard, BudgetOutcome};
use crate::retry::{RetryPolicy, Sleeper, retry_with_backoff};

pub struct AccountedCall<'a> {
    pub provider: &'a dyn Provider,
    pub role: ModelCallRole,
    pub model_hint: String,
    pub request: CompletionRequest,
    pub retry_policy: RetryPolicy,
    pub timeout: Option<Duration>,
    pub estimated_input_tokens: u64,
}

pub enum AccountedCallError {
    Provider(ProviderError),
    Timeout,
    Budget {
        result: CompletionResult,
        outcome: BudgetOutcome,
    },
}

pub async fn run_accounted_call(
    call: AccountedCall<'_>,
    budget: &mut BudgetGuard,
    events: &UnboundedSender<AgentEvent>,
    sleeper: &dyn Sleeper,
) -> Result<CompletionResult, AccountedCallError> {
    let started = Instant::now();
    let future = retry_with_backoff(&call.retry_policy, sleeper, || {
        call.provider.complete(call.request.clone())
    });
    let outcome = match call.timeout {
        Some(limit) => match timeout(limit, future).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => {
                emit_incomplete(
                    &call,
                    events,
                    started.elapsed(),
                    Some(retry_count(&call, &error)),
                );
                return Err(AccountedCallError::Provider(error));
            }
            Err(_) => {
                emit_incomplete(&call, events, started.elapsed(), None);
                return Err(AccountedCallError::Timeout);
            }
        },
        None => match future.await {
            Ok(outcome) => outcome,
            Err(error) => {
                emit_incomplete(
                    &call,
                    events,
                    started.elapsed(),
                    Some(retry_count(&call, &error)),
                );
                return Err(AccountedCallError::Provider(error));
            }
        },
    };
    for attempt in &outcome.retries {
        let _ = events.send(AgentEvent::Retry {
            attempt: attempt.attempt,
            reason: attempt.reason.clone(),
        });
    }
    let result = outcome.value;
    let provider = call.provider.id();
    let _ = events.send(AgentEvent::StepUsage {
        step: 0,
        role: call.role,
        provider: provider.to_string(),
        model: result.model.clone(),
        input_tokens: result.usage.input_tokens,
        output_tokens: result.usage.output_tokens,
        cached_input_tokens: result.usage.cached_input_tokens,
        cache_write_tokens: result.usage.cache_write_tokens,
        estimated_input_tokens: call.estimated_input_tokens,
        cost_usd: result.cost_usd,
        duration_ms: started.elapsed().as_millis() as u64,
        retries: outcome.retries.len() as u32,
        tool_calls: result.tool_calls.len(),
        complete: result.usage.is_complete_for(provider),
    });
    let budget_outcome = budget.record_spend(result.cost_usd);
    let _ = events.send(AgentEvent::BudgetTick {
        spent_usd: budget.spent_usd(),
        limit_usd: budget.turn_limit_usd(),
        mode: budget.mode(),
    });
    if let BudgetOutcome::Warn {
        spent_usd,
        limit_usd,
        ..
    } = budget_outcome
    {
        let _ = events.send(AgentEvent::Error {
            message: format!(
                "budget warning: spent ${spent_usd:.4} against a ${limit_usd:.2} observed limit; continuing"
            ),
            retryable: true,
        });
    }
    if matches!(budget_outcome, BudgetOutcome::AbortTurn { .. }) {
        return Err(AccountedCallError::Budget {
            result,
            outcome: budget_outcome,
        });
    }
    Ok(result)
}

fn retry_count(call: &AccountedCall<'_>, error: &ProviderError) -> u32 {
    if error.is_retryable() {
        call.retry_policy.max_retries
    } else {
        0
    }
}

fn emit_incomplete(
    call: &AccountedCall<'_>,
    events: &UnboundedSender<AgentEvent>,
    duration: Duration,
    retries: Option<u32>,
) {
    let _ = events.send(AgentEvent::UsageIncomplete {
        role: call.role,
        provider: call.provider.id().to_string(),
        model: call.model_hint.clone(),
        reason: if retries.is_some() {
            UsageIncompleteReason::ProviderError
        } else {
            UsageIncompleteReason::Timeout
        },
        duration_ms: duration.as_millis() as u64,
        retries,
    });
}
