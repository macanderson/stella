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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use async_trait::async_trait;
    use serde_json::Value;
    use tokio::sync::mpsc;

    use super::super::TurnHalt;
    use crate::event_sender::EventSender;
    use crate::retry::Sleeper;
    use crate::step::{BudgetSnapshot, CHECKPOINT_VERSION, Checkpoint, TurnState};
    use crate::{Engine, EngineConfig, TurnOutcome};
    use stella_protocol::{
        BudgetMode, CompletionMessage, CompletionRequestRef, CompletionResult, CompletionUsage,
        Provider, ProviderError, ToolCall, ToolOutput, ToolSchema,
    };

    /// Answers a tool call first, then plain text — so the first step is a
    /// committed `Continue` boundary and the second would finish the turn.
    struct ToolThenText {
        calls: Arc<AtomicU32>,
    }

    #[async_trait]
    impl Provider for ToolThenText {
        fn id(&self) -> &str {
            "scripted"
        }
        async fn complete_ref(
            &self,
            _req: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let mut result = CompletionResult {
                text: String::new(),
                tool_calls: vec![],
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 1,
                    ..CompletionUsage::default()
                },
                model: "scripted".into(),
                cost_usd: 0.0,
                finish_reason: None,
            };
            if call == 0 {
                result.tool_calls = vec![ToolCall {
                    call_id: "call_0".into(),
                    name: "touch".into(),
                    input: serde_json::json!({}),
                }];
            } else {
                result.text = "finished after the halt should have fired".into();
            }
            Ok(result)
        }
    }

    struct OkTool;

    #[async_trait]
    impl crate::ToolExecutor for OkTool {
        fn schemas(&self) -> Vec<ToolSchema> {
            vec![ToolSchema {
                name: "touch".into(),
                description: "touch a file".into(),
                input_schema: serde_json::json!({"type": "object"}),
                read_only: false,
                speculation_safe: false,
            }]
        }
        async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
            ToolOutput::Ok {
                content: "ok".into(),
            }
        }
    }

    #[derive(Debug)]
    struct NoopSleeper;
    #[async_trait]
    impl Sleeper for NoopSleeper {
        async fn sleep(&self, _duration_ms: u64) {}
    }

    /// Armed from the start, so the first committed boundary ends the turn.
    #[derive(Debug)]
    struct AlwaysHalt;
    impl TurnHalt for AlwaysHalt {
        fn halt_reason(&self) -> Option<String> {
            Some("the tracked test flipped".into())
        }
    }

    fn restored_at_step_one() -> Checkpoint {
        Checkpoint {
            version: CHECKPOINT_VERSION,
            step: 1,
            messages: vec![
                CompletionMessage::system("sys"),
                CompletionMessage::user("keep going"),
            ],
            budget: BudgetSnapshot {
                mode: BudgetMode::Off,
                turn_limit_usd: None,
                session_limit_usd: None,
                turn_spent_usd: 0.0,
                session_spent_usd: 0.0,
            },
            total_cost_usd: 0.0,
            calibration_model: None,
            loop_steered: false,
            loop_steered_pattern: Vec::new(),
            loop_steered_inputs: None,
            transcript_rewrites: 0,
        }
    }

    /// **The obligation the CLI's hand-rolled resume loop never had.** A
    /// restored turn whose halt predicate fires ends `Completed` at the next
    /// committed boundary instead of running on — one provider call, not two.
    /// Fails on the old shape (two drivers, only the fresh one consulting
    /// `turn_halt`) because this method did not exist and the copy that did
    /// never asked.
    #[tokio::test]
    async fn a_restored_turn_honors_the_turn_halt() {
        let calls = Arc::new(AtomicU32::new(0));
        let provider = ToolThenText {
            calls: calls.clone(),
        };
        let tools = OkTool;
        let config = EngineConfig {
            turn_halt: Some(Arc::new(AlwaysHalt)),
            ..EngineConfig::default()
        };
        let mut state = TurnState::from_checkpoint(restored_at_step_one(), &config);
        let engine = Engine::with_sleeper(&provider, &tools, config, &NoopSleeper);
        let (tx, _rx) = mpsc::unbounded_channel();
        let events = EventSender::new(tx);

        let outcome = engine.drive_restored_turn(&mut state, &events).await;

        let TurnOutcome::Completed { text, .. } = outcome else {
            panic!("a fired halt ends the turn as a success: {outcome:?}");
        };
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the halt fires at the first committed boundary — a second call \
             means the restored driver never consulted it"
        );
        assert!(!text.is_empty());
    }

    /// The control: no halt, and the restored turn runs to its ordinary end —
    /// the second call happens and its text is the outcome.
    #[tokio::test]
    async fn an_unhalted_restored_turn_runs_to_its_ordinary_end() {
        let calls = Arc::new(AtomicU32::new(0));
        let provider = ToolThenText {
            calls: calls.clone(),
        };
        let tools = OkTool;
        let config = EngineConfig::default();
        let mut state = TurnState::from_checkpoint(restored_at_step_one(), &config);
        let engine = Engine::with_sleeper(&provider, &tools, config, &NoopSleeper);
        let (tx, _rx) = mpsc::unbounded_channel();
        let events = EventSender::new(tx);

        let outcome = engine.drive_restored_turn(&mut state, &events).await;

        let TurnOutcome::Completed { text, .. } = outcome else {
            panic!("an unhalted restored turn completes: {outcome:?}");
        };
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(text, "finished after the halt should have fired");
    }
}
