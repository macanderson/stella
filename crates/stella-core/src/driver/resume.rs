// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Drive a checkpoint-restored turn to an ordinary end — the resume-aware
//! sibling of [`Engine::run_turn_with_sender`].
//!
//! [`Engine::run_turn_with_sender`] always *starts* a turn: it adopts the
//! caller's transcript into a fresh [`TurnState`] at step 0. A turn rebuilt
//! from a [`crate::step::Checkpoint`] begins wherever the killed process's
//! last committed step left off, so its driver must accept the state instead
//! of minting one — and must still keep every obligation the fresh loop
//! keeps, in the same places:
//!
//! - **persist on every `Continue`** — a resumed run must stay resumable;
//! - **discard on every terminal path** — a checkpoint that outlives its turn
//!   invites resuming finished work;
//! - **gate on the step cap the state carried across the crash** — a turn
//!   killed at step 37 of 40 gets 3 more, not 40;
//! - **consult [`EngineConfig::turn_halt`] at each committed boundary** — a
//!   restored turn whose tracked test flips mid-run stops exactly where a
//!   fresh one would (the check the CLI's hand-rolled resume loop never had);
//! - **emit the turn lifecycle** — an extension observing turns on the bus
//!   sees a resumed turn as a turn, not a gap.
//!
//! This lives here, in the engine, rather than in each host: `stella-cli`'s
//! `daemon resume` and `stella-pipeline`'s resumed execute stage both drive
//! restored turns, and two copies of this loop is how one of them forgets an
//! obligation (the halt check above was exactly that omission).
//!
//! [`EngineConfig::turn_halt`]: crate::EngineConfig

use stella_protocol::{AgentEvent, StageKind};

use super::{Engine, TurnOutcome, bus, lifecycle, settlement, step_cap_reason};
use crate::event_sender::EventSender;
use crate::step::{AbortKind, StepOutcome, TurnState};

impl<'a> Engine<'a> {
    /// Drive `state` — a turn restored with [`TurnState::from_checkpoint`] —
    /// through [`Engine::run_step`] to an ordinary end.
    ///
    /// The transcript already contains every completed step's effects as
    /// facts; the first provider call here simply asks for the step after the
    /// last committed one. On return `state` holds the finished transcript
    /// (retrieve it with [`TurnState::into_messages`]), which is what lets a
    /// staged pipeline continue over the resumed turn's history.
    pub async fn drive_restored_turn(
        &self,
        state: &mut TurnState,
        events: &EventSender,
    ) -> TurnOutcome {
        let _ = events.send(AgentEvent::Stage {
            name: StageKind::Execute,
        });
        self.emit_lifecycle(bus::names::AGENT_TURN_STARTED, || {
            lifecycle::turn_started_payload(
                state.messages().len(),
                self.config.max_steps,
                self.call_role,
            )
        });
        loop {
            if state.step() >= self.config.max_steps {
                let reason = step_cap_reason(self.config.max_steps);
                let _ = events.send(AgentEvent::Error {
                    message: reason.clone(),
                    retryable: false,
                });
                // The engine's own escalation, same as the in-turn cap: the
                // run stopped by policy, it did not fall over (#1524).
                let outcome = TurnOutcome::Aborted {
                    reason,
                    kind: AbortKind::DeliberateStop,
                    cost_usd: state.total_cost_usd(),
                };
                return self.settle_restored(state, outcome);
            }
            match self.run_step(state, events).await {
                StepOutcome::Continue => {
                    // The checkpoint seam (#971): `Continue` is the one moment
                    // the transcript is guaranteed well-paired, and
                    // `state.step` already names the step that runs NEXT.
                    self.persist_checkpoint(state);
                    // "The goal is already met" — asked at the same committed
                    // boundary the fresh loop asks it, and `Completed` for the
                    // same reason: this turn did its work, and `Aborted`
                    // reaches exit-code readers as a crash.
                    if let Some(reason) = self
                        .config
                        .turn_halt
                        .as_ref()
                        .and_then(|halt| halt.halt_reason())
                    {
                        let text = settlement::last_assistant_text(state).unwrap_or(reason);
                        let outcome = TurnOutcome::Completed {
                            text,
                            cost_usd: state.total_cost_usd(),
                        };
                        return self.settle_restored(state, outcome);
                    }
                }
                StepOutcome::Done { text, cost_usd } => {
                    let outcome = TurnOutcome::Completed { text, cost_usd };
                    return self.settle_restored(state, outcome);
                }
                StepOutcome::Aborted {
                    reason,
                    kind,
                    cost_usd,
                } => {
                    let outcome = TurnOutcome::Aborted {
                        reason,
                        kind,
                        cost_usd,
                    };
                    return self.settle_restored(state, outcome);
                }
            }
        }
    }

    /// The three exit obligations every terminal path above owes, in one
    /// place: report the turn on the lifecycle bus, retire the checkpoint,
    /// hand back the outcome.
    fn settle_restored(&self, state: &TurnState, outcome: TurnOutcome) -> TurnOutcome {
        self.emit_lifecycle(bus::names::AGENT_TURN_COMPLETED, || {
            lifecycle::turn_outcome_payload(&outcome, state.step())
        });
        self.discard_checkpoint();
        outcome
    }
}
