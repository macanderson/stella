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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use stella_core::bus::HookBus;
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
    bus: Option<HookBus>,
) -> GatedToolSet<'a> {
    with_journal(
        session_stack_with_gate(
            base,
            custom_tools,
            cfg.workspace_root.clone(),
            session_tool_policy(cfg),
            session_gate(),
            principal,
        ),
        bus,
    )
}

/// Attach the session bus the gate journals its evaluations onto (#3289),
/// when the driver carries one — `registry.hook_bus()` at every shipped call
/// site, so the authorization plane's `(principal, tool, decision, trace)`
/// lands in the same journal the rest of the policy plane already rides.
fn with_journal(stack: GatedToolSet<'_>, bus: Option<HookBus>) -> GatedToolSet<'_> {
    match bus {
        Some(bus) => stack.with_bus(bus),
        None => stack,
    }
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
    // Derived from the same list the custom layer is about to own, before it
    // is moved: what the stack can dispatch and who each call authorizes as
    // come from one source, so a plugin's tool cannot reach the surface
    // without its principal reaching the gate (#3380).
    let contributed = contributed_principals(&custom_tools, &principal);
    let customs = CustomToolSet::new(base, custom_tools, workspace_root);
    let permitted = PolicyToolSet::new_boxed(Box::new(customs), policy);
    GatedToolSet::new_boxed(Box::new(permitted), gate, principal).with_tool_principals(contributed)
}

/// The principal each plugin-contributed tool authorizes as, keyed by tool
/// name — empty for the overwhelmingly common session with no plugins
/// installed.
///
/// Only the entries that actually *change* the answer are emitted: a tool
/// the user wrote themselves is absent from the map and falls through to the
/// stack's own principal, so the map's size is the number of third-party
/// tools rather than the number of tools.
fn contributed_principals(
    custom_tools: &[CustomTool],
    caller: &Principal,
) -> HashMap<String, Principal> {
    custom_tools
        .iter()
        .filter(|tool| tool.contributed_by.is_some())
        .map(|tool| (tool.name.clone(), tool.principal(caller)))
        .collect()
}

/// The chain without the custom layer: the operator's switches and the gate
/// over a base that already carries the complete surface — a subsession
/// lane's claim tap, a fleet worker's, or the bare registry under
/// process-free authority (which strips the custom layer deliberately).
///
/// # Customs are withheld from dispatched workers on purpose (#3339)
///
/// A `.stella/tools/*.toml` tool is an unreviewed local script
/// (`ToolContract::declared`, graded `High`), and the surfaces that carry it
/// — the deck, a one-shot turn, the goal loop — all have the human at the
/// keyboard as their principal. A subsession lane or fleet worker runs
/// *autonomously*, and its writes are coordinated through the claim tap it
/// uses as its base; a custom script's side effects are invisible to that
/// coordination (no `FileChange` events, no claim acquisition), so handing
/// an unreviewed script to an unattended worker would soften #2716's trust
/// posture twice over. A worker that needs a custom tool argues for
/// promoting the tool through the foundry adoption gate, not for widening
/// this chain.
pub(crate) fn policy_stack<'a>(
    base: &'a dyn ToolExecutor,
    cfg: &Config,
    principal: Principal,
    bus: Option<HookBus>,
) -> GatedToolSet<'a> {
    let permitted = PolicyToolSet::new(base, session_tool_policy(cfg));
    with_journal(
        GatedToolSet::new_boxed(Box::new(permitted), session_gate(), principal),
        bus,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::{Value, json};
    use stella_core::ports::{AuthzDecision, AuthzEvalError};
    use stella_protocol::ToolContract;
    use stella_protocol::tool::{ToolOutput, ToolSchema};

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

    /// A scripted MCP transport: one tool, one canned `tools/call` answer,
    /// and a record of whether the wire call was ever made. Written here
    /// rather than borrowed from `stella-mcp`'s own testkit, which is
    /// `#[cfg(test)]`-private to that crate; `Transport` is the public seam.
    struct CannedTransport {
        called: Arc<std::sync::Mutex<bool>>,
    }

    #[async_trait]
    impl stella_mcp::Transport for CannedTransport {
        async fn request(
            &self,
            method: &str,
            _params: Value,
        ) -> Result<Value, stella_mcp::McpError> {
            match method {
                "initialize" => Ok(json!({ "protocolVersion": "2025-06-18" })),
                "tools/list" => Ok(json!({
                    "tools": [{ "name": "deploy", "inputSchema": { "type": "object" } }]
                })),
                "tools/call" => {
                    *self.called.lock().unwrap() = true;
                    Ok(json!({ "content": [{ "type": "text", "text": "deployed" }] }))
                }
                other => Err(stella_mcp::McpError::Transport(format!("no {other}"))),
            }
        }
        async fn notify(&self, _m: &str, _p: Value) -> Result<(), stella_mcp::McpError> {
            Ok(())
        }
        async fn close(&self) -> Result<(), stella_mcp::McpError> {
            Ok(())
        }
    }

    /// One `.stella/tools/*.toml` script tool named `my_tool`, whose script
    /// touches `ran.marker` — so "did it run" is a fact about the process,
    /// not about the text that came back.
    fn script_tool(root: &std::path::Path) -> stella_tools::custom::CustomTool {
        use std::os::unix::fs::PermissionsExt;
        let path = root.join("s.sh");
        std::fs::write(&path, "#!/bin/sh\ntouch ./ran.marker\necho ran\n").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        stella_tools::custom::CustomTool {
            name: "my_tool".into(),
            description: "d".into(),
            command: vec!["./s.sh".into()],
            timeout_ms: 5_000,
            input_schema: json!({ "type": "object" }),
            env: Default::default(),
            source: path,
            foundry: None,
            claimed_read_only: false,
            claimed_risk: None,
            claimed_idempotent: false,
            output_schema: None,
            contributed_by: None,
        }
    }

    /// **The #2793 witness, through the *shipped* composition.**
    ///
    /// The session chain is `GatedToolSet → PolicyToolSet → CustomToolSet →
    /// McpToolSet → ToolRegistry`, and the two middle layers each dispatch
    /// names of their own that never reach the registry — so the registry's
    /// `tool.call.requested` chain never saw them. An extension policy could
    /// deny a built-in and be silently ignored for an MCP tool or a
    /// `.stella/tools/*.toml` script with the same effect.
    ///
    /// Asserted through `session_stack_with_gate` — the assembly every driver
    /// calls — rather than a hypothetical stack, for the same reason as the
    /// forwarding witnesses in `subagent/tests.rs`: a future decorator
    /// inserted into the real chain that forgets to forward `dispatch_gate`
    /// fails here, and nowhere else.
    #[tokio::test]
    async fn the_production_tool_stack_gates_mcp_and_custom_dispatches() {
        use stella_core::bus::{HookBus, HookDecision, names as hook_names};

        let dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(stella_tools::registry::ToolRegistry::new(
            dir.path().to_path_buf(),
        ));
        let bus = HookBus::new("gate-2793");
        bus.on_blocking(hook_names::TOOL_CALL_REQUESTED, |event| {
            match event.payload["tool"].as_str() {
                Some("mcp__vendor__deploy") | Some("my_tool") => {
                    HookDecision::Deny("denied by the extension policy".into())
                }
                _ => HookDecision::Allow,
            }
        })
        .detach();
        registry.attach_bus(bus);

        let called = Arc::new(std::sync::Mutex::new(false));
        let mut client = stella_mcp::McpClient::new(
            "vendor",
            Box::new(CannedTransport {
                called: called.clone(),
            }),
        );
        client.initialize().await.unwrap();
        let mcp = stella_mcp::McpToolSet::from_clients(vec![client])
            .wrapping(registry.clone() as Arc<dyn ToolExecutor>);

        let stack = session_stack_with_gate(
            &mcp,
            vec![script_tool(dir.path())],
            dir.path().to_path_buf(),
            ToolPolicy::allow_all(),
            session_gate(),
            Principal::User,
        );

        for tool in ["mcp__vendor__deploy", "my_tool"] {
            match stack.execute(tool, &json!({})).await {
                ToolOutput::Error { message, .. } => assert!(
                    message.contains("denied by the extension policy"),
                    "`{tool}`: {message}"
                ),
                ToolOutput::Ok { content, .. } => {
                    panic!("`{tool}` was denied by policy and ran anyway: {content}")
                }
            }
        }
        assert!(
            !*called.lock().unwrap(),
            "the MCP refusal must stop the wire call"
        );
        assert!(
            !dir.path().join("ran.marker").exists(),
            "the custom-tool refusal must stop the script"
        );

        // Anti-vacuity: this exact stack runs a name the policy allows, so
        // the two refusals above are the policy's doing and not the stack
        // being broken.
        assert!(
            matches!(
                stack.execute("task_list", &json!({})).await,
                ToolOutput::Ok { .. }
            ),
            "an allowed call must still run through the same stack"
        );
    }
}
