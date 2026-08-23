//! The served surface's one authorization check.
//!
//! [`AuthzGate`] answers "may this principal call this tool at all" from the
//! call's resolved contract, and it has to be answered on every path that
//! dispatches a tool — not on every path that sends a frame. Those two were
//! the same set until the engine grew a tool of its own: `delegate` is
//! implemented by [`crate::subagents::DelegatingTools`] and never reaches
//! [`crate::remote::RemoteToolExecutor`]'s `execute`, so the `High` contract
//! that wrapper advertises for it was declared and then never evaluated
//! (#4464).
//! That is #3843's shape on the authorization plane rather than the extension
//! plane, and the CLI had no equivalent hole: there `delegate` is a registry
//! tool, so `GatedToolSet` sees it like any other.
//!
//! So the fold lives here, called by both, rather than in the remoted executor
//! and copied. Two surfaces answering one `Deny` differently is the drift
//! `stella-parity` exists to catch; two *dispatch paths on one surface* doing
//! it is the same defect with a smaller blast radius and no matrix row to
//! notice it.

use serde_json::Value;
use stella_core::bus::{HookBus, names as hook_names};
use stella_core::hooks::decision::{GateVerdict, OperatorPosture};
use stella_core::ports::authz::authz_verdict;
use stella_core::ports::{AuthzGate, Principal};
use stella_protocol::{ToolContract, ToolOutput};

/// Evaluate `gate` for `principal` calling `contract`'s tool with `input`.
///
/// `Ok(())` admits the call. `Err(output)` is the refusal the model sees, and
/// the caller must dispatch nothing: a denied call costs the host nothing, no
/// frame is built, and no child is spawned.
///
/// The rule-by-rule account (#3362) is journaled before the fold consumes the
/// evaluation, on the same event name and payload shape the CLI's
/// `GatedToolSet` uses, so a host reading the policy plane sees one vocabulary
/// across both surfaces. A session with no bus journals nothing and the call is
/// unaffected.
pub(crate) fn authorize(
    gate: &dyn AuthzGate,
    principal: &Principal,
    contract: &ToolContract,
    input: &Value,
    bus: Option<&HookBus>,
) -> Result<(), ToolOutput> {
    let name = contract.name();
    let evaluation = gate.check_traced(contract, principal, input);
    if let Some(bus) = bus {
        bus.emit_named(
            hook_names::POLICY_EVALUATED,
            stella_core::ports::authz::evaluation_journal_payload(
                name,
                principal,
                gate.name(),
                &evaluation,
            ),
        );
    }
    let evaluation = evaluation.map(|evaluation| evaluation.decision);
    match authz_verdict(&OperatorPosture::NoOpinion, evaluation, false) {
        GateVerdict::Allow => Ok(()),
        GateVerdict::Deny { reason } => Err(ToolOutput::classified_error(
            stella_protocol::ErrorClass::RefusedByPolicy,
            reason,
        )),
        // A served turn has no human to park on: the structured refusal is the
        // honest answer, exactly as the CLI's headless `ApprovalBroker`
        // refuses. Routing this through a host-side approval exchange is
        // #3288's territory, not silently allowed here.
        GateVerdict::RequireApproval { reason } => Err(ToolOutput::classified_error(
            stella_protocol::ErrorClass::RefusedByPolicy,
            format!("`{name}` requires approval before it can run: {reason}"),
        )),
    }
}
