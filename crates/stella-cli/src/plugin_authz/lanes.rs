// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The gate's say over what a plugin lane holds
//! (`doc:turn-lane-assembly` §9.8).
//!
//! `granted = requested ∩ authorized` has three answers. This module is the
//! third one.
//!
//! The first is the rung a person took at install
//! ([`stella_plugin::ConsentedGrade`]). The second is the operator's own
//! `lanes.custom.<id>.capabilities` ceiling. Both are limits a person
//! agreed to. Neither is a rule a deployment writes.
//!
//! The third is the session's [`AuthzGate`]. It is asked here, as
//! [`Principal::Plugin`]. That case names this exact caller.
//!
//! # Why it lives in `stella-cli`
//!
//! [`AuthzGate`] and [`Principal`] belong to `stella-core`.
//! [`LaneCapability`] belongs to `stella-protocol`. [`LaneGrant`] belongs to
//! `stella-plugin`. The host is the only place all three are in scope.
//! [`crate::plugin_authz`] makes the same case for the tool half.
//!
//! # It narrows a grant; it cannot build one
//!
//! [`narrowed_by_gate`] takes a grant the host already worked out. It can
//! only take seams away. There is no code path that puts one back. So a gate
//! that says yes to everything leaves the rung and the ceiling where they
//! were.
//!
//! # A failure refuses
//!
//! `Ok(Deny)` is a decision. `Err(AuthzEvalError)` is the lack of one. The
//! store was down, or a rule would not compile. The seam is withheld either
//! way. That is the fail-closed rule `stella_core::ports::authz` states in
//! its own docs.
//!
//! [`AuthzDecision::RequireApproval`] withholds too. Nobody is at the
//! keyboard while a manifest is read. An ask that reaches no one would be a
//! grant.

use std::collections::BTreeMap;

use stella_core::ports::{AuthzDecision, AuthzGate, LaneSeam, Principal};
use stella_plugin::LaneGrant;
use stella_protocol::LaneCapability;

/// A lane grant after the session's gate has had its say.
///
/// The grant and the gate's refusals are kept apart. So a report can tell a
/// seam the rung withheld from a seam the gate withheld. They read the same
/// in [`LaneGrant::withheld`]. Only the gate's reason is one a plugin author
/// can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GatedLaneGrant {
    /// The grant, narrowed to what the gate allowed.
    pub(crate) grant: LaneGrant,
    /// Each seam the gate took away, and the reason it gave.
    pub(crate) refused: BTreeMap<LaneCapability, String>,
}

/// Ask `gate` about every seam `grant` holds. Drop the ones it refuses.
///
/// `plugin` is the manifest name. That is the whole of
/// [`Principal::Plugin`]'s payload. The lane id rides in the [`LaneSeam`]
/// instead, because one plugin can ship several lanes. A gate that could not
/// tell them apart would have to answer for the plugin as a whole.
pub(crate) fn narrowed_by_gate(
    grant: &LaneGrant,
    gate: &dyn AuthzGate,
    plugin: &str,
) -> GatedLaneGrant {
    let principal = Principal::Plugin(plugin.to_string());
    let mut granted = grant.granted.clone();
    let mut refused = BTreeMap::new();

    for capability in &grant.granted {
        let seam = LaneSeam::new(grant.lane.clone(), *capability);
        let reason = match gate.check_lane(&seam, &principal) {
            Ok(AuthzDecision::Allow) => continue,
            Ok(AuthzDecision::Deny { reason }) => reason,
            // No one is here to answer while a manifest is read, so an ask
            // nobody can settle is a seam nobody granted.
            Ok(AuthzDecision::RequireApproval { reason }) => {
                format!("{reason} — and no one is here to answer")
            }
            Err(error) => error.to_string(),
        };
        granted.remove(capability);
        refused.insert(*capability, reason);
    }

    GatedLaneGrant {
        grant: LaneGrant {
            lane: grant.lane.clone(),
            requested: grant.requested.clone(),
            granted,
        },
        refused,
    }
}

#[cfg(test)]
mod tests;
