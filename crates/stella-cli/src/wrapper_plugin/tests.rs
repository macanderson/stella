// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for [`crate::wrapper_plugin`] — the CLI as a driver of the wrapper
//! socket.
//!
//! A sibling file rather than an inline `mod tests`, for the reason AGENTS.md
//! § "God files" gives: the parent carries the module doc, the flag decisions,
//! the host assembly and the driver, and this suite grew past what the same
//! file can hold under the 1500-line ratchet.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use stella_core::subagent::{SubAgentDispatcher, SubAgentOutcome, SubAgentReport, SubAgentSpec};
use stella_plugin::PluginManifest;
use stella_runtime::wrapper::TurnDriver;

use super::*;
use crate::plugin_cmd::roster::{InstalledPlugin, PluginScope};

const WRAPPER_MANIFEST: &str = r#"
name = "budget-keeper"
[loop]
participation = "steering"
points = ["before_turn", "after_turn"]
[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh"]
timeout_secs = 30
env = ["PATH", "ANTHROPIC_API_KEY"]
[wrapper]
id = "budget-v1"
[[wrapper.stages]]
name = "execute"
"#;

/// An arbiter-grade wrapper, shaped exactly like `plugins/stella-goal`'s real
/// manifest (`[loop] participation = "arbiter"`, the `Stop` hook it requires,
/// one requirement, and a `flip = "not-applicable"` oracle deciding it) —
/// minimal enough to load, but the same grade `reject_arbiter_wrapper_on_goal`
/// (#3832) exists to catch on `stella goal`.
const ARBITER_WRAPPER_MANIFEST: &str = r#"
name = "goal-arbiter"
[loop]
participation = "arbiter"
hooks = ["Stop"]
points = ["after_turn"]
max_holds = 3
[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh"]
timeout_secs = 30
env = ["PATH"]
[wrapper]
id = "arbiter-v1"
[[wrapper.stages]]
name = "execute"
[requirements]
goal-met = "an independent verifier turn assessed the goal as accomplished"
[oracle]
flip = "not-applicable"
measurements = ["met"]
[[oracle.checks]]
requirement = "goal-met"
check = "met >= 1"
"#;

fn installed(text: &str, dir: &str) -> InstalledPlugin {
    InstalledPlugin {
        manifest: PluginManifest::from_toml_str(text).expect("fixture must load"),
        dir: PathBuf::from(dir),
        scope: PluginScope::User,
    }
}

fn roster(plugins: Vec<InstalledPlugin>) -> PluginRoster {
    PluginRoster::compose(plugins, Vec::new(), &BTreeMap::new())
}

/// A context plane that answers every ask with one frame, so a test can
/// tell "the gate reached the plane" from "the gate refused".
struct OneFrame;

#[async_trait]
impl stella_runtime::wrapper::RecallHost for OneFrame {
    async fn recall(&self, goal: &str) -> Vec<stella_plugin::RecallFrame> {
        vec![stella_plugin::RecallFrame {
            label: "the last run".to_string(),
            kind: "memory".to_string(),
            source: "context.db".to_string(),
            uri: None,
            content: format!("about {goal}"),
        }]
    }
}

fn no_recall() -> Box<dyn stella_runtime::wrapper::RecallHost> {
    Box::new(crate::wrapper_recall::SessionRecallHost::none())
}

fn bound(
    roster: &PluginRoster,
    variant: &str,
    warn: &mut dyn FnMut(String),
) -> Result<BoundWrapper, String> {
    bind_installed(roster, variant, warn)?.serving(WrapperHost::recalling(no_recall()))
}

/// **Witness (#3381 "Flip the default").** No flag at all used to mean
/// `Classic` — the staged pipeline was the default. This assertion fails
/// on the pre-#3381 code (which resolves `(false, None)` to `Classic`)
/// and passes on this one: the raw loop is the default now, with or
/// without `--no-pipeline`.
#[test]
fn no_flag_at_all_resolves_to_the_raw_loop() {
    assert_eq!(
        PipelineChoice::resolve(false, None),
        Ok(PipelineChoice::Raw)
    );
    assert_eq!(
        PipelineChoice::resolve(true, None),
        Ok(PipelineChoice::Raw),
        "--no-pipeline is a deprecated no-op: it names the same choice as no flag at all"
    );
}

/// **Witness (#3865).** `--pipeline classic` used to resolve to a `Classic`
/// variant of [`PipelineChoice`] (itself since deleted, #3867);
/// the built-in staged pipeline is gone, so it must now refuse instead. This
/// assertion fails on the pre-slice-1 code (which still resolves `Ok`) and
/// passes on this one. An unknown/plugin variant is unaffected — it still
/// binds a plugin lookup rather than the retired built-in.
#[test]
fn pipeline_classic_is_refused_and_an_unknown_variant_still_binds_a_plugin_lookup() {
    let refusal =
        PipelineChoice::resolve(false, Some("classic")).expect_err("classic no longer resolves");
    assert!(
        refusal.contains("--pipeline classic"),
        "the refusal names the flag/value that triggered it: {refusal}"
    );
    assert!(
        refusal.contains("stella plugin install"),
        "the refusal points at the wrapper-plugin remedy: {refusal}"
    );
    assert_eq!(
        PipelineChoice::resolve(false, Some("budget-v1")),
        Ok(PipelineChoice::Plugin("budget-v1"))
    );
}

/// **Witness (#3381).** `--no-pipeline` together with `--pipeline` used to
/// be a hard error (`conflicts_with` in clap, then an `Err` from
/// `resolve`). This assertion fails on the pre-#3381 code (which returns
/// `Err` here) and passes on this one: a deprecated no-op flag must not
/// veto an explicit `--pipeline` opt-in.
#[test]
fn no_pipeline_no_longer_vetoes_an_explicit_pipeline_choice() {
    assert_eq!(
        PipelineChoice::resolve(true, Some("budget-v1")),
        Ok(PipelineChoice::Plugin("budget-v1")),
        "--pipeline wins outright; the deprecated flag has nothing left to veto"
    );
}

/// The deprecated `--no-pipeline` flag does not exempt `classic` from its
/// own refusal — the two are independent, and neither can silence the other.
#[test]
fn no_pipeline_does_not_exempt_classic_from_its_refusal() {
    assert!(PipelineChoice::resolve(true, Some("classic")).is_err());
}

/// The notice fires exactly when `--no-pipeline` was passed, regardless of
/// what `--pipeline` said alongside it, and says nothing when it was not.
#[test]
fn the_deprecation_notice_fires_only_when_no_pipeline_was_passed() {
    assert!(no_pipeline_deprecation_notice(false).is_none());
    let notice = no_pipeline_deprecation_notice(true).expect("flag was passed");
    assert!(notice.contains("--no-pipeline"), "{notice}");
    assert!(notice.contains("--pipeline"), "{notice}");
}

/// **Witness (#3832, finding 1).** An arbiter-grade wrapper used to reach
/// `stella goal`'s round loop and only discover it could not be driven
/// after `WrapperDispatch::run` had already billed
/// `1 + DEFAULT_HOST_MAX_HOLDS` worker turns inside one judged round
/// (`report.rounds != 1`, `goal_wrapped::run_goal_wrapped_turn`) — every one
/// of those turns paid for before the whole run was discarded. This
/// assertion fails on the pre-fix code (no such pre-flight check exists, so
/// `reject_arbiter_wrapper_on_goal` is not even a name in scope / always
/// returns `Ok`) and passes on this one: the arbiter grade is refused at
/// resolve/bind time, before any provider is ever built and before a single
/// paid call — mirrored end to end (no cost, no provider reached) by
/// `crates/stella-cli/tests/goal_arbiter_wrapper_refusal_cli.rs`.
#[test]
fn an_arbiter_grade_wrapper_is_refused_on_goal() {
    let mut warn = |_: String| {};
    let roster = roster(vec![installed(
        ARBITER_WRAPPER_MANIFEST,
        "/plugins/goal-arbiter",
    )]);
    let resolved =
        bind_installed(&roster, "arbiter-v1", &mut warn).expect("arbiter manifest must load");
    let err = reject_arbiter_wrapper_on_goal(&resolved)
        .expect_err("arbiter-grade wrappers cannot run on stella goal (#3832)");
    assert!(err.contains("arbiter-v1"), "{err}");
    assert!(err.contains("#3832"), "{err}");
    assert!(
        err.contains("stella run --pipeline arbiter-v1"),
        "the refusal must name the remedy — the wrapper's designed home: {err}"
    );
    assert!(
        err.contains("stella goal"),
        "the refusal must name the door that refused it: {err}"
    );
}

/// The companion half: steering (and, by the same ladder argument, observer)
/// wrappers are unaffected by the arbiter refusal and keep running per round
/// on `stella goal` exactly as before — `a_goal_run_dispatches_each_round_
/// through_the_bound_wrapper` (`goal_wrapped_dispatch_cli.rs`) is the e2e
/// witness that a steering wrapper's rounds still actually advance; this is
/// the unit-level companion pinning the gate itself lets it through.
#[test]
fn a_steering_wrapper_is_not_refused_by_the_arbiter_gate() {
    let mut warn = |_: String| {};
    let roster = roster(vec![installed(WRAPPER_MANIFEST, "/plugins/budget-keeper")]);
    let resolved =
        bind_installed(&roster, "budget-v1", &mut warn).expect("steering manifest must load");
    reject_arbiter_wrapper_on_goal(&resolved)
        .expect("a steering-grade wrapper can never hold a round open — nothing to refuse");
}

// `a_resolved_raw_choice_is_never_refused_on_any_door` lived here, pinning
// `reject_plugin_variant_for_door`: the gate that refused a named plugin
// variant on `stella fleet`, the one door with no `TurnDriver` over its round
// loop. #3896 gave fleet workers that driver — one plugin per attempt — so the
// door it guarded no longer exists and the function was deleted with it. The
// test outlived its subject by three merges because nothing compiled the two
// together until now.

/// **Witness (#3696).** `--keep-witness`, `--require-verified`, and
/// `--test-command` used to reach `run_one_shot` on the `Raw` arm and be
/// silently dropped there (`run_raw_one_shot` takes no `keep_witness`/
/// `require_verified` parameter at all). This assertion fails against
/// that code (which has no such gate, so every call below returns `Ok`)
/// and passes on this one: each flag alone against `Raw` is refused, and
/// the message names the remedy.
#[test]
fn each_verification_flag_alone_is_refused_against_the_raw_loop() {
    let err = reject_verification_flags_without_pipeline(
        PipelineChoice::Raw,
        Some("pytest"),
        false,
        false,
    )
    .expect_err("--test-command does nothing on the raw loop");
    assert!(err.contains("--test-command"), "{err}");
    assert!(err.contains("--pipeline classic"), "{err}");

    let err = reject_verification_flags_without_pipeline(PipelineChoice::Raw, None, true, false)
        .expect_err("--keep-witness does nothing on the raw loop");
    assert!(err.contains("--keep-witness"), "{err}");

    let err = reject_verification_flags_without_pipeline(PipelineChoice::Raw, None, false, true)
        .expect_err("--require-verified does nothing on the raw loop");
    assert!(err.contains("--require-verified"), "{err}");
}

/// **Witness (#3867).** The gate no longer has a bypass arm: `--keep-witness`
/// is refused against **every** [`PipelineChoice`] that exists, with no
/// variant left that accepts it.
///
/// This replaces `verification_flags_are_accepted_against_a_directly_constructed_classic_choice`,
/// which asserted the opposite for the one variant that could accept the flag
/// — `PipelineChoice::Classic`, deleted here. That test could not survive its
/// subject, so the property it guarded is restated as its complement.
///
/// The `match` below is the structural half, and is why this is a witness
/// rather than a restatement: it names every variant with no wildcard, so
/// re-adding a third variant fails to compile here (`E0004`) instead of
/// silently reintroducing an arm that bypasses the refusal. On the pre-#3867
/// code the same `match` does not compile at all — `Classic` is unnamed —
/// which is the fail half of the flip.
#[test]
fn no_pipeline_choice_variant_bypasses_the_verification_flag_gate() {
    for choice in [PipelineChoice::Raw, PipelineChoice::Plugin("budget-v1")] {
        let what = match choice {
            PipelineChoice::Raw => "the raw loop",
            PipelineChoice::Plugin(variant) => variant,
        };
        let err = reject_verification_flags_without_pipeline(choice, None, true, false)
            .expect_err("no choice runs the witness machinery --keep-witness belongs to");
        assert!(err.contains("--keep-witness"), "{what}: {err}");
    }
}

/// A bare raw run with none of the three flags is unaffected — the gate
/// only fires when a flag was actually passed.
#[test]
fn a_bare_raw_run_with_no_verification_flags_is_unaffected() {
    reject_verification_flags_without_pipeline(PipelineChoice::Raw, None, false, false)
        .expect("no verification flag was passed — nothing to refuse");
}

/// `--test-command` is meaningful on a named plugin variant too — it arms
/// the wrapper's own oracle (#3553) — so it is accepted there, while
/// `--keep-witness`/`--require-verified` remain pipeline-only and are
/// still refused, naming `classic` as the remedy.
#[test]
fn plugin_variant_accepts_test_command_but_still_refuses_witness_flags() {
    reject_verification_flags_without_pipeline(
        PipelineChoice::Plugin("budget-v1"),
        Some("pytest"),
        false,
        false,
    )
    .expect("test-command arms the bound wrapper's own oracle");

    let err = reject_verification_flags_without_pipeline(
        PipelineChoice::Plugin("budget-v1"),
        None,
        true,
        false,
    )
    .expect_err("keep-witness is pipeline-only, even under a named variant");
    assert!(err.contains("--keep-witness"), "{err}");
    assert!(err.contains("--pipeline classic"), "{err}");
}

/// A wrapper plugin is a child process the host starts, so the enterprise
/// process-free authority must refuse it exactly as it refused the staged
/// pipeline — and `--pipeline <variant>` must not read as "raw" merely
/// because it names no built-in.
#[test]
fn a_wrapper_plugin_is_not_the_process_free_surface() {
    assert!(PipelineChoice::Raw.is_raw());
    assert!(
        !PipelineChoice::Plugin("budget-v1").is_raw(),
        "a plugin spawns a process, so it is not the surface that spawns none"
    );
    assert!(
        crate::enterprise_telemetry::authorize_execution_surface_with(
            crate::enterprise_telemetry::ExecutionSurface::PipelineOneShot,
            true,
        )
        .is_err(),
        "and that surface is the one process-free authority refuses"
    );
}

/// **Witness (selection).** A plugin installed on disk is found by the
/// variant id `--pipeline` names, its `${plugin_dir}` is interpolated, and
/// the credential its manifest asked for is refused *out loud*.
#[test]
fn an_installed_wrapper_is_bound_by_its_variant_id() {
    // The manifest asks for a credential, so the parent must be carrying
    // one — otherwise an empty `refused` list would prove nothing about the
    // refusal and everything about the fixture.
    let _guard = crate::test_env::lock();
    let _restore = crate::test_env::EnvRestore::capture(&["ANTHROPIC_API_KEY"]);
    // SAFETY: the env lock above is held for the whole mutate-read-restore
    // window, which is what makes this single-threaded with respect to
    // every other env-mutating test in this binary.
    unsafe { std::env::set_var("ANTHROPIC_API_KEY", "sk-not-a-real-key") };

    let roster = roster(vec![installed(
        WRAPPER_MANIFEST,
        "/home/dev/.stella/plugins/budget-keeper",
    )]);
    let mut warnings = Vec::new();
    let wrapper = bound(&roster, "budget-v1", &mut |line| warnings.push(line))
        .expect("the installed plugin declares this variant");
    assert_eq!(wrapper.variant(), "budget-v1");
    assert_eq!(wrapper.dispatch.manifest().name, "budget-keeper");
    assert_eq!(
        warnings.len(),
        1,
        "the refused credential is reported, never silently dropped: {warnings:?}"
    );
    assert!(warnings[0].contains("ANTHROPIC_API_KEY"), "{warnings:?}");
}

/// An unknown variant names what *is* installed rather than failing blank.
#[test]
fn an_unknown_variant_names_the_installed_ones() {
    let roster = roster(vec![installed(WRAPPER_MANIFEST, "/plugins/budget-keeper")]);
    let error = bound(&roster, "vera-v2", &mut |_| {}).expect_err("nothing installed declares it");
    assert!(error.contains("vera-v2"), "{error}");
    assert!(error.contains("budget-v1"), "{error}");

    let nothing = PluginRoster::default();
    let empty = bound(&nothing, "vera-v2", &mut |_| {}).expect_err("none at all");
    assert!(empty.contains("stella plugin list"), "{empty}");
}

/// A manifest that declares `[loop] calls = ["recall"]`.
const RECALLING_MANIFEST: &str = r#"
name = "researcher"
[loop]
participation = "steering"
points = ["before_turn"]
calls = ["recall"]
[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh"]
timeout_secs = 30
[wrapper]
id = "research-v1"
[[wrapper.stages]]
name = "recall"
"#;

/// **Witness (#3561).** Binding an installed wrapper attaches a host-call
/// gate, and a declared `recall` reaches this host's real context plane.
///
/// Before this, `stella-cli` built its transport with
/// `SubprocessWrapper::declare` and bound it straight into the dispatch —
/// no `.serving(..)`, no `HostCallGate` anywhere in the crate — so the
/// plugin's `{"call":"recall",…}` had nowhere to go and `converse` answered
/// `UnannouncedCall`. There was no gate to open, so this test could not be
/// written.
#[tokio::test]
async fn a_declared_recall_reaches_this_hosts_context_plane() {
    use stella_plugin::{HostCallArgs, HostCallOk, HostCallOutcome, RecallArgs};
    use stella_runtime::wrapper::HostCallChannel;

    let roster = roster(vec![installed(RECALLING_MANIFEST, "/plugins/researcher")]);
    let wrapper = bind_installed(&roster, "research-v1", &mut |_| {})
        .expect("the installed plugin declares this variant")
        .serving(WrapperHost::recalling(Box::new(OneFrame)))
        .expect("a found variant binds");

    let channel = wrapper.gate.open();
    let outcome = channel
        .call(HostCallArgs::Recall(RecallArgs {
            goal: "the parser".to_string(),
            limit: None,
        }))
        .await;
    match outcome {
        HostCallOutcome::Ok(HostCallOk::Recall(result)) => {
            assert_eq!(result.frames.len(), 1);
            assert_eq!(result.frames[0].content, "about the parser");
        }
        other => panic!("a declared recall must reach the plane, got {other:?}"),
    }
    assert!(
        wrapper.gate.refusals().is_empty(),
        "nothing was refused, so nothing is reported"
    );
}

/// The gate is attached even when this workspace has no context plane, and
/// an undeclared capability is still refused — *and reported*, which is the
/// half a user can see. An absent gate is the one answer a plugin cannot be
/// given: its call would hang until the point timeout.
#[tokio::test]
async fn a_host_with_no_plane_still_gates_and_reports_what_it_refused() {
    use stella_plugin::{ChildTurnArgs, HostCallArgs, HostCallOutcome, HostCallRefusal};
    use stella_runtime::wrapper::HostCallChannel;

    let roster = roster(vec![installed(RECALLING_MANIFEST, "/plugins/researcher")]);
    let wrapper = bound(&roster, "research-v1", &mut |_| {}).expect("it binds");

    let channel = wrapper.gate.open();
    let undeclared = channel
        .call(HostCallArgs::ChildTurn(ChildTurnArgs {
            role: "verifier".to_string(),
            instruction: "check it".to_string(),
        }))
        .await;
    assert!(
        matches!(
            undeclared,
            HostCallOutcome::Err(ref failure) if failure.refusal == HostCallRefusal::Undeclared
        ),
        "the manifest declares only recall, got {undeclared:?}"
    );
    assert_eq!(
        wrapper.gate.refusals().len(),
        1,
        "a refusal only the plugin learns about is half of \"never silent\""
    );
}

/// A wrapper declaration with no process to ask is refused by name, not
/// driven with an invented default.
#[test]
fn a_wrapper_without_a_runtime_block_is_refused() {
    let no_process = WRAPPER_MANIFEST
        .replace("argv = [\"/bin/sh\", \"${plugin_dir}/main.sh\"]\n", "")
        .replace("timeout_secs = 30\n", "")
        .replace("env = [\"PATH\", \"ANTHROPIC_API_KEY\"]\n", "")
        .replace("[runtime]\n", "");
    let roster = roster(vec![installed(&no_process, "/plugins/budget-keeper")]);
    let error = bound(&roster, "budget-v1", &mut |_| {}).expect_err("no [runtime] block");
    assert!(error.contains("no [runtime] block"), "{error}");
}

/// A manifest that declares `[loop] calls = ["child_turn"]` and two role
/// intents: one resolving to a research seat, one pointed straight at the
/// worker.
///
/// `grader` is declared deliberately. The independence rule has to be
/// tested against a role the *manifest permits*, or it proves only that
/// the manifest check works.
const GRADING_MANIFEST: &str = r#"
name = "grading-wrapper"
[loop]
participation = "steering"
points = ["before_turn"]
calls = ["child_turn"]
[subloop]
stages = ["research"]
[roles.reviewer]
tier = "research"
[roles.grader]
tier = "worker"
[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh"]
timeout_secs = 30
[wrapper]
id = "grading-v1"
[[wrapper.stages]]
name = "execute"
"#;

/// This session's sub-agent dispatcher, standing in for
/// [`crate::subagent::SessionSubAgents`] and recording every
/// [`SubAgentSpec`] it was handed.
///
/// The recording is how these tests answer the question that matters:
/// **who made the model call?** A plugin that had reached for a provider
/// itself would leave nothing here.
#[derive(Default, Clone)]
struct RecordingDispatcher {
    specs: Arc<std::sync::Mutex<Vec<SubAgentSpec>>>,
}

impl RecordingDispatcher {
    fn specs(&self) -> Vec<SubAgentSpec> {
        self.specs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl SubAgentDispatcher for RecordingDispatcher {
    async fn dispatch(&self, spec: SubAgentSpec) -> SubAgentOutcome {
        let summary = format!("read it; asked: {}", spec.instruction);
        self.specs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(spec);
        SubAgentOutcome::Completed(SubAgentReport {
            summary,
            truncated: false,
            cost_usd: 0.02,
            steps: 3,
            absorbed_messages: 7,
        })
    }
}

/// The plugin, this host's dispatcher, and the plane between them — what
/// [`session_host`] assembles on the live path, with a recording
/// dispatcher in place of the session's own.
fn grading_host() -> (BoundWrapper, RecordingDispatcher) {
    let roster = roster(vec![installed(
        GRADING_MANIFEST,
        "/plugins/grading-wrapper",
    )]);
    let manifest = PluginManifest::from_toml_str(GRADING_MANIFEST).expect("fixture must load");
    let dispatcher = RecordingDispatcher::default();
    let plane = Arc::new(child_turn_plane(
        &manifest,
        Arc::new(dispatcher.clone()) as Arc<dyn SubAgentDispatcher>,
    ));
    let wrapper = bind_installed(&roster, "grading-v1", &mut |_| {})
        .expect("the installed plugin declares this variant")
        .serving(WrapperHost::recalling(no_recall()).with_child_turns(plane))
        .expect("a found variant binds");
    (wrapper, dispatcher)
}

fn child_turn(role: &str) -> stella_plugin::HostCallArgs {
    stella_plugin::HostCallArgs::ChildTurn(stella_plugin::ChildTurnArgs {
        role: role.to_string(),
        instruction: "read the diff and say whether the retry survived".to_string(),
    })
}

/// **Witness (#3576).** A declared `child_turn` reaches this host's own
/// sub-agent dispatcher, and the *host* — not the plugin — makes the call.
///
/// Fails before this change for one reason: `stella-cli` assembled its
/// capabilities as `HostPlanes::recalling(..)` and installed no
/// child-turn plane at all, so the only answer this call could get was
/// `Unavailable` (the companion assertion below still proves that is what
/// a host without a plane says). There was no parameter to pass a plane
/// through and no handle to read a spend back from, so this test could not
/// be written.
#[tokio::test]
async fn a_declared_child_turn_runs_on_this_hosts_dispatcher() {
    use stella_plugin::{HostCallOk, HostCallOutcome};
    use stella_runtime::wrapper::HostCallChannel;

    let (wrapper, dispatcher) = grading_host();
    let outcome = wrapper.gate.open().call(child_turn("reviewer")).await;
    match outcome {
        HostCallOutcome::Ok(HostCallOk::ChildTurn(result)) => {
            assert_eq!(result.role, "reviewer");
            assert_eq!(result.seat, "research", "the seat the host resolved it to");
            assert!(result.completed);
            assert!(
                result.report.contains("read the diff"),
                "the child answers the plugin's own instruction: {}",
                result.report
            );
        }
        other => panic!("a declared child turn must reach the plane, got {other:?}"),
    }

    let specs = dispatcher.specs();
    assert_eq!(
        specs.len(),
        1,
        "the host's dispatcher ran the turn — the plugin has no way to run one itself"
    );
    assert_eq!(
        specs[0].role,
        stella_protocol::event::ModelCallRole::Research
    );
    assert!(
        !specs[0].write_access,
        "a plugin's child turn gathers evidence; it does not get a write arm"
    );
    assert_eq!(
        specs[0].turn_instance, PLUGIN_CHILD_TURN_SLOT,
        "not the parent's receipt slot"
    );

    let spends = wrapper.child_spends();
    assert_eq!(
        spends.len(),
        1,
        "what a plugin spent is readable after the run"
    );
    assert_eq!(
        spends[0].seat,
        stella_protocol::event::ModelCallRole::Research
    );
    let line = spend_lines(&spends).remove(0);
    assert!(line.contains("reviewer"), "{line}");
    assert!(
        line.contains("0.02"),
        "the user is told what it cost: {line}"
    );
}

/// A provider that never answers — the candidate turns below are about the
/// *wiring*, not about what a model would say.
struct NeverProvider;

#[async_trait]
impl stella_protocol::Provider for NeverProvider {
    fn id(&self) -> &str {
        "never"
    }
    async fn complete_ref(
        &self,
        _request: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<stella_protocol::CompletionResult, stella_protocol::ProviderError> {
        Err(stella_protocol::ProviderError::Terminal(
            "no provider in tests".into(),
        ))
    }
}

/// A manifest that asks for both halves of best-of-N at the worker tier.
const CANDIDATES_MANIFEST: &str = r#"
name = "candidates-wrapper"
[loop]
participation = "steering"
points = ["before_turn"]
calls = ["candidate_fanout", "adopt_candidate"]
max_fanout_width = 2
[subloop]
stages = ["research"]
[roles.attempt]
tier = "worker"
[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh"]
timeout_secs = 30
[wrapper]
id = "candidates-v1"
[[wrapper.stages]]
name = "research"
"#;

/// **Witness (#3892).** `session_host` installs a candidate fan-out plane, so
/// a `stella run --pipeline <variant>` really can fan out.
///
/// Asserted through `session_host` → `serving` → the gate rather than by
/// building `HostPlanes` by hand, because the regression this guards is
/// exactly a missing `.with_candidate_fanout(..)` at that one call site: a
/// hand-built plane would still pass while every shipped run answered
/// `Unavailable`. The companion assertion below is the anti-vacuity half —
/// a host assembled without one really does say `Unavailable`, so the first
/// assertion is about the wiring and not about the gate being permissive.
#[tokio::test]
async fn a_stella_run_host_serves_candidate_fanout() {
    use stella_plugin::{HostCallOutcome, HostCallRefusal};
    use stella_runtime::wrapper::HostCallChannel;

    let repo = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "a@b.invalid"],
        vec!["config", "user.name", "t"],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(&args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    std::fs::write(repo.path().join("seed.txt"), "seed\n").unwrap();
    for args in [vec!["add", "-A"], vec!["commit", "-qm", "seed"]] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(&args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }

    let mut cfg =
        crate::config::Config::for_tests(crate::config::PROVIDERS[0].clone(), "m".to_string());
    cfg.workspace_root = repo.path().to_path_buf();
    let registry = Arc::new(stella_tools::ToolRegistry::new(repo.path().to_path_buf()));
    let sub_agents = Arc::new(crate::subagent::SessionSubAgents::new(
        Arc::new(NeverProvider),
        &registry,
        stella_core::EngineConfig::default(),
        stella_protocol::BudgetMode::Observed,
    ));
    std::mem::forget(registry);

    let roster = roster(vec![installed(
        CANDIDATES_MANIFEST,
        "/plugins/candidates-wrapper",
    )]);
    let resolved = bind_installed(&roster, "candidates-v1", &mut |_| {})
        .expect("the installed plugin declares this variant");
    let manifest = resolved.manifest().clone();
    let wrapper = resolved
        .serving(super::session_host(&cfg, &manifest, sub_agents))
        .expect("a found variant binds");

    let outcome = wrapper
        .gate
        .open()
        .call(stella_plugin::HostCallArgs::CandidateFanout(
            stella_plugin::CandidateFanoutArgs {
                role: "attempt".to_string(),
                instruction: "try it".to_string(),
                width: 1,
            },
        ))
        .await;
    // The provider never answers, so the candidate's *turn* fails — but the
    // capability was reached, which is the claim. `Unavailable` here would
    // mean no plane was installed at all.
    assert!(
        !matches!(
            outcome,
            HostCallOutcome::Err(ref failure)
                if failure.refusal == HostCallRefusal::Unavailable
        ),
        "`session_host` must install a fan-out plane, got {outcome:?}"
    );
    assert!(
        !wrapper.fanout_spends().is_empty(),
        "and what it bought must be readable back for the run's report"
    );

    // Anti-vacuity: the same plugin on a host assembled without the plane is
    // told `Unavailable`, so the assertion above is about the wiring.
    let bare = bind_installed(&roster, "candidates-v1", &mut |_| {})
        .expect("the installed plugin declares this variant")
        .serving(WrapperHost::recalling(no_recall()))
        .expect("a found variant binds");
    assert!(
        matches!(
            bare.gate
                .open()
                .call(stella_plugin::HostCallArgs::CandidateFanout(
                    stella_plugin::CandidateFanoutArgs {
                        role: "attempt".to_string(),
                        instruction: "try it".to_string(),
                        width: 1,
                    },
                ))
                .await,
            HostCallOutcome::Err(ref failure)
                if failure.refusal == HostCallRefusal::Unavailable
        ),
        "a host with no substrate declares the gap rather than pretending"
    );

    // Nothing is left on disk: the sweep is what a wrapped run calls.
    let leaked = wrapper.sweep_candidates().await;
    assert!(
        leaked.is_empty(),
        "the sweep must take every workspace: {leaked:?}"
    );
}

/// A fan-out's spend is printed, and the clamp is printed with it (#3892).
///
/// The largest spend a plugin can cause on one host call — N *writing* worker
/// turns — so a run that reported child turns and stayed silent here would be
/// visible about the cheap spend and quiet about the expensive one. The
/// requested width appears only when it differs from what ran, because "asked
/// 5, ran 3" and "asked 3, ran 3" are different facts about a plugin and only
/// the host knows which one happened.
#[test]
fn a_fanout_reports_what_it_bought_and_names_a_clamp() {
    use stella_protocol::event::ModelCallRole;
    use stella_runtime::wrapper::CandidateFanoutSpend;

    let clamped = super::fanout_spend_lines(&[CandidateFanoutSpend {
        role: "attempt".into(),
        seat: ModelCallRole::Worker,
        requested_width: 5,
        width: 3,
        cost_usd: 0.4200,
        completed: 2,
    }])
    .remove(0);
    assert!(clamped.contains("3 candidate turn(s)"), "{clamped}");
    assert!(clamped.contains("attempt"), "{clamped}");
    assert!(clamped.contains("0.4200"), "the money is named: {clamped}");
    assert!(clamped.contains("2 finished"), "{clamped}");
    assert!(
        clamped.contains("asked for 5"),
        "a clamp the plugin could not see is a clamp it will report as its \
         own choice: {clamped}"
    );

    let unclamped = super::fanout_spend_lines(&[CandidateFanoutSpend {
        role: "attempt".into(),
        seat: ModelCallRole::Worker,
        requested_width: 3,
        width: 3,
        cost_usd: 0.1,
        completed: 3,
    }])
    .remove(0);
    assert!(
        !unclamped.contains("asked for"),
        "nothing was clamped, so nothing is said about it: {unclamped}"
    );

    assert!(
        super::fanout_spend_lines(&[]).is_empty(),
        "a plugin that never fanned out keeps a silent run silent"
    );
}

/// The independence rule survives the wiring: a role intent the manifest
/// declares but that resolves to the **worker's** seat is refused, and
/// refused as `Forbidden` — "you may not have this" — rather than as the
/// weaker `Unavailable` that would read as a configuration accident.
#[tokio::test]
async fn a_role_intent_pointing_at_the_worker_is_still_forbidden() {
    use stella_plugin::{HostCallOutcome, HostCallRefusal};
    use stella_runtime::wrapper::HostCallChannel;

    let (wrapper, dispatcher) = grading_host();
    let outcome = wrapper.gate.open().call(child_turn("grader")).await;
    assert!(
        matches!(
            outcome,
            HostCallOutcome::Err(ref failure)
                if failure.refusal == HostCallRefusal::Forbidden
        ),
        "a plugin may not spend the model whose work it judges, got {outcome:?}"
    );
    assert!(
        dispatcher.specs().is_empty(),
        "a refusal costs no model call"
    );
    assert_eq!(
        wrapper.gate.refusals().len(),
        1,
        "and the user is told, not only the plugin"
    );
    assert!(wrapper.child_spends().is_empty());
}

/// The shipped `plugins/stella-goal` manifest, read off disk.
///
/// Deliberately the real file rather than a fixture string: #3838's whole
/// failure mode was that a fixture could be made to pass while the plugin
/// that ships in this repository stayed inert, and nobody would notice. If
/// this manifest stops declaring `[roles.verifier]`, this test must fail.
fn shipped_goal_manifest() -> PluginManifest {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/stella-goal/plugin.toml");
    let text = std::fs::read_to_string(&path)
        .expect("the first-party goal plugin ships at plugins/stella-goal/plugin.toml");
    PluginManifest::from_toml_str(&text).expect("the shipped manifest loads")
}

/// **Witness (#3838).** The `verifier` role intent that
/// `plugins/stella-goal` declares resolves on the plane
/// [`child_turn_plane`] actually builds — the production wiring, not a
/// test-only `.with_seat` call.
///
/// Fails before this change with `HostCallRefusal::Unavailable`:
/// `ChildTurns::default_seats()` serves `worker`/`triage`/`research`/`plan`
/// and not `verifier`, and this host bound no seat of its own. The
/// consequence was not a degraded run but an inert plugin — every
/// `after_turn` refused, evidence always empty, `judge` abstaining on
/// `MeasurementMissing`, and the loop ending `Undecided` after exactly one
/// round on every run, forever.
///
/// Asserts the seat is `verdict` and not merely "something resolved",
/// because the attribution is the claim this host is owning: a plugin's
/// verifier turn is booked against the same responsibility
/// `stella_core::goal` books its own independent verifier call against.
#[tokio::test]
async fn the_shipped_goal_plugins_verifier_intent_resolves_on_this_hosts_plane() {
    use stella_plugin::ChildTurnArgs;
    use stella_runtime::wrapper::ChildTurnPlane;

    let manifest = shipped_goal_manifest();
    assert_eq!(
        manifest
            .roles
            .as_ref()
            .and_then(|roles| roles.get("verifier"))
            .map(|role| role.tier.as_str()),
        Some("verifier"),
        "the shipped plugin declares [roles.verifier] tier = \"verifier\"; \
         if that changed, this witness is testing nothing"
    );

    let dispatcher = RecordingDispatcher::default();
    let plane = child_turn_plane(
        &manifest,
        Arc::new(dispatcher.clone()) as Arc<dyn SubAgentDispatcher>,
    );

    let result = plane
        .child_turn(ChildTurnArgs {
            role: "verifier".to_string(),
            instruction: "assess whether the goal was accomplished".to_string(),
        })
        .await
        .expect("the shipped host serves the shipped plugin's one role intent");

    assert_eq!(result.role, "verifier", "the plugin's own intent, echoed");
    assert_eq!(
        result.seat, "verdict",
        "booked against the responsibility stella_core::goal books its own \
         verifier call against, so the spend is visible as what it is"
    );
    assert!(
        result.report.contains("assess whether the goal"),
        "the child answered the plugin's instruction: {}",
        result.report
    );

    let specs = dispatcher.specs();
    assert_eq!(
        specs.len(),
        1,
        "the HOST made the model call — a plugin that reached for a provider \
         itself would leave nothing here"
    );
    assert_eq!(
        specs[0].role,
        stella_protocol::event::ModelCallRole::Verdict,
        "and the receipt says so"
    );
    assert!(
        !specs[0].write_access,
        "a verifier gathers evidence about the work; it does not get a write arm"
    );
}

/// The same plugin against a host that installs no plane is answered
/// `Unavailable` — the state every driver was in before this change, and
/// the one a driver with no dispatcher of its own is still in.
#[tokio::test]
async fn a_host_with_no_child_turn_plane_answers_unavailable() {
    use stella_plugin::{HostCallOutcome, HostCallRefusal};
    use stella_runtime::wrapper::HostCallChannel;

    let roster = roster(vec![installed(
        GRADING_MANIFEST,
        "/plugins/grading-wrapper",
    )]);
    let wrapper = bound(&roster, "grading-v1", &mut |_| {}).expect("it binds");
    let outcome = wrapper.gate.open().call(child_turn("reviewer")).await;
    assert!(
        matches!(
            outcome,
            HostCallOutcome::Err(ref failure)
                if failure.refusal == HostCallRefusal::Unavailable
        ),
        "no plane means a told gap, never a silence: {outcome:?}"
    );
    assert!(wrapper.child_spends().is_empty());
}

/// **The end-to-end witness (#3576).** A real `sh` plugin, started by this
/// driver from what is installed on disk, asks the host for a model call at
/// a role intent it declared — and the host's own dispatcher makes it,
/// through the whole sequence a `stella run --pipeline <variant>` drives.
///
/// The plugin is a shell script with no JSON library, for
/// `stella-runtime`'s reason: a capability only Rust can reach is a Rust
/// API with extra steps (`doc:pipeline-as-plugins` §5 commitment 2). The
/// one thing stubbed is the engine — [`TurnDriver`] is the seam the socket
/// is designed around, and `run_raw_one_shot` supplies the real one — so
/// everything between the plugin's stdout and the dispatcher's spec is the
/// shipping path.
///
/// `cfg(unix)` is the same declared gap the runtime's own socket suite
/// carries (#3497).
#[cfg(unix)]
#[tokio::test]
async fn an_installed_sh_plugin_spends_a_child_turn_through_this_driver() {
    use stella_plugin::{TamperFinding, TurnOutcome};
    use stella_runtime::wrapper::{DrivenTurn, TurnPrelude};

    /// The engine, stubbed: it records what the wrapper contributed and
    /// reports a completed turn.
    #[derive(Default)]
    struct StubTurn {
        contributed: Vec<String>,
    }

    #[async_trait(?Send)]
    impl TurnDriver for StubTurn {
        async fn run_turn(&mut self, prelude: TurnPrelude) -> DrivenTurn {
            self.contributed.extend(
                prelude
                    .into_messages()
                    .into_iter()
                    .map(|message| message.content),
            );
            DrivenTurn {
                outcome: TurnOutcome {
                    completed: true,
                    answer: "the retry is back".to_string(),
                    tools: Some(Vec::new()),
                    changed_files: Some(Vec::new()),
                },
                tamper: TamperFinding::NotChecked,
            }
        }
    }

    let dir = tempfile::tempdir().expect("a temp dir for the installed plugin");
    std::fs::write(
        dir.path().join("main.sh"),
        r#"
read -r request
printf '%s\n' '{"call":"child_turn","id":7,"args":{"role":"reviewer","instruction":"does the diff drop the retry?"}}'
read -r answer
case "$answer" in
  *'"seat":"research"'*) finding="the reviewer read it at the research seat" ;;
  *) finding="no assessment was available" ;;
esac
printf '{"point":"before_turn","body":{"protocol_version":1,"context":[{"label":"reviewer","text":"%s"}]}}\n' "$finding"
"#,
    )
    .expect("the plugin script is written");

    let manifest = PluginManifest::from_toml_str(GRADING_MANIFEST).expect("fixture must load");
    let dispatcher = RecordingDispatcher::default();
    let plane = Arc::new(child_turn_plane(
        &manifest,
        Arc::new(dispatcher.clone()) as Arc<dyn SubAgentDispatcher>,
    ));
    let roster = roster(vec![installed(
        GRADING_MANIFEST,
        &dir.path().to_string_lossy(),
    )]);
    let wrapper = bind_installed(&roster, "grading-v1", &mut |_| {})
        .expect("the installed plugin declares this variant")
        .serving(WrapperHost::recalling(no_recall()).with_child_turns(plane))
        .expect("it binds");

    let mut driver = StubTurn::default();
    let report = wrapper
        .dispatch
        .run(
            RoundInput {
                goal: "put the retry back".to_string(),
                signals: pre_turn_signals(false, false),
                candidate: None,
            },
            &mut driver,
        )
        .await
        .expect("the declared stage program resolves");

    assert_eq!(report.variant, "grading-v1");
    assert!(
        report.faults.is_empty(),
        "the plugin answered every point it declared: {:?}",
        report.faults
    );
    assert_eq!(
        driver.contributed,
        vec!["the reviewer read it at the research seat".to_string()],
        "a real turn's result reached the plugin and became the turn's context"
    );

    let specs = dispatcher.specs();
    assert_eq!(specs.len(), 1, "the host made the call, not the plugin");
    assert_eq!(
        specs[0].role,
        stella_protocol::event::ModelCallRole::Research
    );
    assert_eq!(specs[0].instruction, "does the diff drop the retry?");
    assert!(!specs[0].write_access);
    assert_eq!(wrapper.child_spends().len(), 1);
}

/// A dispatcher that answers a child the way [`crate::subagent::SessionSubAgents`]
/// does — by reading the *current* turn's controls off the registry at dispatch
/// time and refusing to spend when the turn it belongs to has already been
/// stopped.
///
/// It reads the same slot through the same accessor as the shipping dispatcher
/// (`ToolRegistry::turn_controls`), so what it observes is exactly what the
/// real one observes; what it does with a latched stop is a two-line stand-in
/// for the child engine, whose own honouring of these seams is witnessed in
/// `crate::subagent::tests`.
struct StopAwareDispatcher {
    registry: Arc<stella_tools::ToolRegistry>,
}

#[async_trait]
impl SubAgentDispatcher for StopAwareDispatcher {
    async fn dispatch(&self, _spec: SubAgentSpec) -> SubAgentOutcome {
        let controls = self.registry.turn_controls();
        let stopped = controls
            .steering
            .as_ref()
            .is_some_and(|tap| tap.soft_stop_requested());
        if stopped {
            return SubAgentOutcome::Incomplete {
                report: SubAgentReport {
                    summary: String::new(),
                    truncated: false,
                    cost_usd: 0.0,
                    steps: 0,
                    absorbed_messages: 0,
                },
                reason: stella_core::driver::SOFT_STOP_REASON.to_string(),
            };
        }
        SubAgentOutcome::Completed(SubAgentReport {
            summary: "spent the model on a stopped turn".to_string(),
            truncated: false,
            cost_usd: 0.02,
            steps: 3,
            absorbed_messages: 7,
        })
    }
}

/// **Witness (#3803).** A wrapped turn publishes its controls for the span of
/// the dispatch, so the child a *plugin* asks for honours the stop that governs
/// the parent.
///
/// Fails before this change with the child reporting `Completed`: `RawTurnDriver`
/// attached nothing, so `ToolRegistry::turn_controls` answered
/// `TurnControls::none()` and the dispatcher had no stop to see —
/// [`SubAgentDispatcher`]'s contract broken at the one driver that never held up
/// its end. It is the #922 shape one layer out: the parent is stopped, and the
/// plugin's child runs on.
///
/// The plugin issues its `child_turn` from `before_turn` — a point, which runs
/// *between* the rounds — which is why the guard is held across the dispatch and
/// not merely across `run_turn`. A round-scoped guard passes nothing here.
///
/// The engine is the one thing stubbed, exactly as in the sibling witness above
/// and for the same reason: [`TurnDriver`] is the seam the socket is designed
/// around, and everything between the plugin's stdout and what the dispatcher
/// reads is the shipping path.
#[cfg(unix)]
#[tokio::test]
async fn a_stopped_wrapped_turn_stops_the_child_its_plugin_asked_for() {
    use stella_plugin::{TamperFinding, TurnOutcome};
    use stella_runtime::wrapper::{DrivenTurn, TurnPrelude};

    /// The engine, stubbed — this witness is about what the child sees, not
    /// about what the turn answered.
    struct StubTurn;

    #[async_trait(?Send)]
    impl TurnDriver for StubTurn {
        async fn run_turn(&mut self, _prelude: TurnPrelude) -> DrivenTurn {
            DrivenTurn {
                outcome: TurnOutcome {
                    completed: true,
                    answer: "done".to_string(),
                    tools: Some(Vec::new()),
                    changed_files: Some(Vec::new()),
                },
                tamper: TamperFinding::NotChecked,
            }
        }
    }

    let dir = tempfile::tempdir().expect("a temp dir for the installed plugin");
    // The plugin asks for a child and reports back whether it ran, so the
    // assertion below reads the plugin's own view rather than the host's.
    std::fs::write(
        dir.path().join("main.sh"),
        r#"
read -r request
printf '%s\n' '{"call":"child_turn","id":7,"args":{"role":"reviewer","instruction":"read the diff"}}'
read -r answer
case "$answer" in
  *'"completed":false'*) finding="the child stopped with its parent" ;;
  *) finding="the child ran on" ;;
esac
printf '{"point":"before_turn","body":{"protocol_version":1,"context":[{"label":"reviewer","text":"%s"}]}}\n' "$finding"
"#,
    )
    .expect("the plugin script is written");

    let registry = Arc::new(stella_tools::ToolRegistry::new(PathBuf::from(".")));
    let manifest = PluginManifest::from_toml_str(GRADING_MANIFEST).expect("fixture must load");
    let plane = Arc::new(child_turn_plane(
        &manifest,
        Arc::new(StopAwareDispatcher {
            registry: Arc::clone(&registry),
        }) as Arc<dyn SubAgentDispatcher>,
    ));
    let roster = roster(vec![installed(
        GRADING_MANIFEST,
        &dir.path().to_string_lossy(),
    )]);
    let wrapper = bind_installed(&roster, "grading-v1", &mut |_| {})
        .expect("the installed plugin declares this variant")
        .serving(WrapperHost::recalling(no_recall()).with_child_turns(plane))
        .expect("it binds");

    // The turn is stopped before it is driven — the state a user leaves behind
    // by pressing Esc, latched by contract so every boundary after it sees it.
    let tap: Arc<crate::subsession::SteeringTap> = Arc::default();
    tap.request_soft_stop();
    let controls = stella_core::ports::TurnControls::none().with_steering(tap);

    let mut driver = StubTurn;
    let report = super::dispatch_under_turn_controls(
        &wrapper.dispatch,
        RoundInput {
            goal: "put the retry back".to_string(),
            signals: pre_turn_signals(false, false),
            candidate: None,
        },
        &registry,
        controls,
        &mut driver,
    )
    .await
    .expect("the declared stage program resolves");

    assert!(report.faults.is_empty(), "{:?}", report.faults);
    assert_eq!(
        wrapper.child_spends().len(),
        1,
        "the child was dispatched — this is about what it did, not whether it ran"
    );
    assert!(
        !wrapper.child_spends()[0].completed,
        "a stopped parent must not keep spending inside the child its plugin asked for"
    );

    assert!(
        registry.turn_controls().is_empty(),
        "and the guard comes down with the dispatch, so the stop cannot latch \
         onto the children of a turn that has not started yet"
    );
}

/// **Witness (#3576, requirement 3).** What a plugin's last point spent
/// lands on the session's guard.
///
/// The dispatcher settles every child onto the registry's ledger and an
/// engine turn drains it at its next step boundary — but the last point of
/// the last round has no next boundary, so before this change those
/// dollars reached no guard at all. Draining is destructive, so the second
/// call proves it folds exactly once.
#[test]
fn a_plugins_residual_child_spend_folds_into_the_sessions_guard() {
    let registry = stella_tools::ToolRegistry::new(PathBuf::from("."));
    stella_core::subagent::push_sub_agent_spend(&registry.sub_agent_spend_ledger(), 0.25);
    let mut budget = BudgetGuard::new(stella_protocol::BudgetMode::Enforced, None, Some(10.0));

    let folded = settle_plugin_child_spend(&registry, &mut budget);
    assert!((folded - 0.25).abs() < f64::EPSILON, "{folded}");
    assert!((budget.session_spent_usd() - 0.25).abs() < f64::EPSILON);
    assert_eq!(
        settle_plugin_child_spend(&registry, &mut budget),
        0.0,
        "the ledger is drained, so a second fold charges nothing twice"
    );
}

/// The pre-turn snapshot answers every signal, and answers the post-turn
/// ones with what is true before anything has run.
#[test]
fn the_pre_turn_snapshot_states_only_what_is_true_yet() {
    let signals = pre_turn_signals(true, false);
    assert!(signals.test_command);
    assert!(!signals.budget_metered);
    assert_eq!(signals.candidates, 1);
    assert_eq!(signals.mutating_actions, 0);
    assert_eq!(signals.diff_lines, 0);
    assert!(!signals.flip_achieved);
    assert!(!signals.tests_red && !signals.tests_green);
}
