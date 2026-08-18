// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The journal *line*: one [`AgentEvent`] plus the wall-clock stamp the sink
//! that wrote it adds.
//!
//! Until #2111 nothing on the stream anchored an event to wall clock.
//! `step_usage` and `tool_result` carry a `duration_ms`, which measures a span
//! but names no instant, so two questions could not be answered from a recorded
//! `stella-events.jsonl` at all: *when* did this happen, and *where did the wall
//! clock go* between two events. The second is the one that mattered — an idle
//! gap before a timeout is invisible when there is nothing to subtract — and the
//! visible symptom was an arena transcript rendering `0:00` for every row of a
//! finished trial.
//!
//! ## Why the stamp is not a field of [`AgentEvent`]
//!
//! The stamp is a fact about a *write*, not about the thing that happened.
//! The same event is written to more than one sink (the durable JSONL file and
//! stdout), the engine that produced it is a pure, replayable, clock-free fold,
//! and `stella-protocol` carries no time source by charter. Putting `ts` inside
//! the enum would have meant a field on every variant, a clock reachable from
//! `stella-core`, and a stamp baked into every replay fixture.
//!
//! So the event vocabulary is untouched and the stamp rides in an envelope
//! applied at the sink. The envelope **flattens**, so the bytes on the wire are
//! the event's own object with one extra key:
//!
//! ```text
//! {"ts":1754582400123,"type":"tool_result","call_id":"c1",…}
//! ```
//!
//! Every existing consumer keeps reading `event["type"]` exactly as before —
//! this is an added key, not a new nesting level.
//!
//! ## Direction of compatibility
//!
//! Writers always stamp ([`stamped_line`] takes a `u64`, not an option).
//! Readers must not require it: every journal recorded before #2111, and every
//! line produced by a sink that has no clock to offer, has no `ts` at all. On
//! [`StampedEvent`] the field is therefore `Option<u64>` with `serde(default)`,
//! and a `None` serializes to *exactly* the bytes a bare `AgentEvent` produces —
//! which is what makes the change additive rather than a new format.

use serde::{Deserialize, Serialize};

use crate::event::AgentEvent;

/// The wire meaning of the `ts` key, in the words the generated schema and the
/// `.d.ts` publish to downstream implementers.
///
/// One copy on purpose: `schema_export` prints this string into
/// `docs/wire/agentevent.schema.json` and `docs/wire/agentevent.d.ts`, so the
/// contract a consumer reads and the contract this module documents cannot
/// drift apart.
///
/// Deliberately terse — it is repeated on every variant of a generated
/// artifact, and the reasoning behind each clause belongs in this module's docs
/// and in the `.d.ts` banner, which a reader meets exactly once.
pub const TS_DESCRIPTION: &str = "\
Wall-clock instant at which the sink wrote this line, in milliseconds since the \
Unix epoch (UTC). Stamped by the sink rather than carried by the event, so it \
is optional forever — a line recorded before the field existed has none — and \
it is not monotonic, so a consumer computing an elapsed offset must clamp a \
negative delta rather than trust it.";

/// One line of a stream-json journal: an [`AgentEvent`] and the `ts` its sink
/// stamped on it.
///
/// This is the **read** side of the contract — the shape a consumer parses a
/// `stella-events.jsonl` line into. Writers use [`stamped_line`], which cannot
/// express an unstamped line.
///
/// `ts` is [`Option`] because absence is a real and permanent case (see the
/// module docs), and it is skipped when `None` so that a `StampedEvent` with no
/// stamp serializes byte-for-byte identically to the bare [`AgentEvent`] it
/// wraps. That identity is the reason this type can be introduced without a
/// format version: it is the same wire, with one optional key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StampedEvent {
    /// Milliseconds since the Unix epoch (UTC) — see [`TS_DESCRIPTION`] for the
    /// full contract, which is the text downstream implementers read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<u64>,
    /// The event itself, flattened: its keys sit beside `ts` on one object
    /// rather than under a nested member.
    #[serde(flatten)]
    pub event: AgentEvent,
}

/// The write side. Borrowed so that stamping a line never clones an event —
/// a `Text` event carries a whole model answer, and the sinks stamp every one.
#[derive(Serialize)]
struct StampedEventRef<'a> {
    ts: u64,
    #[serde(flatten)]
    event: &'a AgentEvent,
}

/// Serialize one journal line: `event`, stamped with `ts_ms`.
///
/// `ts_ms` is milliseconds since the Unix epoch, read from the caller's clock —
/// this crate has no time source of its own (invariant #2), so the sink that
/// owns a clock supplies the instant and this function only renders it.
///
/// # Errors
///
/// Propagates the `serde_json` failure. Serializing a protocol enum does not
/// fail in practice; the sinks treat it as terminal rather than dropping a line
/// from evidence someone is paying to collect.
pub fn stamped_line(event: &AgentEvent, ts_ms: u64) -> Result<String, serde_json::Error> {
    serde_json::to_string(&StampedEventRef { ts: ts_ms, event })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_event() -> AgentEvent {
        AgentEvent::Text {
            text: "the full answer".to_string(),
        }
    }

    #[test]
    fn a_stamped_line_is_the_event_object_plus_one_key() {
        let line = stamped_line(&text_event(), 1_754_582_400_123).unwrap();
        assert_eq!(
            line,
            r#"{"ts":1754582400123,"type":"text","text":"the full answer"}"#
        );
    }

    #[test]
    fn an_unstamped_value_serializes_exactly_as_the_bare_event() {
        let stamped = StampedEvent {
            ts: None,
            event: text_event(),
        };
        assert_eq!(
            serde_json::to_string(&stamped).unwrap(),
            serde_json::to_string(&text_event()).unwrap()
        );
    }

    #[test]
    fn a_line_recorded_before_the_field_existed_still_parses() {
        let legacy = serde_json::to_string(&text_event()).unwrap();
        let parsed: StampedEvent = serde_json::from_str(&legacy).unwrap();
        assert!(parsed.ts.is_none());
        assert!(matches!(parsed.event, AgentEvent::Text { text } if text == "the full answer"));
    }

    #[test]
    fn a_stamped_line_round_trips_byte_for_byte() {
        let line = stamped_line(&text_event(), 1_754_582_400_123).unwrap();
        let parsed: StampedEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed.ts, Some(1_754_582_400_123));
        assert_eq!(serde_json::to_string(&parsed).unwrap(), line);
    }

    #[test]
    fn an_event_from_a_newer_stella_keeps_its_body_and_its_stamp() {
        // `AgentEvent::Unknown` re-emits the object it parsed. The envelope must
        // not lose the stamp on the way past it, or forward-compat and forensics
        // would be mutually exclusive on exactly the lines that need both.
        let line = r#"{"ts":7,"type":"from_the_future","detail":{"a":1}}"#;
        let parsed: StampedEvent = serde_json::from_str(line).unwrap();
        assert_eq!(parsed.ts, Some(7));
        assert!(parsed.event.is_unknown());
        assert_eq!(parsed.event.type_tag(), "from_the_future");

        let round_tripped: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(
            round_tripped,
            serde_json::from_str::<serde_json::Value>(line).unwrap()
        );
    }
}
