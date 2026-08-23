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
//! 4. **A child-turn plane** (#3576). A plugin that declares
//!    `[loop] calls = ["child_turn"]` and a `[roles.<name>]` gets a real model
//!    call at that role intent — spent by **this session's own**
//!    `SubAgentDispatcher`, the one `task_assign` runs on, so the plugin never
//!    holds a provider, an `Engine` or a credential (invariant 3,
//!    `doc:turn-loop-wrappers` §9.3). The seat it resolves to is the receipt's
//!    attribution, the child is read-only, the allowance is the host's, and
//!    what it spent is printed beside what it was refused. Two things stated
//!    rather than implied: the `verifier` tier **is** bound here, to
//!    `ModelCallRole::Verdict`, so an arbiter plugin's assessment turn runs
//!    and is booked as what it is (#3838 — [`child_turn_plane`] argues why,
//!    including why the refusal that stood here before was right when it was
//!    written and is not any more); and a point runs between the parent's
//!    turns, where the tool registry's event slot is empty — so the spend
//!    reaches the session's guard and this run's report, but not the store's
//!    receipt (#3802). The turn's boundary controls **do** cross (#3803):
//!    [`dispatch_under_turn_controls`] publishes them for the span of the
//!    dispatch — every round and every point between them — so a plugin's
//!    child parks with a paused parent and stops with a stopped one. What
//!    this door supplies is [`TurnControls::none`], because it is headless
//!    and has no pause gate or steering tap to publish; the seam is what
//!    #3554 needs, and a controlled surface fills it by passing its own.
//!
//! The ordering that makes 4 possible is worth naming, because it is the shape
//! of the split between [`bind_installed`] and [`ResolvedWrapper::serving`]: a
//! `--pipeline` naming nothing installed must fail as a typo before a paid
//! call, while a child-turn plane needs a dispatcher, which needs the provider.
//! [`HostPlanes`] is consumed by value at [`HostCallGate::declare`], so a plane
//! cannot be added to a gate afterwards — the two halves are therefore separate
//! moments, not one function with an `Option` in it.
//!
//! And two scope limits. A plugin's `Unmet` fails the process only under
//! `--require-verdict`, which `stella run` alone offers — the door where an
//! exit status is a delivery gate. `stella goal` and `stella fleet` drive
//! wrappers too and take no such flag yet (#3554 shipped the gate on the
//! one-shot door; see `verdict_gate` for what the default is defending). And
//! only this driver of `doc:wrapper-socket` §6's three exists
//! (#3551). `--pipeline <variant>` itself now reaches every door that takes
//! it — `stella run` here, `stella goal` per judged round
//! (`crate::agent::goal::goal_wrapped`), and `stella fleet` per worker
//! attempt (`crate::fleet_cmd::wrapped`, #3695) — so there is no door left
//! refusing a named variant for want of a driver.
//!
//! # `stella goal`'s driver is a second call site, not a second sequence
//!
//! `crate::agent::goal::goal_wrapped::run_goal_wrapped_turn` binds a wrapper exactly as
//! this module's [`resolve`]/[`ResolvedWrapper::serving`] do, then calls
//! [`WrapperDispatch::run`] once per judged round — the goal loop's own
//! round loop, not the wrapper's, decides how many rounds run, because the
//! goal verifier (`stella_core::Engine::assess`) is untouched (#3695, goal
//! half). That only stays true because [`reject_arbiter_wrapper_on_goal`]
//! refuses an arbiter-grade wrapper on this door before any of the above ever
//! runs (#3832): `WrapperDispatch`'s own hold loop only holds a round open
//! for an arbiter-grade plugin, and a hold-open round dispatched *inside* one
//! already-judged goal round is a second round-holder judging the same
//! round, which the goal loop's own `Engine::assess` was already doing —
//! `run_goal_wrapped_turn` used to only discover this after billing
//! `1 + DEFAULT_HOST_MAX_HOLDS` worker turns for it, then discard the whole
//! run (`DispatchReport::rounds != 1`, still checked as a defense-in-depth
//! assertion — see that module's doc comment). So for the steering/observer
//! wrappers this door does accept, a round's dispatch always drives exactly
//! one internal turn, at the round's own `turn_instance`; and the goal
//! round's own execution row (opened once, before the loop, exactly like
//! [`RawTurnDriver`]'s door opens one) records `bound`'s variant id for the
//! whole run, honest because every round under that row really was
//! dispatched through it.
//!
//! The goal door serves `child_turn` too, through [`round_driver_host`]
//! (#3833). It could not while this module pinned the plane to a fixed
//! `turn_instance`: `stella run`'s one-shot worker never uses more than slot 0,
//! but a goal round's own worker/verifier pair claims a slot beside every
//! round, so a fixed child slot collided with whichever round landed on it.
//! [`stella_core::turn_slots`] is the allocation that settles it, and it is
//! stated once there rather than per door — `turn_instance` is read as a lane
//! plus a sequence within that lane, so a plane counting only its own calls
//! can never land where a door's rounds will. `stella fleet` reaches
//! `child_turn` through the same assembly and the same rule (#3882).

use std::sync::Arc;

use async_trait::async_trait;
use stella_core::budget::BudgetGuard;
use stella_core::estimator::CalibrationMap;
use stella_core::ports::{ToolExecutor, TurnControls};
use stella_core::router::Router;
use stella_core::subagent::SubAgentDispatcher;
use stella_model::provider::Provider;
use stella_plugin::{SignalValues, TurnOutcome as WrapperTurnOutcome};
use stella_protocol::CompletionMessage;
use stella_runtime::wrapper::{
    CandidateFanoutSpend, CandidateFanouts, ChildTurnSpend, ChildTurns, DEFAULT_HOST_MAX_CALLS,
    DEFAULT_HOST_MAX_CHILD_TURNS, DEFAULT_HOST_MAX_HOLDS, DispatchReport, DrivenTurn, HostCallGate,
    HostPlanes, RoundInput, SubprocessWrapper, TurnDriver, TurnPrelude, TurnWrapper,
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

/// What becomes of a run's candidate workspaces when it ends.
mod candidates;
use candidates::ended_abnormally;
/// Every `! wrapper:` line a run prints, in one renderer.
mod report;
use report::{report_to, sweep_lines};
/// Whether a wrapper's conclusion decides the run's exit status (#3554).
mod verdict_gate;
use verdict_gate::verdict_refusal;
// The renderers `report_to` composes internally. Nothing in the shipped binary
// calls them directly — only this module's tests do, through `super::` — so the
// re-export is `#[cfg(test)]` rather than an `#[allow(unused_imports)]`: the
// attribute would assert the lint is wrong, and it is not (AGENTS.md § *Code
// style*, and CLAUDE.md on `#[cfg(test)]` being the better answer).
#[cfg(test)]
use report::{fanout_spend_lines, report_lines, spend_lines};

/// Which wrapper runs over a one-shot turn.
///
/// `doc:pipeline-as-plugins` §7 planned `--pipeline <variant>` to *replace*
/// `--no-pipeline`; `docs/spec/turn-loop-wrappers.md` §5 "Flip the default"
/// (#3381) is that replacement landing — the raw loop is now the default on
/// every door this enum reaches, `--pipeline <variant>` is the sole opt-in,
/// and `--no-pipeline` is a deprecated no-op kept parseable so no script
/// breaks the day this ships (see [`resolve`](Self::resolve) and
/// [`no_pipeline_deprecation_notice`]). Making this an enum rather than a
/// `bool` plus an `Option<&str>` is what keeps "two wrappers both ran"
/// unrepresentable.
///
/// A third variant, `Classic`, named the built-in staged pipeline until that
/// crate was deleted (#3865). It is gone (#3867): `--pipeline classic` is
/// refused at [`resolve`](Self::resolve) with [`classic_removed_message`], so
/// there is no longer any input — real or hand-built in a test — that selects
/// the removed wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PipelineChoice<'a> {
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
    /// nothing left to veto, and `--pipeline <variant>` always wins.
    ///
    /// Fallible again as of #3865: `--pipeline classic`
    /// used to resolve to a `Classic` variant (deleted in #3867); it now
    /// refuses with
    /// [`classic_removed_message`], naming the removal and the wrapper-plugin
    /// remedy, rather than silently accepting a name that no longer runs
    /// anything. Every other name still resolves to [`Self::Plugin`] whether
    /// or not it names something installed — `bind_installed` is where a typo
    /// is actually caught, so this refusal is scoped to exactly the one name
    /// this crate itself used to special-case.
    pub(crate) fn resolve(no_pipeline: bool, pipeline: Option<&'a str>) -> Result<Self, String> {
        let _ = no_pipeline; // deprecated no-op (#3381) — see `no_pipeline_deprecation_notice`
        match pipeline {
            None => Ok(Self::Raw),
            Some(variant) if variant == crate::agent::persistence::PIPELINE_VARIANT_CLASSIC => {
                Err(classic_removed_message())
            }
            Some(variant) => Ok(Self::Plugin(variant)),
        }
    }

    /// Whether this turn runs with **nothing** over it.
    ///
    /// The distinction the enterprise process-free authority turns on, and it
    /// is not the same question as "no plugin was named": that authority admits
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
            Self::Raw => None,
        }
    }
}

/// The refusal `--pipeline classic` owes since the built-in staged pipeline
/// was removed (#3865). Named as its own function, mirroring
/// [`reject_verification_flags_without_pipeline`]'s shape, so
/// [`PipelineChoice::resolve`]'s witness test and every door's own error
/// path read the identical sentence rather than five hand-written copies
/// drifting apart.
pub(crate) fn classic_removed_message() -> String {
    "--pipeline classic no longer runs anything: the built-in staged pipeline has been removed. \
     Install a verification wrapper plugin instead (see `stella plugin install`) and pass \
     `--pipeline <variant>` naming it, or omit --pipeline entirely for the raw loop."
        .to_string()
}

/// The one-line deprecation notice owed when `--no-pipeline` was passed
/// (#3381), or `None` when it was not.
///
/// A pure function returning data rather than printing directly, matching
/// [`crate::engine_config::effort_notices`]'s shape, so
/// [`PipelineChoice::resolve`] stays a plain decision a unit test can call
/// without capturing stderr. `arena.rs` never supervises and calls this
/// directly; a door that can supervise asks [`no_pipeline_notice_for`] instead,
/// which adds the posture rule.
pub(crate) fn no_pipeline_deprecation_notice(no_pipeline: bool) -> Option<&'static str> {
    no_pipeline.then_some(
        "--no-pipeline is deprecated and does nothing: the raw step-loop is the default now. \
         Pass --pipeline <variant> to run an installed wrapper plugin.",
    )
}

/// The notice this process owes, given how it meets the supervisor — `None`
/// when another process's copy is the one the user will read.
///
/// A supervising door re-execs its own argv verbatim into a supervised child,
/// and that child reaches this same decision in its own process under
/// `Foreground`. So the launcher's question is not "am I running the turn?" but
/// "will the child's own copy reach the terminal?", which is
/// [`crate::daemon::detach::Posture::relays_child_console`]:
///
/// - `Attached` — it will, live, over the stream the parent writes to. The
///   parent stays silent or the user reads the line twice (the double-print
///   #3381's audit round fixed).
/// - `Detached` — it will not. `detach::release` drops the handle without
///   following the console, so the child's stderr lands in the run's log file
///   and the launching terminal is told nothing. The parent prints, and the
///   line in the log is that run's own record rather than a second copy of
///   this one (#3774).
/// - `Foreground` — there is no child; this process is the one that runs.
pub(crate) fn no_pipeline_notice_for(
    posture: crate::daemon::detach::Posture,
    no_pipeline: bool,
) -> Option<&'static str> {
    if posture.relays_child_console() {
        return None;
    }
    no_pipeline_deprecation_notice(no_pipeline)
}

/// Refuse an arbiter-grade wrapper plugin on `stella goal`'s pre-flight rung,
/// before binding completes and before any paid call (#3832).
///
/// `stella goal`'s own round loop is this door's completion arbiter:
/// `Engine::assess` (called directly by
/// `crate::agent::goal::goal_wrapped::run_goal_wrapped_turn`, or by
/// `Engine::run_goal` on the raw arm) decides met/unmet after every round
/// already. A wrapper plugin declaring `participation = "arbiter"` brings a
/// SECOND hold loop — [`WrapperDispatch`]'s own `judge`/`again` — that wants
/// to run *inside* one judged round, holding that one round open for up to
/// `1 + DEFAULT_HOST_MAX_HOLDS` billed worker turns before
/// `run_goal_wrapped_turn` ever sees the round back
/// (`DispatchReport::rounds != 1`) — and only then discards the whole run,
/// after every one of those turns was already paid for. Two round-holders
/// judging the same round is exactly the doubled-supervisor shape the
/// wrapper design forbids: an arbiter-grade plugin's designed home is
/// `stella run --pipeline <variant>`, where `WrapperDispatch`'s hold loop is
/// the ONLY thing that owns rounds (see `agent/goal/goal_wrapped.rs`'s
/// module doc, and `plugins/stella-goal/README.md`, for why that plugin runs
/// there and not here). So this refuses before the provider is ever built —
/// the same pre-flight rung [`reject_verification_flags_without_pipeline`]
/// uses — before any paid model call,
/// every time, though not before config load and catalog bootstrap.
///
/// Steering and observer wrappers are unaffected and keep running per round
/// on `stella goal` exactly as before: neither grade can reach `again`'s
/// `Continuation::Again` arm (only [`stella_plugin::Participation::Arbiter`]
/// can, `crates/stella-runtime/src/wrapper/verdict.rs`'s `again`), so their
/// `WrapperDispatch::run` call always returns after exactly one internal
/// turn and the goal loop's own round math is untouched.
pub(crate) fn reject_arbiter_wrapper_on_goal(resolved: &ResolvedWrapper) -> Result<(), String> {
    // Any member being arbiter-grade is enough: a composition holds at most
    // one arbiter (`super::compose` refuses two), and one is what brings the
    // second hold loop this door cannot have.
    let Some(arbiter) = resolved.manifests().find(|manifest| {
        manifest.loop_grant.participation == stella_plugin::Participation::Arbiter
    }) else {
        return Ok(());
    };
    Err(format!(
        "--pipeline {variant} (\"{name}\") is arbiter-grade and cannot run on `stella goal` \
         (#3832): the goal loop is this door's own completion arbiter — Engine::assess decides \
         met/unmet after every round already — and a wrapper that holds rounds open runs its \
         own hold loop via WrapperDispatch, which would judge the same round twice. Run it at \
         its designed home instead: `stella run --pipeline {variant}`. Steering and observer \
         wrappers are unaffected and still run per round on `stella goal`.",
        variant = resolved.variant(),
        name = arbiter.name,
    ))
}

/// Refuse a pipeline-only verification flag against a [`PipelineChoice`] that
/// cannot honor it (#3696).
///
/// `--keep-witness`, `--require-verified`, and `--test-command` all belong to
/// the staged pipeline's verification machinery — the witness-authoring stage
/// and its fail→pass flip oracle. Before #3381 they always reached the
/// built-in staged pipeline, because that was the default; after, `stella run`
/// with no `--pipeline` resolves to [`PipelineChoice::Raw`], whose
/// `run_raw_one_shot` does not accept `keep_witness`/`require_verified` as
/// parameters at all and only threads `test_command` to an installed wrapper's
/// own oracle. Silently dropping the flag the caller asked for is exactly the
/// expedient CLAUDE.md forbids, so this refuses before dispatch instead of
/// letting the run start and the flag do nothing. Since #3865 removed the
/// staged pipeline and #3867 removed the variant that named it, there is no
/// choice left that these flags can be honored on unmodified — every arm below
/// either refuses or hands the flag to a plugin's own oracle.
///
/// `test_command` is meaningful on [`PipelineChoice::Plugin`] too — it arms a
/// bound wrapper's own `[oracle]` flip check (#3553) — so only that variant
/// passes it through; `keep_witness`/`require_verified` remain pipeline-only
/// today and are refused on `Plugin` exactly as on `Raw`, both naming an
/// installed verification wrapper plugin as the remedy.
/// Refuse `--require-verdict` where nothing declares a verdict (#3554).
///
/// Unlike the three flags below, this one is honored — on
/// [`PipelineChoice::Plugin`], where a bound wrapper's `judge` reaches an
/// `Outcome` the flag can read. On the raw loop there is no wrapper and no
/// verdict, so accepting it would be the silent drop CLAUDE.md forbids: the
/// caller asked for a delivery gate and would get an unconditional exit `0`.
pub(crate) fn reject_require_verdict_without_wrapper(
    choice: PipelineChoice<'_>,
    require_verdict: bool,
) -> Result<(), String> {
    if !require_verdict || !choice.is_raw() {
        return Ok(());
    }
    Err(
        "--require-verdict has no verdict to read on the raw loop (no --pipeline): only an \
         installed wrapper plugin declares one. Install one (see `stella plugin install`) and \
         pass --pipeline <variant> naming it."
            .to_string(),
    )
}

pub(crate) fn reject_verification_flags_without_pipeline(
    choice: PipelineChoice<'_>,
    test_command: Option<&str>,
    keep_witness: bool,
    require_verified: bool,
) -> Result<(), String> {
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
    };
    Err(format!(
        "{} belong{} to the staged pipeline's verification machinery and {} nothing on {where_run}: \
         install a verification wrapper plugin instead (see `stella plugin install`) and pass \
         --pipeline <variant> naming it — `--pipeline classic` no longer runs anything.",
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
    /// One host-call gate per member, in selection order, kept so
    /// [`HostCallGate::refusals`] reaches a surface after the run.
    ///
    /// Per member rather than shared, because a gate carries one plugin's
    /// `[loop]` grant — see [`ResolvedWrapper::serving`].
    gates: Vec<Arc<HostCallGate>>,
    /// The child-turn planes this host installed, kept for the same reason as
    /// the gates beside them: a plane is the only thing that knows what a
    /// plugin spent, and a driver that installed one and could not report on
    /// it would be silent about money (#3576). One per member that was given
    /// one; empty on a door that installs none.
    child_turns: Vec<Arc<SessionChildTurns>>,
    /// The candidate fan-out planes, kept for the child planes' reason and one
    /// more of their own: a plane is the only thing that knows which isolated
    /// workspaces are still on disk, so a driver that dropped it would leak
    /// every unadopted candidate of the run (#3892).
    candidate_fanout: Vec<Arc<SessionCandidateFanouts>>,
}

impl BoundWrapper {
    /// The variant id this wrapper runs under.
    ///
    /// Owned rather than borrowed since #3801: a composition's id is its
    /// members' ids joined, which is assembled on demand rather than stored.
    pub(crate) fn variant(&self) -> String {
        self.dispatch.variant()
    }

    /// Every child turn this host ran on the plugin's behalf, in order.
    ///
    /// Empty both when the plugin never asked and when no plane was installed;
    /// the two are told apart by [`HostCallGate::refusals`], which records the
    /// `Unavailable` the second case answers with.
    pub(crate) fn child_spends(&self) -> Vec<ChildTurnSpend> {
        self.child_turns
            .iter()
            .flat_map(|plane| plane.spends())
            .collect()
    }

    /// Every candidate fan-out this host performed, in order.
    ///
    /// [`Self::child_spends`]' sibling, and the larger number of the two: a
    /// fan-out is N *writing* worker turns bought on one host call, so a run
    /// that reported child turns and stayed silent about fan-outs would be
    /// visible about the cheap spend and quiet about the expensive one.
    pub(crate) fn fanout_spends(&self) -> Vec<CandidateFanoutSpend> {
        self.candidate_fanout
            .iter()
            .flat_map(|plane| plane.spends())
            .collect()
    }

    /// The one member's gate, for a test that bound exactly one plugin.
    ///
    /// `#[cfg(test)]` rather than an accessor the crate ships: production
    /// reads every gate ([`Self::report`]), and a "the first one" accessor on
    /// the shipping surface is how a composition would come to be reported
    /// through one member's refusals.
    #[cfg(test)]
    fn gate(&self) -> &Arc<HostCallGate> {
        assert_eq!(
            self.gates.len(),
            1,
            "this helper is for a one-member selection; a composition has a gate per member"
        );
        &self.gates[0]
    }

    /// Print what one round's dispatch concluded, exactly as [`run_wrapped`]
    /// does for `stella run`'s one-shot report.
    ///
    /// A method rather than exposing `gate` — the field [`report_to`] reads
    /// alongside `report` — because the gate outlives any single round and a
    /// caller driving several rounds through this same [`BoundWrapper`] (the
    /// goal loop) must never hold a second reference to it that could drift
    /// from what [`Self::child_spends`] already reports honestly.
    ///
    /// `scope` is the lane these lines belong to, `None` on a door where there
    /// is only one — see [`report_to`].
    pub(crate) fn report(
        &self,
        scope: Option<&str>,
        format: OutputFormat,
        report: &DispatchReport,
    ) {
        report_to(
            scope,
            format,
            report,
            &self.gates,
            &self.child_spends(),
            &self.fanout_spends(),
        );
    }
}

/// The child-turn plane a `stella run` session installs: the plugin's declared
/// role intents over **this session's own** sub-agent dispatcher.
///
/// `Arc<dyn SubAgentDispatcher>` rather than the concrete
/// [`crate::subagent::SessionSubAgents`] because that is what
/// [`crate::subagent::install_for_session`] hands back, and a session installs
/// exactly one dispatcher — a second would be a second pool, a second carve
/// and a second ledger over one session's money.
pub(crate) type SessionChildTurns = ChildTurns<Arc<dyn SubAgentDispatcher>>;

/// The candidate fan-out plane a `stella run` session installs: the plugin's
/// declared role intents over **this session's own** worktree substrate.
pub(crate) type SessionCandidateFanouts =
    CandidateFanouts<crate::candidate_workspaces::CandidateSubstrate>;

/// This host's child-turn plane for one installed plugin.
///
/// Three deliberate decisions live here rather than at the call site, because
/// each is a claim about the user's money or the receipt it lands on (#3576):
///
/// - **The `verifier` seat is bound here, to `ModelCallRole::Verdict`**
///   (#3838). [`ChildTurns`] serves `worker`, `triage`, `research` and `plan`
///   by default and deliberately not `verifier`; `ChildTurns::with_seat`'s
///   contract is that a host wanting one *says so and owns the claim*. This
///   host says so.
///
///   It reads as a reversal of what stood here before, so the two premises
///   that changed are worth naming. The old refusal reasoned that attributing
///   a plugin's child turn to `Verdict` "would put a call on the receipt the
///   pipeline itself did not make", `Verdict` being the model verdict #2584
///   removed structurally.
///
///   1. **There is no pipeline receipt to protect.** The built-in staged
///      pipeline was deleted from this workspace (#3865) and
///      `--pipeline classic` is refused outright, so no host-run verification
///      stage exists to be misattributed to.
///   2. **`Verdict` is already this exact call's role.**
///      `stella_core::goal`'s own loop stamps `role: ModelCallRole::Verdict`
///      on its independent verifier call, and its test asserts the durable
///      sequence `[Worker, Verdict, Worker, Verdict]` precisely so a worker
///      and a verifier call stay distinguishable on the receipt. A goal-
///      supervision plugin's verifier turn is the same kind of call, so
///      `Verdict` is the *accurate* attribution, not a borrowed one.
///
///   What this does **not** buy: authority. Nothing branches on
///   `ModelCallRole` — `stella_core::subagent` carries it onto the receipt as
///   `call_role: spec.role` and no code reads it back to decide anything. The
///   seat decides what the call is *called*, never what it may *do*. The
///   independence refusal still compares the resolved seat, so a plugin
///   cannot reach the worker's seat by renaming it (see the sibling test
///   `a_role_intent_pointing_at_the_worker_is_still_forbidden`).
///
///   Bound for any plugin declaring `tier = "verifier"`, not for one plugin
///   id: which capabilities a plugin holds is a property of what its manifest
///   declares and the user consented to, never of its name.
///
///   The deeper problem this does not solve: that this host has an opinion
///   about the word `verifier` at all. The seat table
///   (`ChildTurns::default_seats`) is a core-owned list of *plugin* role
///   names, so a plugin needing a `planner` or a `reviewer` can still only be
///   served by a name core already knows. This adds a fifth known name; it
///   does not make seats opaque. That is #3905, under epic #3903, and it
///   subsumes this line when it lands.
/// - **No per-turn USD carve is requested.** `None` asks for the parent's
///   whole remaining headroom, which `BudgetGuard::carve` clamps — and the
///   dispatcher behind it carves from the session's sub-agent pool
///   ([`crate::subagent::session_pool_limit_usd`]), so "unbounded ask" is
///   still a bounded spend.
/// - **The host's own ceiling stands.** `[loop] max_child_turns` is the
///   plugin's ask and [`stella_runtime::wrapper::DEFAULT_HOST_MAX_CHILD_TURNS`]
///   the authority; neither is overridden here.
pub(crate) fn child_turn_plane(
    manifest: &stella_plugin::PluginManifest,
    dispatcher: Arc<dyn SubAgentDispatcher>,
) -> SessionChildTurns {
    ChildTurns::declare(manifest, dispatcher)
        .in_turn_lane(stella_core::turn_slots::CHILD_TURN_LANE)
        .with_seat("verifier", stella_protocol::event::ModelCallRole::Verdict)
}

/// This host's candidate fan-out plane for one installed plugin (#3892).
///
/// Three decisions, and they are [`child_turn_plane`]'s three read against a
/// capability whose unit is a *writing* worker turn rather than a read:
///
/// - **Only `worker` may be fanned out to, and this host binds no extra
///   tier.** [`CandidateFanouts`] serves the same four tiers
///   [`ChildTurns`] does and refuses every one that does not resolve to the
///   worker's seat, which is the inverse of the child plane's rule and the
///   reason both exist: a child turn is evidence *about* the work and must
///   not be graded by the model that did it, while a candidate **is** the
///   work and must not be booked to a responsibility that wrote nothing.
///   Nothing is bound here, so that rule stands as core wrote it.
/// - **No per-fan-out USD carve is requested.** `None` asks for the whole
///   headroom, divided by the clamped width, and each share is clamped again
///   by the substrate against the session's sub-agent pool
///   ([`crate::subagent::session_pool_limit_usd`]) — so "unbounded ask" is
///   still a bounded spend, exactly as it is for a child turn. Requesting a
///   number here would be this driver inventing a second ceiling that no flag
///   can raise.
/// - **The host's own two ceilings stand.** `[loop] max_fanout_width` is the
///   plugin's ask and
///   [`stella_runtime::wrapper::DEFAULT_HOST_MAX_FANOUT_WIDTH`] the authority;
///   [`stella_runtime::wrapper::DEFAULT_HOST_MAX_FANOUTS`] bounds the run.
///   Neither is overridden.
pub(crate) fn candidate_fanout_plane(
    manifest: &stella_plugin::PluginManifest,
    workspaces: crate::candidate_workspaces::CandidateSubstrate,
) -> SessionCandidateFanouts {
    CandidateFanouts::declare(manifest, workspaces)
        .in_turn_lane(stella_core::turn_slots::FANOUT_LANE)
}

/// What this host will do for a plugin, assembled after the resources exist.
///
/// Separate from [`ResolvedWrapper`] because the two are built at different
/// moments and cannot be merged without losing one of the properties: the
/// wrapper is resolved *before* the provider is built, so a `--pipeline` that
/// names nothing installed fails as a typo rather than after a paid run, while
/// a child-turn plane needs the session's dispatcher, which needs the
/// provider. [`HostPlanes`] is consumed by value at
/// [`HostCallGate::declare`], so a plane cannot be added to a gate afterwards
/// — the host is therefore assembled whole, once, here.
pub(crate) struct WrapperHost {
    recall: Box<dyn stella_runtime::wrapper::RecallHost>,
    child_turns: Option<Arc<SessionChildTurns>>,
    candidate_fanout: Option<Arc<SessionCandidateFanouts>>,
}

impl WrapperHost {
    /// A host serving `recall` from this workspace's context plane and nothing
    /// else — what a driver with no dispatcher of its own can offer.
    pub(crate) fn recalling(recall: Box<dyn stella_runtime::wrapper::RecallHost>) -> Self {
        Self {
            recall,
            child_turns: None,
            candidate_fanout: None,
        }
    }

    /// Also serve `child_turn` from this plane.
    #[must_use]
    pub(crate) fn with_child_turns(mut self, plane: Arc<SessionChildTurns>) -> Self {
        self.child_turns = Some(plane);
        self
    }

    /// Also serve `candidate_fanout` and `adopt_candidate` from this plane.
    #[must_use]
    pub(crate) fn with_candidate_fanout(mut self, plane: Arc<SessionCandidateFanouts>) -> Self {
        self.candidate_fanout = Some(plane);
        self
    }
}

/// Everything a `stella run` session can do for the plugin wrapping it.
///
/// The one place both planes are named together, so a driver cannot install
/// half of them by accident. A door that installs half of them on purpose says
/// so through [`round_driver_host`] instead.
pub(crate) fn session_host(
    cfg: &crate::config::Config,
    manifest: &stella_plugin::PluginManifest,
    dispatcher: Arc<crate::subagent::SessionSubAgents>,
) -> WrapperHost {
    let workspaces = crate::candidate_workspaces::CandidateSubstrate::for_session(
        cfg,
        &manifest.name,
        Arc::clone(&dispatcher),
    );
    WrapperHost::recalling(Box::new(crate::wrapper_recall::SessionRecallHost::open(
        &cfg.workspace_root,
    )))
    .with_child_turns(Arc::new(child_turn_plane(manifest, dispatcher)))
    .with_candidate_fanout(Arc::new(candidate_fanout_plane(manifest, workspaces)))
}

/// What a door that drives **several rounds under one execution row** can do
/// for the plugin wrapping it: `recall` and `child_turn` (#3833, #3882).
///
/// `stella goal` (per judged round) and `stella fleet` (per worker attempt)
/// both took [`WrapperHost::recalling`] alone until the slot rule existed, so a
/// plugin declaring `[loop] calls = ["child_turn"]` was answered `Unavailable`
/// on either door however its manifest was written. What blocked them was one
/// allocation, not two: a fixed child-turn slot collided with whichever of the
/// door's own rounds landed on it, and neither door could hand the plane a
/// per-round slot because a plugin's points run *between* the rounds they are
/// about. [`stella_core::turn_slots`] settles it by residue instead — the plane
/// counts only its own calls and still never lands on a round's slot — and both
/// doors reach `child_turn` through this one assembly.
///
/// `recall_root` is the workspace the context plane is read from, and it is a
/// parameter because the two doors disagree about it honestly: `stella goal`
/// runs in the workspace it was invoked from, while a fleet attempt may run in
/// a fresh worktree that has no context plane of its own and must read the
/// invocation root's (`crate::fleet_cmd::wrapped`'s module doc).
///
/// **No candidate fan-out plane, and no longer for want of a slot.**
/// [`stella_core::turn_slots::FANOUT_LANE`] is reserved beside every round the
/// same way the child lane is, so the collision that justified declining it is
/// gone. What is left is the capability itself: a fan-out is N *writing* worker
/// turns in isolated trees plus an adoption that lands one of them, and both
/// doors already have a writer in the tree — a goal round's own worker turn, a
/// fleet attempt's — so an adoption landing mid-loop would apply a diff over a
/// tree the loop is still mutating. That is a decision about the doors, not
/// about receipt keys, and it is left to whoever takes it rather than shipped
/// as a side effect of the slot rule (#3892 is where the plane came from).
pub(crate) fn round_driver_host(
    recall_root: &std::path::Path,
    manifest: &stella_plugin::PluginManifest,
    dispatcher: Arc<dyn SubAgentDispatcher>,
) -> WrapperHost {
    WrapperHost::recalling(Box::new(crate::wrapper_recall::SessionRecallHost::open(
        recall_root,
    )))
    .with_child_turns(Arc::new(child_turn_plane(manifest, dispatcher)))
}

/// An installed wrapper plugin, found and start-able, before this host has
/// anything to serve it with.
///
/// The half of binding that needs no provider, no registry and no dispatcher,
/// so it can run first — see [`WrapperHost`] for why the split exists.
pub(crate) struct ResolvedWrapper {
    /// The members of the selection, in the order the user named them. Never
    /// empty — [`bind_installed`] refuses a selection that resolves to
    /// nothing rather than building one.
    members: Vec<ResolvedMember>,
    variant: String,
}

/// One plugin of a selection: the manifest a human consented to, and the
/// process that answers for it.
struct ResolvedMember {
    manifest: stella_plugin::PluginManifest,
    wrapper: SubprocessWrapper,
}

impl std::fmt::Debug for ResolvedWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedWrapper")
            .field(
                "plugins",
                &self
                    .members
                    .iter()
                    .map(|member| member.manifest.name.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("variant", &self.variant)
            .finish_non_exhaustive()
    }
}

impl ResolvedWrapper {
    /// The manifests a human consented to at install, in selection order.
    pub(crate) fn manifests(&self) -> impl Iterator<Item = &stella_plugin::PluginManifest> {
        self.members.iter().map(|member| &member.manifest)
    }

    /// The `--pipeline` value that resolved this wrapper — the same string
    /// [`resolve`] was called with, kept for callers (like
    /// [`reject_arbiter_wrapper_on_goal`]) that need to name it in a refusal
    /// without re-deriving it from the manifest's own `[wrapper] id`, which
    /// need not match what the user typed under every alias scheme #3512
    /// leaves room for.
    pub(crate) fn variant(&self) -> &str {
        &self.variant
    }

    /// Bind every member's process to what this host will do for it.
    ///
    /// # A host plane per member, and why cloning one will not do
    ///
    /// `host_for` is asked **once per member**, with that member's own
    /// manifest, rather than taking one assembled [`WrapperHost`] and sharing
    /// it. That is a privilege boundary, not a construction detail:
    /// [`child_turn_plane`] reads the manifest's `[roles]` and `[loop]
    /// max_calls`, so a plane built from member A's manifest and handed to
    /// member B would let B name A's role intents and spend against A's
    /// allowance. The gate is per member for the same reason — the `[loop]`
    /// grant is the authoritative filter and it belongs to one plugin.
    ///
    /// `recall` is a host capability with no manifest-derived narrowing, so a
    /// caller may hand every member an equivalent one; the child-turn and
    /// candidate-fanout planes may not be shared, and this signature is what
    /// stops a caller doing it by accident.
    ///
    /// # Errors
    ///
    /// A selection whose members' declared stage orders cannot be reconciled
    /// — a contradictory order, or two arbiters (`super::compose`). A single
    /// validated manifest cannot hit either, since the variant was found by
    /// its `[wrapper] id`.
    pub(crate) fn serving(
        self,
        host_for: impl Fn(&stella_plugin::PluginManifest) -> WrapperHost,
    ) -> Result<BoundWrapper, String> {
        let variant = self.variant;
        let mut members: Vec<(stella_plugin::PluginManifest, Arc<dyn TurnWrapper>)> =
            Vec::with_capacity(self.members.len());
        let mut gates = Vec::with_capacity(self.members.len());
        let mut child_turns = Vec::new();
        let mut candidate_fanout = Vec::new();
        for member in self.members {
            let host = host_for(&member.manifest);
            let mut planes = HostPlanes::recalling(crate::wrapper_recall::BoxedRecall(host.recall));
            if let Some(plane) = &host.child_turns {
                planes = planes.with_child_turns(Arc::clone(plane));
            }
            if let Some(plane) = &host.candidate_fanout {
                planes = planes.with_candidate_fanout(Arc::clone(plane));
            }
            // The manifest's own `[loop]` grant is the authoritative filter —
            // an undeclared capability is refused before this host performs
            // anything — and `DEFAULT_HOST_MAX_CALLS` clamps whatever
            // allowance it asked for.
            let gate = Arc::new(HostCallGate::declare(
                member.manifest.loop_grant.clone(),
                DEFAULT_HOST_MAX_CALLS,
                Box::new(planes),
            ));
            members.push((
                member.manifest,
                Arc::new(member.wrapper.serving(Arc::clone(&gate))) as Arc<dyn TurnWrapper>,
            ));
            gates.push(gate);
            child_turns.extend(host.child_turns);
            candidate_fanout.extend(host.candidate_fanout);
        }
        let dispatch = WrapperDispatch::bind_composed(members)
            .map_err(|error| format!("wrapper \"{variant}\" cannot be driven: {error}"))?;
        Ok(BoundWrapper {
            dispatch,
            gates,
            child_turns,
            candidate_fanout,
        })
    }
}

/// Read what is installed and resolve the wrapper `variant` names.
///
/// The impure half — it reads the two plugin tiers and the settings that
/// retract them — kept apart from [`bind_installed`], which is the decision and
/// is therefore the half the tests drive.
///
/// # Errors
///
/// Whatever [`bind_installed`] refuses.
pub(crate) fn resolve(
    workspace_root: &std::path::Path,
    variant: &str,
    warn: &mut dyn FnMut(String),
) -> Result<ResolvedWrapper, String> {
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
///
/// It stops short of the [`HostCallGate`]: this half runs before the provider
/// exists, so it cannot yet build the child-turn plane that needs a
/// dispatcher. [`ResolvedWrapper::serving`] is the other half, and every
/// wrapper reaches it — a plugin that asks through a transport with *no* gate
/// has its stdin shut and waits for an answer that never comes (#3561).
/// Say so when this host will fund less than the manifest asked for (#3841).
///
/// `[loop] max_holds` and `max_child_turns` are **asks, never authorities** — the
/// host's own ceilings are what actually bound the spend, and that is the
/// right way round: a plugin that could raise its own ceiling by declaring a
/// bigger number would be setting the user's budget for them.
///
/// The defect is not the clamp, it is the *silence*. A user installing
/// `stella-goal` reads "asks for up to 7 correction rounds" in the consent
/// text and gets 2, with nothing naming the difference as a host default
/// rather than something they chose. `again`'s
/// `StopReason::AllowanceSpent { spent, allowed }` reports it honestly, but
/// only at the end of a run that already hit the wall.
///
/// So the narrowing is announced before the first round, in the same
/// one-line-notice shape invariant 8 uses for a pinned effort a provider
/// cannot honour: never a silent drop.
///
/// Deliberately **not** a refusal. A plugin capped below its ask still does
/// its job, just fewer times, and refusing to run it would turn a visible
/// narrowing back into an unusable door.
fn warn_narrowed_ceilings(manifest: &stella_plugin::PluginManifest, warn: &mut dyn FnMut(String)) {
    let name = manifest.name.as_str();
    if let Some(asked) = manifest.loop_grant.max_holds
        && asked > DEFAULT_HOST_MAX_HOLDS
    {
        warn(format!(
            "plugin \"{name}\" asks to hold a turn open for up to {asked} correction \
             rounds; this host funds {DEFAULT_HOST_MAX_HOLDS} (a host default, not a \
             setting you chose), so the run stops after {} rounds",
            DEFAULT_HOST_MAX_HOLDS + 1
        ));
    }
    // The same number `ChildTurns::declare` reads, resolved the same way: the
    // whole-run key when the manifest has one, and the per-point number for a
    // manifest written before the two were split (#3839). Asking a different
    // question here than the plane asks is how a notice comes to describe a
    // clamp that did not happen.
    if let Some(asked) = manifest
        .loop_grant
        .max_child_turns
        .or(manifest.loop_grant.max_calls)
        && asked > DEFAULT_HOST_MAX_CHILD_TURNS
    {
        warn(format!(
            "plugin \"{name}\" asks for up to {asked} child turns for the whole run; \
             this host funds {DEFAULT_HOST_MAX_CHILD_TURNS}"
        ));
    }
}

/// Resolve a whole `--pipeline` selection: one id, or several separated by
/// commas, in the order the user meant them to run.
///
/// The order is the selection's, because nothing else in the system knows it
/// (#3801, #4094). `--pipeline research-v1,plan-v1` states that grounding
/// comes before planning; no manifest vocabulary says so, and none is added
/// here.
///
/// # Errors
///
/// The first id that names nothing installed, phrased as
/// [`resolve_member`] phrases it. A **repeated** id is refused too: it is a
/// typo, not a request to run a plugin twice — a second copy would compose
/// against itself, spend a second gate's allowance and print every refusal
/// twice.
pub(crate) fn bind_installed(
    roster: &PluginRoster,
    variant: &str,
    warn: &mut dyn FnMut(String),
) -> Result<ResolvedWrapper, String> {
    let mut members = Vec::new();
    let mut named: Vec<&str> = Vec::new();
    for id in variant.split(',').map(str::trim) {
        if id.is_empty() {
            return Err(format!(
                "`--pipeline {variant}` has an empty entry — a selection is one wrapper id, \
                 or several separated by commas in the order they should run"
            ));
        }
        if named.contains(&id) {
            return Err(format!(
                "`--pipeline {variant}` names \"{id}\" more than once — a selection runs each \
                 plugin once, in the order given"
            ));
        }
        named.push(id);
        members.push(resolve_member(roster, id, warn)?);
    }
    Ok(ResolvedWrapper {
        members,
        variant: variant.to_string(),
    })
}

/// Find the one installed plugin declaring `variant`, and declare its
/// transport.
fn resolve_member(
    roster: &PluginRoster,
    variant: &str,
    warn: &mut dyn FnMut(String),
) -> Result<ResolvedMember, String> {
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

    warn_narrowed_ceilings(&installed.manifest, warn);

    // `${plugin_dir}` is the host's substitution — this crate is where the
    // install directory is known, exactly as `PluginRoster::hook_routes` does
    // it for hooks, and through the same shared expander (#4301).
    let argv: Vec<String> = runtime
        .argv
        .iter()
        .map(|arg| stella_plugin::expand_plugin_dir(arg, &installed.dir))
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
    Ok(ResolvedMember {
        manifest: installed.manifest.clone(),
        wrapper: admitted.wrapper,
    })
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
///   **The consequence is a manifest author's, and it is not a small one**
///   (#3547). [`Wrapper::resolve`] drops a stage whose condition is false, so
///   a manifest gating on a triage signal declares a stage this host will
///   never ask it to contribute at: installed, selected, dispatched, silently
///   useless. Both first-party plugins shipped that way — `plan-v1` end to
///   end, because the gated stage was the only one it answered at — until the
///   conditions came off. `doc:wrapper-socket` §5 states the rule where a
///   plugin author meets it. Producing real values here is a paid triage call
///   on the door whose design point is that the raw loop is the default; that
///   is a cost decision, not a refactor, and it is why the answer was to stop
///   transcribing a rule nothing enforces rather than to buy one.
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
    /// This turn's opening recall: the `ContextRecall` event, spent on the
    /// first round only — recall runs once per session, and re-emitting it on
    /// a held-open round would claim a retrieval that never happened — and the
    /// opening block's produced handles, which seed EVERY round's re-query
    /// (#4498): the block is in `messages` for every round alike.
    pub(crate) recall: crate::memory::OpeningRecall,
    /// The session memory, for the execution stamp and skill-usage record.
    pub(crate) memory: Option<&'a mut SessionMemory>,
    /// The artifacts this host pinned before the run, and the finding it
    /// reports about them after each turn (#3553).
    pub(crate) watch: &'a crate::wrapper_candidate::TamperWatch,
    /// This turn's boundary controls — the pause gate and the soft stop that
    /// govern it — published on the registry for the span of the dispatch so
    /// every child dispatched underneath honours them (#3803).
    ///
    /// A field rather than something this driver derives, because a driver
    /// cannot invent the seam: it belongs to whichever surface has a human
    /// behind it. `crate::agent::goal::run_raw_one_shot` supplies
    /// [`TurnControls::none`] — that path is headless, publishes no pause gate
    /// and installs no steering tap, so there is nothing there to honour. A
    /// controlled surface driving a wrapped turn (the deck, #3554) supplies its
    /// own, exactly as `command_deck::lead_control::turn_controls` already does
    /// for the turns it drives directly.
    pub(crate) controls: TurnControls,
    /// What each round's turn returned, in order — the caller's own view of a
    /// loop the dispatcher owns. `Ok(true)` is a turn that *finished*;
    /// `Ok(false)` is one that aborted, which is the ending
    /// [`ended_abnormally`] is about.
    pub(crate) results: Vec<Result<bool, CliFailure>>,
    /// One friction ledger per turn this driver drove, in order (#3976).
    ///
    /// Borrowed, like the conversation and the budget beside it, because the
    /// caller reflects with it after the dispatch returns and the dispatcher
    /// owns the loop in between.
    ///
    /// **It covers the turns this driver drove and nothing else.** A plugin's
    /// own model calls are bought through the host-call channel at its points,
    /// which run *between* these turns; they are reported by
    /// [`BoundWrapper::child_spends`] and are deliberately not folded in here,
    /// because a ledger built from this host's journal would describe them as
    /// steps of a turn they were not part of.
    pub(crate) friction: &'a mut Vec<crate::memory::TurnFriction>,
}

#[async_trait(?Send)]
impl TurnDriver for RawTurnDriver<'_> {
    async fn run_turn(&mut self, prelude: TurnPrelude) -> DrivenTurn {
        // Before the turn, which is the whole of what "authoring time" means
        // here: the artifacts this round's stages declared are pinned at the
        // identity they have now, so the comparison after the turn is about
        // the work (#3587). Per round rather than once, because a wrapper may
        // declare more as it learns more; already-pinned artifacts keep their
        // first baseline, so a later round cannot launder an earlier rewrite.
        self.watch.pin_declared(prelude.witness());
        // Invariant 7, at the one call site that spends it: `into_messages`
        // hands back user messages, and they are appended *after* the
        // byte-stable system prefix the conversation already opens with.
        self.messages.extend(prelude.into_messages());
        // One observer per round: `tools` and `changed_files` are facts about
        // *this* turn, and a fold shared across rounds would report the first
        // round's tools as the third round's (#3552).
        let facts = crate::turn_facts::TurnFacts::new();
        // One ledger per round, for `facts`' reason: friction is a fact about
        // *this* turn, and a fold shared across rounds would report the first
        // round's retries as the third's (#3552, #3976).
        let mut friction = crate::memory::TurnFriction::default();
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
            crate::memory::OpeningRecall {
                event: self.recall.event.take(),
                produced: self.recall.produced.clone(),
            },
            self.memory.as_deref_mut(),
            // The host folds what it does see (#3976). This driver runs the
            // turn through the same `run_turn` every raw door uses, so its
            // journal is reachable and the caller reflects with it; the
            // plugin's own model calls are not in it, and the field's doc says
            // so where a reader meets it.
            Some(&mut friction),
        )
        .await;
        self.friction.push(friction);

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
        // Whether the turn *finished*, not merely whether it returned: an
        // abort on the turn budget is `Ok`, and it is exactly the ending after
        // which nothing scored the candidates a plugin fanned out
        // ([`ended_abnormally`], #2651).
        self.results.push(outcome.map(|_| turn.completed));
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
/// Returns the last round's result, and — under `require_verdict` — the
/// wrapper's own conclusion on top of it (#3554). Without the flag a
/// `DispatchReport` whose outcome is `Unmet` is printed and exits `0`, which
/// is deliberate: installing a third party's manifest must not by itself gain
/// the power to fail somebody's build. `crate::wrapper_plugin::verdict_gate`
/// carries the argument and the wording.
///
/// The turn's own failure wins when both fire. A run that aborted has a more
/// specific thing to say than "the wrapper was not satisfied", and the wrapper
/// was very likely not satisfied *because* it aborted.
///
/// # Errors
///
/// The turn's own failure, a wrapper whose declared stage order could not be
/// resolved — which a validated manifest cannot hit — or, under
/// `require_verdict`, an outcome that is not `Met`.
pub(crate) async fn run_wrapped(
    bound: &BoundWrapper,
    goal: &str,
    signals: SignalValues,
    candidate: Option<stella_plugin::CandidateGrant>,
    require_verdict: bool,
    mut driver: RawTurnDriver<'_>,
) -> Result<(), CliFailure> {
    let format = driver.format;
    // Before anything is minted, so what this names is only ever a run that is
    // already over (#2813). Nothing is deleted: a leftover checkout is either
    // a crash's residue or a live sibling's, and only the person reading can
    // tell this host which.
    for orphan in bound.orphaned_candidates() {
        eprintln!("  ! wrapper: {orphan}");
    }
    let input = RoundInput {
        goal: goal.to_string(),
        signals,
        // The tree the turn actually runs in — see `crate::wrapper_candidate`
        // for why that is the shared work tree and not an isolated worktree.
        candidate,
    };
    // Copied out of the driver before it is borrowed mutably — both are shared
    // handles, so this costs nothing and keeps the publication above the
    // dispatch rather than inside a round.
    let registry = driver.registry;
    let controls = driver.controls.clone();
    let report =
        dispatch_under_turn_controls(&bound.dispatch, input, registry, controls, &mut driver).await;
    // Whatever a plugin's last point spent has no turn left to fold it in, so
    // this driver folds it (#3576). See `settle_plugin_child_spend`.
    settle_plugin_child_spend(driver.registry, &mut *driver.budget);
    // Before the pop, so the whole run is judged rather than every round but
    // its last.
    let aborted = report.is_err() || ended_abnormally(&driver.results);
    let last = driver.results.pop();
    // A run that ended before anything scored its candidates keeps their work
    // as patches, because the sweep below deletes checkouts and branches
    // unconditionally (#2651). Written, never applied: which candidate
    // deserved to land is the plugin's judgement, and it never made one.
    if aborted {
        for kept in bound.preserve_candidates().await {
            eprintln!("  ! wrapper: {kept}");
        }
    }
    // The end-of-run sweep, and it runs on both arms below because a dispatch
    // that failed is exactly the run most likely to have left workspaces
    // behind. Failures are printed rather than raised: the work is done, and
    // turning "a worktree would not go" into the run's exit status would fail
    // a turn that succeeded.
    // One lane on this door, so no scope tag — and rendered by the shared
    // helper rather than formatted here, so the marker has one producer.
    for line in sweep_lines(None, &bound.sweep_candidates().await) {
        eprintln!("{line}");
    }
    match report {
        Ok(report) => {
            report_to(
                // One wrapper, one lane, one process — nothing to attribute.
                None,
                format,
                &report,
                &bound.gates,
                &bound.child_spends(),
                &bound.fanout_spends(),
            );
            // A round always runs, so `results` always has an entry; an empty
            // one would mean the dispatcher returned without driving anything,
            // which is a report about the wrapper and not about the work.
            last.unwrap_or(Ok(true))?;
            match verdict_refusal(require_verdict, &report.outcome) {
                Some(refusal) => Err(CliFailure::from(refusal)),
                None => Ok(()),
            }
        }
        Err(error) => Err(CliFailure::from(error.to_string())),
    }
}

/// Run a wrapper's whole dispatch with `controls` published on `registry`.
///
/// [`SubAgentDispatcher`]'s contract requires the driver of a turn to hand the
/// engine it builds the *current* turn's controls, and a session-scoped
/// dispatcher can only get them by reading them off the registry at dispatch
/// time ([`ToolRegistry::attach_turn_controls`]). Every other driver in this
/// crate publishes them; the wrapped path did not, so every child it dispatched
/// ran with [`TurnControls::none`] — a paused session that kept spending inside
/// the child, and a soft stop that ended the parent while the child ran on
/// (#3803, the #922 shape one layer out).
///
/// **The span is the dispatch, not the round.** Scoping the guard to
/// `RawTurnDriver::run_turn` would look right and be wrong: a plugin spends its
/// child turns from a *point* — `before_turn`, `after_turn` — and those run
/// between the rounds (#3576), which is precisely when a round-scoped guard has
/// already dropped. So the guard is held here, across every round and every
/// point, and comes down when this function returns — including on an unwind,
/// which is the reason [`stella_tools::subagent::TurnControlsGuard`] clears on
/// drop rather than on an explicit detach.
///
/// Takes `&mut dyn TurnDriver` rather than the concrete [`RawTurnDriver`] so
/// the property is witnessable against a stubbed engine: what the witness needs
/// to observe is what a *child* sees, and that is the same on either driver.
async fn dispatch_under_turn_controls(
    dispatch: &WrapperDispatch,
    input: RoundInput,
    registry: &ToolRegistry,
    controls: TurnControls,
    driver: &mut dyn TurnDriver,
) -> Result<DispatchReport, stella_runtime::wrapper::WrapperError> {
    let _controls = registry.attach_turn_controls(controls);
    dispatch.run(input, driver).await
}

/// Fold what a plugin's child turns spent into the session's own guard.
///
/// The dispatcher settles every child onto the registry's
/// [`SubAgentSpendLedger`](stella_core::subagent::SubAgentSpendLedger) the
/// moment it ends, and an engine turn drains that ledger into the parent's
/// guard at its next step boundary — which is why a `before_turn` spend needs
/// nothing from this function: the round's own turn folds it in. The last
/// point of the last round has no next boundary, so without this the user
/// would pay for a child turn that never reached the guard bounding
/// `--spend-limit`, and the reflection call after the run would size its
/// headroom against money already gone.
///
/// Draining is destructive by contract, so this is exactly-once even though it
/// runs after turns that already drained: what it takes is only what no turn
/// did.
///
/// A door that drives **rounds** calls it after every round's dispatch rather
/// than once at the end (`crate::agent::goal::goal_wrapped`,
/// `crate::fleet_cmd`): a round's `after_turn` spend is folded by the *next*
/// round's turn, so the loop's last round would otherwise leave it unbilled —
/// and those doors read `session_spent_usd` at each round's exit to report the
/// run's cost, which would then be short by whatever the plugin had just
/// spent.
pub(crate) fn settle_plugin_child_spend(
    registry: &dyn ToolExecutor,
    budget: &mut BudgetGuard,
) -> f64 {
    let residual = registry.drain_sub_agent_spend_usd();
    if residual > 0.0 {
        budget.record_spend(residual);
    }
    residual
}

#[cfg(test)]
mod tests;
