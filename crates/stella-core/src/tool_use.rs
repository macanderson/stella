// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Which tool call an executor is running for.
//!
//! The engine dispatches each call inside [`scope`], under the id it already
//! uses for that call. Code under `ToolExecutor::execute` reads it back with
//! [`current`]. The MCP client sends it with each `tools/call`, so a gateway
//! can tie the request to the `PreToolUse` and `PostToolUse` hooks that name
//! the same call.

use std::future::Future;

use stella_protocol::CompletionMessage;

tokio::task_local! {
    static TOOL_USE_ID: String;
}

/// Run `future` as the tool call `tool_use_id`.
///
/// The id holds only while `future` is polled, so two calls joined in one
/// task each read their own. A task spawned inside does not inherit it.
pub async fn scope<F: Future>(tool_use_id: String, future: F) -> F::Output {
    TOOL_USE_ID.scope(tool_use_id, future).await
}

/// The id of the tool call this future runs for, or `None` outside one.
#[must_use]
pub fn current() -> Option<String> {
    TOOL_USE_ID.try_with(Clone::clone).ok()
}

/// The id of a call the engine makes on its own, with no model call behind
/// it, such as a parked wait's probe or a read that restores files after
/// compaction.
///
/// The id starts with the id of the last call the model made, then names
/// `kind` and `ordinal`. So each replay gets its own id, and a gateway never
/// sees one id on two requests.
#[must_use]
pub fn replay_id(messages: &[CompletionMessage], kind: &str, ordinal: usize) -> String {
    let anchor = messages
        .iter()
        .rev()
        .find_map(|message| message.tool_calls.last());
    match anchor {
        Some(call) => format!("{}.{kind}-{ordinal}", call.call_id),
        None => format!("{kind}-{ordinal}"),
    }
}

#[cfg(test)]
mod tests {
    use stella_protocol::{CompletionMessage, ToolCall};

    use super::{current, replay_id, scope};

    fn id(text: &str) -> Option<String> {
        Some(text.to_string())
    }

    #[tokio::test]
    async fn no_call_is_named_outside_a_scope() {
        assert_eq!(current(), None);
    }

    #[tokio::test]
    async fn a_scope_names_its_call_and_the_inner_one_wins() {
        let outer = scope("call_outer".into(), async {
            let inner = scope("call_inner".into(), async { current() }).await;
            (current(), inner)
        })
        .await;
        assert_eq!(outer, (id("call_outer"), id("call_inner")));
        assert_eq!(current(), None, "the id ends with its scope");
    }

    /// Two calls polled in one task, the way the engine joins read-only
    /// calls, never see each other's id.
    #[tokio::test]
    async fn two_joined_calls_each_read_their_own_id() {
        let (gate_a, wait_a) = tokio::sync::oneshot::channel::<()>();
        let (gate_b, wait_b) = tokio::sync::oneshot::channel::<()>();
        let first = scope("call_a".into(), async move {
            let before = current();
            let _ = gate_b.send(());
            let _ = wait_a.await;
            (before, current())
        });
        let second = scope("call_b".into(), async move {
            let _ = wait_b.await;
            let seen = current();
            let _ = gate_a.send(());
            seen
        });
        let (first, second) = tokio::join!(first, second);
        assert_eq!(first, (id("call_a"), id("call_a")));
        assert_eq!(second, id("call_b"));
    }

    /// A replay is named after the last call the model made, and two
    /// replays never share an id.
    #[test]
    fn a_replay_is_named_after_the_last_model_call() {
        let mut asked = CompletionMessage::assistant("");
        asked.tool_calls = ["call_1", "call_2"]
            .iter()
            .map(|id| ToolCall {
                call_id: (*id).to_string(),
                name: "ci_status".to_string(),
                input: serde_json::json!({}),
            })
            .collect();
        let messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("watch CI"),
            asked,
            CompletionMessage::user("a later turn"),
        ];
        assert_eq!(replay_id(&messages, "parked-wait", 1), "call_2.parked-wait-1");
        assert_eq!(replay_id(&messages, "parked-wait", 2), "call_2.parked-wait-2");
        let none = vec![CompletionMessage::user("no calls yet")];
        assert_eq!(replay_id(&none, "working-set-restore", 1), "working-set-restore-1");
    }
}
