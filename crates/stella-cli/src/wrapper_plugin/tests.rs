// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for the CLI's wrapper-socket driver — variant resolution, the
//! verification flags each resolution accepts or refuses, and the host planes
//! (`recall`, `child_turn`) an installed wrapper is bound with.
//!
//! Split out of `wrapper_plugin.rs` rather than grown inside it: the parent
//! crossed the 1500-line ratchet (AGENTS.md § "God files"), and a `tests`
//! module in its own file is this crate's established shape for that —
//! `crate::subagent::tests` and `crate::plugin_cmd::tests` are the same move.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::*;
use crate::plugin_cmd::roster::{InstalledPlugin, PluginScope};
use stella_plugin::PluginManifest;

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

/// A dispatcher that records every spec it was handed and answers with a
/// fixed report, so a test can tell "the gate reached this host's real
/// dispatcher" from "the gate refused" — the `child_turn` analogue of
/// [`OneFrame`] for recall.
#[derive(Default, Clone)]
struct RecordingSubAgents {
    specs: Arc<std::sync::Mutex<Vec<SubAgentSpec>>>,
}

#[async_trait]
impl SubAgentDispatcher for RecordingSubAgents {
    async fn dispatch(&self, spec: SubAgentSpec) -> SubAgentOutcome {
        let answer = format!("answered: {}", spec.instruction);
        self.specs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(spec);
        SubAgentOutcome::Completed(stella_core::subagent::SubAgentReport {
            summary: answer,
            truncated: false,
            cost_usd: 0.0,
            steps: 1,
            absorbed_messages: 0,
        })
    }
}

/// A dispatcher no test below asks anything of — most fixtures only need
/// `bind_installed` to have *some* dispatcher to hand `ChildTurns`, the
/// same way most of them pass `no_recall()`.
fn stub_sub_agents() -> Arc<dyn SubAgentDispatcher> {
    Arc::new(RecordingSubAgents::default())
}

fn bound(
    roster: &PluginRoster,
    variant: &str,
    warn: &mut dyn FnMut(String),
) -> Result<BoundWrapper, String> {
    bind_installed(roster, variant, no_recall(), stub_sub_agents(), warn)
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

/// **Witness (#3695).** `stella goal`/`stella fleet` cannot drive a
/// wrapper plugin today — only `stella run` implements [`TurnDriver`] over
/// one — so a named `--pipeline <variant>` must be refused on those doors
/// rather than silently downgraded to raw or promoted to classic. This
/// assertion fails on code that has no such gate at all (every door would
/// accept-and-ignore the variant) and passes on this one.
#[test]
fn a_named_plugin_variant_is_refused_on_a_door_that_cannot_drive_one() {
    let err = reject_plugin_variant_for_door("goal", PipelineChoice::Plugin("budget-v1"))
        .expect_err("goal has no wrapper driver");
    assert!(err.contains("budget-v1"), "{err}");
    assert!(err.contains("stella goal"), "{err}");
    assert!(
        err.contains("stella run --pipeline budget-v1"),
        "the refusal must name the door that CAN run it: {err}"
    );

    let err = reject_plugin_variant_for_door("fleet", PipelineChoice::Plugin("budget-v1"))
        .expect_err("fleet has no wrapper driver either");
    assert!(err.contains("stella fleet"), "{err}");
}

/// `classic` and no flag at all both resolve away from `Plugin` before
/// reaching the gate, so neither is refused on a door with no wrapper
/// driver — only a *named* variant is out of reach there.
#[test]
fn classic_and_raw_are_never_refused_on_a_door_with_no_wrapper_driver() {
    reject_plugin_variant_for_door("goal", PipelineChoice::Classic)
        .expect("classic has no plugin to drive — nothing to refuse");
    reject_plugin_variant_for_door("goal", PipelineChoice::Raw)
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
    let wrapper = bind_installed(
        &roster,
        "research-v1",
        Box::new(OneFrame),
        stub_sub_agents(),
        &mut |_| {},
    )
    .expect("the installed plugin declares this variant");

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

/// A manifest that declares `[loop] calls = ["child_turn"]` and one role
/// intent — `[roles]` requires `[subloop]` to validate
/// (`ManifestError::RolesRequireSubloop`), so this carries one even though
/// `child_turn` and `[subloop]`'s own bounded-turn stages are different
/// mechanisms that merely share the `[roles]` table.
const CHILD_TURN_MANIFEST: &str = r#"
name = "reviewer"
[loop]
participation = "steering"
points = ["after_turn"]
calls = ["child_turn"]
[runtime]
argv = ["/bin/sh", "${plugin_dir}/main.sh"]
timeout_secs = 30
[wrapper]
id = "reviewer-v1"
[[wrapper.stages]]
name = "execute"

[subloop]
stages = ["research"]

[roles.reviewer]
tier = "research"
"#;

/// **Witness (this change).** Binding an installed wrapper attaches a
/// child-turn plane over this session's own sub-agent dispatcher, and a
/// declared role intent spends a real bounded child turn through it —
/// the `child_turn` analogue of `a_declared_recall_reaches_this_hosts_context_plane`.
///
/// Before this, `bind_installed` built `HostPlanes::recalling(..)` with no
/// `.with_child_turns(..)` at all, so every `child_turn` ask answered
/// `Unavailable` regardless of what the manifest declared (#3576). This
/// assertion fails on that code (the match arm below would see `Err` with
/// `HostCallRefusal::Unavailable`, never `Ok`) and passes on this one.
#[tokio::test]
async fn a_declared_child_turn_reaches_this_hosts_dispatcher() {
    use stella_plugin::{ChildTurnArgs, HostCallArgs, HostCallOk, HostCallOutcome};
    use stella_runtime::wrapper::HostCallChannel;

    let roster = roster(vec![installed(CHILD_TURN_MANIFEST, "/plugins/reviewer")]);
    let sub_agents = RecordingSubAgents::default();
    let wrapper = bind_installed(
        &roster,
        "reviewer-v1",
        no_recall(),
        Arc::new(sub_agents.clone()),
        &mut |_| {},
    )
    .expect("the installed plugin declares this variant");

    let channel = wrapper.gate.open();
    let outcome = channel
        .call(HostCallArgs::ChildTurn(ChildTurnArgs {
            role: "reviewer".to_string(),
            instruction: "does the diff drop the retry?".to_string(),
        }))
        .await;
    match outcome {
        HostCallOutcome::Ok(HostCallOk::ChildTurn(result)) => {
            assert_eq!(result.role, "reviewer");
            assert_eq!(result.seat, "research", "the declared tier's resolved seat");
            assert_eq!(result.report, "answered: does the diff drop the retry?");
            assert!(result.completed);
        }
        other => panic!("a declared child_turn must reach the dispatcher, got {other:?}"),
    }
    assert!(
        wrapper.gate.refusals().is_empty(),
        "nothing was refused, so nothing is reported"
    );

    let specs = sub_agents
        .specs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(
        specs.len(),
        1,
        "exactly one call, and this session's real dispatcher made it"
    );
    assert!(
        !specs[0].write_access,
        "a plugin's child turn is read-only, enforced at execution"
    );
}

/// A role the manifest never declared is refused before this host's
/// dispatcher is ever touched — `ChildTurns::resolve`'s own contract,
/// exercised here through the real gate this driver assembles rather than
/// through `stella-runtime`'s unit tests alone.
#[tokio::test]
async fn an_undeclared_role_intent_is_refused_before_the_dispatcher_runs() {
    use stella_plugin::{ChildTurnArgs, HostCallArgs, HostCallOutcome, HostCallRefusal};
    use stella_runtime::wrapper::HostCallChannel;

    let roster = roster(vec![installed(CHILD_TURN_MANIFEST, "/plugins/reviewer")]);
    let sub_agents = RecordingSubAgents::default();
    let wrapper = bind_installed(
        &roster,
        "reviewer-v1",
        no_recall(),
        Arc::new(sub_agents.clone()),
        &mut |_| {},
    )
    .expect("it binds");

    let channel = wrapper.gate.open();
    let refused = channel
        .call(HostCallArgs::ChildTurn(ChildTurnArgs {
            role: "auditor".to_string(),
            instruction: "check it".to_string(),
        }))
        .await;
    assert!(
        matches!(
            refused,
            HostCallOutcome::Err(ref failure) if failure.refusal == HostCallRefusal::Undeclared
        ),
        "the manifest declares only [roles.reviewer], got {refused:?}"
    );
    assert!(
        sub_agents
            .specs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "a refusal the plugin could never have bought must not spend anything"
    );
}

/// A host driver that records the prelude it was handed and completes
/// trivially — the same shape `research_plugin_dispatch.rs`'s
/// `RecordingDriver` uses, so a real subprocess conversation can be driven
/// through [`WrapperDispatch::run`] without spinning up a real engine
/// turn. Enough to answer this suite's only question: did the plugin's
/// real, spawned `child_turn` conversation reach the turn.
#[derive(Default)]
struct RecordingTurnDriver {
    prelude: Option<TurnPrelude>,
}

#[async_trait(?Send)]
impl TurnDriver for RecordingTurnDriver {
    async fn run_turn(&mut self, prelude: TurnPrelude) -> DrivenTurn {
        self.prelude = Some(prelude);
        DrivenTurn {
            outcome: WrapperTurnOutcome {
                completed: true,
                answer: "done".to_string(),
                tools: Some(Vec::new()),
                changed_files: Some(Vec::new()),
            },
            tamper: stella_plugin::TamperFinding::NotChecked,
        }
    }
}

/// A `/bin/sh` fixture plugin's `main.sh`: asks the host for `child_turn`
/// at role `reviewer`, then reports what it read back. No JSON library —
/// `wrapper_child_turn.rs`'s reason (`doc:pipeline-as-plugins` §5
/// commitment 2): a capability only Rust can reach is a Rust API with
/// extra steps.
const CHILD_TURN_SUBPROCESS_SCRIPT: &str = r#"#!/bin/sh
read -r request
printf '%s\n' '{"call":"child_turn","id":1,"args":{"role":"reviewer","instruction":"does the diff drop the retry?"}}'
read -r answer
case "$answer" in
  *'"seat":"research"'*) seat="research" ;;
  *'"refusal":"undeclared"'*) seat="refused" ;;
  *) seat="unknown" ;;
esac
case "$seat" in
  research) finding="the reviewer (research) confirms the retry is dropped" ;;
  refused) finding="the host refused an undeclared role intent; degrading" ;;
  *) finding="no assessment was available" ;;
esac
printf '{"point":"before_turn","body":{"protocol_version":1,"context":[{"label":"reviewer","text":"%s"}]}}\n' "$finding"
"#;

/// Write `CHILD_TURN_SUBPROCESS_SCRIPT` into a fresh temp directory and
/// build the `[runtime]`/`[wrapper]` manifest text around it — `roles` is
/// the only thing that differs between the declared and undeclared cases
/// below, so it is the one parameter.
fn subprocess_plugin(roles_and_subloop: &str, wrapper_id: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("a scratch plugin dir");
    std::fs::write(dir.path().join("main.sh"), CHILD_TURN_SUBPROCESS_SCRIPT)
        .expect("write the fixture script");
    let manifest = format!(
        "name = \"reviewer\"\n[loop]\nparticipation = \"steering\"\npoints = \
         [\"before_turn\"]\ncalls = [\"child_turn\"]\n[runtime]\nargv = [\"/bin/sh\", \
         \"${{plugin_dir}}/main.sh\"]\ntimeout_secs = 10\n[wrapper]\nid = \"{wrapper_id}\"\n\
         [[wrapper.stages]]\nname = \"execute\"\n\n{roles_and_subloop}"
    );
    (dir, manifest)
}

/// **Witness (this change, full subprocess conversation).** A real
/// `/bin/sh` process is spawned through [`bind_installed`]'s own transport
/// — the exact object graph `stella run --pipeline <variant>` assembles —
/// asks this host for `child_turn` over stdio, and the answer it reads back
/// carries this session's own dispatcher's report.
///
/// Fails before this change for the reason the whole task does: `bind_installed`
/// built `HostPlanes::recalling(..)` with no `.with_child_turns(..)`, so the
/// spawned plugin's ask would have read back `{"refusal":"unavailable",...}`
/// regardless of what its manifest declared, and the finding below would
/// never appear in the turn's messages.
#[cfg(unix)]
#[tokio::test]
async fn a_declared_child_turn_survives_the_real_subprocess_conversation() {
    let (plugin_dir, manifest_text) = subprocess_plugin(
        "[subloop]\nstages = [\"research\"]\n\n[roles.reviewer]\ntier = \"research\"\n",
        "reviewer-subprocess-v1",
    );
    let roster = roster(vec![installed(
        &manifest_text,
        plugin_dir.path().to_str().expect("a utf-8 temp path"),
    )]);
    let sub_agents = RecordingSubAgents::default();
    let wrapper = bind_installed(
        &roster,
        "reviewer-subprocess-v1",
        no_recall(),
        Arc::new(sub_agents.clone()),
        &mut |_| {},
    )
    .expect("the installed plugin declares this variant");

    let mut driver = RecordingTurnDriver::default();
    let report = wrapper
        .dispatch
        .run(
            RoundInput {
                goal: "the retry is dropped on a 429".to_string(),
                signals: pre_turn_signals(false, false),
                candidate: None,
            },
            &mut driver,
        )
        .await
        .expect("the declared stage order resolves");

    assert!(
        report.faults.is_empty(),
        "the real subprocess conversation must complete cleanly: {:?}",
        report.faults
    );
    let prelude = driver.prelude.expect("the host was asked to run a turn");
    let messages = prelude.into_messages();
    assert!(
        messages.iter().any(|message| message
            .content
            .contains("the reviewer (research) confirms the retry is dropped")),
        "the plugin's contribution must carry the real child turn's answer: {messages:?}"
    );

    let specs = sub_agents
        .specs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(
        specs.len(),
        1,
        "this session's own dispatcher made exactly one real call"
    );
    assert!(
        !specs[0].write_access,
        "a plugin's child turn is read-only, enforced at execution"
    );
    assert!(
        wrapper.gate.refusals().is_empty(),
        "a declared call inside the allowance is performed, not refused"
    );
}

/// **Witness, the other half.** The identical spawned plugin, bound to a
/// manifest that still declares `calls = ["child_turn"]` — so the
/// transport still offers the conversation, matching
/// [`HostCallGate::offers_calls`]'s contract that a plugin declaring no
/// calls at all never has its stdin held open in the first place — but
/// names no `[roles.reviewer]`: [`ChildTurns::resolve`] refuses before
/// this host's dispatcher is ever touched, the plugin reads that refusal
/// back over the same stdio conversation, and degrades — exactly the
/// contract `crates/stella-runtime/tests/wrapper_child_turn.rs`'s
/// `an_undeclared_role_intent_is_refused_to_the_plugin_and_reported_to_the_host`
/// proves at the generic host layer, reproduced here through this driver's
/// own real subprocess wiring.
#[cfg(unix)]
#[tokio::test]
async fn an_undeclared_child_turn_is_refused_through_the_real_subprocess_conversation() {
    // No `[roles]`/`[subloop]` at all — a plugin that declared the
    // capability but named no role intent for it.
    let (plugin_dir, manifest_text) = subprocess_plugin("", "reviewer-no-roles-v1");
    let roster = roster(vec![installed(
        &manifest_text,
        plugin_dir.path().to_str().expect("a utf-8 temp path"),
    )]);
    let sub_agents = RecordingSubAgents::default();
    let wrapper = bind_installed(
        &roster,
        "reviewer-no-roles-v1",
        no_recall(),
        Arc::new(sub_agents.clone()),
        &mut |_| {},
    )
    .expect("the installed plugin declares this variant");

    let mut driver = RecordingTurnDriver::default();
    let report = wrapper
        .dispatch
        .run(
            RoundInput {
                goal: "the retry is dropped on a 429".to_string(),
                signals: pre_turn_signals(false, false),
                candidate: None,
            },
            &mut driver,
        )
        .await
        .expect("the declared stage order resolves");

    assert!(
        report.faults.is_empty(),
        "a refused call is a value the plugin reads, never a death: {:?}",
        report.faults
    );
    let prelude = driver.prelude.expect("the host was asked to run a turn");
    let messages = prelude.into_messages();
    assert!(
        messages.iter().any(|message| message
            .content
            .contains("the host refused an undeclared role intent; degrading")),
        "the plugin must read back its own refusal and degrade honestly: {messages:?}"
    );

    assert!(
        sub_agents
            .specs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "an undeclared ask must never reach this host's dispatcher"
    );
    let refusals = wrapper.gate.refusals();
    assert_eq!(refusals.len(), 1, "the refusal is reported, never silent");
    assert_eq!(
        refusals[0].refusal,
        stella_plugin::HostCallRefusal::Undeclared
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
