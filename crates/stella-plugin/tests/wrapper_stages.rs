//! #3381's acceptance for the manifest half: the stage order leaves Rust,
//! and every way of writing a manifest that would quietly do nothing is a
//! named load error instead.
//!
//! The two fixtures are the load-bearing claim. `wrapper-staged-v1.toml`
//! transcribes the order the staged pipeline's `pipeline.rs` ran
//! (`crates/stella-pipeline`, deleted in #3865);
//! `wrapper-lean-v1.toml` is a cheaper second shape. They differ in nothing
//! but their text, which is what makes the manifest a declaration the code
//! reads rather than a description of one hardcoded path.
//!
//! What these tests deliberately do **not** claim: that either variant *runs*.
//! Binding a stage name to the loop needs the four wrapper interception
//! points of #3380, which do not exist yet. Everything here is the load-time
//! contract, which is complete on its own.

use stella_plugin::{
    CompareOp, Condition, ManifestError, PluginManifest, Signal, SignalKind, SignalValues,
    StageName, Wrapper,
};
use stella_protocol::StageKind;

const STAGED_V1: &str = include_str!("fixtures/wrapper-staged-v1.toml");
const LEAN_V1: &str = include_str!("fixtures/wrapper-lean-v1.toml");
const EVIDENCE_V1: &str = include_str!("fixtures/wrapper-evidence-v1.toml");

fn parse(text: &str) -> Result<PluginManifest, ManifestError> {
    PluginManifest::from_toml_str(text)
}

/// A `[wrapper]` block with `participation = "steering"` and the given
/// stage list, so each rejection test varies exactly one thing.
fn wrapper_manifest(stages: &str) -> String {
    format!(
        "name = \"probe\"\n\
         [loop]\n\
         participation = \"steering\"\n\
         [wrapper]\n\
         id = \"probe-v1\"\n\
         {stages}"
    )
}

// --- The two variants both load, from the same code. ---------------------

#[test]
fn todays_stage_order_loads_as_a_manifest() {
    let manifest = parse(STAGED_V1).expect("the shipped order must load");
    let wrapper = manifest.wrapper.expect("[wrapper] must parse");

    assert_eq!(wrapper.id, "classic", "the variant id the store records");
    let names: Vec<StageName> = wrapper.stages.iter().map(|s| s.name).collect();
    assert_eq!(
        names,
        vec![
            StageName::Triage,
            StageName::Recall,
            StageName::Research,
            StageName::Plan,
            StageName::Scope,
            StageName::Execute,
            StageName::Witness,
            StageName::Verify,
        ],
        "the order must match the stage_rank the staged pipeline's replay.rs \
         defined before #3865 deleted that crate — this enum is the ordering now"
    );
}

/// #3408 P2: `Wrapper` crosses a crate boundary on the wire — `stella-cli`'s
/// `PipelineFrame::variant` persists it beside a killed run's checkpoint so a
/// resume restores the same manifest, not the built-in `classic` fallback
/// (invariant 4: serde-first, round-trip when a type crosses a boundary).
/// `Wrapper`/`WrapperStage` already derive `Serialize`/`Deserialize`; this is
/// the witness that the round-trip is actually byte-identical, not merely
/// that the derive compiles.
#[test]
fn a_wrapper_round_trips_through_json_byte_for_byte() {
    let wrapper = parse(STAGED_V1)
        .expect("the shipped order must load")
        .wrapper
        .expect("[wrapper] must parse");

    let json = serde_json::to_string(&wrapper).expect("a validated Wrapper always serializes");
    let restored: Wrapper =
        serde_json::from_str(&json).expect("what this crate just wrote, it can read back");
    assert_eq!(
        restored, wrapper,
        "the round trip must reproduce the value exactly"
    );

    // And the round trip is itself stable — serializing the restored value
    // again produces the identical bytes, not merely an equal value.
    let json_again =
        serde_json::to_string(&restored).expect("a value that just parsed always serializes");
    assert_eq!(
        json_again, json,
        "re-serializing must reproduce the same bytes"
    );
}

#[test]
fn a_second_cheaper_variant_loads_from_the_same_code() {
    let staged = parse(STAGED_V1).unwrap().wrapper.unwrap();
    let lean = parse(LEAN_V1).unwrap().wrapper.unwrap();

    assert_ne!(staged.id, lean.id, "two variants need two join keys");
    assert!(
        lean.stages.len() < staged.stages.len(),
        "the lean variant must actually be cheaper, not merely differently named"
    );
    // The claim that matters: nothing in Rust distinguishes them.
    assert!(
        lean.stages
            .iter()
            .all(|s| staged.stages.iter().any(|t| t.name == s.name)),
        "the lean variant must be a subset of the shipped vocabulary"
    );
}

#[test]
fn the_shipped_conditions_parse_to_the_branches_they_transcribe() {
    let wrapper = parse(STAGED_V1).unwrap().wrapper.unwrap();
    let condition = |name: StageName| {
        wrapper
            .stages
            .iter()
            .find(|s| s.name == name)
            .expect("stage declared")
            .condition()
            .expect("condition parses")
    };

    assert_eq!(
        condition(StageName::Research),
        Some(Condition::Compare {
            signal: Signal::Questions,
            op: CompareOp::Greater,
            value: 0,
        }),
        "\"empty questions skip the stage byte-for-byte\""
    );
    assert_eq!(
        condition(StageName::Witness),
        Some(Condition::Boolean {
            signal: Signal::TestCommand,
            negated: true,
        }),
        "witness authoring is gated on there being no configured test command"
    );
    assert_eq!(
        condition(StageName::Recall),
        None,
        "an omitted `if` is unconditional, not a condition that never fires"
    );
}

// --- The vocabulary covers the pipeline (A8). -----------------------------

/// The mirror the [`StageName`] docs claim, mechanically: the manifest
/// vocabulary is one-to-one onto the workspace's one stage vocabulary. The
/// `match` is exhaustive, so a new [`StageKind`] variant fails this file to
/// compile rather than leaving a boundary no wrapper can name.
#[test]
fn every_stage_the_workspace_names_is_declarable() {
    for kind in [
        StageKind::Triage,
        StageKind::ContextRecall,
        StageKind::Research,
        StageKind::Plan,
        StageKind::ScopeReview,
        StageKind::Execute,
        StageKind::Witness,
        StageKind::Verify,
        StageKind::Verdict,
        StageKind::Reflect,
        StageKind::ContextWrite,
        StageKind::Complete,
    ] {
        // Exhaustive on purpose: this arm is what a thirteenth `StageKind`
        // breaks, and breaking here is the point.
        let name = match kind {
            StageKind::Triage => StageName::Triage,
            StageKind::ContextRecall => StageName::Recall,
            StageKind::Research => StageName::Research,
            StageKind::Plan => StageName::Plan,
            StageKind::ScopeReview => StageName::Scope,
            StageKind::Execute => StageName::Execute,
            StageKind::Witness => StageName::Witness,
            StageKind::Verify => StageName::Verify,
            StageKind::Verdict => StageName::Verdict,
            StageKind::Reflect => StageName::Reflect,
            StageKind::ContextWrite => StageName::ContextWrite,
            StageKind::Complete => StageName::Complete,
        };
        assert_eq!(
            name.kind(),
            kind,
            "{name} must denote the stage vocabulary's own {kind:?}"
        );
        // And the manifest spelling round-trips, so the name a wrapper writes
        // is the name this mapping is keyed on.
        let text = wrapper_manifest(&format!(
            "[[wrapper.stages]]\n\
             name = \"{name}\"\n"
        ));
        let wrapper = parse(&text)
            .unwrap_or_else(|err| panic!("\"{name}\" must be declarable, got {err:?}"))
            .wrapper
            .expect("[wrapper] declared");
        assert_eq!(wrapper.stages[0].name, name);
    }
}

/// The witness for A8's second half: a stage gated on a signal that only
/// **execute**, **witness** or **verify** publishes loads, and resolves into
/// the order the values imply. Before this change none of those signals
/// existed — every condition here was `UnknownSignal`, and four of the seven
/// stage names were an unknown-variant parse error.
#[test]
fn a_variant_gated_on_the_new_signals_loads_and_resolves() {
    let wrapper = parse(EVIDENCE_V1)
        .expect("the evidence variant must load")
        .wrapper
        .expect("[wrapper] must parse");
    assert_eq!(wrapper.id, "evidence-v1");
    // The new stage names and conditions survive a serde round-trip too, the
    // same claim `both_variants_round_trip_through_toml_and_json` makes of the
    // two older fixtures (invariant 4).
    let parsed = parse(EVIDENCE_V1).unwrap();
    assert_eq!(parsed, parse(&toml::to_string(&parsed).unwrap()).unwrap());
    let json = serde_json::to_string(&parsed).unwrap();
    assert_eq!(parsed, serde_json::from_str(&json).unwrap());

    // A turn that did work, wanted a witness, and left the suite green.
    let earned = wrapper
        .resolve(&SignalValues {
            wants_witness: true,
            mutating_actions: 6,
            diff_lines: 42,
            witness_authored: true,
            flip_achieved: true,
            tests_green: true,
            ..bare()
        })
        .expect("a validated manifest resolves for every set of values");
    assert_eq!(
        earned.stages(),
        [
            StageName::Triage,
            StageName::Execute,
            StageName::Witness,
            StageName::Verify,
            StageName::Reflect,
            StageName::ContextWrite,
            StageName::Complete,
        ]
    );

    // The same manifest over a turn that produced nothing: no witness worth
    // authoring, nothing to reflect on, nothing corroborated to write back —
    // but still graded, because "it changed nothing" is a finding.
    let idle = wrapper.resolve(&bare()).expect("resolution is total");
    assert_eq!(
        idle.stages(),
        [
            StageName::Triage,
            StageName::Execute,
            StageName::Verify,
            StageName::Complete,
        ]
    );
}

/// The other half of the witness, and the rule the new publishers must not
/// weaken: a signal is readable only *after* the stage that produces it. Every
/// new publisher gets asked, because the check is per-signal and one
/// mis-declared `publisher()` would silently exempt exactly one of them.
#[test]
fn reading_a_later_stages_new_signal_is_still_a_load_error() {
    for (reader, condition, signal, publisher) in [
        (
            StageName::Triage,
            "diff-lines > 0",
            Signal::DiffLines,
            StageName::Execute,
        ),
        (
            StageName::Execute,
            "witness-authored",
            Signal::WitnessAuthored,
            StageName::Witness,
        ),
        (
            StageName::Witness,
            "flip-achieved",
            Signal::FlipAchieved,
            StageName::Verify,
        ),
        (
            StageName::Execute,
            "tests-red",
            Signal::TestsRed,
            StageName::Verify,
        ),
    ] {
        // The publisher is declared, just too late — which is precisely the
        // manifest that would load and then wedge without the graph check.
        let text = wrapper_manifest(&format!(
            "[[wrapper.stages]]\n\
             name = \"{reader}\"\n\
             if = \"{condition}\"\n\
             [[wrapper.stages]]\n\
             name = \"{publisher}\"\n"
        ));
        match parse(&text) {
            Err(ManifestError::SignalNotYetPublished {
                stage,
                signal: named,
                publisher: by,
            }) => {
                assert_eq!(stage, reader);
                assert_eq!(named, signal);
                assert_eq!(by, publisher);
            }
            other => panic!("expected SignalNotYetPublished for \"{condition}\", got {other:?}"),
        }
    }
}

/// Every signal at its emptiest. [`SignalValues`] refuses `Default` so that a
/// new signal must be answered somewhere; this is that somewhere for the
/// example above, which varies only the fields it is about.
fn bare() -> SignalValues {
    SignalValues {
        test_command: false,
        candidates: 1,
        budget_metered: false,
        conversational: false,
        questions: 0,
        plans: false,
        verifies: false,
        wants_witness: false,
        wants_verifier: false,
        mutating_actions: 0,
        diff_lines: 0,
        witness_authored: false,
        flip_achieved: false,
        tests_red: false,
        tests_green: false,
    }
}

#[test]
fn both_variants_round_trip_through_toml_and_json() {
    for text in [STAGED_V1, LEAN_V1] {
        let parsed = parse(text).unwrap();

        let via_toml = parse(&toml::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(parsed, via_toml, "TOML round-trip diverged");

        let json = serde_json::to_string(&parsed).unwrap();
        let via_json: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, via_json, "JSON round-trip diverged (invariant 4)");
    }
}

// --- The witness tests #3381's definition of done names. ------------------

#[test]
fn an_unknown_key_is_a_load_error() {
    let text = wrapper_manifest(
        "[[wrapper.stages]]\n\
         name = \"triage\"\n\
         unless = \"conversational\"\n",
    );
    assert!(
        matches!(parse(&text), Err(ManifestError::Parse(_))),
        "an unknown key must fail the load, never be ignored"
    );
}

#[test]
fn a_condition_naming_an_unpublished_signal_is_a_load_error() {
    let text = wrapper_manifest(
        "[[wrapper.stages]]\n\
         name = \"triage\"\n\
         if = \"moon-is-full\"\n",
    );
    match parse(&text) {
        Err(ManifestError::UnknownSignal { signal, stage, .. }) => {
            assert_eq!(signal, "moon-is-full");
            assert_eq!(stage, StageName::Triage);
        }
        other => panic!("expected UnknownSignal, got {other:?}"),
    }
}

#[test]
fn an_unpublished_signal_error_names_the_published_set() {
    let text = wrapper_manifest(
        "[[wrapper.stages]]\n\
         name = \"triage\"\n\
         if = \"moon-is-full\"\n",
    );
    let rendered = parse(&text).unwrap_err().to_string();
    for signal in Signal::ALL {
        assert!(
            rendered.contains(signal.as_str()),
            "the rejection must name every published signal so the fix needs \
             no doc lookup; \"{signal}\" was missing from: {rendered}"
        );
    }
}

#[test]
fn an_unknown_stage_name_is_a_load_error() {
    let text = wrapper_manifest(
        "[[wrapper.stages]]\n\
         name = \"reticulate-splines\"\n",
    );
    assert!(
        matches!(parse(&text), Err(ManifestError::Parse(_))),
        "a stage the host cannot dispatch is a manifest that quietly does nothing"
    );
}

// --- The §9.4 amendments: a closed grammar and a checked graph. -----------

#[test]
fn a_condition_outside_the_grammar_is_a_load_error() {
    // No expression language: this is the shape a manifest author reaches for
    // once they assume one exists.
    for condition in [
        "questions > 0 && plans",
        "questions>0",
        "not conversational",
    ] {
        let text = wrapper_manifest(&format!(
            "[[wrapper.stages]]\n\
             name = \"triage\"\n\
             if = \"{condition}\"\n"
        ));
        assert!(
            matches!(parse(&text), Err(ManifestError::UnparsableCondition { .. })),
            "\"{condition}\" must be rejected: the grammar is closed"
        );
    }
}

#[test]
fn a_count_signal_read_as_a_boolean_is_a_load_error() {
    let text = wrapper_manifest(
        "[[wrapper.stages]]\n\
         name = \"triage\"\n\
         if = \"questions\"\n",
    );
    match parse(&text) {
        Err(ManifestError::ConditionTypeMismatch {
            signal,
            declared,
            actual,
            ..
        }) => {
            assert_eq!(signal, Signal::Questions);
            assert_eq!(declared, SignalKind::Boolean);
            assert_eq!(actual, SignalKind::Count);
        }
        other => panic!("expected ConditionTypeMismatch, got {other:?}"),
    }
}

#[test]
fn a_boolean_signal_compared_against_a_number_is_a_load_error() {
    let text = wrapper_manifest(
        "[[wrapper.stages]]\n\
         name = \"triage\"\n\
         if = \"conversational > 0\"\n",
    );
    assert!(matches!(
        parse(&text),
        Err(ManifestError::ConditionTypeMismatch { .. })
    ));
}

#[test]
fn a_stage_reading_a_signal_no_earlier_stage_publishes_is_a_load_error() {
    // `questions` is triage's output, and triage is declared *after* the
    // stage that reads it. Without the graph check this manifest loads and
    // wedges at run time.
    let text = wrapper_manifest(
        "[[wrapper.stages]]\n\
         name = \"research\"\n\
         if = \"questions > 0\"\n\
         [[wrapper.stages]]\n\
         name = \"triage\"\n",
    );
    match parse(&text) {
        Err(ManifestError::SignalNotYetPublished {
            stage,
            signal,
            publisher,
        }) => {
            assert_eq!(stage, StageName::Research);
            assert_eq!(signal, Signal::Questions);
            assert_eq!(publisher, StageName::Triage);
        }
        other => panic!("expected SignalNotYetPublished, got {other:?}"),
    }
}

#[test]
fn a_host_fact_is_readable_by_the_very_first_stage() {
    // The other side of the graph check: `test-command` has no publisher
    // stage, so ordering cannot make it unavailable.
    let text = wrapper_manifest(
        "[[wrapper.stages]]\n\
         name = \"execute\"\n\
         if = \"no-test-command\"\n",
    );
    assert!(parse(&text).is_ok(), "a host fact needs no earlier stage");
}

// --- Well-formedness of the block itself. --------------------------------

#[test]
fn a_duplicate_stage_is_a_load_error() {
    let text = wrapper_manifest(
        "[[wrapper.stages]]\n\
         name = \"execute\"\n\
         [[wrapper.stages]]\n\
         name = \"execute\"\n",
    );
    assert!(matches!(
        parse(&text),
        Err(ManifestError::DuplicateWrapperStage {
            stage: StageName::Execute
        })
    ));
}

#[test]
fn an_empty_variant_id_is_a_load_error() {
    let text = "name = \"probe\"\n\
                [loop]\n\
                participation = \"steering\"\n\
                [wrapper]\n\
                id = \"  \"\n\
                [[wrapper.stages]]\n\
                name = \"execute\"\n";
    assert!(
        matches!(parse(text), Err(ManifestError::EmptyWrapperId)),
        "a blank variant id makes a measured run indistinguishable from an \
         unmeasured one"
    );
}

#[test]
fn a_wrapper_with_no_stages_is_a_load_error() {
    let text = "name = \"probe\"\n\
                [loop]\n\
                participation = \"steering\"\n\
                [wrapper]\n\
                id = \"probe-v1\"\n";
    assert!(matches!(
        parse(text),
        Err(ManifestError::EmptyWrapperStages)
    ));
}

#[test]
fn a_wrapper_below_steering_is_a_load_error() {
    let text = "name = \"probe\"\n\
                [loop]\n\
                participation = \"observer\"\n\
                [wrapper]\n\
                id = \"probe-v1\"\n\
                [[wrapper.stages]]\n\
                name = \"execute\"\n";
    assert!(
        matches!(
            parse(text),
            Err(ManifestError::WrapperRequiresSteering { .. })
        ),
        "an observer may only watch; intercepting the turn is steering"
    );
}

#[test]
fn a_manifest_with_no_wrapper_block_is_not_a_wrapper() {
    let text = "name = \"probe\"\n\
                [loop]\n\
                participation = \"steering\"\n";
    assert!(parse(text).unwrap().wrapper.is_none());
}
