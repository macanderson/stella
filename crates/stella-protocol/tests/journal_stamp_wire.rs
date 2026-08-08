//! The `ts` stamp on a journal line, from a consumer's side of the wire.
//!
//! Witness for #2111. Before it, `stella-events.jsonl` anchored nothing to wall
//! clock: `step_usage` and `tool_result` name a span (`duration_ms`) but no
//! instant, so a recorded trial could answer neither "when did this happen" nor
//! "where did the wall clock go between these two events" — and the arena
//! transcript, having nothing to subtract, rendered `0:00` for every row.
//!
//! These tests read the line the way every consumer does — as JSON, not as a
//! Rust value — because that is the surface the contract is made of. The three
//! properties that matter downstream:
//!
//!  1. the stamp is *there*, and it is the instant the sink supplied;
//!  2. it is a sibling key of `type`, not a new nesting level, so every reader
//!     written before #2111 keeps working unchanged;
//!  3. a line without it still parses, forever — archived benchmark evidence
//!     has none, and neither does a sink with no clock to offer.

use serde_json::Value;
use stella_protocol::{AgentEvent, StampedEvent, stamped_line};

/// The instant every assertion below is written against: 2026-08-07T16:00:00Z,
/// well past the point where a plausible-but-wrong `0` would pass unnoticed.
const TS: u64 = 1_754_582_400_123;

fn tool_result() -> AgentEvent {
    AgentEvent::ToolResult {
        call_id: "call-1".to_string(),
        output: stella_protocol::ToolOutput::Ok {
            content: "ok".to_string(),
        },
        duration_ms: 4_200,
        speculated: false,
    }
}

#[test]
fn a_journal_line_carries_the_instant_its_sink_supplied() {
    let line: Value = serde_json::from_str(&stamped_line(&tool_result(), TS).unwrap()).unwrap();

    assert_eq!(
        line.get("ts").and_then(Value::as_u64),
        Some(TS),
        "a journal line with no `ts` anchors nothing to wall clock: {line}"
    );
}

#[test]
fn the_stamp_is_a_sibling_of_the_tag_not_a_new_nesting_level() {
    // Every existing reader — arenabench's transcript reader, the harbor
    // adapter's live feed and exit-cause classifier — keys off `event["type"]`
    // at the top level. Nesting the event under an envelope member would have
    // broken all three at once; flattening costs them nothing.
    let line: Value = serde_json::from_str(&stamped_line(&tool_result(), TS).unwrap()).unwrap();

    assert_eq!(
        line.get("type").and_then(Value::as_str),
        Some("tool_result")
    );
    assert_eq!(line.get("duration_ms").and_then(Value::as_u64), Some(4_200));
    assert_eq!(line.get("call_id").and_then(Value::as_str), Some("call-1"));
}

#[test]
fn elapsed_is_computable_from_two_lines() {
    // The arena transcript's whole job, reduced to arithmetic: the offset of an
    // event from the first line of the trial. This is what could not be done at
    // all before the field existed.
    let first: StampedEvent =
        serde_json::from_str(&stamped_line(&tool_result(), TS).unwrap()).unwrap();
    let later: StampedEvent =
        serde_json::from_str(&stamped_line(&tool_result(), TS + 90_000).unwrap()).unwrap();

    let elapsed_ms = later.ts.zip(first.ts).map(|(later, first)| later - first);
    assert_eq!(elapsed_ms, Some(90_000));
}

#[test]
fn a_line_recorded_before_the_stamp_existed_still_parses() {
    // Every `stella-events.jsonl` under bench/evidence/ is this shape. A reader
    // that required `ts` would refuse to replay the archive.
    let archived = serde_json::to_string(&tool_result()).unwrap();
    let parsed: StampedEvent = serde_json::from_str(&archived).unwrap();

    assert_eq!(parsed.ts, None);
    assert!(matches!(parsed.event, AgentEvent::ToolResult { .. }));
}

#[test]
fn an_unstamped_line_is_byte_identical_to_the_bare_event() {
    // The reason this is an added key rather than a new format: with no stamp,
    // the envelope disappears from the wire entirely.
    let unstamped = StampedEvent {
        ts: None,
        event: tool_result(),
    };

    assert_eq!(
        serde_json::to_string(&unstamped).unwrap(),
        serde_json::to_string(&tool_result()).unwrap()
    );
}

#[test]
fn a_stamped_line_round_trips_byte_for_byte() {
    // Invariant #4, on the type that now crosses the CLI → arena boundary.
    let line = stamped_line(&tool_result(), TS).unwrap();
    let parsed: StampedEvent = serde_json::from_str(&line).unwrap();

    assert_eq!(serde_json::to_string(&parsed).unwrap(), line);
}

#[test]
fn an_event_from_a_newer_stella_keeps_both_its_body_and_its_stamp() {
    // Forward-compat and forensics must not be mutually exclusive: an
    // `AgentEvent::Unknown` re-emits the object it wrapped, and the stamp has to
    // survive that passthrough or the very lines an older reader cannot explain
    // would also be the ones it cannot place in time.
    let line = r#"{"ts":1754582400123,"type":"from_the_future","detail":{"a":1}}"#;
    let parsed: StampedEvent = serde_json::from_str(line).unwrap();

    assert_eq!(parsed.ts, Some(TS));
    assert!(parsed.event.is_unknown());
    assert_eq!(
        serde_json::from_str::<Value>(&serde_json::to_string(&parsed).unwrap()).unwrap(),
        serde_json::from_str::<Value>(line).unwrap()
    );
}
