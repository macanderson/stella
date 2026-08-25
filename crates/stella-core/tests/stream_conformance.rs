// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The structural rules, run over a recording on disk rather than over a
//! stream a test just built.
//!
//! `crates/stella-core/src/event_stream.rs`'s own unit tests construct every
//! stream they check, which proves the rules but not that they survive a
//! round trip through bytes somebody else wrote. This file checks the
//! recording — `tests/fixtures/from_a_newer_stella.jsonl`, restored in #4585
//! from the fixture #3865 deleted along with the validator.
//!
//! What that fixture is for is the *forward* direction. It is a well-formed
//! run carrying two event types this build has never heard of
//! (`quantum_reticulation`, `holographic_verdict`), one of them sitting
//! between a `tool_start` and its `tool_result`. A checker that rejected an
//! unrecognized line, or that let one break a pairing it is not part of,
//! would make every recording from a newer Stella unreadable by an older one
//! — which is the property `AgentEvent::Unknown` exists to give and the one a
//! conformance gate is most likely to take away by accident.

use std::fs;
use std::path::PathBuf;

use stella_core::event_stream::{JsonlError, conform_jsonl, parse_jsonl, to_jsonl};

fn fixture(name: &str) -> String {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", name]
        .iter()
        .collect();
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("reading fixture {path:?}: {err}"))
}

#[test]
fn a_recording_from_a_newer_stella_conforms() {
    let violations = conform_jsonl(&fixture("from_a_newer_stella.jsonl"))
        .expect("an unrecognized event type is not a malformed line");
    assert!(
        violations.is_empty(),
        "the fixture is a well-formed run; got {violations:?}"
    );
}

#[test]
fn the_unrecognized_events_are_read_rather_than_dropped() {
    let events = parse_jsonl(&fixture("from_a_newer_stella.jsonl")).expect("the fixture reads");
    let unknown: Vec<&str> = events
        .iter()
        .filter(|event| event.is_unknown())
        .map(stella_protocol::AgentEvent::type_tag)
        .collect();
    assert_eq!(unknown, ["quantum_reticulation", "holographic_verdict"]);
}

/// The negative half. A gate asserted only against a conforming recording is
/// indistinguishable from one that reports nothing at all, and that failure
/// mode is silent: it stays green forever while checking nothing. So the same
/// recording is broken in the one way each rule exists to catch, through the
/// same read path.
#[test]
fn breaking_the_recording_is_caught_through_the_same_read_path() {
    let clean = parse_jsonl(&fixture("from_a_newer_stella.jsonl")).expect("the fixture reads");

    // Drop the `tool_result` and the call it answered stays open.
    let mut orphaned = clean.clone();
    orphaned.retain(|event| !matches!(event, stella_protocol::AgentEvent::ToolResult { .. }));
    let violations = conform_jsonl(&to_jsonl(&orphaned)).expect("still a readable recording");
    assert!(
        violations.iter().any(|violation| matches!(
            violation,
            stella_core::event_stream::StreamViolation::UnmatchedToolStart { .. }
        )),
        "an unanswered tool call must be caught; got {violations:?}"
    );

    // Anything after the terminator.
    let mut trailing = clean.clone();
    trailing.push(stella_protocol::AgentEvent::Text {
        text: "one more thing".to_string(),
    });
    let violations = conform_jsonl(&to_jsonl(&trailing)).expect("still a readable recording");
    assert!(
        violations.iter().any(|violation| matches!(
            violation,
            stella_core::event_stream::StreamViolation::EventAfterRunComplete { .. }
        )),
        "an event after run_complete must be caught; got {violations:?}"
    );
}

/// The one outcome a conformance gate must never confuse with conformance: a
/// recording it could not read.
#[test]
fn an_unreadable_recording_is_an_error_not_a_clean_bill_of_health() {
    let recording = fixture("from_a_newer_stella.jsonl");
    let split = recording[..recording.len() / 2].rfind('\n').unwrap() + 1;
    let mut torn = recording.clone();
    torn.insert_str(split, "{ not valid json }\n");
    assert!(matches!(
        conform_jsonl(&torn),
        Err(JsonlError::MalformedLine { .. })
    ));
}
