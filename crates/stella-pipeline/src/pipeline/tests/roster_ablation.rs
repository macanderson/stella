// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The #2381 witness: one responsibility can be ablated without taking the
//! rest of the pipeline with it.
//!
//! Before the roster existed, `--no-pipeline` was the only off-switch and it
//! removed triage **and** plan **and** scope **and** witness **and** verify
//! together, which made it structurally impossible to attribute a measured
//! effect to any single stage — so two of the fourteen features under
//! evaluation in #2374 could not be graded at all.
//!
//! What these tests assert is deliberately about the **event stream** rather
//! than about an internal flag. A measurement reads `stella-events.jsonl`, so
//! "triage did not run" has to be visible there as an absent frame; an
//! ablation that merely skipped the call while still emitting `Stage{Triage}`
//! would leave every recorded trace claiming a stage that never happened.

use super::*;

/// Build the standard single-shot pipeline used by every case here, over a
/// scripted provider whose responses are supplied by the caller.
///
/// A local helper rather than a shared one: the point of each case is which
/// calls the provider is asked for, so the script has to stay visible at the
/// call site.
fn pipeline_with<'p>(
    provider: &'p ScriptedProvider,
    resolver: &'p OneProvider<'p>,
    runner: &'p ScriptedRunner,
    tools: &'p EmptyTools,
    recall: &'p NoContextRecall,
    repo: &'p NoRepoStructure,
    repo_status: &'p NoRepoStatus,
    approvals: &'p AutoApproveGate,
    sleeper: &'p NoopSleeper,
    router: &'p Router,
    tx: mpsc::UnboundedSender<AgentEvent>,
    config: PipelineConfig,
) -> Pipeline<'p> {
    let _ = provider;
    Pipeline::new(
        PipelinePorts {
            router,
            providers: resolver,
            tools,
            recall,
            repo,
            repo_status,
            touches: &NoFileTouches,
            diagnostics: runner,
            tests: runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals,
            sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        config,
    )
}

/// A config with `responsibilities` applied, as the CLI would build it.
fn config_with(rows: &[(&str, Option<bool>, Option<&str>)]) -> PipelineConfig {
    let mut config = PipelineConfig {
        // A user-supplied oracle keeps the flip path deterministic and takes
        // witness authoring out of the picture, so these cases isolate the
        // stage each one names.
        test_command: Some("cargo test -p x".into()),
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        ..PipelineConfig::default()
    };
    let _ = config
        .roster
        .apply(rows.iter().map(|(name, enabled, agent)| {
            (
                (*name).to_string(),
                crate::roster::AssignmentOverride {
                    enabled: *enabled,
                    agent: agent.map(crate::roster::AgentId::new),
                },
            )
        }));
    config
}

/// **The #2381 witness.** Disabling triage removes its frame from the stream
/// and nothing else's.
///
/// Fails on `main` without the roster for the plainest possible reason: there
/// is no key to disable triage with, so `Stage{Triage}` is emitted on every
/// run and the first assertion below cannot hold.
#[tokio::test]
async fn ablating_triage_removes_its_frame_and_leaves_execute_and_verify_running() {
    // ONE scripted response, not two: with triage ablated there is no
    // classification call to answer, so a script sized for one would leave a
    // response unconsumed. That the run completes on exactly this script is
    // itself part of the assertion — it proves no triage call was bought.
    let provider = ScriptedProvider::new(vec![text_result("done")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, true], "@@ -1 +1 @@\n-old\n+new");
    let (tools, recall, repo) = (EmptyTools, NoContextRecall, NoRepoStructure);
    let (repo_status, approvals, sleeper) = (NoRepoStatus, AutoApproveGate, NoopSleeper);
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let pipeline = pipeline_with(
        &provider,
        &resolver,
        &runner,
        &tools,
        &recall,
        &repo,
        &repo_status,
        &approvals,
        &sleeper,
        &router,
        tx,
        config_with(&[("triage", Some(false), None)]),
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("an ablated stage must not fail the run");
    assert_eq!(outcome.status, PipelineStatus::Completed);

    let stages = stages(&drain(&mut rx));
    assert!(
        !stages.contains(&StageKind::Triage),
        "an ablated stage must leave NO frame — a reader of stella-events.jsonl has to \
         see the ablation, not infer it; got {stages:?}"
    );
    assert!(
        stages.contains(&StageKind::Execute),
        "ablating triage must not take execute with it; got {stages:?}"
    );
    assert!(
        stages.contains(&StageKind::Verify),
        "ablating triage must not take verification with it; got {stages:?}"
    );
    assert!(
        provider.shapes().len() == 1,
        "exactly one paid call — the worker's — should have been bought; got {}",
        provider.shapes().len()
    );
}

/// The other half of the ablation pair the measurement plan needs: the
/// verifier off, everything else on.
///
/// The deterministic ladder (`StageKind::Verify`) still runs — it is not the
/// verifier — so this ablation removes the model verdict alone, which is
/// exactly the attribution #2374 wants.
#[tokio::test]
async fn ablating_the_verdict_leaves_the_deterministic_ladder_running() {
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, true], "@@ -1 +1 @@\n-old\n+new");
    let (tools, recall, repo) = (EmptyTools, NoContextRecall, NoRepoStructure);
    let (repo_status, approvals, sleeper) = (NoRepoStatus, AutoApproveGate, NoopSleeper);
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let pipeline = pipeline_with(
        &provider,
        &resolver,
        &runner,
        &tools,
        &recall,
        &repo,
        &repo_status,
        &approvals,
        &sleeper,
        &router,
        tx,
        config_with(&[("verdict", Some(false), None)]),
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("an ablated verifier must not fail the run");
    assert_eq!(outcome.status, PipelineStatus::Completed);

    let stages = stages(&drain(&mut rx));
    assert!(
        !stages.contains(&StageKind::Verdict),
        "the ablated verdict stage must leave no frame; got {stages:?}"
    );
    assert!(
        stages.contains(&StageKind::Triage),
        "ablating the verdict must leave triage running; got {stages:?}"
    );
    assert!(
        stages.contains(&StageKind::Verify),
        "the DETERMINISTIC ladder is not the verifier and must still run; got {stages:?}"
    );
}

/// Requirement 4: a roster nobody configured changes nothing.
///
/// The weakest-looking test here and the one that matters most, because it is
/// the configuration almost every real run uses.
#[tokio::test]
async fn a_default_roster_runs_the_pipeline_that_shipped() {
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, true], "@@ -1 +1 @@\n-old\n+new");
    let (tools, recall, repo) = (EmptyTools, NoContextRecall, NoRepoStructure);
    let (repo_status, approvals, sleeper) = (NoRepoStatus, AutoApproveGate, NoopSleeper);
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let pipeline = pipeline_with(
        &provider,
        &resolver,
        &runner,
        &tools,
        &recall,
        &repo,
        &repo_status,
        &approvals,
        &sleeper,
        &router,
        tx,
        config_with(&[]),
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");
    assert_eq!(outcome.status, PipelineStatus::Completed);

    let stages = stages(&drain(&mut rx));
    assert!(
        stages.contains(&StageKind::Triage),
        "an unconfigured roster must leave triage exactly where it was; got {stages:?}"
    );
}

/// A roster that cannot be honoured refuses **before** any paid call, rather
/// than running a pipeline the operator did not describe.
///
/// The provider script is empty on purpose: if the refusal ever moved below
/// the first call, this test would fail by exhausting it rather than by
/// asserting anything, which is the loudest way for that regression to land.
#[tokio::test]
async fn an_unhonourable_roster_refuses_before_spending_anything() {
    let provider = ScriptedProvider::new(vec![]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, true], "@@ -1 +1 @@\n-old\n+new");
    let (tools, recall, repo) = (EmptyTools, NoContextRecall, NoRepoStructure);
    let (repo_status, approvals, sleeper) = (NoRepoStatus, AutoApproveGate, NoopSleeper);
    let router = router();
    let (tx, _rx) = mpsc::unbounded_channel();

    let pipeline = pipeline_with(
        &provider,
        &resolver,
        &runner,
        &tools,
        &recall,
        &repo,
        &repo_status,
        &approvals,
        &sleeper,
        &router,
        tx,
        config_with(&[("verdict", None, Some("verifer"))]),
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let error = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect_err("a typo'd agent name must refuse the run");

    assert!(
        matches!(error.cause, PipelineError::InvalidRoster(_)),
        "expected an InvalidRoster refusal; got {:?}",
        error.cause
    );
    assert!(
        provider.shapes().is_empty(),
        "the refusal must land before any paid call"
    );
}
