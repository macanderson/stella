//! The `text` / `text_delta` wire spellings, both current and legacy.
//!
//! Before #1886 the two variants carried each other's natural field name on
//! the wire: the consolidated body went out as `{"type":"text","delta":…}`
//! and the live fragment as `{"type":"text_delta","text":…}`. Every consumer
//! of `stella-events.jsonl` had to learn the swap by observation (arenabench
//! rendered every response twice until its reader was taught both shapes).
//!
//! The contract now: serialization writes the self-describing name — `text`
//! carries `text`, `text_delta` carries `delta` — and deserialization accepts
//! the legacy spelling as an alias so every recorded stream keeps replaying.
//! These tests witness both halves; the legacy half must never be removed
//! while archived benchmark evidence exists.

use stella_protocol::AgentEvent;

#[test]
fn text_serializes_with_the_self_describing_field_name() {
    let event = AgentEvent::Text {
        text: "the full answer".into(),
    };
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"type":"text","text":"the full answer"}"#
    );
}

#[test]
fn text_delta_serializes_with_the_self_describing_field_name() {
    let event = AgentEvent::TextDelta {
        delta: "the fr".into(),
    };
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"type":"text_delta","delta":"the fr"}"#
    );
}

#[test]
fn current_spellings_round_trip() {
    let text: AgentEvent = serde_json::from_str(r#"{"type":"text","text":"full"}"#).unwrap();
    assert!(matches!(text, AgentEvent::Text { text } if text == "full"));

    let delta: AgentEvent = serde_json::from_str(r#"{"type":"text_delta","delta":"fr"}"#).unwrap();
    assert!(matches!(delta, AgentEvent::TextDelta { delta } if delta == "fr"));
}

#[test]
fn legacy_crossed_spellings_still_deserialize() {
    // The exact shapes every stream recorded before #1886 contains. Replay of
    // archived benchmark evidence depends on these parsing forever.
    let text: AgentEvent = serde_json::from_str(r#"{"type":"text","delta":"full"}"#).unwrap();
    assert!(matches!(text, AgentEvent::Text { text } if text == "full"));

    let delta: AgentEvent = serde_json::from_str(r#"{"type":"text_delta","text":"fr"}"#).unwrap();
    assert!(matches!(delta, AgentEvent::TextDelta { delta } if delta == "fr"));
}

#[test]
fn text_delta_keeps_its_own_type_tag() {
    // `text_delta` stays additive on the wire: a distinct tag from `text`,
    // so a consumer that skips unknown lines keeps parsing the authoritative
    // `text` events unchanged.
    let event = AgentEvent::TextDelta {
        delta: "Hel".into(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"type\":\"text_delta\""), "{json}");
    let back: AgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        AgentEvent::TextDelta { delta } => assert_eq!(delta, "Hel"),
        other => panic!("unexpected variant: {other:?}"),
    }
}
