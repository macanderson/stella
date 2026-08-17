//! #3408's acceptance for the pipeline half: the shipped `classic` manifest is
//! read by this crate, and it resolves to exactly the stage order the pipeline
//! runs today.
//!
//! This is the test that makes `variants/classic.toml` a declaration rather
//! than a description. Before `stella_plugin::Wrapper::resolve` existed nothing
//! could turn a manifest into a stage order at all, so every assertion below
//! was unwritable — that is the witness (`git stash && cargo test -p
//! stella-pipeline --test variant_program` does not compile).
//!
//! What it deliberately does **not** claim: that the resolved program *drives*
//! the run. `crates/stella-pipeline/src/pipeline.rs` still takes its own
//! branches; binding the two needs the four wrapper interception points
//! (#3380). Until then this file is what keeps the manifest and the branches
//! from parting.

use stella_pipeline::replay::stage_transition_legal;
use stella_pipeline::triage::{TaskAssessment, TaskClass};
use stella_pipeline::variant::{self, CLASSIC_VARIANT_ID};
use stella_plugin::{SignalValues, StageName};
use stella_protocol::StageKind;

/// The stage a wrapper name corresponds to in the emitted event stream —
/// which is how the shipped order is checked against `replay.rs`'s canonical
/// ranking rather than against a second hand-written list.
fn stage_kind(name: StageName) -> StageKind {
    match name {
        StageName::Triage => StageKind::Triage,
        StageName::Recall => StageKind::ContextRecall,
        StageName::Research => StageKind::Research,
        StageName::Plan => StageKind::Plan,
        StageName::Scope => StageKind::ScopeReview,
        StageName::Execute => StageKind::Execute,
        StageName::Witness => StageKind::Witness,
        StageName::Verify => StageKind::Verify,
        StageName::Verdict => StageKind::Verdict,
        StageName::Reflect => StageKind::Reflect,
        StageName::ContextWrite => StageKind::ContextWrite,
        StageName::Complete => StageKind::Complete,
    }
}

/// A multi-step task with no configured test command and questions to answer —
/// the turn that pays for every stage the pipeline has.
fn full_ceremony() -> SignalValues {
    variant::signals(
        &TaskAssessment::from_class(TaskClass::MultiStep),
        2,
        None, // no --test-command: the run authors its own witness
        1,    // single-shot
        true, // spend is metered
    )
}

#[test]
fn the_shipped_manifest_loads_and_carries_the_built_in_variant_id() {
    let wrapper = variant::classic().expect("the shipped classic manifest must load");
    assert_eq!(
        wrapper.id, CLASSIC_VARIANT_ID,
        "the id the store's pipeline_variant column records"
    );
}

/// The claim the file exists to make: for the fullest turn, the manifest
/// resolves to today's hardcoded order, stage for stage.
#[test]
fn classic_matches_the_shipped_stage_order() {
    let program = variant::classic_program(&full_ceremony()).unwrap();
    assert_eq!(program.variant(), CLASSIC_VARIANT_ID);
    assert_eq!(
        program.stages(),
        [
            StageName::Triage,
            StageName::Recall,
            StageName::Research,
            StageName::Plan,
            StageName::Scope,
            StageName::Execute,
            StageName::Witness,
            StageName::Verify,
        ],
        "the manifest must reproduce the order pipeline.rs runs today"
    );
}

/// The order is checked against `replay.rs`'s canonical ranking rather than
/// against the list above alone: a resolved program must be a legal walk of the
/// stage graph the event stream is validated by, or a wrapper could emit a
/// stream `validate_stream` rejects.
#[test]
fn every_resolved_program_is_a_legal_stage_walk() {
    for values in representative_turns() {
        let program = variant::classic_program(&values).unwrap();
        let kinds: Vec<StageKind> = program.stages().iter().copied().map(stage_kind).collect();
        for pair in kinds.windows(2) {
            assert!(
                stage_transition_legal(pair[0], pair[1]),
                "{:?} -> {:?} is not a legal stage transition, from {values:?}",
                pair[0],
                pair[1]
            );
        }
    }
}

/// Every condition in the manifest transcribes a branch in `pipeline.rs`, so
/// each one is exercised from both sides here: the fact that turns it on and
/// the fact that turns it off.
fn representative_turns() -> Vec<SignalValues> {
    vec![
        full_ceremony(),
        // A configured --test-command: the run has an oracle, so nothing is
        // authored (`Pipeline::config.test_command`, the witness branch).
        variant::signals(
            &TaskAssessment::from_class(TaskClass::MultiStep),
            2,
            Some("cargo test"),
            1,
            true,
        ),
        // One concrete change: no DAG planning, so no scope review either.
        variant::signals(
            &TaskAssessment::from_class(TaskClass::SingleTask),
            0,
            None,
            1,
            true,
        ),
        // A lookup: the cheapest class, which does not verify unconditionally.
        variant::signals(
            &TaskAssessment::from_class(TaskClass::SimpleLookup),
            0,
            None,
            1,
            true,
        ),
    ]
}

#[test]
fn a_configured_test_command_skips_witness_authoring() {
    let program = variant::classic_program(&variant::signals(
        &TaskAssessment::from_class(TaskClass::MultiStep),
        2,
        Some("cargo test"),
        1,
        true,
    ))
    .unwrap();
    assert!(
        !program.runs(StageName::Witness),
        "an armed oracle means the run does not author its own measuring stick"
    );
    assert!(
        program.runs(StageName::Verify),
        "the ladder still runs — it just has a command to run"
    );
}

#[test]
fn a_single_task_skips_planning_and_scope_review() {
    let program = variant::classic_program(&variant::signals(
        &TaskAssessment::from_class(TaskClass::SingleTask),
        0,
        None,
        1,
        true,
    ))
    .unwrap();
    assert_eq!(
        program.stages(),
        [
            StageName::Triage,
            StageName::Recall,
            StageName::Execute,
            StageName::Witness,
            StageName::Verify,
        ],
        "one concrete change: no research, no plan, no scope review"
    );
}

#[test]
fn a_simple_lookup_does_not_verify_unconditionally() {
    let program = variant::classic_program(&variant::signals(
        &TaskAssessment::from_class(TaskClass::SimpleLookup),
        0,
        None,
        1,
        true,
    ))
    .unwrap();
    assert!(
        !program.runs(StageName::Verify),
        "TaskClass::SimpleLookup skips the ladder (subject to the zero-diff \
         guard, which the pipeline applies separately)"
    );
    assert!(!program.runs(StageName::Plan));
}

/// `signals` is the host's half of the contract, so what it reads from is
/// pinned: a `plans`/`verifies` sourced from anywhere but the task class, or a
/// `test_command` that ignores the configured command, would make every
/// condition above quietly wrong.
#[test]
fn signals_come_from_the_facts_they_name() {
    let multi = variant::signals(
        &TaskAssessment::from_class(TaskClass::MultiStep),
        3,
        None,
        1,
        true,
    );
    assert_eq!(
        multi,
        SignalValues {
            test_command: false,
            candidates: 1,
            budget_metered: true,
            conversational: false,
            questions: 3,
            plans: true,
            verifies: true,
            wants_witness: true,
            wants_verifier: true,
            ..unobserved()
        }
    );

    let lookup = variant::signals(
        &TaskAssessment {
            conversational: true,
            ..TaskAssessment::from_class(TaskClass::SimpleLookup)
        },
        0,
        Some("just test"),
        4,
        false,
    );
    assert_eq!(
        lookup,
        SignalValues {
            test_command: true,
            candidates: 4,
            budget_metered: false,
            conversational: true,
            questions: 0,
            plans: false,
            verifies: false,
            wants_witness: false,
            wants_verifier: true,
            ..unobserved()
        }
    );
}

/// The post-triage half of a [`SignalValues`] as `variant::signals` leaves it:
/// nothing executed, authored or observed yet. Spelled once here so the two
/// assertions above are about the fields they name.
fn unobserved() -> SignalValues {
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

/// The guard behind `variant::signals`'s stated contract: this host takes one
/// snapshot at the triage boundary, so a shipped variant may only read facts
/// that exist by then. A condition on a signal execute, witness or verify
/// publishes would be answered from the snapshot instead of from the stage —
/// silently, and in the direction of "nothing happened".
///
/// Not a limit on the manifest grammar, which is right to have those signals:
/// it is a limit on *this* host until resolution becomes progressive (#3491),
/// and it is checked here so the day a variant reaches for one, this fails
/// instead of the run quietly skipping a stage.
#[test]
fn the_shipped_variant_reads_only_facts_the_triage_boundary_has() {
    let wrapper = variant::classic().expect("the shipped classic manifest must load");
    for stage in &wrapper.stages {
        let Some(condition) = stage.condition().expect("a shipped condition must parse") else {
            continue;
        };
        let signal = condition.signal();
        let publisher = signal.publisher();
        assert!(
            matches!(publisher, None | Some(StageName::Triage)),
            "{} reads {signal}, which {} publishes — this host cannot answer it \
             at the triage boundary (#3491)",
            stage.name,
            publisher.map_or_else(|| "the host".to_string(), |p| p.to_string()),
        );
    }
}
