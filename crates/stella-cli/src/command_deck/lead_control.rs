// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The lead lane's pause seam (#1219) — what `p` on the lead row reaches.
//!
//! Worker lanes have been pausable since they existed; the lead's `Control`
//! arm was a no-op, because the deck had nowhere to put a gate that the
//! *staged* path would honor. `Pipeline::with_turn_gate` (#1214) is that
//! place, so this module is the deck-side plumbing: one watch channel per
//! turn, wrapped in the `WatchGate` the pipeline, the step-loop engine, and
//! the registry's sub-agent dispatcher all hold.
//!
//! It is one type rather than three loose locals in the driver loop because
//! the three pieces have an invariant between them. The channel is what parks
//! the turn; the latch is what the dashboard was last told; and they are not
//! the same fact — a soft stop releases the channel while the row is still
//! painted paused, and the row must be repaid at turn end whether or not the
//! turn ever actually parked. Keeping them together is what makes those two
//! cases impossible to get half-right.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use stella_tui::{AgentControl, AgentStatus, Inbound};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch;

use super::LEAD;
use crate::subsession;

/// One turn's pause seam, built fresh per turn.
///
/// Per-turn is the point: the next turn constructs a new channel that starts
/// `false`, so a pause cannot leak into the turn after it, and a stale
/// pause/resume aimed at a turn that has already ended is a no-op rather than
/// a state the next prompt inherits.
pub(super) struct LeadPause {
    /// Flipped by [`Self::control`]; `true` parks the turn.
    tx: watch::Sender<bool>,
    /// The receiving half, in the form the engine ports want. `Arc` because it
    /// goes three places: the pipeline (which attaches it to every engine it
    /// builds and parks its management calls behind it), the step-loop engine,
    /// and the registry — so sub-agents the turn dispatched park with it,
    /// rather than a paused lead spending on inside its own children.
    gate: Arc<subsession::WatchGate>,
    /// Whether the dashboard row was last painted `Paused`.
    ///
    /// Deliberately not "is the gate closed": the deck's fold refuses to move
    /// a row off `Paused` from the event stream — by design, so streaming
    /// output cannot silently un-pause a lane the user parked — which means
    /// the driver owes the row an explicit status once the turn is over. That
    /// debt outlives the gate being reopened (see
    /// [`Self::release_for_soft_stop`]).
    ///
    /// Atomic rather than a plain `bool` because the driver's turn future
    /// borrows this seam for as long as it runs while the input pump writes to
    /// it — the same shape `watch::Sender` already has, and the reason every
    /// method here takes `&self`.
    painted_paused: AtomicBool,
}

impl LeadPause {
    /// A fresh, unpaused seam for one turn.
    pub(super) fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        Self {
            tx,
            gate: Arc::new(subsession::WatchGate(rx)),
            painted_paused: AtomicBool::new(false),
        }
    }

    /// The gate to hand to `Pipeline::with_turn_gate` and `Engine::with_gate`.
    /// (`attach_turn_controls` wants the owned form — see [`turn_controls`].)
    pub(super) fn turn_gate(&self) -> &dyn stella_core::ports::TurnGate {
        self.gate.as_ref()
    }

    /// Service a `Control` verb aimed at the LEAD lane — the lead twin of
    /// `service_worker_control`, and deliberately much smaller: a worker lane
    /// is a spawned thread with a retained spec, while the lead IS the
    /// session.
    ///
    /// Pause/Resume flip the channel, so the turn parks at its next step
    /// boundary and never mid-tool (L-E6). A `send` that fails means every
    /// receiver is gone — the turn is already over — which is exactly the
    /// stale control that must read as a no-op, so it is dropped rather than
    /// reported.
    ///
    /// The status emitted here is load-bearing, not decoration: `p` is a
    /// *toggle* resolved against the row's current status, so a pause that
    /// parked the turn without painting the row would leave the next `p`
    /// sending Pause again — a lane the user could park and never release.
    ///
    /// `Restart` is not a lead verb, by the fleet's own rule: a restart is the
    /// caller re-dispatching, which the deck already offers (stop, then submit
    /// again), and there is no retained lead spec to respawn from. `Stop`
    /// never arrives here — the driver's own arm claims it, for both lanes —
    /// and is matched only so a newly added control cannot be silently
    /// swallowed.
    pub(super) fn control(&self, control: AgentControl, in_tx: &UnboundedSender<Inbound>) {
        let status = match control {
            AgentControl::Pause => {
                let _ = self.tx.send(true);
                self.painted_paused.store(true, Ordering::SeqCst);
                AgentStatus::Paused
            }
            AgentControl::Resume => {
                let _ = self.tx.send(false);
                self.painted_paused.store(false, Ordering::SeqCst);
                AgentStatus::Running
            }
            AgentControl::Restart | AgentControl::Stop => return,
        };
        let _ = in_tx.send(Inbound::Status {
            agent: LEAD.to_string(),
            status,
        });
    }

    /// Reopen the gate because a soft stop was just latched.
    ///
    /// A soft stop ends the turn at its NEXT step boundary — and a paused turn
    /// is exactly one that cannot reach a boundary, because the gate parks it
    /// just before the stop flag is read. Without this, Esc on a paused lead
    /// reads as doing nothing until the user happens to resume.
    ///
    /// The painted state is deliberately left alone: the row is still showing
    /// paused, and [`Self::settle`] is what owes it a status.
    pub(super) fn release_for_soft_stop(&self) {
        let _ = self.tx.send(false);
    }

    /// The turn is over, so the lane is not paused any more — whether it ran
    /// past its last boundary to completion or was hard-cancelled while
    /// parked. Repay the row if it was ever painted paused.
    ///
    /// Said explicitly because the fold will not move a row off `Paused` from
    /// the event stream: without this the cancel path's terminal `Error` — and
    /// every event of the NEXT turn — would be swallowed, and the row would
    /// read paused for the rest of the session.
    ///
    /// `WaitingInput`, not a terminal state: the driver's own end-of-turn arms
    /// emit the turn's outcome, and this must not pre-empt them.
    pub(super) fn settle(&self, in_tx: &UnboundedSender<Inbound>) {
        if self.painted_paused.load(Ordering::SeqCst) {
            let _ = in_tx.send(Inbound::Status {
                agent: LEAD.to_string(),
                status: AgentStatus::WaitingInput,
            });
        }
    }
}

/// This turn's controls in owned form, for `attach_turn_controls`.
///
/// Both seams, which no driver published before #1219: the soft stop AND the
/// pause reach the sub-agents the lead dispatches, because the dispatcher
/// reads them off the registry at dispatch time. Without the gate here a
/// paused lead would keep spending inside a child it had already dispatched —
/// pause would redirect the spend rather than stop it.
///
/// The steering the child actually gets is narrowed to
/// `stella_core::subagent::ChildSteering` by `run_sub_agent`: the stop
/// crosses, the parent's queued messages never do.
pub(super) fn turn_controls(
    steering: &Arc<subsession::SteeringTap>,
    pause: &LeadPause,
) -> stella_core::ports::TurnControls {
    stella_core::ports::TurnControls::none()
        .with_steering(steering.clone())
        .with_gate(pause.gate.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_core::ports::TurnGate;
    use tokio::sync::mpsc;

    /// `p` on the lead row must reach the gate handed to
    /// `Pipeline::with_turn_gate` and `Engine::with_gate`, so this asserts on
    /// the real adapter's parking behavior rather than on the flag behind it.
    /// Parking IS the acceptance criterion — the engine polls the gate once
    /// per step, so a gate that never parks is a pause that buys another
    /// model call.
    #[tokio::test]
    async fn pausing_the_lead_lane_parks_its_turn_gate_until_resume() {
        let pause = LeadPause::new();
        let (in_tx, mut in_rx) = mpsc::unbounded_channel();

        // Unpaused, a step boundary costs nothing: the turn runs straight
        // through.
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            pause.gate.wait_if_paused(),
        )
        .await
        .expect("an unpaused gate must not park the turn");

        pause.control(AgentControl::Pause, &in_tx);
        assert!(
            pause.painted_paused.load(Ordering::SeqCst),
            "the row is painted paused, so the driver owes it a status at turn \
             end — the fold will not clear it from the event stream"
        );
        match in_rx.try_recv() {
            Ok(Inbound::Status { agent, status }) => {
                assert_eq!(agent, LEAD);
                assert_eq!(status, AgentStatus::Paused);
            }
            other => panic!("expected the lead row to flip to paused, got {other:?}"),
        }

        // Park a real waiter — this is the await the engine's next step takes.
        let waiter = tokio::spawn({
            let gate = pause.gate.clone();
            async move { gate.wait_if_paused().await }
        });
        let mut waiter = std::pin::pin!(waiter);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), &mut waiter)
                .await
                .is_err(),
            "a paused gate must hold the turn at its step boundary"
        );

        pause.control(AgentControl::Resume, &in_tx);
        assert!(
            !pause.painted_paused.load(Ordering::SeqCst),
            "nothing owed at turn end"
        );
        match in_rx.try_recv() {
            Ok(Inbound::Status { agent, status }) => {
                assert_eq!(agent, LEAD);
                assert_eq!(status, AgentStatus::Running);
            }
            other => panic!("expected the lead row to flip back to running, got {other:?}"),
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .expect("resume must release the turn parked at the boundary")
            .expect("the waiter task completes");
    }

    /// Pause and the soft stop are polled at the SAME step boundary, and the
    /// gate parks the turn just before the stop flag is read — so a stop
    /// latched behind a pause would sit unobserved, and Esc on a paused lead
    /// would read as doing nothing.
    #[tokio::test]
    async fn a_soft_stop_releases_a_paused_lead_so_the_turn_can_reach_its_boundary() {
        let pause = LeadPause::new();
        let (in_tx, _in_rx) = mpsc::unbounded_channel();

        pause.control(AgentControl::Pause, &in_tx);
        let waiter = tokio::spawn({
            let gate = pause.gate.clone();
            async move { gate.wait_if_paused().await }
        });
        let mut waiter = std::pin::pin!(waiter);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), &mut waiter)
                .await
                .is_err(),
            "parked, as a paused turn must be"
        );

        pause.release_for_soft_stop();

        tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .expect("the soft stop must release the parked turn, not deadlock it")
            .expect("the waiter task completes");
        assert!(
            pause.painted_paused.load(Ordering::SeqCst),
            "the row is still showing paused, so the debt survives the release \
             — `settle` is what repays it"
        );
    }

    /// The row is repaid at turn end even though the gate was already reopened
    /// by the soft stop: the two are different facts, and only one of them is
    /// what the dashboard is looking at.
    #[test]
    fn settle_repays_a_row_left_painted_paused_by_a_soft_stop() {
        let pause = LeadPause::new();
        let (in_tx, mut in_rx) = mpsc::unbounded_channel();

        pause.control(AgentControl::Pause, &in_tx);
        let _ = in_rx.try_recv();
        pause.release_for_soft_stop();
        pause.settle(&in_tx);

        match in_rx.try_recv() {
            Ok(Inbound::Status { agent, status }) => {
                assert_eq!(agent, LEAD);
                assert_eq!(status, AgentStatus::WaitingInput);
            }
            other => panic!("expected the row to be unlatched at turn end, got {other:?}"),
        }
    }

    /// A turn that was never paused owes the dashboard nothing — `settle` must
    /// not invent a status that pre-empts the turn's real outcome.
    #[test]
    fn settle_is_silent_when_the_turn_was_never_paused() {
        let pause = LeadPause::new();
        let (in_tx, mut in_rx) = mpsc::unbounded_channel();
        pause.settle(&in_tx);
        assert!(in_rx.try_recv().is_err());
    }

    /// Restart is not a lead verb (#1219 leaves it out of scope, by the
    /// fleet's own rule): the lead IS the session, so there is no retained
    /// spec to respawn from and re-dispatching is just submitting the prompt
    /// again. It must leave the turn exactly as it found it — in particular it
    /// must not tell the dashboard about a respawn that never happened.
    #[test]
    fn restart_is_not_a_lead_verb_and_leaves_the_turn_untouched() {
        let (in_tx, mut in_rx) = mpsc::unbounded_channel();

        let paused = LeadPause::new();
        paused.control(AgentControl::Pause, &in_tx);
        let _ = in_rx.try_recv();
        paused.control(AgentControl::Restart, &in_tx);
        assert!(
            paused.painted_paused.load(Ordering::SeqCst),
            "a paused lane stays paused"
        );
        assert!(*paused.tx.borrow(), "and stays parked");

        let running = LeadPause::new();
        running.control(AgentControl::Restart, &in_tx);
        assert!(
            !running.painted_paused.load(Ordering::SeqCst),
            "a running lane stays running"
        );
        assert!(!*running.tx.borrow(), "and the gate is never touched");

        assert!(
            in_rx.try_recv().is_err(),
            "restart emits no status — the row must not claim a respawn"
        );
    }

    /// Acceptance (#1219): a stale pause/resume is a no-op, never an error.
    /// The keystroke can land after every gate receiver is gone — the turn it
    /// aimed at has ended — and the failed `send` is dropped rather than
    /// reported, because there is nothing left to pause and nothing the user
    /// did wrong.
    #[test]
    fn a_stale_lead_control_is_a_no_op_not_an_error() {
        let mut pause = LeadPause::new();
        let (in_tx, _in_rx) = mpsc::unbounded_channel();
        // Drop this seam's only receiver by swapping in one from an unrelated
        // channel — the state the driver is in once the turn it belonged to
        // has ended and its gate has gone with it.
        pause.gate = Arc::new(subsession::WatchGate(watch::channel(false).1));

        pause.control(AgentControl::Pause, &in_tx);
        pause.control(AgentControl::Resume, &in_tx);
    }
}
