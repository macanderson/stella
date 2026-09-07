// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Turning an accepted `[[capabilities]]` list into a gate that refuses
//! everything else (#3482, `doc:pipeline-as-plugins` §A4).
//!
//! `Capability` carries `tool`, `risk`, `purpose` and `scope`, and
//! `stella_plugin::consent_text` renders all four to a human before install.
//! This module is what turns that accepted consent into an authorization
//! rule. Note `scope` is the plugin's own claim, which the consent text says;
//! the tool and the grade are what this gate enforces.
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
//! The rules it applies:
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
//! # A plugin that asked the host for nothing gets nothing from the host
//!
//! This is settled, and `doc:adr/0032-silence-is-not-a-grant` is where it was
//! settled. [`PluginGates::from_roster`] builds a rule for every installed
//! plugin. A plugin's grant is everything its manifest declared and a human
//! accepted, which is the `[[capabilities]]` list plus the tools and MCP
//! servers the package ships itself. An empty `[[capabilities]]` list asks the
//! host for nothing, so it is granted nothing of the host's, and every such
//! call is refused with a reason naming the table an author would write.
//!
//! The rule before it skipped a manifest with an empty list, so a plugin that
//! declared nothing installed no rule and every call it made was allowed.
//! Declaring less bought more, which makes the list a thing an author is
//! penalised for writing.
//!
//! Silence could not mean denial while either of these stood:
//!
//! - The consent prompt rendered *"It asks for no tool capabilities."*, a
//!   sentence about the request rather than the grant. It says what the plugin
//!   may call now (`stella_plugin::consent_text`), so a human reads the
//!   sentence this gate enforces.
//! - `Principal::Plugin` named two callers. A plugin's own contributed tool is
//!   one (`stella_tools::custom::CustomTool::principal`). A whole worker turn
//!   the host ran because the plugin asked for one was the other
//!   (`crate::candidate_workspaces`, #3892), and `plugins/stella-candidates`
//!   declares no capabilities, so denial here would have refused every tool
//!   its candidates call. That turn is
//!   [`stella_core::ports::Principal::PluginWorker`],
//!   which no rule here matches: the model picks those tools, the session's
//!   own policy bounds them, and the plugin's authority for the fan-out is the
//!   `candidate_fanout` host call it declared.
//!
//! So a plugin that wants one of the host's tools asks for it in one line a
//! human reads, and gets nothing it did not ask for.
//!
//! # It answers about one plugin
//!
//! A gate has three outcomes and none of them is "no opinion", so an
//! individual rule that is asked about somebody else answers
//! [`AuthzDecision::Allow`] — "this rule has no objection" — and never about a
//! principal it was not built for. That keeps it composable with the gates a
//! session already binds: this one can only ever *narrow*.
//!
//! # The other half of the plane
//!
//! A tool call is one of the two things a plugin holds authority over. The
//! other is a **lane seam**, and [`lanes`] is where the session's gate is
//! asked about one.

use std::collections::BTreeMap;

use serde_json::Value;
use stella_core::ports::{
    AuthzContribution, AuthzDecision, AuthzEvalError, AuthzEvaluation, AuthzGate, AuthzRuleTrace,
    AuthzTrace, Principal,
};
use stella_plugin::{Capability, RiskLevel};
use stella_protocol::ToolContract;

use crate::plugin_cmd::roster::PluginRoster;

pub(crate) mod lanes;

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
    /// Namespace prefixes this plugin may call anything beneath — one per MCP
    /// server the package ships (`mcp__<server>__`).
    ///
    /// A prefix rather than a name because there is no name: a server
    /// advertises its tools when it connects, long after this rule is built.
    /// The server itself is declared and consented by name, which is what
    /// `crate::agent::tool_stack`'s own principal map keys on for the same
    /// reason.
    granted_prefixes: Vec<String>,
}

impl PluginCapabilityGate {
    /// The rule for `plugin`, from everything its manifest declared and a
    /// human accepted: the capability list, plus the tools and MCP servers the
    /// package ships itself.
    ///
    /// A tool listed twice takes the **highest** grade of its entries. The
    /// manifest's own rule is one entry per tool (`plugins/stella-selfdriving`
    /// says so in as many words: "two entries for one tool make the effective
    /// grant the union of two lines a user read separately"), and this is the
    /// safe reading if one ever slips through validation: the union is what
    /// the user saw, and taking the *lower* grade would refuse a call they
    /// consented to.
    ///
    /// # A package's own contributions are already declared
    ///
    /// `[[capabilities]]` is what a plugin asks of **the host's** tool
    /// surface. A script tool the package ships is not the host's to grant: it
    /// is the package's own code, named and described in `[[tools]]`, and
    /// rendered to a human by `stella_plugin::consent_text` before anything is
    /// copied. Refusing it would break the tool a user just agreed to install
    /// (ADR 0032), so the shipped names join the grant.
    ///
    /// They join it at no ceiling, because `ToolContribution` carries no grade
    /// for the risk check to compare against — that check exists to catch an
    /// author under-grading their own claim, and here there is no claim. An
    /// author who *does* declare a capability for their own tool keeps the
    /// grade they declared: a shipped name is only added where the capability
    /// list is silent about it.
    pub(crate) fn accepted(
        plugin: impl Into<String>,
        capabilities: &[Capability],
        ships_tools: &[String],
        ships_servers: &[String],
    ) -> Self {
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
        for tool in ships_tools {
            granted
                .entry(tool.clone())
                .or_insert(RiskLevel::Destructive);
        }
        Self {
            plugin: plugin.into(),
            granted,
            granted_prefixes: ships_servers
                .iter()
                .map(|server| stella_mcp::namespace_prefix(server))
                .collect(),
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

    /// What a refused caller is told it *may* call, in one clause.
    ///
    /// The namespaces ride along with the names. A grant that listed only the
    /// names would under-report itself to the one reader who has to act on it,
    /// which is the shape this whole module is about.
    fn may_call(&self) -> String {
        let mut parts: Vec<String> = self.granted_tools().iter().map(|s| (*s).into()).collect();
        parts.extend(
            self.granted_prefixes
                .iter()
                .map(|prefix| format!("anything under `{prefix}`")),
        );
        if parts.is_empty() {
            return "nothing — its manifest declares no `[[capabilities]]`".into();
        }
        parts.join(", ")
    }

    /// Whether this rule is even about `principal`.
    fn matches(&self, principal: &Principal) -> bool {
        matches!(principal, Principal::Plugin(name) if name == &self.plugin)
    }

    /// This rule's own verdict, assuming [`Self::matches`] already said yes.
    fn verdict(&self, contract: &ToolContract) -> AuthzDecision {
        let tool = contract.name();
        // A tool under a namespace this package's own server owns: granted for
        // the reason its script tools are, and with no name to have listed.
        if self
            .granted_prefixes
            .iter()
            .any(|prefix| tool.starts_with(prefix.as_str()))
        {
            return AuthzDecision::Allow;
        }
        let Some(accepted) = self.granted.get(tool) else {
            return AuthzDecision::Deny {
                reason: format!(
                    "plugin \"{}\" was not granted \"{tool}\" at install; it may call: {}",
                    self.plugin,
                    self.may_call()
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
    /// The gate for everything installed in this workspace, or `None` when
    /// nothing is installed — in which case the session keeps the `NoAuthz` it
    /// would otherwise have had, chosen by name.
    ///
    /// Every installed plugin gets a rule, whatever its manifest asked for. A
    /// plugin that declared nothing gets a rule granting nothing, which is the
    /// decision ADR 0032 records and the module docs argue. Filtering those out
    /// is what left a plugin that asked for nothing holding everything.
    pub(crate) fn from_roster(roster: &PluginRoster) -> Option<Self> {
        let rules: Vec<PluginCapabilityGate> = roster
            .plugins()
            .iter()
            .map(|plugin| {
                let ships_tools: Vec<String> = plugin
                    .manifest
                    .tools
                    .iter()
                    .map(|tool| tool.name.clone())
                    .collect();
                let ships_servers: Vec<String> = plugin
                    .manifest
                    .mcp
                    .iter()
                    .map(|server| server.server.clone())
                    .collect();
                PluginCapabilityGate::accepted(
                    plugin.manifest.name.clone(),
                    &plugin.manifest.capabilities,
                    &ships_tools,
                    &ships_servers,
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
