// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The session's sub-agent dispatcher — the host half of the `delegate` tool
//! (#922).
//!
//! `stella-tools`' [`SpawnSubAgent`](stella_tools::subagent::SpawnSubAgent)
//! knows *what* to delegate; this knows *how* to run it. It owns the four
//! things a child turn needs that a tool cannot reach from inside
//! `ToolExecutor::execute`: a provider, a sleeper, the workspace tool set,
//! and a budget.
//!
//! # Breaking the ownership cycle
//!
//! The child's tool set is the very [`ToolRegistry`] that owns this
//! dispatcher, so holding an `Arc` back would leak both forever. The handle
//! is therefore [`Weak`]: a dispatch upgrades it, and a registry that has
//! already been dropped yields
//! [`SubAgentOutcome::Refused`]
//! rather than a panic on a torn-down session.
//!
//! # Session-scoped object, turn-scoped wiring
//!
//! One dispatcher serves the whole session, but a child's metering must land
//! in the stream of the turn that asked for it — and every driver path in
//! this crate already re-points that stream per turn via
//! `ToolRegistry::attach_events`. So rather than re-attaching a dispatcher
//! on every turn (five call sites, each easy to forget), this reads the
//! registry's *current* sender at dispatch time. A child that somehow runs
//! between turns emits into a sink instead of failing.
//!
//! The turn's boundary controls ride the same shape and are read in the same
//! breath — see below.
//!
//! # Budget: a pool, and why it is not the ceiling
//!
//! Each child is carved from a session-scoped pool
//! ([`SessionSubAgents::with_pool_limit`]), which bounds what sub-agents may
//! cost in total. That pool is **not** what enforces `--spend-limit`: every child's
//! cost is also pushed onto the registry's spend ledger, and the engine folds
//! it into the *parent's* guard at the next step-boundary check
//! (`ToolExecutor::drain_sub_agent_spend_usd`). The parent's guard stays the
//! hard ceiling; the pool is a second, tighter bound on this one category of
//! spend.
//!
//! The pool lock is deliberately not held across the child's run, so sibling
//! `delegate` calls from one step execute concurrently — reachable since the
//! engine's dispatch scheduler groups the spawn tool with read-only calls
//! (`Tool::parallel_safe`; before that claim existed, every non-read-only
//! call was its own barrier and this concurrency was dead code). Every
//! sibling in one dispatch group can therefore carve against the same
//! headroom before any settles — an overshoot bounded by one child's cap per
//! concurrently-running sibling (up to the engine's dispatch cap of 8, so
//! seven extra caps in the worst case, not one), and caught by the parent's
//! guard at the next step boundary regardless.
//!
//! ## Settling is the child's job
//!
//! Both ledgers are charged from the child's own thread, the moment its turn
//! ends — not after the `wait.await` below, and not by the `delegate` tool after
//! `dispatch()` returns. Those were the original homes, and both share a
//! defect: a parent hard cancelled mid-`delegate` never resumes either line, so a
//! child that had really spent money landed in *no* ledger at all. Whatever is
//! still running when the parent goes away has to be what settles, and that is
//! the thread. Charging late to the session is this ledger's existing doctrine
//! ("charging the same dollars twice is worse than charging them late");
//! never charging is not.
//!
//! The thread being the sole writer is also what keeps it exactly once.
//!
//! # Why a child runs on its own thread
//!
//! An engine turn's future is deliberately **not** `Send` (the speculation
//! pump boxes futures without the bound), while every tool-call path above
//! it — `Tool::execute`, `ToolExecutor::execute` — is `#[async_trait]` and
//! therefore requires one. A tool consequently cannot simply `.await` a
//! turn. Adding `Send` to the engine's boxed futures does compile, but then
//! proving the whole nested future `Send` trips a rustc higher-ranked
//! lifetime limitation inside `execute_tool_calls` ("implementation of
//! `FnOnce` is not general enough"), so the engine's own hot path is left
//! alone.
//!
//! Instead each child gets a fresh OS thread with a current-thread runtime:
//! `block_on` imposes no `Send` bound, everything handed across is owned
//! (`Arc`s and `Copy` values), and this task only awaits a oneshot. It also
//! buys isolation from a panicking child — **in builds that unwind**. The
//! child turn runs under `catch_unwind`, so a panic settles the child's spend
//! and reports as a refusal instead of tearing the parent's turn down.
//!
//! That claim used to be unqualified, and it was wrong for the builds users
//! actually run: the workspace sets `panic = "abort"` (root `Cargo.toml`), so
//! in release there is no unwind to catch and a child panic takes the whole
//! process with it. A thread boundary cannot contain that; only a process
//! boundary could. Stated rather than quietly implied, because the difference
//! is invisible until it is a crash report (#1850).
//!
//! The cost is one thread per live `delegate` call, bounded by the engine's own
//! tool-call concurrency cap.
//!
//! # Pause and stop reach these children too
//!
//! `TurnGate`/`TurnSteering` are turn-scoped: each driver builds them on the
//! stack of the turn it is about to run, and `Engine` takes both as `&'a dyn`.
//! A session-scoped dispatcher cannot name that lifetime, which is why the
//! first cut of this module could not carry them at all — a paused session
//! kept spending inside a tool-dispatched child, the direct analogue of the
//! `goal.rs::assess` defect #922 named.
//!
//! So each driver now publishes its seams in owned form
//! (`stella_core::ports::TurnControls`) into the registry slot this reads at
//! dispatch time — the same "session-scoped object, turn-scoped wiring"
//! treatment the event sender already gets, for the same reason and at the
//! same call site. The controls come back down when the driver's guard drops,
//! including on an unwind, so a latched soft stop cannot leak into the next
//! turn's children.
//!
//! What a child actually inherits is asymmetric, and deliberately so:
//!
//! - **The pause gate, as-is.** A paused session parks its children at their
//!   own step boundaries. Note that a parked child holds the parent's
//!   `delegate` tool call open, which is exactly right — pause means pause — and
//!   that both release together on resume.
//! - **The soft stop, but not the steering messages.** The child engine is
//!   built through `run_sub_agent`, which wraps whatever steering the parent
//!   has in `stella_core::subagent::ChildSteering`: the stop flag is latched
//!   and non-destructive so forwarding it is free, while `drain_steering` is
//!   destructive by contract and a child that inherited it would eat a
//!   message the user addressed to the parent.
//!
//! Every driver that runs turns installs a dispatcher and publishes what it
//! has: the deck's lead and pipeline turns publish both a steering tap and a
//! `WatchGate` (#1219 — `p` on the lead row parks the turn and, through this
//! dispatcher, every child it dispatched), and deck worker lanes and fleet
//! workers publish their own `WatchGate`, and the wrapped one-shot path
//! publishes whatever its caller supplied for the span of the wrapper's whole
//! dispatch rather than for one round, because a plugin spends its children
//! from the points *between* the rounds (#3803,
//! `crate::wrapper_plugin::dispatch_under_turn_controls`). A driver that
//! publishes nothing is
//! still not a failure state — a child then runs bounded by its step cap and
//! its carve, exactly as before.
//!
//! # A cancelled parent: cascade the intent, not the mechanism
//!
//! A hard cancel is the *dropping of a future*. That is the entire mechanism,
//! and it cannot propagate here: the child is an OS thread running `block_on`,
//! deliberately off the parent's future graph (see above), and a thread cannot
//! be killed safely in Rust. Nor should it be, even if it could — an abrupt
//! kill can land mid-tool, which is exactly the half-finished write the
//! step-boundary rule (L-E6) exists to prevent, and it would discard the
//! salvaged text an aborted child is specifically designed to hand back.
//!
//! So the intent cascades instead of the mechanism. [`ParentGone`] is a drop
//! guard living in the suspended `dispatch` body; when the parent's turn
//! future is dropped, it is dropped too, and [`OrphanStop`] reports that to
//! the child as a soft stop alongside the turn's own. The child ends at its
//! next boundary with its work salvaged and its spend settled — an orphan
//! bounded by the model call already in flight, rather than by its whole step
//! cap.
//!
//! The one thing that genuinely cannot be recovered is the child's *report*:
//! the oneshot receiver went with the cancelled future, so the finding is
//! written to the event stream and nowhere else. Paying for it and stopping
//! promptly beats paying for it and running on.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use stella_core::ports::TurnSteering;
use stella_core::subagent::{
    SubAgentDispatcher, SubAgentHost, SubAgentOutcome, SubAgentSpec, push_sub_agent_spend,
};
use stella_core::{BudgetGuard, Engine, EngineConfig, EventSender};
use stella_protocol::Provider;
use stella_tools::ToolRegistry;

use crate::runtime::TokioSleeper;

/// Default ceiling on what sub-agents may cost across one session, in USD.
///
/// Generous enough that ordinary delegation never trips it, small enough
/// that a model looping on `delegate` cannot quietly spend a session's budget on
/// research. Callers with a real `--spend-limit` get a tighter bound anyway: the
/// parent's guard is the hard ceiling.
pub const DEFAULT_POOL_LIMIT_USD: f64 = 2.0;

/// Marks the parent's dispatch as gone once dropped.
///
/// A guard, because the drop has to happen on the path nothing else covers:
/// when a hard cancel drops the parent's turn future, every local in the
/// suspended `dispatch` body is dropped with it, and this is the only
/// notification a detached child could ever get. On the ordinary path it drops
/// after the child has already reported, where flipping the flag is a no-op.
struct ParentGone(Arc<AtomicBool>);

impl Drop for ParentGone {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The turn's steering, plus "your parent is gone" as one more reason to stop.
///
/// A hard cancel cannot cascade as a hard cancel: the child is an OS thread
/// running `block_on`, not a future in the parent's graph, and a thread cannot
/// be killed safely — nor should it be mid-tool, which is exactly the write
/// the boundary rule (L-E6) exists to protect. So the *intent* cascades
/// instead: the parent's cancel becomes the child's soft stop, taken at its
/// next step boundary with its work salvaged and its spend settled.
///
/// This bounds an orphan to one more model call rather than its full step cap.
struct OrphanStop {
    /// The turn's own tap, when its driver published one.
    turn: Option<Arc<dyn TurnSteering>>,
    parent_alive: Arc<AtomicBool>,
}

impl TurnSteering for OrphanStop {
    /// Never drains the turn's tap. `ChildSteering` already refuses to call
    /// this, but a view that sits between a child and a destructive drain has
    /// no business forwarding it even so — the parent's queued messages are
    /// the parent's.
    fn drain_steering(&self) -> Vec<String> {
        Vec::new()
    }

    fn soft_stop_requested(&self) -> bool {
        !self.parent_alive.load(Ordering::SeqCst)
            || self
                .turn
                .as_ref()
                .is_some_and(|turn| turn.soft_stop_requested())
    }
}

/// The two session-wide answers every child this dispatcher runs is held to:
/// may this tool be called at all, and may *this caller* call it.
///
/// One argument rather than two because they travel together and are read
/// together — `crate::agent::tool_stack` composes them into one chain, and a
/// caller that supplied the switches and forgot the gate would build the
/// half-gated child #3930 closed once already.
pub struct ChildToolPosture {
    /// The operator's `tools.<name>` switches.
    pub policy: stella_tools::policy::ToolPolicy,
    /// The session authorization gate — an installed plugin's accepted grant,
    /// or `NoAuthz` by name (#3482).
    pub gate: Arc<dyn stella_core::ports::AuthzGate>,
}

/// The session's own model wiring: the adapter every seat-less child runs on,
/// and the engine config that names it to the child's engine.
///
/// One struct behind one lock rather than two fields, because a `/model`
/// switch has to move both or neither. Split across two locks, a child
/// dispatched between the two writes would run an engine config naming a model
/// its adapter no longer serves — which is the mismatch, not a narrower window
/// of it.
struct SessionModel {
    /// `Arc`, not `Box`: the provider is moved onto each child's thread.
    provider: Arc<dyn Provider>,
    config: EngineConfig,
}

/// Runs sub-agents for the `delegate` tool. One per session.
pub struct SessionSubAgents {
    /// The session's own model, and the answer for every seat `seats` does not
    /// carry — which is every seat at all until a second BYOK provider is
    /// configured.
    ///
    /// Behind a lock because it is the one piece of this dispatcher a running
    /// session re-points: `/model` swaps the lead's adapter between turns, and
    /// a child delegated afterwards must inherit the model the user picked
    /// rather than the one the session booted on (#4625). Everything else here
    /// — the pool's accumulated spend above all — survives a switch untouched,
    /// which is why the switch re-points this field instead of re-installing
    /// the dispatcher.
    model: std::sync::RwLock<SessionModel>,
    /// The models this session serves named seats from.
    ///
    /// Empty is the ordinary case and means "every child runs on `model`" —
    /// exactly what this dispatcher did before seats existed. A hit means the
    /// user assigned a model to the role name the child's requester declared,
    /// which is what lets one plugin's process run several participants on
    /// several models. The names are the plugin's and the assignment is the
    /// user's; this dispatcher only looks them up. See [`crate::agent::seats`].
    seats: crate::agent::seats::SeatProviders,
    /// Weak on purpose — the registry owns this dispatcher. See module docs.
    tools: Weak<ToolRegistry>,
    /// The operator's `tools.<name>` switches, applied to every child this
    /// dispatcher runs (#3930).
    ///
    /// Held rather than re-derived per dispatch because `dispatch` has no
    /// `&Config` — the seam it hands to `run_child` takes the policy as data,
    /// and this is where the session's copy lives. [`Self::new`] leaves it
    /// permissive; [`install_for_session`] is the production installer and is
    /// where the real one is read.
    policy: stella_tools::policy::ToolPolicy,
    /// The session authorization gate, held for the same reason as `policy`
    /// and carried onto every child's thread (#3482).
    ///
    /// A best-of-N candidate's turn runs as
    /// [`Principal::Plugin`](stella_core::ports::Principal::Plugin), so this is
    /// the dispatcher a plugin's grant has to reach — a gate that was built at
    /// session assembly and stopped at the parent stack would be a rule that
    /// never fires where the plugin actually acts. [`Self::new`] leaves it
    /// [`NoAuthz`](stella_core::ports::NoAuthz); [`Self::install`] takes the
    /// session's.
    gate: Arc<dyn stella_core::ports::AuthzGate>,
    /// `Arc` so a clone rides onto the child's thread: the child settles its
    /// own spend the moment it stops, rather than after a `.await` the parent
    /// may never reach. See "Settling is the child's job".
    pool: Arc<Mutex<BudgetGuard>>,
}

impl SessionSubAgents {
    /// Build a dispatcher over `provider`, running children against
    /// `registry`'s tool set with the default pool limit.
    #[must_use]
    pub fn new(
        provider: Arc<dyn Provider>,
        registry: &Arc<ToolRegistry>,
        config: EngineConfig,
        mode: stella_protocol::BudgetMode,
    ) -> Self {
        Self {
            model: std::sync::RwLock::new(SessionModel { provider, config }),
            seats: crate::agent::seats::SeatProviders::new(),
            tools: Arc::downgrade(registry),
            policy: stella_tools::policy::ToolPolicy::allow_all(),
            gate: Arc::new(stella_core::ports::NoAuthz),
            pool: Arc::new(Mutex::new(BudgetGuard::new(
                mode,
                None,
                Some(DEFAULT_POOL_LIMIT_USD),
            ))),
        }
    }

    /// Apply the operator's `tools.<name>` switches to this session's children.
    ///
    /// A separate builder rather than a [`Self::new`] parameter for
    /// [`Self::with_seats`]'s reason: deriving the policy needs a `&Config`,
    /// which the test doubles do not have and do not need. The permissive
    /// default is what `new` already meant — it is the shipped posture
    /// ([`stella_tools::policy::ToolPolicy::allow_all`]), not an exemption.
    #[must_use]
    pub fn with_tool_policy(mut self, policy: stella_tools::policy::ToolPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Run this session's children behind `gate`.
    ///
    /// A separate builder for [`Self::with_tool_policy`]'s reason: building
    /// the session gate reads the plugin roster off disk
    /// ([`crate::agent::tool_stack::session_gate`]), which the test doubles
    /// have no reason to do. `NoAuthz` is what [`Self::new`] already meant.
    #[must_use]
    pub fn with_authz_gate(mut self, gate: Arc<dyn stella_core::ports::AuthzGate>) -> Self {
        self.gate = gate;
        self
    }

    /// Serve the named roles from models of their own.
    ///
    /// Additive to [`Self::new`]'s empty map, and deliberately a separate
    /// builder rather than a `new` parameter: resolving seats needs the
    /// credential discovery pass ([`crate::config::discover_configured_providers`]),
    /// which every existing caller of `new` — the test doubles included — has
    /// no reason to run. A dispatcher with no seats is the single-provider
    /// session, which is the common case and must stay the cheap one.
    #[must_use]
    pub fn with_seats(mut self, seats: crate::agent::seats::SeatProviders) -> Self {
        self.seats = seats;
        self
    }

    /// The wiring a child runs on: the provider serving `seat` (or the
    /// session's own), and the session's engine config.
    ///
    /// A seat miss is not an error and must never become one. It covers all three
    /// ordinary cases and they resolve identically on purpose: the child named
    /// no seat, the seat is one the user assigned no model to, or the assigned
    /// model could not be built and was reported at install. In every one of
    /// them the honest answer is "the model this session is already running",
    /// never a substitute core picked.
    ///
    /// `seat` is compared and never parsed. See [`crate::agent::seats`] for why
    /// core must stay ignorant of what the string means.
    ///
    /// The adapter and the engine config come out of one read, so a child
    /// dispatched while [`Self::retarget`] runs gets the wiring from before the
    /// switch or the wiring from after it, never one field of each.
    fn wiring_for(&self, seat: Option<&str>) -> (Arc<dyn Provider>, EngineConfig) {
        let model = self.model.read().unwrap_or_else(|p| p.into_inner());
        let provider = seat
            .and_then(|seat| self.seats.get(seat))
            .cloned()
            .unwrap_or_else(|| model.provider.clone());
        (provider, model.config.clone())
    }

    /// The provider half of [`Self::wiring_for`], for the seat-routing tests —
    /// which assert on which adapter a seat name resolves to and have no use
    /// for the engine config that rides along. `dispatch` takes the pair.
    #[cfg(test)]
    fn provider_for(&self, seat: Option<&str>) -> Arc<dyn Provider> {
        self.wiring_for(seat).0
    }

    /// Re-point the session's own model at `provider`/`config`.
    ///
    /// Called between turns when `/model` (or an assumed agent's declared
    /// `model:`) switches the running session, so children delegated afterwards
    /// run what the user picked (#4625). Seat assignments are deliberately
    /// untouched: a seat is an explicit per-role choice, and a session-default
    /// switch is not a licence to overrule it. So are `pool`, `policy` and
    /// `gate` — re-installing the dispatcher would have reset the pool's
    /// accumulated spend, quietly handing the session a fresh ceiling on every
    /// switch.
    pub fn retarget(&self, provider: Arc<dyn Provider>, config: EngineConfig) {
        let mut model = self.model.write().unwrap_or_else(|p| p.into_inner());
        *model = SessionModel { provider, config };
    }

    /// Override the session pool ceiling.
    ///
    /// `None` means **unlimited**, and replaces the ceiling
    /// [`Self::new`] installed — it does not mean "leave the default alone".
    /// That reading is what made [`DEFAULT_POOL_LIMIT_USD`] dead code:
    /// `install_for_session` passed `None` meaning "nothing to override" and
    /// got an unbounded pool (#1849). Kept as-is rather than reinterpreted,
    /// because a caller that genuinely wants no pool ceiling needs a way to
    /// say so; the fix belongs at the call site, which now names its choice.
    #[must_use]
    pub fn with_pool_limit(self, limit_usd: Option<f64>) -> Self {
        let mode = self.pool.lock().unwrap_or_else(|p| p.into_inner()).mode();
        Self {
            pool: Arc::new(Mutex::new(BudgetGuard::new(mode, None, limit_usd))),
            ..self
        }
    }

    /// Build the dispatcher and hand it to the registry, so the `delegate` tool
    /// stops reporting sub-agents as unavailable. Also returned, so a caller
    /// that dispatches children outside the native registry can share the
    /// same runner.
    ///
    /// One call per session, next to where the registry is built. Kept a
    /// free function rather than folded into `ToolRegistry::new` because the
    /// registry lives in `stella-tools`, which has no provider factory and
    /// must not grow one.
    ///
    /// Returns the **concrete** dispatcher, not the trait object it is also
    /// attached as. Both are the same allocation, and the trait object is what
    /// `delegate` needs — but [`Self::dispatch_in_workspace`] is not on
    /// [`SubAgentDispatcher`] and cannot be, since a dispatcher's contract is
    /// "run this child against the session's tool set" and that method's whole
    /// point is that the tool set is somewhere else. A caller wanting the
    /// trait object coerces; a caller wanting the rooted seam does not have to
    /// build a second dispatcher to get it, which is what would put a second
    /// pool and a second ledger over one session's money.
    pub fn install(
        provider: Arc<dyn Provider>,
        registry: &Arc<ToolRegistry>,
        config: EngineConfig,
        mode: stella_protocol::BudgetMode,
        pool_limit_usd: Option<f64>,
        seats: crate::agent::seats::SeatProviders,
        posture: ChildToolPosture,
    ) -> Arc<Self> {
        let dispatcher = Arc::new(
            Self::new(provider, registry, config, mode)
                .with_pool_limit(pool_limit_usd)
                .with_seats(seats)
                .with_tool_policy(posture.policy)
                .with_authz_gate(posture.gate),
        );
        registry.attach_sub_agent_dispatcher(dispatcher.clone() as Arc<dyn SubAgentDispatcher>);
        dispatcher
    }
}

/// Install the session's sub-agent dispatcher from a `Config`, so the
/// `delegate` tool can actually run children.
///
/// One line at each session entry point, deliberately: the call sites live
/// in files already well over the size ratchet, and the interesting decisions
/// (which provider, which pool ceiling) belong here next to the type that
/// consumes them rather than repeated five times.
///
/// The dispatcher gets a provider of its own rather than sharing the turn's:
/// the turn holds its own by reference for the whole turn, and a child needs
/// one that outlives any single turn. Metering is `Observed` on the pool —
/// the *parent's* guard is what enforces `--spend-limit`, via the spend ledger the
/// engine drains at each step boundary.
///
/// It also resolves this session's **seats** ([`crate::agent::seats`]) — the
/// models the user assigned to the role names installed plugins declared. A
/// session with no seat assignments (every session, until someone makes one)
/// pays nothing for this: no credential discovery, no extra adapter, and the
/// dispatcher behaves exactly as it did before seats existed.
///
/// Seat notices are surfaced here rather than swallowed, because a seat that
/// could not be built is a settings line that reads like a capability and is
/// not one — the session still runs, on the session's model, and the operator
/// is told which seat degraded and why.
///
/// **Unless the engine config came from the trusted launcher seam**, in which
/// case the same degradation refuses the session instead: this is the host
/// that `Config::engine_settings_trusted` says consults it, and
/// [`crate::agent::seats::EnginePosture`] carries the argument (#1147, #3937).
pub fn install_for_session(
    cfg: &crate::config::Config,
    registry: &Arc<ToolRegistry>,
) -> Result<Arc<SessionSubAgents>, String> {
    let assignments = cfg
        .engine_settings
        .as_ref()
        .and_then(|engine| engine.seat_models.clone())
        .unwrap_or_default();

    // The common path, and the one that must stay free: no assignments means
    // no discovery pass and no adapters. Resolving an empty map would be
    // harmless but would still pay for `discover_configured_providers`, which
    // reads the credential chain on every session start.
    let seats = if assignments.is_empty() {
        crate::agent::seats::SeatProviders::new()
    } else {
        let configured = crate::config::discover_configured_providers();
        // The session's own merged engine config, which is where the ceiling
        // the seat map answers to lives — the same object `seat_models` came
        // from, so the two cannot be read from different scopes.
        let allowed = cfg
            .engine_settings
            .as_ref()
            .map(|engine| engine.allowed_models().to_vec())
            .unwrap_or_default();
        let (seats, notices) = crate::agent::seats::resolve_seat_models(
            &assignments,
            &configured,
            &allowed,
            crate::agent::seats::EnginePosture::of(cfg),
        )?;
        for notice in notices {
            eprintln!("  seat: {notice}");
        }
        seats
    };

    Ok(SessionSubAgents::install(
        Arc::from(crate::agent::build_provider(cfg)?),
        registry,
        crate::agent::engine_config_for(cfg),
        stella_protocol::BudgetMode::Observed,
        session_pool_limit_usd(),
        seats,
        ChildToolPosture {
            policy: crate::agent::session_tool_policy(cfg),
            gate: crate::agent::tool_stack::session_gate(&cfg.workspace_root),
        },
    ))
}

/// The sub-agent pool ceiling a session installs when nothing overrides it.
///
/// A named function rather than a literal at the call site, because a literal
/// there is exactly what went wrong: every production installer passed `None`
/// — which [`SessionSubAgents::with_pool_limit`] reads as *unlimited*, not as
/// "keep the default" — so [`DEFAULT_POOL_LIMIT_USD`] was documented as the
/// bound that stops "a model looping on `delegate`" while binding nothing. A
/// session without `--spend-limit` whose model wedged on delegation ran every child
/// to `max_steps` with no dollar bound at any layer (#1849).
///
/// # Why it warns rather than stops
///
/// The pool is installed `Observed`, so crossing $2 produces a warning and the
/// children keep running. That is this repository's standing posture —
/// degradation warns, never disables — and the enforcing bound is elsewhere
/// and unchanged: the *parent's* guard is the hard ceiling, via the spend
/// ledger the engine drains at each step boundary, so a session that passed
/// `--spend-limit` already stops. Making the pool itself enforcing would add a
/// second wall that a caller never asked for and that no flag can raise.
///
/// The alternative — enforce at the pool — is a maintainer's call, not this
/// function's: say so and this becomes a one-line change to the mode passed by
/// [`install_for_session`].
#[must_use]
pub fn session_pool_limit_usd() -> Option<f64> {
    Some(DEFAULT_POOL_LIMIT_USD)
}

#[async_trait]
impl SubAgentDispatcher for SessionSubAgents {
    /// Run a `delegate` child behind the same chain the parent turn runs
    /// behind: the operator's `tools.<name>` switches and the session
    /// authorization gate (#3930).
    ///
    /// It used to run against the **bare** registry, so a child could call a
    /// tool the operator had switched off for this workspace and its calls
    /// reached no gate and wrote no `tool.call.requested` journal entry. The
    /// parent turn that spawned it was fully gated; the child it spawned was
    /// not — and a capability withheld from a turn is not withheld if the turn
    /// can delegate it.
    ///
    /// `policy_stack_with` rather than `session_stack`, deliberately:
    /// `.stella/tools/*.toml` customs stay withheld from a dispatched child on
    /// #3339's argument — an unreviewed local script's side effects are
    /// invisible to the coordination a child's writes go through, and the
    /// human at the keyboard is not this child's principal. A child that needs
    /// a custom tool argues for promoting the tool, not for widening this
    /// chain.
    async fn dispatch(&self, spec: SubAgentSpec) -> SubAgentOutcome {
        let Some(tools) = self.tools.upgrade() else {
            return SubAgentOutcome::Refused {
                reason: "the session's tool registry is gone".to_string(),
            };
        };
        // Read before `spec` moves onto the child's thread.
        let principal = stella_core::ports::Principal::SubAgent(spec.agent_id.clone());
        self.run_child(
            spec,
            tools,
            Some((self.policy.clone(), self.gate.clone(), principal)),
        )
        .await
    }
}

impl SessionSubAgents {
    /// Run one child against a tool set rooted **somewhere other than the
    /// session's** — a best-of-N candidate's isolated worktree (#3892).
    ///
    /// # Why this is a seam here and not a root on [`SubAgentSpec`]
    ///
    /// Because there is no single root that could be correct. A fan-out runs
    /// N candidates concurrently in N disjoint trees, so a registry whose
    /// `root` were re-pointed per dispatch would be re-pointed by every
    /// sibling underneath the others — and `root` is what every path fence
    /// resolves against (`stella_core::workspace_scope`), so the failure mode
    /// is not a confused tool but a candidate writing into its sibling's tree
    /// while both report isolation. A registry per candidate has one root for
    /// its whole life, which is the only shape that is true.
    ///
    /// Everything *else* is the session's and is deliberately not rebuilt:
    /// the provider, the seat map, the sub-agent pool, the spend ledger, the
    /// turn's event stream and controls, the orphan cascade and the
    /// settle-on-the-thread discipline all come from this one dispatcher. A
    /// second dispatcher would be a second pool and a second ledger over one
    /// session's money, which is exactly what
    /// [`crate::wrapper_plugin::SessionChildTurns`]' doc refuses.
    ///
    /// Like [`SubAgentDispatcher::dispatch`], the child runs behind the
    /// operator's tool switches and the session authorization gate
    /// ([`crate::agent::tool_stack::policy_stack_with`]). The two used to
    /// differ — `delegate`'s children ran against the bare registry — and
    /// #3930 closed that: the principal is what separates them now, not the
    /// chain.
    pub(crate) async fn dispatch_in_workspace(
        &self,
        spec: SubAgentSpec,
        tools: Arc<ToolRegistry>,
        policy: stella_tools::policy::ToolPolicy,
        principal: stella_core::ports::Principal,
    ) -> SubAgentOutcome {
        self.run_child(spec, tools, Some((policy, self.gate.clone(), principal)))
            .await
    }

    /// The one child-running body both entry points share.
    ///
    /// `stack` is the policy, authorization gate and principal the child's
    /// chain is assembled from, inside the child's own thread — `None` only
    /// for a caller that has none, which since #3930 is no production door. A
    /// private helper taking the difference as data is the one shape that
    /// keeps the subtle half — `ParentGone`, `OrphanStop`, `catch_unwind`,
    /// settle-before-report — written exactly once.
    async fn run_child(
        &self,
        spec: SubAgentSpec,
        tools: Arc<ToolRegistry>,
        stack: Option<(
            stella_tools::policy::ToolPolicy,
            Arc<dyn stella_core::ports::AuthzGate>,
            stella_core::ports::Principal,
        )>,
    ) -> SubAgentOutcome {
        // The turn's own stream, read now rather than captured at install:
        // a child's `StepUsage` has to be metered against the turn that
        // asked for it. A sink keeps a between-turns dispatch from failing.
        let events = tools
            .events()
            .unwrap_or_else(|| EventSender::from_fn(|_| Ok(())));
        // Read now, for the same reason and from the same slot discipline as
        // the sender above: these are the seams of the turn that asked for
        // this child, not of the session. Empty between turns, which runs the
        // child uninterruptible rather than refusing it.
        let controls = tools.turn_controls();

        // Snapshot, release, run, fold back. `BudgetGuard` is `Copy`, so the
        // child carves against the pool's real headroom without the lock
        // being held across its turn — which is what keeps sibling `delegate`
        // calls from one step concurrent instead of serialized.
        let pool_view = *self.pool.lock().unwrap_or_else(|p| p.into_inner());
        let before = pool_view.session_spent_usd();

        // Flipped by `ParentGone`'s drop below, which runs when this future is
        // dropped — a hard cancel of the parent's turn. The child polls it as
        // a soft stop, which is the whole cascade: see "A cancelled parent".
        let parent_alive = Arc::new(AtomicBool::new(true));
        let _parent = ParentGone(parent_alive.clone());
        // Wrap rather than replace: the turn's own stop still has to cross,
        // and the gate is untouched.
        let mut controls = controls;
        let turn = controls.steering.take();
        controls.steering = Some(Arc::new(OrphanStop { turn, parent_alive }));

        // Everything crossing the thread is owned; see the module docs on
        // why the child cannot simply be awaited here.
        //
        // The seat is read from the spec rather than from the session: this is
        // the one line that turns a requester's named role into the model that
        // serves it, and it must be taken before the spec moves onto the
        // child's thread.
        let (provider, config) = self.wiring_for(spec.seat.as_deref());
        let pool = self.pool.clone();
        let ledger = tools.sub_agent_spend_ledger();
        let (done, wait) = tokio::sync::oneshot::channel();
        let thread = std::thread::Builder::new()
            .name(format!("stella-subagent-{}", spec.agent_id))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        let _ = done.send(Err(format!("could not start a runtime: {err}")));
                        return;
                    }
                };
                let mut view = pool_view;
                // Built here rather than by the caller because the chain
                // borrows its base, and the base is the registry this thread
                // owns — a `GatedToolSet<'_>` cannot cross a thread boundary,
                // only the owned pieces it is assembled from can.
                let base: &dyn stella_core::ports::ToolExecutor = &*tools;
                let stacked = stack.map(|(policy, gate, principal)| {
                    crate::agent::tool_stack::policy_stack_with(base, policy, gate, principal)
                });
                let child_tools: &dyn stella_core::ports::ToolExecutor = match &stacked {
                    Some(stack) => stack,
                    None => &*tools,
                };
                // Set on the parent engine, not the child's: `run_sub_agent`
                // propagates the gate as-is and narrows the steering to
                // `ChildSteering`, so this is what makes a child pausable and
                // stoppable without letting it steal the parent's messages.
                let engine = Engine::with_sleeper(&*provider, child_tools, config, &TokioSleeper)
                    .with_turn_controls(&controls);
                // `catch_unwind` so a panic INSIDE the turn cannot skip the
                // settle below (#1850). Without it the unwind leaves this
                // frame directly and real dollars the child had already spent
                // landed in neither ledger — the comment at the `wait.await`
                // claimed settling happened "on every path", and a panic was
                // the path it did not.
                //
                // `AssertUnwindSafe` is the honest annotation, not a
                // suppression: the values crossing the boundary are the
                // engine, the spec and the event sender, and the one piece of
                // state a torn turn could leave inconsistent — `view`, the
                // budget carve — is exactly what is read afterwards, on
                // purpose. A partially-billed carve is the number to settle.
                //
                // Only unwinds are catchable. Release builds set
                // `panic = "abort"` (workspace Cargo.toml), so there a child
                // panic takes the process down and nothing below runs — see
                // this module's header, which no longer claims otherwise.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    runtime.block_on(engine.run_sub_agent_with_sender(
                        SubAgentHost::new(&*provider),
                        &spec,
                        &mut view,
                        &events,
                    ))
                }));

                // Settled HERE, before reporting, and deliberately not after
                // the `wait.await` below: a parent cancelled mid-`delegate` never
                // resumes that await, and dollars this child has already
                // spent would land in no ledger at all. Charging late to the
                // session is the ledger's doctrine; never charging is not.
                //
                // Now genuinely on every path the thread can take, panic
                // included — which is what the `catch_unwind` above buys.
                //
                // Exactly once, because this is now the only writer on either
                // side — the `delegate` tool no longer charges what it did not
                // measure.
                let spent = view.session_spent_usd() - before;
                if spent > 0.0 {
                    // The delta, not the view: a sibling may have folded its
                    // own spend into the pool while this child ran, and
                    // overwriting with a stale snapshot would erase it.
                    pool.lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .record_spend(spent);
                    // The parent's guard is the hard ceiling; this is what the
                    // engine drains into it at the next step boundary.
                    push_sub_agent_spend(&ledger, spent);
                }
                // A caught panic reports as a refusal with a reason, rather
                // than as a dropped sender the receiver has to infer one for.
                // The spend above is already settled either way.
                let _ = match outcome {
                    Ok(outcome) => done.send(Ok(outcome)),
                    Err(_) => done.send(Err(
                        "the sub-agent panicked; its spend was still settled".into()
                    )),
                };
            });
        if let Err(err) = thread {
            return SubAgentOutcome::Refused {
                reason: format!("could not start a sub-agent thread: {err}"),
            };
        }

        // A child that panicked reports a refusal naming the panic (an
        // unwinding build) or drops its sender (anything else that killed the
        // thread); both land here rather than unwinding the parent's turn.
        // Nothing is settled on this side — the child already did it, and
        // since #1850 that really is on every path the thread can take,
        // because the turn runs under `catch_unwind` and the settle is after
        // it rather than after the unwind.
        match wait.await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(reason)) => SubAgentOutcome::Refused { reason },
            Err(_) => SubAgentOutcome::Refused {
                reason: "the sub-agent thread ended without reporting".to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests;
