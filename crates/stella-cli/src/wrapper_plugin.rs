// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella-cli` as a **driver** of the wrapper socket (#3494).
//!
//! `stella_runtime::wrapper` shipped four points, two transports and the two
//! host functions, and nothing in the shipping binary called any of them — so
//! `doc:wrapper-socket` §6's first driver was unmet and an installed wrapper
//! plugin participated in nothing. This module is that driver: it resolves
//! `--pipeline <variant>` against what is installed, builds the transport the
//! manifest's `[runtime]` block declares, and implements
//! [`TurnDriver`](stella_runtime::TurnDriver) over the raw engine turn.
//!
//! # What is here and what is deliberately one crate down
//!
//! The **sequence** — before_turn per stage, the turn, after_turn, judge,
//! again — is `stella_runtime::WrapperDispatch` and stays there, because §6
//! requires the same plugin to run under `stella-serve` and an embedded
//! `stella-engine` host too, and a sequence living in this binary is one
//! neither can reach. What is here is only the part that is genuinely the
//! CLI's: which plugins are installed, how this process resolves an
//! environment allowlist, and what one turn of *this* engine is.
//!
//! `stella-core` gains nothing from any of it. The engine never learns plugins
//! exist; this module binds the grants and hands the engine plain messages.
//!
//! # It lives beside `agent.rs`, not inside it
//!
//! `crates/stella-cli/src/agent.rs` is a grandfathered god file closed to
//! growth (AGENTS.md § "God files"), so this is a sibling module — the same
//! placement, for the same reason, as `crate::turn_files`.
//!
//! # Two honest gaps, declared rather than papered over
//!
//! 1. **No candidate workspace and no tamper snapshot.** The raw one-shot path
//!    works in the shared tree, so [`RoundInput::candidate`] is `None` and this
//!    host reports [`TamperFinding::NotChecked`] about its own check. A wrapper
//!    whose `[oracle]` declares `flip = "required"` therefore gets
//!    `Verdict::Undecided` on this path every time — correctly, since nothing
//!    ran a witness — and a wrapper whose definition of done is a measurement
//!    works today. Wiring the pipeline's candidate workspaces to the socket is
//!    #3553, not this slice.
//! 2. **`TurnOutcome::tools` and `changed_files` are empty.** The engine's
//!    `TurnOutcome` carries the answer and the cost; the tool names and the
//!    tree delta exist only as events already drained by the renderer when this
//!    seam sees the turn. A plugin reading either gets an empty list, which is
//!    indistinguishable from "the turn touched nothing" — a real defect for a
//!    wrapper that reads them, tracked as #3552.
//!
//! And two scope limits: `--pipeline` is on `stella run` alone and a plugin's
//! `Unmet` does not fail the process (#3554), and only this driver of
//! `doc:wrapper-socket` §6's three exists (#3551).

use std::sync::Arc;

use async_trait::async_trait;
use stella_core::budget::BudgetGuard;
use stella_core::estimator::CalibrationMap;
use stella_core::ports::ToolExecutor;
use stella_core::router::Router;
use stella_model::provider::Provider;
use stella_plugin::{SignalValues, TamperFinding, TurnOutcome as WrapperTurnOutcome};
use stella_protocol::{AgentEvent, CompletionMessage};
use stella_runtime::wrapper::{
    DispatchReport, DrivenTurn, RoundInput, SubprocessWrapper, TurnDriver, TurnPrelude,
    WrapperDispatch,
};
use stella_store::Store;
use stella_tools::ToolRegistry;
use stella_tools::custom::CustomTool;

use crate::agent::persistence::TurnDoor;
use crate::failure::CliFailure;
use crate::memory::SessionMemory;
use crate::plugin_cmd::roster::PluginRoster;
use crate::{OutputFormat, config::Config};

/// Which wrapper runs over a one-shot turn.
///
/// `doc:pipeline-as-plugins` §7 plans `--pipeline <variant>` to *replace*
/// `--no-pipeline`; this is the additive half of that inversion, so both flags
/// exist and the third arm is the new capability. Making it an enum rather than
/// a `bool` plus an `Option<&str>` is what keeps "the staged pipeline and a
/// plugin both ran" unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PipelineChoice<'a> {
    /// The built-in staged pipeline — today's default, recorded as `classic`.
    Classic,
    /// The raw step-loop with nothing over it (`--no-pipeline`).
    Raw,
    /// The raw step-loop wrapped by an installed plugin whose `[wrapper] id` is
    /// this variant (`--pipeline <variant>`).
    Plugin(&'a str),
}

impl<'a> PipelineChoice<'a> {
    /// Read the two flags into one choice.
    ///
    /// # Errors
    ///
    /// A message naming the conflict when both are given: `--no-pipeline`
    /// asks for nothing over the turn and `--pipeline` names something, and
    /// silently preferring one would make a user's explicit instruction a
    /// no-op. `classic` is accepted by name so `--pipeline classic` means what
    /// it says rather than looking for a plugin nobody installed.
    pub(crate) fn resolve(no_pipeline: bool, pipeline: Option<&'a str>) -> Result<Self, String> {
        match (no_pipeline, pipeline) {
            (true, Some(variant)) => Err(format!(
                "--no-pipeline runs nothing over the turn, but --pipeline {variant} names a \
                 wrapper to run — pass one or the other"
            )),
            (true, None) => Ok(Self::Raw),
            (false, None) => Ok(Self::Classic),
            (false, Some(variant))
                if variant == crate::agent::persistence::PIPELINE_VARIANT_CLASSIC =>
            {
                Ok(Self::Classic)
            }
            (false, Some(variant)) => Ok(Self::Plugin(variant)),
        }
    }

    /// Whether the built-in staged pipeline is the wrapper for this turn.
    pub(crate) fn is_classic(self) -> bool {
        matches!(self, Self::Classic)
    }

    /// Whether this turn runs with **nothing** over it.
    ///
    /// The distinction the enterprise process-free authority turns on, and it
    /// is not the same question as [`Self::is_classic`]: that authority admits
    /// exactly one execution surface — the raw loop, which spawns nothing — and
    /// a wrapper plugin's entire mechanism is a child process the host starts.
    /// Grading `--pipeline <variant>` as "not the pipeline, therefore raw"
    /// would have let an installed plugin spawn inside a boundary drawn to
    /// exclude executable extensions, exactly as `--no-pipeline` does not.
    pub(crate) fn is_raw(self) -> bool {
        matches!(self, Self::Raw)
    }

    /// The installed wrapper plugin's variant id, when one was named.
    pub(crate) fn plugin(self) -> Option<&'a str> {
        match self {
            Self::Plugin(variant) => Some(variant),
            Self::Classic | Self::Raw => None,
        }
    }
}

/// Read what is installed and bind the wrapper `variant` names.
///
/// The impure half — it reads the two plugin tiers and the settings that retract
/// them — kept apart from [`bind_installed`], which is the decision and is
/// therefore the half the tests drive.
///
/// # Errors
///
/// Whatever [`bind_installed`] refuses.
pub(crate) fn resolve(
    workspace_root: &std::path::Path,
    variant: &str,
    warn: &mut dyn FnMut(String),
) -> Result<WrapperDispatch, String> {
    let settings = crate::settings::Settings::load(workspace_root).unwrap_or_default();
    let (roster, notices) = PluginRoster::load(workspace_root, &settings);
    // A plugin that did not load must never vanish silently: "I installed it
    // and nothing happened" is unanswerable without the reason.
    for notice in notices {
        warn(notice.trim_start_matches(" ! ").to_string());
    }
    bind_installed(&roster, variant, warn)
}

/// Find the installed wrapper plugin that declares `variant`, and bind it to
/// its process.
///
/// # Errors
///
/// A message a user can act on: which variants *are* installed, or why the one
/// they named cannot run. `stella-cli` is a binary, so a `String` here is the
/// finished product rather than an unnamed error (AGENTS.md invariant 5).
///
/// A refused environment name is **reported, not silently dropped**:
/// [`SubprocessWrapper::declare`] withholds model credentials at the socket for
/// every driver (#3512), and a plugin author whose manifest asked for one can
/// only stop asking if they are told.
pub(crate) fn bind_installed(
    roster: &PluginRoster,
    variant: &str,
    warn: &mut dyn FnMut(String),
) -> Result<WrapperDispatch, String> {
    let installed = roster
        .plugins()
        .iter()
        .find(|plugin| {
            plugin
                .manifest
                .wrapper
                .as_ref()
                .is_some_and(|wrapper| wrapper.id == variant)
        })
        .ok_or_else(|| {
            let mut available: Vec<&str> = roster
                .plugins()
                .iter()
                .filter_map(|plugin| plugin.manifest.wrapper.as_ref())
                .map(|wrapper| wrapper.id.as_str())
                .collect();
            available.sort_unstable();
            if available.is_empty() {
                format!(
                    "no installed plugin declares the wrapper variant \"{variant}\" — \
                     `stella plugin list` shows what is installed"
                )
            } else {
                format!(
                    "no installed plugin declares the wrapper variant \"{variant}\" — \
                     installed variants: {}",
                    available.join(", ")
                )
            }
        })?;

    let runtime = installed.manifest.runtime.as_ref().ok_or_else(|| {
        format!(
            "plugin \"{}\" declares the wrapper variant \"{variant}\" but no [runtime] block, \
             so there is no process to ask",
            installed.manifest.name
        )
    })?;

    // `${plugin_dir}` is the host's substitution — this crate is where the
    // install directory is known, exactly as `PluginRoster::hook_routes` does
    // it for hooks.
    let dir = installed.dir.to_string_lossy().into_owned();
    let argv: Vec<String> = runtime
        .argv
        .iter()
        .map(|arg| arg.replace("${plugin_dir}", &dir))
        .collect();
    // The one ambient read on this path, and it belongs here: `stella-runtime`
    // reads no process environment by contract, so the host resolves the
    // manifest's allowlist and hands over pairs.
    let env = runtime.child_env(|name| std::env::var(name).ok());
    let admitted = SubprocessWrapper::declare(
        argv,
        env,
        std::time::Duration::from_secs(runtime.timeout_secs),
    )
    .map_err(|error| format!("wrapper \"{variant}\" cannot be started: {error}"))?;
    for refused in &admitted.refused {
        warn(format!(
            "wrapper \"{variant}\" asked for {refused} and will not get it — a plugin never \
             receives a model credential; declare a [roles] tier instead"
        ));
    }
    WrapperDispatch::bind(installed.manifest.clone(), Arc::new(admitted.wrapper))
        .map_err(|error| format!("wrapper \"{variant}\" cannot be driven: {error}"))
}

/// This host's published signal values, as they stand **before** the turn.
///
/// Total by construction — [`SignalValues`] derives no `Default` precisely so a
/// host has to answer every signal — and every answer here is a fact about the
/// run rather than a placeholder:
///
/// - The pre-turn facts are real: `test_command` is whether one was configured,
///   `budget_metered` whether spend is gated rather than merely recorded,
///   `candidates` is 1 because this path runs in the shared tree.
/// - The **post-turn** signals (`mutating_actions`, `diff_lines`,
///   `witness_authored`, `flip_achieved`, `tests_red`/`tests_green`) are the
///   zero/false they genuinely are at this instant: nothing has run yet. What a
///   wrapper cannot do is read one *after* the turn and have it change which
///   stages run, because [`Wrapper::resolve`] takes one up-front snapshot —
///   that is #3491, and it is a gap in the resolver rather than a lie here.
/// - The triage signals (`conversational`, `questions`, `plans`, `verifies`,
///   `wants_witness`, `wants_verifier`) are the staged pipeline's own
///   assessments. This path runs no triage stage, so it publishes the values a
///   run with no assessment has, and a wrapper whose stage condition reads one
///   sees exactly that.
///
/// [`Wrapper::resolve`]: stella_plugin::Wrapper::resolve
pub(crate) fn pre_turn_signals(test_command: bool, budget_metered: bool) -> SignalValues {
    SignalValues {
        test_command,
        candidates: 1,
        budget_metered,
        conversational: false,
        questions: 0,
        plans: false,
        verifies: false,
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

/// Everything one raw engine turn needs, so the dispatcher can ask for several.
///
/// Borrowed rather than owned because the caller
/// (`crate::agent::goal::run_raw_one_shot`) assembled all of it and keeps using
/// it afterwards — for reflection, the episode record and the close-out.
pub(crate) struct RawTurnDriver<'a> {
    /// The provider adapter this session was built with.
    pub(crate) provider: &'a dyn Provider,
    /// The tool set below the session stack.
    pub(crate) base_tools: &'a dyn ToolExecutor,
    /// Developer-defined script tools discovered for this session.
    pub(crate) custom_tools: &'a [CustomTool],
    /// The registry, for its ledgers and its event stream.
    pub(crate) registry: &'a ToolRegistry,
    /// The conversation. Contributions are appended here, after the stable
    /// system prefix at index 0.
    pub(crate) messages: &'a mut Vec<CompletionMessage>,
    /// The session budget guard.
    pub(crate) budget: &'a mut BudgetGuard,
    /// Token-drift calibration.
    pub(crate) calibration: &'a CalibrationMap,
    /// Session-scoped breaker feedback.
    pub(crate) router: &'a Router,
    /// The loaded config.
    pub(crate) cfg: &'a Config,
    /// How this run renders.
    pub(crate) format: OutputFormat,
    /// The workspace store, when persistence is on.
    pub(crate) store: &'a Option<Arc<Store>>,
    /// The user's prompt.
    pub(crate) prompt: &'a str,
    /// This session's presence id.
    pub(crate) session: &'a str,
    /// The variant id recorded on every execution row this driver opens.
    pub(crate) variant: &'a str,
    /// This turn's `ContextRecall`, spent on the first round only — recall runs
    /// once per session, and re-emitting its event on a held-open round would
    /// claim a retrieval that never happened.
    pub(crate) recall_event: Option<AgentEvent>,
    /// The session memory, for the execution stamp and skill-usage record.
    pub(crate) memory: Option<&'a mut SessionMemory>,
    /// What each round's turn returned, in order — the caller's own view of a
    /// loop the dispatcher owns.
    pub(crate) results: Vec<Result<(), CliFailure>>,
}

#[async_trait(?Send)]
impl TurnDriver for RawTurnDriver<'_> {
    async fn run_turn(&mut self, prelude: TurnPrelude) -> DrivenTurn {
        // Invariant 7, at the one call site that spends it: `into_messages`
        // hands back user messages, and they are appended *after* the
        // byte-stable system prefix the conversation already opens with.
        self.messages.extend(prelude.into_messages());
        let outcome = crate::agent::run_turn(
            self.provider,
            self.base_tools,
            self.custom_tools,
            self.registry,
            self.messages,
            self.budget,
            self.calibration,
            self.router,
            self.cfg,
            self.format,
            self.store,
            TurnDoor::new("run").wrapped_by(self.variant),
            self.prompt,
            Some(self.session),
            self.recall_event.take(),
            self.memory.as_deref_mut(),
        )
        .await;

        let turn = match &outcome {
            Ok(stella_core::TurnOutcome::Completed { text, .. }) => WrapperTurnOutcome {
                completed: true,
                answer: text.clone(),
                ..WrapperTurnOutcome::default()
            },
            // An abort is evidence, not an error to swallow: a wrapper whose
            // job is to have an opinion about the turn gets to have one about a
            // turn that did not finish.
            Ok(stella_core::TurnOutcome::Aborted { reason, .. }) => WrapperTurnOutcome {
                completed: false,
                answer: reason.clone(),
                ..WrapperTurnOutcome::default()
            },
            Err(failure) => WrapperTurnOutcome {
                completed: false,
                answer: failure.to_string(),
                ..WrapperTurnOutcome::default()
            },
        };
        self.results.push(outcome.map(|_| ()));
        DrivenTurn {
            outcome: turn,
            // This host holds no candidate worktree and took no authoring-time
            // identity snapshot, so it says so about its own check rather than
            // making the plugin admit to one it could never perform (#3499).
            tamper: TamperFinding::NotChecked,
        }
    }
}

/// Drive one wrapper plugin around as many raw turns as its verdict asks for.
///
/// Returns the last round's result — the run's exit status is the turn's, not
/// the wrapper's verdict. That is deliberate for this slice: `--require-verified`
/// is the flag that turns "completed but unproven" into a failure, and it is
/// wired to the staged pipeline's ladder rather than to a plugin's rule. Making
/// a plugin's `Unmet` fail the process is a separate decision with its own
/// blast radius — a third party's manifest failing a user's build wants an
/// explicit flag, not a side effect of installing something — and it is #3554.
///
/// # Errors
///
/// The turn's own failure, or a wrapper whose declared stage order could not be
/// resolved — which a validated manifest cannot hit.
pub(crate) async fn run_wrapped(
    dispatch: &WrapperDispatch,
    goal: &str,
    signals: SignalValues,
    mut driver: RawTurnDriver<'_>,
) -> Result<(), CliFailure> {
    let format = driver.format;
    let input = RoundInput {
        goal: goal.to_string(),
        signals,
        // The raw path works in the shared tree; see this module's declared
        // gaps.
        candidate: None,
    };
    let report = dispatch.run(input, &mut driver).await;
    let last = driver.results.pop();
    match report {
        Ok(report) => {
            report_to(format, &report);
            // A round always runs, so `results` always has an entry; an empty
            // one would mean the dispatcher returned without driving anything,
            // which is a report about the wrapper and not about the work.
            last.unwrap_or(Ok(()))
        }
        Err(error) => Err(CliFailure::from(error.to_string())),
    }
}

/// Print what the wrapper concluded, and every point that abstained.
///
/// stderr in every format: stdout may be machine-readable JSON, and a wrapper's
/// commentary is not part of either summary contract.
fn report_to(format: OutputFormat, report: &DispatchReport) {
    for fault in &report.faults {
        eprintln!("  ! wrapper: {fault}");
    }
    if format == OutputFormat::Text || !report.met() {
        eprintln!("  ◇ {}", report.summary());
    }
}

#[cfg(test)]
mod tests {
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

    /// The two flags cannot both decide the turn, and `classic` by name is the
    /// built-in rather than a plugin lookup that would always fail.
    #[test]
    fn the_pipeline_flags_resolve_into_exactly_one_choice() {
        assert_eq!(
            PipelineChoice::resolve(false, None),
            Ok(PipelineChoice::Classic),
            "no flag is exactly today's behaviour"
        );
        assert_eq!(PipelineChoice::resolve(true, None), Ok(PipelineChoice::Raw));
        assert_eq!(
            PipelineChoice::resolve(false, Some("classic")),
            Ok(PipelineChoice::Classic),
            "the built-in is selectable by the id it records"
        );
        assert_eq!(
            PipelineChoice::resolve(false, Some("budget-v1")),
            Ok(PipelineChoice::Plugin("budget-v1"))
        );
        let conflict = PipelineChoice::resolve(true, Some("budget-v1"))
            .expect_err("both flags name different things");
        assert!(conflict.contains("--no-pipeline"), "{conflict}");
        assert!(conflict.contains("budget-v1"), "{conflict}");
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
        let dispatch = bind_installed(&roster, "budget-v1", &mut |line| warnings.push(line))
            .expect("the installed plugin declares this variant");
        assert_eq!(dispatch.variant(), "budget-v1");
        assert_eq!(dispatch.manifest().name, "budget-keeper");
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
        let error = bind_installed(&roster, "vera-v2", &mut |_| {})
            .expect_err("nothing installed declares it");
        assert!(error.contains("vera-v2"), "{error}");
        assert!(error.contains("budget-v1"), "{error}");

        let nothing = PluginRoster::default();
        let empty = bind_installed(&nothing, "vera-v2", &mut |_| {}).expect_err("none at all");
        assert!(empty.contains("stella plugin list"), "{empty}");
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
        let error =
            bind_installed(&roster, "budget-v1", &mut |_| {}).expect_err("no [runtime] block");
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
}
