//! #3381's acceptance for the manifest half: the stage order leaves Rust,
//! and every way of writing a manifest that would quietly do nothing is a
//! named load error instead.
//!
//! The two fixtures are the load-bearing claim. `wrapper-staged-v1.toml`
//! transcribes the order `crates/stella-pipeline/src/pipeline.rs` runs today;
//! `wrapper-lean-v1.toml` is a cheaper second shape. They differ in nothing
//! but their text, which is what makes the manifest a declaration the code
//! reads rather than a description of one hardcoded path.
//!
//! What these tests deliberately do **not** claim: that either variant *runs*.
//! Binding a stage name to the loop needs the four wrapper interception
//! points of #3380, which do not exist yet. Everything here is the load-time
//! contract, which is complete on its own.

use stella_plugin::{
    CompareOp, Condition, ManifestError, PluginManifest, Signal, SignalKind, StageName,
};

const STAGED_V1: &str = include_str!("fixtures/wrapper-staged-v1.toml");
const LEAN_V1: &str = include_str!("fixtures/wrapper-lean-v1.toml");

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
        "the order must match stage_rank in stella-pipeline's replay.rs"
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
