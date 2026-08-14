// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Reactive recovery from provider context-overflow rejections (#2680).
//!
//! Stella's context management is pre-emptive: compaction runs off the
//! (usage-anchored, #2681) estimate before every model call. But the anchor
//! prices only the tail appended since the last provider report, so
//! estimation error survives — and when it undershoots, the provider rejects
//! the request as too large. Before this module, that rejection surfaced as
//! `ProviderError::Terminal` and aborted the turn; the pre-emptive pass never
//! got a second opinion.
//!
//! This is the reactive rung layered on top: when a call fails with
//! [`stella_protocol::ProviderError::ContextOverflow`], the engine clamps the
//! next compaction budget *below* the estimated size of the transcript the
//! provider just rejected and re-runs the step. The existing compaction
//! machinery does all the work — the pure retention/dedup/aging passes fire
//! against the clamped budget, and the overflow summarizer fires after them
//! exactly as configured — so there is no parallel recovery path to keep
//! deterministic and no second transcript-repair implementation. The
//! transcript stays well-paired because the failed call appended nothing.
//!
//! # Bounds and the death-spiral guard
//!
//! Recovery is a **per-turn down-only latch**: at most
//! [`MAX_RECOVERY_RUNGS`] rungs fire per turn, the rung counter never resets
//! within the turn, and each rung's clamp is monotonically tighter than the
//! last. The clamp also never loosens once set — even after a later call
//! commits — so a compaction budget configured above what the provider will
//! actually accept cannot oscillate grow → overflow → recover for the rest of
//! the turn. When the ladder is spent, the overflow surfaces exactly as an
//! unrecovered terminal failure does. Error → recover → error can therefore
//! burn at most `MAX_RECOVERY_RUNGS` extra calls, each of which was already
//! billed through the ordinary `UsageIncomplete` attempt observer.
//!
//! # What consumers see
//!
//! While a recovery rung is armed, the overflow is *withheld* from the
//! terminal channels — no `RetriesExhausted`, no `Error { retryable: false }`
//! — so an observer does not tear down a session that is mid-recovery. It is
//! announced as `Error { retryable: true }` (the same notice channel the
//! overflow summarizer's failures use) plus the `model.request.failed`
//! lifecycle signal, and the eventual compaction reports itself through the
//! ordinary `Compaction` event. Only when the ladder is spent do the terminal
//! events fire, carrying every failed attempt's reason.
//!
//! Like `length_continuations`, the latch is deliberately not checkpointed: a
//! resumed turn starts the allowance over, which only re-permits a bounded
//! amount of work.

use stella_protocol::AgentEvent;

use super::{Engine, bus};
use crate::event_sender::EventSender;
use crate::step::{AbortKind, StepOutcome, TurnState};

/// How many overflow-recovery rungs may fire per turn. Two, matching the
/// ladder's two distinct answers: rung 1 clamps enough for the granular
/// passes to drain staged discards, rung 2 clamps hard enough that the
/// overflow summarizer (when configured) must fold a span. A third identical
/// rejection means the transcript's protected content itself exceeds the
/// window, which compaction cannot fix.
pub(crate) const MAX_RECOVERY_RUNGS: u8 = 2;

/// Per-turn latch and budget clamp for reactive overflow recovery. Pure data,
/// no I/O (invariant 2); lives on [`TurnState`] and dies with the turn.
#[derive(Debug, Default)]
pub(crate) struct OverflowRecovery {
    /// Rungs fired this turn. Monotone — never reset, even by a committed
    /// call, so the ladder cannot re-arm (the death-spiral guard above).
    rungs_fired: u8,
    /// The standing budget ceiling, once any rung has fired. Monotone
    /// downward for the rest of the turn.
    clamp_tokens: Option<u64>,
}

impl OverflowRecovery {
    /// Tighten a settled `(budget, factor)` pair by the standing clamp. The
    /// identity until a rung fires.
    pub(crate) fn clamped(&self, sized: (u64, f64)) -> (u64, f64) {
        match self.clamp_tokens {
            Some(clamp) => (sized.0.min(clamp), sized.1),
            None => sized,
        }
    }

    /// Arm the next rung against a transcript the provider just rejected,
    /// whose whole-conversation estimate is `estimated_tokens`. Returns the
    /// new clamp, or `None` when the ladder is spent.
    ///
    /// The clamp is set relative to the rejected transcript's own estimate —
    /// not the configured budget, which the estimate already passed — so the
    /// forced pass is guaranteed to see the transcript as over budget: rung 1
    /// demands a quarter of the estimated weight go, rung 2 half. Floored at
    /// one token so a degenerate estimate can never clamp the budget to zero.
    fn arm(&mut self, estimated_tokens: u64) -> Option<u64> {
        if self.rungs_fired >= MAX_RECOVERY_RUNGS {
            return None;
        }
        self.rungs_fired += 1;
        let target = match self.rungs_fired {
            1 => estimated_tokens.saturating_mul(3) / 4,
            _ => estimated_tokens / 2,
        };
        let clamp = target.min(self.clamp_tokens.unwrap_or(u64::MAX)).max(1);
        self.clamp_tokens = Some(clamp);
        Some(clamp)
    }

    /// Rungs fired so far — the `n` in the "recovery n of MAX" notice.
    fn fired(&self) -> u8 {
        self.rungs_fired
    }
}

/// Why `Engine::run_model_call` could not commit a step. Split by what the
/// caller may still do about it, which is the branch
/// [`Engine::settle_model_call_failure`] takes.
pub(crate) enum ModelCallFailure {
    /// The retry ladder exhausted against the active provider — transport,
    /// 5xx, rate limiting that outlived the parked recovery, or a
    /// first-attempt bail on a non-retryable class (auth, malformed). The
    /// ladder (`driver/rate_limit.rs`) already fed the provider-outcomes
    /// breaker; the terminal events are withheld — the caller decides
    /// between a mid-turn provider fallback (`driver::model_fallback`,
    /// #2679) and the terminal surfacing, and emits accordingly.
    Exhausted {
        /// The final attempt's classified error rendering.
        message: String,
        /// Every failed attempt's reason, in order — the `RetriesExhausted`
        /// payload if this surfaces terminally.
        attempt_reasons: Vec<String>,
        /// Whether the final attempt's error class was retryable, forwarded
        /// onto the terminal events exactly as before (#926).
        retryable: bool,
    },
    /// The provider rejected the request as exceeding its context window.
    /// Every event is withheld (module docs) — the caller decides between a
    /// recovery rung and the terminal surfacing, and emits accordingly.
    ContextOverflow {
        /// The classified error's rendering, `provider context overflow: …`.
        message: String,
        /// Every failed attempt's reason, in order — the `RetriesExhausted`
        /// payload if this ends up surfacing terminally.
        attempt_reasons: Vec<String>,
    },
    /// The provider refused to fund the requested output ceiling. Withheld
    /// from the terminal channels exactly as the overflow arm is — the
    /// caller decides between a clamp rung
    /// (`super::output_budget_recovery`) and the terminal surfacing.
    OutputBudgetExceeded {
        /// The classified error's rendering, `provider output budget …`.
        message: String,
        /// Every failed attempt's reason, in order — the `RetriesExhausted`
        /// payload if this ends up surfacing terminally.
        attempt_reasons: Vec<String>,
        /// The ceiling the provider said it could afford, when it named one.
        affordable_output_tokens: Option<u32>,
    },
}

impl<'a> Engine<'a> {
    /// Settle a model call that could not commit: arm the next
    /// overflow-recovery rung, or latch a mid-turn provider fallback
    /// (`driver::model_fallback`, #2679) — either returns `None` so the
    /// caller re-runs the step — or produce the turn's abort.
    ///
    /// On the recovery path the step index still advances: the retried call
    /// is a new step, so its compaction pass (and a possible overflow-
    /// summarizer receipt at the reserved per-step seat) can never collide
    /// with receipts the failed step already emitted.
    pub(crate) fn settle_model_call_failure(
        &self,
        failure: ModelCallFailure,
        state: &mut TurnState,
        events: &EventSender,
    ) -> Option<StepOutcome> {
        let (message, attempt_reasons) = match failure {
            ModelCallFailure::Exhausted {
                message,
                attempt_reasons,
                retryable,
            } => {
                // The abort string keeps its pre-#2679 bytes: consumers and
                // transcripts read this exact rendering.
                let reason = format!("model call failed: {message}");
                self.emit_lifecycle(
                    bus::names::MODEL_REQUEST_FAILED,
                    || serde_json::json!({ "step": state.step, "reason": reason }),
                );
                // Mid-turn provider fallback (#2679): re-resolve through the
                // router port and continue the turn on the replacement.
                // `true` latched the swap — the step re-runs and the
                // terminal events stay withheld, exactly as an armed
                // overflow rung withholds them below.
                if self.attempt_provider_fallback(&message, state, events) {
                    return None;
                }
                // No fallback: surface the pre-#2679 terminal shape.
                let _ = events.send(AgentEvent::RetriesExhausted {
                    turn_instance: self.config.turn_instance,
                    attempts: attempt_reasons.len() as u32,
                    reasons: attempt_reasons,
                    retryable,
                });
                let _ = events.send(AgentEvent::Error {
                    message: message.clone(),
                    retryable,
                });
                return Some(StepOutcome::Aborted {
                    reason,
                    kind: AbortKind::Failure,
                    cost_usd: state.total_cost_usd,
                });
            }
            ModelCallFailure::ContextOverflow {
                message,
                attempt_reasons,
            } => (message, attempt_reasons),
            // The output-side mirror, settled here rather than in a parallel
            // method so both ladders share one "withheld while recovering,
            // terminal when spent" shape — the property that matters to an
            // observer is that it cannot tell a spent ladder from a failure
            // that never had one.
            ModelCallFailure::OutputBudgetExceeded {
                message,
                attempt_reasons,
                affordable_output_tokens,
            } => {
                self.emit_lifecycle(
                    bus::names::MODEL_REQUEST_FAILED,
                    || serde_json::json!({ "step": state.step, "reason": message }),
                );
                let asked = state
                    .output_budget_recovery
                    .apply(self.config.max_output_tokens);
                return match state
                    .output_budget_recovery
                    .arm(affordable_output_tokens, asked)
                {
                    Some(clamp) => {
                        let _ = events.send(AgentEvent::Error {
                            message: format!(
                                "{message} — asking for at most {clamp} output tokens and \
                                 re-issuing (output budget recovery {n} of {max})",
                                n = state.output_budget_recovery.fired(),
                                max = super::output_budget_recovery::MAX_RECOVERY_RUNGS,
                            ),
                            retryable: true,
                        });
                        state.step += 1;
                        None
                    }
                    None => Some(self.surface_unrecovered(message, attempt_reasons, state, events)),
                };
            }
        };
        self.emit_lifecycle(
            bus::names::MODEL_REQUEST_FAILED,
            || serde_json::json!({ "step": state.step, "reason": message }),
        );
        // The whole-conversation estimate, attachments included — the same
        // measure the compaction pass compares against its budget, so the
        // clamp derived from it is guaranteed to read as over budget there.
        let estimated = crate::estimator::estimate_conversation_tokens(&state.messages);
        match state.overflow_recovery.arm(estimated) {
            Some(clamp) => {
                // Withheld from the terminal channels while recovering
                // (module docs); this is the same retryable-notice shape the
                // overflow summarizer's own failures use.
                let _ = events.send(AgentEvent::Error {
                    message: format!(
                        "{message} — forcing compaction to ~{clamp} estimated tokens and \
                         re-issuing (overflow recovery {n} of {MAX_RECOVERY_RUNGS})",
                        n = state.overflow_recovery.fired(),
                    ),
                    retryable: true,
                });
                state.step += 1;
                None
            }
            None => Some(self.surface_unrecovered(message, attempt_reasons, state, events)),
        }
    }

    /// The terminal shape a spent recovery ladder produces — shared by both
    /// ladders so an exhausted recovery is indistinguishable downstream from
    /// a failure that never had a recovery to spend.
    fn surface_unrecovered(
        &self,
        message: String,
        attempt_reasons: Vec<String>,
        state: &TurnState,
        events: &EventSender,
    ) -> StepOutcome {
        let retryable = false;
        let _ = events.send(AgentEvent::RetriesExhausted {
            turn_instance: self.config.turn_instance,
            attempts: attempt_reasons.len() as u32,
            reasons: attempt_reasons,
            retryable,
        });
        let _ = events.send(AgentEvent::Error {
            message: message.clone(),
            retryable,
        });
        if let Some(outcomes) = self.outcomes {
            outcomes.record_failure(self.active_provider().id());
        }
        StepOutcome::Aborted {
            reason: format!("model call failed: {message}"),
            kind: AbortKind::Failure,
            cost_usd: state.total_cost_usd,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rung 1 sheds a quarter of the rejected estimate, rung 2 half, and the
    /// third overflow finds the ladder spent.
    #[test]
    fn the_ladder_arms_twice_with_tightening_clamps_then_refuses() {
        let mut recovery = OverflowRecovery::default();
        assert_eq!(recovery.arm(100_000), Some(75_000));
        assert_eq!(recovery.arm(100_000), Some(50_000));
        assert_eq!(recovery.arm(100_000), None, "the latch is the bound");
        assert_eq!(recovery.arm(10), None, "…and it never re-arms");
    }

    /// A rung can only tighten the standing clamp: a smaller transcript on
    /// rung 2 (compaction worked, the provider still rejected) must not hand
    /// back a looser budget than rung 1 imposed.
    #[test]
    fn a_later_rung_never_loosens_an_earlier_clamp() {
        let mut recovery = OverflowRecovery::default();
        assert_eq!(recovery.arm(100_000), Some(75_000));
        // Half of 200k would be 100k — looser than the standing 75k clamp.
        assert_eq!(recovery.arm(200_000), Some(75_000));
    }

    /// The clamp applies through `clamped` and floors at one token, so a
    /// degenerate estimate cannot zero the budget and compact every step
    /// forever.
    #[test]
    fn the_clamp_tightens_the_budget_and_floors_at_one_token() {
        let mut recovery = OverflowRecovery::default();
        assert_eq!(
            recovery.clamped((150_000, 1.5)),
            (150_000, 1.5),
            "identity until armed"
        );
        recovery.arm(100_000);
        assert_eq!(recovery.clamped((150_000, 1.5)), (75_000, 1.5));
        assert_eq!(
            recovery.clamped((10_000, 1.0)),
            (10_000, 1.0),
            "a budget already under the clamp is untouched"
        );

        let mut tiny = OverflowRecovery::default();
        assert_eq!(tiny.arm(0), Some(1));
    }
}
