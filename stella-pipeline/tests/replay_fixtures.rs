//! Golden-trajectory replay harness, driven against synthetic fixtures.
//! These fixtures are **synthetic** streams that exercise the harness's
//! invariants and its structural differ — hand-authored to hit specific edges
//! (a torn tail, a judge escalation) that a recording may not happen to
//! contain.
//!
//! Real recordings live elsewhere and are kept distinct on purpose:
//! `tests/fixtures/golden/` holds trajectories actually recorded from this
//! pipeline (see `src/pipeline/tests/golden.rs`), and
//! `tests/reference_conformance.rs` pins what an independent reference engine
//! must emit before its runs can join them. See
//! `docs/design/replay-golden-trajectories.md`.

use std::fs;
use std::path::PathBuf;

use stella_pipeline::replay::{parse_jsonl, streams_equivalent, structural_diff, validate_stream};
use stella_protocol::AgentEvent;

fn fixture(name: &str) -> String {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", name]
        .iter()
        .collect();
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading fixture {path:?}: {e}"))
}

#[test]
fn a_well_formed_trajectory_passes_every_invariant() {
    let events = parse_jsonl(&fixture("single_task_flip.jsonl")).expect("fixture parses");
    let violations = validate_stream(&events);
    assert!(
        violations.is_empty(),
        "expected a clean stream, got violations: {violations:?}"
    );
}

#[test]
fn two_runs_of_the_same_flow_are_structurally_equivalent() {
    // Same staged flow, different providers/costs/ids/summaries — the golden
    // replay must treat them as equivalent (kinds + order, volatile fields
    // ignored).
    let run = parse_jsonl(&fixture("single_task_flip.jsonl")).unwrap();
    let reference = parse_jsonl(&fixture("single_task_flip_reference.jsonl")).unwrap();
    let diff = structural_diff(&run, &reference);
    assert!(
        streams_equivalent(&run, &reference),
        "structurally-equal runs must not diff; got: {diff:?}"
    );
}

#[test]
fn a_judge_escalation_diverges_from_a_deterministic_pass() {
    // The deterministic-flip path skips the judge; the escalation path has a
    // `judge` stage and a non-deterministic verdict — they must diverge.
    let deterministic = parse_jsonl(&fixture("single_task_flip.jsonl")).unwrap();
    let escalation = parse_jsonl(&fixture("judge_escalation.jsonl")).unwrap();
    let diff = structural_diff(&deterministic, &escalation);
    assert!(
        !diff.is_empty(),
        "a submit-fast run and a judge-escalation run must be structurally distinct"
    );
    // And the escalation stream is itself well-formed.
    assert!(validate_stream(&escalation).is_empty());
}

#[test]
fn a_stream_from_a_newer_stella_parses_and_stays_valid() {
    // The conformance fixture: a well-formed turn carrying two event types
    // this build has never heard of. An older reader must take the whole
    // stream, not choke on the first line it does not recognize.
    //
    // Any client in any language that claims to consume stella's event stream
    // should be held to exactly this: parse it, skip what you cannot decode,
    // and keep going. A client that errors on `from_a_newer_stella.jsonl` is
    // not forward compatible, however well it handles today's vocabulary.
    let events = parse_jsonl(&fixture("from_a_newer_stella.jsonl"))
        .expect("future events must not be fatal");
    assert_eq!(
        events.len(),
        10,
        "every line is kept, including the unknown"
    );

    let unknown: Vec<&str> = events
        .iter()
        .filter(|e| e.is_unknown())
        .map(AgentEvent::type_tag)
        .collect();
    assert_eq!(unknown, vec!["quantum_reticulation", "holographic_verdict"]);

    // The known vocabulary around them still validates: stage ordering, tool
    // pairing, and the terminal `complete` are unaffected by their presence.
    let violations = validate_stream(&events);
    assert!(
        violations.is_empty(),
        "unknown events must not break stream invariants, got: {violations:?}"
    );

    // Payloads survive intact, so this reader could re-export the stream
    // without dropping what it could not interpret.
    let reticulation = events.iter().find(|e| e.is_unknown()).unwrap();
    let AgentEvent::Unknown { payload, .. } = reticulation else {
        unreachable!("filtered above")
    };
    assert_eq!(payload["splines"][1], "beta");
    assert_eq!(payload["nested"]["depth"], 3);
}

#[test]
fn a_newer_stream_is_structurally_equivalent_to_its_older_recording() {
    // The point of the whole exercise, stated as a comparison: the same run
    // recorded by a newer stella (with two extra event types) and by an older
    // one must be judged structurally equivalent. If unknown events counted
    // toward the positional diff, every golden trajectory would break the
    // moment the vocabulary grew — which is precisely the failure the
    // `structural_diff` keep-set exists to prevent.
    let newer = parse_jsonl(&fixture("from_a_newer_stella.jsonl")).unwrap();
    let older = parse_jsonl(&fixture("from_a_newer_stella_baseline.jsonl")).unwrap();
    let diff = structural_diff(&newer, &older);
    assert!(
        streams_equivalent(&newer, &older),
        "a vocabulary addition must not read as behavioural drift; got: {diff:?}"
    );
}

#[test]
fn a_torn_writer_tail_is_tolerated_not_fatal() {
    // A crashed writer left a partial final line; the reader keeps the clean
    // prefix (L-T1).
    let events = parse_jsonl(&fixture("torn_tail.jsonl")).expect("torn tail must not fail parsing");
    assert_eq!(events.len(), 3, "the partial final line is dropped");
    // The recovered prefix is still a legal (if unterminated) stage sequence.
    assert!(validate_stream(&events).is_empty());
}
