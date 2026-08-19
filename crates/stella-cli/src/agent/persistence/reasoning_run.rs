//! Coalescing a run of streamed reasoning fragments into one durable row.
//!
//! `AgentEvent::Reasoning { delta }` **is** the fragment: the provider's
//! stream emits one per few tokens and nothing later restates the whole
//! thought. That is what distinguishes it from `AgentEvent::TextDelta`, which
//! both event drains drop outright because the step's `Text` event carries the
//! same characters in full. Reasoning has no such authoritative sibling, so it
//! cannot be dropped — it has to be *joined*, and until #3969 nothing joined
//! it on the way in: one ordinary turn in this workspace wrote 39,544 `events`
//! rows holding, between them, about seven paragraphs of prose, each row its
//! own `record_event` transaction on the streaming path.
//!
//! The shape is the session sidecar's, deliberately: `stella_store::journal`'s
//! `PendingDeltas` already coalesces a delta run for the JSONL journal, down
//! to the 8 KiB flush threshold. This is that discipline pointed at the SQLite
//! path, which had no equivalent.
//!
//! Only the **durable** write coalesces. The arriving event is still handed
//! back to the caller unchanged, so `stream-json` keeps emitting fragments
//! live and the deck keeps painting them as they land.

use stella_protocol::AgentEvent;

/// Bytes of buffered reasoning that force an open run out to the store before
/// the stream itself ends it.
///
/// The same threshold the session sidecar uses
/// (`stella_store::journal::DELTA_FLUSH_BYTES`), for the same two reasons: it
/// bounds how much prose one row holds, and it bounds how much an unclean
/// process death can lose. That second bound is why the number is copied
/// rather than lowered — a telemetry row is the record of what happened and
/// cannot be re-derived by re-running the turn the way the sidecar's can.
///
/// The window it opens is strictly *smaller* than the one already there: the
/// channel feeding both drains is unbounded by construction (invariant #2
/// keeps `stella-core` I/O-free, so its sender cannot await), and everything
/// queued in it at a `SIGKILL` was already lost. Every ordinary ending — a
/// finished turn, an abort, a `Ctrl-C`, a budget stop — closes the channel,
/// and closing it runs [`ReasoningRun::close`].
const RUN_FLUSH_BYTES: usize = 8 * 1024;

/// The durable writes one arriving event owes, in the order they must land.
///
/// Order is the whole point of returning both halves together: a run that is
/// still open when a `ToolStart` arrives has to reach the `events` table
/// *before* that `ToolStart`, or the transcript reads the thought after the
/// action it preceded.
#[derive(Debug, Default)]
pub(crate) struct Owed {
    /// A completed reasoning block to write first, at the current `seq`.
    pub(crate) block: Option<AgentEvent>,
    /// Whether the arriving event is itself written. False exactly when it
    /// was a reasoning fragment folded into the open run — the case that
    /// spends no `seq` and skips the blocking hop entirely.
    pub(crate) writes_arriving: bool,
}

impl Owed {
    /// Nothing to persist for this event: it was folded into an open run that
    /// is not full yet.
    pub(crate) fn is_empty(&self) -> bool {
        self.block.is_none() && !self.writes_arriving
    }
}

/// The open, coalescing run of reasoning fragments for one event drain.
///
/// One per drain, and a drain is one lane's stream — which is as much identity
/// as the fragments carry: `AgentEvent::Reasoning` names no agent, so two
/// agents' thoughts interleaved on one channel were already indistinguishable
/// in the `events` table, and the read-side rejoin
/// (`stella_observatory::journal::coalesce_reasoning`) already merged them.
/// This is faithful to that, not looser than it.
#[derive(Debug, Default)]
pub(crate) struct ReasoningRun {
    buf: String,
}

impl ReasoningRun {
    /// Fold one arriving event into the run and report what it owes the store.
    ///
    /// Every event that is not a reasoning fragment closes the run — including
    /// the ones a caller drops before persisting, which simply never reach
    /// here. That is what keeps a thought interrupted by a tool call two
    /// blocks rather than one: the boundary between thoughts is a fact about
    /// the turn, not a rendering artifact.
    pub(crate) fn admit(&mut self, event: &AgentEvent) -> Owed {
        let AgentEvent::Reasoning { delta } = event else {
            return Owed {
                block: self.take(),
                writes_arriving: true,
            };
        };
        self.buf.push_str(delta);
        Owed {
            block: (self.buf.len() >= RUN_FLUSH_BYTES)
                .then(|| self.take())
                .flatten(),
            writes_arriving: false,
        }
    }

    /// Drain whatever the run still holds — the close-out every drain owes its
    /// stream, so an interrupted turn keeps the reasoning it had streamed.
    pub(crate) fn close(&mut self) -> Option<AgentEvent> {
        self.take()
    }

    /// The buffered run as the event it will be persisted as, or `None` when
    /// nothing is open. An empty run yields no row: a zero-length fragment
    /// records nothing that a reader could tell from its absence.
    fn take(&mut self) -> Option<AgentEvent> {
        (!self.buf.is_empty()).then(|| AgentEvent::Reasoning {
            delta: std::mem::take(&mut self.buf),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reasoning(delta: &str) -> AgentEvent {
        AgentEvent::Reasoning {
            delta: delta.into(),
        }
    }

    fn delta_of(event: &AgentEvent) -> &str {
        match event {
            AgentEvent::Reasoning { delta } => delta,
            other => panic!("expected a reasoning block, got {other:?}"),
        }
    }

    #[test]
    fn a_run_of_fragments_costs_one_write_and_replays_byte_identically() {
        let fragments = ["Let", " me look", " at the issue. I"];
        let mut run = ReasoningRun::default();
        for fragment in fragments {
            assert!(
                run.admit(&reasoning(fragment)).is_empty(),
                "an unfull run owes the store nothing"
            );
        }
        let block = run.close().expect("the run's tail");
        assert_eq!(delta_of(&block), fragments.concat());
    }

    #[test]
    fn a_non_reasoning_event_flushes_the_run_ahead_of_itself() {
        let mut run = ReasoningRun::default();
        run.admit(&reasoning("thinking"));
        let owed = run.admit(&AgentEvent::Text {
            text: "answer".into(),
        });
        assert_eq!(
            delta_of(owed.block.as_ref().expect("flushed block")),
            "thinking"
        );
        assert!(owed.writes_arriving, "the arriving event still lands");
        assert!(run.close().is_none(), "the run closed with the flush");
    }

    #[test]
    fn an_interrupting_event_leaves_two_blocks_not_one() {
        // The boundary between thoughts is a fact about the turn: a tool call
        // between two runs must not fuse them into a single block.
        let mut run = ReasoningRun::default();
        run.admit(&reasoning("before"));
        let first = run
            .admit(&AgentEvent::Text {
                text: "tool".into(),
            })
            .block
            .expect("first block");
        run.admit(&reasoning("after"));
        let second = run.close().expect("second block");
        assert_eq!(delta_of(&first), "before");
        assert_eq!(delta_of(&second), "after");
    }

    #[test]
    fn a_run_past_the_threshold_flushes_without_waiting_for_the_stream() {
        let mut run = ReasoningRun::default();
        let fragment = "x".repeat(1024);
        let mut flushed = None;
        for _ in 0..8 {
            flushed = run.admit(&reasoning(&fragment)).block.or(flushed);
        }
        let block = flushed.expect("the threshold forces a flush mid-stream");
        assert_eq!(delta_of(&block).len(), RUN_FLUSH_BYTES);
        assert!(run.close().is_none(), "the flush emptied the run");
    }

    #[test]
    fn a_stream_with_no_reasoning_owes_exactly_what_it_always_did() {
        let mut run = ReasoningRun::default();
        let owed = run.admit(&AgentEvent::Text {
            text: "answer".into(),
        });
        assert!(owed.block.is_none());
        assert!(owed.writes_arriving);
        assert!(run.close().is_none());
    }
}
