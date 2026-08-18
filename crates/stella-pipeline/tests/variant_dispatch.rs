//! #3408's witness that the pipeline's stage schedule is genuinely read from
//! a manifest, at the boundary where each decision is honestly answerable —
//! not decided from a Rust branch that merely agrees with one, and not
//! decided from a placeholder that happens to agree on the shipped variant.
//!
//! `crates/stella-pipeline/src/pipeline.rs` consults
//! [`stella_pipeline::schedule::Schedule`] at exactly the points this file
//! drives directly: [`Pipeline::begin_turn_schedule`] resolves `triage`
//! through `scope` right after triage's real facts exist (`pub(super)` to
//! `stella-pipeline`, so this file exercises the same [`Schedule`] sequence
//! rather than calling it), and `Pipeline::run_candidate` resolves
//! `execute`/`witness`/`verify` **per candidate, after that candidate's real
//! diff exists** — the fix this file's `verify_runs_or_not_by_the_real_diff`
//! test demonstrates end to end, through an actual `Pipeline::run`, not just
//! at the `Schedule` level.
//!
//! Before #3408's second half landed, `run_candidate` never consulted
//! `Schedule` at all: `verify` was decided once, before any candidate
//! existed, against a permanent `diff_lines: 0` / `mutating_actions: 0`
//! placeholder. A manifest with `verify if = "diff-lines > 0"` therefore
//! never verified ANY candidate, however large its real diff — the exact
//! failure `verify_runs_or_not_by_the_real_diff` would have caught on the
//! pre-fix code (see that test's own doc comment for which assertion fails
//! and why).
//!
//! `crates/stella-pipeline/src/pipeline/tests.rs`'s existing end-to-end
//! stage-sequence assertions (`clean_lookup_skips_plan_verify_and_verifier`,
//! `single_task_with_a_flip_submits_fast_and_skips_the_verifier`, …) are the
//! regression net that would go red if the shipped `classic` wiring
//! diverged from what `pipeline.rs` actually consults — see this crate's
//! `variant_program` and `pipeline::tests` suites, unchanged by this file.
//!
//! A condition naming an unknown key or an unpublished signal is already
//! covered where the parser lives:
//! `crates/stella-plugin/tests/wrapper_stages.rs::an_unknown_key_is_a_load_error`
//! and `::a_condition_naming_an_unpublished_signal_is_a_load_error`. Nothing
//! here duplicates them — a [`stella_plugin::Wrapper`] only reaches
//! [`PipelineConfig::variant`](stella_pipeline::PipelineConfig) after already
//! passing through that loader, so there is no second load-time check at the
//! pipeline layer to witness.
//!
//! [`Pipeline::begin_turn_schedule`]: stella_pipeline::pipeline::Pipeline

use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use stella_core::retry::Sleeper;
use stella_core::router::{CircuitBreaker, ProviderProfile, RoleTable};
use stella_core::{Clock, Router, ToolExecutor};
use stella_pipeline::ports::{
    AlwaysAbortGate, CmdKind, CmdOutcome, DiagnosticInvocation, DiagnosticRunner, NoContextRecall,
    NoRepoStatus, NoRepoStructure, PipelinePorts, ProviderResolver, TestInvocation, TestRunner,
};
use stella_pipeline::schedule::{HostFacts, Schedule, refuse_witness_reading_post_triage};
use stella_pipeline::{Pipeline, PipelineConfig};
use stella_protocol::event::BudgetMode;
use stella_protocol::{
    AgentEvent, CompletionMessage, CompletionRequestRef, CompletionResult, CompletionUsage,
    ModelRef, Provider, ProviderError, StageKind, ToolOutput, ToolSchema,
};

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

/// The pre-execute batch `Pipeline::begin_turn_schedule` performs (through
/// `scope`; `witness` is answered by a throwaway clone, matching the real
/// function — see its own module doc), reproduced here against a manifest
/// handed in from outside — the same sequence, driven the same way, so a
/// divergence in either would show up as a failure here without requiring a
/// live `Pipeline::run`.
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
    let mut witness_probe = schedule.clone();
    witness_probe.decide(StageName::Execute).unwrap();
    let witness = witness_probe.decide(StageName::Witness).unwrap();
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

/// **P1 guard, restored under the new semantics.** The deleted test
/// `the_shipped_variant_reads_only_facts_the_triage_boundary_has` protected
/// against a manifest silently reading placeholder values by asserting the
/// pipeline's one-shot reader (`variant.rs::classic_program`/`signals`) never
/// resolved a post-triage signal. That premise is gone now that `witness` is
/// answered progressively — this is its replacement: a manifest whose
/// `witness` condition reads a signal `execute` (or `witness`, or `verify`)
/// publishes is REFUSED with a named error at the exact boundary that would
/// otherwise have silently resolved it against a placeholder, rather than
/// producing a wrong answer that happens to be unreachable today.
#[test]
fn a_witness_condition_reading_a_post_triage_signal_is_refused() {
    let bad = load(
        "name = \"probe\"\n\
         [loop]\n\
         participation = \"steering\"\n\
         [wrapper]\n\
         id = \"bad-v1\"\n\
         [[wrapper.stages]]\n\
         name = \"execute\"\n\
         [[wrapper.stages]]\n\
         name = \"witness\"\n\
         if = \"diff-lines > 0\"\n",
    );
    let err = refuse_witness_reading_post_triage(&bad)
        .expect_err("a witness condition reading execute's diff-lines must be refused");
    let message = err.to_string();
    assert!(
        message.contains("diff-lines") && message.contains("3408"),
        "the refusal must name the offending signal and cite #3408: {message}"
    );

    // `classic.toml`'s own shape — `witness if = "no-test-command"`, a host
    // fact — must still load and pass this check unchanged.
    let classic = load(STAGED);
    assert!(
        refuse_witness_reading_post_triage(&classic).is_ok(),
        "the shipped variant's witness condition reads a host fact, never a \
         stage-published signal, and must never be refused"
    );
}

// --- The end-to-end witness: a real `Pipeline::run`, not just `Schedule`. --

/// A [`ProviderResolver`] serving one scripted [`Provider`] for every role.
struct OneProvider<'p>(&'p ScriptedProvider);
impl ProviderResolver for OneProvider<'_> {
    fn provider_for(&self, _model: &ModelRef) -> Option<&dyn Provider> {
        Some(self.0)
    }
}

/// Answers triage, then the worker, from one ordered queue.
struct ScriptedProvider {
    script: Mutex<std::collections::VecDeque<CompletionResult>>,
}
impl ScriptedProvider {
    fn new(results: Vec<CompletionResult>) -> Self {
        Self {
            script: Mutex::new(results.into_iter().collect()),
        }
    }
}
#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted"
    }
    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ProviderError::Terminal("scripted provider exhausted".into()))
    }
}

fn text_result(text: &str) -> CompletionResult {
    CompletionResult {
        upstream_provider: None,
        text: text.into(),
        tool_calls: vec![],
        usage: CompletionUsage {
            reported: true,
            ..CompletionUsage::default()
        },
        model: "scripted".into(),
        cost_usd: 0.0001,
        finish_reason: None,
    }
}

/// No tools at all: the candidate's diff comes entirely from the scripted
/// [`DiagnosticRunner`] below, not from any tool call this run dispatches.
struct NoTools;
#[async_trait]
impl ToolExecutor for NoTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        Vec::new()
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: String::new(),
            data: None,
        }
    }
}

/// Reports a fixed `git diff` and a `Pass` for any test invocation — this
/// variant declares no `witness`/`test-command`, so `run_test` is never
/// reached, but answering harmlessly rather than panicking keeps this
/// double honest about what it is scripting (a diff probe), not about every
/// path the pipeline could theoretically take.
struct ScriptedDiff {
    diff: String,
}
#[async_trait]
impl DiagnosticRunner for ScriptedDiff {
    async fn run_diagnostic(&self, invocation: &DiagnosticInvocation) -> CmdOutcome {
        if matches!(invocation, DiagnosticInvocation::GitDiff) {
            return CmdOutcome {
                exit_code: 0,
                stdout_tail: self.diff.clone(),
                stderr_tail: String::new(),
                kind: CmdKind::Completed,
            };
        }
        CmdOutcome {
            exit_code: 0,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            kind: CmdKind::Completed,
        }
    }
}
#[async_trait]
impl TestRunner for ScriptedDiff {
    async fn runner_available(&self, _probe: &TestInvocation) -> bool {
        true
    }
    async fn run_test(&self, _invocation: &TestInvocation) -> CmdOutcome {
        CmdOutcome {
            exit_code: 0,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            kind: CmdKind::Completed,
        }
    }
}

struct NoSleeper;
#[async_trait]
impl Sleeper for NoSleeper {
    async fn sleep(&self, _duration_ms: u64) {}
}

struct ZeroClock;
impl Clock for ZeroClock {
    fn now_ms(&self) -> u64 {
        0
    }
}

fn router() -> Router {
    Router::new(
        RoleTable::new(),
        vec![ProviderProfile::new(
            "scripted",
            ModelRef::new("scripted", "worker"),
            ModelRef::new("scripted", "triage"),
            ModelRef::new("scripted", "verifier"),
        )],
        CircuitBreaker::new(Box::new(ZeroClock)),
    )
}

/// Drive `lean-diff-v1` (`verify if = "diff-lines > 0"`) through a real
/// [`Pipeline::run`], once per real diff outcome, and assert `verify` runs
/// exactly when the manifest says it should.
///
/// **What would fail on the pre-fix code.** Before `run_candidate` consulted
/// `Schedule` at all, `verify` was decided once, before any candidate ran,
/// against a permanent `diff_lines: 0` placeholder — so `"diff-lines > 0"`
/// could never be true and `touched_stages.contains(&StageKind::Verify)`
/// below would be `false` for BOTH runs (`with_diff` included), failing the
/// first assertion. The empty-diff run's assertion happens to hold either
/// way (`0 > 0` is false under both the placeholder and the real value), so
/// the pre-fix code passes half this test and fails the half that proves the
/// bug: a real diff is silently never rewarded with verification.
async fn run_lean_diff_gated(diff: &str) -> Vec<AgentEvent> {
    let variant = load(LEAN_DIFF_GATED);
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let resolver = OneProvider(&provider);
    let tools = NoTools;
    let diagnostics = ScriptedDiff { diff: diff.into() };
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AlwaysAbortGate;
    let sleeper = NoSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let config = PipelineConfig {
        variant: Some(variant),
        test_command: None,
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        ..PipelineConfig::default()
    };
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            diagnostics: &diagnostics,
            tests: &diagnostics,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        config,
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = stella_core::BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run("Fix the widget", &mut messages, &mut budget)
        .await
        .expect("run succeeds");

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

fn ran_verify(events: &[AgentEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::Stage {
                name: StageKind::Verify,
                ..
            }
        )
    })
}

/// The end-to-end witness the audit demanded: `verify` runs for a candidate
/// that produced a real diff, and does not for one that produced none — the
/// SAME manifest, the SAME `Pipeline::run` call, driven twice.
#[tokio::test]
async fn verify_runs_or_not_by_the_real_diff() {
    let with_diff = run_lean_diff_gated("@@ -1 +1 @@\n-old\n+new").await;
    assert!(
        ran_verify(&with_diff),
        "a candidate producing a real diff must verify under `lean-diff-v1` \
         (`verify if = \"diff-lines > 0\"`); events: {with_diff:#?}"
    );

    let without_diff = run_lean_diff_gated("").await;
    assert!(
        !ran_verify(&without_diff),
        "a candidate producing no diff at all must not verify under the same \
         manifest; events: {without_diff:#?}"
    );
}
