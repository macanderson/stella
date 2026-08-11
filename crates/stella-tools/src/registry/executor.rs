// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! [`super::ToolRegistry`]'s implementation of `stella-core`'s
//! [`ToolExecutor`] port — the whole surface the engine drives a session
//! through, in one file.
//!
//! A child module of `registry` (like [`super::turn_probe`] and
//! [`super::options`]) so the registry's internals stay reachable, split out
//! to keep the registry file under its size gate: `registry.rs` is a
//! grandfathered god file closed to growth, and this seam is the coherent
//! piece to take with it. Every method here is a forwarding or aggregating
//! one, which is what makes it a seam rather than an arbitrary cut — the
//! port's shape belongs together, and none of it needs anything the parent
//! keeps private beyond `tools`/`late_tools`.

use super::*;

/// `ToolRegistry` is the production implementation of `stella-core`'s
/// `ToolExecutor` port — the engine drives every tool call through this
/// impl, never through `stella-tools` types directly.
#[async_trait]
impl ToolExecutor for ToolRegistry {
    fn schemas(&self) -> Vec<ToolSchema> {
        ToolRegistry::schemas(self)
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        ToolRegistry::execute(self, name, input).await
    }

    /// Take what the `task` tool's children cost since the last step
    /// boundary. Destructive by the port's contract: the engine charges
    /// whatever this returns, so reporting twice would bill twice.
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        stella_core::subagent::drain_sub_agent_spend(&self.sub_agent_spend)
    }

    /// Collect the parked-wait request a tool deposited this step, if any
    /// (#1471). Consults the primary map and the late-enabled overlay — the
    /// same two sources every other read path reads. First hit wins: a step
    /// dispatches at most a handful of calls, and `Tool::take_wait_request`
    /// is destructive, so at most one request exists per boundary.
    fn drain_wait_request(&self) -> Option<stella_core::WaitRequest> {
        let from_primary = self
            .tools
            .values()
            .find_map(|tool| tool.take_wait_request());
        from_primary.or_else(|| {
            self.late_tools
                .read()
                .ok()
                .and_then(|late| late.values().find_map(|tool| tool.take_wait_request()))
        })
    }

    /// Aggregate each registered tool's [`Tool::parallel_safe`] claim for the
    /// engine's dispatch grouping. Consults the primary map and the
    /// late-enabled overlay — the same two sources every other read path
    /// (`schemas`, dispatch) reads, so a late-enabled tool's claim cannot be
    /// lost.
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        let mut names: std::collections::HashSet<String> = self
            .tools
            .iter()
            .filter(|(_, tool)| tool.parallel_safe())
            .map(|(name, _)| name.clone())
            .collect();
        // Poison-tolerant like every sibling `late_tools` read path.
        let late = self.late_tools.read().unwrap_or_else(|p| p.into_inner());
        names.extend(
            late.iter()
                .filter(|(_, tool)| tool.parallel_safe())
                .map(|(name, _)| name.clone()),
        );
        names
    }

    /// Aggregate every registered tool's still-running processes for the
    /// engine's end-of-turn assertion (#2764). Consults the primary map and
    /// the late-enabled overlay, the same two sources every other read path
    /// reads, so a late-enabled tool's services cannot go unreported.
    ///
    /// Ordered oldest handle first, so a turn's assertion — and any
    /// transcript or golden stream carrying it — reads the same way twice.
    /// `HashMap` iteration order is not stable across runs, and this text
    /// lands in the model's context, where an unstable ordering is both a
    /// confusing message and a prompt-cache miss (invariant 7). Keyed on the
    /// handle's numeric suffix, the same monotonic rule `ProcessTable::reap`
    /// and its tombstone bound already sort by, with the whole handle as the
    /// tiebreaker so a tool inventing some other handle shape still sorts
    /// deterministically rather than collapsing into one bucket's arrival
    /// order.
    fn live_services(&self) -> Vec<stella_core::LiveService> {
        let mut services: Vec<stella_core::LiveService> = self
            .tools
            .values()
            .flat_map(|tool| tool.live_services())
            .collect();
        // Poison-tolerant like every sibling `late_tools` read path.
        let late = self.late_tools.read().unwrap_or_else(|p| p.into_inner());
        services.extend(late.values().flat_map(|tool| tool.live_services()));
        services.sort_by_key(|service| {
            let ordinal = service
                .handle
                .rsplit('-')
                .next()
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(u64::MAX);
            (ordinal, service.handle.clone())
        });
        services
    }
}
