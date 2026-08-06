//! I/O-free one-shot provider accounting shared by non-engine callers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use stella_protocol::{
    AgentEvent, CompletionRequest, CompletionResult, ModelCallRole, Provider, ProviderError,
    ToolCall, ToolCallObserver, UsageIncompleteReason,
};
use tokio::time::timeout;

use crate::budget::{BudgetGuard, BudgetOutcome};
use crate::event_sender::EventSender;
use crate::receipts::ReceiptLedger;
use crate::retry::{RetryPolicy, Sleeper, retry_with_backoff_observed};
use crate::step::StreamProgress;

/// Where an auxiliary call sits in the receipt coordinate space, so the context
/// it sent is reconstructable alongside the engine's own steps. `call_seq` must
/// be unique within the execution (see `AgentEvent::StepManifest::call_seq`).
///
/// `None` on an [`AccountedCall`] means "emit no receipt" — reserved for callers
/// that are not part of a recorded execution (tests, one-off tooling).
#[derive(Debug, Clone, Copy)]
pub struct ReceiptContext {
    pub turn_instance: u32,
    pub step: usize,
    pub call_seq: u64,
    /// The session's `context.lifecycle.enabled` — Phase 2 (#713). Carried per
    /// call rather than read from a global because an auxiliary call has no
    /// [`crate::EngineConfig`] of its own to inherit it from, and a management
    /// role's prompt deserves the same frame identity a worker step gets:
    /// leaving it permanently off here would make exactly the calls that are
    /// hardest to reconstruct the ones with no frame hash.
    pub lifecycle_enabled: bool,
}

/// One fully-specified provider call for [`run_accounted_call`]: what to send,
/// who to bill it to, and the reliability envelope around it. Non-engine
/// callers (the overflow summarizer, skill authoring, triage) use this instead
/// of standing up an [`crate::Engine`] for a call that has no tools, no steps,
/// and no conversation of its own.
pub struct AccountedCall<'a> {
    /// The adapter to dispatch through. Borrowed — this type never owns I/O.
    pub provider: &'a dyn Provider,
    /// Pipeline role the spend and usage are attributed to.
    pub role: ModelCallRole,
    /// Model name reported on a `UsageIncomplete` envelope, where no result
    /// exists to name the model that actually served the call. The successful
    /// path always reports `result.model` instead, so this is only a hint.
    pub model_hint: String,
    /// The completion request. Owned, because these callers build one for a
    /// single dispatch and have no transcript to borrow from; borrowed per
    /// attempt, so a retry costs nothing beyond the retry itself (#921).
    pub request: CompletionRequest,
    /// Retry + backoff envelope (`crate::retry`).
    pub retry_policy: RetryPolicy,
    /// Idle deadline over the WHOLE retry future, backoff sleeps included —
    /// measured as time since the last streamed fragment
    /// (`stella_protocol::ToolCallObserver`'s text/reasoning/tool-call/tool-
    /// argument ticks), not since dispatch started, mirroring
    /// `crate::step::bounded_generation`'s idle bound for the engine's own
    /// step loop. A call that is actively answering is never cut off merely
    /// for taking a while; only a window that passes in complete silence
    /// trips it. That is what stops a provider whose usage/cost accounting
    /// trails the content in one final frame — OpenRouter's `usage.include`
    /// gateway accounting — from losing that frame to a ceiling sized for
    /// "is anyone home" rather than "is the model still talking" (#1467).
    /// `None` leaves the call bounded only by `retry_policy`.
    pub timeout: Option<Duration>,
    /// The caller's pre-call token estimate, paired with the provider's
    /// reported usage on `StepUsage` as a drift sample (`crate::estimator`).
    pub estimated_input_tokens: u64,
    /// Receipt coordinates for this call. `Some` makes the exact context it
    /// sent reconstructable after the fact; `None` records cost only.
    pub receipt: Option<ReceiptContext>,
}

/// Why an accounted call did not return a usable result. `Budget` is
/// deliberately not a plain failure: the completion COMMITTED and was paid
/// for, so it carries the result through for a caller that can still use it
/// (the overflow summarizer splices its summary in either way).
///
/// `Debug`/`Display`/`Error` so this behaves like every other error in the
/// workspace: it can be `?`-propagated into a boxed error, logged, and
/// `unwrap`ed in a test. The `Display` messages are content-free by
/// construction — `Budget` names the axis-free fact, never the completion
/// text it carries — so `{}` (and `{:#}` through a boxed `Error`) can never
/// leak a response body. `Debug` is NOT content-free: `Budget` derives it
/// over the whole `CompletionResult`, response text included, so `{:?}` and
/// `unwrap()`'s panic message belong in tests, never in a log line.
#[derive(Debug, thiserror::Error)]
pub enum AccountedCallError {
    /// The provider failed terminally, or retries were exhausted.
    #[error("provider call failed: {0}")]
    Provider(ProviderError),
    /// [`AccountedCall::timeout`] expired before the call resolved.
    #[error("the call's deadline expired before the provider responded")]
    Timeout,
    /// The call succeeded, but settling its cost breached an enforced budget.
    #[error("the call committed, but settling its cost breached an enforced budget")]
    Budget {
        /// The committed, already-paid-for result.
        result: CompletionResult,
        /// The breaching outcome — always `BudgetOutcome::AbortTurn`.
        outcome: BudgetOutcome,
    },
}

/// Dispatch one provider call with retry, per-call timeout, budget metering,
/// and the full accounting event trail (`UsageIncomplete` per failed attempt,
/// `Retry` per committed retry, then `StepUsage` + `BudgetTick`).
///
/// Every failed attempt reports its own content-free `UsageIncomplete`
/// envelope synchronously, before a later attempt can succeed: a successful
/// retry can report its own usage but can never make an earlier attempt's
/// unknown usage knowable after the fact. A deadline that expires during a
/// backoff sleep — with no paid dispatch in flight — deliberately emits no
/// `Timeout` envelope, since the preceding attempt already accounted for
/// itself.
///
/// [`AccountedCall::timeout`] is an IDLE deadline, not a wall clock: it is
/// measured since the last streamed fragment, so it re-arms every time the
/// dispatch is observed to still be producing — the same distinction
/// `crate::step::bounded_generation` draws for the engine's own step loop.
/// A flat wall-clock deadline here used to abandon a call the instant total
/// elapsed time crossed the ceiling even while the provider was actively
/// answering, which lost OpenRouter's trailing usage/cost frame (it arrives
/// in a final SSE frame *after* the content, once the gateway has settled
/// the routed call's price) to a ceiling sized to catch silence, not a
/// slow-but-live generation (#1467).
pub async fn run_accounted_call(
    call: AccountedCall<'_>,
    budget: &mut BudgetGuard,
    events: &EventSender,
    sleeper: &dyn Sleeper,
) -> Result<CompletionResult, AccountedCallError> {
    let started = Instant::now();
    // True only while a provider dispatch is actually being polled — cleared
    // for the backoff sleeps between attempts. A per-call timeout wraps the
    // whole retry future (sleeps included), so its expiry must not attribute a
    // Timeout envelope to a moment when nothing was in flight: the attempt that
    // preceded a sleep already reported its own per-attempt envelope.
    let attempt_in_flight = AtomicBool::new(false);
    // The idle clock backing `call.timeout` below. Shared (via `Clone`, an
    // `Arc` count) across every attempt a retry makes: a fresh attempt must
    // not be penalized for a stale "last seen" left over from a sibling
    // attempt's own silence, and comparing the count at the start of a check
    // window against the count at its end already gives each window its own
    // delta regardless of how many attempts ran inside it.
    let progress = StreamProgress::default();
    let future = retry_with_backoff_observed(
        &call.retry_policy,
        sleeper,
        || {
            // Borrowed, not cloned: this closure is `FnMut` for the same reason
            // the driver's is, and used to pay a full deep copy of the request
            // on every attempt — including the overflow summarizer's, whose
            // input is a rendered span of the transcript (#921).
            let request = call.request.as_borrowed();
            let provider = call.provider;
            let attempt_progress = progress.clone();
            let in_flight = &attempt_in_flight;
            async move {
                // Built *inside* the async block, not before it: the future
                // `complete_observed_ref` returns borrows this observer for
                // the dispatch's whole lifetime, so it must be owned by the
                // same generator state that future lives in (the shape
                // `crate::driver`'s `SpeculationGate` construction uses for
                // the same reason).
                let observer = IdleObserver(attempt_progress);
                in_flight.store(true, Ordering::SeqCst);
                let result = provider.complete_observed_ref(request, &observer).await;
                in_flight.store(false, Ordering::SeqCst);
                result
            }
        },
        // Per-attempt duration (retry.rs times each dispatch individually):
        // the failed call's own latency, never cumulative across attempts.
        |attempt, error, attempt_duration| {
            emit_incomplete(
                &call,
                events,
                attempt_duration,
                Some(attempt.saturating_sub(1)),
                error.partial_usage().copied(),
            );
        },
    );
    let outcome = match call.timeout {
        Some(limit) => {
            let mut future = std::pin::pin!(future);
            let mut seen = progress.count();
            loop {
                match timeout(limit, &mut future).await {
                    Ok(Ok(outcome)) => break outcome,
                    Ok(Err(error)) => {
                        return Err(AccountedCallError::Provider(error));
                    }
                    Err(_) => {
                        // The window elapsed with the call still unresolved.
                        // Whether that is the deadline this bound exists for
                        // depends on what arrived during it: any fragment at
                        // all — including one from an attempt that has since
                        // failed and moved into backoff — means something is
                        // still happening, so re-arm and keep waiting. Only a
                        // window that passed in complete silence is the wedge
                        // (or genuinely-too-slow decision) this ceiling is for.
                        let now = progress.count();
                        if now == seen {
                            // Attribute a Timeout envelope only if a paid
                            // attempt was genuinely in flight — an expiry
                            // during a backoff sleep would double-report a
                            // failure the per-attempt observer already
                            // accounted for.
                            if attempt_in_flight.load(Ordering::SeqCst) {
                                // A deadline fires from outside the adapter,
                                // so nothing was salvaged: the stream is
                                // still open and its usage frame may yet
                                // have been in flight.
                                emit_incomplete(&call, events, started.elapsed(), None, None);
                            }
                            return Err(AccountedCallError::Timeout);
                        }
                        seen = now;
                    }
                }
            }
        }
        None => match future.await {
            Ok(outcome) => outcome,
            Err(error) => return Err(AccountedCallError::Provider(error)),
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
    // The context receipt for this call, emitted just before `StepUsage` so the
    // pair — what the model saw, what it cost — lands together at the settled
    // boundary, exactly as the engine's step loop does it. Every role routed
    // through here (the overflow summarizer, and the pipeline's triage / verifier /
    // plan / guidance / conversational roles) is otherwise reconstructable only
    // as a cost line: without this the prompt it actually sent is unrecoverable.
    // A fresh ledger per call is correct — these are one-shot contexts, so every
    // block is first-seen and registers with its bytes.
    // The step this call rides, for both the receipt and the usage event
    // below: consumers pair the two by step, so they must agree. A
    // receipt-less call has no step loop to key against and stays at 0.
    let step = call.receipt.as_ref().map_or(0, |receipt| receipt.step);
    if let Some(receipt) = call.receipt {
        // One-shot context: no prior estimate exists for these messages, so the
        // ledger computes it. The step loop passes its own in instead.
        ReceiptLedger::with_call_seq(receipt.turn_instance, receipt.call_seq)
            .with_lifecycle(receipt.lifecycle_enabled)
            .emit_step_receipt_estimating(
                &call.request.messages,
                receipt.step,
                crate::receipts::ServedBy {
                    role: call.role,
                    provider,
                    model: &result.model,
                },
                events,
            );
    }
    let _ = events.send(AgentEvent::StepUsage {
        step,
        role: call.role,
        provider: provider.to_string(),
        // Every role routed through here is a management or compaction call —
        // none emit a separate `Text` event, so this is the only durable record
        // of what the model actually said (the bench harness's ATIF audit trail
        // reads it). Execute calls take the engine path and leave this `None`.
        output_text: Some(result.text.clone()),
        model: result.model.clone(),
        input_tokens: result.usage.input_tokens,
        output_tokens: result.usage.output_tokens,
        cached_input_tokens: result.usage.cached_input_tokens,
        cache_write_tokens: result.usage.cache_write_tokens,
        reasoning_tokens: result.usage.reasoning_tokens,
        estimated_input_tokens: call.estimated_input_tokens,
        cost_usd: result.cost_usd,
        duration_ms: started.elapsed().as_millis() as u64,
        retries: outcome.retries.len() as u32,
        tool_calls: result.tool_calls.len(),
        complete: result.usage.is_complete(),
        // Management calls truncate too — a verifier verdict or a plan cut off at
        // the ceiling is exactly the silent failure this field exists to name.
        finish_reason: result.finish_reason,
    });
    let budget_outcome = budget.record_spend(result.cost_usd);
    let _ = events.send(AgentEvent::BudgetTick {
        spent_usd: budget.spent_usd(),
        limit_usd: budget.turn_limit_usd(),
        mode: budget.mode(),
        session_spent_usd: Some(budget.session_spent_usd()),
        session_limit_usd: budget.session_limit_usd(),
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

fn emit_incomplete(
    call: &AccountedCall<'_>,
    events: &EventSender,
    duration: Duration,
    retries: Option<u32>,
    partial: Option<stella_protocol::PartialUsage>,
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
        partial,
    });
}

/// Ticks [`StreamProgress`] for [`run_accounted_call`]'s idle deadline — the
/// minimal [`ToolCallObserver`] a raw one-shot call needs. Unlike
/// `crate::speculation::SpeculationGate`, this observer does nothing with
/// what it sees: an `AccountedCall` never carries tools (`tools: Vec::new()`
/// at every call site — triage, verifier, plan, guidance, the overflow
/// summarizer), so there is no speculative execution to gate and no preview
/// event to forward. Every method exists only to record that *something*
/// arrived.
struct IdleObserver(StreamProgress);

impl ToolCallObserver for IdleObserver {
    fn tool_call_streamed(&self, _call: &ToolCall) {
        self.0.record();
    }

    fn text_delta(&self, _delta: &str) {
        self.0.record();
    }

    fn reasoning_delta(&self, _delta: &str) {
        self.0.record();
    }

    fn tool_input_delta(&self) {
        self.0.record();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use stella_protocol::{BudgetMode, CompletionMessage, CompletionRequestRef, CompletionUsage};

    use super::*;

    struct NoopSleeper;

    #[async_trait]
    impl Sleeper for NoopSleeper {
        async fn sleep(&self, _duration_ms: u64) {}
    }

    struct RetryThenSuccess {
        attempts: Mutex<u32>,
    }

    #[async_trait]
    impl Provider for RetryThenSuccess {
        fn id(&self) -> &str {
            "scripted"
        }

        async fn complete_ref(
            &self,
            _request: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            let mut attempts = self.attempts.lock().expect("attempt lock");
            *attempts += 1;
            if *attempts == 1 {
                return Err(ProviderError::transport("private failed body"));
            }
            Ok(CompletionResult {
                text: "done".into(),
                tool_calls: Vec::new(),
                usage: CompletionUsage::reported_zero(),
                model: "scripted-model".into(),
                cost_usd: 0.25,
                finish_reason: None,
            })
        }
    }

    #[tokio::test]
    async fn successful_retry_preserves_failed_attempt_incompleteness_and_known_cost() {
        let provider = RetryThenSuccess {
            attempts: Mutex::new(0),
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        let result = match run_accounted_call(
            AccountedCall {
                provider: &provider,
                role: ModelCallRole::SkillAuthor,
                model_hint: "configured-model".into(),
                request: CompletionRequest {
                    messages: vec![CompletionMessage::user("work")],
                    max_output_tokens: None,
                    temperature: None,
                    effort: None,
                    tools: Vec::new(),
                    reasoning: None,
                    params: None,
                },
                retry_policy: RetryPolicy::new(1, 0, 0),
                timeout: None,
                estimated_input_tokens: 1,
                receipt: None,
            },
            &mut budget,
            &EventSender::new(tx),
            &NoopSleeper,
        )
        .await
        {
            Ok(result) => result,
            Err(_) => panic!("retry should succeed"),
        };

        assert_eq!(result.cost_usd, 0.25);
        assert_eq!(budget.spent_usd(), 0.25);
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let incomplete: Vec<_> = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::UsageIncomplete { .. }))
            .collect();
        assert_eq!(incomplete.len(), 1);
        assert!(matches!(
            incomplete[0],
            AgentEvent::UsageIncomplete {
                role: ModelCallRole::SkillAuthor,
                retries: Some(0),
                ..
            }
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::StepUsage {
                role: ModelCallRole::SkillAuthor,
                cost_usd,
                retries: 1,
                complete: true,
                ..
            } if (*cost_usd - 0.25).abs() < f64::EPSILON
        )));
        assert!(
            !serde_json::to_string(&incomplete)
                .expect("wire")
                .contains("private failed body")
        );
    }

    struct Succeeds;

    #[async_trait]
    impl Provider for Succeeds {
        fn id(&self) -> &str {
            "scripted"
        }

        async fn complete_ref(
            &self,
            _request: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            Ok(CompletionResult {
                text: "summary".into(),
                tool_calls: Vec::new(),
                usage: CompletionUsage::reported_zero(),
                model: "scripted-model".into(),
                cost_usd: 0.01,
                finish_reason: None,
            })
        }
    }

    /// The gap this closes: every role routed through here — the overflow
    /// summarizer, the pipeline's triage/verifier/plan/guidance — used to leave a
    /// cost line and nothing else, so the prompt it sent was unrecoverable.
    /// A receipt context makes its system prefix a registered block carrying
    /// its own bytes, keyed apart from the worker call by `call_seq`.
    #[tokio::test]
    async fn a_receipt_context_makes_an_auxiliary_call_reconstructable() {
        let provider = Succeeds;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        let _ = run_accounted_call(
            AccountedCall {
                provider: &provider,
                role: ModelCallRole::Summarization,
                model_hint: "configured-model".into(),
                request: CompletionRequest {
                    messages: vec![
                        CompletionMessage::system("condense this span faithfully"),
                        CompletionMessage::user("t0 t1 t2"),
                    ],
                    max_output_tokens: None,
                    temperature: None,
                    effort: None,
                    tools: Vec::new(),
                    reasoning: None,
                    params: None,
                },
                retry_policy: RetryPolicy::new(1, 0, 0),
                timeout: None,
                estimated_input_tokens: 1,
                receipt: Some(ReceiptContext {
                    turn_instance: 4,
                    step: 7,
                    call_seq: crate::receipts::RECEIPT_SEQ_SUMMARIZER,
                    lifecycle_enabled: false,
                }),
            },
            &mut budget,
            &EventSender::new(tx),
            &NoopSleeper,
        )
        .await;

        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        // The system prefix registers with its bytes — that is the whole point.
        let carries_prompt = events.iter().any(|event| {
            matches!(
                event,
                AgentEvent::BlockRegistered { content: Some(c), .. }
                    if c == "condense this span faithfully"
            )
        });
        assert!(
            carries_prompt,
            "the summarizer's own system prompt must be a registered block: {events:?}"
        );
        // The manifest is keyed at the auxiliary seat, not over the worker's.
        let manifest = events
            .iter()
            .find(|event| matches!(event, AgentEvent::StepManifest { .. }))
            .expect("a manifest is emitted");
        assert!(matches!(
            manifest,
            AgentEvent::StepManifest {
                turn_instance: 4,
                step: 7,
                call_seq: 1,
                role: ModelCallRole::Summarization,
                ..
            }
        ));
        // Receipt precedes StepUsage, matching the engine's ordering.
        let manifest_at = events
            .iter()
            .position(|e| matches!(e, AgentEvent::StepManifest { .. }))
            .expect("manifest");
        let usage_at = events
            .iter()
            .position(|e| matches!(e, AgentEvent::StepUsage { .. }))
            .expect("usage");
        assert!(
            manifest_at < usage_at,
            "receipt lands with, and before, usage"
        );
    }

    #[tokio::test]
    async fn no_receipt_context_emits_no_manifest() {
        let provider = Succeeds;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        let _ = run_accounted_call(
            AccountedCall {
                provider: &provider,
                role: ModelCallRole::SkillAuthor,
                model_hint: "configured-model".into(),
                request: CompletionRequest {
                    messages: vec![CompletionMessage::system("secret standalone prompt")],
                    max_output_tokens: None,
                    temperature: None,
                    effort: None,
                    tools: Vec::new(),
                    reasoning: None,
                    params: None,
                },
                retry_policy: RetryPolicy::new(1, 0, 0),
                timeout: None,
                estimated_input_tokens: 1,
                receipt: None,
            },
            &mut budget,
            &EventSender::new(tx),
            &NoopSleeper,
        )
        .await;

        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            !events.iter().any(|e| matches!(
                e,
                AgentEvent::StepManifest { .. } | AgentEvent::BlockRegistered { .. }
            )),
            "an opt-out caller records cost only, and never its prompt bytes"
        );
    }

    /// A [`Sleeper`] backed by real (here, paused-virtual) tokio time so a
    /// caller-supplied per-call timeout can expire *during* a backoff sleep.
    struct TokioSleeper;

    #[async_trait]
    impl Sleeper for TokioSleeper {
        async fn sleep(&self, duration_ms: u64) {
            tokio::time::sleep(Duration::from_millis(duration_ms)).await;
        }
    }

    struct AlwaysRetryable;

    #[async_trait]
    impl Provider for AlwaysRetryable {
        fn id(&self) -> &str {
            "scripted"
        }

        async fn complete_ref(
            &self,
            _request: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            Err(ProviderError::transport("private failed body"))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_during_backoff_does_not_emit_a_spurious_timeout_envelope() {
        // Deadline (100ms) shorter than the first backoff floor (250ms): the
        // per-call timeout expires while the retry loop is sleeping between
        // attempts, not while a paid dispatch is in flight.
        let provider = AlwaysRetryable;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        let result = run_accounted_call(
            AccountedCall {
                provider: &provider,
                role: ModelCallRole::SkillAuthor,
                model_hint: "configured-model".into(),
                request: CompletionRequest {
                    messages: vec![CompletionMessage::user("work")],
                    max_output_tokens: None,
                    temperature: None,
                    effort: None,
                    tools: Vec::new(),
                    reasoning: None,
                    params: None,
                },
                retry_policy: RetryPolicy::new(3, 250, 250),
                timeout: Some(Duration::from_millis(100)),
                estimated_input_tokens: 1,
                receipt: None,
            },
            &mut budget,
            &EventSender::new(tx),
            &TokioSleeper,
        )
        .await;

        assert!(matches!(result, Err(AccountedCallError::Timeout)));
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let incomplete: Vec<_> = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::UsageIncomplete { .. }))
            .collect();
        // The one failed attempt reported its own per-attempt `ProviderError`
        // envelope; no `Timeout` envelope may follow, because the deadline fired
        // mid-backoff with nothing in flight (that would double-report the
        // already-accounted failure).
        assert_eq!(
            incomplete.len(),
            1,
            "exactly the single failed attempt's envelope, no spurious timeout: {incomplete:?}"
        );
        assert!(
            incomplete.iter().all(|event| matches!(
                event,
                AgentEvent::UsageIncomplete {
                    reason: UsageIncompleteReason::ProviderError,
                    ..
                }
            )),
            "the deadline fired during a backoff sleep, so no Timeout envelope is owed: {incomplete:?}"
        );
    }

    /// A provider whose content streams immediately but whose result only
    /// resolves after `trailing_delay` — the shape of OpenRouter's
    /// `usage.include` gateway accounting, reported in one final SSE frame
    /// after the content, once the gateway has settled the routed call's
    /// price.
    struct DelayedUsageFrame {
        trailing_delay: Duration,
    }

    #[async_trait]
    impl Provider for DelayedUsageFrame {
        fn id(&self) -> &str {
            "openrouter"
        }

        async fn complete_ref(
            &self,
            _request: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            panic!("this test dispatches through complete_observed_ref, not complete_ref");
        }

        async fn complete_observed_ref(
            &self,
            _request: CompletionRequestRef<'_>,
            observer: &dyn ToolCallObserver,
        ) -> Result<CompletionResult, ProviderError> {
            // The answer streams right away...
            observer.text_delta("SIMPLE_LOOKUP");
            // ...but the gateway's own usage/cost accounting trails it,
            // arriving only after this gap.
            tokio::time::sleep(self.trailing_delay).await;
            Ok(CompletionResult {
                text: "SIMPLE_LOOKUP".into(),
                tool_calls: Vec::new(),
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 120,
                    output_tokens: 8,
                    cached_input_tokens: 0,
                    cache_write_tokens: 0,
                    // `None`, not `Some(0)`: this fake gateway reports a usage
                    // envelope with no reasoning breakdown in it, which is the
                    // "not reported" fact — the distinction this field exists
                    // to keep.
                    reasoning_tokens: None,
                },
                model: "z-ai/glm-5.2".into(),
                cost_usd: 0.0042,
                finish_reason: None,
            })
        }
    }

    /// Witness for #1467: OpenRouter's usage/cost accounting lands in one
    /// final SSE frame that can arrive after the content is done. The
    /// content here streams at t=0 (one `text_delta` tick), then the call
    /// goes silent for 30ms before resolving — 30ms exceeds the 20ms
    /// per-call ceiling, so a flat wall-clock deadline (measured from
    /// dispatch start, oblivious to the tick) abandons the call and loses
    /// its usage. An idle deadline (measured from the last fragment) re-arms
    /// on the tick and lets the 30ms trailing wait land.
    #[tokio::test(start_paused = true)]
    async fn a_call_that_streams_before_a_delayed_resolution_keeps_its_usage() {
        let provider = DelayedUsageFrame {
            trailing_delay: Duration::from_millis(30),
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        let result = run_accounted_call(
            AccountedCall {
                provider: &provider,
                role: ModelCallRole::Triage,
                model_hint: "z-ai/glm-5.2".into(),
                request: CompletionRequest {
                    messages: vec![CompletionMessage::user("inspect the repository")],
                    max_output_tokens: None,
                    temperature: None,
                    effort: None,
                    tools: Vec::new(),
                    reasoning: None,
                    params: None,
                },
                retry_policy: RetryPolicy::new(1, 0, 0),
                timeout: Some(Duration::from_millis(20)),
                estimated_input_tokens: 12,
                receipt: None,
            },
            &mut budget,
            &EventSender::new(tx),
            &NoopSleeper,
        )
        .await
        .expect("the trailing gap must not abandon a call that was actively answering");

        assert_eq!(result.cost_usd, 0.0042);
        assert!(result.usage.is_complete());

        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::StepUsage { complete: true, .. })),
            "the delayed usage frame must still land as a complete StepUsage: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::UsageIncomplete { .. })),
            "no accounting should be abandoned when the call was actively answering: {events:?}"
        );
    }
}
