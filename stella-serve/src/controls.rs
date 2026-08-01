// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Mid-turn controls: the pause gate and the steering tap, as `Send` handles.
//!
//! The engine already models both — `stella_core::ports::TurnGate` parks a turn
//! at its next step boundary, `TurnSteering` injects queued user messages there
//! — but until #932 neither port was reachable over HTTP: a served turn ran to
//! completion or was cancelled outright. This module is the bridge. It follows
//! the [`crate::pending::Pending`] pattern exactly: the session (turn) object is
//! exclusively owned by whichever `/events` stream is running, so anything a
//! POST handler needs to reach mid-turn must be a cloneable, `Send` handle held
//! by the registry entry instead.
//!
//! [`Controls::new`] returns the two halves. [`Controls`] (the sender half)
//! lives on the registry `Entry` and answers `POST /v1/turns/{id}/pause`,
//! `/resume` and `/steer`. [`ControlPorts`] (the receiver half) crosses onto the
//! session thread, where `run_session` lends the port impls to the engine.
//!
//! # Cancel must release the gate
//!
//! `Engine::run_step` checks cancellation before parking on the gate and again
//! immediately after it releases — but nothing *inside* `wait_if_paused` looks
//! at the cancel latch. A turn that is paused when its cancel arrives therefore
//! stays parked until something flips the gate, holding its OS thread for a
//! resume that is never coming. Two mechanisms close that, and both are load-
//! bearing:
//!
//! - Every cancel path also calls [`Controls::resume`] (see `Session::cancel`,
//!   `Drop for Session`, and `routes::handle_cancel`).
//! - [`PauseGate`] treats a dropped sender as *resumed*, so a turn whose entry
//!   was torn down without an explicit resume still unparks, observes the
//!   cancel latch, and unwinds. This is why [`ControlPorts`] must never carry a
//!   sender clone onto the session thread: a thread that holds its own gate's
//!   sender can park on it forever.

use std::sync::{Arc, Mutex};

use stella_core::ports::{TurnGate, TurnSteering};
use tokio::sync::watch;

/// The sender half: pause/resume and steering, cloneable and `Send`, held by
/// the turn's registry entry alongside its [`crate::pending::Pending`].
#[derive(Clone)]
pub(crate) struct Controls {
    /// `true` while the turn is asked to hold at its next step boundary.
    ///
    /// A `watch` channel rather than an `AtomicBool` because pause is not a
    /// flag the engine polls — it *parks* on it, and the release has to wake
    /// the parked future. `watch` is runtime-agnostic, so the flip crosses
    /// from the server runtime to the session thread's current-thread runtime
    /// the same way the `Pending` one-shots do.
    pause: Arc<watch::Sender<bool>>,
    steering: Arc<SteerQueue>,
}

/// The receiver half, moved onto the session thread exactly once.
///
/// Deliberately *not* `Clone` and deliberately without a `watch::Sender`: see
/// the module docs — the session thread holding its own gate's sender would
/// defeat the dropped-sender-means-resumed release path.
pub(crate) struct ControlPorts {
    gate: PauseGate,
    steering: Arc<SteerQueue>,
}

impl Controls {
    /// A fresh control pair for one turn.
    pub(crate) fn new() -> (Controls, ControlPorts) {
        let (pause_tx, pause_rx) = watch::channel(false);
        let steering = Arc::new(SteerQueue::default());
        (
            Controls {
                pause: Arc::new(pause_tx),
                steering: Arc::clone(&steering),
            },
            ControlPorts {
                gate: PauseGate(pause_rx),
                steering,
            },
        )
    }

    /// Hold the turn at its next step boundary. Idempotent — pausing a paused
    /// turn is a no-op, not an error.
    pub(crate) fn pause(&self) {
        self.pause.send_replace(true);
    }

    /// Let a held turn proceed. Idempotent, and also the release every cancel
    /// path must perform — see the module docs.
    pub(crate) fn resume(&self) {
        self.pause.send_replace(false);
    }

    /// Queue a user message for injection at the turn's next step boundary.
    ///
    /// The engine drains the queue oldest-first, pushes each message into the
    /// transcript, and emits `AgentEvent::Steered` per message — so the host
    /// sees its own steer echoed on the event stream.
    pub(crate) fn steer(&self, text: String) {
        self.steering.push(text);
    }
}

impl ControlPorts {
    /// Split into the two port impls `run_session` lends to the engine.
    pub(crate) fn into_ports(self) -> (PauseGate, Arc<SteerQueue>) {
        (self.gate, self.steering)
    }
}

/// `TurnGate` over the watch receiver: parks while the sender holds `true`.
///
/// A dropped sender reads as *resumed* — the same posture as the CLI's worker
/// gate, and here it is a correctness requirement, not merely politeness: the
/// entry (and with it the sender) can be dropped while the turn is parked, and
/// the turn must then observe its cancel latch rather than sleep forever.
pub(crate) struct PauseGate(watch::Receiver<bool>);

#[async_trait::async_trait]
impl TurnGate for PauseGate {
    async fn wait_if_paused(&self) {
        let mut rx = self.0.clone();
        while *rx.borrow() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

/// `TurnSteering` over a shared queue. The drain is destructive by the trait's
/// contract — whatever it returns *will* be injected.
///
/// `soft_stop_requested` is a hard `false`: the HTTP surface deliberately does
/// not expose the soft stop (issue #932 scopes the wire to steer, pause and
/// resume), so no state exists that could request one.
#[derive(Default)]
pub(crate) struct SteerQueue {
    queue: Mutex<Vec<String>>,
}

impl SteerQueue {
    fn push(&self, text: String) {
        self.queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(text);
    }
}

impl TurnSteering for SteerQueue {
    fn drain_steering(&self) -> Vec<String> {
        std::mem::take(&mut *self.queue.lock().unwrap_or_else(|p| p.into_inner()))
    }

    fn soft_stop_requested(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate must not park an unpaused turn — `wait_if_paused` on a fresh
    /// pair returns immediately or the engine would stall on every step.
    #[tokio::test]
    async fn an_unpaused_gate_does_not_park() {
        let (_controls, ports) = Controls::new();
        let (gate, _) = ports.into_ports();
        gate.wait_if_paused().await;
    }

    /// Pause parks; resume releases the parked future. This is the wake path
    /// `POST /v1/turns/{id}/resume` rides.
    #[tokio::test]
    async fn resume_wakes_a_parked_gate() {
        let (controls, ports) = Controls::new();
        let (gate, _) = ports.into_ports();
        controls.pause();

        let parked = tokio::spawn(async move {
            gate.wait_if_paused().await;
        });
        // The parked future must still be pending after the pause…
        tokio::task::yield_now().await;
        assert!(!parked.is_finished(), "a paused gate must park");

        controls.resume();
        tokio::time::timeout(std::time::Duration::from_secs(5), parked)
            .await
            .expect("resume must wake the parked gate")
            .unwrap();
    }

    /// Dropping the sender half releases a parked gate. This is what stops a
    /// paused turn whose entry was torn down from holding its OS thread
    /// forever — see the module docs.
    #[tokio::test]
    async fn a_dropped_sender_reads_as_resumed() {
        let (controls, ports) = Controls::new();
        let (gate, _) = ports.into_ports();
        controls.pause();

        let parked = tokio::spawn(async move {
            gate.wait_if_paused().await;
        });
        tokio::task::yield_now().await;
        assert!(!parked.is_finished());

        drop(controls);
        tokio::time::timeout(std::time::Duration::from_secs(5), parked)
            .await
            .expect("a dropped sender must release the gate")
            .unwrap();
    }

    /// Steered messages drain oldest-first, and the drain empties the queue —
    /// the injection order is the order the host posted.
    #[test]
    fn steering_drains_in_post_order_and_empties() {
        let (controls, ports) = Controls::new();
        let (_, steering) = ports.into_ports();
        controls.steer("first".to_string());
        controls.steer("second".to_string());
        assert_eq!(steering.drain_steering(), vec!["first", "second"]);
        assert!(steering.drain_steering().is_empty());
    }
}
