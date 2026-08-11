//! The shell-hook → approval-flow bridge (#2684): the production
//! implementation of the engine's
//! [`stella_core::hooks::decision::HookApprovalRoute`] port, over the #2676
//! [`ApprovalBroker`].
//!
//! When a settings-declared `PreToolUse` hook prints
//! `{"action":"require_approval", …}`, the engine parks the dispatch on its
//! attached route. This module is that route: it converts the engine-side
//! request into the broker's [`ApprovalRequest`] and calls
//! [`ApprovalBroker::resolve`](crate::registry::approval::ApprovalBroker::resolve)
//! — so the emit-before-park ordering, the bounded TTL wait, the
//! `approval.requested`/`granted`/`denied`/`expired` audit events, and the
//! headless grant-path refusal are all the broker's own, never a copy. A
//! host wires it once at assembly time, next to
//! [`ToolRegistry::attach_approval_responder`](crate::ToolRegistry::attach_approval_responder):
//!
//! ```ignore
//! let route = BrokerApprovalRoute::for_registry(&registry);
//! let engine = engine.with_hooks(hooks, &runner).with_hook_approval_route(&route);
//! ```
//!
//! The decision fold itself (stdout JSON → [`stella_core::bus::HookDecision`]
//! → `resolve_precedence`) is pure and lives engine-side
//! (`stella_core::hooks::decision`); this crate contributes only the two
//! I/O halves — spawning the hook ([`crate::hook_runner::ShellHookRunner`])
//! and asking the human (here).

use async_trait::async_trait;
use stella_core::bus::HookBus;
use stella_core::hooks::decision::{
    HookApprovalRequest, HookApprovalResolution, HookApprovalRoute,
};

use crate::ToolRegistry;
use crate::registry::approval::{ApprovalBroker, ApprovalOutcome, ApprovalRequest};

/// The `gate` name the bridge stamps on its [`ApprovalRequest`]s — a
/// bridge's own event name per that field's contract, spelled like the
/// settings key so an operator reading an `approval.requested` payload can
/// find the hook that raised it.
pub const PRE_TOOL_USE_GATE: &str = "PreToolUse";

/// [`HookApprovalRoute`] over the session's [`ApprovalBroker`]: every
/// shell-hook `require_approval` runs the same emit → park → TTL flow as
/// the registry's own gates, on the same bus.
pub struct BrokerApprovalRoute {
    broker: ApprovalBroker,
    bus: Option<HookBus>,
}

impl BrokerApprovalRoute {
    /// A route over an explicit broker + bus — the seam tests use, and
    /// what a host without a full registry composes by hand.
    pub fn new(broker: ApprovalBroker, bus: Option<HookBus>) -> Self {
        Self { broker, bus }
    }

    /// The session's route: the registry's broker (interactive if
    /// [`ToolRegistry::attach_approval_responder`](crate::ToolRegistry::attach_approval_responder)
    /// ran, headless otherwise) and its attached bus. Snapshots both at
    /// construction — build it after the responder and bus are attached,
    /// which is the same assembly-time ordering the registry itself needs.
    pub fn for_registry(registry: &ToolRegistry) -> Self {
        Self::new(registry.approval_broker(), registry.hook_bus())
    }

    fn approval_request(request: &HookApprovalRequest) -> ApprovalRequest {
        ApprovalRequest {
            tool: request.tool.clone(),
            read_only: request.read_only,
            reason: request.reason.clone(),
            gate: PRE_TOOL_USE_GATE.to_string(),
            subject: None,
        }
    }
}

#[async_trait]
impl HookApprovalRoute for BrokerApprovalRoute {
    async fn resolve(&self, request: &HookApprovalRequest) -> HookApprovalResolution {
        let outcome = self
            .broker
            .resolve(self.bus.as_ref(), &Self::approval_request(request))
            .await;
        match outcome {
            ApprovalOutcome::Approved => HookApprovalResolution::Approved,
            ApprovalOutcome::Denied { reason } => HookApprovalResolution::Denied { reason },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::Value;
    use stella_core::bus::names as hook_names;
    use stella_core::hooks::decision::{OperatorPosture, run_decision_hooks};
    use stella_core::hooks::{HookAction, HookMatcher, HookPayload, Hooks};

    use super::*;
    use crate::registry::approval::{ApprovalResponder, ApprovalResponse};

    struct Scripted {
        answer: ApprovalResponse,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ApprovalResponder for Scripted {
        async fn respond(&self, _request: &ApprovalRequest) -> ApprovalResponse {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.answer.clone()
        }
    }

    fn collect_approval_events(bus: &HookBus) -> Arc<Mutex<Vec<(String, Value)>>> {
        let seen: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let sink = seen.clone();
        bus.on("approval.*", move |event| {
            sink.lock()
                .unwrap()
                .push((event.name.clone(), event.payload.clone()));
            Ok(())
        })
        .detach();
        seen
    }

    fn hook_request() -> HookApprovalRequest {
        HookApprovalRequest {
            tool: "bash".into(),
            read_only: false,
            reason: "hook wants a human".into(),
        }
    }

    /// **Witness (b), #2684 — bridge half.** A shell hook's
    /// `require_approval` resolves through the #2676 flow: the responder is
    /// consulted, and the broker's `approval.requested` → `approval.granted`
    /// audit trail lands on the bus with the request's tool, `read_only`
    /// bit, and reason.
    #[tokio::test]
    async fn a_hook_approval_resolves_through_the_broker_with_the_audit_trail() {
        let bus = HookBus::new("hook-bridge-test");
        let events = collect_approval_events(&bus);
        let responder = Arc::new(Scripted {
            answer: ApprovalResponse::Approve,
            calls: AtomicUsize::new(0),
        });
        let route = BrokerApprovalRoute::new(
            ApprovalBroker::interactive(responder.clone(), Duration::from_secs(5)),
            Some(bus),
        );

        let resolution = route.resolve(&hook_request()).await;
        assert_eq!(resolution, HookApprovalResolution::Approved);
        assert_eq!(
            responder.calls.load(Ordering::SeqCst),
            1,
            "the human was asked through the ApprovalResponder port"
        );
        let events = events.lock().unwrap();
        let names: Vec<&str> = events.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&hook_names::APPROVAL_REQUESTED),
            "emit-before-park: the request is audited: {names:?}"
        );
        assert!(
            names.contains(&hook_names::APPROVAL_GRANTED),
            "the grant is audited: {names:?}"
        );
        let requested = events
            .iter()
            .find(|(n, p)| n == hook_names::APPROVAL_REQUESTED && p.get("tool").is_some())
            .expect("the rich approval.requested emission");
        assert_eq!(requested.1["tool"], "bash");
        assert_eq!(requested.1["read_only"], false);
        assert_eq!(requested.1["reason"], "hook wants a human");
        assert_eq!(
            requested.1["event_name"], PRE_TOOL_USE_GATE,
            "the gate names the shell-hook surface"
        );
    }

    /// A refusing human's words come back through the bridge, and the
    /// denial is audited.
    #[tokio::test]
    async fn a_denying_responder_reaches_the_bridge_with_its_reason() {
        let bus = HookBus::new("hook-bridge-test");
        let events = collect_approval_events(&bus);
        let route = BrokerApprovalRoute::new(
            ApprovalBroker::interactive(
                Arc::new(Scripted {
                    answer: ApprovalResponse::Deny {
                        reason: "not on my watch".into(),
                    },
                    calls: AtomicUsize::new(0),
                }),
                Duration::from_secs(5),
            ),
            Some(bus),
        );
        match route.resolve(&hook_request()).await {
            HookApprovalResolution::Denied { reason } => {
                assert!(reason.contains("not on my watch"), "{reason}");
            }
            other => panic!("a refused hook approval must deny: {other:?}"),
        }
        let names: Vec<String> = events
            .lock()
            .unwrap()
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        assert!(
            names.contains(&hook_names::APPROVAL_DENIED.to_string()),
            "the denial is audited: {names:?}"
        );
    }

    /// Headless (no responder attached): the bridge relays the broker's
    /// grant-path refusal instead of hanging or allowing.
    #[tokio::test]
    async fn a_headless_broker_refuses_through_the_bridge_naming_the_grant_path() {
        let route = BrokerApprovalRoute::new(ApprovalBroker::default(), None);
        match route.resolve(&hook_request()).await {
            HookApprovalResolution::Denied { reason } => {
                assert!(reason.contains("no interactive surface"), "{reason}");
                assert!(reason.contains("tools.bash"), "{reason}");
            }
            other => panic!("headless must refuse with the grant path: {other:?}"),
        }
    }

    /// **Witness (c), #2684 — end to end on the real runner.** A hook that
    /// exits non-zero under the production [`ShellHookRunner`] is an
    /// evaluation failure, and [`resolve_precedence`] denies it for every
    /// value of the enforcement-softening flag (OXA-2056).
    #[tokio::test]
    async fn a_real_hook_exiting_nonzero_denies_even_with_softening_flags() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hooks = Hooks {
            pre_tool_use: Some(vec![HookMatcher {
                matcher: Some("*".into()),
                hooks: vec![HookAction::new("echo policy engine down 1>&2; exit 3")],
            }]),
            ..Hooks::default()
        };
        let payload = HookPayload::pre_tool_use(
            dir.path().display().to_string(),
            "bash",
            serde_json::json!({"cmd": "rm -rf /"}),
            false,
        );
        let run =
            run_decision_hooks(&crate::hook_runner::ShellHookRunner, Some(&hooks), &payload).await;
        assert!(
            run.evaluation.is_err(),
            "non-zero exit fails the evaluation"
        );
        for softened in [false, true] {
            match run.verdict(&OperatorPosture::NoOpinion, softened) {
                stella_core::hooks::decision::GateVerdict::Deny { reason } => {
                    assert!(reason.contains("failing closed"), "{reason}");
                    assert!(reason.contains("policy engine down"), "{reason}");
                }
                other => panic!(
                    "softened={softened}: a failed hook must deny unconditionally: {other:?}"
                ),
            }
        }
    }

    /// **Witness (f), #2684 — end to end on the real runner.** An operator
    /// deny outranks a real hook's printed `allow`.
    #[tokio::test]
    async fn an_operator_deny_beats_a_real_hooks_printed_allow() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hooks = Hooks {
            pre_tool_use: Some(vec![HookMatcher {
                matcher: Some("*".into()),
                hooks: vec![HookAction::new(r#"echo '{"action":"allow"}'"#)],
            }]),
            ..Hooks::default()
        };
        let payload = HookPayload::pre_tool_use(
            dir.path().display().to_string(),
            "bash",
            serde_json::json!({}),
            false,
        );
        let run =
            run_decision_hooks(&crate::hook_runner::ShellHookRunner, Some(&hooks), &payload).await;
        assert_eq!(
            run.evaluation,
            Ok(stella_core::bus::HookDecision::Allow),
            "the hook genuinely allowed"
        );
        let verdict = run.verdict(
            &OperatorPosture::Deny {
                reason: "org policy".into(),
            },
            false,
        );
        assert!(
            matches!(
                verdict,
                stella_core::hooks::decision::GateVerdict::Deny { .. }
            ),
            "a hook Allow can never override an operator deny: {verdict:?}"
        );
    }

    /// A real hook printing a `modify` decision through the production
    /// runner rewrites the input — the (a) witness's I/O half.
    #[tokio::test]
    async fn a_real_hook_printing_modify_rewrites_the_input() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hooks = Hooks {
            pre_tool_use: Some(vec![HookMatcher {
                matcher: Some("*".into()),
                hooks: vec![HookAction::new(
                    r#"echo '{"action":"modify","payload":{"input":{"cmd":"echo safe"}}}'"#,
                )],
            }]),
            ..Hooks::default()
        };
        let payload = HookPayload::pre_tool_use(
            dir.path().display().to_string(),
            "bash",
            serde_json::json!({"cmd": "rm -rf /"}),
            false,
        );
        let run =
            run_decision_hooks(&crate::hook_runner::ShellHookRunner, Some(&hooks), &payload).await;
        assert_eq!(
            run.rewritten_input,
            Some(serde_json::json!({"cmd": "echo safe"})),
            "{run:?}"
        );
    }
}
