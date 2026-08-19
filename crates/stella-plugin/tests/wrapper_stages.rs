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
//! Everything here is the load-time contract, which is complete on its own —
//! the four wrapper interception points that bind a stage name to a turn
//! landed in #3380/#3479 and are driven from
//! `stella_runtime::wrapper::WrapperDispatch`, whose own tests
//! (`crates/stella-runtime/tests/wrapper_dispatch.rs`) are where "it runs" is
//! established.

use stella_plugin::{
    CompareOp, Condition, HostStage, MAX_CONTRIBUTED_STAGE_LEN, ManifestError, PluginManifest,
    Signal, SignalKind, SignalValues, StageName, Wrapper,
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
    let names: Vec<StageName> = wrapper.stages.iter().map(|s| s.name.clone()).collect();
    assert_eq!(
        names,
        vec![
            StageName::Host(HostStage::Triage),
            StageName::Host(HostStage::Recall),
            StageName::Host(HostStage::Research),
            StageName::Host(HostStage::Plan),
            StageName::Host(HostStage::Scope),
            StageName::Host(HostStage::Execute),
            StageName::Host(HostStage::Witness),
            StageName::Host(HostStage::Verify),
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
        condition(StageName::Host(HostStage::Research)),
        Some(Condition::Compare {
            signal: Signal::Questions,
            op: CompareOp::Greater,
            value: 0,
        }),
        "\"empty questions skip the stage byte-for-byte\""
    );
    assert_eq!(
        condition(StageName::Host(HostStage::Witness)),
        Some(Condition::Boolean {
            signal: Signal::TestCommand,
            negated: true,
        }),
        "witness authoring is gated on there being no configured test command"
    );
    assert_eq!(
        condition(StageName::Host(HostStage::Recall)),
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
            StageKind::Triage => StageName::Host(HostStage::Triage),
            StageKind::ContextRecall => StageName::Host(HostStage::Recall),
            StageKind::Research => StageName::Host(HostStage::Research),
            StageKind::Plan => StageName::Host(HostStage::Plan),
            StageKind::ScopeReview => StageName::Host(HostStage::Scope),
            StageKind::Execute => StageName::Host(HostStage::Execute),
            StageKind::Witness => StageName::Host(HostStage::Witness),
            StageKind::Verify => StageName::Host(HostStage::Verify),
            StageKind::Verdict => StageName::Host(HostStage::Verdict),
            StageKind::Reflect => StageName::Host(HostStage::Reflect),
            StageKind::ContextWrite => StageName::Host(HostStage::ContextWrite),
            StageKind::Complete => StageName::Host(HostStage::Complete),
        };
        assert_eq!(
            name.kind(),
            Some(kind),
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
            StageName::Host(HostStage::Triage),
            StageName::Host(HostStage::Execute),
            StageName::Host(HostStage::Witness),
            StageName::Host(HostStage::Verify),
            StageName::Host(HostStage::Reflect),
            StageName::Host(HostStage::ContextWrite),
            StageName::Host(HostStage::Complete),
        ]
    );

    // The same manifest over a turn that produced nothing: no witness worth
    // authoring, nothing to reflect on, nothing corroborated to write back —
    // but still graded, because "it changed nothing" is a finding.
    let idle = wrapper.resolve(&bare()).expect("resolution is total");
    assert_eq!(
        idle.stages(),
        [
            StageName::Host(HostStage::Triage),
            StageName::Host(HostStage::Execute),
            StageName::Host(HostStage::Verify),
            StageName::Host(HostStage::Complete),
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
            StageName::Host(HostStage::Triage),
            "diff-lines > 0",
            Signal::DiffLines,
            HostStage::Execute,
        ),
        (
            StageName::Host(HostStage::Execute),
            "witness-authored",
            Signal::WitnessAuthored,
            HostStage::Witness,
        ),
        (
            StageName::Host(HostStage::Witness),
            "flip-achieved",
            Signal::FlipAchieved,
            HostStage::Verify,
        ),
        (
            StageName::Host(HostStage::Execute),
            "tests-red",
            Signal::TestsRed,
            HostStage::Verify,
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
            assert_eq!(stage, StageName::Host(HostStage::Triage));
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

/// The rule this replaces, and why replacing it is not a weakening.
///
/// Until #3963 a name outside the host's twelve was a `ManifestError::Parse` —
/// serde refusing an unknown enum variant — on the argument that a stage the
/// host cannot dispatch is a manifest that quietly does nothing. The argument
/// was right and its conclusion no longer follows: the host *can* dispatch this
/// stage, because dispatch iterates the resolved program rather than matching
/// on a closed set. So the name loads and the stage runs, and the thing that
/// used to be a serde rejection is now a contributed stage.
///
/// What the old rule genuinely bought is kept by the tests at the end of this
/// file: a name that cannot be rendered, or that would read as one of the
/// host's own boundaries, is still refused at load.
#[test]
fn an_unknown_stage_name_is_now_a_contributed_stage() {
    let text = wrapper_manifest(
        "[[wrapper.stages]]\n\
         name = \"reticulate-splines\"\n",
    );
    let wrapper = parse(&text)
        .expect("an unknown name is a contributed stage, not a rejection")
        .wrapper
        .expect("[wrapper]");
    assert_eq!(
        wrapper.stages[0].name,
        StageName::Contributed("reticulate-splines".to_string())
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
            assert_eq!(stage, StageName::Host(HostStage::Research));
            assert_eq!(signal, Signal::Questions);
            assert_eq!(publisher, HostStage::Triage);
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
            stage: StageName::Host(HostStage::Execute)
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

// --- The vocabulary is open: a stage may come from the manifest. ----------
//
// #3963. Everything above this line was written when the twelve were the whole
// vocabulary and an unknown name was a load error. That rule bought one thing
// — no manifest declares a stage nothing will dispatch — and the tests below
// are the argument that opening the vocabulary keeps it: a contributed stage
// dispatches *because* it was declared, and the names that could not be
// dispatched, rendered, or told apart from a host boundary are still refused.

/// **The witness.** A stage's existence and its name come from the manifest.
///
/// Fails before #3963 for the reason the issue names: `name = "triage-lite"`
/// did not load at all, so there was nothing to resolve and nothing to
/// dispatch.
#[test]
fn a_contributed_stage_loads_and_resolves_under_its_own_word() {
    let text = wrapper_manifest(
        "[[wrapper.stages]]\n\
         name = \"triage-lite\"\n\
         [[wrapper.stages]]\n\
         name = \"execute\"\n",
    );
    let manifest = parse(&text).expect("a contributed stage must load");
    let wrapper = manifest.wrapper.expect("[wrapper] must parse");

    let contributed = &wrapper.stages[0].name;
    assert_eq!(contributed.as_str(), "triage-lite");
    assert!(contributed.is_contributed());
    assert_eq!(
        contributed.host(),
        None,
        "the host has no boundary of its own for it"
    );
    assert_eq!(
        contributed.kind(),
        None,
        "and no StageKind either — the wire carries the plugin's word instead (#3964)"
    );

    // It resolves like any other stage, which is what makes it dispatchable:
    // `stella_runtime`'s dispatcher asks `before_turn` once per stage in here.
    let program = wrapper
        .resolve(&bare())
        .expect("a validated wrapper resolves for every signal set");
    assert_eq!(
        program.stages(),
        [
            StageName::Contributed("triage-lite".to_string()),
            StageName::Host(HostStage::Execute),
        ]
    );
    assert!(program.runs(&StageName::new("triage-lite")));
}

/// Removing the stage from the manifest removes it from the turn — the other
/// half of "its existence comes from the manifest", and the reason uninstalling
/// the plugin takes its stage with it.
#[test]
fn a_turn_has_a_contributed_stage_only_while_the_manifest_declares_it() {
    let without = parse(&wrapper_manifest(
        "[[wrapper.stages]]\n\
         name = \"execute\"\n",
    ))
    .expect("loads")
    .wrapper
    .expect("[wrapper]")
    .resolve(&bare())
    .expect("resolves");
    assert_eq!(without.stages(), [StageName::Host(HostStage::Execute)]);
    assert!(!without.runs(&StageName::new("triage-lite")));
}

/// A contributed stage reads the host's facts and the outputs of host stages
/// declared before it. Consuming is the whole half of the signal contract it
/// gets, and it is a real one.
#[test]
fn a_contributed_stage_may_be_conditional_on_what_the_host_published() {
    let text = wrapper_manifest(
        "[[wrapper.stages]]\n\
         name = \"triage\"\n\
         [[wrapper.stages]]\n\
         name = \"triage-lite\"\n\
         if = \"questions > 0\"\n\
         [[wrapper.stages]]\n\
         name = \"execute\"\n",
    );
    let wrapper = parse(&text)
        .expect("a condition over a published signal loads")
        .wrapper
        .expect("[wrapper]");

    let curious = SignalValues {
        questions: 2,
        ..bare()
    };
    assert!(
        wrapper
            .resolve(&curious)
            .expect("resolves")
            .runs(&StageName::new("triage-lite")),
        "triage published two questions, so the contributed stage runs"
    );
    assert!(
        !wrapper
            .resolve(&bare())
            .expect("resolves")
            .runs(&StageName::new("triage-lite")),
        "and it is skipped on the turn that has none"
    );
}

/// The other side of that contract: a contributed stage **publishes** nothing,
/// so nothing downstream can be made to depend on one. A condition naming a
/// fact a plugin might imagine its stage produces is the same load error it has
/// always been — the signal vocabulary did not open with the stage vocabulary.
#[test]
fn a_contributed_stage_publishes_no_signal() {
    assert_eq!(
        StageName::new("triage-lite").publishes(),
        [],
        "a signal is a fact the host produces"
    );
    let text = wrapper_manifest(
        "[[wrapper.stages]]\n\
         name = \"triage-lite\"\n\
         [[wrapper.stages]]\n\
         name = \"execute\"\n\
         if = \"triaged-lightly\"\n",
    );
    match parse(&text) {
        Err(ManifestError::UnknownSignal { signal, .. }) => {
            assert_eq!(signal, "triaged-lightly");
        }
        other => panic!("expected UnknownSignal, got {other:?}"),
    }
}

/// A contributed name that the *wire* vocabulary answers to would stop being
/// contributed the moment it crossed the socket:
/// `stella_protocol::StageName::new` resolves it back into a host boundary, so
/// every renderer would show this plugin's stage as one of Stella's own.
#[test]
fn a_contributed_stage_may_not_shadow_a_host_boundary() {
    for (written, spell_it) in [
        ("context_recall", "recall"),
        ("scope_review", "scope"),
        ("context_write", "contextwrite"),
        // The historical spellings of `verdict`, which `stella-protocol`
        // still resolves so recorded streams keep replaying.
        ("judge", "verdict"),
        ("verifier", "verdict"),
    ] {
        let text = wrapper_manifest(&format!(
            "[[wrapper.stages]]\n\
             name = \"{written}\"\n"
        ));
        match parse(&text) {
            Err(ManifestError::ContributedStageShadowsBoundary { stage, spelled }) => {
                assert_eq!(stage, written);
                assert_eq!(
                    spelled, spell_it,
                    "the rejection names the manifest spelling to write instead"
                );
            }
            other => {
                panic!("expected ContributedStageShadowsBoundary for {written}, got {other:?}")
            }
        }
    }
}

/// The manifest's own spellings are not shadowing — they *are* the host
/// stages, resolved rather than contributed. This is the normalization that
/// keeps one word one stage.
#[test]
fn a_host_stage_written_in_the_manifest_spelling_is_never_contributed() {
    for stage in HostStage::ALL {
        let name = StageName::new(stage.as_str());
        assert_eq!(name, StageName::Host(stage));
        assert!(
            !name.is_contributed(),
            "{stage} is the host's, however it arrived"
        );
    }
}

/// A name no surface can print is refused, which is the producer-side half of
/// `stella_protocol::StageName` treating a contributed name as opaque: strict
/// where the manifest is written, tolerant where someone else's stream is read.
#[test]
fn a_contributed_stage_name_the_deck_cannot_render_is_a_load_error() {
    for (written, because) in [
        ("Triage-Lite", "uppercase"),
        ("triage lite", "a space"),
        ("triage_lite", "an underscore"),
        ("2fast", "a leading digit"),
        ("-lite", "a leading dash"),
        ("triáge", "a non-ASCII letter"),
    ] {
        let text = wrapper_manifest(&format!(
            "[[wrapper.stages]]\n\
             name = \"{written}\"\n"
        ));
        let got = parse(&text);
        assert!(
            matches!(
                got,
                Err(ManifestError::MalformedContributedStage { ref stage, .. }) if stage == written
            ),
            "{written} must be refused for {because}, got {got:?}"
        );
    }

    let too_long = "a".repeat(MAX_CONTRIBUTED_STAGE_LEN + 1);
    let text = wrapper_manifest(&format!(
        "[[wrapper.stages]]\n\
         name = \"{too_long}\"\n"
    ));
    assert!(matches!(
        parse(&text),
        Err(ManifestError::MalformedContributedStage { .. })
    ));

    let blank = wrapper_manifest(
        "[[wrapper.stages]]\n\
         name = \"   \"\n",
    );
    assert!(
        matches!(parse(&blank), Err(ManifestError::EmptyWrapperStageName)),
        "a stage whose name is whitespace dispatches under a word nothing can print"
    );
}

/// Duplicate detection did not weaken when the vocabulary opened: two
/// contributed stages of the same name still make both their position and
/// their condition ambiguous.
#[test]
fn a_duplicate_contributed_stage_is_still_a_load_error() {
    let text = wrapper_manifest(
        "[[wrapper.stages]]\n\
         name = \"triage-lite\"\n\
         [[wrapper.stages]]\n\
         name = \"triage-lite\"\n",
    );
    match parse(&text) {
        Err(ManifestError::DuplicateWrapperStage { stage }) => {
            assert_eq!(stage.as_str(), "triage-lite");
        }
        other => panic!("expected DuplicateWrapperStage, got {other:?}"),
    }
}

/// Invariant 4 for the newly-open field: the manifest a host re-serializes is
/// the manifest it read, contributed name included.
#[test]
fn a_contributed_stage_round_trips_through_serde() {
    let wrapper = parse(&wrapper_manifest(
        "[[wrapper.stages]]\n\
         name = \"triage-lite\"\n\
         [[wrapper.stages]]\n\
         name = \"execute\"\n",
    ))
    .expect("loads")
    .wrapper
    .expect("[wrapper]");

    let json = serde_json::to_string(&wrapper).expect("serializes");
    assert!(
        json.contains("\"name\":\"triage-lite\""),
        "a contributed stage is a plain string on the wire, like every host \
         stage beside it: {json}"
    );
    assert!(
        json.contains("\"name\":\"execute\""),
        "and a host stage sends exactly the byte it always did: {json}"
    );
    let read_back: Wrapper = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(read_back, wrapper);
}
