//! The one place a session's tool chain is assembled (#3283).
//!
//! Every driver — the one-shot turn, the goal loop, a resume, the deck's lead
//! turn, a subsession lane, a fleet worker, a best-of-N candidate — used to
//! stack its own copy of the same decorators, which is how a new outermost
//! layer would reach some surfaces and quietly miss others. This module owns
//! the composition instead, in the order
//! [`stella_tools::gated::GatedToolSet`]'s module docs specify:
//!
//! ```text
//! GatedToolSet        <- authorization: who is asking, and may they? (#2716)
//!   PolicyToolSet     <- operator switches: is this tool on at all?
//!     CustomToolSet   <- .stella/tools/*.toml
//!       base          <- registry / MCP view / claim tap, per driver
//! ```
//!
//! The base of the chain stays the driver's own: which registry, whether an
//! MCP view sits on it, whether a claim tap coordinates writes — those differ
//! per surface for reasons this module has no opinion about. What must *not*
//! differ is everything above the base, and the [`Principal`] naming who the
//! stack acts as: the human at the keyboard for an interactive or one-shot
//! turn, the dispatched lane for a subsession or fleet worker, the pipeline
//! role for a candidate workspace.
//!
//! # The gate is `NoAuthz`, and that is written down here
//!
//! No shipped configuration attaches an authorization plane yet, so every
//! stack passes [`NoAuthz`] — **chosen by name** in [`session_gate`], never a
//! nullable slot defaulting open (#2716's constructor-dependency rule). When a
//! deployment grows a real gate (an RBAC plugin, a host-supplied policy over
//! the serve wire), `session_gate` is the one function that changes.

use std::path::PathBuf;
use std::sync::Arc;

use stella_core::ports::{AuthzGate, NoAuthz, Principal, ToolExecutor};
use stella_tools::custom::{CustomTool, CustomToolSet};
use stella_tools::gated::GatedToolSet;
use stella_tools::policy::ToolPolicy;

use super::session_tool_policy;
use crate::config::Config;
use crate::tool_policy::PolicyToolSet;

/// The authorization gate every session stack runs under.
///
/// [`NoAuthz`] by name: this deployment does not authorize tool calls, and
/// that is a decision typed here where a reviewer can see it — not the
/// absence of a value. A configured gate replaces this one function, and
/// every driver picks it up through the assembly below.
pub(crate) fn session_gate() -> Arc<dyn AuthzGate> {
    Arc::new(NoAuthz)
}

/// The full session chain over `base`: custom `.stella/tools/*.toml` tools,
/// the operator's switches, and the authorization gate, outermost-last.
pub(crate) fn session_stack<'a>(
    base: &'a dyn ToolExecutor,
    custom_tools: Vec<CustomTool>,
    cfg: &Config,
    principal: Principal,
) -> GatedToolSet<'a> {
    session_stack_with_gate(
        base,
        custom_tools,
        cfg.workspace_root.clone(),
        session_tool_policy(cfg),
        session_gate(),
        principal,
    )
}

/// [`session_stack`] with every dependency explicit — the seam the witness
/// tests inject a denying gate through, so the chain they prove is the chain
/// the drivers ship.
pub(crate) fn session_stack_with_gate<'a>(
    base: &'a dyn ToolExecutor,
    custom_tools: Vec<CustomTool>,
    workspace_root: PathBuf,
    policy: ToolPolicy,
    gate: Arc<dyn AuthzGate>,
    principal: Principal,
) -> GatedToolSet<'a> {
    let customs = CustomToolSet::new(base, custom_tools, workspace_root);
    let permitted = PolicyToolSet::new_boxed(Box::new(customs), policy);
    GatedToolSet::new_boxed(Box::new(permitted), gate, principal)
}

/// The chain without the custom layer: the operator's switches and the gate
/// over a base that already carries the complete surface — a subsession
/// lane's claim tap, a fleet worker's, or the bare registry under
/// process-free authority (which strips the custom layer deliberately).
pub(crate) fn policy_stack<'a>(
    base: &'a dyn ToolExecutor,
    cfg: &Config,
    principal: Principal,
) -> GatedToolSet<'a> {
    let permitted = PolicyToolSet::new(base, session_tool_policy(cfg));
    GatedToolSet::new_boxed(Box::new(permitted), session_gate(), principal)
}

/// [`policy_stack`] owning its base by `Arc` — for a best-of-N candidate
/// workspace, whose chain is built dynamically and outlives every borrow.
/// Without this the gate would stop at the candidate boundary, and best-of-N
/// would be a way around authorization.
pub(crate) fn policy_stack_owned(
    base: Arc<dyn ToolExecutor>,
    policy: ToolPolicy,
    principal: Principal,
) -> GatedToolSet<'static> {
    let permitted = PolicyToolSet::new_owned(base, policy);
    GatedToolSet::new_owned(Arc::new(permitted), session_gate(), principal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::{Value, json};
    use stella_core::ports::{AuthzDecision, AuthzEvalError};
    use stella_protocol::tool::{ToolOutput, ToolSchema};
    use stella_protocol::{RiskLevel, ToolContract};

    /// Stands in for a session's base (registry / MCP view) and records
    /// whether any call actually got through the assembled stack.
    struct Leaf {
        reached: std::sync::Mutex<Vec<String>>,
    }

    impl Leaf {
        fn new() -> Self {
            Self {
                reached: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn reached(&self) -> Vec<String> {
            self.reached.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ToolExecutor for Leaf {
        fn schemas(&self) -> Vec<ToolSchema> {
            vec![ToolSchema {
                name: "get_state".into(),
                description: "d".into(),
                input_schema: json!({}),
                read_only: true,
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

    struct DenyAll;

    impl AuthzGate for DenyAll {
        fn name(&self) -> &'static str {
            "deny-all"
        }
        fn check(
            &self,
            contract: &ToolContract,
            _principal: &Principal,
            _input: &Value,
        ) -> Result<AuthzDecision, AuthzEvalError> {
            Ok(AuthzDecision::Deny {
                reason: format!("`{}` is not permitted here", contract.name()),
            })
        }
    }

    fn stack<'a>(leaf: &'a Leaf, gate: Arc<dyn AuthzGate>) -> GatedToolSet<'a> {
        session_stack_with_gate(
            leaf,
            Vec::new(),
            PathBuf::from("."),
            ToolPolicy::allow_all(),
            gate,
            Principal::User,
        )
    }

    /// **The #3283 witness.** Through the *assembled session chain* — not the
    /// gate's own unit tests — a denying gate refuses a call that the shipped
    /// default ([`session_gate`], `NoAuthz` by name) allows, and the base is
    /// never reached. This is what makes the seam real: the gate holds at the
    /// position every driver actually builds.
    #[tokio::test]
    async fn a_denying_gate_blocks_a_call_the_default_session_stack_allows() {
        let leaf = Leaf::new();
        assert!(
            matches!(
                stack(&leaf, session_gate())
                    .execute("get_state", &json!({}))
                    .await,
                ToolOutput::Ok { .. }
            ),
            "anti-vacuity: the shipped default must allow this exact call"
        );
        assert_eq!(leaf.reached(), vec!["get_state".to_string()]);

        let leaf = Leaf::new();
        match stack(&leaf, Arc::new(DenyAll))
            .execute("get_state", &json!({}))
            .await
        {
            ToolOutput::Error { message, .. } => {
                assert!(message.contains("get_state"), "names the tool: {message}");
            }
            other => panic!("the gate must refuse through the full stack, got {other:?}"),
        }
        assert!(
            leaf.reached().is_empty(),
            "the base must never see a denied call: {:?}",
            leaf.reached()
        );
    }

    /// The same seam holds for the owned (candidate-workspace) assembly —
    /// best-of-N must not be a way around authorization.
    #[tokio::test]
    async fn the_owned_candidate_stack_is_gated_too() {
        let stack = policy_stack_owned(
            Arc::new(Leaf::new()),
            ToolPolicy::allow_all(),
            Principal::Role("worker".into()),
        );
        assert!(matches!(
            stack.execute("get_state", &json!({})).await,
            ToolOutput::Ok { .. }
        ));

        // A ceiling below `High` refuses a name the snapshot never saw —
        // fail-closed reaches the candidate chain unchanged.
        let ceiling = GatedToolSet::new_owned(
            Arc::new(PolicyToolSet::new_owned(
                Arc::new(Leaf::new()),
                ToolPolicy::allow_all(),
            )),
            Arc::new(stella_core::ports::RiskCeiling::new(RiskLevel::Medium)),
            Principal::Role("worker".into()),
        );
        assert!(matches!(
            ceiling.execute("mcp__vendor__deploy", &json!({})).await,
            ToolOutput::Error { .. }
        ));
    }
}
