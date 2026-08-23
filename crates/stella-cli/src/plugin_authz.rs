// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Turning an accepted `[[capabilities]]` list into a gate that refuses
//! everything else (#3482, `doc:pipeline-as-plugins` §A4).
//!
//! # The defect this closes
//!
//! `Capability` carries `tool`, `risk`, `purpose` and `scope`, and
//! `stella_plugin::consent_text` renders all four to a human before install.
//! **Nothing turned an accepted consent into an authorization rule.** The
//! consent text was honest about the half a user could otherwise be misled by
//! — it labels `scope` as the plugin's own claim — but "the gate enforces the
//! tool and the grade" was true only in the sense that
//! `agent::tool_stack::session_gate` returned `NoAuthz` and no plugin could be
//! refused anything.
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
//! both are in scope, so the host is where the translation belongs.
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
//! # A plugin that declared nothing installs no rule
//!
//! [`PluginGates::from_roster`] skips a manifest with an empty
//! `[[capabilities]]` list rather than building a gate that refuses it
//! everything, and that is the owner call this change had to make out loud.
//!
//! Two reasons, and the second is the one that decides it:
//!
//! 1. The consent prompt renders *"It asks for no tool capabilities."* for
//!    such a manifest — a statement that the plugin made no request, not that
//!    the user granted it nothing. Turning that sentence into a total denial
//!    would enforce a decision nobody was shown.
//! 2. `Principal::Plugin` is carried by two different things. A plugin's own
//!    contributed tool is one (`stella_tools::custom::CustomTool::principal`);
//!    a **best-of-N candidate's whole worker turn** is the other
//!    (`crate::candidate_workspaces`, #3892). `plugins/stella-candidates` is
//!    the only shipped plugin that fans out, and it declares no capabilities —
//!    so a deny-by-default rule would refuse every tool its candidates call,
//!    on its first run, on a declaration written for a different question.
//!
//! The rule is therefore *undeclared-within-a-declared-grant is denied*, not
//! *silence is denial*. Declaring a list is a plugin opting into being bounded
//! by it. The residual hole — a plugin that declares nothing is not narrowed
//! here at all — is real and is tracked separately: closing it needs a default
//! grant a user accepts, which is a decision, not an implementation.
//!
//! # It answers about one plugin
//!
//! A gate has three outcomes and none of them is "no opinion", so an
//! individual rule that is asked about somebody else answers
//! [`AuthzDecision::Allow`] — "this rule has no objection" — and never about a
//! principal it was not built for. That keeps it composable with the gates a
//! session already binds: this one can only ever *narrow*.

use std::collections::BTreeMap;

use serde_json::Value;
use stella_core::ports::{
    AuthzContribution, AuthzDecision, AuthzEvalError, AuthzEvaluation, AuthzGate, AuthzRuleTrace,
    AuthzTrace, Principal,
};
use stella_plugin::{Capability, RiskLevel};
use stella_protocol::ToolContract;

use crate::plugin_cmd::roster::PluginRoster;

/// The name every rule in this plane reports itself under.
const GATE_NAME: &str = "plugin-capability";

/// The authority one installed plugin holds, as the rule that enforces it.
///
/// Built from the capability list a human accepted at install. Nothing else:
/// a plugin's authority is what was granted then and not one capability more.
#[derive(Debug, Clone)]
pub(crate) struct PluginCapabilityGate {
    /// The manifest name this rule answers about — `Principal::Plugin`'s
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
    /// The rule for `plugin`, from the capabilities the user accepted.
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

    /// The rule's identifier in an [`AuthzTrace`] — which plugin refused, not
    /// just which plane did (#3289). A manifest name, never content.
    fn rule_name(&self) -> String {
        format!("{GATE_NAME}:{}", self.plugin)
    }

    /// The tools this rule would permit, in order — for a denial's reason and
    /// for anything that wants to show a grant back to a human.
    fn granted_tools(&self) -> Vec<&str> {
        self.granted.keys().map(String::as_str).collect()
    }

    /// Whether this rule is even about `principal`.
    fn matches(&self, principal: &Principal) -> bool {
        matches!(principal, Principal::Plugin(name) if name == &self.plugin)
    }

    /// This rule's own verdict, assuming [`Self::matches`] already said yes.
    fn verdict(&self, contract: &ToolContract) -> AuthzDecision {
        let tool = contract.name();
        let Some(accepted) = self.granted.get(tool) else {
            return AuthzDecision::Deny {
                reason: format!(
                    "plugin \"{}\" was not granted \"{tool}\" at install; it may call: {}",
                    self.plugin,
                    if self.granted.is_empty() {
                        "nothing".to_string()
                    } else {
                        self.granted_tools().join(", ")
                    }
                ),
            };
        };

        // The grade the plugin declared and the user accepted is a ceiling on
        // the grade the *registered contract* carries. A plugin that
        // under-graded itself — declaring `read_file` as `low` and then being
        // pointed at a tool the registry grades `destructive` — is refused
        // here, which is the check `Capability::risk`'s own doc comment says a
        // host performs ("a plugin that under-grades itself is making a
        // checkable claim").
        //
        // `within` rather than a hand-rolled comparison: it is the shape a
        // grant is already written in ("may call up to Medium"), so this reads
        // the same way `RiskCeiling` does.
        if !contract.risk.within(*accepted) {
            return AuthzDecision::Deny {
                reason: format!(
                    "plugin \"{}\" was granted \"{tool}\" at {} risk, and this host grades it {}",
                    self.plugin,
                    accepted.as_str(),
                    contract.risk.as_str(),
                ),
            };
        }

        AuthzDecision::Allow
    }
}

/// Every installed plugin's rule, as the one `AuthzGate` a session binds.
///
/// `AuthzGate` is a single trait object, so N installed plugins need a
/// composite. The fold is `stella_core`'s own precedence ladder — deny
/// outranks any ask, which outranks any allow — reused rather than re-written,
/// which is the explicit instruction at `stella_core::ports::authz`'s module
/// docs ("one ladder, not a second one").
#[derive(Debug, Clone)]
pub(crate) struct PluginGates {
    /// In roster order, which is manifest-name order — so the trace a denial
    /// carries is stable across runs.
    rules: Vec<PluginCapabilityGate>,
}

impl PluginGates {
    /// The gate for everything installed in this workspace, or `None` when no
    /// installed plugin declared a capability — in which case the session
    /// keeps the `NoAuthz` it would otherwise have had, chosen by name.
    pub(crate) fn from_roster(roster: &PluginRoster) -> Option<Self> {
        let rules: Vec<PluginCapabilityGate> = roster
            .plugins()
            .iter()
            .filter(|plugin| !plugin.manifest.capabilities.is_empty())
            .map(|plugin| {
                PluginCapabilityGate::accepted(
                    plugin.manifest.name.clone(),
                    &plugin.manifest.capabilities,
                )
            })
            .collect();
        (!rules.is_empty()).then_some(Self { rules })
    }

    /// The rule-by-rule account, with the decider marked.
    ///
    /// Overridden rather than left to the default single-rule trace for the
    /// reason #3289 exists: a composite reporting itself as one opaque
    /// `plugin-capability` rule would lose **which plugin refused**, which is
    /// the only part of the answer a plugin author can act on.
    fn evaluate(&self, contract: &ToolContract, principal: &Principal) -> AuthzEvaluation {
        let mut rules = Vec::with_capacity(self.rules.len());
        let mut decision = AuthzDecision::Allow;
        let mut decided_at: Option<usize> = None;

        for rule in &self.rules {
            if !rule.matches(principal) {
                rules.push(AuthzRuleTrace {
                    rule: rule.rule_name(),
                    matched: false,
                    contribution: AuthzContribution::None,
                    deciding: false,
                });
                continue;
            }
            let verdict = rule.verdict(contract);
            rules.push(AuthzRuleTrace {
                rule: rule.rule_name(),
                matched: true,
                contribution: match &verdict {
                    AuthzDecision::Allow => AuthzContribution::Allow,
                    AuthzDecision::Deny { .. } => AuthzContribution::Deny,
                    AuthzDecision::RequireApproval { .. } => AuthzContribution::RequireApproval,
                },
                deciding: false,
            });
            // Deny outranks everything and cannot be overturned by a later
            // rule, so the first one wins and the rest still evaluate — a
            // trace that stopped early could not answer "was this rule even
            // consulted", which is what `AuthzContribution::None` is for.
            if matches!(verdict, AuthzDecision::Deny { .. })
                && !matches!(decision, AuthzDecision::Deny { .. })
            {
                decision = verdict;
                decided_at = Some(rules.len() - 1);
            }
        }

        // Nothing refused: the deciding entry is the last rule that matched,
        // or none at all when this gate was asked about a principal it holds
        // no rule for.
        let deciding = decided_at.or_else(|| {
            rules
                .iter()
                .rposition(|trace| trace.contribution == AuthzContribution::Allow)
        });
        if let Some(index) = deciding {
            rules[index].deciding = true;
        }
        AuthzEvaluation {
            decision,
            trace: AuthzTrace { rules },
        }
    }
}

impl AuthzGate for PluginGates {
    fn name(&self) -> &'static str {
        GATE_NAME
    }

    fn check(
        &self,
        contract: &ToolContract,
        principal: &Principal,
        _input: &Value,
    ) -> Result<AuthzDecision, AuthzEvalError> {
        Ok(self.evaluate(contract, principal).decision)
    }

    fn check_traced(
        &self,
        contract: &ToolContract,
        principal: &Principal,
        _input: &Value,
    ) -> Result<AuthzEvaluation, AuthzEvalError> {
        Ok(self.evaluate(contract, principal))
    }
}

#[cfg(test)]
mod tests;
