// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for [`super::narrowed_by_gate`]. It is the gate's say over what a
//! plugin lane holds.
//!
//! Each test here fails on the old tree. Nothing there asked a gate about a
//! lane seam. `AuthzGate::check_lane` did not exist to ask.

use std::collections::BTreeSet;
use std::sync::Mutex;

use serde_json::Value;
use stella_core::ports::{
    AuthzDecision, AuthzEvalError, AuthzGate, LaneSeam, NoAuthz, Principal, RiskCeiling,
};
use stella_plugin::{Capability, LaneGrant};
use stella_protocol::{LaneCapability, LaneId, RiskLevel, ToolContract};

use super::narrowed_by_gate;
use crate::plugin_authz::{PluginCapabilityGate, PluginGates};

const PLUGIN: &str = "acme";
const LANE: &str = "acme.replay";

/// The grant the rung and the ceiling already settled. The lane asked for
/// three seams. It holds two.
fn resolved_grant() -> LaneGrant {
    LaneGrant {
        lane: LaneId::new(LANE),
        requested: BTreeSet::from([
            LaneCapability::Bus,
            LaneCapability::Steering,
            LaneCapability::Gate,
        ]),
        granted: BTreeSet::from([LaneCapability::Bus, LaneCapability::Steering]),
    }
}

/// A gate that answers the way a test tells it to. It also writes down what
/// it was asked. So a witness can check the question, not just the answer.
struct FixtureGate {
    answer: fn(&LaneSeam) -> Result<AuthzDecision, AuthzEvalError>,
    asked: Mutex<Vec<(String, Principal)>>,
}

impl FixtureGate {
    fn new(answer: fn(&LaneSeam) -> Result<AuthzDecision, AuthzEvalError>) -> Self {
        Self {
            answer,
            asked: Mutex::new(Vec::new()),
        }
    }

    /// Every question this gate was put, in order.
    fn asked(&self) -> Vec<(String, Principal)> {
        self.asked.lock().expect("no test poisons the lock").clone()
    }
}

impl AuthzGate for FixtureGate {
    fn name(&self) -> &'static str {
        "fixture"
    }

    fn check(
        &self,
        _contract: &ToolContract,
        _principal: &Principal,
        _input: &Value,
    ) -> Result<AuthzDecision, AuthzEvalError> {
        Ok(AuthzDecision::Allow)
    }

    fn check_lane(
        &self,
        seam: &LaneSeam,
        principal: &Principal,
    ) -> Result<AuthzDecision, AuthzEvalError> {
        self.asked
            .lock()
            .expect("no test poisons the lock")
            .push((seam.label(), principal.clone()));
        (self.answer)(seam)
    }
}

/// **The witness for the gate's say.** A gate refuses `bus` for this plugin.
/// The seam drops out of the grant. The ask does not change.
#[test]
fn a_gate_that_refuses_a_seam_takes_it_out_of_the_grant() {
    let gate = FixtureGate::new(|seam| {
        if seam.capability == LaneCapability::Bus {
            Ok(AuthzDecision::Deny {
                reason: "the watching seam is off in this deployment".into(),
            })
        } else {
            Ok(AuthzDecision::Allow)
        }
    });

    let gated = narrowed_by_gate(&resolved_grant(), &gate, PLUGIN);

    assert_eq!(
        gated.grant.granted,
        BTreeSet::from([LaneCapability::Steering]),
        "the refused seam is gone and nothing else moved"
    );
    assert_eq!(
        gated.grant.requested,
        resolved_grant().requested,
        "what the manifest asked for is kept as written"
    );
    assert_eq!(
        gated.refused.keys().copied().collect::<Vec<_>>(),
        vec![LaneCapability::Bus],
        "the gate's refusal is reported apart from the rung's"
    );
    assert!(
        gated.refused[&LaneCapability::Bus].contains("off in this deployment"),
        "the gate's own reason reaches the report"
    );
}

/// **The witness for who is asked about.** The gate is asked as
/// [`Principal::Plugin`]. The name it carries is the manifest name. The seam
/// names the lane's own id.
#[test]
fn the_gate_is_asked_about_the_plugin_and_the_lane() {
    let gate = FixtureGate::new(|_| Ok(AuthzDecision::Allow));
    narrowed_by_gate(&resolved_grant(), &gate, PLUGIN);

    let asked = gate.asked();
    assert_eq!(asked.len(), 2, "one question per seam the lane holds");
    for (_, principal) in &asked {
        assert_eq!(
            principal,
            &Principal::Plugin(PLUGIN.to_string()),
            "a lane seam is asked for as the plugin, never as the host"
        );
    }
    assert_eq!(
        asked
            .iter()
            .map(|(seam, _)| seam.as_str())
            .collect::<Vec<_>>(),
        vec!["acme.replay:bus", "acme.replay:steering"],
        "each question names the lane that asked, and only the seams that \
         cleared the rung are asked about"
    );
}

/// **The witness for the fail-closed rule.** A gate that cannot decide holds
/// the seam back. It never grants one.
#[test]
fn a_gate_that_cannot_evaluate_withholds_the_seam() {
    let gate = FixtureGate::new(|_| {
        Err(AuthzEvalError::new(
            "fixture",
            "the policy store is unreachable",
        ))
    });

    let gated = narrowed_by_gate(&resolved_grant(), &gate, PLUGIN);

    assert!(
        gated.grant.granted.is_empty(),
        "no decision is not a yes: {:?}",
        gated.grant.granted
    );
    assert_eq!(
        gated.grant.withheld(),
        resolved_grant().requested,
        "everything asked for is withheld, the rung's own seam included"
    );
    assert!(
        gated.refused[&LaneCapability::Bus].contains("could not evaluate"),
        "the reason says it could not tell, not that it refused"
    );
}

/// An ask nobody can answer is held back too. No one is at the keyboard
/// while a manifest is read.
#[test]
fn an_approval_nobody_can_answer_withholds_the_seam() {
    let gate = FixtureGate::new(|_| {
        Ok(AuthzDecision::RequireApproval {
            reason: "an operator has to allow this lane".into(),
        })
    });

    let gated = narrowed_by_gate(&resolved_grant(), &gate, PLUGIN);

    assert!(gated.grant.granted.is_empty());
    assert!(
        gated.refused[&LaneCapability::Steering].contains("no one is here to answer"),
        "the report says why the ask could not be put to anyone"
    );
}

/// **The witness for "the gate can only narrow".** A gate says yes to
/// everything. It still cannot hand back a seam the rung held back.
#[test]
fn a_permissive_gate_cannot_widen_the_grant() {
    let ceiling = RiskCeiling::new(RiskLevel::Destructive);
    let gates: [&dyn AuthzGate; 2] = [&NoAuthz, &ceiling];

    for gate in gates {
        let gated = narrowed_by_gate(&resolved_grant(), gate, PLUGIN);

        assert_eq!(
            gated.grant,
            resolved_grant(),
            "a gate with no opinion about lane seams leaves the grant alone"
        );
        assert!(gated.refused.is_empty());
        assert!(
            gated.grant.withheld().contains(&LaneCapability::Gate),
            "the seam the rung withheld stays withheld"
        );
    }
}

/// The gate this host really binds keeps the default too. So a workspace
/// with plugins reports what it always did.
#[test]
fn a_tool_rule_has_no_opinion_about_a_lane_seam() {
    let installed = PluginGates {
        rules: vec![PluginCapabilityGate::accepted(
            PLUGIN,
            &[Capability {
                tool: "bash".into(),
                risk: RiskLevel::High,
                purpose: "runs the lane".into(),
                scope: Vec::new(),
            }],
        )],
    };

    let gated = narrowed_by_gate(&resolved_grant(), &installed, PLUGIN);

    assert_eq!(
        gated.grant,
        resolved_grant(),
        "a rule built from a tool list does not answer for a seam"
    );
    assert!(gated.refused.is_empty());
}
