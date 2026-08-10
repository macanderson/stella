//! The model-call attempt ladder and its rate-limit recovery (#2677): drive
//! `retry_with_backoff_observed` for one step, and when sustained rate
//! limiting outlives the inline ladder, park the turn — chunked engine-side
//! sleeps bounded by the task deadline's real headroom — instead of aborting
//! a run that still has budget to spend (#2667 measured exactly that loss:
//! 7 attempts burned in 15s with 880s of budget left). A child module of
//! `driver` (the `settlement.rs` pattern) so the engine internals stay
//! reachable without growing `driver.rs` past the size gate.
//!
//! The decision arithmetic — when a park is affordable, how long it waits —
//! is pure and lives in [`crate::retry`] (`plan_rate_limit_park`); this
//! module owns the effects: the per-attempt usage envelopes, the
//! `TurnParked`/`TurnWoken` span and per-chunk keep-alive narration, the
//! soft-stop check that ends a multi-minute wait early, and the
//! exhausted-ladder event pair. The park honors invariant 6's spirit the
//! same way `driver/waiting.rs` does: the wait sits between model attempts,
//! never mid-tool, and the budget's deadline bounds it from the outside.

use stella_protocol::{AgentEvent, ProviderError};

use super::overflow_recovery::ModelCallFailure;
use super::{CompletionResultAlias, Engine, RetryAttemptFn, SpeculationFuture, lifecycle};
use crate::budget::BudgetGuard;
use crate::bus;
use crate::event_sender::EventSender;
use crate::retry::{ParkDirective, ParkSupervisor, RetryOutcome, retry_with_backoff_observed};
use crate::step::CancelUsageGuard;

/// Absolute ceiling on the parked waiting one model call may accumulate,
/// deadline or no deadline — six hours, matching the reset-aware extension
/// the comparator lineage allows its unattended rate-limit retries. Without
/// this, a deadline-less interactive session against a hard-down provider
/// would park forever; with it, the session narrates its waits for six hours
/// and then surfaces the rate limit.
pub(super) const MAX_PARKED_WAIT_MS: u64 = 6 * 60 * 60 * 1000;

/// Wall clock a park must leave unspent under a task deadline: waking with
/// less than this left means the retried call itself would be aborted at the
/// next safe boundary, so the wait would spend the budget and buy nothing.
const PARK_DEADLINE_RESERVE_MS: u64 = 30_000;

/// The engine's [`ParkSupervisor`]: allowance from the budget guard's task
/// deadline (capped by [`MAX_PARKED_WAIT_MS`]), the `TurnParked`/`TurnWoken`
/// span plus per-chunk keep-alive narration on the event stream, and the
/// soft-stop latch as the early exit — a park must not make Esc wait out a
/// provider brownout.
struct RateLimitPark<'e, 'a> {
    engine: &'e Engine<'a>,
    budget: &'e BudgetGuard,
    events: &'e EventSender,
}

impl ParkSupervisor for RateLimitPark<'_, '_> {
    fn wait_allowance_ms(&mut self, waited_ms: u64) -> u64 {
        let cap = MAX_PARKED_WAIT_MS.saturating_sub(waited_ms);
        match self.budget.deadline_remaining(std::time::Instant::now()) {
            Some(remaining) => {
                let remaining_ms = u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX);
                cap.min(remaining_ms.saturating_sub(PARK_DEADLINE_RESERVE_MS))
            }
            None => cap,
        }
    }

    fn park_opened(&mut self, planned_ms: u64, chunk_ms: u64, reason: &str) {
        let deadline_secs = planned_ms.div_ceil(1000);
        let _ = self.events.send(AgentEvent::TurnParked {
            description: format!("provider rate limited — backing off {deadline_secs}s ({reason})"),
            poll_interval_secs: chunk_ms / 1000,
            deadline_secs,
        });
        self.engine
            .emit_lifecycle(bus::names::AGENT_TURN_PARKED, || {
                lifecycle::turn_parked_payload(chunk_ms / 1000, deadline_secs)
            });
    }

    fn park_tick(&mut self, waited_ms: u64, planned_ms: u64) -> ParkDirective {
        if self
            .engine
            .steering
            .is_some_and(|steering| steering.soft_stop_requested())
        {
            return ParkDirective::Abort;
        }
        // The per-chunk keep-alive (#2677): a multi-minute wait announces
        // its liveness at chunk cadence so no observer of the stream reads
        // the parked turn as a dead one. Suppressed on the first tick — the
        // `TurnParked` span-open just above it already said everything this
        // would.
        if waited_ms > 0 {
            let _ = self.events.send(AgentEvent::Text {
                text: format!(
                    "\n⏳ Still rate limited — waited {}s of {}s before the next attempt.\n",
                    waited_ms / 1000,
                    planned_ms.div_ceil(1000),
                ),
            });
        }
        ParkDirective::Continue
    }

    // The waited duration already reached the wire through the keep-alive
    // ticks; the span close carries only the chunk count, mirroring
    // `driver/waiting.rs`'s poll accounting.
    fn park_closed(&mut self, _waited_ms: u64, chunks: u64, aborted: bool) {
        let reason = if aborted {
            "soft_stop"
        } else {
            "retry_window_elapsed"
        };
        let _ = self.events.send(AgentEvent::TurnWoken {
            reason: reason.to_string(),
            polls_used: chunks,
        });
        self.engine
            .emit_lifecycle(bus::names::AGENT_TURN_WOKEN, || {
                lifecycle::turn_woken_payload(reason, chunks)
            });
    }
}

impl<'a> Engine<'a> {
    /// Drive one step's attempt ladder to a committed completion or a clean
    /// failure. Owns everything between "the attempt closure exists" and
    /// "a result committed": the cancellation usage guard, the per-attempt
    /// incompleteness envelopes, the parked rate-limit recovery above, and —
    /// on exhaustion — the `RetriesExhausted` + `Error` event pair. Returns
    /// the instant the ladder started (the committed call's duration axis)
    /// alongside the outcome.
    pub(super) async fn drive_attempt_ladder<'f>(
        &self,
        attempt: RetryAttemptFn<'f>,
        attempt_in_flight: std::sync::Arc<std::sync::atomic::AtomicBool>,
        budget: &BudgetGuard,
        events: &EventSender,
    ) -> Result<
        (
            std::time::Instant,
            RetryOutcome<(CompletionResultAlias, SpeculationFuture<'f>)>,
        ),
        ModelCallFailure,
    > {
        let call_started = std::time::Instant::now();
        // Armed for exactly the interval where a paid attempt may be in
        // flight: a caller-side hard cancel that drops this future mid-await
        // still leaves one content-free `Cancelled` envelope behind.
        // Disarmed on BOTH normal exits — a success reports through its
        // `StepUsage`, and a terminal failure's attempts already reported
        // through the per-attempt observer below. The shared latch narrows
        // "in flight" to the dispatch itself: a drop landing in a backoff
        // sleep (or a parked wait) emits nothing, because the failed attempt
        // before the sleep already reported its own envelope.
        let mut cancel_guard = CancelUsageGuard {
            events: events.clone(),
            role: self.call_role,
            provider: self.provider.id().to_string(),
            started: call_started,
            armed: true,
            attempt_in_flight,
        };
        let incomplete_events = events.clone();
        // Every failed attempt's reason, accumulated through the observer:
        // retry.rs returns retry history only for calls that COMMIT, so on
        // exhaustion this is the sole record of the doomed attempts
        // (receipts spec §6.3 — RetriesExhausted). A std Mutex, never held
        // across an await and contention-free — the observer runs serially
        // within this call.
        let attempt_reasons: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let mut park = RateLimitPark {
            engine: self,
            budget,
            events,
        };
        match retry_with_backoff_observed(
            &self.config.retry_policy,
            self.sleeper,
            attempt,
            // Per-attempt duration (retry.rs times each dispatch
            // individually): the failed call's own latency, never
            // cumulative across earlier attempts or backoff sleeps.
            |attempt, error, attempt_duration| {
                attempt_reasons
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(error.to_string());
                let _ = incomplete_events.send(AgentEvent::UsageIncomplete {
                    role: self.call_role,
                    provider: self.provider.id().to_string(),
                    model: "unknown".into(),
                    reason: stella_protocol::UsageIncompleteReason::ProviderError,
                    duration_ms: attempt_duration.as_millis() as u64,
                    retries: Some(attempt.saturating_sub(1)),
                    // Whatever the adapter salvaged from the dying stream.
                    // This observer is the only place it can be read: retry.rs
                    // returns history only for calls that COMMIT, so a doomed
                    // attempt's accounting reaches the wire here or nowhere.
                    partial: error.partial_usage().copied(),
                });
            },
            &mut park,
        )
        .await
        {
            Ok(outcome) => {
                cancel_guard.disarm();
                // Call-outcome feedback (#2673): a committed step closes the
                // routing circuit breaker's window for this provider.
                if let Some(outcomes) = self.outcomes {
                    outcomes.record_success(self.provider.id());
                }
                Ok((call_started, outcome))
            }
            Err(error) => {
                cancel_guard.disarm();
                let reasons =
                    std::mem::take(&mut *attempt_reasons.lock().unwrap_or_else(|p| p.into_inner()));
                // A context overflow is withheld from the terminal events and
                // the breaker: the caller may still recover it, and an
                // oversized request is the engine's accounting miss, not
                // provider ill-health (`driver::overflow_recovery`, #2680).
                if matches!(error, ProviderError::ContextOverflow { .. }) {
                    return Err(ModelCallFailure::ContextOverflow {
                        message: error.to_string(),
                        attempt_reasons: reasons,
                    });
                }
                // Shared with the paired `Error` event below: whether the
                // FINAL attempt's error is of a retryable class. `false`
                // means every attempt (typically just one — see
                // `retry_with_backoff_observed`, which bails on the first
                // non-retryable error) was doomed from the start, not
                // exhausted by an actual retry loop (#926).
                let retryable = error.is_retryable();
                let _ = events.send(AgentEvent::RetriesExhausted {
                    turn_instance: self.config.turn_instance,
                    attempts: reasons.len() as u32,
                    reasons,
                    retryable,
                });
                let message = error.to_string();
                let _ = events.send(AgentEvent::Error {
                    message: message.clone(),
                    retryable,
                });
                // Call-outcome feedback (#2673): an exhausted ladder is the
                // signal the router's circuit breaker trips on.
                if let Some(outcomes) = self.outcomes {
                    outcomes.record_failure(self.provider.id());
                }
                Err(ModelCallFailure::Fatal {
                    reason: format!("model call failed: {message}"),
                })
            }
        }
    }
}
