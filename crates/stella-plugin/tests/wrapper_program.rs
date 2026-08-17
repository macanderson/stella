//! #3408's acceptance for the reader half: a `[wrapper]` manifest resolves
//! into a stage order, and the two load-time graph rules are together
//! sufficient to make that resolution total.
//!
//! `wrapper_stages.rs` covers what a manifest may *say*. This file covers what
//! it *answers*: given the host's published signal values, which stages run and
//! in what order. The witness for the slice is
//! [`resolving_a_manifest_produces_a_stage_order`] — before
//! `Wrapper::resolve` existed there was no way to ask.
//!
//! The property is the load check's own claim, turned into a test: for every
//! manifest that loads and every possible set of signal values, resolution
//! succeeds, is deterministic, preserves declared order, and never yields a
//! stage whose condition reads a fact no earlier *running* stage produced.

use proptest::prelude::*;
use stella_plugin::{
    ManifestError, PluginManifest, Signal, SignalValues, StageName, StageProgram, Wrapper,
};

/// The canonical stage order, which every generated manifest is a
/// subsequence of — an out-of-order manifest is `wrapper_stages.rs`'s subject,
/// not this one.
const CANONICAL: [StageName; 8] = [
    StageName::Triage,
    StageName::Recall,
    StageName::Research,
    StageName::Plan,
    StageName::Scope,
    StageName::Execute,
    StageName::Witness,
    StageName::Verify,
];

/// The conditions a generated stage may carry. Index 0 is "unconditional";
/// the rest are every shape of the closed grammar over both a host fact and a
/// stage-published one, which is what lets the generator produce manifests on
/// both sides of each graph rule.
const CONDITIONS: [&str; 10] = [
    "",
    "conversational",
    "no-conversational",
    "plans",
    "no-plans",
    "verifies",
    "no-verifies",
    "questions > 0",
    "questions >= 2",
    "no-test-command",
];

fn manifest_text(choices: &[Option<usize>; 8]) -> String {
    let mut text = String::from(
        "name = \"generated\"\n\
         [loop]\n\
         participation = \"steering\"\n\
         [wrapper]\n\
         id = \"generated-v1\"\n",
    );
    for (stage, choice) in CANONICAL.iter().zip(choices) {
        let Some(index) = choice else { continue };
        text.push_str("[[wrapper.stages]]\n");
        text.push_str(&format!("name = \"{stage}\"\n"));
        let condition = CONDITIONS[*index % CONDITIONS.len()];
        if !condition.is_empty() {
            text.push_str(&format!("if = \"{condition}\"\n"));
        }
    }
    text
}

/// Every stage the program runs whose condition reads a stage-published
/// signal must have that signal's publisher **earlier in the program** — not
/// merely earlier in the manifest. This is the claim the two load rules exist
/// to make true, asserted through the evaluator rather than assumed from them.
fn graph_is_satisfied(wrapper: &Wrapper, program: &StageProgram) -> Result<(), String> {
    let mut produced: Vec<Signal> = Vec::new();
    for name in program.stages() {
        let stage = wrapper
            .stages
            .iter()
            .find(|s| s.name == *name)
            .ok_or_else(|| format!("{name} is not declared in the manifest it resolved from"))?;
        if let Some(condition) = stage.condition().map_err(|e| e.to_string())? {
            let signal = condition.signal();
            if let Some(publisher) = signal.publisher()
                && !produced.contains(&signal)
            {
                return Err(format!(
                    "{name} reads {signal}, which {publisher} publishes, but \
                     {publisher} did not run before it"
                ));
            }
        }
        produced.extend_from_slice(name.publishes());
    }
    Ok(())
}

/// The witness. Nothing before this change could turn a manifest into an
/// ordered list of the stages a turn runs: `Wrapper::resolve`, `SignalValues`
/// and `StageProgram` did not exist, so this file did not compile.
#[test]
fn resolving_a_manifest_produces_a_stage_order() {
    let manifest = PluginManifest::from_toml_str(
        "name = \"probe\"\n\
         [loop]\n\
         participation = \"steering\"\n\
         [wrapper]\n\
         id = \"probe-v1\"\n\
         [[wrapper.stages]]\n\
         name = \"triage\"\n\
         [[wrapper.stages]]\n\
         name = \"research\"\n\
         if = \"questions > 0\"\n\
         [[wrapper.stages]]\n\
         name = \"execute\"\n\
         [[wrapper.stages]]\n\
         name = \"verify\"\n\
         if = \"verifies\"\n",
    )
    .unwrap();
    let wrapper = manifest.wrapper.unwrap();

    let asked = wrapper
        .resolve(&SignalValues {
            test_command: false,
            conversational: false,
            questions: 3,
            plans: true,
            verifies: true,
        })
        .unwrap();
    assert_eq!(asked.variant(), "probe-v1");
    assert_eq!(
        asked.stages(),
        [
            StageName::Triage,
            StageName::Research,
            StageName::Execute,
            StageName::Verify,
        ]
    );

    let cheap = wrapper
        .resolve(&SignalValues {
            test_command: false,
            conversational: false,
            questions: 0,
            plans: false,
            verifies: false,
        })
        .unwrap();
    assert_eq!(cheap.stages(), [StageName::Triage, StageName::Execute]);
}

/// The load rule this reader made necessary: a publisher that might be
/// skipped leaves its signal with no value, and "declared earlier" cannot see
/// that. Rejected at load, with the conditional publisher named.
#[test]
fn a_conditional_publisher_read_by_a_later_stage_is_a_load_error() {
    let text = "name = \"probe\"\n\
                [loop]\n\
                participation = \"steering\"\n\
                [wrapper]\n\
                id = \"probe-v1\"\n\
                [[wrapper.stages]]\n\
                name = \"triage\"\n\
                if = \"no-test-command\"\n\
                [[wrapper.stages]]\n\
                name = \"research\"\n\
                if = \"questions > 0\"\n";
    match PluginManifest::from_toml_str(text) {
        Err(ManifestError::PublisherMayBeSkipped {
            stage,
            signal,
            publisher,
        }) => {
            assert_eq!(stage, StageName::Research);
            assert_eq!(signal, Signal::Questions);
            assert_eq!(publisher, StageName::Triage);
        }
        other => panic!("expected PublisherMayBeSkipped, got {other:?}"),
    }
}

/// The same manifest with nothing reading triage's output is fine: a
/// conditional stage is only a hazard for the stages that read it.
#[test]
fn a_conditional_publisher_nobody_reads_still_loads() {
    let text = "name = \"probe\"\n\
                [loop]\n\
                participation = \"steering\"\n\
                [wrapper]\n\
                id = \"probe-v1\"\n\
                [[wrapper.stages]]\n\
                name = \"triage\"\n\
                if = \"no-test-command\"\n\
                [[wrapper.stages]]\n\
                name = \"execute\"\n";
    assert!(PluginManifest::from_toml_str(text).is_ok());
}

fn signal_values() -> impl Strategy<Value = SignalValues> {
    (
        any::<bool>(),
        any::<bool>(),
        0u64..5,
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(test_command, conversational, questions, plans, verifies)| SignalValues {
                test_command,
                conversational,
                questions,
                plans,
                verifies,
            },
        )
}

proptest! {
    /// The load check's claim, as a property: a manifest that loads resolves
    /// for **every** set of signal values, deterministically, in declared
    /// order, and never reads a fact nothing produced. A manifest that does
    /// not load fails on one of the rules that make that true — never
    /// silently.
    #[test]
    fn a_loadable_manifest_resolves_totally_and_deterministically(
        choices in prop::array::uniform8(prop::option::of(0usize..CONDITIONS.len())),
        values in signal_values(),
    ) {
        let text = manifest_text(&choices);
        let wrapper = match PluginManifest::from_toml_str(&text) {
            Ok(manifest) => manifest.wrapper.expect("[wrapper] was written"),
            Err(err) => {
                prop_assert!(
                    matches!(
                        err,
                        ManifestError::EmptyWrapperStages
                            | ManifestError::SignalNotYetPublished { .. }
                            | ManifestError::PublisherMayBeSkipped { .. }
                    ),
                    "a generated manifest may only be rejected for having no \
                     stages or for an unsatisfiable graph, got {err:?} from:\n{text}"
                );
                return Ok(());
            }
        };

        let program = wrapper
            .resolve(&values)
            .map_err(|e| TestCaseError::fail(format!("a validated manifest must resolve: {e}")))?;
        let again = wrapper
            .resolve(&values)
            .map_err(|e| TestCaseError::fail(format!("resolution is not total: {e}")))?;
        prop_assert_eq!(&program, &again, "resolution must be deterministic");

        prop_assert_eq!(
            program.variant(),
            "generated-v1",
            "the program must carry the variant id it resolved from"
        );

        // Declared order is preserved: the program is a subsequence of the
        // manifest's stage list, never a reordering of it.
        let declared: Vec<StageName> = wrapper.stages.iter().map(|s| s.name).collect();
        let mut declared = declared.iter();
        for name in program.stages() {
            prop_assert!(
                declared.any(|d| d == name),
                "the program reordered the declared stages"
            );
        }

        graph_is_satisfied(&wrapper, &program).map_err(TestCaseError::fail)?;
    }
}
