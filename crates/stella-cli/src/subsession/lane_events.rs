//! The event stream a worker lane sends on, and the task tag it carries.
//!
//! Every work event carries a `task_id`. The lead session fills it in by
//! reading the board it works through.
//!
//! A worker lane must not read a board. It has its own tool registry, with
//! its own board, and that board is empty. The read answers `None`, so the
//! lane's tool calls, file changes and metering rows all go out with no tag.
//! `Store::task_events` for the task then comes back empty, and
//! `Store::task_cost` reports `$0.00` for work that ran and was paid for.
//!
//! A `task_assign` lane works one board task, and
//! [`stella_core::tasks::SpawnRequest`]'s `task_id` names it at spawn time.
//! So the tag here is a **constant**.
//!
//! A constant is also the safe answer. Board ids are per-session ordinals, so
//! the worker's own board offers a `"1"` that means some other task. Reading
//! it would file this lane's evidence in a stranger's ledger.
//!
//! A `req:<n>` lane names no task and gets no source. Its work stays untagged,
//! in no task's ledger, which is the truth about a prompt nobody filed against
//! a task.

use stella_core::{EventSender, RunningTask};
use stella_protocol::AgentEvent;
use tokio::sync::mpsc::UnboundedSender;

use super::SubSessionSpec;

/// Open the sender a worker lane's registry and engine both send through.
///
/// One sender for both halves, for the reason `turn_files::open_turn_streams`
/// gives: the tag belongs to the *stream*, not to the engine. A lane that
/// tagged its registry's events and not its engine's would file half the
/// evidence, and the half it dropped is the half that carries the cost.
///
/// The caller must drop the returned sender before closing the lane's stream.
/// It is one more clone of `tx`, and a clone still alive leaves the
/// forwarder's `recv()` pending forever, wedging the lane after the deck has
/// painted it done.
pub(crate) fn open_lane_stream(
    spec: &SubSessionSpec,
    tx: &UnboundedSender<AgentEvent>,
) -> EventSender {
    let events = EventSender::new(tx.clone());
    if let Some(task) = spec.board_task.clone() {
        events.attach_running_task(RunningTask::from_fn(move || Some(task.clone())));
    }
    events
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use stella_protocol::{TaskId, ToolCall};
    use stella_store::Store;

    use super::*;
    use crate::command_deck::{SharedRevisions, close_turn_stream, spawn_forwarder};

    fn spec_for(lane: &str, board_task: Option<&str>) -> SubSessionSpec {
        SubSessionSpec {
            lane: lane.to_string(),
            title: lane.to_string(),
            purpose: String::new(),
            prompt: "p".to_string(),
            notify_title: String::new(),
            dispatched_by: None,
            board_task: board_task.map(TaskId::new),
        }
    }

    fn tool_start(call_id: &str) -> AgentEvent {
        AgentEvent::ToolStart {
            call: ToolCall {
                call_id: call_id.to_string(),
                name: "edit_file".to_string(),
                input: serde_json::json!({ "path": "src/auth.rs" }),
            },
            sub_agent_id: None,
            task_id: None,
        }
    }

    fn insight_scope() -> crate::cache_insight::InsightScope {
        crate::cache_insight::InsightScope {
            provider_id: "anthropic".into(),
            cache_ttl: stella_model::CacheTtl::default(),
            opens_execute_stage: true,
        }
    }

    /// Script one lane end to end: open its stream, send `events` through it,
    /// close the stream the way `run_worker` does, and hand back the store the
    /// forwarder persisted into.
    ///
    /// Everything but the model is the shipping path — the same
    /// `spawn_forwarder`, the same `close_turn_stream`, and an execution row
    /// stamped with the deck's session id, which is the join
    /// `Store::task_events` makes.
    async fn scripted_lane(
        session: &str,
        spec: &SubSessionSpec,
        events: Vec<AgentEvent>,
    ) -> Arc<Store> {
        let root = tempfile::tempdir().expect("workspace root");
        let store = Arc::new(Store::open(root.path()).expect("store"));
        let execution = store
            .begin_execution("deck-sub", "p", "anthropic", "m")
            .expect("execution row");
        store
            .set_execution_session(execution, session)
            .expect("the lane's row carries the deck's session");

        let registry = stella_tools::ToolRegistry::new(root.path().to_path_buf());
        let (in_tx, _in_rx) = tokio::sync::mpsc::unbounded_channel();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let forwarder = spawn_forwarder(
            rx,
            Some((Arc::clone(&store), execution)),
            insight_scope(),
            in_tx,
            spec.lane.clone(),
            Some(registry.task_board()),
            SharedRevisions::default(),
        );

        let lane = open_lane_stream(spec, &tx);
        registry.attach_events(lane.clone());
        for event in events {
            lane.send(event).expect("the forwarder is alive");
        }
        // The drop `run_worker` owes before the close, for the reason
        // [`open_lane_stream`]'s docs give.
        drop(lane);
        close_turn_stream(&registry, tx, forwarder).await;
        store
    }

    /// The witness: a delegated task's ledger holds its worker's rows.
    ///
    /// A lane whose sender carries no running-task source sends every event
    /// with `task_id: None`. `Store::task_events` filters on
    /// `events.task_id = ?`, so it matches none of them and hands back an
    /// empty journal for work that really ran — which is what this assertion
    /// catches.
    #[tokio::test]
    async fn a_delegated_lanes_work_lands_in_that_tasks_ledger() {
        let store = scripted_lane(
            "deck-1",
            &spec_for("sub:4", Some("4")),
            vec![tool_start("c1"), tool_start("c2")],
        )
        .await;

        let journal = store
            .task_events("deck-1", &TaskId::new("4"))
            .expect("read the task ledger");
        assert_eq!(
            journal.events.len(),
            2,
            "both of the worker's calls belong to the task it was assigned: {:?}",
            journal.events
        );
        assert!(
            journal
                .events
                .iter()
                .all(|record| record.event.task_id().map(TaskId::as_str) == Some("4")),
            "each persisted row names the task: {:?}",
            journal.events
        );
    }

    /// The other direction. A `req:<n>` lane comes from a person typing into
    /// the composer. It is filed against no task, so it must claim none.
    #[tokio::test]
    async fn a_prompt_lane_names_no_task_and_stays_untagged() {
        let store = scripted_lane("deck-1", &spec_for("req:1", None), vec![tool_start("c1")]).await;

        let journal = store
            .task_events("deck-1", &TaskId::new("1"))
            .expect("read the task ledger");
        assert!(
            journal.events.is_empty(),
            "an untasked lane must not borrow a board id: {:?}",
            journal.events
        );
    }
}
