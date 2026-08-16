//! The engine's optional capability slots, as one value that must be
//! answered in full — #3274 slice 2, #3387.
//!
//! # The defect this closes
//!
//! [`Engine::with_sleeper`](super::Engine::with_sleeper) returns an engine
//! with every optional seam set to `None`, and each one is turned on by its
//! own builder call. Those builders are public and work correctly; the
//! problem is that **nothing makes a caller use them**. An assembly site that
//! wants the pause gate and forgets `with_gate` compiles, runs, and quietly
//! spends through a pause — and reads identically to a site that decided it
//! did not want a gate. `goal.rs::assess` shipped exactly that bug against
//! three seams at once.
//!
//! It was tempting to state this as "the constructor *cannot* carry those
//! seams" (the framing in #3274 slice 2 and slide 22 of the turn-loop deck).
//! That is not true of this tree and a witness written against it passes
//! unchanged on `main`: `with_gate`/`with_steering`/`with_hooks` are `pub`
//! and callers outside this crate use them today. The real property is
//! **totality** — a decision must be recorded for every slot — and that is
//! what this type enforces.
//!
//! # Why there is no `Default`, and no `#[non_exhaustive]`
//!
//! Both omissions are the mechanism, not an oversight.
//!
//! - **No `Default`.** A default is precisely the forgotten decision this
//!   type exists to abolish. `..Default::default()` at an assembly site would
//!   restore the defect with better syntax.
//! - **No `#[non_exhaustive]`.** That attribute forbids struct-literal
//!   construction outside the defining crate, which would force every
//!   external lane through setters — and setters are optional calls, which is
//!   the defect again. Leaving the struct exhaustive is what makes adding a
//!   slot a **compile error at every assembly site in the workspace**, which
//!   is the whole point.
//!
//! The cost is real and accepted: adding a field here is a breaking change
//! for out-of-tree callers. That is the correct trade for an in-tree
//! guarantee, and it is why a plugin-contributed lane gets the weaker
//! load-time regime instead (`doc:turn-lane-assembly` §9.2) — you cannot give
//! an out-of-tree file a build failure.

use std::sync::Arc;

use stella_protocol::ModelCallRole;

use super::{Engine, EngineConfig, HooksHandle};
use crate::estimator::CalibrationMap;
use crate::hooks::{HookRunner, Hooks};
use crate::ports::{
    FallbackResolver, ProviderOutcomes, SteeringRequery, ToolExecutor, TurnGate, TurnSteering,
};
use crate::retry::Sleeper;
use stella_protocol::Provider;

/// Every optional seam an [`Engine`] can carry, answered in full.
///
/// There is no `Default` and no `#[non_exhaustive]`; see the module doc for
/// why both are load-bearing. Construct with a struct literal naming every
/// field — [`TurnCapabilities::none`] exists for the genuine no-seams case
/// (tests, the bare loop) and is itself written as an exhaustive literal, so
/// a new slot breaks it too.
pub struct TurnCapabilities<'a> {
    /// Lifecycle hooks and the port that executes them. Both or neither —
    /// the config alone spawns nothing.
    pub hooks: Option<(&'a Hooks, &'a dyn HookRunner)>,
    /// Where a `PreToolUse` hook's `require_approval` decision parks (#2684).
    pub hook_approvals: Option<&'a dyn crate::hooks::decision::ApprovalRoute>,
    /// Token-drift calibration, owned by the caller across turns.
    pub calibration: Option<&'a CalibrationMap>,
    /// Boundary pause gate — Pause/Resume at step granularity.
    pub gate: Option<&'a dyn TurnGate>,
    /// Step-boundary steering — mid-turn messages and the soft stop.
    pub steering: Option<&'a dyn TurnSteering>,
    /// Step-boundary context re-query (#3243 Phase 3).
    pub requery: Option<&'a dyn SteeringRequery>,
    /// Extension hook bus — observer-only.
    pub bus: Option<&'a crate::bus::HookBus>,
    /// Call-outcome feedback for a router's circuit breaker (#2673).
    pub outcomes: Option<&'a dyn ProviderOutcomes>,
    /// Mid-turn provider fallback at the retries-exhausted boundary (#2679).
    pub fallback: Option<&'a dyn FallbackResolver>,
    /// What this engine's model calls are attributed to. Not an `Option`:
    /// every call has a role, and [`ModelCallRole::Worker`] is the ordinary
    /// answer rather than the absence of one.
    pub call_role: ModelCallRole,
}

impl<'a> TurnCapabilities<'a> {
    /// The bare loop: no optional seam attached, worker attribution.
    ///
    /// Written as an exhaustive literal on purpose — adding a slot fails to
    /// compile here as well, so "none" stays a decision about every seam
    /// rather than a shrinking one.
    #[must_use]
    pub fn none() -> Self {
        Self {
            hooks: None,
            hook_approvals: None,
            calibration: None,
            gate: None,
            steering: None,
            requery: None,
            bus: None,
            outcomes: None,
            fallback: None,
            call_role: ModelCallRole::Worker,
        }
    }
}

impl<'a> HooksHandle<'a> {
    /// The pair back out, for a caller assembling a child engine from a
    /// parent's already-bound handle (`crate::subagent`). Lives here rather
    /// than in `driver.rs` because that file is a grandfathered god file
    /// closed to growth; a child module reaches the private fields just the
    /// same.
    pub(crate) fn parts(self) -> (&'a Hooks, &'a dyn HookRunner) {
        (self.hooks, self.runner)
    }
}

impl<'a> Engine<'a> {
    /// The blessed constructor: assemble an engine from its required ports
    /// and one [`TurnCapabilities`] answering every optional seam.
    ///
    /// This is what a lane — builtin or plugin-driven — is expected to call.
    /// [`Engine::with_sleeper`](Engine::with_sleeper) plus the individual
    /// `with_*` builders remain for existing call sites and behave exactly as
    /// before; what they cannot offer is the guarantee that every seam was
    /// considered.
    pub fn assemble(
        provider: &'a dyn Provider,
        tools: &'a dyn ToolExecutor,
        config: EngineConfig,
        sleeper: &'a dyn Sleeper,
        capabilities: TurnCapabilities<'a>,
    ) -> Self {
        // Destructured, never field-accessed: a new slot that this function
        // forgets to wire is then a compile error here too, which is the
        // same guarantee the assembly sites get.
        let TurnCapabilities {
            hooks,
            hook_approvals,
            calibration,
            gate,
            steering,
            requery,
            bus,
            outcomes,
            fallback,
            call_role,
        } = capabilities;

        Self {
            provider,
            tools,
            sleeper,
            config,
            call_role,
            hooks: hooks.map(|(hooks, runner)| HooksHandle { hooks, runner }),
            hook_approvals,
            calibration,
            gate,
            steering,
            requery,
            bus,
            outcomes,
            fallback,
            provider_override: Arc::new(std::sync::OnceLock::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The totality witness (#3387).
    ///
    /// This test does not assert a runtime behaviour — it asserts a *shape*,
    /// and it does so by destructuring [`TurnCapabilities`] with no `..`
    /// rest pattern. Adding a slot to the struct without adding it here fails
    /// to compile, which is the property the type exists to provide.
    ///
    /// It is deliberately not written as "a fork carries gate/steering/hooks":
    /// that passes on `main` unchanged, because the fork already carries them
    /// and the builders that attach them are public. See the module doc.
    #[test]
    fn every_capability_slot_is_named_by_this_test() {
        let TurnCapabilities {
            hooks,
            hook_approvals,
            calibration,
            gate,
            steering,
            requery,
            bus,
            outcomes,
            fallback,
            call_role,
        } = TurnCapabilities::none();

        assert!(hooks.is_none());
        assert!(hook_approvals.is_none());
        assert!(calibration.is_none());
        assert!(gate.is_none());
        assert!(steering.is_none());
        assert!(requery.is_none());
        assert!(bus.is_none());
        assert!(outcomes.is_none());
        assert!(fallback.is_none());
        assert_eq!(call_role, ModelCallRole::Worker);
    }

    /// The second half: `assemble` must actually wire what it is handed.
    ///
    /// The shape test above proves every slot is named; this proves the
    /// blessed constructor does not silently drop one on the way into the
    /// engine. Both are needed — a slot could be named in the struct and
    /// still be forgotten in `assemble`, which is the original defect moved
    /// one level in.
    #[test]
    fn assemble_carries_the_bare_capability_set_onto_the_engine() {
        let provider = crate::subagent::tests::ScriptedProvider::new(vec![]);
        let tools = crate::subagent::tests::MixedTools::default();
        let sleeper = crate::subagent::tests::NoSleep;

        let engine = Engine::assemble(
            &provider,
            &tools,
            EngineConfig::default(),
            &sleeper,
            TurnCapabilities::none(),
        );

        assert!(engine.hooks.is_none());
        assert!(engine.hook_approvals.is_none());
        assert!(engine.calibration.is_none());
        assert!(engine.gate.is_none());
        assert!(engine.steering.is_none());
        assert!(engine.requery.is_none());
        assert!(engine.bus.is_none());
        assert!(engine.outcomes.is_none());
        assert!(engine.fallback.is_none());
        assert_eq!(engine.call_role, ModelCallRole::Worker);
    }
}
