// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The frame channel's bound (#1266).
//!
//! # What was unbounded, and what already was not
//!
//! Turn reclamation bounds the *count* of live turns (`MAX_LIVE_TURNS`) and
//! [`crate::history`] bounds *retention* with a ring. Neither bounds the
//! delivery queue, and `server.rs` said so itself:
//!
//! > reclamation frees a settled turn's entry and the frames it buffered, but
//! > the frame channel itself is unbounded, so *one* abandoned turn can still
//! > buffer arbitrarily much before it settles.
//!
//! A client that creates a turn and never reads its SSE stream — a crashed
//! consumer, a dropped connection, a reader slower than the model — is exactly
//! that case: `AgentEvent::TextDelta` fires per token, so the queue grows for
//! as long as the turn runs.
//!
//! # Why dropping, and not backpressure
//!
//! Backpressure is the obvious fix and the wrong one here. The design this
//! server already committed to is that **a starved UI pump cannot stall a
//! turn** ([`crate::history`]'s module docs; it is why the two remote ports
//! bypass the event forwarder). Making the producer wait would hand any slow
//! or hostile reader a lever on the engine's progress, and would only move the
//! unboundedness one channel upstream into the `AgentEvent` queue feeding the
//! forwarder.
//!
//! So the bound is a *drop* policy, matching the bounded ring
//! [`crate::history`] already uses for the same pressure — and, like that ring,
//! the drop is recorded rather than silent.
//!
//! # What may be dropped, and what never may
//!
//! Only [`ServerFrame::Event`] is droppable. Everything else is load-bearing
//! and self-limiting:
//!
//! - `ToolRequest` / `ProviderRequest` are reverse RPC — the turn is *parked*
//!   waiting for each answer, so at most a handful are ever in flight, and
//!   dropping one would wedge the turn permanently.
//! - `TurnComplete` is the terminal frame a host reconciles against. Dropping
//!   it turns a finished turn into a hang.
//! - `TurnHeld` / `TurnReleased` are user-driven control acks, one per action.
//!
//! Their combined count is bounded by the protocol itself, so admitting them
//! unconditionally keeps the queue bounded at `CAP + (in-flight essentials)`
//! while making the failure mode impossible to hit for anything that matters.
//!
//! # The accounting
//!
//! Drops are counted, and [`FrameBacklog::dropped`] hands the count to the
//! turn's settle record (`TurnTally::frames_dropped`). A dropped delta means the *text* a live subscriber
//! sees has a hole in it; that is the honest cost of refusing to stall the
//! turn, and it is why the count is surfaced rather than swallowed.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::frame::ServerFrame;

/// Frames allowed to sit undelivered before deltas start being dropped.
///
/// Sized like [`crate::history::DEFAULT_RETAINED_FRAMES`] and for the same
/// reason: a reader that is briefly behind — a browser tab mid-repaint, a
/// host doing a synchronous write between reads — must never lose a token,
/// while a reader that is *gone* must not be able to grow the server. A
/// consumer more than this far behind on one turn is not slow, it is absent.
pub(crate) const DEFAULT_MAX_QUEUED_FRAMES: usize = 4096;

/// Shared depth-and-drop accounting for one turn's frame channel.
#[derive(Debug)]
pub(crate) struct FrameBacklog {
    /// Frames sent but not yet received. Incremented by the sink, decremented
    /// at the receive funnel.
    depth: AtomicUsize,
    /// Droppable frames refused because [`Self::cap`] was already reached.
    dropped: AtomicU64,
    cap: usize,
}

impl FrameBacklog {
    pub(crate) fn new(cap: usize) -> Arc<Self> {
        Arc::new(Self {
            depth: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
            cap,
        })
    }

    /// Whether a frame of this kind may be enqueued right now.
    ///
    /// Essential frames are always admitted — see the module docs for why the
    /// queue stays bounded anyway.
    fn admits(&self, frame: &ServerFrame) -> bool {
        if !droppable(frame) {
            return true;
        }
        self.depth.load(Ordering::Acquire) < self.cap
    }

    fn enter(&self) {
        self.depth.fetch_add(1, Ordering::AcqRel);
    }

    /// One frame left the queue. Saturating rather than wrapping: an
    /// underflow here would read as a colossal depth and wedge the channel
    /// into dropping everything, which is a far worse failure than a count
    /// that is briefly one low.
    pub(crate) fn leave(&self) {
        let _ = self
            .depth
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |d| {
                Some(d.saturating_sub(1))
            });
    }

    fn record_drop(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Frames dropped so far.
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn depth(&self) -> usize {
        self.depth.load(Ordering::Acquire)
    }
}

/// Whether this frame may be dropped under pressure.
///
/// Deliberately written as an exhaustive match rather than
/// `matches!(f, Event { .. })`: a future frame type must force a decision
/// here, and the compiler is the only reviewer guaranteed to notice. Getting
/// this wrong in the *safe* direction costs memory; getting it wrong in the
/// other direction wedges a turn.
fn droppable(frame: &ServerFrame) -> bool {
    match frame {
        ServerFrame::Event { .. } => true,
        ServerFrame::ToolRequest { .. }
        | ServerFrame::ProviderRequest { .. }
        | ServerFrame::ScopeReviewRequest { .. }
        | ServerFrame::TurnHeld { .. }
        | ServerFrame::TurnReleased
        | ServerFrame::TurnComplete { .. } => false,
    }
}

/// A frame sender that enforces [`FrameBacklog`].
///
/// Wraps the unbounded sender rather than replacing it with a bounded channel
/// on purpose: every send site in this crate is synchronous (`Controls::hold`,
/// the remote ports, the terminal-frame paths), and a bounded `tokio` channel
/// offers only `try_send` or an `await`. Keeping the sender synchronous means
/// the bound is enforced in one place instead of at eight call sites that
/// would each have to decide what "full" means.
#[derive(Clone, Debug)]
pub(crate) struct FrameSink {
    tx: tokio::sync::mpsc::UnboundedSender<ServerFrame>,
    backlog: Arc<FrameBacklog>,
}

impl FrameSink {
    pub(crate) fn new(
        tx: tokio::sync::mpsc::UnboundedSender<ServerFrame>,
        backlog: Arc<FrameBacklog>,
    ) -> Self {
        Self { tx, backlog }
    }

    /// Enqueue `frame`, dropping it if it is droppable and the queue is full.
    ///
    /// `Err` means the receiver is gone — the same signal
    /// `UnboundedSender::send` gives, so callers that break their loop on a
    /// closed channel keep working unchanged. A *dropped* frame is `Ok`: the
    /// channel is healthy, this frame simply did not fit.
    pub(crate) fn send(&self, frame: ServerFrame) -> Result<(), ()> {
        if !self.backlog.admits(&frame) {
            self.backlog.record_drop();
            return Ok(());
        }
        self.backlog.enter();
        self.tx.send(frame).map_err(|_| {
            // Never delivered, so it will never be received: undo the entry or
            // the depth ratchets up on a closed channel and starts dropping
            // frames that would otherwise have been admitted.
            self.backlog.leave();
        })
    }

    pub(crate) fn backlog(&self) -> &Arc<FrameBacklog> {
        &self.backlog
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::AgentEvent;

    fn sink(cap: usize) -> (FrameSink, tokio::sync::mpsc::UnboundedReceiver<ServerFrame>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (FrameSink::new(tx, FrameBacklog::new(cap)), rx)
    }

    fn delta() -> ServerFrame {
        ServerFrame::Event {
            event: AgentEvent::TextDelta { text: "tok".into() },
        }
    }

    /// The defect: a turn nobody reads used to buffer without limit.
    #[test]
    fn a_reader_that_never_reads_cannot_grow_the_queue_without_bound() {
        let (sink, _rx) = sink(4);
        for _ in 0..1_000 {
            sink.send(delta()).unwrap();
        }
        assert_eq!(sink.backlog().depth(), 4, "queue must stop at the cap");
        assert_eq!(
            sink.backlog().dropped(),
            996,
            "every refusal must be counted"
        );
    }

    /// Dropping one of these wedges the turn permanently — a parked reverse
    /// RPC never answered, or a finished turn that never says so.
    #[test]
    fn an_essential_frame_is_never_dropped_however_full_the_queue() {
        let (sink, mut rx) = sink(1);
        for _ in 0..50 {
            sink.send(delta()).unwrap();
        }
        sink.send(ServerFrame::TurnComplete {
            outcome: crate::frame::TurnOutcomeWire::Completed {
                text: String::new(),
                cost_usd: 0.0,
            },
        })
        .unwrap();

        let mut saw_terminal = false;
        while let Ok(frame) = rx.try_recv() {
            if matches!(frame, ServerFrame::TurnComplete { .. }) {
                saw_terminal = true;
            }
        }
        assert!(saw_terminal, "the terminal frame must survive a full queue");
    }

    /// Draining must restore capacity, or one burst permanently degrades a
    /// turn that later has an attentive reader.
    #[test]
    fn draining_the_queue_restores_capacity() {
        let (sink, mut rx) = sink(2);
        for _ in 0..10 {
            sink.send(delta()).unwrap();
        }
        assert_eq!(sink.backlog().depth(), 2);

        while rx.try_recv().is_ok() {
            sink.backlog().leave();
        }
        assert_eq!(sink.backlog().depth(), 0);

        sink.send(delta()).unwrap();
        assert_eq!(sink.backlog().depth(), 1, "capacity must come back");
    }

    /// A zero cap drops every droppable frame — the degenerate case that
    /// proves the admission check runs before, not after, the depth bump.
    #[test]
    fn a_zero_cap_drops_every_droppable_frame() {
        let (sink, _rx) = sink(0);
        sink.send(delta()).unwrap();
        sink.send(delta()).unwrap();
        assert_eq!(sink.backlog().dropped(), 2);
        assert_eq!(sink.backlog().depth(), 0);
    }

    /// A closed channel must not ratchet the depth up, or a reconnecting
    /// consumer finds a sink that drops everything.
    #[test]
    fn a_closed_channel_does_not_leak_depth() {
        let (sink, rx) = sink(4);
        drop(rx);
        assert!(sink.send(delta()).is_err());
        assert_eq!(sink.backlog().depth(), 0);
    }
}
