// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What this host will do for an installed plugin: the planes, and the two
//! assemblies of them a door picks between.
//!
//! A child module of `wrapper_plugin` rather than more lines in it, for the
//! reason `candidates.rs` beside this file gives and AGENTS.md § "God files"
//! states: the parent sits under the 1500-line ratchet and was within eight
//! lines of it when `run_test` needed a fourth plane (#4536). The parent still
//! owns the *binding* — `ResolvedWrapper::serving` reads these planes off a
//! [`WrapperHost`] and hands each member its own gate — so the fields below are
//! `pub(super)`: visible where the binding happens, and nowhere else.

use std::sync::Arc;

use stella_core::subagent::SubAgentDispatcher;
use stella_runtime::wrapper::{CandidateFanouts, ChildTurns, TestRuns};

use crate::wrapper_test_run::GrantedTestRuns;

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

/// The test-run plane a door installs: the plugin's allowance over the
/// **grants that door already minted** (#4536).
///
/// Over grants rather than over a workspace substrate, because a grant is what
/// every door has and a substrate is what only `stella run` has. See
/// [`crate::wrapper_test_run`] for why that is the honest boundary rather than
/// a convenience.
pub(crate) type SessionTestRuns = TestRuns<GrantedTestRuns>;

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
///   It reads as a reversal of what stood here before, and two premises
///   changed under it. The old refusal reasoned that attributing
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
/// Separate from [`ResolvedWrapper`](super::ResolvedWrapper) because the two are built at different
/// moments and cannot be merged without losing one of the properties: the
/// wrapper is resolved *before* the provider is built, so a `--pipeline` that
/// names nothing installed fails as a typo rather than after a paid run, while
/// a child-turn plane needs the session's dispatcher, which needs the
/// provider. [`HostPlanes`](stella_runtime::wrapper::HostPlanes) is consumed by value at
/// [`HostCallGate::declare`](stella_runtime::wrapper::HostCallGate::declare), so a plane cannot be added to a gate afterwards
/// — the host is therefore assembled whole, once, here.
pub(crate) struct WrapperHost {
    pub(super) recall: Box<dyn stella_runtime::wrapper::RecallHost>,
    pub(super) child_turns: Option<Arc<SessionChildTurns>>,
    pub(super) candidate_fanout: Option<Arc<SessionCandidateFanouts>>,
    pub(super) test_runs: Option<Arc<SessionTestRuns>>,
}

impl WrapperHost {
    /// A host serving `recall` from this workspace's context plane and nothing
    /// else — what a driver with no dispatcher of its own can offer.
    pub(crate) fn recalling(recall: Box<dyn stella_runtime::wrapper::RecallHost>) -> Self {
        Self {
            recall,
            child_turns: None,
            candidate_fanout: None,
            test_runs: None,
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

    /// Also serve `run_test` over the grants this door minted.
    ///
    /// A door with **no** grant installs no plane at all rather than one that
    /// refuses every handle, and the difference is what a plugin's author reads:
    /// no plane is "this host does not do that", while an empty plane would be
    /// "it does, and your handle is wrong" for a handle that was never wrong.
    #[must_use]
    pub(crate) fn with_test_runs(mut self, plane: Arc<SessionTestRuns>) -> Self {
        self.test_runs = Some(plane);
        self
    }
}

/// This host's test-run plane for one installed plugin, over the grants a door
/// holds — or `None` when it holds none.
///
/// The ceilings are not overridden here, on [`child_turn_plane`]'s rule: the
/// manifest's `[loop] max_calls` is the plugin's ask and
/// [`stella_runtime::wrapper::DEFAULT_HOST_MAX_TEST_RUNS`] is the authority, so
/// a number invented at this call site would be a second ceiling no flag can
/// raise. What *is* decided here is that a door with nothing to run installs
/// nothing.
pub(crate) fn test_run_plane<'a>(
    manifest: &stella_plugin::PluginManifest,
    grants: impl IntoIterator<Item = &'a stella_plugin::CandidateGrant>,
) -> Option<SessionTestRuns> {
    let host = GrantedTestRuns::over(grants);
    if host.is_empty() {
        return None;
    }
    Some(TestRuns::declare(manifest, host))
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
///
/// **A test-run plane, where the fan-out plane is refused, and the two are not
/// in tension** (#4536). What disqualifies a fan-out here is adoption: it lands
/// a diff over a tree the loop is still mutating. `run_test` adopts nothing and
/// writes nothing — it runs the invocation the door's own grant already carried,
/// in the tree that grant names, and reports what the assertions said. Both
/// doors mint exactly one such grant, and it is what `granted` carries; a door
/// that minted none (no `--pipeline`, no `--test-command`) installs no plane
/// rather than one that refuses every handle.
pub(crate) fn round_driver_host(
    recall_root: &std::path::Path,
    manifest: &stella_plugin::PluginManifest,
    dispatcher: Arc<dyn SubAgentDispatcher>,
    granted: Option<&stella_plugin::CandidateGrant>,
) -> WrapperHost {
    let host = WrapperHost::recalling(Box::new(crate::wrapper_recall::SessionRecallHost::open(
        recall_root,
    )))
    .with_child_turns(Arc::new(child_turn_plane(manifest, dispatcher)));
    match test_run_plane(manifest, granted) {
        Some(plane) => host.with_test_runs(Arc::new(plane)),
        None => host,
    }
}
