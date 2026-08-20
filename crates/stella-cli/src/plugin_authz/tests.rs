// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for [`super::PluginCapabilityGate`] — an accepted `[[capabilities]]`
//! list, as the rule that refuses everything else (#3482).

use serde_json::json;
use stella_core::ports::{AuthzDecision, AuthzGate, Principal};
use stella_plugin::{Capability, RiskLevel};
use stella_protocol::{ToolContract, ToolSchema};

use super::PluginCapabilityGate;

fn contract(name: &str, risk: RiskLevel) -> ToolContract {
    ToolContract::builtin(
        ToolSchema {
            name: name.into(),
            description: "d".into(),
            input_schema: json!({}),
            read_only: false,
            speculation_safe: false,
        },
        risk,
    )
}

fn capability(tool: &str, risk: RiskLevel) -> Capability {
    Capability {
        tool: tool.into(),
        risk,
        purpose: "because the plugin says so".into(),
        scope: Vec::new(),
    }
}

fn gate() -> PluginCapabilityGate {
    PluginCapabilityGate::accepted(
        "stella-selfdriving",
        &[
            capability("bash", RiskLevel::Destructive),
            capability("read_file", RiskLevel::Low),
        ],
    )
}

fn decide(gate: &PluginCapabilityGate, tool: &str, risk: RiskLevel, principal: &Principal) -> AuthzDecision {
    gate.check(&contract(tool, risk), principal, &json!({}))
        .expect("this gate reaches a decision for every input")
}

/// **Witness (#3482, first half of item 2).** A tool absent from the accepted
/// list is denied.
///
/// Fails before this change for the plainest possible reason: nothing turned an
/// accepted consent into a rule, so there was no gate to ask and every declared
/// capability was decoration on a prompt.
#[test]
fn a_tool_absent_from_the_accepted_list_is_denied() {
    let principal = Principal::Plugin("stella-selfdriving".into());
    let decision = decide(&gate(), "write_file", RiskLevel::Medium, &principal);

    let AuthzDecision::Deny { reason } = decision else {
        panic!("a capability nobody granted must be refused, got {decision:?}");
    };
    assert!(
        reason.contains("was not granted \"write_file\""),
        "the refusal names the tool: {reason}"
    );
    assert!(
        reason.contains("bash, read_file"),
        "and names what the plugin MAY call, so the author can fix the manifest \
         rather than guess: {reason}"
    );
}

/// **Witness (#3482, second half of item 2).** A tool graded above what was
/// accepted is denied, even though the tool itself was granted.
///
/// This is the check `Capability::risk`'s own doc comment says a host performs:
/// "a plugin that under-grades itself is making a checkable claim, and a host
/// comparing this against the registered contract's own grade catches it."
/// Nothing performed it until now.
#[test]
fn a_tool_graded_above_what_was_accepted_is_denied() {
    let principal = Principal::Plugin("stella-selfdriving".into());
    // `read_file` was accepted at Low. This host grades this one Destructive.
    let decision = decide(&gate(), "read_file", RiskLevel::Destructive, &principal);

    let AuthzDecision::Deny { reason } = decision else {
        panic!("a grade above the accepted ceiling must be refused, got {decision:?}");
    };
    assert!(
        reason.contains("at low risk") && reason.contains("grades it destructive"),
        "the refusal names both grades, because the gap between them is the whole \
         finding: {reason}"
    );
}

/// The other direction, so the two above are not passing because the gate
/// refuses everything: a granted tool at or below its accepted grade is
/// allowed.
#[test]
fn a_granted_tool_within_its_accepted_grade_is_allowed() {
    let principal = Principal::Plugin("stella-selfdriving".into());
    assert_eq!(
        decide(&gate(), "bash", RiskLevel::Destructive, &principal),
        AuthzDecision::Allow,
        "granted at destructive, called at destructive"
    );
    assert_eq!(
        decide(&gate(), "bash", RiskLevel::Low, &principal),
        AuthzDecision::Allow,
        "a ceiling permits everything below it, not only the grade itself"
    );
    assert_eq!(
        decide(&gate(), "read_file", RiskLevel::Low, &principal),
        AuthzDecision::Allow
    );
}

/// **Witness (#3482, item 1's consequence).** The gate answers about the plugin
/// it was built for and nobody else.
///
/// This is what makes `Principal::Plugin` worth having: the same call, the same
/// tool, refused for the plugin and permitted for the user. A host that ran an
/// installed plugin as `Principal::User` — which `stella-cli` does today
/// (`agent.rs`'s constant) — would get `Allow` for every one of these, which is
/// precisely the marketplace defect §A1 exists to prevent.
#[test]
fn the_gate_answers_about_its_own_plugin_and_no_one_else() {
    let gate = gate();
    let ungranted = contract("write_file", RiskLevel::Medium);

    assert!(
        matches!(
            gate.check(
                &ungranted,
                &Principal::Plugin("stella-selfdriving".into()),
                &json!({})
            ),
            Ok(AuthzDecision::Deny { .. })
        ),
        "refused for the plugin this gate is about"
    );

    for other in [
        Principal::User,
        Principal::Plugin("some-other-plugin".into()),
        Principal::Role("triage".into()),
        Principal::SubAgent("sub-1".into()),
        Principal::Host("embedder".into()),
    ] {
        assert_eq!(
            gate.check(&ungranted, &other, &json!({}))
                .expect("a decision"),
            AuthzDecision::Allow,
            "this rule has no objection about {other:?} — a gate cannot abstain, so it \
             must only ever narrow, never widen"
        );
    }
}

/// A plugin that was granted nothing may call nothing, and the refusal says so
/// rather than listing an empty set.
#[test]
fn a_plugin_granted_nothing_may_call_nothing() {
    let gate = PluginCapabilityGate::accepted("inert", &[]);
    let decision = decide(
        &gate,
        "bash",
        RiskLevel::Low,
        &Principal::Plugin("inert".into()),
    );
    let AuthzDecision::Deny { reason } = decision else {
        panic!("got {decision:?}");
    };
    assert!(
        reason.contains("it may call: nothing"),
        "an empty grant reads as `nothing`, not as an empty list: {reason}"
    );
}

/// Two entries for one tool take the **highest** grade of the two.
///
/// The manifest's own rule is one entry per tool, and this is the safe reading
/// if one ever slips past validation: the union is what the user saw rendered,
/// so taking the lower grade would refuse a call they consented to.
#[test]
fn a_tool_listed_twice_takes_the_highest_grade_the_user_saw() {
    let gate = PluginCapabilityGate::accepted(
        "double",
        &[
            capability("bash", RiskLevel::Low),
            capability("bash", RiskLevel::High),
        ],
    );
    assert_eq!(
        decide(
            &gate,
            "bash",
            RiskLevel::High,
            &Principal::Plugin("double".into())
        ),
        AuthzDecision::Allow,
        "the user read both lines; the effective grant is their union"
    );
    assert!(
        matches!(
            decide(
                &gate,
                "bash",
                RiskLevel::Destructive,
                &Principal::Plugin("double".into())
            ),
            AuthzDecision::Deny { .. }
        ),
        "and the union is still a ceiling, not permission for everything"
    );
}

/// The gate names itself, which every `AuthzGate` owes an audit line.
#[test]
fn the_gate_names_itself() {
    assert_eq!(gate().name(), "plugin-capability");
}
