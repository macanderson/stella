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

/// The scripted ports every case here shares, owned in one place so they
/// outlive the [`Pipeline`] that borrows them.
///
/// A struct rather than a twelve-argument constructor: the doubles are
/// identical in every case and only the *script* and the *roster* vary, so
/// threading them through a parameter list would bury the two things each test
/// is actually about.
struct Fixture {
    provider: ScriptedProvider,
    runner: ScriptedRunner,
    tools: EmptyTools,
    recall: NoContextRecall,
    repo: NoRepoStructure,
    repo_status: NoRepoStatus,
    approvals: AutoApproveGate,
    sleeper: NoopSleeper,
    router: Router,
}

impl Fixture {
    /// `script` is the provider's queued responses, in call order — the part
    /// of each case that carries its meaning, since a stage that was ablated
    /// buys no call and so consumes no entry.
    fn new(script: Vec<CompletionResult>) -> Self {
        Self {
            provider: ScriptedProvider::new(script),
            runner: ScriptedRunner::new(vec![false, true], "@@ -1 +1 @@\n-old\n+new"),
            tools: EmptyTools,
            recall: NoContextRecall,
            repo: NoRepoStructure,
            repo_status: NoRepoStatus,
            approvals: AutoApproveGate,
            sleeper: NoopSleeper,
            router: router(),
        }
    }

    fn pipeline<'p>(
        &'p self,
        resolver: &'p OneProvider<'p>,
        tx: mpsc::UnboundedSender<AgentEvent>,
        config: PipelineConfig,
    ) -> Pipeline<'p> {
        Pipeline::new(
            PipelinePorts {
                router: &self.router,
                providers: resolver,
                tools: &self.tools,
                recall: &self.recall,
                repo: &self.repo,
                repo_status: &self.repo_status,
                touches: &NoFileTouches,
                diagnostics: &self.runner,
                tests: &self.runner,
                lint: None,
                mutation: None,
                coverage: None,
                approvals: &self.approvals,
                sleeper: &self.sleeper,
                hooks: None,
                candidate_workspaces: None,
                mcp_prefetch: None,
                steering: None,
            },
            tx,
            config,
        )
    }
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
    let fixture = Fixture::new(vec![text_result("done")]);
    let resolver = OneProvider(&fixture.provider);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = fixture.pipeline(&resolver, tx, config_with(&[("triage", Some(false), None)]));

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
        fixture.provider.shapes().len() == 1,
        "exactly one paid call — the worker's — should have been bought; got {}",
        fixture.provider.shapes().len()
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
    let fixture = Fixture::new(vec![text_result("single"), text_result("done")]);
    let resolver = OneProvider(&fixture.provider);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = fixture.pipeline(
        &resolver,
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
    let fixture = Fixture::new(vec![text_result("single"), text_result("done")]);
    let resolver = OneProvider(&fixture.provider);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = fixture.pipeline(&resolver, tx, config_with(&[]));

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
    let fixture = Fixture::new(vec![]);
    let resolver = OneProvider(&fixture.provider);
    let (tx, _rx) = mpsc::unbounded_channel();
    let pipeline = fixture.pipeline(
        &resolver,
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
        fixture.provider.shapes().is_empty(),
        "the refusal must land before any paid call"
    );
}
