//! One `--pipeline` selection served by **several** plugins (#3801).
//!
//! `WrapperDispatch::bind` took exactly one manifest, so a selection was one
//! plugin and nothing else. `doc:pipeline-as-plugins` §7 turns each pipeline
//! stage into its own plugin, which makes that limit decisive rather than
//! inconvenient: with one plugin per selection, the only way to get what the
//! deleted staged pipeline did is a single plugin reimplementing every stage
//! — the monolith again, in a different language. Until composition exists the
//! plugin path is strictly less capable than the thing it replaced, and
//! `doc:plugin-completion-plan` §3 says so in those words.
//!
//! These tests drive the composed sequence two ways. The toy pair is #3801's
//! own definition of done ("a test with two toy plugins, one contributing at
//! `recall`, one at `plan`"); the shipped pair is
//! `doc:plugin-completion-plan` §6 P2.1's (`research-v1` + `plan-v1` bound
//! together in one selection, and a stage-graph conflict refused at bind
//! time). Both are here because they fail differently: a toy pair proves the
//! fold, and the shipped pair proves the fold survives contact with the two
//! manifests this repository actually ships.
//!
//! `cfg(unix)` for `wrapper_socket.rs`'s reason (#3497).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use stella_plugin::{PluginManifest, SignalValues, StageName, TamperFinding, TurnOutcome};
use stella_protocol::completion::CompletionMessage;
use stella_runtime::wrapper::{
    DEFAULT_WRAPPER_TIMEOUT, DrivenTurn, RoundInput, SubprocessWrapper, TurnDriver, TurnPrelude,
    TurnWrapper, WrapperDispatch, WrapperError,
};

/// Grounding. Contributes at `recall` and nowhere else.
const RECALLER: &str = r#"
name = "toy-recaller"
[loop]
participation = "steering"
points = ["before_turn"]
[wrapper]
id = "recall-v1"
[[wrapper.stages]]
name = "recall"
[[wrapper.stages]]
name = "execute"
"#;

/// Planning. Contributes at `plan`, and declares `recall` before it — which is
/// what makes the two orders composable rather than merely non-overlapping.
const PLANNER: &str = r#"
name = "toy-planner"
[loop]
participation = "steering"
points = ["before_turn"]
[wrapper]
id = "plan-v1"
[[wrapper.stages]]
name = "recall"
[[wrapper.stages]]
name = "plan"
[[wrapper.stages]]
name = "execute"
"#;

/// The same stages as [`RECALLER`], in the opposite order. Composing the two
/// has no one answer, which is the point.
const CONTRARIAN: &str = r#"
name = "toy-contrarian"
[loop]
participation = "steering"
points = ["before_turn"]
[wrapper]
id = "contrary-v1"
[[wrapper.stages]]
name = "execute"
[[wrapper.stages]]
name = "recall"
"#;

/// An arbiter with an oracle — the shape `plugins/stella-goal` ships.
const ARBITER: &str = r#"
name = "toy-arbiter"
[loop]
participation = "arbiter"
hooks = ["Stop"]
points = ["after_turn"]
max_holds = 2
[requirements]
done = "the work is finished"
[oracle]
flip = "not-applicable"
measurements = ["met"]
[[oracle.checks]]
requirement = "done"
check = "met >= 1"
# An `[oracle]` needs somewhere to run: either its own `command`, or a
# `[runtime]` making the plugin's own process the oracle (`ManifestError::
# OracleCommandRequired` refuses neither). This fixture takes the second,
# which is the shape `plugins/stella-goal` ships.
[runtime]
argv = ["/bin/sh", "-c", "cat >/dev/null"]
timeout_secs = 30
env = ["PATH"]
[wrapper]
id = "arbiter-v1"
[[wrapper.stages]]
name = "execute"
"#;

/// A second arbiter that collides on the **grade alone** — a different
/// requirement name and no `[oracle]` of its own — so the arbiter witness is
/// genuinely about the grade rather than another check wearing its name.
const OTHER_ARBITER: &str = r#"
name = "toy-other-arbiter"
[loop]
participation = "arbiter"
hooks = ["Stop"]
points = ["after_turn"]
max_holds = 2
[requirements]
shipped = "the change is released"
[wrapper]
id = "other-arbiter-v1"
[[wrapper.stages]]
name = "execute"
"#;

/// Steering-grade, no oracle, and one requirement name [`ARBITER`] also
/// declares — with a different statement. Collides on that axis alone.
const DISAGREEING_REQUIREMENT: &str = r#"
name = "toy-disagreer"
[loop]
participation = "steering"
points = ["before_turn"]
[requirements]
done = "something else entirely"
[wrapper]
id = "disagree-v1"
[[wrapper.stages]]
name = "execute"
"#;

/// Echoes which plugin and which stage it was asked about, so the fold is
/// readable off the messages the turn receives rather than inferred.
fn manifest(text: &str) -> PluginManifest {
    PluginManifest::from_toml_str(text).expect("the fixture manifest loads")
}

const FIXTURE: &str = env!("CARGO_BIN_EXE_wrapper-plugin-fixture");

/// One member of a composition, driving the portable fixture (#4697).
fn plugin(mode: &[&str]) -> Arc<SubprocessWrapper> {
    let mut argv = vec![FIXTURE.to_string()];
    argv.extend(mode.iter().map(|part| (*part).to_string()));
    Arc::new(
        SubprocessWrapper::declare(argv, Vec::new(), DEFAULT_WRAPPER_TIMEOUT)
            .expect("the transport is declared with a program and a budget")
            .wrapper,
    )
}

fn signals() -> SignalValues {
    SignalValues {
        test_command: false,
        candidates: 1,
        budget_metered: false,
        conversational: false,
        questions: 2,
        plans: true,
        verifies: true,
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

fn input(goal: &str) -> RoundInput {
    RoundInput {
        goal: goal.into(),
        signals: signals(),
        candidate: None,
    }
}

#[derive(Default)]
struct Recorder {
    stages: Vec<StageName>,
    messages: Vec<CompletionMessage>,
}

#[async_trait(?Send)]
impl TurnDriver for Recorder {
    async fn run_turn(&mut self, prelude: TurnPrelude) -> DrivenTurn {
        self.stages = prelude.stages().to_vec();
        self.messages = prelude.into_messages();
        DrivenTurn {
            outcome: TurnOutcome {
                completed: true,
                answer: "done".into(),
                tools: None,
                changed_files: None,
            },
            tamper: TamperFinding::NotChecked,
        }
    }
}

impl Recorder {
    /// The contributed text, in the order it reached the turn.
    fn contributions(&self) -> Vec<String> {
        self.messages
            .iter()
            .map(|message| message.content.clone())
            .collect()
    }
}

/// **The witness (#3801).** Two plugins, one selection, and both
/// contributions reach one turn — in the merged stage order, not in the order
/// the plugins were named.
///
/// Fails before this change at the type level: `WrapperDispatch::bind` took
/// one `PluginManifest`, so there was no way to express "these two serve this
/// selection" and no second contribution to assert about.
#[tokio::test]
async fn two_plugins_compose_into_one_selection_and_both_reach_the_turn() {
    let dispatch = WrapperDispatch::bind_composed(vec![
        (manifest(RECALLER), plugin(&["echo-stage", "recaller"])),
        (manifest(PLANNER), plugin(&["echo-stage", "planner"])),
    ])
    .expect("two agreeing manifests compose");

    assert_eq!(
        dispatch.variant(),
        "recall-v1,plan-v1",
        "the selection records what actually ran, not whichever member was first"
    );

    let mut driver = Recorder::default();
    dispatch
        .run(input("make the retry budget honoured"), &mut driver)
        .await
        .expect("the composed stage order resolves");

    let stages: Vec<String> = driver.stages.iter().map(ToString::to_string).collect();
    assert_eq!(
        stages,
        vec!["recall", "plan", "execute"],
        "the union of both members' stages, in the one order they agree with — \
         `plan` sits between `recall` and `execute` on the planner's say-so, \
         and the recaller never contradicted it"
    );

    let contributions = driver.contributions();
    let joined = contributions.join("\n");
    assert!(
        joined.contains("recaller contributed at recall"),
        "the grounding plugin's contribution reached the turn: {joined}"
    );
    assert!(
        joined.contains("planner contributed at plan"),
        "the planning plugin's contribution reached the turn — this is the half \
         that was impossible before composition: {joined}"
    );

    let recall_first = contributions
        .iter()
        .position(|text| text.contains("at recall"))
        .expect("the recall contribution is present");
    let plan_later = contributions
        .iter()
        .position(|text| text.contains("planner contributed at plan"))
        .expect("the plan contribution is present");
    assert!(
        recall_first < plan_later,
        "stage-major order: everything contributed at `recall` reaches the turn \
         before anything contributed at `plan`, because that is what lets a \
         planner read grounding it did not gather"
    );
}

/// Each member keeps its **own** grants. A member that never declared
/// `before_turn` is not asked it because a sibling did.
#[tokio::test]
async fn composition_unions_contributions_and_not_permissions() {
    // The arbiter declares `points = ["after_turn"]` only.
    let dispatch = WrapperDispatch::bind_composed(vec![
        (manifest(RECALLER), plugin(&["echo-stage", "recaller"])),
        (manifest(ARBITER), plugin(&["echo-stage", "arbiter"])),
    ])
    .expect("a steering member and an arbiter member compose");

    let mut driver = Recorder::default();
    dispatch
        .run(input("anything"), &mut driver)
        .await
        .expect("the composed stage order resolves");

    let joined = driver.contributions().join("\n");
    assert!(
        joined.contains("recaller contributed"),
        "the member that declared before_turn was asked: {joined}"
    );
    assert!(
        !joined.contains("arbiter contributed"),
        "the member that did NOT declare before_turn was never asked — a \
         composition unions what plugins contribute, never what they are \
         permitted: {joined}"
    );
}

/// **The witness (`doc:plugin-completion-plan` §6 P2.1, second half).** Two
/// manifests that order the same stages differently are refused **at bind
/// time**, named, rather than resolved silently in favour of whoever was
/// first.
#[test]
fn a_stage_order_conflict_is_refused_at_bind_time() {
    let error = WrapperDispatch::bind_composed(vec![
        (manifest(RECALLER), plugin(&["exit", "0", ""])),
        (manifest(CONTRARIAN), plugin(&["exit", "0", ""])),
    ])
    .expect_err("`recall` before `execute` and `execute` before `recall` have no one order");

    match error {
        WrapperError::ConflictingStageOrder {
            ref wrapper,
            ref other,
            ..
        } => {
            assert_eq!(wrapper, "toy-recaller");
            assert_eq!(other, "toy-contrarian");
        }
        other => panic!("a stage-order conflict must name itself, got {other:?}"),
    }
    let rendered = error.to_string();
    assert!(
        rendered.contains("recall") && rendered.contains("execute"),
        "the refusal names the two stages that disagree: {rendered}"
    );
}

/// Two arbiters is two things holding one turn open and two definitions of
/// done, and it is the **only** grade conflict a composition can have.
///
/// `OTHER_ARBITER` declares no `[oracle]` and a different requirement name, so
/// nothing but the grade collides here — this is the arbiter check itself
/// rather than another check wearing its name.
#[test]
fn two_arbiters_are_refused_at_bind_time() {
    let error = WrapperDispatch::bind_composed(vec![
        (manifest(ARBITER), plugin(&["exit", "0", ""])),
        (manifest(OTHER_ARBITER), plugin(&["exit", "0", ""])),
    ])
    .expect_err("at most one member may hold a turn open");
    match error {
        WrapperError::TwoArbiters {
            ref wrapper,
            ref other,
        } => {
            assert_eq!(wrapper, "toy-arbiter");
            assert_eq!(other, "toy-other-arbiter");
        }
        other => panic!("the conflict must name both members, got {other:?}"),
    }
}

/// **Why there is no separate "two oracles" rule**, pinned rather than
/// asserted in a comment.
///
/// An `[oracle]` requires arbiter grade
/// (`ManifestError::OracleRequiresArbiter`), so two members with oracles are
/// two arbiters and the rule above already refuses them. A dedicated check
/// would be unreachable — and an error that cannot fire reads to the next
/// maintainer as a guarantee somebody tested.
///
/// If the schema ever lets a steering plugin declare an oracle, the first
/// assertion here fails and `compose::merge_rule` needs the check it
/// deliberately does not have.
#[test]
fn an_oracle_requires_arbiter_grade_which_is_why_two_of_them_need_no_rule() {
    let steering_with_oracle = r#"
name = "toy-steering-oracle"
[loop]
participation = "steering"
points = ["after_turn"]
[oracle]
flip = "not-applicable"
measurements = ["met"]
[wrapper]
id = "steering-oracle-v1"
[[wrapper.stages]]
name = "execute"
"#;
    let refused = PluginManifest::from_toml_str(steering_with_oracle)
        .expect_err("an oracle below arbiter grade is refused at parse");
    assert!(
        format!("{refused:?}").contains("OracleRequiresArbiter"),
        "refused for the reason the composition rule depends on: {refused:?}"
    );

    // And two arbiters *with* oracles are refused as two arbiters — the same
    // refusal, arrived at one layer up.
    assert!(matches!(
        WrapperDispatch::bind_composed(vec![
            (manifest(ARBITER), plugin(&["exit", "0", ""])),
            (manifest(ARBITER), plugin(&["exit", "0", ""])),
        ])
        .expect_err("two oracles are two arbiters"),
        WrapperError::TwoArbiters { .. }
    ));
}

/// **The assumption the requirement fold rests on**, pinned so it cannot
/// loosen silently.
///
/// `compose::merge_rule` unions the members' `[requirements]` with **no**
/// collision check, and that is only sound because at most one member can
/// carry any. Two independent rules make it so, and this test holds both:
///
/// 1. the manifest schema refuses `[requirements]` below arbiter grade, and
/// 2. a composition refuses a second arbiter (the test above).
///
/// Together: at most one arbiter, and only an arbiter has requirements, so a
/// union over "at most one non-empty set" cannot disagree with itself. If
/// either half ever loosens, this is the test that goes red, and the fold
/// needs a check it deliberately does not have.
#[test]
fn only_an_arbiter_may_declare_requirements_which_is_why_they_cannot_collide() {
    let refused = PluginManifest::from_toml_str(DISAGREEING_REQUIREMENT)
        .expect_err("a steering-grade manifest may not declare [requirements]");
    assert!(
        format!("{refused:?}").contains("RequirementsRequireArbiter"),
        "the schema refuses it for the reason this fold depends on: {refused:?}"
    );

    // And the arbiter that *may* declare them composes with a member that has
    // none, which is the ordinary case the fold exists to serve.
    WrapperDispatch::bind_composed(vec![
        (manifest(ARBITER), plugin(&["exit", "0", ""])),
        (manifest(RECALLER), plugin(&["exit", "0", ""])),
    ])
    .expect("one member's requirements and another's none is a union of one");
}

/// A composition with no members has no variant id and no stage order, so it
/// is refused rather than driving a turn with nothing wrapping it.
#[test]
fn an_empty_composition_is_refused() {
    assert!(matches!(
        WrapperDispatch::bind_composed(Vec::new()).expect_err("nothing to compose"),
        WrapperError::EmptyComposition
    ));
}

// ---------------------------------------------------------------------------
// The shipped pair
// ---------------------------------------------------------------------------

fn plugin_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins")
        .join(name)
        .canonicalize()
        .expect("the first-party plugins ship in this repository under plugins/")
}

fn shipped(name: &str) -> (PluginManifest, Arc<dyn TurnWrapper>) {
    let dir = plugin_dir(name);
    let text = std::fs::read_to_string(dir.join("plugin.toml"))
        .expect("the manifest file is `plugin.toml`, exactly");
    let manifest = PluginManifest::from_toml_str(&text).expect("the shipped manifest loads");
    let runtime = manifest
        .runtime
        .as_ref()
        .expect("a dispatchable plugin declares [runtime]");
    let argv: Vec<String> = runtime
        .argv
        .iter()
        .map(|arg| stella_plugin::expand_plugin_dir(arg, &dir))
        .collect();
    let env = runtime.child_env(|name| std::env::var(name).ok());
    let transport = SubprocessWrapper::declare(
        argv,
        env,
        std::time::Duration::from_secs(runtime.timeout_secs),
    )
    .expect("the declared runtime starts")
    .wrapper;
    (manifest, Arc::new(transport))
}

/// **The witness (`doc:plugin-completion-plan` §6 P2.1, first half).**
/// `research-v1` and `plan-v1` — the two plugins this repository actually
/// ships — bound together in one selection, with both stages observable in
/// what the turn receives.
///
/// This is the pair §3 names as the reason composition decides whether any of
/// this is a platform: grounding and planning were structurally mutually
/// exclusive choices, so a user got one or the other and the deleted pipeline
/// ran both.
#[tokio::test]
async fn the_two_shipped_plugins_compose_into_one_selection() {
    let dispatch =
        WrapperDispatch::bind_composed(vec![shipped("stella-research"), shipped("stella-plan")])
            .expect("the two shipped manifests declare the same stage order, so they compose");

    assert_eq!(
        dispatch.variant(),
        "research-v1,plan-v1",
        "the composed selection names both, in the order it was given them"
    );
    assert_eq!(
        dispatch.manifests().count(),
        2,
        "both members are held, not one with the other discarded"
    );

    let stages: Vec<String> = {
        let mut driver = Recorder::default();
        dispatch
            .run(input("retry_budget is not honoured"), &mut driver)
            .await
            .expect("the composed stage order resolves");
        driver.stages.iter().map(ToString::to_string).collect()
    };

    for stage in ["research", "plan"] {
        assert!(
            stages.iter().any(|ran| ran == stage),
            "the composed program runs `{stage}`, so both members' own stage \
             reaches the turn: {stages:?}"
        );
    }
    let research_at = stages
        .iter()
        .position(|stage| stage == "research")
        .expect("research ran");
    let plan_at = stages
        .iter()
        .position(|stage| stage == "plan")
        .expect("plan ran");
    assert!(
        research_at < plan_at,
        "grounding before planning, which is the whole reason to compose these \
         two rather than choose between them: {stages:?}"
    );
}
