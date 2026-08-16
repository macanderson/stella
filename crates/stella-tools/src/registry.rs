//! Tool trait + registry. The agent loop drives every tool through
//! `ToolRegistry::execute` — no tool-specific code lives outside this crate.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use stella_core::bus::{self, HookBus, names as hook_names};
use stella_core::ports::ToolExecutor;
use stella_protocol::tool::{ToolOutput, ToolSchema};

pub mod approval;
mod executor;
mod output;
mod validate;

/// One tool the agent can call. Input arrives as the model-produced JSON;
/// output is always a typed `ToolOutput` (never a bare string).
#[async_trait]
pub trait Tool: Send + Sync {
    /// Schema advertised to the model: name, description, JSON Schema.
    fn schema(&self) -> ToolSchema;

    /// Execute the tool. `root` is the workspace root for path resolution;
    /// tools must never read or write outside it without explicit opt-in.
    async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput;

    /// Whether sibling calls to this tool (and to read-only tools) in one
    /// step may be dispatched concurrently despite the schema not claiming
    /// `read_only`. The registry aggregates this into
    /// `ToolExecutor::parallel_safe_names` for the engine's dispatch
    /// grouping. Defaults to false — the safe direction. Override only when
    /// the tool mutates no workspace state AND its own machinery is built
    /// for concurrent siblings; the shipped case is the sub-agent spawn
    /// tool (`crate::subagent`), whose children are read-only by
    /// construction and whose dispatcher carves budget per child.
    fn parallel_safe(&self) -> bool {
        false
    }

    /// The parked-wait request this tool deposited during its last
    /// `execute`, taken destructively — the registry aggregates it into
    /// `ToolExecutor::drain_wait_request` and the engine parks the turn on
    /// it at the next step boundary (#1471, `stella_core::waiting`).
    /// Defaults to `None`: almost no tool waits on external state.
    fn take_wait_request(&self) -> Option<stella_core::WaitRequest> {
        None
    }

    /// Long-running processes this tool started that are still up, read
    /// non-destructively — the registry aggregates these into
    /// `ToolExecutor::live_services` and the engine's end-of-turn assertion
    /// names them before a turn's declaration stands (#2764,
    /// `stella_core::driver::live_services`). Defaults to empty: almost no
    /// tool owns state that outlives the turn that made it.
    ///
    /// Answering this must never change anything, least of all stop a
    /// process — a service left running can be the correct final state
    /// (#2666).
    fn live_services(&self) -> Vec<stella_core::LiveService> {
        Vec::new()
    }
}

/// Registry of the built-in tools, keyed by name: the sub-agent spawn tool
/// (`task`), the session task board (`task_*`), the session scratch state
/// plane (`save_state` / `get_state` / `list_state` / `delete_state`), and
/// the environment report (`get_environment`).
///
/// Also the session's carrier for the per-execution ledgers a host drains —
/// sub-agent spend, queued spawn requests, MCP usage, agent-definition uses —
/// so `record_execution_end` has one handle for every ledger.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    root: PathBuf,
    /// Per-session agent-invocation ledger (see [`crate::agent_use`]) —
    /// drained once per execution by the persistence layer, as an event log,
    /// never aggregated.
    agent_uses: std::sync::Mutex<crate::agent_use::AgentUseLedger>,
    /// The session's MCP tool-usage ledger. External MCP tools bypass this
    /// registry (they run through `stella-mcp`'s `McpToolSet`), so nothing here
    /// writes to it — the CLI hands a clone ([`ToolRegistry::mcp_usage_ledger`])
    /// to the `McpToolSet` at connect, which appends, and drains it per
    /// execution via [`ToolRegistry::take_mcp_usage`]. The registry is just the
    /// carrier so `record_execution_end` has one handle for every ledger.
    mcp_usage: stella_core::mcp_usage::McpUsageLedger,
    /// The session task board, shared with the six registered `task_*` tool
    /// instances. The CLI snapshots it into `AgentEvent::TaskUpdate` after
    /// executions via [`ToolRegistry::task_board`].
    task_board: crate::tasks::TaskBoardHandle,
    /// Sub-agent spawn requests queued by `task_assign`, drained once per
    /// execution by the session driver via
    /// [`ToolRegistry::take_spawn_requests`] — a drain discipline, so no
    /// request is dispatched twice.
    spawn_queue: crate::tasks::SpawnQueue,
    /// Whether a host will drain `spawn_queue`. Set by
    /// [`ToolRegistry::enable_task_delegation`]; `false` here makes
    /// `task_assign` refuse rather than confirm an undispatched request.
    spawn_dispatch: crate::tasks::SpawnDispatch,
    /// The sub-agent dispatcher (#922), filled by the host after
    /// construction via [`ToolRegistry::attach_sub_agent_dispatcher`] — it
    /// needs this registry as the child's tool set, so taking it as a
    /// constructor argument would be a reference cycle.
    sub_agent_dispatcher: crate::subagent::DispatcherSlot,
    /// Spend by sub-agents the `task` tool dispatched, drained by the engine
    /// at each step-boundary budget check. A tool cannot charge the turn's
    /// guard directly (the engine holds it mutably), so this ledger is how
    /// `--spend-limit` stays a hard ceiling once turns nest.
    sub_agent_spend: stella_core::subagent::SubAgentSpendLedger,
    /// The pause gate and steering tap of the turn currently running, in the
    /// owned form a session-scoped dispatcher can hold — published by the
    /// driver alongside `events` below and read at dispatch time, so a child
    /// pauses and stops with the turn that asked for it.
    turn_controls: crate::subagent::TurnControlsSlot,
    bus: std::sync::RwLock<Option<HookBus>>,
    /// The session's approval flow (#2676): how a `RequireApproval` gate
    /// decision reaches a human. Headless by default (the structured
    /// grant-path refusal); a host injects its responder at assembly time
    /// via [`ToolRegistry::attach_approval_responder`].
    approval: std::sync::RwLock<approval::ApprovalBroker>,
    /// The live policy-plane bridge subscription (receipts spec §6.4), if
    /// [`ToolRegistry::bridge_policy_plane`] wired one. Held so a re-bridge
    /// (a new turn's event channel) replaces — unsubscribes — the previous
    /// observer instead of accumulating stale senders.
    policy_bridge: std::sync::Mutex<Option<stella_core::bus::HookSubscription>>,
    /// The turn's event channel, when a host attached one — read by the
    /// session-scoped sub-agent dispatcher at dispatch time
    /// ([`ToolRegistry::events`]) so a child turn streams onto the channel of
    /// the turn that spawned it.
    events: std::sync::RwLock<Option<stella_core::EventSender>>,
}

impl ToolRegistry {
    /// Construct the built-in tool set rooted at `root`.
    ///
    /// The session scratch directory (`crate::scratch::ScratchDir`) is
    /// created here; when it cannot be initialized the four state tools are
    /// withheld (a truthfully absent capability, not a dead schema) and the
    /// environment report states the absence.
    pub fn new(root: PathBuf) -> Self {
        let task_board: crate::tasks::TaskBoardHandle = Arc::default();
        let spawn_queue: crate::tasks::SpawnQueue = Arc::default();
        let spawn_dispatch: crate::tasks::SpawnDispatch = Arc::default();
        let sub_agent_dispatcher: crate::subagent::DispatcherSlot = Arc::default();
        let sub_agent_spend: stella_core::subagent::SubAgentSpendLedger = Arc::default();
        let scratch = crate::scratch::ScratchDir::new().map(Arc::new);
        let scratch_path = scratch.as_ref().ok().map(|s| s.path().to_path_buf());

        let mut entries: Vec<Arc<dyn Tool>> = vec![
            Arc::new(crate::tasks::TaskCreate(task_board.clone())),
            Arc::new(crate::tasks::TaskList(task_board.clone())),
            Arc::new(crate::tasks::TaskStart(task_board.clone())),
            Arc::new(crate::tasks::TaskComplete(task_board.clone())),
            Arc::new(crate::tasks::TaskCancel(task_board.clone())),
            Arc::new(crate::tasks::TaskAssign(
                task_board.clone(),
                spawn_queue.clone(),
                spawn_dispatch.clone(),
            )),
            // Registered unconditionally — see `attach_sub_agent_dispatcher`
            // on why an unattached dispatcher is still the honest shape.
            Arc::new(crate::subagent::SpawnSubAgent::new(
                sub_agent_dispatcher.clone(),
            )),
            Arc::new(crate::environment::GetEnvironment {
                scratch_dir: scratch_path,
            }),
        ];
        match scratch {
            Ok(scratch) => entries.extend([
                Arc::new(crate::scratch::SaveState(scratch.clone())) as Arc<dyn Tool>,
                Arc::new(crate::scratch::GetState(scratch.clone())),
                Arc::new(crate::scratch::ListState(scratch.clone())),
                Arc::new(crate::scratch::DeleteState(scratch)),
            ]),
            Err(_) => eprintln!("warning: session scratch directory failed to initialize"),
        }

        let mut tools: HashMap<String, Arc<dyn Tool>> = HashMap::new();
        for tool in entries {
            let name = tool.schema().name;
            tools.insert(name, tool);
        }
        Self {
            tools,
            root,
            agent_uses: std::sync::Mutex::new(crate::agent_use::AgentUseLedger::default()),
            mcp_usage: Arc::default(),
            task_board,
            spawn_queue,
            spawn_dispatch,
            sub_agent_dispatcher,
            sub_agent_spend,
            turn_controls: crate::subagent::TurnControlsSlot::default(),
            bus: std::sync::RwLock::new(None),
            approval: Default::default(),
            policy_bridge: std::sync::Mutex::new(None),
            events: std::sync::RwLock::new(None),
        }
    }

    /// Attach the session's extension hook bus. From this point every tool
    /// call runs the blocking `tool.call.requested` policy chain before
    /// executing, and emits the observer events documented in
    /// `website/content/docs/agent-tools/hooks.mdx`. Also emits one
    /// `tool.registered` per registered tool, name-sorted, so extensions see
    /// the tool surface up front.
    pub fn attach_bus(&self, bus: HookBus) {
        let mut schemas = ToolRegistry::schemas(self);
        schemas.sort_by(|a, b| a.name.cmp(&b.name));
        for schema in schemas {
            bus.emit_named(
                hook_names::TOOL_REGISTERED,
                serde_json::json!({
                    "tool": schema.name,
                    "read_only": schema.read_only,
                    "speculation_safe": schema.speculation_safe,
                }),
            );
        }
        *self.bus.write().unwrap_or_else(|p| p.into_inner()) = Some(bus);
    }

    /// Bridge the attached bus's policy/extension audit plane into an
    /// `AgentEvent` stream (receipts spec §6.4, #364 gap 6): every
    /// `policy.evaluated` / `policy.blocked` / `approval.requested` /
    /// `secret.detected` the bus emits lands as a content-free
    /// [`stella_protocol::AgentEvent::PolicyDecision`] on `events`, so
    /// whatever journal the host hangs off that stream carries the policy
    /// plane too — previously a process-ephemeral ring that evaporated at
    /// exit. Returns `false` (registering nothing) when no bus is attached:
    /// the plane doesn't exist without one. Call after [`Self::attach_bus`];
    /// calling again (a new turn's event channel) replaces the previous
    /// bridge, so stale senders never accumulate as observer failures.
    pub fn bridge_policy_plane(&self, events: stella_core::EventSender) -> bool {
        let Some(bus) = self.bus() else {
            return false;
        };
        let subscription = stella_core::bus::bridge_policy_plane(&bus, events);
        // Dropping the previous subscription (if any) unsubscribes it.
        *self.policy_bridge.lock().unwrap_or_else(|p| p.into_inner()) = Some(subscription);
        true
    }

    /// The attached hook bus, if any (cheap clone — shared inner).
    fn bus(&self) -> Option<HookBus> {
        self.bus.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Publish this turn's event channel on the registry, for the
    /// session-scoped consumers that read it at dispatch time (the sub-agent
    /// dispatcher, the policy-plane bridge).
    ///
    /// Call once per turn with that turn's channel; a later call replaces the
    /// previous sender, so a stale channel is dropped rather than accumulated.
    pub fn attach_events(&self, events: stella_core::EventSender) {
        *self.events.write().unwrap_or_else(|p| p.into_inner()) = Some(events);
    }

    /// Release this turn's event channel: the counterpart every turn owes
    /// [`Self::attach_events`] and [`Self::bridge_policy_plane`].
    ///
    /// Both hand the registry an `EventSender`, which is an `Arc<dyn Fn>` over
    /// the renderer's channel — so while the registry holds one, that channel
    /// has a live sender. A one-shot run's registry outlives its turn, so the
    /// caller dropping its own sender never closed the channel: the renderer's
    /// `recv()` loop stayed pending and a *completed* `stella run` hung until
    /// something killed it (#960). Detaching here is what actually ends the
    /// stream.
    ///
    /// Idempotent, and safe on a registry that never attached either one.
    pub fn detach_event_stream(&self) {
        *self.events.write().unwrap_or_else(|p| p.into_inner()) = None;
        // Dropping the subscription unsubscribes it, releasing the sender the
        // bridge closure captured.
        *self.policy_bridge.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// Let the `task` tool run sub-agents through `dispatcher` (#922).
    ///
    /// Late attachment, not a constructor argument: a dispatcher needs this
    /// registry as the child's tool set, so the two would otherwise own each
    /// other. Until it is called the tool reports sub-agents as unavailable —
    /// a truthful result, not a silently missing capability. Once per
    /// session; a later call replaces the previous dispatcher.
    pub fn attach_sub_agent_dispatcher(
        &self,
        dispatcher: std::sync::Arc<dyn stella_core::subagent::SubAgentDispatcher>,
    ) {
        *self
            .sub_agent_dispatcher
            .write()
            .unwrap_or_else(|p| p.into_inner()) = Some(dispatcher);
    }

    /// The dispatcher this registry runs sub-agents through, if one is
    /// attached.
    ///
    /// For a host building a *second* registry that should delegate through
    /// the same runner — a best-of-N candidate workspace is the case that
    /// motivated it. Children run read-only, so sharing the session's runner
    /// costs the candidate nothing it needed: a research child reads to
    /// understand the codebase, and the snapshot it would otherwise read is a
    /// copy of the same tree.
    pub fn sub_agent_dispatcher(
        &self,
    ) -> Option<std::sync::Arc<dyn stella_core::subagent::SubAgentDispatcher>> {
        self.sub_agent_dispatcher
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Publish this turn's pause gate and steering tap, until the returned
    /// guard drops.
    ///
    /// The companion to [`Self::attach_events`], called from the same place
    /// for the same reason: a sub-agent dispatcher is session-scoped, so
    /// anything turn-shaped it needs has to be read from here at dispatch
    /// time. Without this, a child dispatched by a tool call never sees the
    /// pause or the soft stop that governs its parent. A guard rather than a
    /// bare setter — see [`crate::subagent::TurnControlsGuard`].
    pub fn attach_turn_controls(
        &self,
        controls: stella_core::ports::TurnControls,
    ) -> crate::subagent::TurnControlsGuard {
        crate::subagent::TurnControlsGuard::attach(&self.turn_controls, controls)
    }

    /// The ledger a dispatcher charges finished children to (cheap clone —
    /// shared inner), drained by the engine at each step-boundary check.
    ///
    /// `pub` because settling is the *dispatcher's* job, per
    /// [`stella_core::subagent::SubAgentDispatcher`]'s contract: it must
    /// happen the moment a child stops, on the child's own thread, or a
    /// parent cancelled mid-`task` leaves paid-for dollars in no ledger at
    /// all.
    pub fn sub_agent_spend_ledger(&self) -> stella_core::subagent::SubAgentSpendLedger {
        self.sub_agent_spend.clone()
    }

    /// The current turn's boundary controls — empty between turns, and on a
    /// driver that publishes none.
    pub fn turn_controls(&self) -> stella_core::ports::TurnControls {
        self.turn_controls
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// The attached turn event sender, if any (cheap clone — shared inner).
    /// `pub` so a session-scoped sub-agent dispatcher (#922) can read the
    /// *current* turn's sender at dispatch time rather than each driver
    /// having to re-attach a dispatcher of its own.
    pub fn events(&self) -> Option<stella_core::EventSender> {
        self.events
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// All tool schemas, for advertising to the model, sorted by name.
    ///
    /// Sorted because the map iterates in per-process-randomized `HashMap`
    /// order, and this list is serialized verbatim at position 0 of the
    /// prompt prefix. Prompt caching is a byte-level prefix match, so a
    /// deterministic order lets two processes (or a restart within the
    /// cache TTL) share the tools+system cache entry instead of each
    /// writing a divergent one.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        let mut schemas: Vec<ToolSchema> = self.tools.values().map(|t| t.schema()).collect();
        schemas.sort_by(|a, b| a.name.cmp(&b.name));
        schemas
    }

    /// Execute a tool by name. Returns an error `ToolOutput` if the name is
    /// unknown or the input is malformed — never panics.
    pub async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        let bus = self.bus();
        let started_at = std::time::Instant::now();

        // Extension policy: the `tool.call.requested` blocking chain. Runs
        // FIRST — a `modify` decision replaces the input, and everything
        // downstream (validation, execution) must see the final input, not
        // the original.
        let mut modified_input: Option<Value> = None;
        if let Some(bus) = &bus {
            match self.gate_tool_call(bus, name, input).await {
                Ok(replacement) => modified_input = replacement,
                Err(denied) => return denied,
            }
        }
        let input: &Value = modified_input.as_ref().unwrap_or(input);

        let tool = self.tools.get(name).cloned();

        // Dispatch-time input validation (#3144): a call whose input
        // contradicts the tool's advertised `input_schema` is refused here,
        // before anything downstream runs — see `registry/validate.rs`.
        if let Some(deny) = validate::refusal(tool.as_deref(), input) {
            return deny;
        }

        if let Some(bus) = &bus {
            bus.emit_named(
                hook_names::TOOL_CALL_STARTED,
                serde_json::json!({ "tool": name, "input": bus::sanitize_tool_input(input) }),
            );
        }

        let output = match &tool {
            Some(tool) => tool.execute(input, &self.root).await,
            None => ToolOutput::classified_error(
                stella_protocol::ErrorClass::NotFound,
                format!(
                    "unknown tool `{name}` — available: {}",
                    self.available_names()
                ),
            ),
        };
        // Post-execution output validation (#3285): a success whose
        // structured half breaks the tool's declared `output_schema` is a
        // tool defect, surfaced as a classified `Internal` error rather than
        // passed silently to the model — see `registry/output.rs`.
        let output = match output::defect(tool.as_deref(), &output) {
            Some(defect) => defect,
            None => output,
        };
        if let Some(bus) = &bus {
            let duration_ms = started_at.elapsed().as_millis() as u64;
            match &output {
                ToolOutput::Error { message, .. } => {
                    bus.emit_named(
                        hook_names::TOOL_CALL_FAILED,
                        serde_json::json!({
                            "tool": name, "error": message, "duration_ms": duration_ms,
                        }),
                    );
                }
                ToolOutput::Ok { .. } => {
                    bus.emit_named(
                        hook_names::TOOL_CALL_COMPLETED,
                        serde_json::json!({ "tool": name, "duration_ms": duration_ms }),
                    );
                }
            }
        }
        output
    }

    /// A clone of the session task-board handle, shared with the registered
    /// `task_*` tool instances — the CLI snapshots it into
    /// `AgentEvent::TaskUpdate` after each execution.
    pub fn task_board(&self) -> crate::tasks::TaskBoardHandle {
        self.task_board.clone()
    }

    /// A clone of the `task_assign` spawn-queue handle — for hosts that want
    /// to observe queued spawns without draining them.
    pub fn spawn_queue(&self) -> crate::tasks::SpawnQueue {
        self.spawn_queue.clone()
    }

    /// Declare that this host drains [`Self::take_spawn_requests`] and spawns
    /// what it finds, which is what lets `task_assign` accept a delegation.
    ///
    /// Late-attached for the same reason the sub-agent dispatcher is: whether
    /// anything will honor a queued request is a fact about the *host*, not
    /// about the registry, and it is not known at construction. Registries
    /// that never call this (a best-of-N candidate workspace, a bare embedding
    /// host) get a `task_assign` that refuses rather than one that confirms a
    /// spawn no one performs.
    pub fn enable_task_delegation(&self) {
        self.spawn_dispatch
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Drain the sub-agent spawn requests `task_assign` queued since the
    /// last drain — the session driver calls this exactly once per
    /// execution and dispatches each request through the fleet seam, so no
    /// request is ever handled twice.
    pub fn take_spawn_requests(&self) -> Vec<stella_core::tasks::SpawnRequest> {
        std::mem::take(&mut *self.spawn_queue.lock().unwrap_or_else(|p| p.into_inner()))
    }

    /// Record one invocation of an installed agent definition (see
    /// [`crate::agent_use`]). `version` is the definition's pinned version at
    /// invocation time; `reason` is a short free-text why/how (may be empty).
    pub fn record_agent_use(&self, agent: &str, version: u32, reason: &str) {
        self.agent_uses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record(crate::agent_use::AgentUseEvent {
                agent: agent.to_string(),
                version,
                reason: reason.to_string(),
            });
    }

    /// Take every agent invocation recorded since the last drain — the
    /// per-execution persistence step calls this exactly once per execution
    /// so each invocation lands in the store attributed to the execution it
    /// happened under.
    pub fn drain_agent_uses(&self) -> Vec<crate::agent_use::AgentUseEvent> {
        self.agent_uses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain()
    }

    /// A clone of the MCP usage-ledger handle, for handing to the `McpToolSet`
    /// at connect so its successful calls are recorded against this registry's
    /// ledger (which [`ToolRegistry::take_mcp_usage`] later drains).
    pub fn mcp_usage_ledger(&self) -> stella_core::mcp_usage::McpUsageLedger {
        self.mcp_usage.clone()
    }

    /// Drain the MCP tool calls recorded since the last drain — persisted
    /// under exactly one execution id so per-call counts never inflate.
    pub fn take_mcp_usage(&self) -> Vec<stella_core::mcp_usage::McpUsageRecord> {
        stella_core::mcp_usage::drain_usage(&self.mcp_usage)
    }

    /// Comma-separated sorted list of registered tool names, for error
    /// messages.
    pub fn available_names(&self) -> String {
        let mut names: Vec<String> = self.tools.keys().cloned().collect();
        names.sort();
        names.join(", ")
    }

    /// The workspace root this registry resolves paths against.
    pub fn root(&self) -> &PathBuf {
        &self.root
    }
}

#[cfg(test)]
mod tests;
