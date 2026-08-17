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
}

/// The run owner's half of the one-way ending contract (#3379).
///
/// The engine ends every turn with [`AgentEvent::TurnComplete`] and stops
/// there, because it cannot know whether its caller wants another turn. That
/// leaves the run-terminal [`AgentEvent::Complete`] — exactly once, last, only
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
