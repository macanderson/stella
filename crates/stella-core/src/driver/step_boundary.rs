// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The step boundary's host consults: steering, the soft stop, and the
//! proactive context re-query (#3243 Phase 3).
//!
//! Split out of `run_step_inner` when the re-query joined the boundary —
//! `driver.rs` is closed to growth under the file-size ratchet — but the
//! *order* here is part of the driver's contract, not an implementation
//! detail: the steering drain precedes the soft-stop check (a steer typed
//! just before Esc is preserved in history for the next turn instead of
//! evaporating with the per-turn tap), and the re-query runs after both, so
//! a user steer landed this step is part of what the next query answers to
//! rather than something it races.

use stella_protocol::{AgentEvent, CompletionMessage, SteerCause};

use crate::event_sender::EventSender;
use crate::step::{StepOutcome, TurnState};

use super::{AbortKind, SOFT_STOP_REASON, SOFT_STOP_TOOL_RESULT, loop_evidence};

/// Steering rides the same safe boundary as the pause gate: queued user
/// messages land BEFORE compaction (so the pass sees them) and before the
/// model call (so it answers them this step). Returns the aborting outcome
/// when a latched soft stop ends the turn.
pub(super) async fn consult_hosts(
    steering: Option<&dyn crate::ports::TurnSteering>,
    requery: Option<&dyn crate::ports::SteeringRequery>,
    state: &mut TurnState,
    events: &EventSender,
) -> Option<StepOutcome> {
    if let Some(steering) = steering {
        for text in steering.drain_steering() {
            // The port's queue is fed by a host on a person's behalf — the
            // deck's `>` steer, `stella-serve`'s control channel — and by
            // nothing else. The engine's own two rungs push their text
            // straight onto `messages` in `loop_escalation`, so this is the
            // one site that may claim `User`.
            let _ = events.send(AgentEvent::Steered {
                text: text.clone(),
                cause: SteerCause::User,
            });
            state.messages.push(CompletionMessage::user(text));
        }
        if steering.soft_stop_requested() {
            // A user choice, not a failure: no Error event, and the caller
            // keeps every completed step (unlike the hard cancel, which
            // drops the future and truncates). Open `tool_use`s are closed
            // exactly as the cancel exit closes them: the engine's own path
            // never leaves one open at this boundary, but history a caller
            // handed in (or appended to) mid-turn can, and the kept
            // transcript must stay valid for the next turn on this exit for
            // the same reason it must on that one.
            crate::step::close_open_tool_calls(&mut state.messages, SOFT_STOP_TOOL_RESULT, events);
            return Some(StepOutcome::Aborted {
                reason: SOFT_STOP_REASON.to_string(),
                kind: AbortKind::DeliberateStop,
                cost_usd: state.total_cost_usd,
            });
        }
    }
    // The proactive context re-query (#3243 Phase 3). The engine brings only
    // what the transcript witnesses — tools called, error classes seen, the
    // step index and the steps since the port last answered; the host side
    // merges its own ledger (touched paths, active domains) and owns
    // hysteresis and dedup, so an unchanged signal costs nothing here.
    if let Some(requery) = requery {
        let evidence = loop_evidence::turn_evidence(&state.messages);
        let prompt = evidence
            .prompt_index
            .map(|i| state.messages[i].content.as_str())
            .unwrap_or("");
        let signal = crate::steering::TurnSignal {
            prompt,
            recent_tool_calls: &evidence.tool_names,
            touched_paths: &evidence.touched_paths,
            active_domains: &[],
            step: u32::try_from(state.step).unwrap_or(u32::MAX),
            since_last_query: state.steps_since_requery,
            errors_seen: &evidence.error_classes,
        };
        match requery.requery(&signal).await {
            Some(block) => {
                state.messages.push(CompletionMessage::user(block));
                state.steps_since_requery = 0;
            }
            None => {
                state.steps_since_requery = state.steps_since_requery.saturating_add(1);
            }
        }
    }
    None
}
