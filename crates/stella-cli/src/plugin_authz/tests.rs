// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for [`super::PluginGates`] — an accepted `[[capabilities]]` list, as
//! the rule that refuses everything else (#3482).

use serde_json::json;
use stella_core::ports::{AuthzContribution, AuthzDecision, AuthzGate, Principal};
use stella_plugin::{Capability, RiskLevel};
use stella_protocol::{ToolContract, ToolSchema};

use super::{PluginCapabilityGate, PluginGates};

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

/// One installed plugin, `stella-selfdriving`, granted `bash` at destructive
/// and `read_file` at low.
fn gate() -> PluginGates {
    PluginGates {
        rules: vec![PluginCapabilityGate::accepted(
            "stella-selfdriving",
            &[
                capability("bash", RiskLevel::Destructive),
                capability("read_file", RiskLevel::Low),
            ],
        )],
    }
}

fn decide(gate: &PluginGates, tool: &str, risk: RiskLevel, principal: &Principal) -> AuthzDecision {
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
/// it holds a rule for and nobody else.
///
/// This is what makes `Principal::Plugin` worth having: the same call, the same
/// tool, refused for the plugin and permitted for the user. A host that ran an
/// installed plugin as `Principal::User` would get `Allow` for every one of
/// these, which is precisely the marketplace defect §A1 exists to prevent.
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

/// Two entries for one tool take the **highest** grade of the two.
///
/// The manifest's own rule is one entry per tool, and this is the safe reading
/// if one ever slips past validation: the union is what the user saw rendered,
/// so taking the lower grade would refuse a call they consented to.
#[test]
fn a_tool_listed_twice_takes_the_highest_grade_the_user_saw() {
    let gate = PluginGates {
        rules: vec![PluginCapabilityGate::accepted(
            "double",
            &[
                capability("bash", RiskLevel::Low),
                capability("bash", RiskLevel::High),
            ],
        )],
    };
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

/// Two installed plugins compose into one gate that answers about each
/// separately, and the trace names **which** plugin refused (#3289).
///
/// A composite falling back to the default single-rule trace would report
/// `plugin-capability` as one opaque rule, which is the half of the answer a
/// plugin author cannot act on.
#[test]
fn two_plugins_compose_and_the_trace_names_the_one_that_refused() {
    let gate = PluginGates {
        rules: vec![
            PluginCapabilityGate::accepted("alpha", &[capability("bash", RiskLevel::High)]),
            PluginCapabilityGate::accepted("beta", &[capability("read_file", RiskLevel::Low)]),
        ],
    };

    assert_eq!(
        decide(
            &gate,
            "bash",
            RiskLevel::High,
            &Principal::Plugin("alpha".into())
        ),
        AuthzDecision::Allow,
        "alpha's own grant still holds with beta installed beside it"
    );

    let evaluation = gate
        .check_traced(
            &contract("bash", RiskLevel::High),
            &Principal::Plugin("beta".into()),
            &json!({}),
        )
        .expect("a decision");
    assert!(
        matches!(&evaluation.decision, AuthzDecision::Deny { reason } if reason.contains("beta")),
        "beta was granted read_file only: {:?}",
        evaluation.decision
    );

    let alpha = &evaluation.trace.rules[0];
    assert_eq!(alpha.rule, "plugin-capability:alpha");
    assert!(
        !alpha.matched && alpha.contribution == AuthzContribution::None && !alpha.deciding,
        "alpha's rule was consulted and is not about this caller: {alpha:?}"
    );
    let beta = &evaluation.trace.rules[1];
    assert_eq!(beta.rule, "plugin-capability:beta");
    assert!(
        beta.matched && beta.contribution == AuthzContribution::Deny && beta.deciding,
        "beta's rule decided this call, and the trace has to say so: {beta:?}"
    );
}

/// A plugin that declared nothing installs no rule, so the gate is not built
/// from it at all — the owner call in this change, argued in the module docs.
///
/// `plugins/stella-candidates` is the shipped instance: it declares no
/// `[[capabilities]]` and is the only plugin that runs a best-of-N candidate's
/// whole worker turn as `Principal::Plugin`.
#[test]
fn a_roster_of_plugins_that_declared_nothing_builds_no_gate() {
    let empty = PluginGates { rules: Vec::new() };
    assert!(
        PluginGates::from_roster(&crate::plugin_cmd::roster::PluginRoster::default()).is_none(),
        "an empty roster installs no rule"
    );
    // And the shape it would have had refuses nothing, which is what keeps the
    // `None` above from being the only thing standing between a candidate
    // fan-out and a total denial.
    assert_eq!(
        decide(
            &empty,
            "bash",
            RiskLevel::Destructive,
            &Principal::Plugin("stella-candidates".into())
        ),
        AuthzDecision::Allow
    );
}

/// The gate names itself, which every `AuthzGate` owes an audit line.
#[test]
fn the_gate_names_itself() {
    assert_eq!(gate().name(), "plugin-capability");
}

/// **The end-to-end witness (#3482).** The gate the *shipped*
/// [`crate::agent::tool_stack::session_gate`] hands a session, over a real
/// workspace with a real installed plugin, refuses that plugin a tool its
/// manifest did not ask for.
///
/// This is the assertion that fails on `main`: `session_gate` took no
/// arguments, read nothing, and returned `NoAuthz` — which allows everything
/// for every principal — so an installed plugin held its operator's authority
/// no matter what its `[[capabilities]]` said.
#[test]
fn the_shipped_session_gate_enforces_an_installed_plugins_declared_grant() {
    let _env = crate::test_env::lock();
    let _restore = crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT"]);
    let root = std::env::temp_dir().join(format!(
        "stella-plugin-authz-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temp root");
    let _paths = crate::paths::test_user_home(root.join("home"));
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };

    let dir = stella_home::resolve_project_plugins_dir(&root).join("p");
    std::fs::create_dir_all(&dir).expect("plugin dir");
    std::fs::write(
        dir.join("plugin.toml"),
        "name = \"p\"\n\
         description = \"a fixture\"\n\n\
         [loop]\nparticipation = \"none\"\n\n\
         [[capabilities]]\ntool = \"read_file\"\nrisk = \"low\"\n\
         purpose = \"reads the file it is pointed at\"\n",
    )
    .expect("plugin manifest");

    let gate = crate::agent::tool_stack::session_gate(&root);
    let plugin = Principal::Plugin("p".into());
    let refused = gate
        .check(
            &contract("bash", RiskLevel::Destructive),
            &plugin,
            &json!({}),
        )
        .expect("a decision");
    assert!(
        matches!(&refused, AuthzDecision::Deny { reason } if reason.contains("\"p\"")
            && reason.contains("bash")),
        "the installed plugin asked for `read_file` and nothing else: {refused:?}"
    );

    // Anti-vacuity twice over: what it *did* ask for still runs, and the same
    // refused call made by the operator is not the plugin's and is allowed.
    assert_eq!(
        gate.check(&contract("read_file", RiskLevel::Low), &plugin, &json!({}))
            .expect("a decision"),
        AuthzDecision::Allow
    );
    assert_eq!(
        gate.check(
            &contract("bash", RiskLevel::Destructive),
            &Principal::User,
            &json!({})
        )
        .expect("a decision"),
        AuthzDecision::Allow
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// **The assembly witness.** Through the *shipped session chain* — not the
/// gate's own unit tests — a plugin-attributed call to a tool the plugin was
/// not granted is refused and the leaf is never reached, while the identical
/// call attributed to the user runs.
///
/// This is the assertion that fails on `main`: `session_gate()` returned
/// `NoAuthz`, so a plugin-attributed `bash` call was allowed and reached the
/// base.
#[tokio::test]
async fn a_plugin_is_refused_through_the_assembled_stack_and_the_user_is_not() {
    use async_trait::async_trait;
    use serde_json::Value;
    use std::sync::{Arc, Mutex};
    use stella_core::ports::ToolExecutor;
    use stella_protocol::tool::ToolOutput;

    struct Leaf {
        reached: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ToolExecutor for Leaf {
        fn schemas(&self) -> Vec<ToolSchema> {
            vec![ToolSchema {
                name: "bash".into(),
                description: "d".into(),
                input_schema: json!({}),
                read_only: false,
                speculation_safe: false,
            }]
        }
        async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
            self.reached.lock().unwrap().push(name.to_string());
            ToolOutput::Ok {
                content: format!("ran {name}"),
                data: None,
            }
        }
    }

    let gate: Arc<dyn AuthzGate> = Arc::new(PluginGates {
        rules: vec![PluginCapabilityGate::accepted(
            "p",
            &[capability("read_file", RiskLevel::Low)],
        )],
    });

    let leaf = Leaf {
        reached: Mutex::new(Vec::new()),
    };
    let refused = crate::agent::tool_stack::policy_stack_with(
        &leaf,
        stella_tools::policy::ToolPolicy::allow_all(),
        gate.clone(),
        Principal::Plugin("p".into()),
    )
    .execute("bash", &json!({}))
    .await;
    match refused {
        ToolOutput::Error { message, .. } => {
            assert!(
                message.contains("p") && message.contains("bash"),
                "{message}"
            );
        }
        other => panic!("a plugin calling an ungranted tool must be refused, got {other:?}"),
    }
    assert!(
        leaf.reached.lock().unwrap().is_empty(),
        "the base must never see a denied call"
    );

    // Anti-vacuity, same gate, same tool, same stack: the user is not the
    // plugin, and the call runs.
    let leaf = Leaf {
        reached: Mutex::new(Vec::new()),
    };
    let allowed = crate::agent::tool_stack::policy_stack_with(
        &leaf,
        stella_tools::policy::ToolPolicy::allow_all(),
        gate,
        Principal::User,
    )
    .execute("bash", &json!({}))
    .await;
    assert!(
        matches!(allowed, ToolOutput::Ok { .. }),
        "the same call as the user must still run: {allowed:?}"
    );
    assert_eq!(leaf.reached.lock().unwrap().len(), 1);
}
