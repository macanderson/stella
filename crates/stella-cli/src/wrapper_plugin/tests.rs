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
    assert_eq!(PipelineChoice::resolve(false, None), PipelineChoice::Raw);
    assert_eq!(
        PipelineChoice::resolve(true, None),
        PipelineChoice::Raw,
        "--no-pipeline is a deprecated no-op: it names the same choice as no flag at all"
    );
}

/// `classic` is still selectable by the id it records, and an unknown
/// variant still binds a plugin lookup rather than the built-in.
#[test]
fn pipeline_variant_selects_classic_or_a_plugin_by_name() {
    assert_eq!(
        PipelineChoice::resolve(false, Some("classic")),
        PipelineChoice::Classic,
        "the built-in is selectable by the id it records"
    );
    assert_eq!(
        PipelineChoice::resolve(false, Some("budget-v1")),
        PipelineChoice::Plugin("budget-v1")
    );
}

/// **Witness (#3381).** `--no-pipeline` together with `--pipeline` used to
/// be a hard error (`conflicts_with` in clap, then an `Err` from
/// `resolve`). This assertion fails on the pre-#3381 code (which returns
/// `Err` here) and passes on this one: a deprecated no-op flag must not
/// veto an explicit `--pipeline` opt-in, on either variant arm.
#[test]
fn no_pipeline_no_longer_vetoes_an_explicit_pipeline_choice() {
    assert_eq!(
        PipelineChoice::resolve(true, Some("budget-v1")),
        PipelineChoice::Plugin("budget-v1"),
        "--pipeline wins outright; the deprecated flag has nothing left to veto"
    );
    assert_eq!(
        PipelineChoice::resolve(true, Some("classic")),
        PipelineChoice::Classic
    );
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

/// **Witness (#3695, fleet half).** `stella fleet` cannot drive a wrapper
/// plugin yet — it has no [`TurnDriver`] over its own worker-attempt round
/// loop — so a named `--pipeline <variant>` must still be refused there
/// rather than silently downgraded to raw or promoted to classic. This
/// assertion fails on code that has no such gate at all (every door would
/// accept-and-ignore the variant) and passes on this one.
#[test]
fn a_named_plugin_variant_is_refused_on_fleet() {
    let err = reject_plugin_variant_for_door("fleet", PipelineChoice::Plugin("budget-v1"))
        .expect_err("fleet has no wrapper driver yet");
    assert!(err.contains("budget-v1"), "{err}");
    assert!(err.contains("stella fleet"), "{err}");
    assert!(
        err.contains("stella run --pipeline budget-v1")
            && err.contains("stella goal --pipeline budget-v1"),
        "the refusal must name the doors that CAN run it: {err}"
    );
}

/// **Witness (#3695, goal half).** `stella goal` now has a real
/// [`TurnDriver`] (`crate::agent::goal_wrapped`), so a named `--pipeline
/// <variant>` is no longer refused there — it used to be, unconditionally,
/// before this change. This assertion fails against that code (which
/// returns `Err` for every plugin variant on `goal`) and passes on this one.
#[test]
fn a_named_plugin_variant_is_accepted_on_goal() {
    reject_plugin_variant_for_door("goal", PipelineChoice::Plugin("budget-v1"))
        .expect("goal now drives a wrapper plugin per round");
}

/// `classic` and no flag at all both resolve away from `Plugin` before
/// reaching the gate, so neither is refused anywhere — only a *named*
/// variant is ever in reach of the gate, and only on `fleet`.
#[test]
fn classic_and_raw_are_never_refused_on_any_door() {
    reject_plugin_variant_for_door("goal", PipelineChoice::Classic)
        .expect("classic has no plugin to drive — nothing to refuse");
    reject_plugin_variant_for_door("goal", PipelineChoice::Raw)
        .expect("raw has no plugin to drive — nothing to refuse");
    reject_plugin_variant_for_door("fleet", PipelineChoice::Classic)
        .expect("classic has no plugin to drive — nothing to refuse");
    reject_plugin_variant_for_door("fleet", PipelineChoice::Raw)
        .expect("raw has no plugin to drive — nothing to refuse");
}

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

/// The same three flags are accepted once `--pipeline classic` selects
/// the staged pipeline — the refusal only fires against a resolution
/// that cannot honor the flag, never against `Classic` itself.
#[test]
fn verification_flags_are_accepted_with_pipeline_classic() {
    reject_verification_flags_without_pipeline(PipelineChoice::Classic, Some("pytest"), true, true)
        .expect("classic runs the verification machinery these flags belong to");
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
/// process-free authority must refuse it exactly as it refuses the staged
/// pipeline — and `--pipeline <variant>` must not read as "raw" merely
/// because it is not `classic`.
#[test]
fn a_wrapper_plugin_is_not_the_process_free_surface() {
    assert!(PipelineChoice::Raw.is_raw());
    assert!(!PipelineChoice::Classic.is_raw());
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
