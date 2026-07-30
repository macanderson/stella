// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The session's sub-agent dispatcher — the host half of the `task` tool
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
//! cost in total. That pool is **not** what enforces `--budget`: the tool pushes
//! every child's cost onto the registry's spend ledger, and the engine folds
//! it into the *parent's* guard at the next step-boundary check
//! (`ToolExecutor::drain_sub_agent_spend_usd`). The parent's guard stays the
//! hard ceiling; the pool is a second, tighter bound on this one category of
//! spend.
//!
//! The pool lock is deliberately not held across the child's run, so sibling
//! `task` calls from one step still execute concurrently. Two siblings can
//! therefore each carve against the same headroom before either settles —
//! an overshoot bounded by one child's cap, and caught by the parent's guard
//! at the next step boundary regardless.
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
//! buys real isolation — a child that panics resolves to a refusal here
//! instead of unwinding the parent's turn. The cost is one thread per live
//! `task` call, bounded by the engine's own tool-call concurrency cap.
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
//!   `task` tool call open, which is exactly right — pause means pause — and
//!   that both release together on resume.
//! - **The soft stop, but not the steering messages.** The child engine is
//!   built through `run_sub_agent`, which wraps whatever steering the parent
//!   has in `stella_core::subagent::ChildSteering`: the stop flag is latched
//!   and non-destructive so forwarding it is free, while `drain_steering` is
//!   destructive by contract and a child that inherited it would eat a
//!   message the user addressed to the parent.
//!
//! A driver that publishes nothing is not a failure state — a child then runs
//! bounded by its step cap and its carve, exactly as before.
//!
//! One thing this does *not* buy: a hard cancel still cannot reach a child.
//! The parent's turn future dropping takes the oneshot receiver with it while
//! the child's thread runs on to its own cap. The soft stop is the
//! interruption that arrives; that is a reason to prefer it, and it is why
//! the deck's Esc now ends delegated work instead of orphaning it.

use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use stella_core::subagent::{SubAgentDispatcher, SubAgentHost, SubAgentOutcome, SubAgentSpec};
use stella_core::{BudgetGuard, Engine, EngineConfig, EventSender};
use stella_protocol::Provider;
use stella_tools::ToolRegistry;

use crate::runtime::TokioSleeper;

/// Default ceiling on what sub-agents may cost across one session, in USD.
///
/// Generous enough that ordinary delegation never trips it, small enough
/// that a model looping on `task` cannot quietly spend a session's budget on
/// research. Callers with a real `--budget` get a tighter bound anyway: the
/// parent's guard is the hard ceiling.
pub const DEFAULT_POOL_LIMIT_USD: f64 = 2.0;

/// Runs sub-agents for the `task` tool. One per session.
pub struct SessionSubAgents {
    /// `Arc`, not `Box`: the provider is moved onto each child's thread.
    provider: Arc<dyn Provider>,
    /// Weak on purpose — the registry owns this dispatcher. See module docs.
    tools: Weak<ToolRegistry>,
    config: EngineConfig,
    pool: Mutex<BudgetGuard>,
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
            provider,
            tools: Arc::downgrade(registry),
            config,
            pool: Mutex::new(BudgetGuard::new(mode, None, Some(DEFAULT_POOL_LIMIT_USD))),
        }
    }

    /// Override the session pool ceiling.
    #[must_use]
    pub fn with_pool_limit(self, limit_usd: Option<f64>) -> Self {
        let mode = self.pool.lock().unwrap_or_else(|p| p.into_inner()).mode();
        Self {
            pool: Mutex::new(BudgetGuard::new(mode, None, limit_usd)),
            ..self
        }
    }

    /// Build the dispatcher and hand it to the registry, so the `task` tool
    /// stops reporting sub-agents as unavailable.
    ///
    /// One call per session, next to where the registry is built. Kept a
    /// free function rather than folded into `ToolRegistry::new` because the
    /// registry lives in `stella-tools`, which has no provider factory and
    /// must not grow one.
    pub fn install(
        provider: Arc<dyn Provider>,
        registry: &Arc<ToolRegistry>,
        config: EngineConfig,
        mode: stella_protocol::BudgetMode,
        pool_limit_usd: Option<f64>,
    ) {
        let dispatcher =
            Arc::new(Self::new(provider, registry, config, mode).with_pool_limit(pool_limit_usd));
        registry.attach_sub_agent_dispatcher(dispatcher);
    }
}

/// Install the session's sub-agent dispatcher from a `Config`, so the
/// `task` tool can actually run children.
///
/// One line at each session entry point, deliberately: the call sites live
/// in files already well over the size ratchet, and the interesting decisions
/// (which provider, which pool ceiling) belong here next to the type that
/// consumes them rather than repeated five times.
///
/// The dispatcher gets a provider of its own rather than sharing the turn's:
/// the turn holds its own by reference for the whole turn, and a child needs
/// one that outlives any single turn. Metering is `Observed` on the pool —
/// the *parent's* guard is what enforces `--budget`, via the spend ledger the
/// engine drains at each step boundary.
pub fn install_for_session(
    cfg: &crate::config::Config,
    registry: &Arc<ToolRegistry>,
) -> Result<(), String> {
    SessionSubAgents::install(
        Arc::from(crate::agent::build_provider(cfg)?),
        registry,
        crate::agent::engine_config_for(cfg),
        stella_protocol::BudgetMode::Observed,
        None,
    );
    Ok(())
}

#[async_trait]
impl SubAgentDispatcher for SessionSubAgents {
    async fn dispatch(&self, spec: SubAgentSpec) -> SubAgentOutcome {
        let Some(tools) = self.tools.upgrade() else {
            return SubAgentOutcome::Refused {
                reason: "the session's tool registry is gone".to_string(),
            };
        };
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
        // being held across its turn — which is what keeps sibling `task`
        // calls from one step concurrent instead of serialized.
        let pool_view = *self.pool.lock().unwrap_or_else(|p| p.into_inner());
        let before = pool_view.session_spent_usd();

        // Everything crossing the thread is owned; see the module docs on
        // why the child cannot simply be awaited here.
        let provider = self.provider.clone();
        let config = self.config.clone();
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
                // Set on the parent engine, not the child's: `run_sub_agent`
                // propagates the gate as-is and narrows the steering to
                // `ChildSteering`, so this is what makes a child pausable and
                // stoppable without letting it steal the parent's messages.
                let engine = Engine::with_sleeper(&*provider, &*tools, config, &TokioSleeper)
                    .with_turn_controls(&controls);
                let outcome = runtime.block_on(engine.run_sub_agent_with_sender(
                    SubAgentHost::new(&*provider),
                    &spec,
                    &mut view,
                    &events,
                ));
                let _ = done.send(Ok((outcome, view.session_spent_usd())));
            });
        if let Err(err) = thread {
            return SubAgentOutcome::Refused {
                reason: format!("could not start a sub-agent thread: {err}"),
            };
        }

        // A child that panicked drops its sender, which lands here as a
        // refusal rather than unwinding the parent's turn.
        let (outcome, spent_total) = match wait.await {
            Ok(Ok(pair)) => pair,
            Ok(Err(reason)) => return SubAgentOutcome::Refused { reason },
            Err(_) => {
                return SubAgentOutcome::Refused {
                    reason: "the sub-agent thread ended without reporting".to_string(),
                };
            }
        };

        // The delta, not the view: another sibling may have folded its own
        // spend into the pool while this child ran, and overwriting with a
        // stale snapshot would erase it.
        let spent = spent_total - before;
        if spent > 0.0 {
            let mut pool = self.pool.lock().unwrap_or_else(|p| p.into_inner());
            pool.record_spend(spent);
        }
        outcome
    }
}

#[cfg(test)]
#[path = "subagent/tests.rs"]
mod tests;
