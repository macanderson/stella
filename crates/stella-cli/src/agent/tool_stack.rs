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
//!   LeanToolSet       <- the tool allowance: how many schemas fit? (#6057)
//!     PolicyToolSet   <- operator switches: is this tool on at all?
//!       CustomToolSet <- .stella/tools/*.toml
//!         base        <- registry / MCP view / claim tap, per driver
//! ```
//!
//! The allowance layer is composed only when a session asks for one — the
//! `Lean` arm of [`ToolAdvertisement`], off by default — so with the lever
//! off the chain is the four layers it always was, and the advertised array
//! is byte-identical rather than merely equivalent.
//!
//! The base of the chain stays the driver's own: which registry, whether an
//! MCP view sits on it, whether a claim tap coordinates writes — those differ
//! per surface for reasons this module has no opinion about. What must *not*
//! differ is everything above the base, and the [`Principal`] naming who the
//! stack acts as: the human at the keyboard for an interactive or one-shot
//! turn, the dispatched lane for a subsession or fleet worker, the installed
//! plugin for a best-of-N candidate workspace (#3892 — it read "the pipeline
//! role" while the staged pipeline minted candidates; that crate is gone
//! (#3865) and a candidate is asked for by a plugin now).
//!
//! # The gate is an installed plugin's grant, or `NoAuthz` by name
//!
//! [`session_gate`] is the one function that decides. A workspace where an
//! installed plugin declared `[[capabilities]]` gets
//! [`crate::plugin_authz::PluginGates`], the rule that refuses that plugin
//! anything it was not granted at install (#3482); every other workspace gets
//! [`NoAuthz`] — **chosen by name**, never a nullable slot defaulting open
//! (#2716's constructor-dependency rule). A deployment that grows a further
//! plane (an RBAC plugin, a host-supplied policy over the serve wire) composes
//! it here and nowhere else.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use stella_core::bus::HookBus;
use stella_core::ports::{AuthzGate, NoAuthz, Principal, ToolExecutor};
use stella_core::steering::ledger::SteeringLedger;
use stella_core::steering::tools::ToolAdvertisement;
use stella_tools::custom::{CustomTool, CustomToolSet};
use stella_tools::gated::GatedToolSet;
use stella_tools::policy::ToolPolicy;

use super::session_tool_policy;
use crate::config::Config;
use crate::tool_lean::LeanToolSet;
use crate::tool_policy::PolicyToolSet;

/// The authorization gate every session stack runs under.
///
/// Reads the workspace's plugin roster **once, here**, and turns each
/// installed manifest's accepted `[[capabilities]]` list into a rule
/// ([`crate::plugin_authz::PluginGates`]). That is the only I/O in the plane:
/// `AuthzGate::check` is consulted per call and must stay pure over data it
/// prefetched (invariant 2), so the read happens at construction and the gate
/// carries the answer.
///
/// With nothing installed — or nothing installed that declared a capability —
/// it is [`NoAuthz`] by name: this deployment does not authorize tool calls,
/// and that is a decision typed here where a reviewer can see it, not the
/// absence of a value.
pub(crate) fn session_gate(workspace_root: &std::path::Path) -> Arc<dyn AuthzGate> {
    match crate::plugin_authz::PluginGates::from_roster(
        &crate::plugin_cmd::package::session_roster(workspace_root),
    ) {
        Some(gates) => Arc::new(gates),
        None => Arc::new(NoAuthz),
    }
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
            cfg.tool_advertisement,
            &cfg.steering_ledger,
            session_gate(&cfg.workspace_root),
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
    advertisement: ToolAdvertisement,
    ledger: &SteeringLedger,
    gate: Arc<dyn AuthzGate>,
    principal: Principal,
) -> GatedToolSet<'a> {
    // Derived from the same list the custom layer is about to own, before it
    // is moved: what the stack can dispatch and who each call authorizes as
    // come from one source, so a plugin's tool cannot reach the surface
    // without its principal reaching the gate (#3380).
    let contributed = contributed_principals(&custom_tools, &principal);
    // The same rule for a contributed MCP server, one step removed: its tool
    // names are the server's to choose at connect time, so the namespace is
    // what can be declared here rather than the names (#4733).
    let namespaces = contributed_server_principals(&workspace_root);
    let customs = CustomToolSet::new(base, custom_tools, workspace_root);
    let permitted = PolicyToolSet::new_boxed(Box::new(customs), policy);
    GatedToolSet::new_boxed(
        budgeted(Box::new(permitted), advertisement, ledger),
        gate,
        principal,
    )
    .with_tool_principals(contributed)
    .with_prefix_principals(namespaces)
}

/// The principal each plugin-contributed **MCP server's** tools authorize as,
/// as `(namespace prefix, principal)` pairs — empty for a session with no
/// package shipping one.
///
/// Keyed by namespace rather than by tool name because there is no tool name
/// to key on: a server advertises its tools when it connects, which is after
/// this map is built and after the gate is assembled. Every one of them is
/// called `mcp__<server>__…` though, and the server is declared, so the
/// prefix is exactly as knowable as a script tool's name is.
///
/// The trust gate is the roster's, as everywhere else here: a project-tier
/// package does not reach `contributed_mcp_servers` at all in an untrusted
/// checkout.
fn contributed_server_principals(workspace_root: &std::path::Path) -> Vec<(String, Principal)> {
    // A notice here would have no one to say it to — this is the authorization
    // map, not session start — and `load_mcp_plan` reports the same package's
    // broken file where a human is listening.
    let mut ignored = Vec::new();
    // The user's own names, so the collision rule reads the same here as in
    // `load_mcp_plan`: yours wins, and the package's copy is the one dropped.
    // Skipping it matters more here than there — a prefix left in this list
    // for a name the user's own server actually answers would authorize *your*
    // server's calls as somebody's plugin.
    let taken = crate::agent::own_mcp_server_names(workspace_root);
    crate::plugin_cmd::package::contributed_mcp_servers(workspace_root, &mut ignored)
        .into_iter()
        .flat_map(|contributed| {
            let plugin = contributed.plugin;
            contributed
                .servers
                .into_iter()
                .map(move |server| (server.name, Principal::Plugin(plugin.clone())))
        })
        .filter(|(server, _)| !taken.iter().any(|held| held == server))
        .map(|(server, plugin)| (stella_mcp::namespace_prefix(&server), plugin))
        .collect()
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
    with_journal(
        policy_stack_with(
            base,
            session_tool_policy(cfg),
            cfg.tool_advertisement,
            &cfg.steering_ledger,
            session_gate(&cfg.workspace_root),
            principal,
        ),
        bus,
    )
}

/// [`policy_stack`] with the policy passed in and no journal — the explicit
/// sibling, standing to it as [`session_stack_with_gate`] stands to
/// [`session_stack`].
///
/// It exists for one caller that genuinely cannot supply the others: a
/// best-of-N candidate's turn assembles its chain *inside the child's own
/// thread*, over the rooted registry that thread owns
/// ([`crate::subagent::SessionSubAgents::dispatch_in_workspace`]). A `&Config`
/// would have to be cloned across that boundary to derive a policy the caller
/// has already derived, and the rooted registry carries no session bus to
/// journal onto — so both are named here rather than re-derived there.
///
/// The gate is passed in for the same reason, and it is the call site that
/// matters most: this is the one that already carries
/// [`Principal::Plugin`], so a session
/// gate that reached every other stack and missed this one would be a rule
/// that never fires where it is needed (#3482).
pub(crate) fn policy_stack_with<'a>(
    base: &'a dyn ToolExecutor,
    policy: ToolPolicy,
    advertisement: ToolAdvertisement,
    ledger: &SteeringLedger,
    gate: Arc<dyn AuthzGate>,
    principal: Principal,
) -> GatedToolSet<'a> {
    let permitted = PolicyToolSet::new(base, policy);
    GatedToolSet::new_boxed(
        budgeted(Box::new(permitted), advertisement, ledger),
        gate,
        principal,
    )
}

/// Compose the tool allowance over `permitted`, or hand it back untouched.
///
/// `Full` returns the chain it was given rather than a forwarding layer that
/// filters nothing, so the lever's off state is the absence of a layer. "Off
/// advertises the whole surface" is then a fact about the composition rather
/// than a claim about a filter, which is what a bench arm measuring the lever
/// needs on its control side. `ledger` is untouched on that arm too: with no
/// tool withheld there is nothing for a shared total to decide.
///
/// On the `Lean` arm the budget the packer receives is not the one the
/// workspace declared — it is what this turn's volatile block left of it.
/// `ledger` answers once per declared allowance and holds that answer, so a
/// later spend cannot re-rank an array the provider is already caching.
fn budgeted<'a>(
    permitted: Box<dyn ToolExecutor + 'a>,
    advertisement: ToolAdvertisement,
    ledger: &SteeringLedger,
) -> Box<dyn ToolExecutor + 'a> {
    match advertisement {
        ToolAdvertisement::Full => permitted,
        ToolAdvertisement::Lean(declared) => {
            let lean = LeanToolSet::new(permitted, ledger.settle(declared));
            lean.report_drops();
            Box::new(lean)
        }
    }
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
            ToolAdvertisement::Full,
            &SteeringLedger::default(),
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
                stack(&leaf, session_gate(std::path::Path::new(".")))
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
            foundry_runtime: Default::default(),
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
            ToolAdvertisement::Full,
            &SteeringLedger::default(),
            session_gate(dir.path()),
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

    /// **The tool-origin witness, through the *shipped* composition.**
    ///
    /// The stagnation rung exempts a tool that came from outside the binary,
    /// and it learns that from `ToolExecutor::tool_origin`. The answer starts
    /// three or four layers down — the registry knows its catalog rows, the
    /// MCP set knows its namespace, the custom set knows its manifest — and
    /// has to survive every decorator above it. The port's default is `None`,
    /// so a layer that forgets to forward reports "unknown" and the exemption
    /// silently stops reaching the session that needed it.
    ///
    /// Asserted through `session_stack_with_gate` with the skill plane on
    /// top, which is the chain a turn driver mounts, for the same reason the
    /// gate witness above is: a future decorator that forgets to forward
    /// fails here, and nowhere else.
    #[tokio::test]
    async fn the_production_tool_stack_forwards_tool_origin() {
        use stella_core::loop_detect::ToolOrigin;
        use stella_tools::skill_plane::{SkillInvocationPlane, SkillScopedTools};

        let dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(stella_tools::registry::ToolRegistry::new(
            dir.path().to_path_buf(),
        ));
        let mut client = stella_mcp::McpClient::new(
            "vendor",
            Box::new(CannedTransport {
                called: Arc::new(std::sync::Mutex::new(false)),
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
            ToolAdvertisement::Full,
            &SteeringLedger::default(),
            session_gate(dir.path()),
            Principal::User,
        );
        let view = SkillScopedTools::new(&stack, SkillInvocationPlane::new());

        assert_eq!(
            view.tool_origin("task_list"),
            Some(ToolOrigin::Builtin),
            "a catalog row is a built-in and the rung must keep firing for it"
        );
        assert_eq!(
            view.tool_origin("mcp__vendor__deploy"),
            Some(ToolOrigin::Mcp),
            "a server's tool, whose constant ack is its own design"
        );
        assert_eq!(
            view.tool_origin("my_tool"),
            Some(ToolOrigin::Custom),
            "a .stella/tools script, which may print one constant line"
        );
        assert_eq!(
            view.tool_origin("no_such_tool"),
            None,
            "a name nothing registered is unknown, not a built-in"
        );
    }

    /// **The skill-invocation witness, through the shipped composition.**
    /// The skill invocation plane composed over the assembled session chain — the
    /// position every turn driver mounts it at — is exactly the
    /// `operator ∧ grant` intersection: a live grant DENIES a disallowed
    /// tool at execution time, a tool the operator switched off stays off
    /// even when the grant names it, the granted-and-permitted call still
    /// runs, and `active_skill_slugs` answers the live slug through the
    /// stack — the port every shipped executor answered empty before this
    /// plane existed.
    #[tokio::test]
    async fn a_skill_grant_over_the_session_stack_denies_disallowed_and_never_widens() {
        use stella_tools::skill_plane::{SkillInvocationPlane, SkillScopedTools};

        let dir = tempfile::tempdir().unwrap();
        let registry = stella_tools::registry::ToolRegistry::new(dir.path().to_path_buf());
        // The operator switched `save_state` off; the grant below names it
        // anyway, which must change nothing.
        let policy = ToolPolicy::from_switches(vec![("save_state".to_string(), false)]);
        let stack = session_stack_with_gate(
            &registry,
            Vec::new(),
            dir.path().to_path_buf(),
            policy,
            ToolAdvertisement::Full,
            &SteeringLedger::default(),
            session_gate(dir.path()),
            Principal::User,
        );
        let plane = SkillInvocationPlane::new();
        let view = SkillScopedTools::new(&stack, plane.clone());

        let _span = plane.begin(
            "generate-quarter-seed",
            Some(&["task_list".to_string(), "save_state".to_string()]),
        );
        assert_eq!(
            view.active_skill_slugs(),
            vec!["generate-quarter-seed".to_string()],
            "the live invocation answers through the shipped chain"
        );
        // Granted and operator-permitted: runs.
        assert!(
            matches!(
                view.execute("task_list", &json!({})).await,
                ToolOutput::Ok { .. }
            ),
            "anti-vacuity: the granted call must run"
        );
        // Outside the grant: denied by the plane, naming the skill.
        match view.execute("get_state", &json!({"key": "k"})).await {
            ToolOutput::Error { message, .. } => assert!(
                message.contains("generate-quarter-seed"),
                "the denial names the invoking skill: {message}"
            ),
            other => panic!("a call outside the grant must be denied, got {other:?}"),
        }
        // Named by the grant but operator-denied below: stays denied — the
        // grant selects within the operator surface, never re-enables.
        assert!(
            matches!(
                view.execute("save_state", &json!({"key": "k", "value": "v"}))
                    .await,
                ToolOutput::Error { .. }
            ),
            "a grant must never re-enable an operator-denied tool"
        );
    }

    /// A base with `count` fat schemas, so an allowance can actually bind.
    struct WideLeaf {
        count: usize,
    }

    #[async_trait]
    impl ToolExecutor for WideLeaf {
        fn schemas(&self) -> Vec<ToolSchema> {
            (0..self.count)
                .map(|i| ToolSchema {
                    name: format!("builtin_{i:02}"),
                    description: "x".repeat(400),
                    input_schema: json!({"type": "object"}),
                    read_only: true,
                    speculation_safe: false,
                })
                .collect()
        }
        async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
            ToolOutput::Ok {
                content: format!("ran {name}"),
                data: None,
            }
        }
    }

    /// The whole tool surface, costed the way the packer costs it.
    fn full_cost(leaf: &WideLeaf) -> u64 {
        leaf.schemas()
            .iter()
            .map(stella_core::steering::tools::schema_tokens)
            .sum()
    }

    /// How many schemas the assembled session chain advertises under
    /// `declared`, given a `ledger` the turn's block has already spent from.
    fn advertised(
        leaf: &WideLeaf,
        declared: stella_core::steering::tools::ToolBudget,
        ledger: &SteeringLedger,
    ) -> usize {
        session_stack_with_gate(
            leaf,
            Vec::new(),
            PathBuf::from("."),
            ToolPolicy::allow_all(),
            ToolAdvertisement::Lean(declared),
            ledger,
            Arc::new(NoAuthz),
            Principal::User,
        )
        .schemas()
        .len()
    }

    /// A declared allowance wide enough to hold `leaf`'s whole surface.
    fn wide_enough(leaf: &WideLeaf) -> stella_core::steering::tools::ToolBudget {
        stella_core::steering::tools::ToolBudget {
            max_tokens: full_cost(leaf),
            mcp_max_tokens: full_cost(leaf),
        }
    }

    /// **The witness.** Two turns over one tool set and one declared
    /// allowance. The turn whose records filled most of the allowance
    /// advertises strictly fewer tools than the turn that recalled nothing.
    ///
    /// Before the ledger, [`budgeted`] read the declared allowance and
    /// nothing else, so both turns advertised the same array: a rule and a
    /// tool schema each spent a budget the other could not see.
    #[test]
    fn a_turn_whose_records_fill_the_allowance_advertises_fewer_tools() {
        let leaf = WideLeaf { count: 40 };
        let declared = wide_enough(&leaf);

        let quiet = SteeringLedger::default();
        let recalled = SteeringLedger::default();
        recalled.spend(declared.max_tokens * 3 / 4);

        let with_no_records = advertised(&leaf, declared, &quiet);
        let with_records = advertised(&leaf, declared, &recalled);

        assert_eq!(
            with_no_records, leaf.count,
            "the control: an unspent allowance holds the whole surface"
        );
        assert!(
            with_records < with_no_records,
            "a records-heavy turn must advertise fewer tools: {with_records} against \
             {with_no_records}"
        );
        assert!(
            with_records > 0,
            "and the allowance was not so tight that the arms differ trivially"
        );
    }

    /// The array a session settles is settled once. A block rendered after it
    /// — a mid-turn re-query, or the next turn's own recall — is recorded and
    /// changes nothing, because the tools array is prompt-cache prefix and
    /// re-ranking it re-bills the conversation.
    #[test]
    fn a_later_block_does_not_move_an_array_the_session_already_settled() {
        let leaf = WideLeaf { count: 40 };
        let declared = wide_enough(&leaf);
        let ledger = SteeringLedger::default();

        let first = advertised(&leaf, declared, &ledger);
        ledger.spend(declared.max_tokens);

        assert_eq!(
            advertised(&leaf, declared, &ledger),
            first,
            "a spend after the array settled must not re-rank it"
        );
    }

    /// With the lever off nothing is withheld, however much the block spent —
    /// the control arm a bench comparison needs, and the promise that turning
    /// steering off never takes a tool away.
    #[test]
    fn the_lever_off_advertises_every_tool_whatever_the_block_spent() {
        let leaf = WideLeaf { count: 40 };
        let ledger = SteeringLedger::default();
        ledger.spend(u64::MAX);

        let stack = session_stack_with_gate(
            &leaf,
            Vec::new(),
            PathBuf::from("."),
            ToolPolicy::allow_all(),
            ToolAdvertisement::Full,
            &ledger,
            Arc::new(NoAuthz),
            Principal::User,
        );

        assert_eq!(stack.schemas().len(), leaf.count);
    }
}
