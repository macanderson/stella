//! Frame sequencing and bounded retention — what makes an SSE stream resumable.
//!
//! # The problem this closes
//!
//! Before this module, a turn's frames existed exactly once: they were read off
//! the session channel, written to a socket, and forgotten. A subscriber whose
//! connection dropped mid-turn — a laptop lid, a load balancer idle timeout, a
//! redeployed proxy — could not recover. The turn was cancelled by
//! `Drop for Session` and everything already emitted was gone. For a host
//! paying for the model call that produced those frames, a dropped TCP
//! connection meant paying twice.
//!
//! # Two halves, both required
//!
//! **A sequence number.** Every frame the host receives carries a monotonic
//! `seq`, so "resume after 42" is expressible at all. It is assigned in
//! [`FrameHistory::record`], which is called from the single point where
//! frames leave the session ([`crate::session::Session::next_frame`]) rather
//! than at each sender. That matters: frames enter the channel from three
//! independent producers — the `AgentEvent` forwarder task, `RemoteProvider`
//! and `RemoteToolExecutor` (the latter two bypass the forwarder on purpose,
//! so a starved UI pump cannot stall a turn). Numbering at the senders would
//! need a shared counter *and* would number frames in production order, which
//! is not the order the host receives them. Numbering at the receive funnel
//! numbers them in delivery order, which is exactly what a resuming client
//! means.
//!
//! **Retention.** A `seq` a client cannot ask for again is decoration, so the
//! frames stay in a bounded ring. Bounded, not unbounded: a long turn streams
//! one `TextDelta` per token, and a server that retained every frame of every
//! live turn would be a memory leak with a sequence number on it. When the
//! ring overflows, the drop is *recorded* — a resuming client that asks for a
//! `seq` older than the window is told its resume point is gone
//! ([`Replay::Truncated`]) rather than silently handed a stream with a hole in
//! it. Silent holes are worse than an honest error: the client cannot detect
//! them, and the events that go missing are the tool calls and completions the
//! host reconciles its own state against.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde::Serialize;

use crate::frame::ServerFrame;

/// How many frames are retained per turn for replay.
///
/// Sized for the realistic reconnect, not the pathological one: a browser tab
/// that reconnects after a few seconds of a token-streaming turn is well inside
/// this window, while a client gone for minutes of dense output is not, and is
/// told so. Raising it trades memory for a wider resume window — the cost is
/// per *live* turn, and the server already caps those.
pub(crate) const DEFAULT_RETAINED_FRAMES: usize = 4096;

/// A [`ServerFrame`] with its position in the turn's stream.
///
/// `#[serde(flatten)]` on purpose: the wire shape is the frame's own object
/// with one extra `seq` key, so this is a strictly **additive** change to a
/// contract clients already parse. An existing consumer still discriminates on
/// `type` and simply ignores a field it does not know; a new one can resume.
///
/// Generic over the frame so the encoder's failure arm stays testable: real
/// [`ServerFrame`]s are built from serde-clean types and cannot fail to
/// serialize, and a fallback that no test can reach is a fallback nobody has
/// checked. See `an_unencodable_frame_becomes_a_terminal_aborted_frame`.
#[derive(Debug, Serialize)]
pub(crate) struct SeqFrame<T> {
    /// This frame's position in the turn's stream, starting at 1. Monotonic
    /// and gapless in delivery order.
    pub(crate) seq: u64,
    /// The frame itself.
    #[serde(flatten)]
    pub(crate) frame: T,
}

/// What a resume request can be answered with.
pub(crate) enum Replay {
    /// The requested resume point is inside the retained window. Carries every
    /// frame after it as `(seq, json)`, in order — possibly empty when the
    /// client is current. The `seq` rides along because a replayed frame is
    /// re-emitted with its original SSE `id:`, so a client that disconnects
    /// again mid-replay resumes from where it actually got to.
    Frames(Vec<(u64, String)>),
    /// The requested resume point has already been evicted from the ring, so
    /// the frames between it and the oldest retained one are unrecoverable.
    /// Carries the oldest `seq` still available, which is what a client needs
    /// to decide whether to restart the turn or accept the gap.
    Truncated {
        /// The oldest sequence number still retained.
        oldest: u64,
    },
}

/// The retained tail of one turn's frame stream.
///
/// Holds pre-encoded JSON rather than `ServerFrame`s. Every retained frame has
/// already been serialized once to be written to the socket, and `ServerFrame`
/// is deliberately not `Clone` (each is produced once and moved onto the
/// channel). Keeping the string is therefore both cheaper and the only option
/// that does not force a `Clone` bound onto the wire type to serve a feature
/// the wire type has no opinion about.
pub(crate) struct FrameHistory {
    inner: Mutex<Ring>,
}

struct Ring {
    /// `(seq, encoded json)` in order, oldest first.
    frames: VecDeque<(u64, String)>,
    /// The seq of the most recently recorded frame; 0 before the first.
    last_seq: u64,
    /// How many frames have been evicted from the front. Non-zero means some
    /// resume points are no longer serviceable.
    evicted: u64,
    capacity: usize,
}

impl FrameHistory {
    /// A history retaining [`DEFAULT_RETAINED_FRAMES`] frames.
    pub(crate) fn new() -> Self {
        Self::with_capacity(DEFAULT_RETAINED_FRAMES)
    }

    /// A history retaining `capacity` frames. A zero capacity is clamped to 1:
    /// a ring that retains nothing would report every resume as truncated,
    /// which is a confusing way to spell "disabled".
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Ring {
                frames: VecDeque::new(),
                last_seq: 0,
                evicted: 0,
                capacity: capacity.max(1),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Ring> {
        // A poisoned history is still a usable history: the data behind it is
        // an append-only ring of already-encoded strings, so a panic in another
        // holder cannot have left it in a state that misleads a reader. The
        // alternative — propagating the poison — would turn one panicked
        // connection into a permanently unreadable turn.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Assign the next `seq`, encode the frame at that number, retain the
    /// result, and hand back both.
    ///
    /// `encode` receives the assigned `seq` because the number has to appear
    /// *inside* the JSON — the wire frame is `{"seq":N, ...}` — so assignment
    /// and encoding cannot be separate steps without a window in which a frame
    /// holds a number nothing has stamped. Running it under the lock is sound:
    /// it is pure serialization with no I/O and no path back into this type.
    ///
    /// What is retained is the exact JSON that goes on the socket, so a
    /// replayed frame is byte-identical to the one first delivered.
    /// Re-serializing on replay would risk a client seeing two spellings of
    /// one frame — and `ServerFrame` is deliberately not `Clone`, so retaining
    /// the frame itself is not on the table.
    pub(crate) fn record(&self, encode: impl FnOnce(u64) -> String) -> (u64, String) {
        let mut ring = self.lock();
        ring.last_seq += 1;
        let seq = ring.last_seq;
        let encoded = encode(seq);
        ring.frames.push_back((seq, encoded.clone()));
        while ring.frames.len() > ring.capacity {
            ring.frames.pop_front();
            ring.evicted += 1;
        }
        (seq, encoded)
    }

    /// Every retained frame after `after`, or a truncation report when that
    /// resume point has already been evicted.
    ///
    /// `after == 0` means "from the beginning" and is only serviceable while
    /// nothing has been evicted — which is the honest answer, since a turn
    /// that has already overflowed its ring genuinely cannot be replayed from
    /// its start.
    pub(crate) fn replay_after(&self, after: u64) -> Replay {
        let ring = self.lock();
        let oldest = ring.frames.front().map(|(seq, _)| *seq);
        // A client that is already current asks for the newest seq it has and
        // gets nothing back — correct, and not a truncation even if the ring
        // has evicted plenty.
        if after >= ring.last_seq {
            return Replay::Frames(Vec::new());
        }
        match oldest {
            // Retained frames start after the client's resume point, so
            // everything between the two is gone.
            Some(oldest) if oldest > after + 1 => Replay::Truncated { oldest },
            // Nothing retained at all, yet `after < last_seq` — only reachable
            // with a capacity so small the ring emptied behind us.
            None => Replay::Truncated {
                oldest: ring.last_seq,
            },
            Some(_) => Replay::Frames(
                ring.frames
                    .iter()
                    .filter(|(seq, _)| *seq > after)
                    .cloned()
                    .collect(),
            ),
        }
    }
}

impl Default for FrameHistory {
    fn default() -> Self {
        Self::new()
    }
}

/// One frame, sequenced and ready for the wire.
pub(crate) struct Encoded {
    /// The frame's position in the stream.
    pub(crate) seq: u64,
    /// The JSON payload of its SSE `data:` line.
    pub(crate) json: String,
    /// `true` when this is the turn's terminal frame, so the stream loop can
    /// stop without re-inspecting a frame it has already given up ownership of.
    pub(crate) terminal: bool,
    /// Set when the frame would not serialize and the abort fallback was
    /// substituted. Carried out rather than logged here so the caller — which
    /// holds the observer — reports it.
    pub(crate) unencodable: Option<String>,
}

/// Render a frame as the JSON payload of an SSE `data:` line, stamped with its
/// `seq`, reporting whether the fallback had to be used.
///
/// Every [`ServerFrame`] is built from serde-clean types, so the failure arm is
/// unreachable in practice — but "unreachable" is not "harmless". Emitting
/// `{}` would be a frame with no `type` at all: a host would skip it silently,
/// and if the frame it replaced was the terminal `turn_complete`, the stream
/// would end with no terminator and the host would wait forever. So the
/// fallback is itself a terminal abort frame — the stream always ends with
/// something a client can act on.
pub(crate) fn encode_seq_frame<T: Serialize>(seq: u64, frame: T) -> (String, Option<String>) {
    let seq_frame = SeqFrame { seq, frame };
    match serde_json::to_string(&seq_frame) {
        Ok(json) => (json, None),
        Err(err) => {
            let reason = format!("the engine produced a frame the server could not encode: {err}");
            let json = serde_json::to_string(&SeqFrame {
                seq,
                frame: ServerFrame::TurnComplete {
                    outcome: crate::frame::TurnOutcomeWire::Aborted {
                        reason,
                        cost_usd: 0.0,
                    },
                },
            })
            // The fallback's fallback: a literal that is valid on the wire, so
            // the stream still terminates with a `turn_complete` no matter what.
            .unwrap_or_else(|_| {
                format!(
                    r#"{{"seq":{seq},"type":"turn_complete","outcome":{{"status":"aborted","reason":"frame encoding failed","cost_usd":0.0}}}}"#
                )
            });
            (json, Some(err.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::TurnOutcomeWire;
    use serde::Serialize;

    /// A value whose `Serialize` always fails, standing in for the frame
    /// encoding failure that real [`ServerFrame`]s cannot produce. Without it
    /// the fallback would be untestable — and an untested fallback on the one
    /// path that carries a turn's outcome is how `{}` sat on the wire.
    struct Unserializable;

    impl Serialize for Unserializable {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("nope"))
        }
    }

    /// A frame that cannot be encoded must still leave the stream terminated:
    /// the old `{}` fallback was a frame with no `type`, so a host skipped it
    /// silently and — when the lost frame was the terminal one — never learned
    /// the turn's outcome or its settled cost.
    #[test]
    fn an_unencodable_frame_becomes_a_terminal_aborted_frame_not_an_empty_object() {
        let (json, unencodable) = encode_seq_frame(7, &Unserializable);
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON on the wire");
        assert_eq!(value["type"], "turn_complete", "{json}");
        assert_eq!(value["outcome"]["status"], "aborted", "{json}");
        assert!(
            value["outcome"]["reason"]
                .as_str()
                .is_some_and(|r| r.contains("nope")),
            "the cause must survive, not be swallowed: {json}"
        );
        assert!(
            unencodable.is_some_and(|err| err.contains("nope")),
            "the failure must be reportable, not merely papered over on the wire"
        );
    }

    #[test]
    fn an_ordinary_frame_encodes_unchanged() {
        let (json, unencodable) = encode_seq_frame(
            3,
            &ServerFrame::TurnComplete {
                outcome: TurnOutcomeWire::Completed {
                    text: "done".to_string(),
                    cost_usd: 0.5,
                },
            },
        );
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["outcome"]["text"], "done");
        assert_eq!(value["outcome"]["cost_usd"], 0.5);
        assert!(unencodable.is_none(), "a clean encode reports no failure");
    }

    fn history_of(capacity: usize, frames: u64) -> FrameHistory {
        let history = FrameHistory::with_capacity(capacity);
        for n in 1..=frames {
            history.record(|_| format!("{{\"n\":{n}}}"));
        }
        history
    }

    #[test]
    fn seq_starts_at_one_and_is_gapless() {
        let history = FrameHistory::new();
        assert_eq!(history.record(|seq| seq.to_string()).0, 1);
        assert_eq!(history.record(|seq| seq.to_string()).0, 2);
        assert_eq!(history.record(|seq| seq.to_string()).0, 3);
        assert_eq!(history.lock().last_seq, 3);
    }

    #[test]
    fn the_encoder_sees_the_seq_it_is_stamped_with() {
        // The whole reason `record` owns encoding: a frame's JSON must carry
        // the same number the ring filed it under, with no window between.
        let history = FrameHistory::new();
        let (seq, encoded) = history.record(|seq| format!("{{\"seq\":{seq}}}"));
        assert_eq!(seq, 1);
        assert_eq!(encoded, "{\"seq\":1}");
        let Replay::Frames(frames) = history.replay_after(0) else {
            panic!("nothing evicted");
        };
        assert_eq!(frames, vec![(1, "{\"seq\":1}".to_string())]);
    }

    #[test]
    fn replay_returns_only_frames_after_the_resume_point() {
        let history = history_of(16, 5);
        let Replay::Frames(frames) = history.replay_after(3) else {
            panic!("a resume point inside the window is serviceable");
        };
        assert_eq!(
            frames,
            vec![(4, "{\"n\":4}".to_string()), (5, "{\"n\":5}".to_string())]
        );
    }

    #[test]
    fn a_current_client_replays_nothing() {
        let history = history_of(16, 5);
        let Replay::Frames(frames) = history.replay_after(5) else {
            panic!("being current is not a truncation");
        };
        assert!(frames.is_empty());
    }

    #[test]
    fn replay_from_zero_returns_the_whole_retained_stream() {
        let history = history_of(16, 3);
        let Replay::Frames(frames) = history.replay_after(0) else {
            panic!("nothing evicted, so the start is still serviceable");
        };
        assert_eq!(frames.len(), 3);
    }

    #[test]
    fn an_evicted_resume_point_is_reported_not_silently_skipped() {
        // Capacity 3, 10 frames recorded: seqs 1-7 are gone, 8-10 retained.
        // A client resuming after 2 must NOT be handed 8,9,10 as if contiguous
        // — the gap is exactly the tool calls it would fail to reconcile.
        let history = history_of(3, 10);
        match history.replay_after(2) {
            Replay::Truncated { oldest } => assert_eq!(oldest, 8),
            Replay::Frames(_) => panic!("an evicted resume point must be reported as truncated"),
        }
    }

    #[test]
    fn the_boundary_resume_point_is_still_serviceable() {
        // Retained 8..=10; resuming "after 7" is exactly contiguous with the
        // oldest retained frame, so it is serviceable, not truncated.
        let history = history_of(3, 10);
        let Replay::Frames(frames) = history.replay_after(7) else {
            panic!("a resume point contiguous with the window is serviceable");
        };
        assert_eq!(frames.len(), 3);
    }

    #[test]
    fn capacity_bounds_retention() {
        let history = history_of(4, 100);
        assert_eq!(history.lock().frames.len(), 4);
        assert_eq!(history.lock().last_seq, 100);
    }
}

#[cfg(test)]
mod envelope_pin {
    //! The one line of `docs/wire/serveframe.d.ts` that is hand-written.
    //!
    //! Everything else in that artifact is printed from the types, so it cannot
    //! drift. `StellaWireFrame = ServerFrame & { seq: number }` cannot be:
    //! `seq` is added by the transport at delivery time, so no derive on
    //! `ServerFrame` describes it. These tests are what stop that one line from
    //! becoming the drift the whole exercise exists to prevent.

    use super::*;
    use crate::frame::TurnOutcomeWire;

    fn keys(json: &str) -> Vec<String> {
        let value: serde_json::Value = serde_json::from_str(json).expect("valid JSON on the wire");
        let mut keys: Vec<String> = value
            .as_object()
            .expect("a frame is a JSON object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    /// The wire object is exactly the frame's own keys plus `seq` — no
    /// envelope, no nesting. A client typed as `ServerFrame & { seq: number }`
    /// is therefore describing what actually arrives.
    #[test]
    fn envelope_shape_is_frame_plus_seq() {
        let frame = ServerFrame::TurnComplete {
            outcome: TurnOutcomeWire::Completed {
                text: "done".to_string(),
                cost_usd: 0.5,
            },
        };
        let bare = serde_json::to_string(&frame).expect("frames serialize");
        let bare_keys = keys(&bare);

        let (json, unencodable) = encode_seq_frame(
            42,
            ServerFrame::TurnComplete {
                outcome: TurnOutcomeWire::Completed {
                    text: "done".to_string(),
                    cost_usd: 0.5,
                },
            },
        );
        assert!(unencodable.is_none());
        let mut expected = bare_keys.clone();
        expected.push("seq".to_string());
        expected.sort();
        assert_eq!(
            keys(&json),
            expected,
            "the wire frame must be the frame's own keys plus `seq`, or the \
             hand-written `StellaWireFrame` intersection in \
             docs/wire/serveframe.d.ts is wrong: {json}"
        );

        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["seq"], 42);
        assert_eq!(
            value["type"], "turn_complete",
            "`seq` must not displace the discriminant a client switches on"
        );
    }

    /// `seq` is a number in JSON, not a string — `Last-Event-ID` arrives as
    /// text and a client that round-trips it must parse, so the two spellings
    /// have to be pinned apart.
    #[test]
    fn seq_is_a_json_number() {
        let (json, _) = encode_seq_frame(
            7,
            ServerFrame::Event {
                event: stella_protocol::AgentEvent::TextDelta {
                    text: "hi".to_string(),
                },
            },
        );
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value["seq"].is_u64(), "seq must be a number: {json}");
    }
}
