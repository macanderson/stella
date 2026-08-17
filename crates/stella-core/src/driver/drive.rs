// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The turn loop — [`Engine::drive`], the only one.
//!
//! [`Engine::run_step`] is deliberately the only implementation of a *step*
//! ("`run_turn` IS this method in a loop, so there is one code path and no
//! second implementation to drift"). This module is the same commitment for
//! the *loop around* the step, which used to exist three times: the fresh
//! turn in `Engine::run_turn_with_sender`, the restored turn in the engine's
//! own resume driver, and the served turn in `stella-serve`'s session thread.
//!
//! The three never disagreed about what a turn *is*. They disagreed only
//! about how a [`TurnState`] is minted — adopted from a caller's borrows,
//! rebuilt from a [`crate::step::Checkpoint`], or owned by a host — and how
//! the finished outcome is handed back. Everything between those two points
//! was the same obligations written out three times, which is how one of them
//! ends up keeping four of five:
//!
//! - **frame the turn** — the opening `Stage(Execute)` and the
//!   `agent.turn.started` / `agent.turn.completed` pair on the hook bus, so an
//!   extension observing turns sees every turn as a turn and never an unpaired
//!   boundary (#1133);
//! - **gate on the step cap the state carries** — a turn restored at step 37
//!   of 40 gets 3 more, not 40;
//! - **run exactly one committed step** through [`Engine::run_step`];
//! - **persist on every `Continue`** — the checkpoint seam (#971), the one
//!   moment the transcript is guaranteed well-paired, so a resumed run stays
//!   resumable;
//! - **consult [`EngineConfig::turn_halt`] at each committed boundary** — a
//!   turn whose tracked test flips mid-run stops where the goal was met rather
//!   than where a limit was hit;
//! - **discard on every terminal path** — a checkpoint that outlives its turn
//!   invites resuming work the caller already saw finish.
//!
//! Two of these have already been forgotten by a copy. `turn_halt` was missing
//! from the CLI's hand-rolled resume loop until the engine grew a resume driver
//! of its own, and it was missing from `stella-serve`'s loop until this module
//! existed — silently, because nothing fails when an obligation lands in two of
//! three places.
//!
//! # What stays outside
//!
//! A host that drives [`Engine::run_step`] itself still owns its own framing;
//! `run_step` is per-step and cannot emit a turn boundary. That is why
//! [`crate::driver::lifecycle`]'s shapers remain public. What changed is only
//! that a host wanting an ordinary turn no longer has to write the loop to get
//! one.
//!
//! The **outer** loops are deliberately not here: the goal round loop
//! ([`crate::goal`]), the pipeline's verify/revise loop, and the fleet's wave
//! loop each have their own termination vocabulary and cancellation contract,
//! and folding them together would produce a config struct that is mostly
//! `None` at every call site.
//!
//! [`EngineConfig::turn_halt`]: crate::EngineConfig

use stella_protocol::{AgentEvent, StageKind};

use super::{Engine, TurnOutcome, bus, lifecycle, settlement, step_cap_reason};
use crate::event_sender::EventSender;
use crate::step::{AbortKind, TurnState};

impl Engine<'_> {
    /// Drive `state` to an ordinary end: a loop over [`Engine::run_step`],
    /// wrapped in the turn framing and the checkpoint and halt obligations a
    /// turn owes (see the module docs).
    ///
    /// The caller mints `state` — [`Engine::run_turn_with_sender`] adopts a
    /// borrowed transcript, a resuming caller rebuilds one with
    /// [`TurnState::from_checkpoint`], a durable host owns one from
    /// [`Engine::new_turn`] — and settles the returned outcome. On return
    /// `state` holds the finished transcript, which is what lets a staged
    /// pipeline continue over a resumed turn's history.
    ///
    /// # This future is `!Send`
    ///
    /// It inherits [`Engine::run_step`]'s contract: drive it on a
    /// current-thread runtime. See that method for why, and for why a caller
    /// that needs to stop a turn should cancel rather than drop this future.
    pub async fn drive(&self, state: &mut TurnState, events: &EventSender) -> TurnOutcome {
        let _ = events.send(AgentEvent::Stage { name: StageKind::Execute, scope: stella_protocol::StageScope::Turn });
        self.emit_lifecycle(bus::names::AGENT_TURN_STARTED, || {
            lifecycle::turn_started_payload(
                state.messages().len(),
                self.config.max_steps,
                self.call_role,
            )
        });
        let outcome = self.step_loop(state, events).await;
        // Emitted here rather than at each of the loop's exits — including the
        // step-cap exit, whose turn is exactly the runaway an observer most
        // wants paired with its `started`. The same reasoning that put
        // `agent.step.completed` in a wrapper around `run_step_inner` rather
        // than at each of its dozen returns.
        self.emit_lifecycle(bus::names::AGENT_TURN_COMPLETED, || {
            lifecycle::turn_outcome_payload(&outcome, state.step())
        });
        // A turn that is over — completed, aborted, or run into the cap — must
        // not leave a resume point offering to replay it.
        self.discard_checkpoint();
        outcome
    }

    /// The loop itself, split from the framing above so every terminal path is
    /// a plain `return` and the two boundary emissions cannot be forgotten on
    /// one of them.
    async fn step_loop(&self, state: &mut TurnState, events: &EventSender) -> TurnOutcome {
        loop {
            if state.step() >= self.config.max_steps {
                // The belt-and-suspenders backstop. `DeliberateStop`, not a
                // failure: the run stopped by policy, it did not fall over
                // (#1524).
                let reason = step_cap_reason(self.config.max_steps);
                let _ = events.send(AgentEvent::Error {
                    message: reason.clone(),
                    retryable: false,
                });
                return TurnOutcome::Aborted {
                    reason,
                    kind: AbortKind::DeliberateStop,
                    cost_usd: state.total_cost_usd(),
                };
            }
            // `StepOutcome::into_turn_outcome` is the one conversion from "how
            // the step ended" to "how the turn ended", and `None` is its
            // contract for "the turn continues" — so falling through here is
            // that contract being read, not a variant being assumed away.
            if let Some(outcome) = self.run_step(state, events).await.into_turn_outcome() {
                return outcome;
            }

            // Past here the step was `Continue`: the checkpoint seam (#971),
            // the one moment the transcript is guaranteed well-paired — no
            // `tool_use` without its `tool_result` — and `run_step` has already
            // advanced `state.step()`, so the snapshot names the step that runs
            // NEXT rather than the one that just finished. Writing here and
            // nowhere else is what makes a resumed turn indistinguishable from
            // one that was never interrupted.
            self.persist_checkpoint(state);

            // "The goal is already met" — the one stopping condition that is
            // not a limit being hit. Asked here, at a committed boundary, for
            // the same reason the checkpoint is written here: ending now can
            // never orphan a `tool_use` from its `tool_result`. Asking mid-step
            // could.
            //
            // Asked AFTER a step rather than at the top of the loop so a turn
            // always does something: a predicate already true on entry
            // describes a goal met before this turn began, which is the
            // caller's business to notice, not a reason to return an empty
            // turn.
            //
            // `Completed`, never `Aborted`. This turn did its work; `Aborted`
            // is the failure arm and reaches the CLI as a non-zero exit, which
            // Harbor — and any other host reading exit codes — scores
            // identically to the agent crashing.
            if let Some(reason) = self
                .config
                .turn_halt
                .as_ref()
                .and_then(|halt| halt.halt_reason())
            {
                // Prefer the model's own last words; fall back to the
                // predicate's reason when the final step was pure tool calls
                // (an assistant message that only called tools has empty
                // `content`), so the turn never reports an empty answer.
                let text = settlement::last_assistant_text(state).unwrap_or(reason);
                return TurnOutcome::Completed {
                    text,
                    cost_usd: state.total_cost_usd(),
                };
            }
        }
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
                upstream_provider: None,
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
                data: None,
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
            loop_steers_spent: 0,
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

        let outcome = engine.drive(&mut state, &events).await;

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

        let outcome = engine.drive(&mut state, &events).await;

        let TurnOutcome::Completed { text, .. } = outcome else {
            panic!("an unhalted restored turn completes: {outcome:?}");
        };
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(text, "finished after the halt should have fired");
    }

    /// A turn a host owns from [`Engine::new_turn`] — the shape `stella-serve`
    /// drives — keeps the same halt obligation as a fresh CLI turn and a
    /// restored one. Fails on the old shape, where the served loop was a third
    /// copy that never consulted `turn_halt`: `EngineConfig::turn_halt` is a
    /// public field of the public `SessionSpec::config`, so an embedder could
    /// arm it and be silently ignored.
    #[tokio::test]
    async fn a_host_owned_turn_honors_the_turn_halt() {
        let calls = Arc::new(AtomicU32::new(0));
        let provider = ToolThenText {
            calls: calls.clone(),
        };
        let tools = OkTool;
        let config = EngineConfig {
            turn_halt: Some(Arc::new(AlwaysHalt)),
            ..EngineConfig::default()
        };
        let engine = Engine::with_sleeper(&provider, &tools, config, &NoopSleeper);
        let mut state = engine.new_turn(
            vec![
                CompletionMessage::system("sys"),
                CompletionMessage::user("go"),
            ],
            crate::budget::BudgetGuard::new(BudgetMode::Off, None, None),
        );
        let (tx, _rx) = mpsc::unbounded_channel();
        let events = EventSender::new(tx);

        let outcome = engine.drive(&mut state, &events).await;

        let TurnOutcome::Completed { .. } = outcome else {
            panic!("a fired halt ends the turn as a success: {outcome:?}");
        };
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the halt fires at the first committed boundary — a second call \
             means the host-owned driver never consulted it"
        );
    }
}
