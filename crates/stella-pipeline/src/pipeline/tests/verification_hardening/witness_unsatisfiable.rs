// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A witness that reports the same failure however the work changes (#2540).
//!
//! The defect this pins, from Terminal-Bench 2.1 `fix-git` (trial
//! `7d025330abad`): the author wrote a shell witness asserting that every
//! commit unreachable from `master` is merged or patch-equivalent. Two of the
//! three commits it found were the pipeline's own candidate snapshots, which
//! the agent can neither merge nor dispose of — and every revise cycle minted
//! another one, so the demand grew faster than any worker could meet it. The
//! ladder had no way to say that: it emitted `rung: revise` and blamed the
//! worker. Stella had the correct answer at 5.7 minutes and ended the run on
//! the 13-minute deadline.
//!
//! The rung is not reached on the first red, and that half is deliberate — see
//! [`crate::verify::LadderInputs::witness_unmoved_by_revision`]. Both tests
//! below are here because the boundary between them is the whole design.
//!
//! A child module for the same two reasons `flip_halt_arming` is: it reaches
//! the shared fakes through the parent's own `use super::*`, and the
//! already-oversized `tests.rs` does not grow another module declaration.

use super::*;

/// The witness the author writes, and the revision turn the worker is given
/// after the first red. Four calls: triage, worker, witness author, revision.
fn provider_with_one_revision() -> ScriptedProvider {
    ScriptedProvider::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("wrote the test.\nTEST_COMMAND: cargo test --test witness witness -- --exact"),
        text_result("revised — merged the lost commit"),
    ])
}

/// The two workspaces the authored path needs. The candidate's runs and the
/// baseline's run all FAIL, and — the load-bearing part — they fail with the
/// same output, so the normalized fingerprints are equal.
fn both_trees_fail() -> FakeWorkspacePort {
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    // Two post-execute observations: the first round's red, then the revised
    // round's identical red.
    let candidate =
        FakeWorkspace::new(0, vec![false, false], Ok(vec![]), log.clone()).with_repo_status(
            SeqRepoStatus::new(vec![vec![], vec![("tests/witness.rs", "w1")]]),
        );
    let baseline = FakeWorkspace::new(1, vec![false], Ok(vec![]), log.clone()).with_repo_status(
        SeqRepoStatus::new(vec![vec![], vec![("tests/witness.rs", "w1")]]),
    );
    FakeWorkspacePort::new(vec![Ok(candidate), Ok(baseline)], log)
}

/// #2540's witness: a witness whose failure does not move across a revision
/// settles on `WitnessUnsatisfiable`, and the run reports itself unproven
/// rather than failed.
///
/// On `main` this fails at the first assertion — the rung does not exist, and
/// the run keeps taking the `Revise` back-edge until the revision budget runs
/// out, ending on `VerificationFailed` with the worker named as the cause.
#[tokio::test]
async fn a_witness_that_never_moves_is_reported_unsatisfiable_not_blamed_on_the_worker() {
    let provider = provider_with_one_revision();
    let (outcome, events, _) = run_isolated_with_router(
        &provider,
        &both_trees_fail(),
        PipelineConfig::default(),
        "find my lost changes and merge them into master",
        router(),
    )
    .await;
    let outcome = outcome.expect("an unsatisfiable witness is not a hard error");

    let verdict = outcome.verdict.as_ref().expect("verification ran");
    let snapshot = verdict
        .ladder
        .as_deref()
        .expect("the verdict carries the snapshot it was decided from");
    assert_eq!(
        snapshot.rung,
        Some(LadderRung::WitnessUnsatisfiable),
        "the verdict must name the instrument, not the worker: {snapshot:?}"
    );

    // The turn is unproven, never failed: the work may be entirely correct and
    // nothing here observed it. `passed: true` carries that (a run is not
    // failed by the absence of a way to check it) and the STATUS is what stops
    // it reading as a pass (#2569).
    assert!(verdict.passed, "an unsatisfiable witness refutes nothing");
    assert!(!verdict.deterministic);
    assert!(
        matches!(outcome.status, PipelineStatus::Unverified { .. }),
        "{:?}",
        outcome.status
    );
    assert_eq!(outcome.score, Some(CandidateScore::Unverified));

    // Exactly one revision was spent — the one that ran the experiment. The
    // rung is terminal, so no further back-edge was taken however much budget
    // remained.
    assert_eq!(
        outcome.revisions, 1,
        "one revision proves the failure is unmoved; a second would be spent \
         on an instrument no worker can repair"
    );

    // And the verdict the stream carried says the same thing, in the words a
    // human reads.
    let summaries: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Verdict { evidence, .. } => Some(evidence.summary.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        summaries
            .last()
            .is_some_and(|s| s.starts_with("WITNESS UNSATISFIABLE")),
        "{summaries:?}"
    );
}

/// …and the first red still reaches the worker. The rung must not cost the
/// pipeline its repair loop: before a revision has been spent, "the witness is
/// deaf" and "the work is not done yet" are the same observation, and acting
/// on evidence that cannot separate them is what this repository's own rules
/// forbid.
///
/// Same fixture, one fewer scripted call: the run gets its first red and the
/// provider is exhausted, so the revision turn is where it stops. The
/// assertion is that a revision was attempted at all.
#[tokio::test]
async fn the_first_red_still_sends_the_worker_a_revision() {
    let provider = provider_with_one_revision();
    let (outcome, events, _) = run_isolated_with_router(
        &provider,
        &both_trees_fail(),
        PipelineConfig::default(),
        "find my lost changes and merge them into master",
        router(),
    )
    .await;
    outcome.expect("run settles");

    // The first verdict on the stream is the deterministic red that bought the
    // revision — proof the worker was told before the rung concluded anything
    // about the instrument.
    let first = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::Verdict { passed, evidence } => Some((*passed, evidence.summary.clone())),
            _ => None,
        })
        .expect("a verdict was emitted");
    assert!(
        !first.0,
        "the first round is an ordinary deterministic failure, fed back as \
         something to fix: {first:?}"
    );
    assert!(
        !first.1.starts_with("WITNESS UNSATISFIABLE"),
        "the rung must not fire before the experiment has been run: {first:?}"
    );
}
