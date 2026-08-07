//! The tag table's own contract, as distinct from the per-variant
//! round-trips in the parent module: that `type_tag()` agrees with what
//! serde writes, that [`KNOWN_TYPE_TAGS`] is total over the typed variants,
//! and that a tag from a *newer* stella degrades to
//! [`AgentEvent::Unknown`] instead of failing the parse.
//!
//! Split from `event/tests.rs` (#1857) rather than left to grow: the parent
//! gains a test per new variant, and one file carrying both concerns was
//! within thirty lines of the file-size ratchet.

use super::*;

#[test]
fn type_tag_matches_the_serde_type_wire_tag() {
    // `type_tag()` must return exactly the `"type"` string serde writes,
    // since both come from the same `snake_case` variant name. The match's
    // exhaustiveness is compiler-enforced (a new variant cannot escape a
    // tag), so this only pins that the hand-written strings are correct —
    // cross-checked against serde for a representative sample, weighted to
    // the recently added variants most prone to a copy-paste tag.
    let sample = vec![
        AgentEvent::Stage {
            name: StageKind::Triage,
        },
        AgentEvent::Text { text: "hi".into() },
        AgentEvent::TextDelta { delta: "h".into() },
        AgentEvent::Reasoning { delta: "r".into() },
        AgentEvent::SpeculationDiscarded {
            call_id: "c".into(),
            name: "n".into(),
            reason: "attempt_failed".into(),
        },
        AgentEvent::Retry {
            attempt: 1,
            reason: "x".into(),
        },
        AgentEvent::Steered { text: "s".into() },
        AgentEvent::TurnParked {
            description: "CI settles".into(),
            poll_interval_secs: 5,
            deadline_secs: 600,
        },
        AgentEvent::TurnWoken {
            reason: "changed".into(),
            polls_used: 3,
        },
        AgentEvent::LoopDetected {
            turn_instance: 1,
            kind: "exact_repeat".into(),
            pattern: vec!["read".into()],
            repeats: 2,
            evidence: "e".into(),
            aborted: false,
        },
        AgentEvent::BudgetDenied {
            scope: BudgetScope::Turn,
            spent_usd: 1.0,
            limit_usd: 0.5,
            mode: BudgetMode::Enforced,
        },
        AgentEvent::RetriesExhausted {
            turn_instance: 1,
            attempts: 3,
            reasons: vec!["t".into()],
            retryable: true,
        },
        AgentEvent::PolicyDecision {
            kind: PolicyKind::Blocked,
            subject: "write_file".into(),
            outcome: "deny".into(),
        },
        AgentEvent::BudgetTick {
            spent_usd: 0.1,
            limit_usd: None,
            mode: BudgetMode::Off,
            session_spent_usd: None,
            session_limit_usd: None,
        },
        AgentEvent::UsageIncomplete {
            role: ModelCallRole::Worker,
            provider: "z".into(),
            model: "m".into(),
            reason: UsageIncompleteReason::Timeout,
            duration_ms: 1,
            retries: None,
            partial: None,
        },
        AgentEvent::GoalVerdict {
            round: 1,
            met: true,
            reasoning: "ok".into(),
            cost_usd: 0.0,
        },
        AgentEvent::ProviderFallback {
            from: "a".into(),
            to: "b".into(),
            reason: "r".into(),
        },
        AgentEvent::Commit {
            sha: "abc".into(),
            message: "m".into(),
        },
        AgentEvent::Pr {
            url: "u".into(),
            status: PrStatus::Open,
            number: None,
            ci: None,
        },
        AgentEvent::Error {
            message: "m".into(),
            retryable: false,
        },
        AgentEvent::Complete {
            model: "m".into(),
            cost_usd: 0.0,
        },
    ];
    for event in &sample {
        let value = serde_json::to_value(event).unwrap();
        let wire = value
            .get("type")
            .and_then(|tag| tag.as_str())
            .unwrap_or_else(|| panic!("event has no string `type` tag: {event:?}"));
        assert_eq!(
            event.type_tag(),
            wire,
            "type_tag disagrees with the serde wire tag for {event:?}"
        );
    }
    // Pin two exact tags so a wholesale serde-rename change is caught too.
    assert_eq!(
        AgentEvent::TextDelta {
            delta: String::new()
        }
        .type_tag(),
        "text_delta"
    );
    assert_eq!(
        AgentEvent::SpeculationDiscarded {
            call_id: String::new(),
            name: String::new(),
            reason: String::new(),
        }
        .type_tag(),
        "speculation_discarded"
    );
}

// ---- forward compatibility: events from a newer stella ----------------

#[test]
fn an_unrecognized_type_tag_degrades_to_unknown_rather_than_failing() {
    // The whole point of the change. A reader built before this variant
    // existed must not fail the line — it must keep going.
    let line = r#"{"type":"quantum_entangled","turn":7,"nested":{"a":[1,2]}}"#;
    let event: AgentEvent = serde_json::from_str(line).expect("future event must parse");
    let AgentEvent::Unknown {
        event_type,
        payload,
    } = &event
    else {
        panic!("expected Unknown, got {event:?}");
    };
    assert_eq!(event_type, "quantum_entangled");
    assert_eq!(payload["turn"], 7);
    assert_eq!(payload["nested"]["a"][1], 2);
    assert!(event.is_unknown());
}

#[test]
fn an_unknown_event_round_trips_without_losing_data() {
    // A recorder or proxy must be able to pass a future event through
    // without corrupting it. Compare parsed values, not raw bytes: object
    // key order is not preserved (and is not meaningful).
    let line = r#"{"type":"from_the_future","alpha":1,"beta":["x",null,true]}"#;
    let event: AgentEvent = serde_json::from_str(line).unwrap();
    let reserialized = serde_json::to_string(&event).unwrap();

    let before: Value = serde_json::from_str(line).unwrap();
    let after: Value = serde_json::from_str(&reserialized).unwrap();
    assert_eq!(before, after, "round-trip changed the event's content");
    // The tag must survive: it lives only inside `payload`.
    assert_eq!(after["type"], "from_the_future");
}

#[test]
fn a_known_tag_with_a_malformed_body_is_still_a_hard_error() {
    // The load-bearing negative test. Forward compatibility is scoped to
    // *unrecognized tags only*; a recognized tag whose body does not fit
    // is an encoder bug or a corrupt record, and must stay loud. If this
    // ever degrades to Unknown, corruption becomes indistinguishable from
    // a version skew and the store's `skipped` counter starts lying.
    let err = serde_json::from_str::<AgentEvent>(r#"{"type":"text"}"#)
        .expect_err("`text` without `delta` must not parse");
    assert!(
        !format!("{err}").is_empty(),
        "a known tag with a bad body must produce a real error"
    );

    // Same for a wrong field type under a known tag.
    assert!(
        serde_json::from_str::<AgentEvent>(r#"{"type":"retry","attempt":"one","reason":"x"}"#)
            .is_err(),
        "`retry.attempt` is a u32; a string must not be laundered into Unknown"
    );

    // And an object with no tag at all keeps the derived error path.
    assert!(serde_json::from_str::<AgentEvent>(r#"{"delta":"hi"}"#).is_err());
}

#[test]
fn every_known_type_tag_resolves_to_a_typed_variant() {
    // Proves the variant→tag list in `agent_event_tags!` has no typo.
    //
    // Combined with two structural facts this is airtight: the generated
    // match is exhaustive (so every variant is listed) and duplicate arms
    // would be unreachable (so each is listed once) — meaning
    // `KNOWN_TYPE_TAGS.len()` equals the variant count. If every listed tag
    // is additionally a *real* serde name, as asserted below, the mapping is
    // a bijection and no real tag can be missing from the list. A missing
    // one would silently demote all of its events to `Unknown`.
    //
    // The probe is a bare `{"type": tag}`, and today every variant has at
    // least one required field, so it always errors. That is expected — the
    // assertion is on WHICH error. `missing field ...` proves serde routed
    // the tag to a variant; `unknown variant ...` proves it did not, which
    // is exactly the typo this test exists to catch. Asserting only on the
    // `Ok` arm would assert nothing at all.
    for tag in KNOWN_TYPE_TAGS {
        let probe = serde_json::json!({ "type": tag });
        match serde_json::from_value::<AgentEvent>(probe) {
            Ok(event) => assert!(
                !event.is_unknown(),
                "`{tag}` is in KNOWN_TYPE_TAGS but deserialized as Unknown — \
                 the tag string does not match any serde variant name"
            ),
            Err(err) => assert!(
                !err.to_string().contains("unknown variant"),
                "`{tag}` is in KNOWN_TYPE_TAGS but serde has no variant by \
                 that name, so every event carrying it decodes as Unknown: {err}"
            ),
        }
    }

    let mut sorted = KNOWN_TYPE_TAGS.to_vec();
    sorted.sort_unstable();
    let before = sorted.len();
    sorted.dedup();
    assert_eq!(before, sorted.len(), "duplicate tag in KNOWN_TYPE_TAGS");
}

#[test]
fn type_tag_of_an_unknown_event_is_the_preserved_wire_tag() {
    // `type_tag()` tells the truth even for an event it cannot decode, so
    // logs and metrics can name a future event correctly.
    let event: AgentEvent = serde_json::from_str(r#"{"type":"newer_thing"}"#).unwrap();
    assert_eq!(event.type_tag(), "newer_thing");
    assert!(!KNOWN_TYPE_TAGS.contains(&event.type_tag()));
}

#[test]
fn a_literal_unknown_tag_is_not_privileged() {
    // `Unknown` is `serde(skip)`, so it has no wire tag of its own. An
    // event literally tagged `"unknown"` is just another unrecognized tag
    // and must round-trip as one rather than being mistaken for the
    // fallback variant's own encoding.
    let line = r#"{"type":"unknown","event_type":"spoof","payload":{}}"#;
    let event: AgentEvent = serde_json::from_str(line).unwrap();
    assert_eq!(event.type_tag(), "unknown");

    // It round-trips as the opaque object it is — the decoy `event_type`
    // and `payload` keys stay ordinary payload data, not the variant's
    // own fields.
    let after: Value = serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
    assert_eq!(after, serde_json::from_str::<Value>(line).unwrap());
    assert_eq!(after["event_type"], "spoof");
}

#[test]
fn a_known_event_wire_format_is_unchanged_by_the_fallback() {
    // The hand-written codec must be a pass-through for known variants —
    // no tag rename, no extra wrapper, no reordering.
    let event = AgentEvent::Text {
        text: "hello".into(),
    };
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"type":"text","text":"hello"}"#
    );
    let back: AgentEvent = serde_json::from_str(r#"{"type":"text","text":"hello"}"#).unwrap();
    assert!(matches!(back, AgentEvent::Text { text } if text == "hello"));
}
