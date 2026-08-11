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

    /// Swap the router, for the cases that need three agents to resolve to
    /// three *different* models — the only way to make the verifier and the
    /// assigned witness author disagree (#2467).
    fn with_router(mut self, router: Router) -> Self {
        self.router = router;
        self
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

/// A router whose worker, triage and verifier agents resolve to three named
/// models, so a case can make the verifier and the *assigned* witness author
/// disagree. `Fixture`'s own `router()` resolves everything to one model,
/// which is precisely the posture under which #2467 is unreachable.
fn triple_router(worker: &str, triage: &str, verifier: &str) -> Router {
    let mut roles = RoleTable::new();
    roles.pin(Role::Worker, ModelRef::new("scripted", worker));
    roles.pin(Role::Triage, ModelRef::new("scripted", triage));
    roles.pin(Role::Verifier, ModelRef::new("scripted", verifier));
    Router::new(
        roles,
        vec![ProviderProfile::new(
            "scripted",
            ModelRef::new("scripted", "worker"),
            ModelRef::new("scripted", "triage"),
            ModelRef::new("scripted", "verifier"),
        )],
        CircuitBreaker::new(Box::new(ZeroClock)),
    )
}

/// A config with witness authoring live — no `test_command`, since an explicit
/// oracle pre-empts authoring before the gate is ever consulted.
fn witness_config(roster: Roster) -> PipelineConfig {
    PipelineConfig {
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        max_revisions: 0,
        roster: roster.with_enabled(ModelCallRole::DistressGuidance, false),
        ..PipelineConfig::default()
    }
}

/// **#2467's witness, false-negative direction.**
///
/// The gate resolved `Role::Verifier` literally while the resolution went
/// through the roster, so moving `witness_author` elsewhere made the two
/// disagree. Here the verifier resolves to the worker's own model — the
/// posture that legitimately blocks authoring — but authoring has been
/// reassigned to the `triage` agent, which resolves to a third model. An
/// independent author exists, and the gate must say so.
///
/// Fails on this PR's HEAD before the fix: the gate probes the verifier,
/// finds the worker's model, and declines to author with a message advising a
/// change to `pipeline_verifier_model` — a row the operator deliberately moved
/// authoring off.
#[tokio::test]
async fn the_witness_gate_follows_the_assignment() {
    let fixture = Fixture::new(vec![]).with_router(triple_router("shared", "third", "shared"));
    let resolver = OneProvider(&fixture.provider);
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut roster = Roster::default();
    roster.set_agent(
        ModelCallRole::WitnessAuthor,
        crate::roster::AgentId::new("triage"),
    );
    let pipeline = fixture.pipeline(&resolver, tx, witness_config(roster));

    assert!(
        pipeline.can_author_independent_witness(),
        "the `triage` agent resolves to a model the worker does not use, so an independent \
         author exists — the gate must answer for the assigned agent, not for the verifier"
    );
}

/// **#2467's witness, false-positive direction — the one that loses evidence.**
///
/// The mirror: the verifier resolves to a model distinct from the worker's, so
/// the old gate answered "independent" and the run therefore did NOT tell the
/// worker its own failing test was the only deterministic evidence it would
/// carry. Authoring is assigned to the `worker` agent, so
/// `resolve_witness_author`'s worker-model filter then dropped the author —
/// silently, by contract, because the gate was supposed to have spoken. The
/// run finished with no witness, no test-first instruction, and nothing said.
///
/// Both halves are asserted: the gate declines, and the worker's prompt
/// carries `VerificationContract::WorkerTestFirst`, which is chosen off the
/// gate several steps before the resolution could have healed it.
#[tokio::test]
async fn an_author_bound_to_the_worker_makes_the_run_ask_for_a_worker_test() {
    let fixture = Fixture::new(vec![
        text_result("single"),
        text_result("done"),
        text_result("PASS"),
    ])
    .with_router(triple_router("w-model", "t-model", "v-model"));
    let resolver = OneProvider(&fixture.provider);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut roster = Roster::default();
    roster.set_agent(
        ModelCallRole::WitnessAuthor,
        crate::roster::AgentId::new("worker"),
    );
    let pipeline = fixture.pipeline(&resolver, tx, witness_config(roster));

    assert!(
        !pipeline.can_author_independent_witness(),
        "a witness the worker wrote proves nothing about the worker's diff, so authoring \
         bound to the worker must not read as independent"
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("losing the author costs the witness, never the task");
    // Losing the author costs the *proof*, and #2569 is that the cost is now
    // stated: the run settles `Unverified` rather than borrowing `Completed`.
    assert!(
        matches!(outcome.status, PipelineStatus::Unverified { .. }),
        "{:?}",
        outcome.status
    );

    // Not merely "the prompt carrying the goal": the triage classification
    // prompt quotes the goal too, and matching it first is how this assertion
    // passes for the wrong reason. `CLASS:` is the classifier's own answer
    // grammar and appears in no other call.
    let worker_prompt = fixture
        .provider
        .prompts()
        .into_iter()
        .find(|prompt| prompt.contains("Fix the failing test") && !prompt.contains("CLASS:"))
        .expect("the worker was prompted");
    assert!(
        worker_prompt.contains("write the failing test"),
        "with no independent author the worker must be told test-first, up front — the \
         contract is fixed before the resolution could heal the flag: {worker_prompt}"
    );

    let events = drain(&mut rx);
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Error { message, .. }
                if message.contains("no model independent of the worker")
        )),
        "the degradation is announced rather than silent: {events:?}"
    );
    assert!(!stages(&events).contains(&StageKind::Witness));
}

/// A responsibility the operator switched **off** is not an outage, and must
/// not be reported as one.
///
/// `report_roster_posture` already states the ablation once, before any spend.
/// The gate seeing `Independence::Withheld` therefore declines in silence —
/// the distinction #2381 exists to keep legible, and the reason the probe
/// returns four states rather than three.
#[tokio::test]
async fn an_ablated_witness_author_is_not_announced_as_a_degradation() {
    let fixture = Fixture::new(vec![]).with_router(triple_router("w-model", "t-model", "v-model"));
    let resolver = OneProvider(&fixture.provider);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = fixture.pipeline(
        &resolver,
        tx,
        witness_config(Roster::default().with_enabled(ModelCallRole::WitnessAuthor, false)),
    );

    assert!(!pipeline.can_author_independent_witness());
    let events = drain(&mut rx);
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::Error { message, .. }
                if message.contains("no model independent of the worker")
        )),
        "an ablation the operator asked for must not be filed as a degradation: {events:?}"
    );
}
