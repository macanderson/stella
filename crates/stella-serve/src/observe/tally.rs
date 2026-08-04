// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Folding the engine's own event stream into one per-turn tally.
//!
//! `ServerFrame::Event { event: AgentEvent }` already flows through this crate
//! on every turn — stages, retries, tool timings, loop detections, discarded
//! speculation. #930 suggests a span in `stella-core`'s step loop "would pay for
//! itself"; those signals already exist, typed and wire-stable, and this server
//! simply was not listening. So the answer needs no engine change at all.
//!
//! Two details make the tap correct rather than merely appealing.
//!
//! **It taps where frames are produced, not where they are streamed.** The
//! obvious home is the SSE loop in `server.rs`, and it is wrong: a turn nobody
//! streams buffers its frames and the tap never runs — and unstreamed turns are
//! precisely the abandoned ones worth observing. So the fold lives on the
//! session thread's forwarder, which every turn runs.
//!
//! **It counts; it does not log.** `AgentEvent::TextDelta` fires per token, so a
//! record per frame would make this server slower than the model it waits on.
//! The whole fold rides one [`ServeEvent::TurnSettled`](super::event::ServeEvent).
//!
//! # Why there is no per-frame debug record
//!
//! `docs/design/serve-observability.md` §7 planned individual `AgentEvent`
//! records behind `STELLA_SERVE_LOG=debug`. Building it showed that to be a
//! mistake: `AgentEvent::Text`, `TextDelta` and `Reasoning` carry model output
//! verbatim, so such a record would put prompt-adjacent content in a log file —
//! and would make the crate's no-content property conditional on a verbosity
//! flag, which is exactly the kind of "safe unless you turn the knob" invariant
//! that fails in production. The tally supersedes it: aggregates answer the
//! diagnostic questions (is this turn progressing? how many retries?) without
//! any payload reaching a record.

use stella_protocol::AgentEvent;

use super::event::TurnTally;

/// Accumulates a [`TurnTally`] from the engine's event stream.
#[derive(Debug, Default)]
pub(crate) struct TallyFold {
    tally: TurnTally,
}

impl TallyFold {
    /// Fold one engine event. Cheap by construction — integer increments only,
    /// no allocation, no I/O — because this runs on the turn's hot path.
    pub(crate) fn observe(&mut self, event: &AgentEvent) {
        let tally = &mut self.tally;
        match event {
            // The progress axis. A turn whose stages stop advancing while a
            // reverse request's wait climbs is wedged; one that keeps advancing
            // is merely slow. That distinction is the whole point of counting.
            AgentEvent::Stage { .. } => tally.stages = tally.stages.saturating_add(1),
            // "One committed model call — the metering record", emitted exactly
            // once per step that lands.
            AgentEvent::StepUsage { .. } => tally.model_calls = tally.model_calls.saturating_add(1),
            AgentEvent::Retry { .. } => tally.retries = tally.retries.saturating_add(1),
            // A doomed sequence reports its whole run at once instead of a
            // `Retry` per attempt, so count the retries it stands for: the
            // initial call is attempt 1 and is not itself a retry.
            AgentEvent::RetriesExhausted { attempts, .. } => {
                tally.retries = tally.retries.saturating_add(attempts.saturating_sub(1));
            }
            AgentEvent::ToolStart { .. } => tally.tool_calls = tally.tool_calls.saturating_add(1),
            AgentEvent::ToolResult { output, .. } if output.is_error() => {
                tally.tools_failed = tally.tools_failed.saturating_add(1);
            }
            // `stella-core` already names its discarded speculative work, so
            // this server counts it rather than re-deriving it.
            AgentEvent::SpeculationDiscarded { .. } => {
                tally.speculation_discarded = tally.speculation_discarded.saturating_add(1);
            }
            AgentEvent::LoopDetected { .. } => {
                tally.loop_detections = tally.loop_detections.saturating_add(1);
            }
            // Everything else — text, deltas, reasoning, budget ticks — is
            // either payload we deliberately never record or a signal with no
            // counterpart here. `AgentEvent` is forward-compatible, so an
            // unknown variant must be inert rather than a compile error.
            _ => {}
        }
    }

    /// The accumulated tally.
    pub(crate) fn finish(self) -> TurnTally {
        self.tally
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::{StageKind, ToolCall, ToolOutput};

    fn tool_call(id: &str) -> ToolCall {
        ToolCall {
            call_id: id.to_string(),
            name: "read".to_string(),
            input: serde_json::json!({}),
        }
    }

    /// The signals an operator needs are counted, and each from the event that
    /// actually means it.
    #[test]
    fn the_fold_counts_the_signals_that_distinguish_wedged_from_slow() {
        let mut fold = TallyFold::default();
        fold.observe(&AgentEvent::Stage {
            name: StageKind::Execute,
        });
        fold.observe(&AgentEvent::Stage {
            name: StageKind::Verify,
        });
        fold.observe(&AgentEvent::Retry {
            attempt: 1,
            reason: "429".to_string(),
        });
        fold.observe(&AgentEvent::ToolStart {
            call: tool_call("a"),
        });
        fold.observe(&AgentEvent::ToolResult {
            call_id: "a".to_string(),
            output: ToolOutput::Error {
                message: "nope".to_string(),
            },
            duration_ms: 3,
            speculated: false,
        });
        fold.observe(&AgentEvent::ToolStart {
            call: tool_call("b"),
        });
        fold.observe(&AgentEvent::ToolResult {
            call_id: "b".to_string(),
            output: ToolOutput::Ok {
                content: "fine".to_string(),
            },
            duration_ms: 4,
            speculated: false,
        });
        fold.observe(&AgentEvent::SpeculationDiscarded {
            call_id: "c".to_string(),
            name: "read".to_string(),
            reason: "harvest_mismatch".to_string(),
        });

        let tally = fold.finish();
        assert_eq!(tally.stages, 2);
        assert_eq!(tally.tool_calls, 2);
        assert_eq!(tally.tools_failed, 1, "only the error arm counts as failed");
        assert_eq!(tally.retries, 1);
        assert_eq!(tally.speculation_discarded, 1);
    }

    /// A doomed retry sequence reports once, for the whole run — so counting
    /// it as a single retry would undercount a turn that burned five attempts.
    #[test]
    fn an_exhausted_retry_sequence_counts_every_retry_it_stands_for() {
        let mut fold = TallyFold::default();
        fold.observe(&AgentEvent::RetriesExhausted {
            turn_instance: 0,
            attempts: 4,
            reasons: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            retryable: true,
        });
        // Four dispatched attempts is one initial call plus three retries.
        assert_eq!(fold.finish().retries, 3);
    }

    /// Payload-bearing events must be inert: they are the reason there is no
    /// per-frame debug record, and they must not sneak into a count either.
    #[test]
    fn payload_events_are_inert() {
        let mut fold = TallyFold::default();
        fold.observe(&AgentEvent::Text {
            delta: "secret".to_string(),
        });
        fold.observe(&AgentEvent::TextDelta {
            text: "secret".to_string(),
        });
        fold.observe(&AgentEvent::Reasoning {
            delta: "secret".to_string(),
        });
        fold.observe(&AgentEvent::Steered {
            text: "secret".to_string(),
        });
        assert_eq!(fold.finish(), TurnTally::default());
    }

    /// The fold must survive a turn long enough to overflow a `u32` counter
    /// without panicking in a release-mode-divergent way.
    #[test]
    fn counters_saturate_rather_than_wrap() {
        let mut fold = TallyFold::default();
        fold.tally.stages = u32::MAX;
        fold.observe(&AgentEvent::Stage {
            name: StageKind::Execute,
        });
        assert_eq!(fold.finish().stages, u32::MAX);
    }
}
