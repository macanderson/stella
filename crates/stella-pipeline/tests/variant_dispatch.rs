//! #3408's witness that the pipeline's stage schedule is read from a
//! manifest rather than decided by a Rust branch that merely agrees with one.
//!
//! `crates/stella-pipeline/src/pipeline.rs` consults
//! [`stella_pipeline::schedule::Schedule`] at exactly the points this file
//! drives directly: a batch of pre-execute decisions taken right after
//! triage (`Pipeline::decide_pre_execute_schedule`), and a per-candidate
//! `verify` decision taken after `execute`'s real output is known
//! (`Pipeline::run_candidate`). Both are `pub(super)` to `stella-pipeline`
//! and cannot be called from here, so this file exercises the same
//! [`Schedule`] sequence those call sites make — the mechanism `pipeline.rs`
//! is built on — rather than re-deriving it against a live, fully-mocked
//! `Pipeline::run`. `crates/stella-pipeline/src/pipeline/tests.rs`'s existing
//! end-to-end stage-sequence assertions (`clean_lookup_skips_plan_verify_and_
//! verifier`, `single_task_with_a_flip_submits_fast_and_skips_the_verifier`,
//! …) are the regression net that would go red if this wiring diverged from
//! what `pipeline.rs` actually consults — see this crate's `variant_program`
//! and `pipeline::tests` suites, unchanged by this slice.
//!
//! A condition naming an unknown key or an unpublished signal is already
//! covered where the parser lives:
//! `crates/stella-plugin/tests/wrapper_stages.rs::an_unknown_key_is_a_load_error`
//! and `::a_condition_naming_an_unpublished_signal_is_a_load_error`. Nothing
//! here duplicates them — a [`stella_plugin::Wrapper`] only reaches
//! [`PipelineConfig::variant`](stella_pipeline::PipelineConfig) after already
//! passing through that loader, so there is no second load-time check at the
//! pipeline layer to witness.

use stella_pipeline::schedule::{HostFacts, Schedule};
use stella_plugin::{PluginManifest, StageName, Wrapper};

fn load(toml: &str) -> Wrapper {
    PluginManifest::from_toml_str(toml)
        .expect("test manifest must load")
        .wrapper
        .expect("[wrapper] declared")
}

/// `classic.toml`'s own shape, transcribed as a test manifest so this file
/// does not depend on the shipped file's exact text.
const STAGED: &str = "name = \"staged\"\n\
     [loop]\n\
     participation = \"steering\"\n\
     [wrapper]\n\
     id = \"staged-v1\"\n\
     [[wrapper.stages]]\n\
     name = \"triage\"\n\
     [[wrapper.stages]]\n\
     name = \"recall\"\n\
     [[wrapper.stages]]\n\
     name = \"research\"\n\
     if = \"questions > 0\"\n\
     [[wrapper.stages]]\n\
     name = \"plan\"\n\
     if = \"plans\"\n\
     [[wrapper.stages]]\n\
     name = \"scope\"\n\
     if = \"plans\"\n\
     [[wrapper.stages]]\n\
     name = \"execute\"\n\
     [[wrapper.stages]]\n\
     name = \"witness\"\n\
     if = \"no-test-command\"\n\
     [[wrapper.stages]]\n\
     name = \"verify\"\n\
     if = \"verifies\"\n";

/// A deliberately cheaper variant (#3381's shape): no witness at all, and
/// `verify` reads `execute`'s real output instead of the task class — the
/// signal that does not exist until the candidate has actually run.
const LEAN_DIFF_GATED: &str = "name = \"lean\"\n\
     [loop]\n\
     participation = \"steering\"\n\
     [wrapper]\n\
     id = \"lean-diff-v1\"\n\
     [[wrapper.stages]]\n\
     name = \"triage\"\n\
     [[wrapper.stages]]\n\
     name = \"recall\"\n\
     [[wrapper.stages]]\n\
     name = \"execute\"\n\
     [[wrapper.stages]]\n\
     name = \"verify\"\n\
     if = \"diff-lines > 0\"\n";

fn host() -> HostFacts {
    HostFacts {
        test_command: false,
        candidates: 1,
        budget_metered: true,
    }
}

/// The pre-execute batch `Pipeline::decide_pre_execute_schedule` performs,
/// reproduced here against a manifest handed in from outside — the same
/// sequence, driven the same way, so a divergence in either would show up as
/// a failure here without requiring a live `Pipeline::run`.
struct Decided {
    research: bool,
    plan: bool,
    witness: bool,
}

fn decide_pre_execute(schedule: &mut Schedule<'_>) -> Decided {
    schedule.decide(StageName::Triage).unwrap();
    schedule.decide(StageName::Recall).unwrap();
    let research = schedule.decide(StageName::Research).unwrap();
    let plan = schedule.decide(StageName::Plan).unwrap();
    let scope = schedule.decide(StageName::Scope).unwrap();
    schedule.decide(StageName::Execute).unwrap();
    let witness = schedule.decide(StageName::Witness).unwrap();
    Decided {
        research,
        plan: plan && scope,
        witness,
    }
}

/// 6a: the same goal's facts, scheduled under two different variants, decide
/// `witness` differently — and each schedule still carries its OWN manifest's
/// id, the fact `executions.pipeline_variant` records (#3388).
#[test]
fn two_variants_decide_witness_differently_for_the_same_turn() {
    let staged = load(STAGED);
    let lean = load(LEAN_DIFF_GATED);

    let mut under_staged = Schedule::new(&staged, host());
    under_staged.update(|v| {
        v.questions = 2;
        v.plans = true;
        v.verifies = true;
        v.wants_witness = true;
    });
    let staged_decision = decide_pre_execute(&mut under_staged);
    assert!(staged_decision.research, "triage named questions");
    assert!(staged_decision.plan, "the class plans");
    assert!(
        staged_decision.witness,
        "no --test-command is configured, so `staged` authors its own witness"
    );
    assert_eq!(under_staged.variant_id(), "staged-v1");

    let mut under_lean = Schedule::new(&lean, host());
    // `lean` never declares `research`/`plan`/`scope`/`witness` at all — a
    // variant is entitled to omit a stage outright (`Schedule::decide`'s own
    // contract), so the SAME triage facts that turned every one of those on
    // above answer `false` here without `lean`'s manifest text mentioning
    // any of them.
    under_lean.update(|v| {
        v.questions = 2;
        v.plans = true;
        v.verifies = true;
        v.wants_witness = true;
    });
    assert!(under_lean.decide(StageName::Triage).unwrap());
    assert!(under_lean.decide(StageName::Recall).unwrap());
    assert!(!under_lean.decide(StageName::Research).unwrap());
    assert!(!under_lean.decide(StageName::Plan).unwrap());
    assert!(!under_lean.decide(StageName::Scope).unwrap());
    assert!(under_lean.decide(StageName::Execute).unwrap());
    assert!(
        !under_lean.decide(StageName::Witness).unwrap(),
        "`lean` never declares a witness stage at all"
    );
    assert_eq!(under_lean.variant_id(), "lean-diff-v1");

    // The two variants really did diverge on the one question this test is
    // about, not merely on their ids.
    assert!(staged_decision.witness, "staged authors a witness");
}

/// 6a continued, and 6c: `verify`'s decision under `lean-diff-v1` flips with
/// the real `diff-lines` value alone — no branch anywhere decided this, the
/// schedule read it straight off the manifest's `if = "diff-lines > 0"`.
/// Two clones of the same post-execute schedule, one per "candidate", answer
/// independently — the shape best-of-N needs.
#[test]
fn verify_under_the_diff_gated_variant_follows_the_real_diff_not_the_task_class() {
    let lean = load(LEAN_DIFF_GATED);
    let mut prefix = Schedule::new(&lean, host());
    assert!(prefix.decide(StageName::Triage).unwrap());
    assert!(prefix.decide(StageName::Recall).unwrap());
    assert!(prefix.decide(StageName::Execute).unwrap());

    let mut touched_files = prefix.clone();
    touched_files.update(|v| v.diff_lines = 12);
    assert!(
        touched_files.decide(StageName::Verify).unwrap(),
        "a candidate that produced a real diff verifies under `lean-diff-v1`"
    );

    let mut touched_nothing = prefix.clone();
    touched_nothing.update(|v| v.diff_lines = 0);
    assert!(
        !touched_nothing.decide(StageName::Verify).unwrap(),
        "a candidate with an empty diff does not — the SAME manifest, the \
         same code path, a different real fact"
    );
}

/// 6c, stated directly: flipping one condition in an otherwise-identical
/// manifest flips the schedule's answer, with the calling code (this
/// function) completely unchanged between the two — the grammar decides,
/// nothing here does.
#[test]
fn flipping_a_manifest_condition_flips_the_decision_with_no_code_change() {
    fn scheduled_verify(condition: &str, diff_lines: u64) -> bool {
        let toml = format!(
            "name = \"probe\"\n\
             [loop]\n\
             participation = \"steering\"\n\
             [wrapper]\n\
             id = \"probe-v1\"\n\
             [[wrapper.stages]]\n\
             name = \"execute\"\n\
             [[wrapper.stages]]\n\
             name = \"verify\"\n\
             if = \"{condition}\"\n"
        );
        let wrapper = load(&toml);
        let mut schedule = Schedule::new(&wrapper, host());
        schedule.decide(StageName::Execute).unwrap();
        schedule.update(|v| v.diff_lines = diff_lines);
        schedule.decide(StageName::Verify).unwrap()
    }

    // One manifest byte changed (`>` to `==`), same 12-line diff, same
    // function: the answer flips because the grammar says something
    // different, not because any Rust changed.
    assert!(scheduled_verify("diff-lines > 0", 12));
    assert!(!scheduled_verify("diff-lines == 0", 12));
}
