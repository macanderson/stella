//! Cloneable event sender with an optional synchronous, ordered boundary.
//!
//! The core remains I/O-free: callers may supply a closure that durably
//! journals an event before it is admitted to the ordinary Tokio channel.
//! Because every clone shares that closure (and any mutex it captures), the
//! durable order and channel order can be made identical across concurrent
//! producers. A paid-call producer does not return from [`EventSender::send`]
//! until the caller's persistence boundary has completed.

use std::fmt;
use std::sync::{Arc, RwLock};

use stella_protocol::AgentEvent;
use tokio::sync::mpsc::UnboundedSender;

use crate::tasks::RunningTask;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventSendError;

impl fmt::Display for EventSendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("agent event receiver is closed")
    }
}

type SendFn = dyn Fn(AgentEvent) -> Result<(), EventSendError> + Send + Sync;

#[derive(Clone)]
pub struct EventSender {
    send: Arc<SendFn>,
    /// The running-task source, shared by every clone of this sender so a
    /// host can attach it once (see the module docs). `None` until a host
    /// does, which is every non-board caller and every test.
    running_task: Arc<RwLock<Option<RunningTask>>>,
}

impl EventSender {
    /// Wrap an ordinary Tokio sender without a persistence boundary.
    pub fn new(sender: UnboundedSender<AgentEvent>) -> Self {
        Self::from_fn(move |event| sender.send(event).map_err(|_| EventSendError))
    }

    /// Build a sender from a caller-owned synchronous admission closure.
    ///
    /// Benchmark callers use this to append+flush under a shared mutex and
    /// only then enqueue the same event. The closure must not return success
    /// unless the event crossed its required durability boundary.
    pub fn from_fn(
        send: impl Fn(AgentEvent) -> Result<(), EventSendError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            send: Arc::new(send),
            running_task: Arc::new(RwLock::new(None)),
        }
    }

    /// Declare where this sender reads "which board task is running now"
    /// (SPEC 7.1's evidence ledger, #5039).
    ///
    /// Every clone of this sender — the registry's, the re-query adapter's,
    /// the one the engine drives its turn through — starts tagging from the
    /// same instant, because they share one slot.
    ///
    /// # Why the tag rides the sender, and why the slot is late-attached
    ///
    /// The tag has to be applied **synchronously, at send**. A drain would be
    /// the obvious place — a renderer or journal writer sees every event — but
    /// it sees them later, and by the time it folds a `tool_result` the board
    /// may have moved on, so the ledger would be misattributed. The emit sites
    /// are the other obvious place, and there are dozens of them: threading
    /// the running task to each is a rule every future call site has to
    /// remember, and the one that forgets is silent, which is the shape
    /// AGENTS.md #10 exists to end.
    ///
    /// Late-attached because whether anything can answer "which task is
    /// running" is a fact about the *host*, not about the sender, and it is
    /// not known where a sender is built — the same shape
    /// `ToolRegistry::enable_task_delegation` and `attach_call_measure` have.
    /// Attaching to any one clone reaches all of them, so a host wires the
    /// engine's stream and the registry's stream by wiring the sender they
    /// already share: no second sender to keep alive, and no drop order to get
    /// right.
    ///
    /// What it does not do is chase a sender it cannot see. A host that builds
    /// a *second* sender over the same channel gets a second, empty slot, and
    /// its events go out untagged — the right failure, because an event with
    /// no tag is in no task's ledger where a guessed tag would put it in the
    /// wrong one.
    ///
    /// Attaching twice replaces the source rather than layering a second one:
    /// there is one board per lane, so two sources would be two answers to a
    /// question that has one.
    pub fn attach_running_task(&self, running: RunningTask) {
        *self
            .running_task
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(running);
    }

    pub fn send(&self, event: AgentEvent) -> Result<(), EventSendError> {
        (self.send)(self.tagged(event))
    }

    /// Stamp the running task onto an event that has a slot for one and has
    /// not already been stamped.
    ///
    /// The two guards before the lock are what keep this off the hot path: a
    /// turn's stream is mostly narration, and `carries_task_tag` is a match on
    /// the case rather than a board read. An event that arrives already
    /// tagged is left alone — see `AgentEvent::stamp_task`, which is where the
    /// never-overwrite rule lives.
    fn tagged(&self, mut event: AgentEvent) -> AgentEvent {
        if !event.carries_task_tag() || event.task_id().is_some() {
            return event;
        }
        let running = self
            .running_task
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .and_then(RunningTask::current);
        if let Some(task) = running {
            event.stamp_task(&task);
        }
        event
    }

    /// Wrap this sender so a run owner's closing `Stage(Complete)` rides
    /// immediately ahead of the engine's terminal `TurnComplete`.
    ///
    /// The engine emits no stage boundary of its own: `StageKind` is the run
    /// *owner's* vocabulary, and a turn is one step of a run that may have six
    /// stages left to go (#3416). A raw run owner still owes its consumers the
    /// boundary — `stella-tui`'s `hud.stage` and `plain`'s stage rule both read
    /// it — and owes it in the order they already render, which is why this is
    /// a sender combinator rather than a send after the turn returns: the
    /// engine's `TurnComplete` is emitted *inside* the turn, so anything appended
    /// afterwards would arrive behind the terminal event some consumers stop
    /// at.
    ///
    /// Only the completed path gets a boundary, exactly as the engine's copy
    /// did — an aborted turn reached no completion, and a boundary claiming
    /// otherwise would be a HUD's last word on a run that failed. A staged run
    /// owner must **not** use this: the pipeline emits every boundary of its
    /// own, on its own schedule.
    pub fn pairing_stage_complete(&self) -> Self {
        let inner = self.clone();
        Self::from_fn(move |event| {
            if matches!(event, AgentEvent::TurnComplete { .. }) {
                inner.send(AgentEvent::Stage {
                    name: stella_protocol::StageKind::Complete.into(),
                    // The owner's vocabulary spans the whole run, not the turn
                    // that happened to trigger it (#3398's `StageScope`).
                    scope: stella_protocol::StageScope::Run,
                })?;
            }
            inner.send(event)
        })
    }
}

/// The run owner's half of the one-way ending contract (#3379).
///
/// The engine ends every turn with [`AgentEvent::TurnComplete`] and stops
/// there, because it cannot know whether its caller wants another turn. That
/// leaves the run-terminal [`AgentEvent::RunComplete`] — exactly once, last, only
/// on success — to whoever owns the run. This is that emitter, for the owners
/// whose run is a plain sequence of engine turns: the CLI's one-shot and
/// interactive paths, the deck's lead turn, a resumed turn, a fleet worker, a
/// served session. (A staged pipeline authors a richer ending of its own and
/// does not use this.)
///
/// It **observes**; it never edits. Every event it is handed reaches the inner
/// sender unchanged, `TurnComplete` included — the turn endings and the run
/// ending both appear in the journal, in order. All this type keeps is the
/// last turn's `model` and the running total of what the turns reported, so
/// the ending it authors can be summarized honestly instead of guessed from
/// configuration.
///
/// A run whose turns all failed has nothing to summarize and emits nothing: a
/// failed run ends on [`AgentEvent::Error`], never on `RunComplete`. That is the
/// same rule the engine used to apply one turn at a time.
/// # Why it seals on drop
///
/// The run ends when its event stream does, and *that* is the moment the
/// terminal event has to be emitted — `RunComplete` must be last, so anything
/// that emits it earlier is guessing that nothing else will be sent. Tying it
/// to the drop of the last sender clone makes "last" true by construction
/// rather than by every owner remembering to call a method in the right place.
/// It is also what lets owners whose turn call sits in a closed-to-growth file
/// adopt this by replacing a line instead of adding one.
///
/// The consequence to know: the inner sender must still be alive when the
/// wrapper drops. Every current owner satisfies this — each builds the wrapper
/// from a sender it goes on to close afterwards — and a receiver already gone
/// is not a corruption, only a dropped send, exactly as everywhere else here.
pub struct RunEnding {
    inner: EventSender,
    settled: std::sync::Mutex<Option<(String, f64)>>,
}

impl RunEnding {
    /// Wrap `inner` in a sender that observes the engine's turn endings and
    /// emits the run's own when the last clone of it drops.
    #[must_use]
    pub fn sealing(inner: EventSender) -> EventSender {
        let ending = Arc::new(Self {
            inner,
            settled: std::sync::Mutex::new(None),
        });
        // Synchronous, like every other sender in this module: the observation
        // is recorded before the event is admitted, so a stream that closes
        // the instant its last turn ends cannot seal on a total one turn old.
        EventSender::from_fn(move |event| {
            if let AgentEvent::TurnComplete { model, cost_usd } = &event
                && let Ok(mut settled) = ending.settled.lock()
            {
                let total = settled.as_ref().map_or(0.0, |(_, spent)| *spent) + *cost_usd;
                *settled = Some((model.clone(), total));
            }
            ending.inner.send(event)
        })
    }
}

impl Drop for RunEnding {
    fn drop(&mut self) {
        let settled = self.settled.get_mut().ok().and_then(Option::take);
        if let Some((model, cost_usd)) = settled {
            let _ = self.inner.send(AgentEvent::RunComplete { model, cost_usd });
        }
    }
}

impl From<UnboundedSender<AgentEvent>> for EventSender {
    fn from(sender: UnboundedSender<AgentEvent>) -> Self {
        Self::new(sender)
    }
}

#[cfg(test)]
mod run_ending_tests {
    use super::*;

    fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            out.push(event);
        }
        out
    }

    /// The contract a raw run's consumers actually depend on: the engine's
    /// per-turn ending goes through untouched, and the run's own follows it.
    ///
    /// This exists because losing it is silent. The wiring is one call at one
    /// call site, and when a merge dropped that call (#3379 landing against
    /// #3414, which rewrote the same lines) the only complaint was a dead-code
    /// lint on the now-uncalled constructor. Nothing said the thing that
    /// actually broke: a raw `stella run` emitted `turn_complete` and then
    /// simply stopped, so every consumer waiting for the terminal event waited
    /// forever. A lint on the producer is not a test of the behaviour.
    #[test]
    fn a_wrapped_turn_ending_is_forwarded_and_followed_by_the_run_ending() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = RunEnding::sealing(EventSender::new(tx));
        events
            .send(AgentEvent::TurnComplete {
                model: "opus".to_string(),
                cost_usd: 0.25,
            })
            .expect("the receiver is alive");
        drop(events);

        let seen = drain(&mut rx);
        assert!(
            matches!(
                seen.as_slice(),
                [
                    AgentEvent::TurnComplete { .. },
                    AgentEvent::RunComplete { model, cost_usd },
                ] if model == "opus" && (*cost_usd - 0.25).abs() < f64::EPSILON
            ),
            "the turn ending passes through unedited and the run's follows it, \
             carrying that turn's model and spend: {seen:?}"
        );
    }

    /// Several turns settle as one run ending carrying their total — and the
    /// per-turn endings are still all present, because this observes rather
    /// than replaces them.
    #[test]
    fn several_turns_end_the_run_once_with_their_total() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = RunEnding::sealing(EventSender::new(tx));
        for cost_usd in [0.1, 0.2, 0.3] {
            events
                .send(AgentEvent::TurnComplete {
                    model: "opus".to_string(),
                    cost_usd,
                })
                .expect("the receiver is alive");
        }
        drop(events);

        let seen = drain(&mut rx);
        assert_eq!(
            seen.iter()
                .filter(|e| matches!(e, AgentEvent::TurnComplete { .. }))
                .count(),
            3,
            "every turn ending survives: {seen:?}"
        );
        let terminal: Vec<&AgentEvent> = seen
            .iter()
            .filter(|e| matches!(e, AgentEvent::RunComplete { .. }))
            .collect();
        assert!(
            matches!(
                terminal.as_slice(),
                [AgentEvent::RunComplete { cost_usd, .. }]
                    if (*cost_usd - 0.6).abs() < 1e-9
            ),
            "exactly one run ending, carrying the run's total: {terminal:?}"
        );
        assert!(
            matches!(seen.last(), Some(AgentEvent::RunComplete { .. })),
            "and it is last: {seen:?}"
        );
    }

    /// A run no turn of which succeeded ends on `Error`, never on `Complete` —
    /// the same rule the engine used to apply one turn at a time.
    #[test]
    fn a_run_with_no_successful_turn_emits_no_run_ending() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = RunEnding::sealing(EventSender::new(tx));
        events
            .send(AgentEvent::Error {
                message: "provider refused".to_string(),
                retryable: false,
            })
            .expect("the receiver is alive");
        drop(events);

        let seen = drain(&mut rx);
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, AgentEvent::RunComplete { .. })),
            "a failed run must not be sealed as a success: {seen:?}"
        );
    }
}

/// The task-tagging half (#5039), tested against a real
/// [`crate::tasks::TaskBoard`] rather than a stub source: the thing that must
/// be true is that a *board* answers, not that a closure does.
#[cfg(test)]
mod task_tag_tests {
    use std::sync::Mutex;

    use stella_protocol::TaskStatus;

    use super::*;
    use crate::tasks::TaskBoard;

    fn tool_start(call_id: &str) -> AgentEvent {
        AgentEvent::ToolStart {
            call: stella_protocol::ToolCall {
                call_id: call_id.to_string(),
                name: "edit_file".to_string(),
                input: serde_json::json!({ "path": "src/auth.rs" }),
            },
            sub_agent_id: None,
            task_id: None,
        }
    }

    fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            out.push(event);
        }
        out
    }

    /// A sender with a board attached tags the work it carries with whichever
    /// task the board says is running **at the moment of the send** — and
    /// re-reads it, so moving to the next task moves the tag with it.
    ///
    /// The second half is the one a cached copy would get wrong, and it is
    /// the whole reason `RunningTask` is a closure over the board.
    #[test]
    fn work_dispatched_while_a_task_runs_is_tagged_with_that_task() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        let board = Arc::new(Mutex::new(TaskBoard::new()));
        {
            let mut guard = board.lock().expect("fresh board");
            guard.seed_from_plan(&["read the layout", "fold the rail"]);
        }
        let source = Arc::clone(&board);
        events.attach_running_task(RunningTask::from_fn(move || {
            source.lock().expect("board").running()
        }));

        // Before any task starts, work is in no task's ledger.
        events.send(tool_start("c0")).expect("receiver alive");

        board
            .lock()
            .expect("board")
            .set_status("1", TaskStatus::InProgress)
            .expect("start task 1");
        events.send(tool_start("c1")).expect("receiver alive");

        {
            let mut guard = board.lock().expect("board");
            guard
                .set_status("1", TaskStatus::Completed)
                .expect("close task 1");
            guard
                .set_status("2", TaskStatus::InProgress)
                .expect("start task 2");
        }
        events.send(tool_start("c2")).expect("receiver alive");

        let tags: Vec<Option<String>> = drain(&mut rx)
            .iter()
            .map(|event| event.task_id().map(|id| id.as_str().to_string()))
            .collect();
        assert_eq!(
            tags,
            vec![None, Some("1".to_string()), Some("2".to_string())],
            "each send reads the board as it stood at that instant"
        );
    }

    /// Every clone shares one slot, which is what lets a host attach once and
    /// have the registry's stream and the engine's stream both tagged. Without
    /// it the two would have to be wired separately, and the one nobody
    /// remembered would be the silent gap.
    #[test]
    fn attaching_to_one_clone_tags_every_clone() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        let registry_side = events.clone();
        events.attach_running_task(RunningTask::from_fn(|| {
            Some(stella_protocol::TaskId::new("7"))
        }));

        registry_side
            .send(tool_start("c1"))
            .expect("receiver alive");
        // ...including a clone taken AFTER the attachment.
        events
            .clone()
            .send(tool_start("c2"))
            .expect("receiver alive");

        for event in drain(&mut rx) {
            assert_eq!(
                event.task_id().map(|id| id.as_str().to_string()),
                Some("7".to_string()),
                "a clone must not carry its own empty slot"
            );
        }
    }

    /// Narration is not work: an event with no slot passes through untouched
    /// and, critically, never pays for a board read.
    #[test]
    fn an_event_with_no_task_slot_is_untouched() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&reads);
        events.attach_running_task(RunningTask::from_fn(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(stella_protocol::TaskId::new("7"))
        }));

        events
            .send(AgentEvent::Text {
                text: "the answer".to_string(),
            })
            .expect("receiver alive");
        assert_eq!(
            reads.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the board must not be locked for an event that cannot carry a tag"
        );
        assert!(drain(&mut rx).iter().all(|e| e.task_id().is_none()));
    }

    /// A tag applied closer to the work outranks the ambient one: a delegated
    /// lane's event reaching the lead's sender keeps its own attribution.
    #[test]
    fn an_already_tagged_event_is_not_relabelled() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        events.attach_running_task(RunningTask::from_fn(|| {
            Some(stella_protocol::TaskId::new("7"))
        }));

        let mut delegated = tool_start("c1");
        delegated.stamp_task(&stella_protocol::TaskId::new("2"));
        events.send(delegated).expect("receiver alive");

        assert_eq!(
            drain(&mut rx)
                .first()
                .and_then(|e| e.task_id())
                .map(|id| id.as_str().to_string()),
            Some("2".to_string())
        );
    }

    /// A sender nobody attached a board to is unchanged — every existing
    /// caller, and every test, keeps the behaviour it had.
    #[test]
    fn a_sender_with_no_board_leaves_work_untagged() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        events.send(tool_start("c1")).expect("receiver alive");
        assert!(drain(&mut rx).iter().all(|e| e.task_id().is_none()));
    }
}
