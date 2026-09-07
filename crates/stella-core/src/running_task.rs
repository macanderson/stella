//! Which task the session works on, asked rather than stored.
//!
//! The engine stamps `task_id` on the work it emits. The board that knows
//! the answer sits in `stella-tools`, beside the six `task_*` tools that
//! move it. [`RunningTask`] is the port between the two. A host hands the
//! engine a closure, and [`crate::event_sender::EventSender`] calls it as it
//! sends.
//!
//! So the engine reaches the closure. It never reaches the board's rules,
//! and it has no need to. That is what lets AGENTS.md rule 12 be answered
//! here.

use std::sync::Arc;

use stella_protocol::TaskId;

/// A live read of the running board task, for a producer that must stamp an
/// event without owning the board.
///
/// It reads through a closure instead of caching the answer. A cached
/// `Option<TaskId>` would be a second place the running task is written
/// down. Six tools move the board. So do a plan seeding, a `/clear`, and
/// every sub-agent assignment. The copy would go stale on the path nobody
/// thought about. Reading through leaves one authority and nothing to
/// refresh.
///
/// The cost is one board lock per stamped event. Only an event that can
/// carry a tag pays it, and only when it is not stamped already (see
/// `EventSender::send`). That is a small part of a turn's stream and none of
/// its per-token traffic.
///
/// # What a lane's source answers for its delegates
///
/// An in-process delegate ([`crate::subagent`]) sends its events to the lane
/// that dispatched it. They carry that lane's running task. That is the
/// reading we want. Work the lead handed off for task 4 is task 4's evidence
/// and task 4's cost. A ledger that dropped it would under-report every task
/// that fanned out.
///
/// A `task_assign` worker is the other shape. It has its own session and its
/// own board, and that board is empty, so a read of it answers `None`. Its
/// host attaches a constant source instead. The constant names the one task
/// the lane was spawned to work (`stella-cli`'s `subsession::lane_events`).
/// Each form names the authority that knows. The worker's own board is not
/// it. Board ids are per-session ordinals, so its `"1"` is not the lead's
/// `"1"`.
#[derive(Clone)]
pub struct RunningTask(Arc<dyn Fn() -> Option<TaskId> + Send + Sync>);

impl RunningTask {
    /// Build a source from anything that can answer the question — in
    /// practice a closure over the host's shared board handle.
    #[must_use]
    pub fn from_fn(read: impl Fn() -> Option<TaskId> + Send + Sync + 'static) -> Self {
        Self(Arc::new(read))
    }

    /// Which task is running at this instant, if any.
    #[must_use]
    pub fn current(&self) -> Option<TaskId> {
        (self.0)()
    }
}

impl std::fmt::Debug for RunningTask {
    /// The closure has no useful form to print. So this reports what a reader
    /// of a log line wants: the answer it gives right now.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RunningTask").field(&self.current()).finish()
    }
}
