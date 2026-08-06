//! The deck's task-board tap — split out of `command_deck.rs` (a god file
//! closed to growth) when the tap grew its `parallel_safe_names` forward.

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

    /// Forwarded: letting the empty default stand would silently serialize
    /// the inner executor's sibling spawns (see the port's contract). The
    /// spawn tool is `task`, not `task_*` — the tap never fires for it.
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        self.inner.parallel_safe_names()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A leaf claiming one parallel-safe name, standing in for the registry.
    struct Claiming;

    #[async_trait]
    impl ToolExecutor for Claiming {
        fn schemas(&self) -> Vec<ToolSchema> {
            Vec::new()
        }
        async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
            ToolOutput::Ok {
                content: String::new(),
            }
        }
        fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
            std::collections::HashSet::from(["task".to_string()])
        }
    }

    /// The deck's lead lane wraps the whole stack in this tap last, so a tap
    /// that swallowed the claim would kill concurrent sibling spawns for
    /// every deck session no matter what the layers below advertised.
    #[test]
    fn the_task_tap_forwards_parallel_safe_names() {
        let inner = Claiming;
        let registry = ToolRegistry::with_issue_backend(std::path::PathBuf::from("."), None);
        let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tap = TaskTap {
            inner: &inner,
            events,
            registry: &registry,
            supervisor: None,
        };
        assert!(
            tap.parallel_safe_names().contains("task"),
            "the tap must forward the inner executor's concurrency claims"
        );
    }
}
