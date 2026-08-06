//! The deck's task-board decorator, split out of `command_deck.rs` to keep
//! it under the size gate (the `driver/settlement.rs` pattern).

use async_trait::async_trait;
use serde_json::Value;
use stella_core::ports::ToolExecutor;
use stella_protocol::{AgentEvent, TaskItem, ToolOutput, ToolSchema};
use stella_tools::ToolRegistry;
use tokio::sync::mpsc::UnboundedSender;

use crate::subsession::SupervisorMsg;

/// Mirrors the task board into the event stream: after any `task_*` tool
/// call the FULL board snapshot rides the turn's channel as
/// `AgentEvent::TaskUpdate` — persisted by the forwarder, so replay shows
/// the checklist exactly as it moved — and `task_assign`'s spawn requests
/// are handed to the driver's supervisor channel. `supervisor: None` is the
/// worker configuration (v1 delegation runs from the lead only; a worker's
/// stranded requests are reported on its lane by `crate::subsession`).
pub(crate) struct TaskTap<'a> {
    pub(crate) inner: &'a dyn ToolExecutor,
    pub(crate) events: UnboundedSender<AgentEvent>,
    pub(crate) registry: &'a ToolRegistry,
    pub(crate) supervisor: Option<UnboundedSender<SupervisorMsg>>,
}

#[async_trait]
impl ToolExecutor for TaskTap<'_> {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.inner.schemas()
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        let output = self.inner.execute(name, input).await;
        if name.starts_with("task_") {
            let tasks: Vec<TaskItem> = {
                let board = self.registry.task_board();
                let guard = board.lock().unwrap_or_else(|p| p.into_inner());
                guard.items().to_vec()
            };
            let _ = self.events.send(AgentEvent::TaskUpdate { tasks });
            if let Some(sup) = &self.supervisor {
                for request in self.registry.take_spawn_requests() {
                    let _ = sup.send(SupervisorMsg::SpawnTask(request));
                }
            }
        }
        output
    }

    /// Forwarded: this is a decorator, and a decorator that let the default
    /// `0.0` stand would silently drop sub-agent spend out of the parent's
    /// budget (see the port's contract).
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        self.inner.drain_sub_agent_spend_usd()
    }

    /// Forwarded for the same reason: a swallowed wait request silently
    /// turns parked waits (#1471) back into model-step polling.
    fn drain_wait_request(&self) -> Option<stella_core::WaitRequest> {
        self.inner.drain_wait_request()
    }
}
