//! Where a prompt typed while a turn is running goes, and the tap that
//! carries it there.
//!
//! The deck's [`stella_core::ports::TurnSteering`] tap lives here, with the
//! route the driver's mid-turn input arm picks by it. Split out of
//! `subsession.rs` under the file-size ratchet (AGENTS.md § "God files").

/// The deck's [`stella_core::ports::TurnSteering`]: a tap the input loop
/// feeds (`>` steers) and an engine drains at each step boundary.
///
/// The fields lock and latch, so the tap can be held by shared reference.
/// The turn future and the input arms both hold it that way. The lead turn
/// takes it as a per-turn stack local. Each worker lane takes it by `Arc`.
/// [`SubSessions::steer`](super::SubSessions::steer) feeds a worker's tap
/// from the driver thread, and the worker's own engine drains it.
///
/// `soft_stop` is latched for the lead alone. A worker's stop stays a hard
/// cancel, sent at once (`SubSessions::stop`).
///
/// The tap also holds `settling`, which is not steering. It sits here for
/// the reason the queue does: the driver's input arms and the turn future
/// both need it, and the tap is what they already share. See
/// [`SteeringTap::mark_settling`].
#[derive(Default)]
pub(crate) struct SteeringTap {
    queue: std::sync::Mutex<Vec<String>>,
    soft_stop: std::sync::atomic::AtomicBool,
    settling: std::sync::atomic::AtomicBool,
}

impl SteeringTap {
    pub(crate) fn push(&self, text: String) {
        self.queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(text);
    }
    pub(crate) fn request_soft_stop(&self) {
        self.soft_stop
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// The model is done. The turn future now only does bookkeeping.
    ///
    /// There is a real gap between the two. `AgentEvent::TurnComplete`
    /// leaves the driver, and the deck paints `✓ done · stage complete ·
    /// 100%`. But `run_lead_turn` has not returned. It still has to drop
    /// the event channel. It has to `await` the forwarder that saves every
    /// event of the turn. It has to free write claims and log the end of
    /// the run. All of that is disk work, and it grows with how much the
    /// turn did.
    ///
    /// The driver's `select!` keeps reading user input across that gap. Its
    /// mid-turn arm takes a prompt as a *new request*, and spawns a sidecar
    /// sub-session for it. So a user who read "done" and typed the next
    /// message got a stranger agent. What they wanted was the next turn of
    /// the thread they were in. It went wrong nearly every time, because
    /// "done" is just the cue to start typing. Latching this the moment the
    /// engine returns makes that window route like the idle path it looks
    /// like.
    pub(crate) fn mark_settling(&self) {
        self.settling
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether the turn is past its last model step (see
    /// [`SteeringTap::mark_settling`]). A prompt that lands now belongs to
    /// the NEXT lead turn, never to a sidecar.
    pub(crate) fn is_settling(&self) -> bool {
        self.settling.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl stella_core::ports::TurnSteering for SteeringTap {
    fn drain_steering(&self) -> Vec<String> {
        std::mem::take(&mut *self.queue.lock().unwrap_or_else(|p| p.into_inner()))
    }
    fn soft_stop_requested(&self) -> bool {
        // Latched: set once, read at every boundary until the turn ends.
        self.soft_stop.load(std::sync::atomic::Ordering::SeqCst)
    }
}

// Agent whistle's push-side seam (`crate::whistle::tap::Whistleable`). This
// tap is what `>` feeds in process; see the type's own doc comment. So a
// whistle link that reaches it needs nothing new here. It needs a listener
// that calls this.
//
// Nothing calls it so far. The deck makes a fresh `SteeringTap` each turn,
// in `command_deck.rs`'s per-turn `steering` local. That file is closed to
// growth under the file-size ratchet (AGENTS.md "God files"). A
// session-scoped whistle socket there needs a small `file-size-update`
// first, or the per-turn setup moved to a sibling module. This impl is here
// so that step is just "spawn a listener and hand it
// `Arc::clone(&steering) as Arc<dyn Whistleable>`".
impl crate::whistle::tap::Whistleable for SteeringTap {
    fn push(&self, text: String) {
        SteeringTap::push(self, text);
    }
}

/// Where a prompt goes when it is sent while the lead's turn future is
/// still alive.
///
/// This choice decides whether a long thread stays one thread. It lives
/// here, named and tested, and not inline in the driver's `select!` arm.
/// The case that counts most is the moment right after the deck paints
/// "done", and that one is easy to get wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MidTurnRoute {
    /// Inject at the running turn's next step boundary.
    Steer(String),
    /// Run as the lead's next turn, continuing this conversation. Queued
    /// without draining, so the idle arm picks it up.
    NextTurn(String),
    /// A genuinely concurrent request: backlog it for a sidecar lane.
    Sidecar(String),
}

/// Route one mid-turn submission. `settling` is
/// [`SteeringTap::is_settling`]: the turn is past its last model step, and
/// only does bookkeeping now.
///
/// Two rules, in order:
///
/// 1. **A settling turn owns nothing.** Its steps are over. No boundary is
///    left to steer at, and no work is left for a sidecar to run *beside*.
///    So what is sent here is the next thing the user wants to say, and it
///    goes on with the thread. This is the fix for the bug that was filed.
///    "done" is just the cue to start typing, so this window caught nearly
///    every follow-up prompt. It gave each one to a stranger agent that
///    could not see the thread.
/// 2. **`>` steers a live turn.** Anything else is a request to run beside
///    it.
pub(crate) fn route_mid_turn(text: String, settling: bool) -> MidTurnRoute {
    let steer = text
        .trim_start()
        .strip_prefix('>')
        .map(|rest| rest.trim_start().to_string());
    match (settling, steer) {
        (true, Some(rest)) => MidTurnRoute::NextTurn(rest),
        (true, None) => MidTurnRoute::NextTurn(text),
        (false, Some(rest)) => MidTurnRoute::Steer(rest),
        (false, None) => MidTurnRoute::Sidecar(text),
    }
}
