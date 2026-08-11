//! Witness for #2676 (approval flow), exercised through the crate's PUBLIC
//! surface only: a `RequireApproval` gate decision on a surface with no
//! interactive responder must refuse with a structured message naming the
//! missing surface and the grant path. Before #2676 the refusal was a bare
//! "`task_list` requires approval before it can run: …" wall, so this test
//! fails on that tree and passes with the flow — verified both ways in a
//! detached worktree of the pre-change commit.
//!
//! The interactive halves of the flow (responder approve/deny, TTL expiry,
//! emit-before-park ordering, single-call grant scope) live beside the
//! implementation in `src/registry/approval.rs`, where the test-only
//! responder implementations can reach the port directly.

use stella_core::bus::{HookBus, HookDecision, names as hook_names};
use stella_tools::ToolRegistry;

#[tokio::test]
async fn headless_require_approval_names_the_missing_surface_and_grant_path() {
    let dir = tempfile::tempdir().unwrap();
    let reg = ToolRegistry::with_issue_backend(dir.path().to_path_buf(), None);
    let bus = HookBus::new("witness-2676");
    bus.on_blocking(hook_names::TOOL_CALL_REQUESTED, |_| {
        HookDecision::RequireApproval {
            reason: "policy wants a human".into(),
        }
    })
    .detach();
    reg.attach_bus(bus);

    match reg.execute("task_list", &serde_json::json!({})).await {
        stella_protocol::ToolOutput::Error { message } => {
            assert!(
                message.contains("no interactive surface"),
                "the refusal must name the missing surface, got: {message}"
            );
            assert!(
                message.contains("tools.task_list"),
                "the refusal must name the grant path, got: {message}"
            );
        }
        other => panic!("an unanswered approval must refuse: {other:?}"),
    }
}
