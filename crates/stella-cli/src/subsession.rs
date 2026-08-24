//! Deck sub-sessions — one dedicated engine session per dispatched request.
//!
//! The deck's contract is "input never blocks", but until now *dispatch* did:
//! one lead conversation ran prompts strictly FIFO, so a prompt submitted
//! mid-turn waited for the whole current turn. Sub-sessions close that gap:
//! when the lead is busy, the driver hands the prompt to a dedicated worker
//! session (`req:<n>`), and `task_assign` hands a board task to a dedicated
//! worker (`sub:<task-id>`). Each worker is a real engine session — its own
//! provider, tool registry, budget guard, execution row (linked to the deck's
//! session id for replay), and event lane in the deck — running on its own OS
//! thread with a current-thread runtime, because the engine's turn future is
//! deliberately not `Send` (the same bridge `fleet_cmd::EngineWorker` uses).
//!
//! Results come back three ways, none of which block the deck: live events on
//! the worker's lane (watch it from the Agents tab), a persist-until-read
//! notification when it finishes (the `/inbox` flow), and — for task workers —
//! the board task auto-completing on success.
//!
//! Scope (v1, documented rather than implied): workers run the raw engine
//! step-loop with native tools only (no MCP set, no custom tools — an
//! autonomous worker runs on the built-in surface alone), recall is skipped
//! in favor of latency, and delegation is not recursive — a worker's own
//! `task_assign` requests are reported on its lane instead of spawning.

mod closeout;

use std::collections::HashMap;
use std::sync::Arc;

use stella_core::Engine;
use stella_core::tasks::SpawnRequest;
use stella_protocol::{AgentEvent, CompletionMessage};
use stella_tui::{AgentMeta, AgentStatus, Inbound};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::{oneshot, watch};

use crate::agent;
use crate::command_deck::{LEAD, close_turn_stream, now_ms, prompt_line, spawn_forwarder};
use crate::config::Config;
use crate::runtime::TokioSleeper;

/// How many sub-session workers may run at once. Prompts (and task
/// assignments) beyond this wait in the driver's backlog and dispatch as
/// slots free — the cap bounds provider concurrency and CPU, not the queue.
pub(crate) const MAX_CONCURRENT: usize = 3;

/// How a worker ended — the supervisor's bookkeeping distinguishes the user
/// stopping a worker from the worker failing (a stop must not read as a
/// failure, must not auto-complete a task, and needs no inbox notification —
/// the user was there).
pub(crate) enum WorkerEnd {
    Done,
    Failed(String),
    /// Stopped by the user (Agents tab `s`, or Esc on the worker's lane).
    Stopped,
}

/// Messages the driver's supervisor channel carries. Spawn requests travel
/// tap → driver (a tool call cannot spawn a thread that outlives its turn);
/// endings travel worker → driver (bookkeeping + backlog drain).
pub(crate) enum SupervisorMsg {
    /// A `task_assign` request drained from the lead's tool tap.
    SpawnTask(QueuedSpawn),
    /// A worker finished (its thread is exiting).
    Ended {
        lane: String,
        /// Which spawn of this lane ended (from [`SubSessions::started`]) —
        /// the bookkeeping frees a lane only for the worker that actually
        /// ended, so a late `Ended` can never tear down a replacement.
        generation: u64,
        /// The worker's execution row, when the store recorded one — the
        /// driver stamps the post-completion board mirror against it.
        execution_id: Option<i64>,
        /// USD the worker's turn spent — metered into the session's parent
        /// budget guard by the driver (the L-E9 discipline: child spend
        /// always reaches the parent's ledger).
        cost_usd: f64,
        end: WorkerEnd,
    },
}

/// Driver-side sub-session bookkeeping: the live-worker count, the lane
/// counter for `req:<n>` prompt workers, and each live worker's stop signal.
pub(crate) struct SubSessions {
    active: usize,
    next_req: u64,
    /// Monotonic spawn counter: each start stamps its worker, and `ended`
    /// only frees a lane for the generation that actually ended.
    next_generation: u64,
    /// The generation watermark below which a worker predates the CURRENT
    /// task board — moved to `next_generation` by every `/clear` (#1692).
    /// See [`Self::seal_task_board`].
    board_epoch: u64,
    stops: HashMap<String, (u64, oneshot::Sender<()>)>,
    /// Live workers' pause switches (watch: `true` = parked at the next
    /// step boundary).
    pauses: HashMap<String, watch::Sender<bool>>,
    /// Live workers' steering taps — the driver's half of each lane's
    /// [`SteeringTap`], so a steer aimed at a lane lands at that lane's next
    /// step boundary (#2899). Removed with the stop sender: a winding-down
    /// worker has no boundary left to inject at.
    taps: HashMap<String, Arc<SteeringTap>>,
    /// Lanes `/clear` stopped whose `Ended` has not arrived. Their deck rows
    /// come down when it does — never before, because the worker's terminal
    /// status would re-register a row removed ahead of it.
    cleared: std::collections::HashSet<String>,
    /// Lanes whose stop signal was sent but whose `Ended` has not arrived —
    /// the worker thread is still winding down (forwarder drain, store
    /// closeout). These still count as live: the slot is not free, and a
    /// respawn before the old worker settles would put two workers on one
    /// lane, sharing (and corrupting) its channels.
    winding_down: HashMap<String, u64>,
    /// Every lane's spec, retained past its end — what Restart respawns.
    specs: HashMap<String, SubSessionSpec>,
}

impl SubSessions {
    pub(crate) fn new() -> Self {
        Self {
            active: 0,
            next_req: 0,
            next_generation: 0,
            board_epoch: 0,
            stops: HashMap::new(),
            pauses: HashMap::new(),
            taps: HashMap::new(),
            cleared: std::collections::HashSet::new(),
            winding_down: HashMap::new(),
            specs: HashMap::new(),
        }
    }

    pub(crate) fn has_slot(&self) -> bool {
        self.active < MAX_CONCURRENT
    }

    /// Register a spawned worker; returns its generation, which travels
    /// through [`spawn`] into the worker's `Ended` message, and the steering
    /// tap the worker's engine drains — minted here so the driver keeps the
    /// other handle.
    fn started(
        &mut self,
        lane: &str,
        stop: oneshot::Sender<()>,
        pause: watch::Sender<bool>,
        spec: SubSessionSpec,
    ) -> (u64, Arc<SteeringTap>) {
        let generation = self.next_generation;
        self.next_generation += 1;
        self.active += 1;
        let tap: Arc<SteeringTap> = Arc::default();
        self.stops.insert(lane.to_string(), (generation, stop));
        self.pauses.insert(lane.to_string(), pause);
        self.taps.insert(lane.to_string(), tap.clone());
        // A lane respawned after `/clear` is a new row, not a cleared one.
        self.cleared.remove(lane);
        self.specs.insert(lane.to_string(), spec);
        (generation, tap)
    }

    /// Inject `text` at `lane`'s next step boundary. `false` when no worker
    /// is live there, or the one that is has made its last model call and
    /// is only closing out ([`SteeringTap::is_settling`]) — either way there
    /// is no boundary left, and the caller keeps the words somewhere else.
    pub(crate) fn steer(&self, lane: &str, text: String) -> bool {
        match self.taps.get(lane) {
            Some(tap) if !tap.is_settling() => {
                tap.push(text);
                true
            }
            _ => false,
        }
    }

    /// Stop every live worker for `/clear` and remember each one, so its row
    /// comes down when its `Ended` arrives ([`Self::finish_cleared`]). Lanes
    /// with no worker behind them are dropped now — spec included, so a
    /// Restart cannot revive a lane the user just cleared — and returned for
    /// the caller to deregister immediately.
    pub(crate) fn clear_lanes(&mut self) -> Vec<String> {
        let live = self.live_lanes();
        self.stop_all();
        let mut ended: Vec<String> = self
            .specs
            .keys()
            .filter(|lane| !live.contains(lane))
            .cloned()
            .collect();
        ended.sort();
        for lane in &ended {
            self.specs.remove(lane);
        }
        self.cleared.extend(live);
        ended
    }

    /// Whether `lane` was stopped by `/clear` and has now ended: `true` once,
    /// with the lane's spec dropped, so the caller deregisters its row.
    /// `false` while the worker is still live — its terminal status is still
    /// to come, and a row removed ahead of it would be re-registered.
    pub(crate) fn finish_cleared(&mut self, lane: &str) -> bool {
        if self.is_live(lane) || !self.cleared.remove(lane) {
            return false;
        }
        self.specs.remove(lane);
        true
    }

    /// Supersede the task board: every worker alive right now belongs to the
    /// board `/clear` just destroyed, and none of them may write to the new
    /// one (#1692).
    ///
    /// The spawn generation is already this driver's stale-event token —
    /// [`Self::ended`] uses it to stop a replaced worker freeing its
    /// successor's slot — and it is monotonic across every lane, so a single
    /// watermark separates "spawned before the clear" from "spawned after"
    /// with no per-lane bookkeeping and no new id to keep in step. Sealing is
    /// a pure sequencing fact: it does not stop, pause, or deregister a
    /// worker, and a sealed worker keeps its lane, its context and its output
    /// exactly as #1631 decided.
    pub(crate) fn seal_task_board(&mut self) {
        self.board_epoch = self.next_generation;
    }

    /// Register a live lane with no worker thread behind it, and return its
    /// generation. Tests about what the DRIVER does with live lanes and
    /// spawn generations want exactly this and nothing more: a real spawn
    /// needs a runtime, a config and a provider to say something about two
    /// integers and a `HashMap` key.
    #[cfg(test)]
    pub(crate) fn started_for_test(&mut self, lane: &str) -> u64 {
        let (stop_tx, _stop_rx) = oneshot::channel();
        let (pause_tx, _pause_rx) = watch::channel(false);
        self.started(
            lane,
            stop_tx,
            pause_tx,
            SubSessionSpec {
                lane: lane.to_string(),
                title: lane.to_string(),
                purpose: String::new(),
                prompt: String::new(),
                notify_title: String::new(),
                dispatched_by: None,
            },
        )
        .0
    }

    /// The driver's handle on `lane`'s steering tap, for tests that check
    /// what a steer delivered.
    #[cfg(test)]
    pub(crate) fn tap_for_test(&self, lane: &str) -> Option<Arc<SteeringTap>> {
        self.taps.get(lane).cloned()
    }

    /// Whether `generation` was spawned before the last
    /// [`Self::seal_task_board`] — i.e. whether its task-board closeout would
    /// be writing to a board that no longer exists.
    pub(crate) fn predates_task_board(&self, generation: u64) -> bool {
        generation < self.board_epoch
    }

    /// Free `lane` — but only for the generation that actually ended.
    /// `false` (nothing freed) for any other generation: a late `Ended`
    /// from a replaced worker must not tear down its replacement's channels
    /// or corrupt the active count.
    pub(crate) fn ended(&mut self, lane: &str, generation: u64) -> bool {
        if self.winding_down.get(lane) == Some(&generation) {
            self.winding_down.remove(lane);
            self.active = self.active.saturating_sub(1);
            return true;
        }
        if self.stops.get(lane).is_some_and(|(g, _)| *g == generation) {
            self.stops.remove(lane);
            self.pauses.remove(lane);
            self.taps.remove(lane);
            self.active = self.active.saturating_sub(1);
            return true;
        }
        // `specs` is retained on purpose: Restart respawns an ended lane.
        false
    }

    /// Pause (`true`) or resume (`false`) a live worker at its next step
    /// boundary. `false` when no such worker is live.
    pub(crate) fn set_paused(&mut self, lane: &str, paused: bool) -> bool {
        match self.pauses.get(lane) {
            Some(tx) => tx.send(paused).is_ok(),
            None => false,
        }
    }

    /// Whether `lane` currently has a live worker — winding-down included:
    /// a stopped worker whose `Ended` has not arrived still owns the lane
    /// (and its slot), so a respawn now must be deferred, not started.
    pub(crate) fn is_live(&self, lane: &str) -> bool {
        self.stops.contains_key(lane) || self.winding_down.contains_key(lane)
    }

    /// The retained spec for `lane`, for a Restart respawn.
    pub(crate) fn spec(&self, lane: &str) -> Option<SubSessionSpec> {
        self.specs.get(lane).cloned()
    }

    /// Drop `lane`'s retained spec — Delete's half of the ledger, so a later
    /// Restart cannot revive a row the user removed. `false` (and the spec
    /// kept) while a worker is live on the lane: the delete's deregister is
    /// deferred to its `Ended`, and the spec goes with it.
    pub(crate) fn forget(&mut self, lane: &str) -> bool {
        if self.is_live(lane) {
            return false;
        }
        self.cleared.remove(lane);
        self.specs.remove(lane).is_some()
    }

    /// Signal one worker to stop (clean cancel: its turn future drops at the
    /// next await point, exactly the lead's cancel semantics). `false` when
    /// no such worker is live — a stale stop is a no-op, never an error.
    /// The lane stays live (winding down) until its `Ended` arrives.
    pub(crate) fn stop(&mut self, lane: &str) -> bool {
        match self.stops.remove(lane) {
            Some((generation, tx)) => {
                self.winding_down.insert(lane.to_string(), generation);
                // A winding-down worker cannot be paused or steered; dropping
                // the pause sender also releases a currently-parked gate.
                self.pauses.remove(lane);
                self.taps.remove(lane);
                tx.send(()).is_ok()
            }
            None => false,
        }
    }

    /// Signal every live worker to stop (session teardown, `/clear`). Each
    /// lane winds down until its `Ended` arrives, exactly like a single stop.
    pub(crate) fn stop_all(&mut self) {
        let lanes: Vec<String> = self.stops.keys().cloned().collect();
        for lane in lanes {
            self.stop(&lane);
        }
    }

    /// How many workers are live right now. The driver refuses to
    /// navigate to another session (`SessionResume`) while this is nonzero —
    /// live workers stream into the current session's lanes and settle
    /// against its records.
    pub(crate) fn live(&self) -> usize {
        self.active
    }

    /// The next prompt-worker lane id (`req:1`, `req:2`, …).
    pub(crate) fn next_req_lane(&mut self) -> String {
        self.next_req += 1;
        format!("req:{}", self.next_req)
    }

    /// Every lane a worker has ever run on this tenancy, live or ended
    /// (specs are retained past a worker's end for Restart). Sorted for
    /// deterministic iteration. The session-switch site deregisters these
    /// rows when the deck navigates to another session — they are all
    /// terminal there (the switch refuses while workers are live).
    pub(crate) fn lanes(&self) -> Vec<String> {
        let mut lanes: Vec<String> = self.specs.keys().cloned().collect();
        lanes.sort();
        lanes
    }

    /// The lanes carrying a worker *right now* — winding-down included,
    /// exactly like [`Self::is_live`], because a stopped worker whose
    /// `Ended` has not arrived is still writing (forwarder drain, store
    /// closeout). Sorted, so a caller rendering them gets a stable string.
    ///
    /// Distinct from [`Self::lanes`], which is every lane ever started this
    /// tenancy: `/clear` names *these*, because an ended lane is not
    /// something the user needs warning about.
    pub(crate) fn live_lanes(&self) -> Vec<String> {
        let mut lanes: Vec<String> = self
            .stops
            .keys()
            .chain(self.winding_down.keys())
            .cloned()
            .collect();
        lanes.sort();
        lanes.dedup();
        lanes
    }
}

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

/// `stella_core::ports::TurnGate` over a watch channel: the turn parks at
/// its next step boundary while the driver holds `true` (Pause) and
/// continues on `false` (Resume). A dropped sender (driver gone) reads
/// as resumed — a turn must never park forever on teardown.
///
/// `pub(crate)` for one caller beyond this module: the deck's LEAD lane
/// (#1219), which builds the same adapter over its own per-turn channel and
/// hands it to `Pipeline::with_turn_gate` / `Engine::with_gate`. Deck worker
/// lanes and the deck lead sit on the same side of the deck/fleet boundary,
/// so they share this item rather than duplicating it; `fleet_cmd.rs` keeps
/// its own co-located twin precisely because it does not.
pub(crate) struct WatchGate(pub(crate) watch::Receiver<bool>);

#[async_trait::async_trait]
impl stella_core::ports::TurnGate for WatchGate {
    async fn wait_if_paused(&self) {
        let mut rx = self.0.clone();
        while *rx.borrow() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

/// A `task_assign` the lead's board queued, and the turn that queued it.
///
/// The dispatcher rides with the request rather than being read when a slot
/// frees, because a request can wait in the deck's `pending_spawns` past the
/// end of the turn that made it — by then there is nothing left to read it
/// from (#4628).
pub(crate) struct QueuedSpawn {
    pub request: SpawnRequest,
    /// The execution id of the turn whose `task_assign` queued this. `None`
    /// only when the session opened no store, which is a workspace with no
    /// telemetry rather than a lane nobody asked for.
    pub dispatched_by: Option<i64>,
}

/// Everything a worker needs to run, owned (the thread outlives the caller's
/// borrows).
#[derive(Clone)]
pub(crate) struct SubSessionSpec {
    /// Deck lane id — `req:<n>` for a dispatched prompt, `sub:<task-id>` for
    /// an assigned task.
    pub lane: String,
    /// Dashboard row title.
    pub title: String,
    /// One sentence on what the lane is for, in the words it was handed —
    /// the task's description or subject for an assigned task, the prompt's
    /// first line for a dispatched one. The SUB-AGENTS overlay's second row.
    pub purpose: String,
    /// The full prompt the worker's model receives.
    pub prompt: String,
    /// Notification title on completion (the body is the outcome).
    pub notify_title: String,
    /// The execution id of the lead turn that dispatched this lane, stamped
    /// onto its own execution row (`executions.parent_execution_id`, schema
    /// v36) so a turn page can list the lanes it fanned out (#4628).
    ///
    /// `None` for a lane a person dispatched from the composer between turns,
    /// which is most of them — no turn asked for those, and claiming one would
    /// invent a parent.
    pub dispatched_by: Option<i64>,
}

/// Build the worker prompt for a `task_assign` spawn: the task's identity,
/// then the lead's briefing verbatim (the "communication" of the tool).
pub(crate) fn task_prompt(req: &SpawnRequest) -> String {
    let mut prompt = format!(
        "You are a sub-agent dispatched to complete one task from the lead session's board.\n\n\
         Task #{}: {}\n",
        req.task_id, req.subject
    );
    if let Some(description) = &req.description {
        prompt.push_str(description);
        prompt.push('\n');
    }
    prompt.push_str(&format!(
        "\nBriefing from the lead agent:\n{}\n\n\
         Work autonomously — nobody is watching live. Complete the task, then \
         summarize what you did and how you verified it.",
        req.briefing
    ));
    prompt
}

/// The sentence the SUB-AGENTS overlay shows for an assigned task: the
/// description's first sentence when the lead wrote one, the subject
/// otherwise. The briefing is deliberately not used — it is instructions to
/// the worker, and a row is about what the task *is*.
pub(crate) fn task_purpose(req: &SpawnRequest) -> String {
    req.description
        .as_deref()
        .map(first_sentence)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| first_sentence(&req.subject))
}

/// Most characters a purpose sentence spends.
const PURPOSE_CAP: usize = 160;

/// The first sentence of `text`, on one line: cut at the first sentence
/// break or newline, whitespace collapsed, capped at [`PURPOSE_CAP`].
pub(crate) fn first_sentence(text: &str) -> String {
    let first_line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut sentence = first_line;
    // `. ` rather than `.`, so a file name like `lib.rs` does not end the
    // sentence.
    for mark in [". ", "! ", "? "] {
        if let Some(i) = sentence.find(mark) {
            sentence = &sentence[..i + 1];
        }
    }
    let joined = sentence.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() <= PURPOSE_CAP {
        joined
    } else {
        let head: String = joined.chars().take(PURPOSE_CAP - 1).collect();
        format!("{head}…")
    }
}

/// The reasoning effort a worker's calls are pinned to, as the word the
/// settings spell it — resolved through `engine_config::tuning_for`, the same
/// helper the engine builder applies (`agent::engine`), with the builder's
/// capability clamp: a catalog-confirmed non-reasoning model carries no
/// effort, so none is shown. `None` when nothing pins one. A provider that
/// ignores the pin still receives it and says so at boot (AGENTS.md
/// invariant 8's `ReasoningPosture`), so the word here is what the request
/// carries, not a promise the model honours it.
pub(crate) fn pinned_effort(cfg: &Config) -> Option<&'static str> {
    if crate::engine_config::model_supports_reasoning(cfg.provider.id, &cfg.model_id) == Some(false)
    {
        return None;
    }
    cfg.engine_settings
        .as_ref()
        .and_then(|engine| crate::engine_config::tuning_for(engine).effort)
        .map(crate::engine_config::effort_to_str)
}

/// The panic text a caught worker payload carries (`panic!("…")` is a
/// `&str`, `panic!("{x}")` a `String` — anything else has no message).
fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

/// Prefix of the `Failed` reason [`run_caught`] synthesizes — also how
/// [`spawn`] recognizes the panic path afterward (a panicked worker never
/// reached its own claim release).
const PANIC_FAILURE_PREFIX: &str = "worker panicked: ";

/// Run one worker body, converting a panic into a `Failed` ending so the
/// supervisor ALWAYS receives `Ended` — a panicking tool must cost one
/// failed worker, not a lane stuck "Running" and a leaked slot. Effective
/// in unwind builds; under `panic = "abort"` (release) the process dies in
/// the panic hook before any catch — stella-tui's hook restores the
/// terminal there, and the deck's journal hook flushes the session journal.
fn run_caught<F>(body: F) -> (Option<i64>, f64, WorkerEnd)
where
    F: FnOnce() -> (Option<i64>, f64, WorkerEnd),
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(ended) => ended,
        Err(payload) => (
            None,
            0.0,
            WorkerEnd::Failed(format!(
                "{PANIC_FAILURE_PREFIX}{}",
                panic_message(payload.as_ref())
            )),
        ),
    }
}

/// Spawn one worker. Registers its deck lane immediately (the sub-second
/// acknowledgement — the row exists before any heavy setup), then runs the
/// session on a dedicated OS thread. Never blocks the caller.
// A worker genuinely needs every one of these (identity, budget, session
// link, both channels, stop signal) — bundling them into a struct would just
// move the field list one hop away from the one call shape.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn(
    cfg: &Config,
    spec: SubSessionSpec,
    generation: u64,
    budget_limit: Option<f64>,
    session_id: String,
    workspace_name: String,
    in_tx: UnboundedSender<Inbound>,
    sup_tx: UnboundedSender<SupervisorMsg>,
    stop_rx: oneshot::Receiver<()>,
    pause_rx: watch::Receiver<bool>,
    tap: Arc<SteeringTap>,
) {
    // Delegation runs from the lead session only (see the stranded
    // `task_assign` note in `run_worker`), so the lead is every lane's
    // dispatcher — stated on the row rather than assumed by the deck.
    let mut meta = AgentMeta::new(spec.lane.clone(), spec.title.clone(), now_ms())
        .with_role("subagent")
        .with_pid(std::process::id())
        .with_purpose(spec.purpose.clone())
        .with_parent(crate::command_deck::LEAD);
    meta.model = Some(format!("{}/{}", cfg.provider.id, cfg.model_id));
    meta.effort = pinned_effort(cfg).map(str::to_string);
    let _ = in_tx.send(Inbound::Register(meta));
    let _ = in_tx.send(Inbound::Status {
        agent: spec.lane.clone(),
        status: AgentStatus::Running,
    });

    let cfg = cfg.clone();
    std::thread::spawn(move || {
        let lane = spec.lane.clone();
        let (execution_id, cost_usd, end) = run_caught(|| {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt.block_on(run_worker(
                    &cfg,
                    &spec,
                    budget_limit,
                    &session_id,
                    &in_tx,
                    stop_rx,
                    pause_rx,
                    tap,
                )),
                Err(e) => (
                    None,
                    0.0,
                    WorkerEnd::Failed(format!("worker runtime failed to start: {e}")),
                ),
            }
        });
        // A panic unwound past the worker's own closeout, so its claims are
        // still held — release them here or they block rivals until the
        // age-based sweep.
        if matches!(&end, WorkerEnd::Failed(reason) if reason.starts_with(PANIC_FAILURE_PREFIX))
            && let Some(store) = agent::open_store(&cfg.workspace_root)
        {
            let _ = store.release_file_locks_for_holder(&format!("{session_id}/{lane}"));
        }

        // Terminal lane status. On failure the Error event (already on the
        // lane via the forwarder or below) carries the reason.
        let _ = in_tx.send(Inbound::Status {
            agent: lane.clone(),
            status: match &end {
                WorkerEnd::Done => AgentStatus::Done,
                WorkerEnd::Failed(_) => AgentStatus::Failed,
                WorkerEnd::Stopped => AgentStatus::Killed,
            },
        });
        if let WorkerEnd::Failed(reason) = &end {
            let _ = in_tx.send(Inbound::Event {
                agent: lane.clone(),
                event: AgentEvent::Error {
                    message: reason.clone(),
                    retryable: false,
                },
            });
        }

        // The `/inbox` flow: a worker finishing (or failing) lands a
        // persist-until-read notification linked to this session, so the
        // user finds the result — and can open the session, replaying it if
        // needed — without having watched the lane. A user-initiated stop
        // lands none: the user was there.
        let notification = match &end {
            WorkerEnd::Done => Some((
                format!("{workspace_name}: {}", spec.notify_title),
                prompt_line(&spec.prompt, 160),
            )),
            WorkerEnd::Failed(reason) => Some((
                format!("{workspace_name}: {} — FAILED", spec.notify_title),
                format!("{} — {reason}", prompt_line(&spec.prompt, 80)),
            )),
            WorkerEnd::Stopped => None,
        };
        if let Some((title, body)) = notification {
            let _ = stella_store::NotificationStore::open_default().push(
                &stella_store::Notification::new(title, body, session_id.clone())
                    .with_session_id(session_id.clone()),
            );
        }

        let _ = sup_tx.send(SupervisorMsg::Ended {
            lane,
            generation,
            execution_id,
            cost_usd,
            end,
        });
    });
}

/// One worker session, on the calling thread's runtime: fresh provider +
/// registry + budget, its own execution row linked to the deck session, the
/// shared persist-and-forward event path, one raw engine turn raced against
/// the driver's stop signal (the same clean drop-at-await cancel the lead
/// uses) and steered through `tap` at each step boundary. Returns
/// `(execution_id, cost_usd, end)`.
#[allow(clippy::too_many_arguments)]
async fn run_worker(
    cfg: &Config,
    spec: &SubSessionSpec,
    budget_limit: Option<f64>,
    session_id: &str,
    in_tx: &UnboundedSender<Inbound>,
    stop_rx: oneshot::Receiver<()>,
    pause_rx: watch::Receiver<bool>,
    tap: Arc<SteeringTap>,
) -> (Option<i64>, f64, WorkerEnd) {
    let provider = match agent::build_provider(cfg) {
        Ok(p) => p,
        Err(e) => return (None, 0.0, WorkerEnd::Failed(e)),
    };
    // `Arc` because the lane's sub-agent dispatcher holds a `Weak` back to it
    // (`crate::subagent`) — the registry is the child's tool set, so an owning
    // handle either way would leak both.
    let registry = Arc::new(crate::write_dirs::registry_for(cfg));
    // A worker lane delegates research like any other turn. Without this the
    // `delegate` tool is still advertised (the registry registers it
    // unconditionally) and answers "sub-agents are unavailable" every time —
    // and the lane's pause gate, published below, would have nothing to reach.
    if let Err(error) = crate::subagent::install_for_session(cfg, &registry) {
        return (None, 0.0, WorkerEnd::Failed(error));
    }
    // `false`: a sub-agent lane is headless by design — an approval gate in a
    // child refuses with the grant-path message rather than contending for the
    // lead's interactive surface.
    let active_rules = crate::rules::enforce_workspace_rules(
        &registry,
        &cfg.workspace_root,
        &cfg.authority,
        crate::rules::MidTurnAsk::Headless,
    );

    let system_prompt = agent::with_session_hook_context(
        agent::build_system_prompt(cfg, &cfg.workspace_root, &active_rules),
        cfg,
    )
    .await;
    let mut messages = vec![
        CompletionMessage::system(system_prompt),
        crate::attachments::user_message_in(&spec.prompt, &cfg.workspace_root),
    ];
    let mut budget = agent::build_budget_guard(budget_limit);
    budget.begin_turn();
    let dispatch_spend_usd = budget.session_spent_usd();

    let store = agent::open_store(&cfg.workspace_root);
    let calibration = agent::seed_calibration(&store, cfg);
    let execution = agent::begin_execution(
        &store,
        "deck-sub",
        &spec.prompt,
        cfg,
        Some(session_id),
        None,
    );
    let execution_id = execution.as_ref().map(|(_, id)| *id);
    // Which turn asked for this lane (#4628). Best-effort like the session
    // link beside it: an unrecorded dispatcher costs a row on a turn page,
    // never the lane's work.
    if let (Some((store, id)), Some(parent)) = (execution.as_ref(), spec.dispatched_by) {
        let _ = store.set_execution_parent(*id, parent);
    }

    let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
    let forwarder = spawn_forwarder(
        rx,
        execution.clone(),
        crate::cache_insight::InsightScope::from_config(cfg),
        in_tx.clone(),
        spec.lane.clone(),
        Some(registry.task_board()),
    );

    /// How the raced turn resolved, before store closeout.
    enum RacedTurn {
        Outcome(stella_core::TurnOutcome),
        Stopped,
    }
    // Claim-on-first-write over the shared tree: workers coordinate with the
    // lead, each other, and any other process in this workspace through the
    // store's lock table — no coordinator, sub-millisecond acquire, rivals
    // named in the refusal (crate::claims).
    let claims = crate::claims::ClaimTap::new(
        &*registry,
        execution.as_ref().map(|(store, _)| store.clone()),
        format!("{session_id}/{}", spec.lane),
    );
    // A worker's tool surface is the session's built-in/MCP surface, so the
    // operator's switches and the authorization gate apply to it identically
    // — a sub-agent must not be a way to reach a tool the lead was denied.
    // Deliberately NOT `session_stack`: `.stella/tools` customs are withheld
    // from autonomous lanes on purpose (#3339, see `policy_stack`'s docs) —
    // an unreviewed script's writes would bypass the claim coordination this
    // tap exists to enforce. The principal names the lane, not the human, so
    // a gate can tell the two apart.
    let permitted = agent::tool_stack::policy_stack(
        &claims,
        cfg,
        stella_core::ports::Principal::SubAgent(format!("{session_id}/{}", spec.lane)),
        registry.hook_bus(),
    );
    // Registry-born events (task board, sub-agent lifecycle) ride this
    // lane's own channel, so the lane's live view and its journal agree.
    registry.attach_events(stella_core::EventSender::new(tx.clone()));
    // `Arc` so the same gate can be published for the turn as well as
    // borrowed by it: a lane parked at Pause must not keep spending inside a
    // sub-agent it dispatched, and this lane has a dispatcher (installed
    // above) for that to reach.
    let gate: Arc<WatchGate> = Arc::new(WatchGate(pause_rx));
    // The tap rides beside the gate for the same reason: a steer at a lane
    // reaches the sub-agents that lane dispatched, as the lead's does.
    let _controls = registry.attach_turn_controls(
        stella_core::ports::TurnControls::none()
            .with_gate(gate.clone())
            .with_steering(tap.clone()),
    );
    let raced = {
        let engine = Engine::with_sleeper(
            &*provider,
            &permitted,
            // Never `engine_config_for`: that attaches the LEAD session's
            // checkpoint sink, and this worker runs concurrently with the lead
            // turn against the same `CHECKPOINT_BLOB` — see
            // `subsession_engine_config_for`.
            agent::subsession_engine_config_for(cfg),
            &TokioSleeper,
        )
        .with_calibration(&calibration)
        .with_gate(gate.as_ref())
        .with_steering(tap.as_ref());
        // The run-terminal `Complete` this lane's deck row settles on is
        // synthesized by its forwarder when the stream closes (#3379), so the
        // turn is driven on the plain sender exactly as before.
        let turn = engine.run_turn(&mut messages, &mut budget, &tx);
        // A dropped sender (driver gone at session teardown) must not read
        // as a stop — only an actual signal cancels, so the wait parks
        // forever on a closed channel and the turn always wins the race.
        let stop_wait = async move {
            if stop_rx.await.is_err() {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            outcome = turn => RacedTurn::Outcome(outcome),
            _ = stop_wait => RacedTurn::Stopped,
        }
    };
    // No boundary is left to steer at: a steer from here on is refused by
    // `SubSessions::steer` and re-parked, the same latch the lead sets.
    tap.mark_settling();
    let persistence_complete = close_turn_stream(&registry, tx, forwarder)
        .await
        .persistence_complete;
    // Release the worker's whole claim set — the stop path included (the
    // dropped turn future cannot release for itself).
    claims.release_all();

    // Honesty over silence: a worker's own task_assign calls have no
    // supervisor to spawn them (delegation is the lead's, v1) — say so on
    // the lane instead of letting the tool's confirmation stand.
    let stranded = registry.take_spawn_requests();
    if !stranded.is_empty() {
        let _ = in_tx.send(Inbound::Event {
            agent: spec.lane.clone(),
            event: AgentEvent::Text {
                text: format!(
                    "note: {} task_assign request(s) were not dispatched — delegation \
                     runs from the lead session only",
                    stranded.len()
                ),
            },
        });
    }

    let (label, cost, end) = match raced {
        RacedTurn::Outcome(stella_core::TurnOutcome::Completed { cost_usd, .. }) => {
            ("completed", cost_usd, WorkerEnd::Done)
        }
        RacedTurn::Outcome(stella_core::TurnOutcome::Aborted {
            reason, cost_usd, ..
        }) => ("aborted", cost_usd, WorkerEnd::Failed(reason)),
        RacedTurn::Stopped => (
            "cancelled",
            agent::settled_cost_since(dispatch_spend_usd, budget.session_spent_usd()),
            WorkerEnd::Stopped,
        ),
    };
    // Audit record only — deliberately NO task-board mirror. The worker's
    // private board is scaffolding for this one run, and the session's
    // `tasks` rows have exactly one writer: the driver, whose `/clear` seal
    // this thread cannot consult (#1708 — see `closeout`'s module docs).
    closeout::close_worker_execution(
        execution.as_ref(),
        &registry,
        label,
        cost,
        persistence_complete,
    );
    (execution_id, cost, end)
}

/// Drain the driver's prompt backlog into free worker slots, oldest first.
/// Stops at a slash command (those belong to the lead's dispatcher — letting
/// a later prompt jump it would also desync the deck's FIFO queue view) and
/// while dispatch is held. Sends the `PromptStarted` front-pop for every
/// prompt it takes, exactly like lead dispatch does.
#[allow(clippy::too_many_arguments)]
pub(crate) fn drain_queue(
    queue: &mut crate::session_persist::DurableQueue,
    subs: &mut SubSessions,
    dispatch_held: bool,
    cfg: &Config,
    budget_limit: Option<f64>,
    session_id: &str,
    workspace_name: &str,
    in_tx: &UnboundedSender<Inbound>,
    sup_tx: &UnboundedSender<SupervisorMsg>,
) {
    while !dispatch_held
        && subs.has_slot()
        && queue
            .front()
            .is_some_and(|text| !text.trim_start().starts_with('/'))
    {
        let Some(text) = queue.pop_front() else {
            break;
        };
        spawn_prompt_lane(
            text,
            subs,
            cfg,
            budget_limit,
            session_id,
            workspace_name,
            in_tx,
            sup_tx,
        );
    }
}

/// Start one `req:<n>` lane on `text` — the drain's loop body, and the
/// agents page's "describe a task for a new session"
/// (`WorkspaceInput::SpawnLane`). The caller has already checked
/// [`SubSessions::has_slot`]. Returns the lane id it started.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_prompt_lane(
    text: String,
    subs: &mut SubSessions,
    cfg: &Config,
    budget_limit: Option<f64>,
    session_id: &str,
    workspace_name: &str,
    in_tx: &UnboundedSender<Inbound>,
    sup_tx: &UnboundedSender<SupervisorMsg>,
) -> String {
    let lane = subs.next_req_lane();
    let _ = in_tx.send(Inbound::PromptStarted {
        agent: lane.clone(),
        text: text.clone(),
    });
    // Say so where the user is actually looking. Until now the only trace
    // of a spawn was a trace-strip row and a new dashboard lane on another
    // tab, so a prompt typed mid-turn appeared to do nothing — the deck
    // silently started a second agent and never said which one or how to
    // reach it. `ShellEvent` is the transcript-only channel (no status
    // flip, no counters, no second trace row), which is exactly right for
    // a notice about a lane other than the one it prints on.
    let _ = in_tx.send(Inbound::ShellEvent {
        agent: LEAD.to_string(),
        event: AgentEvent::Text {
            text: spawn_notice(&lane, &text),
        },
    });
    let (stop_tx, stop_rx) = oneshot::channel();
    let (pause_tx, pause_rx) = watch::channel(false);
    let spec = SubSessionSpec {
        lane: lane.clone(),
        title: prompt_line(&text, 48),
        purpose: first_sentence(&text),
        notify_title: format!("reply ready — {}", prompt_line(&text, 40)),
        prompt: text,
    };
    let (generation, tap) = subs.started(&lane, stop_tx, pause_tx, spec.clone());
    spawn(
        cfg,
        spec,
        generation,
        budget_limit,
        session_id.to_string(),
        workspace_name.to_string(),
        in_tx.clone(),
        sup_tx.clone(),
        stop_rx,
        pause_rx,
        tap,
    );
    lane
}

/// The lead-transcript notice a spawn prints: that the prompt started, which
/// lane took it, that this turn is unaffected, and — the part that was missing
/// entirely — the keys in and back out again.
///
/// The navigation line repeats on every spawn rather than only the first.
/// Workers are capped at [`MAX_CONCURRENT`], so this is a handful of lines per
/// session at worst, and a hint the user has to remember from ten minutes ago
/// is a hint that isn't there.
pub(crate) fn spawn_notice(lane: &str, prompt: &str) -> String {
    format!(
        "▸ {lane} started in parallel — {}\n  \
         This turn keeps running; {lane} does not block it.\n  \
         Open it: ↓ on an empty prompt (or ctrl-a) → ↑↓ select {lane} → ⏎.  \
         Back here: ↓ → l ({LEAD}).\n",
        prompt_line(prompt, 56),
    )
}

/// `WorkspaceInput::SpawnLane` — the agents page's "describe a task for a
/// new session": start a lane on `text` when a worker slot is free, and say
/// so on the lead transcript when none is, quoting the task so the words
/// survive the refusal (the page's composer already cleared on submit).
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_lane_or_notice(
    text: String,
    subs: &mut SubSessions,
    cfg: &Config,
    budget_limit: Option<f64>,
    session_id: &str,
    workspace_name: &str,
    in_tx: &UnboundedSender<Inbound>,
    sup_tx: &UnboundedSender<SupervisorMsg>,
) {
    if subs.has_slot() {
        spawn_prompt_lane(
            text,
            subs,
            cfg,
            budget_limit,
            session_id,
            workspace_name,
            in_tx,
            sup_tx,
        );
        return;
    }
    let _ = in_tx.send(Inbound::ShellEvent {
        agent: LEAD.to_string(),
        event: AgentEvent::Text {
            text: format!(
                "every worker slot is taken ({MAX_CONCURRENT}) — the task was not started. \
                 Stop a lane (↓ → ⌃x⌃x) or wait for one to finish, then resubmit: {text}"
            ),
        },
    });
}

/// Respawn an ended lane from its retained spec — the Restart verb. `false`
/// when the lane has no retained spec or is still live (stop it first).
#[allow(clippy::too_many_arguments)]
pub(crate) fn respawn(
    lane: &str,
    subs: &mut SubSessions,
    cfg: &Config,
    budget_limit: Option<f64>,
    session_id: &str,
    workspace_name: &str,
    in_tx: &UnboundedSender<Inbound>,
    sup_tx: &UnboundedSender<SupervisorMsg>,
) -> bool {
    if subs.is_live(lane) {
        return false;
    }
    let Some(spec) = subs.spec(lane) else {
        return false;
    };
    let (stop_tx, stop_rx) = oneshot::channel();
    let (pause_tx, pause_rx) = watch::channel(false);
    let (generation, tap) = subs.started(lane, stop_tx, pause_tx, spec.clone());
    spawn(
        cfg,
        spec,
        generation,
        budget_limit,
        session_id.to_string(),
        workspace_name.to_string(),
        in_tx.clone(),
        sup_tx.clone(),
        stop_rx,
        pause_rx,
        tap,
    );
    true
}

/// The deck lane a `task_assign` worker runs on — the task's identity, so
/// the driver can refuse a second worker for a task that already has one.
pub(crate) fn task_lane(task_id: &str) -> String {
    format!("sub:{task_id}")
}

/// Dispatch one `task_assign` spawn request (or park it if no slot is free —
/// the caller owns the pending queue).
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_task_worker(
    queued: &QueuedSpawn,
    subs: &mut SubSessions,
    cfg: &Config,
    budget_limit: Option<f64>,
    session_id: &str,
    workspace_name: &str,
    in_tx: &UnboundedSender<Inbound>,
    sup_tx: &UnboundedSender<SupervisorMsg>,
) {
    let req = &queued.request;
    let lane = task_lane(&req.task_id);
    let (stop_tx, stop_rx) = oneshot::channel();
    let (pause_tx, pause_rx) = watch::channel(false);
    let spec = SubSessionSpec {
        lane: lane.clone(),
        title: format!("task #{}: {}", req.task_id, prompt_line(&req.subject, 40)),
        purpose: task_purpose(req),
        prompt: task_prompt(req),
        notify_title: format!(
            "task #{} done — {}",
            req.task_id,
            prompt_line(&req.subject, 40)
        ),
        dispatched_by: queued.dispatched_by,
    };
    let (generation, tap) = subs.started(&lane, stop_tx, pause_tx, spec.clone());
    spawn(
        cfg,
        spec,
        generation,
        budget_limit,
        session_id.to_string(),
        workspace_name.to_string(),
        in_tx.clone(),
        sup_tx.clone(),
        stop_rx,
        pause_rx,
        tap,
    );
}

/// How long Quit waits for stopped workers to settle before abandoning
/// them. Workers cancel at their next await point and then only close out
/// (forwarder drain, store writes, claim release), so this is generous —
/// the bound exists so a wedged worker can never hold the exit hostage.
pub(crate) const QUIT_JOIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);

/// Session teardown: signal every live worker to stop, then wait — bounded
/// by `deadline` — for their `Ended` messages, so executions close out,
/// notifications land, and claims release instead of dying mid-tool as
/// detached threads at process exit. A worker that does not settle in time
/// is abandoned exactly as every worker used to be; spawn requests arriving
/// during teardown are dropped (there is no session left to run them).
pub(crate) async fn shutdown_workers(
    subs: &mut SubSessions,
    sup_rx: &mut mpsc::UnboundedReceiver<SupervisorMsg>,
    deadline: std::time::Duration,
) {
    subs.stop_all();
    let end_by = tokio::time::Instant::now() + deadline;
    while subs.live() > 0 {
        match tokio::time::timeout_at(end_by, sup_rx.recv()).await {
            Ok(Some(SupervisorMsg::Ended {
                lane, generation, ..
            })) => {
                let _ = subs.ended(&lane, generation);
            }
            Ok(Some(SupervisorMsg::SpawnTask(_))) => {}
            Ok(None) | Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests;
