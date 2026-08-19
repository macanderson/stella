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

impl<'a> TaskTap<'a> {
    /// Build the tap and tell the registry whether delegation is real here.
    ///
    /// The supervisor is the only thing that turns a queued `task_assign`
    /// request into a running sub-agent, so it is also the only honest answer
    /// to "may `task_assign` accept?" — binding the two in one constructor is
    /// what keeps the next tap from advertising a delegation it cannot
    /// perform.
    pub(crate) fn new(
        inner: &'a dyn ToolExecutor,
        events: UnboundedSender<AgentEvent>,
        registry: &'a ToolRegistry,
        supervisor: Option<UnboundedSender<SupervisorMsg>>,
    ) -> Self {
        if supervisor.is_some() {
            registry.enable_task_delegation();
        }
        Self {
            inner,
            events,
            registry,
            supervisor,
        }
    }
}

#[async_trait]
impl ToolExecutor for TaskTap<'_> {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.inner.schemas()
    }

    /// Forwarded unfiltered, like `schemas()` (#3287): the tap observes
    /// board writes, it does not change what exists.
    fn contracts(&self) -> Vec<stella_protocol::ToolContract> {
        self.inner.contracts()
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

    /// Forwarded: a decorator that let the empty default stand would silently
    /// turn the end-of-turn service assertion (#2764) off for every surface
    /// composed through it — the agent goes back to declaring a service done
    /// without ever being asked whether it is still listening.
    fn live_services(&self) -> Vec<stella_core::LiveService> {
        self.inner.live_services()
    }

    /// Forwarded: letting the empty default stand would silently serialize
    /// the inner executor's sibling spawns (see the port's contract). The
    /// spawn tool is `delegate`, not `task_*` — the tap never fires for it.
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        self.inner.parallel_safe_names()
    }

    /// Forwarded: letting the `None` default stand would drop the blocking
    /// hook chain and the approval flow for every tool dispatched under a
    /// deck session — the tap sits between the decorators that dispatch names
    /// of their own and the registry that owns the gate, so a gate it does not
    /// forward is a gate nothing consults (#2793).
    fn dispatch_gate(&self) -> Option<&dyn stella_core::ports::DispatchGate> {
        self.inner.dispatch_gate()
    }

    /// Forwarded: the deck's lead lane wraps the discovery mount (which owns
    /// the invocation plane) in this tap, so a tap that let the empty default
    /// stand would silently stop active skill bodies surviving summarization
    /// (#2685) for every deck session.
    fn active_skill_slugs(&self) -> Vec<String> {
        self.inner.active_skill_slugs()
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
                data: None,
            }
        }
        fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
            std::collections::HashSet::from(["delegate".to_string()])
        }
        fn active_skill_slugs(&self) -> Vec<String> {
            vec!["deploy".to_string()]
        }
    }

    /// The deck's lead lane wraps the whole stack in this tap last, so a tap
    /// that swallowed the claim would kill concurrent sibling spawns for
    /// every deck session no matter what the layers below advertised.
    #[test]
    fn the_task_tap_forwards_parallel_safe_names() {
        let inner = Claiming;
        let registry = ToolRegistry::new(std::path::PathBuf::from("."));
        let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tap = TaskTap {
            inner: &inner,
            events,
            registry: &registry,
            supervisor: None,
        };
        assert!(
            tap.parallel_safe_names().contains("delegate"),
            "the tap must forward the inner executor's concurrency claims"
        );
    }

    /// Same shape for the invocation plane (#2685): the tap sits above the
    /// discovery mount, so swallowing the live-slug answer would stop active
    /// skill bodies surviving summarization on every deck session.
    #[test]
    fn the_task_tap_forwards_active_skill_slugs() {
        let inner = Claiming;
        let registry = ToolRegistry::new(std::path::PathBuf::from("."));
        let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
        let tap = TaskTap {
            inner: &inner,
            events,
            registry: &registry,
            supervisor: None,
        };
        assert_eq!(
            tap.active_skill_slugs(),
            vec!["deploy".to_string()],
            "the tap must forward the inner executor's live invocations"
        );
    }
}
