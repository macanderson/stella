//! The event stream's structural invariants, run as a conformance gate over
//! recorded fixtures — in both directions.
//!
//! `docs/wire/agentevent.schema.json` (issue #971) describes what one *event*
//! may look like, and `stella-protocol/tests/wire_contract.rs` proves every
//! variant satisfies it. Neither says anything about what a *stream* may look
//! like: JSON Schema has no way to express "a `tool_start` is answered later
//! by a `tool_result` with the same `call_id`", or "`complete` is last", or
//! "spend never goes backwards". Those live in
//! [`stella_pipeline::replay::validate_stream`] — the design doc that asked
//! for this places the validator in `stella-protocol`, which it never was —
//! and this file is where they are actually *run* against recordings.
//!
//! The half that matters is the negative one. A validator asserted only
//! against well-formed input is indistinguishable from `fn validate(_) -> []`,
//! and that failure mode is quiet: the gate stays green forever while checking
//! nothing. So every invariant here is exercised twice — once on a recording
//! that must pass, and once on the same recording deliberately corrupted in
//! the one way that invariant exists to catch.

use std::fs;
use std::path::PathBuf;

use stella_pipeline::replay::{StreamViolation, conform_jsonl, to_jsonl};
use stella_protocol::{AgentEvent, StageKind};

fn fixture(relative: &str) -> String {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", relative]
        .iter()
        .collect();
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading fixture {path:?}: {e}"))
}

/// Recordings that must conform. The first two are real trajectories recorded
/// from this pipeline (`tests/fixtures/golden/`, gated by their manifests);
/// the rest are the synthetic streams that hit edges a recording may not
/// happen to contain — a verifier escalation, a stream from a newer stella, a
/// writer that crashed mid-line.
const CONFORMING: &[&str] = &[
    "golden/single_task_deterministic_flip.jsonl",
    "golden/verifier_escalation_without_a_test_command.jsonl",
    "single_task_flip.jsonl",
    "single_task_flip_reference.jsonl",
    "verifier_escalation.jsonl",
    "from_a_newer_stella.jsonl",
    "from_a_newer_stella_baseline.jsonl",
    "torn_tail.jsonl",
];

#[test]
fn every_recorded_fixture_stream_conforms() {
    for name in CONFORMING {
        let violations = conform_jsonl(&fixture(name))
            .unwrap_or_else(|e| panic!("{name} is not a readable recording: {e}"));
        assert!(
            violations.is_empty(),
            "{name} must conform to the stream invariants, got: {violations:?}"
        );
    }
}

/// One way to break a stream, and the violation it must produce.
struct Corruption {
    /// What the corruption is, for the failure message.
    name: &'static str,
    /// Which invariant it exists to trip, as a substring of the reason.
    expect: &'static str,
    /// Applied to a parsed copy of the conforming fixture.
    apply: fn(&mut Vec<AgentEvent>),
}

/// The corruptions, one per invariant `validate_stream` claims to enforce.
fn corruptions() -> Vec<Corruption> {
    vec![
        Corruption {
            name: "an out-of-order stage (Complete, then back to Triage)",
            expect: "illegal stage transition",
            apply: |events| {
                // Insert a backward jump that is not one of the two legal
                // revise back-edges, before the terminal event.
                let at = events.len() - 1;
                events.insert(
                    at,
                    AgentEvent::Stage {
                        name: StageKind::Triage,
                    },
                );
            },
        },
        Corruption {
            name: "a second Complete",
            expect: "more than one Complete",
            apply: |events| {
                let terminal = events
                    .iter()
                    .rev()
                    .find(|e| matches!(e, AgentEvent::Complete { .. }))
                    .expect("the fixture terminates")
                    .clone();
                events.push(terminal);
            },
        },
        Corruption {
            name: "an event after Complete",
            expect: "not the last event",
            apply: |events| {
                events.push(AgentEvent::Text {
                    delta: "one more thing".into(),
                });
            },
        },
        Corruption {
            name: "a tool_start whose tool_result was dropped",
            expect: "never matched",
            apply: |events| {
                let at = events
                    .iter()
                    .position(|e| matches!(e, AgentEvent::ToolResult { .. }))
                    .expect("the fixture calls a tool");
                events.remove(at);
            },
        },
        Corruption {
            name: "a tool_result with no preceding tool_start",
            expect: "no preceding tool_start",
            apply: |events| {
                let at = events
                    .iter()
                    .position(|e| matches!(e, AgentEvent::ToolStart { .. }))
                    .expect("the fixture calls a tool");
                events.remove(at);
            },
        },
        Corruption {
            name: "budget spend going backwards",
            expect: "backwards",
            apply: |events| {
                let last = events
                    .iter()
                    .rposition(|e| matches!(e, AgentEvent::BudgetTick { .. }))
                    .expect("the fixture meters spend");
                if let AgentEvent::BudgetTick { spent_usd, .. } = &mut events[last] {
                    *spent_usd = -1.0;
                }
            },
        },
    ]
}

#[test]
fn each_corruption_of_a_conforming_stream_is_caught() {
    // `single_task_flip.jsonl` is the fixture that exercises all four
    // invariants at once: staged, tool-calling, budget-metered, terminated.
    let clean = stella_pipeline::replay::parse_jsonl(&fixture("single_task_flip.jsonl"))
        .expect("the fixture parses");

    for corruption in corruptions() {
        let mut broken = clean.clone();
        (corruption.apply)(&mut broken);
        let violations = conform_jsonl(&to_jsonl(&broken))
            .expect("a corrupted stream is still readable, just illegal");
        assert!(
            violations
                .iter()
                .any(|v: &StreamViolation| v.reason.contains(corruption.expect)),
            "{} must be caught by a `{}` violation, got: {violations:?}",
            corruption.name,
            corruption.expect
        );
    }
}

#[test]
fn the_corruption_set_covers_every_invariant_the_validator_claims() {
    // The gate is only as honest as its negative cases. `validate_stream`'s
    // doc comment enumerates four invariants; if a fifth is added without a
    // corruption to trip it, this fails rather than letting an unexercised
    // check ride along looking enforced.
    let expectations: Vec<&str> = corruptions().into_iter().map(|c| c.expect).collect();
    for invariant in [
        "illegal stage transition", // 1. legal stage ordering
        "never matched",            // 2. tool pairing, open side
        "no preceding tool_start",  // 2. tool pairing, orphan side
        "more than one Complete",   // 3. terminal Complete, count
        "not the last event",       // 3. terminal Complete, position
        "backwards",                // 4. monotonic budget
    ] {
        assert!(
            expectations.contains(&invariant),
            "no corruption exercises the `{invariant}` invariant"
        );
    }
}

#[test]
fn an_unreadable_recording_is_an_error_not_a_clean_bill_of_health() {
    // The one outcome a conformance gate must never confuse with conformance:
    // a stream it could not read. A malformed *interior* line is real damage
    // (a torn tail is not — `torn_tail.jsonl` is in CONFORMING above).
    let mut broken = fixture("single_task_flip.jsonl");
    broken.insert_str(broken.len() / 2, "{ not valid json }\n");
    assert!(
        conform_jsonl(&broken).is_err(),
        "an unreadable recording must fail loudly, never report zero violations"
    );
}
