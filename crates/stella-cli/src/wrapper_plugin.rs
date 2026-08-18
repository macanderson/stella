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
//! [`TurnDriver`] over the raw engine turn.
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
//! The holes this module shipped with are closed, and each closure names the
//! limit that remains rather than implying there is none:
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
//! 4. **A child-turn plane, on this driver** (#3576, this door's slice of it).
//!    `[loop] calls = ["child_turn"]` reaches
//!    [`stella_runtime::wrapper::ChildTurns`] over this session's
//!    own sub-agent dispatcher — the same dispatcher `task_assign` runs
//!    children on ([`crate::subagent::SessionSubAgents`]),
//!    shared rather than duplicated so budget carving, read-only tooling and
//!    report clamping stay one implementation. `resolve` and `bind_installed`
//!    now take that dispatcher as a parameter rather than opening one
//!    themselves, which is why `run_raw_one_shot` (`agent/goal.rs`)
//!    builds the session's provider, registry and dispatcher *before* binding
//!    the wrapper — see that function's own comment for why reordering it does
//!    not weaken the "fails as a typo before a paid call" guarantee. The other
//!    two drivers §6 asks for still assemble nothing (#3551), and `stella-serve`'s
//!    and an embedded `stella-engine` host's own dispatchers are not this one.
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
use stella_core::subagent::{SubAgentDispatcher, SubAgentOutcome, SubAgentSpec};
use stella_model::provider::Provider;
use stella_plugin::{SignalValues, TurnOutcome as WrapperTurnOutcome};
use stella_protocol::{AgentEvent, CompletionMessage};
use stella_runtime::wrapper::{
    ChildTurns, DEFAULT_HOST_MAX_CALLS, DispatchReport, DrivenTurn, HostCallGate, HostPlanes,
    RoundInput, SubprocessWrapper, TurnDriver, TurnPrelude, WrapperDispatch,
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
/// `doc:pipeline-as-plugins` §7 planned `--pipeline <variant>` to *replace*
/// `--no-pipeline`; `docs/spec/turn-loop-wrappers.md` §5 "Flip the default"
/// (#3381) is that replacement landing — the raw loop is now the default on
/// every door this enum reaches, `--pipeline <variant>` is the sole opt-in,
/// and `--no-pipeline` is a deprecated no-op kept parseable so no script
/// breaks the day this ships (see [`resolve`](Self::resolve) and
/// [`no_pipeline_deprecation_notice`]). Making this a three-way enum rather
/// than a `bool` plus an `Option<&str>` is what keeps "the staged pipeline
/// and a plugin both ran" unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PipelineChoice<'a> {
    /// The built-in staged pipeline, recorded as `classic`. Opt-in only,
    /// since #3381 — `--pipeline classic` selects it by name.
    Classic,
    /// The raw step-loop with nothing over it — the default since #3381.
    Raw,
    /// The raw step-loop wrapped by an installed plugin whose `[wrapper] id` is
    /// this variant (`--pipeline <variant>`).
    Plugin(&'a str),
}

impl<'a> PipelineChoice<'a> {
    /// Read the two flags into one choice.
    ///
    /// `no_pipeline` (`--no-pipeline`) is a deprecated no-op as of #3381: the
    /// raw loop is the default with or without it, so it plays no part in the
    /// match below and can no longer disagree with `--pipeline`. Passing both
    /// flags together used to be a hard conflict (clap's `conflicts_with`,
    /// removed in the same change); now the deprecated flag simply has
    /// nothing left to veto, and `--pipeline <variant>` always wins. `classic`
    /// is accepted by name so `--pipeline classic` means what it says rather
    /// than looking for a plugin nobody installed. Infallible — the one case
    /// that used to fail no longer can — so callers no longer need `?` here.
    pub(crate) fn resolve(no_pipeline: bool, pipeline: Option<&'a str>) -> Self {
        let _ = no_pipeline; // deprecated no-op (#3381) — see `no_pipeline_deprecation_notice`
        match pipeline {
            None => Self::Raw,
            Some(variant) if variant == crate::agent::persistence::PIPELINE_VARIANT_CLASSIC => {
                Self::Classic
            }
            Some(variant) => Self::Plugin(variant),
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

/// The one-line deprecation notice owed when `--no-pipeline` was passed
/// (#3381), or `None` when it was not.
///
/// A pure function returning data rather than printing directly, matching
/// [`crate::engine_config::effort_notices`]'s shape: [`PipelineChoice::resolve`]
/// stays a plain decision a unit test can call without capturing stderr, and
/// each door prints the line at most once **to the user's terminal** —
/// `arena.rs`, which never supervises, at the one place it reads `no_pipeline`
/// off its parsed args; `main.rs`'s `Run`/`Goal`/`Fleet` arms print it only
/// after their `Posture`'s early-return, so it fires in whichever single
/// process actually runs the turn, not in the parent that re-execs an
/// `Attached`/`Detached` launch's argv verbatim into a supervised child (that
/// child reaches this same call independently, and its `Foreground` posture
/// is what prints it — once, on its own stderr, which `Supervised::follow`
/// then relays back live under `Attached`). Printing it before that
/// early-return used to mean the parent printed it on its own account and the
/// re-exec'd child printed it again on the same relayed stream, doubling the
/// most common invocation's notice.
pub(crate) fn no_pipeline_deprecation_notice(no_pipeline: bool) -> Option<&'static str> {
    no_pipeline.then_some(
        "--no-pipeline is deprecated and does nothing: the raw step-loop is the default now. \
         Pass --pipeline <variant> to run a wrapper (\"classic\" names the built-in staged \
         pipeline).",
    )
}

/// Refuse a named wrapper plugin variant on a door that cannot drive one yet.
///
/// `stella run` is the only door with a real [`TurnDriver`] implementation
/// ([`WrapperDispatch`] over one raw turn): `goal` and `fleet` each drive
/// their own **round loop** (a judged goal round, a fleet worker's attempt),
/// and wiring a wrapper plugin through either is a real driver — not a
/// formatting difference — that nothing in this crate implements (#3695).
/// `--pipeline classic` and no `--pipeline` at all both resolve to
/// [`PipelineChoice::Classic`]/[`PipelineChoice::Raw`], which is fine on
/// every door; only a *named* plugin variant is out of reach here, and it is
/// refused with a message naming `stella run` as the door that can run it —
/// never silently downgraded to raw or to classic, which would run something
/// other than what was asked for without saying so.
pub(crate) fn reject_plugin_variant_for_door(
    door: &str,
    choice: PipelineChoice<'_>,
) -> Result<(), String> {
    match choice.plugin() {
        Some(variant) => Err(format!(
            "--pipeline {variant} is not supported on `stella {door}` yet — wrapper plugins \
             run only on `stella run --pipeline {variant}` today (#3695). Pass `--pipeline \
             classic` for the staged pipeline, or omit --pipeline for the raw loop."
        )),
        None => Ok(()),
    }
}

/// Refuse a pipeline-only verification flag against a [`PipelineChoice`] that
/// cannot honor it (#3696).
///
/// `--keep-witness`, `--require-verified`, and `--test-command` all belong to
/// the staged pipeline's verification machinery — the witness-authoring stage
/// and its fail→pass flip oracle. Before #3381 they always reached
/// [`PipelineChoice::Classic`], because that was the default; after, `stella
/// run` with no `--pipeline` resolves to [`PipelineChoice::Raw`], whose
/// `run_raw_one_shot` does not accept `keep_witness`/`require_verified` as
/// parameters at all and only threads `test_command` to an installed wrapper's
/// own oracle. Silently dropping the flag the caller asked for is exactly the
/// expedient CLAUDE.md forbids, so this refuses before dispatch instead of
/// letting the run start and the flag do nothing — and it never silently
/// implies `classic`, which would run something other than what was asked
/// for without saying so.
///
/// `test_command` is meaningful on [`PipelineChoice::Plugin`] too — it arms a
/// bound wrapper's own `[oracle]` flip check (#3553) — so only that variant
/// passes it through; `keep_witness`/`require_verified` remain pipeline-only
/// today and are refused on `Plugin` exactly as on `Raw`, both naming
/// `--pipeline classic` as the remedy.
pub(crate) fn reject_verification_flags_without_pipeline(
    choice: PipelineChoice<'_>,
    test_command: Option<&str>,
    keep_witness: bool,
    require_verified: bool,
) -> Result<(), String> {
    if choice.is_classic() {
        return Ok(());
    }
    let mut offending: Vec<&str> = Vec::new();
    if choice.is_raw() && test_command.is_some() {
        offending.push("--test-command");
    }
    if keep_witness {
        offending.push("--keep-witness");
    }
    if require_verified {
        offending.push("--require-verified");
    }
    if offending.is_empty() {
        return Ok(());
    }
    let where_run = match choice {
        PipelineChoice::Raw => "the raw loop (no --pipeline)".to_string(),
        PipelineChoice::Plugin(variant) => format!("`--pipeline {variant}`"),
        PipelineChoice::Classic => unreachable!("classic returned Ok above"),
    };
    Err(format!(
        "{} belong{} to the staged pipeline's verification machinery and {} nothing on {where_run}: \
         pass --pipeline classic to run the staged pipeline.",
        offending.join(", "),
        if offending.len() == 1 { "s" } else { "" },
        if offending.len() == 1 { "does" } else { "do" },
    ))
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
///
/// `sub_agents` is this session's own sub-agent dispatcher — the one
/// `crate::subagent::install_for_session` already installed for the `task`
/// tool — taken as a parameter rather than built here for the same reason
/// `recall` is: the decision stays testable without a live provider, and the
/// caller is the one that knows which dispatcher backs this session.
pub(crate) fn resolve(
    workspace_root: &std::path::Path,
    variant: &str,
    sub_agents: Arc<dyn SubAgentDispatcher>,
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
        sub_agents,
        warn,
    )
}

/// A shared handle on the session's sub-agent dispatcher, so [`ChildTurns`]
/// can hold it by value.
///
/// [`ChildTurns::declare`] is generic over its dispatcher rather than taking a
/// trait object, so a caller holding an `Arc<dyn SubAgentDispatcher>` — the
/// shape [`crate::subagent::install_for_session`] hands back, shared with the
/// `task` tool's own dispatcher — needs one line saying the handle itself
/// dispatches. Mirrors [`crate::wrapper_recall::BoxedRecall`]'s reason
/// exactly, for the sibling plane.
struct ArcSubAgents(Arc<dyn SubAgentDispatcher>);

#[async_trait]
impl SubAgentDispatcher for ArcSubAgents {
    async fn dispatch(&self, spec: SubAgentSpec) -> SubAgentOutcome {
        self.0.dispatch(spec).await
    }
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
///
/// `sub_agents` is bound the same way, for `child_turn`: every wrapper gets a
/// [`ChildTurns`] plane over it, declared from the manifest's own `[roles]`
/// table, so a declared role intent spends a real bounded child turn and an
/// undeclared one is refused `Undeclared` before anything runs — the gate does
/// that refusing, this function only ever wires the plane behind it.
pub(crate) fn bind_installed(
    roster: &PluginRoster,
    variant: &str,
    recall: Box<dyn stella_runtime::wrapper::RecallHost>,
    sub_agents: Arc<dyn SubAgentDispatcher>,
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
    // `ChildTurns::declare` reads the same manifest's `[roles]` table and its
    // own `[loop] max_calls` ask, so a plugin that named no role intents can
    // ask for nothing here either.
    let planes =
        HostPlanes::recalling(crate::wrapper_recall::BoxedRecall(recall)).with_child_turns(
            ChildTurns::declare(&installed.manifest, ArcSubAgents(sub_agents)),
        );
    let gate = Arc::new(HostCallGate::declare(
        installed.manifest.loop_grant.clone(),
        DEFAULT_HOST_MAX_CALLS,
        Box::new(planes),
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

        let err =
            reject_verification_flags_without_pipeline(PipelineChoice::Raw, None, true, false)
                .expect_err("--keep-witness does nothing on the raw loop");
        assert!(err.contains("--keep-witness"), "{err}");

        let err =
            reject_verification_flags_without_pipeline(PipelineChoice::Raw, None, false, true)
                .expect_err("--require-verified does nothing on the raw loop");
        assert!(err.contains("--require-verified"), "{err}");
    }

    /// The same three flags are accepted once `--pipeline classic` selects
    /// the staged pipeline — the refusal only fires against a resolution
    /// that cannot honor the flag, never against `Classic` itself.
    #[test]
    fn verification_flags_are_accepted_with_pipeline_classic() {
        reject_verification_flags_without_pipeline(
            PipelineChoice::Classic,
            Some("pytest"),
            true,
            true,
        )
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
}
