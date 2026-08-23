// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Unit tests for [`super`] — the remoted provider/tool/approval ports.
//! Split out of `remote.rs` as a sibling submodule (the
//! `driver/settlement.rs` pattern) to keep the parent under the file-size
//! gate; `use super::*`-style paths keep the private surface reachable.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use stella_core::bus::{HookBus, HookEvent, names as hook_names};
use stella_core::ports::ToolExecutor;
use stella_protocol::ToolOutput;

use super::RemoteToolExecutor;
use crate::frame::ServerFrame;
use crate::observe::event::TurnRef;
use crate::pending::Pending;

/// A tool port whose frame sink is already dead — the host-disconnected
/// path — plus a bus recording every event it sees.
fn disconnected_port() -> (RemoteToolExecutor, Arc<Mutex<Vec<String>>>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    // Dropping the receiver is what makes `send` fail, which is exactly
    // what a subscriber's connection going away does in production.
    drop(rx);
    let sink = crate::backlog::FrameSink::new(
        tx,
        crate::backlog::FrameBacklog::new(crate::backlog::DEFAULT_MAX_QUEUED_FRAMES),
    );
    let pending = Pending::new(
        crate::observe::null_observer(),
        TurnRef::new("turn-remotetest"),
    );
    let bus = HookBus::new("turn-remotetest");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    bus.on("tool.call.*", move |event: &HookEvent| {
        recorder
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(event.name.clone());
        Ok(())
    })
    .detach();
    let port = RemoteToolExecutor::new(
        Vec::new(),
        std::sync::Arc::new(stella_core::ports::NoAuthz),
        stella_core::ports::Principal::Host("test".to_string()),
        sink,
        pending,
        std::time::Duration::from_millis(50),
        Some(bus),
    );
    (port, seen)
}

/// Two executors sharing one registry never mint the same request id.
///
/// This is the sub-agent boundary (#1496). `run_session` builds a second
/// [`RemoteToolExecutor`] over a **clone** of the parent's [`Pending`] —
/// and cloning shares the map — so before the instance tag both counters
/// started at zero and both first calls were `tool-0`.
/// [`Pending::register_tool`] inserts with `HashMap::insert`, which
/// replaces rather than rejects, so the collision was silent in the worst
/// way: the parent's parked sender dropped, its waiter woke claiming the
/// host had abandoned the call, and the host's single answer for `tool-0`
/// resolved whichever request registered second.
///
/// Both dispatches here time out — the point is the ids on the frames that
/// went out, not the answers that never come back.
#[tokio::test]
async fn two_executors_over_one_registry_mint_distinct_request_ids() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let sink = crate::backlog::FrameSink::new(
        tx,
        crate::backlog::FrameBacklog::new(crate::backlog::DEFAULT_MAX_QUEUED_FRAMES),
    );
    let pending = Pending::new(
        crate::observe::null_observer(),
        TurnRef::new("turn-subagent-boundary"),
    );
    let timeout = std::time::Duration::from_millis(20);

    // The parent's port and the sub-agent view's, over the same registry.
    let parent = RemoteToolExecutor::new(
        Vec::new(),
        std::sync::Arc::new(stella_core::ports::NoAuthz),
        stella_core::ports::Principal::Host("test".to_string()),
        sink.clone(),
        pending.clone(),
        timeout,
        None,
    );
    let child = RemoteToolExecutor::new(
        Vec::new(),
        std::sync::Arc::new(stella_core::ports::NoAuthz),
        stella_core::ports::Principal::Host("test".to_string()),
        sink,
        pending,
        timeout,
        None,
    );

    let _ = parent.dispatch("parent_tool", &serde_json::json!({})).await;
    let _ = child.dispatch("child_tool", &serde_json::json!({})).await;

    let mut ids = Vec::new();
    while let Ok(frame) = rx.try_recv() {
        if let ServerFrame::ToolRequest { request_id, .. } = frame {
            ids.push(request_id);
        }
    }

    assert_eq!(
        ids.len(),
        2,
        "both dispatches emit a request frame: {ids:?}"
    );
    assert_ne!(
        ids[0], ids[1],
        "a sub-agent's tool call collided with its parent's in the shared \
         Pending registry — one of these two answers would resolve the \
         wrong request"
    );
}

/// A `tool.call.started` with no outcome after it leaves an observer
/// holding a call that never closes — and it would do so on precisely the
/// paths worth watching, the ones where the host went away. The bracket is
/// structural (`execute` wraps `dispatch`), and this is what keeps it so.
#[tokio::test]
async fn a_tool_call_the_host_never_receives_still_reports_an_outcome() {
    let (port, seen) = disconnected_port();
    let output = port
        .execute("echo", &serde_json::json!({ "text": "hi" }))
        .await;
    assert!(
        matches!(&output, ToolOutput::Error { message, .. } if message.contains("disconnected")),
        "expected the host-disconnected error, got {output:?}"
    );
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [
            hook_names::TOOL_CALL_STARTED.to_string(),
            hook_names::TOOL_CALL_FAILED.to_string(),
        ],
        "every `started` must be closed by an outcome, however the call ended"
    );
}

/// The same bracket, on the path the host *does* answer badly: a deadline
/// nobody meets. Distinct from the case above because it exits from the
/// other end of `dispatch`.
#[tokio::test]
async fn a_tool_call_the_host_never_answers_still_reports_an_outcome() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let sink = crate::backlog::FrameSink::new(
        tx,
        crate::backlog::FrameBacklog::new(crate::backlog::DEFAULT_MAX_QUEUED_FRAMES),
    );
    let pending = Pending::new(
        crate::observe::null_observer(),
        TurnRef::new("turn-remotetest"),
    );
    let bus = HookBus::new("turn-remotetest");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    bus.on("tool.call.*", move |event: &HookEvent| {
        recorder
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(event.name.clone());
        Ok(())
    })
    .detach();
    let port = RemoteToolExecutor::new(
        Vec::new(),
        std::sync::Arc::new(stella_core::ports::NoAuthz),
        stella_core::ports::Principal::Host("test".to_string()),
        sink,
        pending,
        std::time::Duration::from_millis(20),
        Some(bus),
    );
    let output = port.execute("echo", &serde_json::json!({})).await;
    assert!(
        matches!(&output, ToolOutput::Error { message, .. } if message.contains("deadline")),
        "expected the reverse-request deadline error, got {output:?}"
    );
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [
            hook_names::TOOL_CALL_STARTED.to_string(),
            hook_names::TOOL_CALL_FAILED.to_string(),
        ],
        "a wedged host is exactly when an observer must not be left waiting"
    );
}

/// #3167 witness: a reverse-request deadline is classified `Timeout`, and the
/// message bytes are exactly what they were before classification — the loop
/// detector and prompt cache compare these bytes. Fails on the pre-#3167
/// tree, where `class` is always `None`.
#[tokio::test]
async fn a_reverse_request_deadline_is_classified_timeout_with_unchanged_message_bytes() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let sink = crate::backlog::FrameSink::new(
        tx,
        crate::backlog::FrameBacklog::new(crate::backlog::DEFAULT_MAX_QUEUED_FRAMES),
    );
    let pending = Pending::new(
        crate::observe::null_observer(),
        TurnRef::new("turn-remotetest-timeout-class"),
    );
    let timeout = std::time::Duration::from_millis(20);
    let port = RemoteToolExecutor::new(
        Vec::new(),
        std::sync::Arc::new(stella_core::ports::NoAuthz),
        stella_core::ports::Principal::Host("test".to_string()),
        sink,
        pending,
        timeout,
        None,
    );
    let output = port.execute("echo", &serde_json::json!({})).await;
    assert_eq!(
        output,
        ToolOutput::classified_error(
            stella_protocol::ErrorClass::Timeout,
            format!(
                "serve host did not answer the `echo` tool call within {timeout:?} \
                 (reverse-request deadline)"
            ),
        )
    );
}

/// A fresh, connected reverse-RPC channel triple: the pending registry, a
/// frame receiver a test can drain, and the sink both new ports
/// (#1288) are constructed over.
fn live_channel() -> (
    crate::backlog::FrameSink,
    tokio::sync::mpsc::UnboundedReceiver<ServerFrame>,
    Pending,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let sink = crate::backlog::FrameSink::new(
        tx,
        crate::backlog::FrameBacklog::new(crate::backlog::DEFAULT_MAX_QUEUED_FRAMES),
    );
    let pending = Pending::new(
        crate::observe::null_observer(),
        TurnRef::new("turn-remotetest"),
    );
    (sink, rx, pending)
}

/// The parent's tool port and a sub-agent's, over one turn's registry,
/// must not mint the same request id (#1496).
///
/// `run_session` builds exactly this pair — `session.rs` constructs one
/// executor for the turn and a second for the sub-agent view, both over
/// the same [`Pending`]. With a bare per-instance counter both start at
/// zero, `register_tool`'s `HashMap::insert` replaces the first entry, and
/// the host's answer for `tool-0` resolves whichever registered last while
/// the displaced waiter wakes to a dropped sender. The assertion that
/// fails first on the old code is the id inequality; the two that follow
/// are the consequence worth naming — each caller gets *its own* answer,
/// not the other's.
#[tokio::test]
async fn two_tool_ports_sharing_a_turn_do_not_collide_on_request_ids() {
    let (sink, mut rx, pending) = live_channel();
    let parent = RemoteToolExecutor::new(
        Vec::new(),
        std::sync::Arc::new(stella_core::ports::NoAuthz),
        stella_core::ports::Principal::Host("test".to_string()),
        sink.clone(),
        pending.clone(),
        Duration::from_secs(5),
        None,
    );
    let child = RemoteToolExecutor::new(
        Vec::new(),
        std::sync::Arc::new(stella_core::ports::NoAuthz),
        stella_core::ports::Principal::Host("test".to_string()),
        sink,
        pending.clone(),
        Duration::from_secs(5),
        None,
    );

    let parent_call =
        tokio::spawn(async move { parent.execute("echo", &serde_json::json!({})).await });
    let ServerFrame::ToolRequest {
        request_id: first, ..
    } = rx.recv().await.unwrap()
    else {
        panic!("expected the parent's tool request frame");
    };
    let child_call =
        tokio::spawn(async move { child.execute("echo", &serde_json::json!({})).await });
    let ServerFrame::ToolRequest {
        request_id: second, ..
    } = rx.recv().await.unwrap()
    else {
        panic!("expected the sub-agent's tool request frame");
    };

    assert_ne!(
        first, second,
        "two executors over one Pending minted the same request id, so one \
         registration silently replaced the other"
    );

    // Answer each by its own id, distinguishably.
    pending
        .resolve_tool(
            &first,
            ToolOutput::Ok {
                content: "for-the-parent".to_string(),
                data: None,
            },
        )
        .expect("the parent's request is still registered");
    pending
        .resolve_tool(
            &second,
            ToolOutput::Ok {
                content: "for-the-sub-agent".to_string(),
                data: None,
            },
        )
        .expect("the sub-agent's request is still registered");

    assert!(
        matches!(parent_call.await.unwrap(), ToolOutput::Ok { content , .. } if content == "for-the-parent"),
        "the parent must receive the answer addressed to the parent"
    );
    assert!(
        matches!(child_call.await.unwrap(), ToolOutput::Ok { content , .. } if content == "for-the-sub-agent"),
        "the sub-agent must receive the answer addressed to the sub-agent"
    );
}

/// **The #3286 serve witness**: a denying gate refuses a remoted call
/// *before its request frame leaves* — the host is never asked — at the
/// same seam position as the CLI's `GatedToolSet`. Its parity twin on
/// the CLI surface is `stella-cli`'s
/// `a_denying_gate_blocks_a_call_the_default_session_stack_allows`;
/// `stella-parity` names both.
#[tokio::test]
async fn a_denying_gate_refuses_a_remoted_call_before_any_frame_leaves() {
    struct DenyAll;
    impl stella_core::ports::AuthzGate for DenyAll {
        fn name(&self) -> &'static str {
            "deny-all"
        }
        fn check(
            &self,
            contract: &stella_protocol::ToolContract,
            _principal: &stella_core::ports::Principal,
            _input: &serde_json::Value,
        ) -> Result<stella_core::ports::AuthzDecision, stella_core::ports::AuthzEvalError> {
            Ok(stella_core::ports::AuthzDecision::Deny {
                reason: format!("`{}` is not permitted here", contract.name()),
            })
        }
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let sink = crate::backlog::FrameSink::new(
        tx,
        crate::backlog::FrameBacklog::new(crate::backlog::DEFAULT_MAX_QUEUED_FRAMES),
    );
    let pending = Pending::new(
        crate::observe::null_observer(),
        TurnRef::new("turn-authztest0"),
    );
    let contract = stella_protocol::ToolContract::declared(stella_protocol::ToolSchema {
        name: "deploy".to_string(),
        description: "d".to_string(),
        input_schema: serde_json::json!({ "type": "object" }),
        read_only: false,
        speculation_safe: false,
    });
    // A bus recording `policy.evaluated`: the served surface must journal
    // the same rule-by-rule account the CLI's `GatedToolSet` does (#3362).
    let bus = HookBus::new("turn-authztest0");
    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
    let journal = Arc::clone(&seen);
    bus.on(hook_names::POLICY_EVALUATED, move |event: &HookEvent| {
        journal
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(event.payload.clone());
        Ok(())
    })
    .detach();

    let port = RemoteToolExecutor::new(
        vec![contract],
        Arc::new(DenyAll),
        stella_core::ports::Principal::Host("acme".to_string()),
        sink,
        pending,
        Duration::from_millis(50),
        Some(bus),
    );

    let out = port.execute("deploy", &serde_json::json!({})).await;
    match out {
        ToolOutput::Error { message, class } => {
            assert_eq!(class, Some(stella_protocol::ErrorClass::RefusedByPolicy));
            assert!(message.contains("deploy"), "names the tool: {message}");
        }
        other => panic!("the gate must refuse: {other:?}"),
    }
    assert!(
        rx.try_recv().is_err(),
        "a denied call must never reach the host — no frame may exist"
    );

    // **The #3362 witness**: the account reaches the policy plane, carrying
    // the trace inside `decision` — the field `bridge_policy_plane` copies
    // verbatim into the journaled `PolicyDecision`. Byte-parallel with the
    // CLI surface's, because both come from `evaluation_journal_payload`.
    let events = seen.lock().unwrap_or_else(|p| p.into_inner());
    assert_eq!(events.len(), 1, "one account per evaluation: {events:?}");
    let account = &events[0];
    assert_eq!(account["event_name"], "authz.gate");
    assert_eq!(account["tool"], "deploy");
    assert_eq!(account["principal"], "host:acme");
    assert_eq!(account["gate"], "deny-all");
    assert_eq!(account["decision"]["verdict"], "deny");
    let rules = account["decision"]["trace"]["rules"]
        .as_array()
        .expect("the trace rides the decision payload");
    assert_eq!(rules.len(), 1, "the default single-rule trace: {rules:?}");
    assert_eq!(rules[0]["rule"], "deny-all");
    assert_eq!(rules[0]["deciding"], true);
    assert!(
        account["input"].is_null() && account["decision"]["input"].is_null(),
        "the call's arguments must never ride an audit payload (invariant #3)"
    );
}

/// A `Medium` risk ceiling refuses a host-declared (untrusted, `High`)
/// tool from contract metadata alone — the side-table the wire change
/// exists to retire.
#[tokio::test]
async fn a_risk_ceiling_authorizes_from_wire_contract_metadata() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let sink = crate::backlog::FrameSink::new(
        tx,
        crate::backlog::FrameBacklog::new(crate::backlog::DEFAULT_MAX_QUEUED_FRAMES),
    );
    let pending = Pending::new(
        crate::observe::null_observer(),
        TurnRef::new("turn-authztest1"),
    );
    let contract = stella_protocol::ToolContract::declared(stella_protocol::ToolSchema {
        name: "mcp__vendor__wipe".to_string(),
        description: "d".to_string(),
        input_schema: serde_json::json!({ "type": "object" }),
        read_only: false,
        speculation_safe: false,
    });
    let port = RemoteToolExecutor::new(
        vec![contract],
        Arc::new(stella_core::ports::RiskCeiling::new(
            stella_protocol::RiskLevel::Medium,
        )),
        stella_core::ports::Principal::Host("acme".to_string()),
        sink,
        pending,
        Duration::from_millis(50),
        None,
    );

    let out = port
        .execute("mcp__vendor__wipe", &serde_json::json!({}))
        .await;
    assert!(out.is_error(), "a Medium ceiling must refuse High: {out:?}");
    assert!(rx.try_recv().is_err(), "and the host is never asked");
}

/// A tool port whose deployment installed one blocking policy: `refused` is
/// denied, everything else allowed. The frame sink is dead, as in
/// [`disconnected_port`] — the gate is asked before any frame is built, so a
/// live host is not what these assertions are about.
fn policed_port(refused: &'static str) -> RemoteToolExecutor {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    drop(rx);
    let sink = crate::backlog::FrameSink::new(
        tx,
        crate::backlog::FrameBacklog::new(crate::backlog::DEFAULT_MAX_QUEUED_FRAMES),
    );
    let pending = Pending::new(
        crate::observe::null_observer(),
        TurnRef::new("turn-policedtest"),
    );
    let bus = HookBus::new("turn-policedtest");
    bus.on_blocking(hook_names::TOOL_CALL_REQUESTED, move |event| {
        if event.payload["tool"].as_str() == Some(refused) {
            return stella_core::bus::HookDecision::Deny(
                format!("`{refused}` is not permitted on this deployment").into(),
            );
        }
        stella_core::bus::HookDecision::Allow
    })
    .detach();
    RemoteToolExecutor::new(
        Vec::new(),
        std::sync::Arc::new(stella_core::ports::NoAuthz),
        stella_core::ports::Principal::Host("test".to_string()),
        sink,
        pending,
        Duration::from_millis(50),
        Some(bus),
    )
}

/// **Witness (#3843).** The served surface's blocking policy chain is
/// reachable through the [`stella_core::ports::DispatchGate`] port, which is
/// what lets a decorator that dispatches a name of its own — serve has one,
/// `DelegatingTools` and its engine-side `delegate` — run the same chain a
/// remoted call passes through. Before this change `RemoteToolExecutor` kept
/// the trait's `None` default and there was no way to ask it.
#[tokio::test]
async fn the_remote_executor_offers_its_policy_chain_as_a_dispatch_gate() {
    let port = policed_port("delegate");
    let gate = port
        .dispatch_gate()
        .expect("the executor that owns the chain is the surface's dispatch gate");

    let refusal = gate
        .admit(
            "delegate",
            &serde_json::json!({ "prompt": "read the code" }),
        )
        .await;
    let stella_core::ports::DispatchAdmission::Refuse(output) = refusal else {
        panic!("a denied name must be refused, not admitted: {refusal:?}");
    };
    // Classified, not a bare error string: the CLI's `ToolRegistry` answers
    // one `Deny` with `RefusedByPolicy`, and this file had drifted to
    // `ToolOutput::error` on both refusal arms.
    assert!(
        matches!(
            &output,
            ToolOutput::Error {
                class: Some(stella_protocol::ErrorClass::RefusedByPolicy),
                ..
            }
        ),
        "a policy refusal must carry its class: {output:?}"
    );

    assert!(
        matches!(
            gate.admit("echo", &serde_json::json!({ "text": "hi" }))
                .await,
            stella_core::ports::DispatchAdmission::Admit
        ),
        "a name the policy allows is admitted unchanged"
    );
}
