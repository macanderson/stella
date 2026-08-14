//! The approval flow behind `RequireApproval` gate decisions (#2676), plus
//! the blocking policy chain (`ToolRegistry::gate_tool_call` — private, so
//! cited as prose) that produces them.
//!
//! Before this module existed, a `RequireApproval` from a blocking policy
//! chain dead-ended as a model-visible `ToolOutput::Error` — the human was
//! never asked. It is now a real flow with a fixed shape:
//!
//! 1. **Emit before parking.** `approval.requested` — carrying the tool
//!    name, its advertised `read_only` bit, and the gate reason — goes onto
//!    the bus BEFORE the dispatch parks. The ordering is a hard rule: a
//!    park-first flow leaves the event stream silent exactly while the
//!    human is being asked, so no surface could ever render the approval
//!    card. (The bus's own `emit_blocking` audit stamp also fires under
//!    this name; a card-rendering consumer selects this module's richer
//!    emission by its `tool` payload key.)
//! 2. **Park, do not hang.** The dispatch awaits the responder with a TTL —
//!    the same parked-wait shape as the engine's pause gate
//!    ([`stella_core::ports::TurnGate`], #1471): an await at a safe
//!    boundary, released by external input. The park sits BEFORE the tool
//!    runs, so the engine's "abort at safe boundaries only, never
//!    mid-tool" rule (invariant #6) holds. A TTL expiry resolves to a deny
//!    with reason [`APPROVAL_TIMED_OUT`] — never an indefinite hang — and
//!    emits `approval.expired`.
//! 3. **A human answers through a port.** [`ApprovalResponder`] is injected
//!    at assembly time ([`ToolRegistry::attach_approval_responder`]) so the
//!    flow itself does no terminal I/O; the CLI implements the port over
//!    its own interactive prompt io. With no responder attached
//!    (headless), the refusal names the missing surface and the grant path
//!    (`tools.<name>` policy, or rerunning interactively), so the model's
//!    next move is legible instead of a bare wall.
//! 4. **One approval is one call.** Nothing here caches a grant: a repeat
//!    of the same call re-asks until an operator changes policy. A single
//!    "yes" is never standing authorization.
//!
//! # Precedence and failure posture
//!
//! [`resolve_precedence`] is the one pure function that folds an operator
//! posture and a gate evaluation into a [`GateVerdict`]:
//! **operator deny > gate/hook `RequireApproval` > any allow** — a hook or
//! plugin `Allow` can never override an operator deny — and an *errored*
//! evaluation is an unconditional deny regardless of any
//! enforcement-softening flag (the OXA-2056 shape). The gate here routes
//! every chain outcome through it.
//!
//! # Seams
//!
//! - **#2716 (`AuthzGate`)**: today decisions are produced by the
//!   [`HookBus`] blocking chains and folded by [`resolve_precedence`]. The
//!   planned `AuthzGate` port slots in as another producer feeding the
//!   same function — this module consumes verdicts and owns only what
//!   happens after `RequireApproval`, so swapping the producer does not
//!   touch the flow.
//! - **#2684 (shell-hook bridge)**: [`ApprovalBroker::resolve`] is the
//!   reusable entry point — build an [`ApprovalRequest`], hand over the
//!   bus, and the emit → park → TTL contract is honoured for that caller
//!   too. [`ToolRegistry::approval_broker`] exposes the session's broker,
//!   and `crate::hook_bridge::BrokerApprovalRoute` is the shipped caller:
//!   it implements the engine's `HookApprovalRoute` port over this flow.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use stella_core::bus::{HookBus, HookDecision, HookEventDraft, names as hook_names};
use stella_protocol::tool::ToolOutput;

use super::ToolRegistry;

/// How long a parked dispatch waits for a human answer before the flow
/// resolves to a deny ([`APPROVAL_TIMED_OUT`]). Hosts pass their own TTL to
/// [`ToolRegistry::attach_approval_responder`]; this is the default they
/// reach for absent a better-informed number.
pub const DEFAULT_APPROVAL_TTL: Duration = Duration::from_secs(120);

/// The deny reason of a TTL expiry — the exact string, so callers and tests
/// can match it instead of parsing prose.
pub const APPROVAL_TIMED_OUT: &str = "approval timed out";

/// One approval question, as the responder and the bus both see it. Crosses
/// the crate boundary (the CLI implements [`ApprovalResponder`] over it),
/// so it is serde round-trippable by contract (invariant #4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// The tool whose dispatch is parked.
    pub tool: String,
    /// The tool's advertised `read_only` bit — `false` for a name the
    /// registry does not know, the cautious direction.
    pub read_only: bool,
    /// The gate's reason, verbatim from the `RequireApproval` decision.
    pub reason: String,
    /// The blocking chain that raised the requirement
    /// (`tool.call.requested`, `file.created`/`updated`/`deleted`,
    /// `command.started`, or a bridge's own event name).
    pub gate: String,
    /// The chain's narrower subject when it has one — the workspace path
    /// for a `file.*` gate, the command line for `command.started`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

/// The human's answer, as the responder port returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum ApprovalResponse {
    /// Run this one call.
    Approve,
    /// Refuse it. `reason` carries the human's words when they gave any;
    /// empty means a bare "no".
    Deny { reason: String },
}

/// How an approval question reaches a human — the port a surface implements
/// and injects at assembly time over its own interactive prompt io. The
/// flow calls it at most once per parked dispatch and bounds the
/// await with the broker's TTL, so an implementation may block on real
/// input indefinitely without risking a hang.
#[async_trait]
pub trait ApprovalResponder: Send + Sync {
    /// Present `request` and return the human's decision.
    async fn respond(&self, request: &ApprovalRequest) -> ApprovalResponse;
}

/// The resolution of one approval flow, TTL and headless cases folded in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// A human granted this single call.
    Approved,
    /// The call must not run: a human refused, the TTL expired
    /// ([`APPROVAL_TIMED_OUT`]), or no interactive surface exists to ask.
    Denied { reason: String },
}

// The precedence ladder itself — [`OperatorPosture`], [`GateVerdict`],
// [`resolve_precedence`] — moved DOWN to `stella_core::hooks::decision`
// when #2684 gave it a second feeder below this crate (the engine's
// shell-hook surface); re-exported here so the wave-1 #2676 seam — and
// every caller and property test in this module — is unchanged. One pure
// function, two feeders, zero copies.
pub use stella_core::hooks::decision::{GateVerdict, OperatorPosture, resolve_precedence};

/// The session's approval flow: an optional responder plus the TTL that
/// bounds every park. Headless by default — [`ApprovalBroker::resolve`]
/// then refuses with the grant-path message instead of asking.
#[derive(Clone)]
pub struct ApprovalBroker {
    responder: Option<Arc<dyn ApprovalResponder>>,
    ttl: Duration,
}

impl Default for ApprovalBroker {
    fn default() -> Self {
        Self {
            responder: None,
            ttl: DEFAULT_APPROVAL_TTL,
        }
    }
}

impl ApprovalBroker {
    /// A broker that parks on `responder`, for at most `ttl` per question.
    pub fn interactive(responder: Arc<dyn ApprovalResponder>, ttl: Duration) -> Self {
        Self {
            responder: Some(responder),
            ttl,
        }
    }

    /// Run one approval flow: emit `approval.requested`, park on the
    /// responder with the TTL, emit the resolution
    /// (`approval.granted`/`denied`/`expired`), and return the outcome.
    ///
    /// This is the reusable entry point #2684's shell-hook bridge calls;
    /// the registry's own gates go through it too. The `approval.requested`
    /// emission happens synchronously before the first await — the
    /// emit-before-park ordering is structural, not incidental.
    ///
    /// The grant, when granted, covers exactly the call that asked:
    /// nothing is cached here, so an identical later call runs the whole
    /// flow again.
    pub async fn resolve(
        &self,
        bus: Option<&HookBus>,
        request: &ApprovalRequest,
    ) -> ApprovalOutcome {
        // EMIT BEFORE PARKING — the hard ordering rule (spec item 1).
        if let Some(bus) = bus {
            bus.emit_named(hook_names::APPROVAL_REQUESTED, requested_payload(request));
        }
        let Some(responder) = &self.responder else {
            let reason = headless_refusal(request);
            if let Some(bus) = bus {
                bus.emit_named(
                    hook_names::APPROVAL_DENIED,
                    resolution_payload(request, &reason),
                );
            }
            return ApprovalOutcome::Denied { reason };
        };
        match tokio::time::timeout(self.ttl, responder.respond(request)).await {
            Ok(ApprovalResponse::Approve) => {
                if let Some(bus) = bus {
                    bus.emit_named(
                        hook_names::APPROVAL_GRANTED,
                        resolution_payload(request, "granted by the user"),
                    );
                }
                ApprovalOutcome::Approved
            }
            Ok(ApprovalResponse::Deny { reason }) => {
                let reason = if reason.trim().is_empty() {
                    "denied by the user".to_string()
                } else {
                    format!("denied by the user: {reason}")
                };
                if let Some(bus) = bus {
                    bus.emit_named(
                        hook_names::APPROVAL_DENIED,
                        resolution_payload(request, &reason),
                    );
                }
                ApprovalOutcome::Denied { reason }
            }
            Err(_elapsed) => {
                if let Some(bus) = bus {
                    bus.emit_named(
                        hook_names::APPROVAL_EXPIRED,
                        resolution_payload(request, APPROVAL_TIMED_OUT),
                    );
                }
                ApprovalOutcome::Denied {
                    reason: APPROVAL_TIMED_OUT.to_string(),
                }
            }
        }
    }
}

/// The `approval.requested` payload: the spec-required trio (tool,
/// `read_only`, gate reason) plus the chain and subject. `event_name` and
/// `decision` mirror the bus's audit-stamp keys so
/// `stella_core::bus::bridge_policy_plane`'s subject/outcome extraction
/// reads this richer emission coherently too.
fn requested_payload(request: &ApprovalRequest) -> Value {
    serde_json::json!({
        "event_name": request.gate,
        "tool": request.tool,
        "read_only": request.read_only,
        "reason": request.reason,
        "subject": request.subject,
        "decision": HookDecision::RequireApproval { reason: request.reason.clone() },
    })
}

/// The `approval.granted`/`denied`/`expired` payload: the request's
/// identity plus how it resolved.
fn resolution_payload(request: &ApprovalRequest, resolution: &str) -> Value {
    serde_json::json!({
        "event_name": request.gate,
        "tool": request.tool,
        "read_only": request.read_only,
        "reason": request.reason,
        "subject": request.subject,
        "resolution": resolution,
    })
}

/// The headless refusal: names the missing surface AND the grant path, so
/// the model's next move is legible (spec item 3 of #2676) — where the old
/// dead-end was a bare "requires approval" wall.
fn headless_refusal(request: &ApprovalRequest) -> String {
    format!(
        "no interactive surface is attached to answer it — grant the call via policy \
         (`tools.{tool}` in `.stella/settings.json`) or rerun interactively; gate reason: {reason}",
        tool = request.tool,
        reason = request.reason,
    )
}

impl ToolRegistry {
    /// Attach the session's interactive approval responder — the assembly-
    /// time seam (spec item 3 of #2676). Until it is called the broker is
    /// headless and every `RequireApproval` resolves to the structured
    /// grant-path refusal; calling again replaces the previous responder.
    pub fn attach_approval_responder(&self, responder: Arc<dyn ApprovalResponder>, ttl: Duration) {
        *self.approval.write().unwrap_or_else(|p| p.into_inner()) =
            ApprovalBroker::interactive(responder, ttl);
    }

    /// The session's approval broker (cheap clone — shared responder).
    /// `pub` so a hook bridge (#2684) routes its own `RequireApproval`s
    /// through the same flow the registry's gates use.
    pub fn approval_broker(&self) -> ApprovalBroker {
        self.approval
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// The session's attached hook bus, if any (cheap clone — shared
    /// inner). For the #2684 bridge, whose approval flow emits its
    /// `approval.*` audit events on the same bus the registry's gates use.
    pub(crate) fn hook_bus(&self) -> Option<HookBus> {
        self.bus.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The `read_only` bit `name` advertises — `false` when the registry
    /// does not know the name, which is the cautious direction (an unknown
    /// tool is treated as mutating everywhere else too).
    fn advertised_read_only(&self, name: &str) -> bool {
        self.tools
            .get(name)
            .map(|t| t.schema().read_only)
            .unwrap_or(false)
    }

    /// Park this dispatch on the approval flow. `Ok(())` means a human
    /// granted the single call; `Err` carries the `ToolOutput` the model
    /// sees, `context` naming what was refused (the tool, the tool + path,
    /// or "command").
    async fn seek_approval(
        &self,
        bus: &HookBus,
        request: ApprovalRequest,
        context: &str,
    ) -> Result<(), ToolOutput> {
        match self.approval_broker().resolve(Some(bus), &request).await {
            ApprovalOutcome::Approved => Ok(()),
            ApprovalOutcome::Denied { reason } => Err(ToolOutput::classified_error(
                stella_protocol::ErrorClass::RefusedByPolicy,
                format!("{context} requires approval — {reason}"),
            )),
        }
    }

    /// Run the `tool.call.requested` blocking chain. `Ok(None)`: allowed
    /// unchanged. `Ok(Some(input))`: allowed with a policy-modified input.
    /// `Err(output)`: denied — by policy, by a human, by the approval TTL,
    /// or by the headless refusal — the error the model sees instead of a
    /// tool result. The chain receives the RAW input (the interception
    /// point is privileged by design); observable events carry only
    /// sanitized inputs.
    pub(super) async fn gate_tool_call(
        &self,
        bus: &HookBus,
        name: &str,
        input: &Value,
    ) -> Result<Option<Value>, ToolOutput> {
        let outcome = bus.emit_blocking(HookEventDraft::new(
            hook_names::TOOL_CALL_REQUESTED,
            serde_json::json!({ "tool": name, "input": input }),
        ));
        // The bus has already folded a panicking handler into `Deny` (fail
        // closed), so the evaluation reaching the ladder here is always
        // `Ok`; operator denies act upstream today (`crate::policy`
        // withholds the tool) — see [`OperatorPosture`].
        match resolve_precedence(&OperatorPosture::NoOpinion, Ok(&outcome.decision), false) {
            GateVerdict::Deny { reason } => {
                return Err(ToolOutput::classified_error(
                    stella_protocol::ErrorClass::RefusedByPolicy,
                    format!("`{name}` was denied by an extension policy: {reason}"),
                ));
            }
            GateVerdict::RequireApproval { reason } => {
                let request = ApprovalRequest {
                    tool: name.to_string(),
                    read_only: self.advertised_read_only(name),
                    reason,
                    gate: hook_names::TOOL_CALL_REQUESTED.to_string(),
                    subject: None,
                };
                self.seek_approval(bus, request, &format!("`{name}`"))
                    .await?;
            }
            GateVerdict::Allow => {}
        }
        if !outcome.modified {
            return Ok(None);
        }
        match outcome.event.payload.get("input") {
            Some(new_input) => Ok(Some(new_input.clone())),
            None => {
                // A `modify` that dropped the `input` field is a broken
                // policy handler — surface it, keep the original input
                // rather than executing garbage.
                bus.emit_named(
                    hook_names::EXTENSION_ERROR,
                    serde_json::json!({
                        "failed_event": hook_names::TOOL_CALL_REQUESTED,
                        "error": "modify decision dropped the `input` field; original input kept",
                    }),
                );
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use proptest::prelude::*;

    use super::*;

    // ---- fixtures ------------------------------------------------------

    /// A registry in a fresh tempdir with a bus whose blocking handler
    /// answers `RequireApproval` on `gate_event`. The tempdir is returned
    /// so the root outlives the registry.
    fn fixture(gate_event: &str, reason: &str) -> (tempfile::TempDir, ToolRegistry, HookBus) {
        let dir = tempfile::tempdir().unwrap();
        let reg = ToolRegistry::new(dir.path().to_path_buf());
        let bus = HookBus::new("approval-test");
        let reason = reason.to_string();
        bus.on_blocking(gate_event, move |_| HookDecision::RequireApproval {
            reason: reason.clone(),
        })
        .detach();
        reg.attach_bus(bus.clone());
        (dir, reg, bus)
    }

    /// Collect every `approval.*` event name + payload the bus emits.
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

    /// Responder scripted with one fixed answer; counts its invocations.
    struct Scripted {
        answer: ApprovalResponse,
        calls: AtomicUsize,
    }

    impl Scripted {
        fn approving() -> Arc<Self> {
            Arc::new(Self {
                answer: ApprovalResponse::Approve,
                calls: AtomicUsize::new(0),
            })
        }

        fn denying(reason: &str) -> Arc<Self> {
            Arc::new(Self {
                answer: ApprovalResponse::Deny {
                    reason: reason.to_string(),
                },
                calls: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl ApprovalResponder for Scripted {
        async fn respond(&self, _request: &ApprovalRequest) -> ApprovalResponse {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.answer.clone()
        }
    }

    /// A responder that never answers — the park must be ended by the TTL,
    /// never by this future resolving.
    struct NeverAnswers;

    #[async_trait]
    impl ApprovalResponder for NeverAnswers {
        async fn respond(&self, _request: &ApprovalRequest) -> ApprovalResponse {
            std::future::pending().await
        }
    }

    // ---- witness (a): a human decision decides the parked call ---------

    #[tokio::test]
    async fn approving_responder_lets_the_parked_call_proceed() {
        let (_dir, reg, bus) = fixture(hook_names::TOOL_CALL_REQUESTED, "policy wants a human");
        let events = collect_approval_events(&bus);
        let responder = Scripted::approving();
        reg.attach_approval_responder(responder.clone(), Duration::from_secs(5));

        let out = reg.execute("task_list", &serde_json::json!({})).await;
        assert!(!out.is_error(), "approved call must run: {out:?}");
        assert_eq!(responder.calls.load(Ordering::SeqCst), 1);
        let names: Vec<String> = events
            .lock()
            .unwrap()
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        assert!(
            names.contains(&hook_names::APPROVAL_GRANTED.to_string()),
            "grant is audited: {names:?}"
        );
    }

    #[tokio::test]
    async fn denying_responder_fails_the_call_with_the_humans_reason() {
        let (_dir, reg, bus) = fixture(hook_names::TOOL_CALL_REQUESTED, "policy wants a human");
        let events = collect_approval_events(&bus);
        reg.attach_approval_responder(Scripted::denying("not on my watch"), Duration::from_secs(5));

        match reg.execute("task_list", &serde_json::json!({})).await {
            ToolOutput::Error { message, .. } => {
                assert!(
                    message.contains("not on my watch"),
                    "the human's words reach the model: {message}"
                );
            }
            other => panic!("a denied call must not run: {other:?}"),
        }
        let names: Vec<String> = events
            .lock()
            .unwrap()
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        assert!(
            names.contains(&hook_names::APPROVAL_DENIED.to_string()),
            "denial is audited: {names:?}"
        );
    }

    // ---- witness (b): TTL resolves to deny, never a hang ---------------

    #[tokio::test]
    async fn approval_timeout_resolves_to_deny_not_a_hang() {
        let (_dir, reg, bus) = fixture(hook_names::TOOL_CALL_REQUESTED, "policy wants a human");
        let events = collect_approval_events(&bus);
        reg.attach_approval_responder(Arc::new(NeverAnswers), Duration::from_millis(30));

        match reg.execute("task_list", &serde_json::json!({})).await {
            ToolOutput::Error { message, .. } => {
                assert!(
                    message.contains(APPROVAL_TIMED_OUT),
                    "the timeout is named: {message}"
                );
            }
            other => panic!("a timed-out approval must deny: {other:?}"),
        }
        let names: Vec<String> = events
            .lock()
            .unwrap()
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
        assert!(
            names.contains(&hook_names::APPROVAL_EXPIRED.to_string()),
            "expiry is audited: {names:?}"
        );
    }

    // ---- witness (e): emit-before-park ordering ------------------------

    /// A responder that records, at the moment the park begins, whether the
    /// rich `approval.requested` (the one carrying a `tool` key) was
    /// already observable on the bus.
    struct OrderProbe {
        requested_seen: Arc<AtomicBool>,
        seen_at_park: AtomicBool,
    }

    #[async_trait]
    impl ApprovalResponder for OrderProbe {
        async fn respond(&self, _request: &ApprovalRequest) -> ApprovalResponse {
            self.seen_at_park
                .store(self.requested_seen.load(Ordering::SeqCst), Ordering::SeqCst);
            ApprovalResponse::Approve
        }
    }

    #[tokio::test]
    async fn approval_requested_is_observable_before_the_park_resolves() {
        let (_dir, reg, bus) = fixture(hook_names::TOOL_CALL_REQUESTED, "ordering check");
        let requested_seen = Arc::new(AtomicBool::new(false));
        let payloads: Arc<Mutex<Vec<Value>>> = Arc::default();
        {
            let requested_seen = requested_seen.clone();
            let payloads = payloads.clone();
            bus.on(hook_names::APPROVAL_REQUESTED, move |event| {
                // Key on the rich emission — the bus's own audit stamp
                // shares the name but carries no `tool` field.
                if event.payload.get("tool").is_some() {
                    requested_seen.store(true, Ordering::SeqCst);
                    payloads.lock().unwrap().push(event.payload.clone());
                }
                Ok(())
            })
            .detach();
        }
        let probe = Arc::new(OrderProbe {
            requested_seen,
            seen_at_park: AtomicBool::new(false),
        });
        reg.attach_approval_responder(probe.clone(), Duration::from_secs(5));

        let out = reg.execute("task_list", &serde_json::json!({})).await;
        assert!(!out.is_error(), "{out:?}");
        assert!(
            probe.seen_at_park.load(Ordering::SeqCst),
            "approval.requested must be on the bus BEFORE the park resolves"
        );
        // The spec-required payload trio: tool, read_only, gate reason.
        let payloads = payloads.lock().unwrap();
        let payload = payloads.first().expect("one rich approval.requested");
        assert_eq!(payload["tool"], "task_list");
        assert_eq!(payload["read_only"], true, "task_list advertises read_only");
        assert_eq!(payload["reason"], "ordering check");
    }

    // ---- witness spec item 6: one approval is one call -----------------

    #[tokio::test]
    async fn one_approval_is_scoped_to_the_single_call_repeat_calls_re_ask() {
        let (_dir, reg, _bus) = fixture(hook_names::TOOL_CALL_REQUESTED, "each call re-asks");
        let responder = Scripted::approving();
        reg.attach_approval_responder(responder.clone(), Duration::from_secs(5));

        let first = reg.execute("task_list", &serde_json::json!({})).await;
        let second = reg.execute("task_list", &serde_json::json!({})).await;
        assert!(!first.is_error() && !second.is_error());
        assert_eq!(
            responder.calls.load(Ordering::SeqCst),
            2,
            "a grant is never standing authorization — the second call must re-ask"
        );
    }

    // ---- witness (f): headless refusal names the grant path ------------

    #[tokio::test]
    async fn headless_refusal_names_the_missing_surface_and_the_grant_path() {
        let (_dir, reg, _bus) = fixture(hook_names::TOOL_CALL_REQUESTED, "policy wants a human");
        // No responder attached: the headless default.
        match reg.execute("task_list", &serde_json::json!({})).await {
            ToolOutput::Error { message, .. } => {
                assert!(
                    message.contains("no interactive surface"),
                    "the missing surface is named: {message}"
                );
                assert!(
                    message.contains("tools.task_list"),
                    "the grant path is named: {message}"
                );
                assert!(
                    message.contains("rerun interactively"),
                    "the interactive alternative is named: {message}"
                );
            }
            other => panic!("headless approval must refuse: {other:?}"),
        }
    }

    // ---- witness (c) + (d): the precedence ladder, pure ----------------

    #[test]
    fn precedence_operator_deny_beats_a_hook_approval_requirement() {
        let verdict = resolve_precedence(
            &OperatorPosture::Deny {
                reason: "org policy".into(),
            },
            Ok(&HookDecision::RequireApproval {
                reason: "ask first".into(),
            }),
            false,
        );
        match verdict {
            GateVerdict::Deny { reason } => assert!(reason.contains("org policy")),
            other => panic!("operator deny must win: {other:?}"),
        }
    }

    #[test]
    fn precedence_operator_deny_beats_a_hook_allow() {
        let verdict = resolve_precedence(
            &OperatorPosture::Deny {
                reason: "org policy".into(),
            },
            Ok(&HookDecision::Allow),
            false,
        );
        assert!(
            matches!(verdict, GateVerdict::Deny { .. }),
            "a hook Allow can never override an operator deny: {verdict:?}"
        );
    }

    #[test]
    fn precedence_approval_requirement_beats_any_allow() {
        let ask = HookDecision::RequireApproval {
            reason: "ask first".into(),
        };
        let verdict = resolve_precedence(&OperatorPosture::NoOpinion, Ok(&ask), false);
        assert!(matches!(verdict, GateVerdict::RequireApproval { .. }));
        for decision in [
            HookDecision::Allow,
            HookDecision::Modify {
                payload: serde_json::json!({}),
            },
        ] {
            let verdict = resolve_precedence(&OperatorPosture::NoOpinion, Ok(&decision), false);
            assert_eq!(verdict, GateVerdict::Allow);
        }
    }

    #[test]
    fn precedence_hook_deny_denies_without_asking() {
        let verdict = resolve_precedence(
            &OperatorPosture::NoOpinion,
            Ok(&HookDecision::Deny {
                reason: "blocked path".into(),
            }),
            false,
        );
        assert_eq!(
            verdict,
            GateVerdict::Deny {
                reason: "blocked path".into()
            }
        );
    }

    #[test]
    fn evaluation_failure_denies_even_with_the_softening_flag_set() {
        // The OXA-2056 shape: an errored evaluation is an unconditional
        // deny — no enforcement-softening flag value changes that.
        for softened in [false, true] {
            let verdict = resolve_precedence(
                &OperatorPosture::NoOpinion,
                Err("handler crashed"),
                softened,
            );
            match verdict {
                GateVerdict::Deny { reason } => {
                    assert!(reason.contains("failing closed"), "{reason}");
                }
                other => panic!("an errored evaluation must fail closed: {other:?}"),
            }
        }
    }

    fn hook_decision_strategy() -> impl Strategy<Value = HookDecision> {
        prop_oneof![
            Just(HookDecision::Allow),
            ".*".prop_map(|reason| HookDecision::Deny { reason }),
            ".*".prop_map(|reason| HookDecision::RequireApproval { reason }),
            Just(HookDecision::Modify {
                payload: serde_json::json!({ "input": {} }),
            }),
        ]
    }

    proptest! {
        /// Operator deny wins over EVERY evaluation and EVERY softening
        /// flag — the top of the ladder is unconditional.
        #[test]
        fn operator_deny_beats_every_evaluation(
            reason in ".*",
            decision in hook_decision_strategy(),
            softened in any::<bool>(),
        ) {
            let verdict = resolve_precedence(
                &OperatorPosture::Deny { reason: reason.clone() },
                Ok(&decision),
                softened,
            );
            prop_assert!(
                matches!(verdict, GateVerdict::Deny { .. }),
                "operator deny must win, got {:?}", verdict
            );
        }

        /// An errored evaluation fails closed for every error string and
        /// every softening flag.
        #[test]
        fn evaluation_failure_always_fails_closed(
            error in ".*",
            softened in any::<bool>(),
        ) {
            let verdict = resolve_precedence(
                &OperatorPosture::NoOpinion,
                Err(error.as_str()),
                softened,
            );
            prop_assert!(
                matches!(verdict, GateVerdict::Deny { .. }),
                "an errored evaluation must fail closed, got {:?}", verdict
            );
        }
    }

    // ---- serde round-trips (invariant #4) ------------------------------

    #[test]
    fn approval_request_round_trips_through_serde_json() {
        let request = ApprovalRequest {
            tool: "bash".into(),
            read_only: false,
            reason: "commands need a human".into(),
            gate: "command.started".into(),
            subject: Some("rm -rf build/".into()),
        };
        let json = serde_json::to_string(&request).unwrap();
        let back: ApprovalRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(request, back);

        // `subject: None` round-trips too (the field is skipped on the wire).
        let request = ApprovalRequest {
            subject: None,
            ..request
        };
        let json = serde_json::to_string(&request).unwrap();
        let back: ApprovalRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(request, back);
    }

    #[test]
    fn approval_response_round_trips_through_serde_json() {
        for response in [
            ApprovalResponse::Approve,
            ApprovalResponse::Deny {
                reason: "ask my lead".into(),
            },
        ] {
            let json = serde_json::to_string(&response).unwrap();
            let back: ApprovalResponse = serde_json::from_str(&json).unwrap();
            assert_eq!(response, back);
        }
    }
}
