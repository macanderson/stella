// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Turning an accepted `[[capabilities]]` list into a gate that refuses
//! everything else (#3482, `doc:pipeline-as-plugins` §A4).
//!
//! # The defect this closes
//!
//! `Capability` carried `tool`, `risk`, `purpose` and `scope`, and
//! `stella_plugin::consent_text` rendered all four to a human before install.
//! **Nothing turned an accepted consent into an authorization rule.** The
//! consent text was honest about the half a user could otherwise be misled by
//! — it labels `scope` as the plugin's own claim — but "the gate enforces the
//! tool and the grade" was true only in the sense that no plugin could call
//! anything at all.
//!
//! `doc:pipeline-as-plugins` §A1 states the stake without hedging: *"a
//! marketplace shipped on top of a system that cannot distinguish an installed
//! plugin from its operator grants every plugin the operator's authority."*
//!
//! # Why it lives in `stella-cli`
//!
//! `AuthzGate` and `Principal` are `stella-core`'s; `Capability` is
//! `stella-plugin`'s; and those two crates must never depend on each other
//! (#3245 open question 3, both crate READMEs). The host is the only place
//! both are in scope, so the host is where the translation belongs — which is
//! exactly what #3482 says.
//!
//! Both grade themselves in `stella_protocol::RiskLevel`, so the comparison
//! below needs no mapping table and cannot drift into one.
//!
//! # What it decides, and what it deliberately does not
//!
//! Two rules, and they are the two #3482's definition of done names:
//!
//! - a tool **absent** from the accepted list is denied;
//! - a tool graded **above** what was accepted is denied.
//!
//! It does **not** match [`Capability::scope`]. That is #3482's third item,
//! and the decision is explicit: `scope` stays a declared claim and the
//! consent wording stays as it is. Making it gate-matched would mean parsing
//! free text a plugin author wrote — argv prefixes, paths, hosts — into a
//! predicate over an arbitrary tool's JSON input, which is a matching problem
//! with no closed grammar and no way to be wrong safely. A `scope` that
//! *nearly* matches would refuse honest calls; one that over-matches would
//! grant what nobody read. The wording and the enforcement therefore agree
//! today, which is the property #3482 asks for — "decide explicitly; do not
//! let the wording and the enforcement drift apart."
//!
//! # It answers about one plugin
//!
//! A gate has three outcomes and none of them is "no opinion", so a gate that
//! is asked about somebody else answers [`AuthzDecision::Allow`] — "this rule
//! has no objection" — and never about a principal it was not built for. That
//! keeps it composable with the gates a session already binds: this one can
//! only ever *narrow*.

use std::collections::BTreeMap;

use serde_json::Value;
use stella_core::ports::{AuthzDecision, AuthzEvalError, AuthzGate, Principal};
use stella_plugin::{Capability, RiskLevel};
use stella_protocol::ToolContract;

/// The authority one installed plugin holds, as the gate that enforces it.
///
/// Built from the capability list a human accepted at install. Nothing else:
/// a plugin's authority is what was granted then and not one capability more.
#[derive(Debug, Clone)]
pub(crate) struct PluginCapabilityGate {
    /// The manifest name this gate answers about — `Principal::Plugin`'s
    /// payload, compared exactly.
    plugin: String,
    /// Tool name to the highest risk grade accepted for it.
    ///
    /// A `BTreeMap` so a denial's reason lists the granted tools in a
    /// deterministic order — invariant 7's discipline, applied to a string a
    /// human reads when a call is refused.
    granted: BTreeMap<String, RiskLevel>,
}

impl PluginCapabilityGate {
    /// The gate for `plugin`, from the capabilities the user accepted.
    ///
    /// A tool listed twice takes the **highest** grade of its entries. The
    /// manifest's own rule is one entry per tool (`plugins/stella-selfdriving`
    /// says so in as many words: "two entries for one tool make the effective
    /// grant the union of two lines a user read separately"), and this is the
    /// safe reading if one ever slips through validation: the union is what
    /// the user saw, and taking the *lower* grade would refuse a call they
    /// consented to.
    pub(crate) fn accepted(plugin: impl Into<String>, capabilities: &[Capability]) -> Self {
        let mut granted: BTreeMap<String, RiskLevel> = BTreeMap::new();
        for capability in capabilities {
            granted
                .entry(capability.tool.clone())
                .and_modify(|highest| {
                    if capability.risk > *highest {
                        *highest = capability.risk;
                    }
                })
                .or_insert(capability.risk);
        }
        Self {
            plugin: plugin.into(),
            granted,
        }
    }

    /// The tools this gate would permit, in order — for a denial's reason and
    /// for anything that wants to show a grant back to a human.
    fn granted_tools(&self) -> Vec<&str> {
        self.granted.keys().map(String::as_str).collect()
    }
}

impl AuthzGate for PluginCapabilityGate {
    fn name(&self) -> &'static str {
        "plugin-capability"
    }

    fn check(
        &self,
        contract: &ToolContract,
        principal: &Principal,
        _input: &Value,
    ) -> Result<AuthzDecision, AuthzEvalError> {
        // Asked about anyone but the plugin this gate was built for, it has no
        // objection. See the module doc: a gate cannot abstain, and this one
        // must only ever narrow.
        let Principal::Plugin(name) = principal else {
            return Ok(AuthzDecision::Allow);
        };
        if name != &self.plugin {
            return Ok(AuthzDecision::Allow);
        }

        let tool = contract.name();
        let Some(accepted) = self.granted.get(tool) else {
            return Ok(AuthzDecision::Deny {
                reason: format!(
                    "plugin \"{}\" was not granted \"{tool}\" at install; it may call: {}",
                    self.plugin,
                    if self.granted.is_empty() {
                        "nothing".to_string()
                    } else {
                        self.granted_tools().join(", ")
                    }
                ),
            });
        };

        // The grade the plugin declared and the user accepted is a ceiling on
        // the grade the *registered contract* carries. A plugin that
        // under-graded itself — declaring `read_file` as `low` and then being
        // pointed at a tool the registry grades `destructive` — is refused
        // here, which is the check `Capability::risk`'s own doc comment says a
        // host performs ("a plugin that under-grades itself is making a
        // checkable claim").
        // `within` rather than a hand-rolled comparison: it is the shape a
        // grant is already written in ("may call up to Medium"), so this reads
        // the same way `RiskCeiling` does.
        if !contract.risk.within(*accepted) {
            return Ok(AuthzDecision::Deny {
                reason: format!(
                    "plugin \"{}\" was granted \"{tool}\" at {} risk, and this host grades it {}",
                    self.plugin,
                    accepted.as_str(),
                    contract.risk.as_str(),
                ),
            });
        }

        Ok(AuthzDecision::Allow)
    }
}

#[cfg(test)]
mod tests;
