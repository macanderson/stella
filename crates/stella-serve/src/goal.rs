//! Judged multi-round runs over the wire (#1297).
//!
//! "Keep working until an independent verifier agrees it is done" is the
//! capability an agent-app host is most likely to be paying for, and until now
//! it existed only on the command line (`stella goal`) — not restricted, not
//! disabled, simply unrequestable. This is the API half.
//!
//! # Why it is a mode on a turn rather than a `/v1/goals` resource
//!
//! Everything a goal run needs from the transport already exists on a turn and
//! is already witnessed: `GET /v1/turns/{id}/events` streams its progress
//! (rounds emit `GoalVerdict` events like any other agent event),
//! `POST /v1/turns/{id}/cancel` stops it at the next step boundary, pause and
//! steer reach it, and the settlement hook writes the transcript back. A
//! `/v1/goals` resource would restate every one of those with its own id space
//! and its own bugs. The mode flag is the *narrower* change, and
//! `stella-parity`'s own row named it as one of the two acceptable shapes.
//!
//! # Progress arrives as a stream, not by polling
//!
//! Rounds take minutes, and the issue behind this asked the question directly.
//! The answer is the SSE stream that already exists: a goal run emits the same
//! events a turn does, plus one `GoalVerdict` per round carrying the verifier's
//! reasoning and that round's cost. Nothing new to poll, nothing to reconcile
//! between two channels, and a client that drops reconnects with `?after=` and
//! replays the rounds it missed.
//!
//! # What stopping does to the work already done
//!
//! Cancel unwinds the round in flight at its next step boundary and settles.
//! Completed rounds are kept: their work is in the transcript the settlement
//! hook writes back, and the events describing them have already been
//! delivered. A cancelled goal run therefore ends as an ordinary `aborted`
//! turn outcome carrying its real cost — the same contract a cancelled
//! single-turn run has, which is what lets a host handle one path.

use stella_core::bus::HookBus;
use stella_core::{BudgetGuard, Engine, EventSender, GoalConfig, TurnOutcome};
use stella_engine::CancelToken;
use stella_protocol::{AgentEvent, CompletionMessage, Provider, StageKind};
use tokio::sync::mpsc;

use crate::session::DrivenTurn;

/// A judged multi-round run, as the host asked for it.
pub struct GoalRun {
    /// What the verifier assesses each round against.
    pub goal: String,
    /// Round cap, verifier output cap, verifier transcript window — the engine's own
    /// tuning, already clamped by the route that built this.
    pub config: GoalConfig,
    /// The provider id the verifier's calls announce, when the caller named a
    /// different one than the turn's worker.
    ///
    /// `None` runs the verifier on the turn's own provider id, which is the
    /// single-model deployment and stays the default. Naming a second one is
    /// how a caller buys the property the goal loop is built on: a verifier that
    /// is not the model that did the work. The engine cannot enforce that —
    /// the host runs the calls — so this is a request the host honors, stamped
    /// on every verifier frame as `provider_id` + `role: verifier`.
    pub verifier_provider_id: Option<String>,
}

// The seed and feedback wording is `stella_core::goal`'s
// (`goal_kickoff_text` / `verifier_feedback_text`): a served goal round and a
// CLI goal round must put the same words in front of the model or they are
// two features wearing one name — this file used to carry its own copies
// with a comment promising byte-identity that nothing enforced.

/// Drive a judged goal run to its outcome (#1297).
///
/// Structurally [`Engine::run_goal`], re-expressed as a loop over this crate's
/// own [`crate::session::drive_turn`] — the same relationship `drive_turn`
/// itself has to `Engine::run_turn`, and for the same two reasons: a served
/// round must be cancellable *between* reverse requests, and it must reach the
/// checkpoint seam at every step boundary. A goal run is the case where both
/// matter most, since it is the longest-lived thing this server does.
///
/// The rest is `run_goal`'s contract, kept deliberately identical:
///
/// - each round takes its own receipt turn slot (worker even, verifier odd), so
///   two rounds' manifests cannot collide;
/// - a round that aborts ends the run — an unclean turn is never silently
///   retried;
/// - a verifier that cannot answer ends it too, rather than looping unjudged
///   work or fabricating a verdict;
/// - the round cap is the backstop, and reaching it is `Unmet` with a named
///   reason, never a silent success.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn drive_goal(
    engine: &Engine<'_>,
    verifier: &dyn Provider,
    run: &GoalRun,
    // The turn slot this run starts from — the caller's own
    // `EngineConfig::turn_instance`, passed in rather than read back off the
    // engine so this needs no new accessor on `stella-core`'s driver.
    base_turn: u32,
    mut messages: Vec<CompletionMessage>,
    mut budget: BudgetGuard,
    events: &mpsc::UnboundedSender<AgentEvent>,
    cancel: CancelToken,
    // This turn's operator hook plane (#1298), forwarded into every round.
    // A goal run is many turns wearing one turn's id, and an observer that
    // saw only the first round would report a truncated run — so the bus
    // reaches each round exactly as it reaches a single served turn.
    bus: Option<&HookBus>,
) -> DrivenTurn {
    let sender = EventSender::new(events.clone());
    messages.push(CompletionMessage::user(
        stella_core::goal::goal_kickoff_text(&run.goal),
    ));

    for round in 1..=run.config.max_rounds {
        budget.begin_turn();
        let offset = stella_core::goal::goal_round_turn_offset(round);
        let round_engine = engine.with_turn_instance(base_turn.saturating_add(offset));
        let driven = crate::session::drive_turn(
            &round_engine,
            messages,
            budget,
            events,
            cancel.clone(),
            bus,
        )
        .await;
        messages = driven.messages;
        budget = driven.budget;
        if let TurnOutcome::Aborted {
            reason,
            kind,
            cost_usd,
        } = driven.outcome
        {
            // Includes cancellation: the completed rounds' work stays in the
            // transcript that settles below, and the reason names what ended
            // the run rather than pretending the goal was assessed.
            return DrivenTurn {
                outcome: TurnOutcome::Aborted {
                    reason: format!("goal not met — working round {round} ended: {reason}"),
                    kind,
                    cost_usd,
                },
                messages,
                budget,
            };
        }

        let _ = sender.send(AgentEvent::Stage {
            name: StageKind::Verdict,
        });
        let (verdict, verifier_cost) = match round_engine
            .assess(
                verifier,
                &run.goal,
                &messages,
                &mut budget,
                events,
                &run.config,
            )
            .await
        {
            Ok(pair) => pair,
            Err(reason) => {
                return DrivenTurn {
                    outcome: TurnOutcome::Aborted {
                        reason: format!("goal not met — verifier unavailable: {reason}"),
                        kind: stella_core::AbortKind::Failure,
                        cost_usd: budget.session_spent_usd(),
                    },
                    messages,
                    budget,
                };
            }
        };
        let _ = sender.send(AgentEvent::GoalVerdict {
            round,
            met: verdict.met,
            reasoning: verdict.reasoning.clone(),
            cost_usd: verifier_cost,
        });
        if verdict.met {
            return DrivenTurn {
                outcome: TurnOutcome::Completed {
                    // The verifier's reasoning IS the answer to "is it done?", so
                    // it is what the terminal frame carries. A host that wants
                    // the worker's own last words has them in the transcript
                    // the settlement hook writes back.
                    text: verdict.reasoning,
                    cost_usd: budget.session_spent_usd(),
                },
                messages,
                budget,
            };
        }
        messages.push(CompletionMessage::user(
            stella_core::goal::verifier_feedback_text(&run.goal, &verdict),
        ));
    }

    DrivenTurn {
        outcome: TurnOutcome::Aborted {
            reason: format!(
                "goal not met — round cap ({}) reached without a passing verdict",
                run.config.max_rounds
            ),
            kind: stella_core::AbortKind::DeliberateStop,
            cost_usd: budget.session_spent_usd(),
        },
        messages,
        budget,
    }
}
