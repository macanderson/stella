//! The policy plane, bridged onto the `AgentEvent` stream.
//!
//! Split out of `bus.rs`, which is a god file closed to growth.

use std::sync::Mutex;

use stella_protocol::AgentEvent;

use super::{HookBus, HookSubscription, names};

/// Bridge the policy plane onto an `AgentEvent` stream (receipts spec §6.4).
///
/// Subscribes an observer that maps the four audit event names —
/// `policy.evaluated`, `policy.blocked`, `approval.requested`,
/// `secret.detected` — onto [`AgentEvent::PolicyDecision`], content-free.
/// Every other event name passes through untouched. The bus itself never
/// writes, which is its module contract. What makes the plane durable is
/// whatever journal the host hangs off `events`.
///
/// `subject` is the gated chain's event name (`tool.call.requested`,
/// `file.updated`, …). For a secret detection it is the workspace-relative
/// path. `outcome` is the decision record's compact JSON (`decision` plus
/// `handlers_consulted`), or the detector's kind list. Neither one ever
/// carries file contents or a secret value.
///
/// # One ask, one row
///
/// `approval.requested` reaches the bus twice for one question.
/// `emit_blocking` stamps it when a chain ends in `RequireApproval`. The
/// approval flow emits a richer one of its own before it parks. Both map to a
/// `PolicyDecision`, so a journal held two rows per ask and a count of asks
/// read double.
///
/// Neither emission may simply be dropped. A chain can require approval with
/// no flow behind it. A flow can park for a gate no chain evaluated. So each
/// one is the only signal on some path.
///
/// The bridge holds one ask open per gate instead. The first
/// `approval.requested` for a gate emits a row. A second for that same gate is
/// the pair's other half, and emits nothing. A resolution —
/// `approval.granted`, `.denied`, `.expired` — closes the ask, so the next
/// question about that gate is a new row.
pub fn bridge_policy_plane(
    bus: &HookBus,
    events: crate::event_sender::EventSender,
) -> HookSubscription {
    use stella_protocol::PolicyKind;
    // The gate of the ask now open, if one is. A `Mutex` because the bus
    // holds an observer as `Fn` + `Sync`. Two threads can gate a call at once,
    // and the pair this collapses must not be split across them.
    let open_ask: Mutex<Option<String>> = Mutex::new(None);
    bus.on("*", move |event| {
        if matches!(
            event.name.as_str(),
            names::APPROVAL_GRANTED | names::APPROVAL_DENIED | names::APPROVAL_EXPIRED
        ) {
            *open_ask.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return Ok(());
        }
        let kind = match event.name.as_str() {
            names::POLICY_EVALUATED => PolicyKind::Evaluated,
            names::POLICY_BLOCKED => PolicyKind::Blocked,
            names::APPROVAL_REQUESTED => PolicyKind::ApprovalRequested,
            names::SECRET_DETECTED => PolicyKind::SecretDetected,
            _ => return Ok(()),
        };
        let (subject, outcome) = if kind == PolicyKind::SecretDetected {
            (
                event.payload["path"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                event.payload["kinds"].to_string(),
            )
        } else {
            (
                event.payload["event_name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                serde_json::json!({
                    "decision": event.payload["decision"],
                    "handlers_consulted": event.payload["handlers_consulted"],
                })
                .to_string(),
            )
        };
        if kind == PolicyKind::ApprovalRequested {
            let mut open = open_ask.lock().unwrap_or_else(|p| p.into_inner());
            if open.as_deref() == Some(subject.as_str()) {
                return Ok(());
            }
            *open = Some(subject.clone());
        }
        events
            .send(AgentEvent::PolicyDecision {
                kind,
                subject,
                outcome,
            })
            .map_err(|e| e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use stella_protocol::PolicyKind;

    use super::super::{HookDecision, HookEventDraft};
    use super::*;

    fn kinds(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) -> Vec<PolicyKind> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .map(|event| match event {
                AgentEvent::PolicyDecision { kind, .. } => kind,
                other => panic!("only PolicyDecision crosses the bridge: {other:?}"),
            })
            .collect()
    }

    /// **Witness.** One question, one row.
    ///
    /// The chain's stamp and the flow's richer emission both name the same
    /// gate. Before they were paired, a journal held two `ApprovalRequested`
    /// rows per ask, and a count of asks read double.
    #[test]
    fn one_ask_is_one_row_however_many_emissions_it_makes() {
        let bus = HookBus::new("approval-pair");
        bus.on_blocking(names::TOOL_CALL_REQUESTED, |_| {
            HookDecision::RequireApproval {
                reason: "destructive".into(),
            }
        })
        .detach();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let _bridge = bridge_policy_plane(&bus, crate::event_sender::EventSender::new(tx));

        // The chain's own stamp.
        bus.emit_blocking(HookEventDraft::new(
            names::TOOL_CALL_REQUESTED,
            serde_json::json!({ "tool": "bash" }),
        ));
        // The flow's richer emission, before it parks.
        bus.emit_named(
            names::APPROVAL_REQUESTED,
            serde_json::json!({
                "event_name": names::TOOL_CALL_REQUESTED,
                "tool": "bash",
                "read_only": false,
            }),
        );

        assert_eq!(
            kinds(&mut rx),
            vec![PolicyKind::Evaluated, PolicyKind::ApprovalRequested],
            "the pair is one row, and the chain's evaluation still gets its own"
        );
    }

    /// A resolution closes the ask. The next question about the same gate is
    /// a new row, not the other half of the last one.
    #[test]
    fn a_resolved_ask_lets_the_next_one_through() {
        let bus = HookBus::new("approval-reopen");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let _bridge = bridge_policy_plane(&bus, crate::event_sender::EventSender::new(tx));

        let ask = || {
            serde_json::json!({
                "event_name": names::TOOL_CALL_REQUESTED,
                "tool": "bash",
            })
        };
        bus.emit_named(names::APPROVAL_REQUESTED, ask());
        bus.emit_named(
            names::APPROVAL_GRANTED,
            serde_json::json!({ "event_name": names::TOOL_CALL_REQUESTED }),
        );
        bus.emit_named(names::APPROVAL_REQUESTED, ask());

        assert_eq!(
            kinds(&mut rx),
            vec![PolicyKind::ApprovalRequested, PolicyKind::ApprovalRequested],
            "a granted ask is finished, so the next one is a new question"
        );
    }

    /// Two gates asked about at once are two asks. Keying the pair on the gate
    /// is what keeps them apart.
    #[test]
    fn two_gates_are_two_asks() {
        let bus = HookBus::new("approval-two-gates");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let _bridge = bridge_policy_plane(&bus, crate::event_sender::EventSender::new(tx));

        for gate in [names::TOOL_CALL_REQUESTED, names::FILE_UPDATED] {
            bus.emit_named(
                names::APPROVAL_REQUESTED,
                serde_json::json!({ "event_name": gate }),
            );
        }

        assert_eq!(
            kinds(&mut rx),
            vec![PolicyKind::ApprovalRequested, PolicyKind::ApprovalRequested]
        );
    }
}
