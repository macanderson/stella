//! Where a prompt typed while a turn is running goes, and the tap that
//! carries it there.
//!
//! The deck's [`stella_core::ports::TurnSteering`] implementation plus the
//! routing decision the driver's mid-turn input arm makes with it. Split out
//! of `subsession.rs` under the file-size ratchet (AGENTS.md § "God files").

/// The deck's [`stella_core::ports::TurnSteering`] implementation: a tap the
/// input loop feeds (`>` steers) and an engine drains at each step boundary.
/// Interior mutability because the turn future and the input arms share it
/// immutably. Shared by reference for the lead turn (a per-turn stack local)
/// and by `Arc` for each worker lane ([`SubSessions::steer`] feeds the
/// worker's tap from the driver thread while the worker's engine drains it
/// on its own). `soft_stop` is latched only for the lead; a worker's stop
/// stays the immediate hard cancel (`SubSessions::stop`).
///
/// It also carries `settling`, which is not steering at all but belongs to the
/// same object for the same reason: it is the one piece of turn state the
/// driver's input arms and the turn future both need, and the tap is already
/// the thing they share. See [`SteeringTap::mark_settling`].
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

    /// The model is done; the turn future is now only finishing bookkeeping.
    ///
    /// There is a real gap between the two. `AgentEvent::TurnComplete` leaves the
    /// driver and the deck paints `✓ done · stage complete · 100%`, but
    /// `run_lead_turn` has not returned: it still has to drop the event
    /// channel, `await` the forwarder that persists every event of the turn,
    /// release write claims, and record the execution end — all of it disk
    /// work that scales with how much the turn did.
    ///
    /// The driver's `select!` keeps polling user input across that whole gap,
    /// and its mid-turn arm reads a prompt as a *new request* and spawns a
    /// sidecar sub-session for it. So a user who read "done" and typed the
    /// next message got a stranger agent instead of the next turn of the
    /// conversation they were having — reliably, because "done" is exactly
    /// the cue to start typing. Latching this the instant the engine returns
    /// makes that window route like the idle path it visually is.
    pub(crate) fn mark_settling(&self) {
        self.settling
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether the turn is past its last model step (see
    /// [`SteeringTap::mark_settling`]). A prompt arriving now belongs to the
    /// NEXT lead turn, never to a sidecar.
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

// Agent whistle's push-side seam (`crate::whistle::tap::Whistleable`): this
// tap is already what `>` feeds in-process (see the type's own doc comment),
// so a whistle connection reaching it needs nothing new here — only a
// listener that calls this. Not yet wired to one: the deck mints a fresh
// `SteeringTap` per turn (`command_deck.rs`'s per-turn `steering` local), and
// that file is closed to growth under the file-size ratchet
// (AGENTS.md "God files"), so publishing a session-scoped whistle socket
// there needs either a small `file-size-update` or the per-turn construction
// moved into a sibling module first. This impl exists so that follow-up is
// exactly "spawn a listener and hand it `Arc::clone(&steering) as Arc<dyn
// Whistleable>`", not "also design the trait".
impl crate::whistle::tap::Whistleable for SteeringTap {
    fn push(&self, text: String) {
        SteeringTap::push(self, text);
    }
}

/// Where a prompt submitted while the lead's turn future is still alive
/// should actually go.
///
/// This is the decision that decides whether a long collaboration stays one
/// thread. It used to be two lines inlined in the driver's `select!` arm and
/// it was wrong in the case that matters most — the moment right after the
/// deck paints "done" — so it lives here, named and tested.
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
/// [`SteeringTap::is_settling`] — the turn is past its last model step and is
/// only finishing bookkeeping.
///
/// Two rules, in order:
///
/// 1. **A settling turn owns nothing.** Its steps are over, so there is no
///    boundary left to steer at and no work left for a sidecar to run
///    *alongside*. Everything submitted here is simply the next thing the
///    user wants to say, and it continues the thread. This is the fix for the
///    reported bug: "done" is precisely the cue to start typing, so this
///    window caught nearly every follow-up prompt and handed it to a stranger
///    agent that could not see the conversation.
/// 2. **`>` steers a live turn**, anything else is a concurrent request.
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
