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
//! # A hold has to be visible, and it has to survive a reconnect
//!
//! Parking silently is not enough. A subscriber whose connection drops while
//! the turn is held reconnects to a stream that says nothing — and a held turn
//! and a slow one look identical from there. So [`PauseGate`] announces itself:
//! [`ServerFrame::TurnHeld`] when it first parks, [`ServerFrame::TurnReleased`]
//! when it lets go. Those are ordinary frames, which means they are numbered
//! and retained by `crate::history` like any other, which is exactly what makes
//! the hold re-learnable from a `?after=` replay rather than lost with the
//! connection.
//!
//! The frame sender rides the **port** half, never [`Controls`]. Two reasons,
//! both load-bearing: the registry `Entry` outlives the session thread, and a
//! sender clone parked there would hold the frame channel open forever, so
//! `Session::next_frame` would never observe the end of the stream (the same
//! shape of bug as the registry-held `EventSender` clones that stopped `stella
//! run` from exiting). And it keeps [`Controls`] free of anything the session
//! thread owns, which is the rule the next section is about.
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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use stella_core::ports::{TurnGate, TurnSteering};
use tokio::sync::watch;

use crate::frame::ServerFrame;

/// What the pause channel carries: whether the turn may cross its next step
/// boundary, and — when it may not — why.
///
/// The reason travels *with* the flag rather than beside it so there is no
/// window in which a gate has observed the hold but not yet the reason it
/// should announce.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HoldState {
    /// The turn is free to proceed.
    Running,
    /// The turn is asked to hold. `reason` is whatever the pausing host wrote,
    /// and `None` when it wrote nothing — a plain `POST /pause` need not say
    /// why.
    Held { reason: Option<String> },
}

/// The sender half: pause/resume and steering, cloneable and `Send`, held by
/// the turn's registry entry alongside its [`crate::pending::Pending`].
#[derive(Clone)]
pub(crate) struct Controls {
    /// [`HoldState::Held`] while the turn is asked to hold at its next step
    /// boundary.
    ///
    /// A `watch` channel rather than an `AtomicBool` because pause is not a
    /// flag the engine polls — it *parks* on it, and the release has to wake
    /// the parked future. `watch` is runtime-agnostic, so the flip crosses
    /// from the server runtime to the session thread's current-thread runtime
    /// the same way the `Pending` one-shots do.
    pause: Arc<watch::Sender<HoldState>>,
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
    ///
    /// `frames` is the session's outbound channel, and the clone taken here
    /// goes onto the *port* half only — see the module docs on why the
    /// registry-held [`Controls`] must not carry one.
    pub(crate) fn new(frames: crate::backlog::FrameSink) -> (Controls, ControlPorts) {
        let (pause_tx, pause_rx) = watch::channel(HoldState::Running);
        let steering = Arc::new(SteerQueue::default());
        (
            Controls {
                pause: Arc::new(pause_tx),
                steering: Arc::clone(&steering),
            },
            ControlPorts {
                gate: PauseGate {
                    pause: pause_rx,
                    frames,
                },
                steering,
            },
        )
    }

    /// Hold the turn at its next step boundary, optionally saying why.
    /// Idempotent — pausing a paused turn is a no-op, not an error; a second
    /// pause simply replaces the reason the eventual
    /// [`ServerFrame::TurnHeld`] will carry, since the frame is emitted when
    /// the boundary is reached, not when the request lands.
    pub(crate) fn pause(&self, reason: Option<String>) {
        self.pause.send_replace(HoldState::Held { reason });
    }

    /// Let a held turn proceed. Idempotent, and also the release every cancel
    /// path must perform — see the module docs.
    pub(crate) fn resume(&self) {
        self.pause.send_replace(HoldState::Running);
    }

    /// Whether this turn is currently asked to hold.
    ///
    /// The read-side twin of the [`ServerFrame::TurnHeld`] frame, for a host
    /// that wants the state without replaying a stream — `GET
    /// /v1/sessions/{id}` reports it. Note what it means precisely: the turn
    /// has been *asked* to hold. It reads `true` from the moment `POST /pause`
    /// lands, which is a little before the turn actually parks at its next
    /// boundary. The frame is the one that says the boundary was reached.
    pub(crate) fn is_paused(&self) -> bool {
        matches!(*self.pause.borrow(), HoldState::Held { .. })
    }

    /// Queue a user message for injection at the turn's next step boundary.
    ///
    /// The engine drains the queue oldest-first, pushes each message into the
    /// transcript, and emits `AgentEvent::Steered` per message — so the host
    /// sees its own steer echoed on the event stream.
    pub(crate) fn steer(&self, text: String) {
        self.steering.push(text);
    }

    /// Ask the turn to stop at its next step boundary, keeping everything it
    /// has completed. The graceful half of a shutdown drain (#1131).
    ///
    /// Distinct from [`crate::session::Session::cancel`] in exactly one way
    /// that matters here: cancel wakes a parked reverse request *immediately*,
    /// which throws away a model call the host is already paying for. A soft
    /// stop lets that call land, folds its result into the transcript, and
    /// ends the turn at the boundary after it. That is the difference between
    /// a redeploy that costs one in-flight generation per turn and one that
    /// costs nothing.
    ///
    /// It follows that a soft stop is *not* a bound: a turn parked on a host
    /// that never answers will never reach a boundary to observe it. The drain
    /// pairs it with a grace period and cancels whatever outlives that — see
    /// `crate::server::drain`.
    ///
    /// The pause gate is released as a matter of course, for the same reason
    /// every cancel path releases it: a paused turn is parked on the gate, not
    /// at a step boundary, so it cannot observe a stop request until something
    /// unparks it.
    pub(crate) fn soft_stop(&self) {
        self.steering.request_soft_stop();
        self.resume();
    }
}

impl ControlPorts {
    /// Split into the two port impls `run_session` lends to the engine.
    pub(crate) fn into_ports(self) -> (PauseGate, Arc<SteerQueue>) {
        (self.gate, self.steering)
    }
}

/// `TurnGate` over the watch receiver: parks while the sender holds
/// [`HoldState::Held`], and says so on the frame stream while it does.
///
/// A dropped sender reads as *resumed* — the same posture as the CLI's worker
/// gate, and here it is a correctness requirement, not merely politeness: the
/// entry (and with it the sender) can be dropped while the turn is parked, and
/// the turn must then observe its cancel latch rather than sleep forever.
pub(crate) struct PauseGate {
    pause: watch::Receiver<HoldState>,
    /// Where [`ServerFrame::TurnHeld`] / [`ServerFrame::TurnReleased`] go.
    ///
    /// Straight onto the session's frame channel, bypassing the `AgentEvent`
    /// forwarder — the same route `RemoteProvider` and `RemoteToolExecutor`
    /// take, and for the same reason: it is the receive funnel
    /// (`Session::next_seq_frame`) that assigns the `seq` and retains the
    /// frame, and only a frame that went through there is replayable.
    frames: crate::backlog::FrameSink,
}

#[async_trait::async_trait]
impl TurnGate for PauseGate {
    async fn wait_if_paused(&self) {
        let mut rx = self.pause.clone();
        // Announce at most once per park, and only if we actually parked: a
        // running turn crosses this on every step and must stay silent, or the
        // stream would carry two frames per step for a feature nobody used.
        let mut announced = false;
        loop {
            let HoldState::Held { reason } = rx.borrow().clone() else {
                break;
            };
            if !announced {
                announced = true;
                // `let _ =`, never `expect`: the release path this gate exists
                // for is a torn-down turn, and by then the receiver may already
                // be gone. A panic here would take the session thread with it.
                let _ = self.frames.send(ServerFrame::TurnHeld { reason });
            }
            if rx.changed().await.is_err() {
                break;
            }
        }
        if announced {
            // Both exits of the loop above are a release — an explicit resume
            // and a dropped sender alike — so the paired frame is emitted here
            // rather than on the resume arm only. Losing it on the
            // dropped-sender path would leave a host that reconnects after a
            // cancel replaying a hold that has already ended.
            let _ = self.frames.send(ServerFrame::TurnReleased);
        }
    }
}

/// `TurnSteering` over a shared queue. The drain is destructive by the trait's
/// contract — whatever it returns *will* be injected.
///
/// `soft_stop_requested` has no HTTP route behind it: #932 scoped the wire to
/// steer, pause and resume, and that is still the case. What sets it is the
/// **shutdown drain** (#1131) — a process-level event, not a request — so the
/// latch is reachable from `crate::server` and from nowhere a client can
/// address.
#[derive(Default)]
pub(crate) struct SteerQueue {
    queue: Mutex<Vec<String>>,
    /// Latched, never cleared: a turn asked to stop does not un-ask. Relaxed
    /// ordering is sufficient because the flag guards nothing but itself —
    /// the engine reads it at a step boundary and needs only to observe the
    /// write eventually, and "eventually" here is bounded by the drain's own
    /// grace period.
    soft_stop: AtomicBool,
}

impl SteerQueue {
    fn push(&self, text: String) {
        self.queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(text);
    }

    fn request_soft_stop(&self) {
        self.soft_stop.store(true, Ordering::Relaxed);
    }
}

impl TurnSteering for SteerQueue {
    fn drain_steering(&self) -> Vec<String> {
        std::mem::take(&mut *self.queue.lock().unwrap_or_else(|p| p.into_inner()))
    }

    fn soft_stop_requested(&self) -> bool {
        self.soft_stop.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// A control pair plus the receiving end of the frames its gate emits.
    fn controls() -> (Controls, ControlPorts, mpsc::UnboundedReceiver<ServerFrame>) {
        let (frame_tx, frame_rx) = mpsc::unbounded_channel();
        let (controls, ports) = Controls::new(crate::backlog::FrameSink::new(
            frame_tx,
            crate::backlog::FrameBacklog::new(crate::backlog::DEFAULT_MAX_QUEUED_FRAMES),
        ));
        (controls, ports, frame_rx)
    }

    /// The frames the gate emitted, as their wire tags.
    fn tags(frames: &mut mpsc::UnboundedReceiver<ServerFrame>) -> Vec<String> {
        let mut tags = Vec::new();
        while let Ok(frame) = frames.try_recv() {
            let json = serde_json::to_value(&frame).expect("a frame serializes");
            tags.push(json["type"].as_str().unwrap_or_default().to_string());
        }
        tags
    }

    /// The gate must not park an unpaused turn — `wait_if_paused` on a fresh
    /// pair returns immediately or the engine would stall on every step — and
    /// it must say nothing, or every step of every turn would carry a hold it
    /// never had.
    #[tokio::test]
    async fn an_unpaused_gate_does_not_park_and_stays_silent() {
        let (_controls, ports, mut frames) = controls();
        let (gate, _) = ports.into_ports();
        gate.wait_if_paused().await;
        assert!(
            tags(&mut frames).is_empty(),
            "an unheld turn announces nothing"
        );
    }

    /// Pause parks; resume releases the parked future. This is the wake path
    /// `POST /v1/turns/{id}/resume` rides — and the hold is announced on the
    /// frame stream at the moment the gate parks, then closed when it lets go.
    #[tokio::test]
    async fn resume_wakes_a_parked_gate_and_the_hold_is_announced_then_closed() {
        let (controls, ports, mut frames) = controls();
        let (gate, _) = ports.into_ports();
        controls.pause(Some("waiting on a human".to_string()));
        assert!(controls.is_paused(), "the read side sees the hold");

        let parked = tokio::spawn(async move {
            gate.wait_if_paused().await;
        });
        // The parked future must still be pending after the pause…
        tokio::task::yield_now().await;
        assert!(!parked.is_finished(), "a paused gate must park");
        let held = frames.try_recv().expect("the park is announced");
        let held = serde_json::to_value(&held).unwrap();
        assert_eq!(held["type"], "turn_held");
        assert_eq!(held["reason"], "waiting on a human");

        controls.resume();
        tokio::time::timeout(std::time::Duration::from_secs(5), parked)
            .await
            .expect("resume must wake the parked gate")
            .unwrap();
        assert!(!controls.is_paused(), "the read side sees the release");
        assert_eq!(tags(&mut frames), vec!["turn_released"]);
    }

    /// Dropping the sender half releases a parked gate. This is what stops a
    /// paused turn whose entry was torn down from holding its OS thread
    /// forever — see the module docs.
    ///
    /// It is also the release a naive implementation forgets to announce: the
    /// obvious place to emit `turn_released` is the resume arm, and this path
    /// does not go through one. A host that reconnects after a cancel would
    /// then replay a hold that had already ended and park itself forever.
    #[tokio::test]
    async fn a_dropped_sender_reads_as_resumed_and_still_closes_the_hold() {
        let (controls, ports, mut frames) = controls();
        let (gate, _) = ports.into_ports();
        controls.pause(None);

        let parked = tokio::spawn(async move {
            gate.wait_if_paused().await;
        });
        tokio::task::yield_now().await;
        assert!(!parked.is_finished());
        assert_eq!(tags(&mut frames), vec!["turn_held"]);

        drop(controls);
        tokio::time::timeout(std::time::Duration::from_secs(5), parked)
            .await
            .expect("a dropped sender must release the gate")
            .unwrap();
        assert_eq!(
            tags(&mut frames),
            vec!["turn_released"],
            "the dropped-sender release must close the hold it opened"
        );
    }

    /// A gate whose frame receiver is already gone must still release. The
    /// torn-down turn is precisely the case the dropped-sender path exists for,
    /// and by then nobody is reading frames — a send that panicked there would
    /// take the session thread down instead of letting the turn unwind.
    #[tokio::test]
    async fn a_gate_whose_stream_is_gone_still_releases() {
        let (controls, ports, frames) = controls();
        let (gate, _) = ports.into_ports();
        controls.pause(None);
        drop(frames);

        let parked = tokio::spawn(async move {
            gate.wait_if_paused().await;
        });
        tokio::task::yield_now().await;
        controls.resume();
        tokio::time::timeout(std::time::Duration::from_secs(5), parked)
            .await
            .expect("a closed frame channel must not wedge the gate")
            .unwrap();
    }

    /// Steered messages drain oldest-first, and the drain empties the queue —
    /// the injection order is the order the host posted.
    #[test]
    fn steering_drains_in_post_order_and_empties() {
        let (controls, ports, _frames) = controls();
        let (_, steering) = ports.into_ports();
        controls.steer("first".to_string());
        controls.steer("second".to_string());
        assert_eq!(steering.drain_steering(), vec!["first", "second"]);
        assert!(steering.drain_steering().is_empty());
    }
}
