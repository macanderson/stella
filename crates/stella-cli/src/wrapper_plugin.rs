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
//! # What a plugin is handed, and what it is told it cannot have
//!
//! The three holes this module shipped with are closed, and each closure names
//! the limit that remains rather than implying there is none:
//!
//! 1. **A real candidate grant and a real tamper finding** (#3553). The grant
//!    names the **shared work tree** — the tree the turn actually runs in — and
//!    carries the parsed `--test-command` as a [`stella_plugin::TestPlan`], so a wrapper whose
//!    `[oracle]` declares `flip = "required"` can observe red before and green
//!    after and reach a *decided* verdict. The host pins the identity of the
//!    artifacts that invocation names and vouches for them itself. See
//!    [`crate::wrapper_candidate`] for why the grant is not an isolated
//!    worktree, and for the invocation shapes whose tamper finding is still
//!    [`stella_plugin::TamperFinding::NotChecked`].
//! 2. **Real turn facts** (#3552). `TurnOutcome::tools` and `changed_files` are
//!    folded from the turn's own event stream by [`crate::turn_facts`] and sent
//!    as `Some(..)` — so `Some(vec![])` is "the turn touched nothing" and the
//!    `None` the wire now also carries is "this host does not report it", which
//!    is what a plugin could not tell apart before.
//! 3. **A host-call gate** (#3561). Every wrapper is bound with one, so a
//!    plugin's `recall` reaches this workspace's context plane
//!    ([`crate::wrapper_recall`]) instead of finding no channel at all. A host
//!    with no plane still attaches the gate and answers with no frames: an
//!    empty answer is something a plugin can degrade on, and an absent gate is
//!    the one thing it cannot be told about.
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
use stella_plugin::{SignalValues, TurnOutcome as WrapperTurnOutcome};
use stella_protocol::{AgentEvent, CompletionMessage};
use stella_runtime::wrapper::{
    DEFAULT_HOST_MAX_CALLS, DispatchReport, DrivenTurn, HostCallGate, HostPlanes, RoundInput,
    SubprocessWrapper, TurnDriver, TurnPrelude, WrapperDispatch,
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

/// One installed wrapper, bound to its process and to what this host will do
/// for it.
///
/// The two travel together because the gate outlives the call that built it and
/// nothing else can read it: [`WrapperDispatch`] holds the transport as an
/// `Arc<dyn TurnWrapper>` and has no way to hand back the gate inside it, so a
/// caller holding only the dispatch could never report what a plugin was
/// refused — the silent half of "a refusal is reported, never silent".
#[derive(Debug)]
pub(crate) struct BoundWrapper {
    /// The sequence that drives the plugin's points.
    pub(crate) dispatch: WrapperDispatch,
    /// The host-call gate this plugin's conversations run through, kept so
    /// [`HostCallGate::refusals`] reaches a surface after the run.
    gate: Arc<HostCallGate>,
}

impl BoundWrapper {
    /// The variant id this wrapper runs under.
    pub(crate) fn variant(&self) -> &str {
        self.dispatch.variant()
    }
}

/// Read what is installed and bind the wrapper `variant` names.
///
/// The impure half — it reads the two plugin tiers, the settings that retract
/// them, and this workspace's context plane — kept apart from
/// [`bind_installed`], which is the decision and is therefore the half the tests
/// drive.
///
/// # Errors
///
/// Whatever [`bind_installed`] refuses.
pub(crate) fn resolve(
    workspace_root: &std::path::Path,
    variant: &str,
    warn: &mut dyn FnMut(String),
) -> Result<BoundWrapper, String> {
    let settings = crate::settings::Settings::load(workspace_root).unwrap_or_default();
    let (roster, notices) = PluginRoster::load(workspace_root, &settings);
    // A plugin that did not load must never vanish silently: "I installed it
    // and nothing happened" is unanswerable without the reason.
    for notice in notices {
        warn(notice.trim_start_matches(" ! ").to_string());
    }
    bind_installed(
        &roster,
        variant,
        Box::new(crate::wrapper_recall::SessionRecallHost::open(
            workspace_root,
        )),
        warn,
    )
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
///
/// `recall` is this host's context plane, taken as a parameter rather than
/// opened here so the decision stays testable without a workspace on disk. It
/// is always bound into a [`HostCallGate`], even when the plane behind it is
/// empty: a plugin that asks and is answered "no frames" degrades honestly,
/// while a plugin that asks through a transport with *no* gate has its stdin
/// shut and waits for an answer that never comes (#3561).
pub(crate) fn bind_installed(
    roster: &PluginRoster,
    variant: &str,
    recall: Box<dyn stella_runtime::wrapper::RecallHost>,
    warn: &mut dyn FnMut(String),
) -> Result<BoundWrapper, String> {
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
    // The manifest's own `[loop]` grant is the authoritative filter — an
    // undeclared capability is refused before this host performs anything —
    // and `DEFAULT_HOST_MAX_CALLS` clamps whatever allowance it asked for.
    let gate = Arc::new(HostCallGate::declare(
        installed.manifest.loop_grant.clone(),
        DEFAULT_HOST_MAX_CALLS,
        Box::new(HostPlanes::recalling(crate::wrapper_recall::BoxedRecall(
            recall,
        ))),
    ));
    let dispatch = WrapperDispatch::bind(
        installed.manifest.clone(),
        Arc::new(admitted.wrapper.serving(Arc::clone(&gate))),
    )
    .map_err(|error| format!("wrapper \"{variant}\" cannot be driven: {error}"))?;
    Ok(BoundWrapper { dispatch, gate })
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
    /// The artifacts this host pinned before the run, and the finding it
    /// reports about them after each turn (#3553).
    pub(crate) watch: &'a crate::wrapper_candidate::TamperWatch,
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
        // One observer per round: `tools` and `changed_files` are facts about
        // *this* turn, and a fold shared across rounds would report the first
        // round's tools as the third round's (#3552).
        let facts = crate::turn_facts::TurnFacts::new();
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
            TurnDoor::new("run")
                .wrapped_by(self.variant)
                .reporting_to(facts.clone()),
            self.prompt,
            Some(self.session),
            self.recall_event.take(),
            self.memory.as_deref_mut(),
        )
        .await;

        // `Some` in every arm, including the aborted one: this host *does*
        // observe both facts, and a turn that aborted after two tool calls made
        // those two calls. `None` is reserved for a host that cannot look.
        let observed = |completed: bool, answer: String| WrapperTurnOutcome {
            completed,
            answer,
            tools: Some(facts.tools()),
            changed_files: Some(facts.changed_files()),
        };
        let turn = match &outcome {
            Ok(stella_core::TurnOutcome::Completed { text, .. }) => observed(true, text.clone()),
            // An abort is evidence, not an error to swallow: a wrapper whose
            // job is to have an opinion about the turn gets to have one about a
            // turn that did not finish.
            Ok(stella_core::TurnOutcome::Aborted { reason, .. }) => observed(false, reason.clone()),
            Err(failure) => observed(false, failure.to_string()),
        };
        self.results.push(outcome.map(|_| ()));
        DrivenTurn {
            outcome: turn,
            // The host's own comparison, over artifacts it pinned before the
            // run — never the plugin's claim about its own witness (#3499).
            // `NotChecked` survives as a real answer for an invocation that
            // named nothing to watch; see `crate::wrapper_candidate`.
            tamper: self.watch.finding(),
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
    bound: &BoundWrapper,
    goal: &str,
    signals: SignalValues,
    candidate: Option<stella_plugin::CandidateGrant>,
    mut driver: RawTurnDriver<'_>,
) -> Result<(), CliFailure> {
    let format = driver.format;
    let input = RoundInput {
        goal: goal.to_string(),
        signals,
        // The tree the turn actually runs in — see `crate::wrapper_candidate`
        // for why that is the shared work tree and not an isolated worktree.
        candidate,
    };
    let report = bound.dispatch.run(input, &mut driver).await;
    let last = driver.results.pop();
    match report {
        Ok(report) => {
            report_to(format, &report, &bound.gate);
            // A round always runs, so `results` always has an entry; an empty
            // one would mean the dispatcher returned without driving anything,
            // which is a report about the wrapper and not about the work.
            last.unwrap_or(Ok(()))
        }
        Err(error) => Err(CliFailure::from(error.to_string())),
    }
}

/// Print what the wrapper concluded, every point that abstained, and every
/// capability this host refused it.
///
/// The refusals are the host's half of "a refusal is reported, never silent"
/// (#3561): the plugin already read the `err` and degraded, and a user watching
/// a plugin contribute nothing needs the same sentence to know why. They print
/// in every format, like the faults beside them, because an unanswered
/// capability is a fact about the run rather than commentary about it.
///
/// stderr in every format: stdout may be machine-readable JSON, and a wrapper's
/// commentary is not part of either summary contract.
fn report_to(format: OutputFormat, report: &DispatchReport, gate: &HostCallGate) {
    for fault in &report.faults {
        eprintln!("  ! wrapper: {fault}");
    }
    for refused in gate.refusals() {
        eprintln!("  ! wrapper: {refused}");
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
        bind_installed(roster, variant, no_recall(), warn)
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
        let error =
            bound(&roster, "vera-v2", &mut |_| {}).expect_err("nothing installed declares it");
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
        let wrapper = bind_installed(&roster, "research-v1", Box::new(OneFrame), &mut |_| {})
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
}
