//! Cloneable event sender with an optional synchronous, ordered boundary.
//!
//! The core remains I/O-free: callers may supply a closure that durably
//! journals an event before it is admitted to the ordinary Tokio channel.
//! Because every clone shares that closure (and any mutex it captures), the
//! durable order and channel order can be made identical across concurrent
//! producers. A paid-call producer does not return from [`EventSender::send`]
//! until the caller's persistence boundary has completed.

use std::fmt;
use std::sync::Arc;

use stella_protocol::AgentEvent;
use tokio::sync::mpsc::UnboundedSender;

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
        }
    }

    pub fn send(&self, event: AgentEvent) -> Result<(), EventSendError> {
        (self.send)(event)
    }

    /// Wrap this sender so a run owner's closing `Stage(Complete)` rides
    /// immediately ahead of the engine's terminal `Complete`.
    ///
    /// The engine emits no stage boundary of its own: `StageKind` is the run
    /// *owner's* vocabulary, and a turn is one step of a run that may have six
    /// stages left to go (#3416). A raw run owner still owes its consumers the
    /// boundary — `stella-tui`'s `hud.stage` and `plain`'s stage rule both read
    /// it — and owes it in the order they already render, which is why this is
    /// a sender combinator rather than a send after the turn returns: the
    /// engine's `Complete` is emitted *inside* the turn, so anything appended
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
            if matches!(event, AgentEvent::Complete { .. }) {
                inner.send(AgentEvent::Stage {
                    name: stella_protocol::StageKind::Complete,
                })?;
            }
            inner.send(event)
        })
    }
}

impl From<UnboundedSender<AgentEvent>> for EventSender {
    fn from(sender: UnboundedSender<AgentEvent>) -> Self {
        Self::new(sender)
    }
}
