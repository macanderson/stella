//! The pluggable authorization seam (#2716): one port, asked once per tool
//! call, that an operator or an embedding product implements to govern every
//! tool in the session without patching a single tool.
//!
//! # Why a port and not a policy
//!
//! `stella-tools::policy::ToolPolicy` already answers "is this tool switched
//! on in this session?" — a session-global fact read from `settings.json`.
//! What it cannot express is *who is asking*. A sub-agent, a pipeline role, a
//! human at the keyboard, and an opaque principal handed over the serve wire
//! by an embedding host all reach the same dispatch through the same
//! executor, and today they arrive indistinguishable. Any RBAC system worth
//! the name needs that distinction, and it must be able to supply its own
//! rules rather than convince us to hardcode them — invariant #1, ports not
//! concretions.
//!
//! So: the engine defines [`Principal`] (who), consumes
//! [`stella_protocol::ToolContract`] (what, and how dangerous), and delegates
//! the verdict to whoever implements [`AuthzGate`]. Nothing in `stella-core`
//! interprets a host-supplied principal — reading meaning into
//! [`Principal::Host`] is the plugin's job, not ours.
//!
//! # Fail closed, structurally
//!
//! [`AuthzGate::check`] returns `Result<AuthzDecision, AuthzEvalError>`, and
//! the two arms are not interchangeable. `Ok(Deny)` is a *decision* — a rule
//! matched and refused, and an operator running with enforcement softened may
//! legitimately downgrade it to a warning. `Err` is the absence of a
//! decision: the policy store was unreachable, the resolver panicked, a rule
//! failed to compile. That is never softenable, and [`authz_verdict`] is
//! where the difference is enforced rather than remembered — it hands the
//! `Err` to [`resolve_precedence`], whose signature already proves no value
//! of the softening flag can rescue an errored evaluation.
//!
//! This is oxagen-platform's OXA-2056 defect encoded at the type level: there,
//! an authorization check that *failed to evaluate* was treated as one that
//! passed, and the leniency flag applied to both. A gate that cannot decide
//! must refuse.
//!
//! # One ladder, not a second one
//!
//! The vocabulary here is deliberately [`crate::bus::HookDecision`] minus
//! `Modify` — a gate authorizes, it does not rewrite arguments — and
//! [`authz_verdict`] folds through the same [`resolve_precedence`] the bus
//! chains and shell hooks already fold through. Three producers, one
//! precedence order: **operator deny > any ask > any allow**. A second ladder
//! would be a second place for the order to be wrong.

use serde_json::Value;
use stella_protocol::{RiskLevel, ToolContract};

use crate::bus::HookDecision;
use crate::hooks::decision::{GateVerdict, OperatorPosture, resolve_precedence};

/// Who is asking for a tool call.
///
/// Engine-side identity, not an authenticated one: nothing here proves a
/// claim, it names the caller so a gate can apply rules to it. Authentication
/// — if the deployment has any — happens in the host that constructs the
/// principal, which is why [`Self::Host`] is opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// The human driving this session directly.
    User,
    /// A pipeline role acting inside a staged run (`triage`, `witness_author`,
    /// …). Carries the role name as the pipeline spells it.
    Role(String),
    /// A sub-agent the loop spawned, identified by its dispatch id.
    SubAgent(String),
    /// An identity supplied by an embedding host over the serve wire.
    ///
    /// Deliberately an opaque string: `stella-core` must not grow an opinion
    /// about a host's identity model (invariant #1). The gate that the host
    /// also supplies is the thing that understands it.
    Host(String),
}

impl Principal {
    /// A short stable label for audit lines and deny reasons — never parsed,
    /// only displayed.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::User => "user".into(),
            Self::Role(role) => format!("role:{role}"),
            Self::SubAgent(id) => format!("subagent:{id}"),
            Self::Host(id) => format!("host:{id}"),
        }
    }
}

/// What a gate decided about one call.
///
/// [`crate::bus::HookDecision`]'s vocabulary minus `Modify`: authorizing a
/// call and rewriting its arguments are different powers, and a gate that
/// could silently edit the input would make the contract's input schema a
/// suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthzDecision {
    /// This principal may make this call.
    Allow,
    /// Refuse without asking anyone.
    Deny {
        /// Why — surfaced to the model and the journal.
        reason: String,
    },
    /// Park and ask a human; the answer decides.
    RequireApproval {
        /// What the human is being asked to permit.
        reason: String,
    },
}

impl From<AuthzDecision> for HookDecision {
    fn from(decision: AuthzDecision) -> Self {
        match decision {
            AuthzDecision::Allow => HookDecision::Allow,
            AuthzDecision::Deny { reason } => HookDecision::Deny { reason },
            AuthzDecision::RequireApproval { reason } => HookDecision::RequireApproval { reason },
        }
    }
}

/// A gate that could not reach a decision.
///
/// Named and typed rather than a bare `String` (invariant #5) because the
/// distinction from `Ok(Deny)` is the entire safety property: callers must
/// not be able to conflate "refused" with "could not tell", and a
/// `Result<_, String>` invites exactly that by making both arms prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthzEvalError {
    /// Which gate failed — its [`AuthzGate::name`], for the audit line.
    pub gate: String,
    /// What went wrong, for a human reading the refusal.
    pub detail: String,
}

impl AuthzEvalError {
    /// Build an evaluation failure.
    #[must_use]
    pub fn new(gate: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            gate: gate.into(),
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for AuthzEvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "gate `{}` could not evaluate: {}",
            self.gate, self.detail
        )
    }
}

impl std::error::Error for AuthzEvalError {}

/// The pluggable authorization port: one question, asked once per call,
/// before the tool runs.
///
/// Implementations must be **pure over their inputs plus data they prefetched**
/// (invariant #2): no I/O here. A gate that needs to consult a policy store
/// loads it when it is constructed, so "why was this denied" stays a value a
/// test can assert on rather than a log line and a network round trip.
pub trait AuthzGate: Send + Sync {
    /// Name this gate, for audit lines and for the "chosen by name" rule
    /// below. No default — a gate that will not say what it is has no
    /// business deciding anything.
    fn name(&self) -> &'static str;

    /// Decide whether `principal` may call `contract` with `input`.
    ///
    /// Return `Err` **only** when no decision could be reached; it is an
    /// unconditional refusal that no enforcement flag can soften.
    fn check(
        &self,
        contract: &ToolContract,
        principal: &Principal,
        input: &Value,
    ) -> Result<AuthzDecision, AuthzEvalError>;
}

/// The explicit "no authorization plane is installed" gate.
///
/// Chosen **by name** at construction, never by a nullable slot that defaults
/// open. That is not a stylistic preference: oxagen-platform carries five
/// module-level gate slots initialized to null, and a bootstrap path that
/// forgot to fill one shipped as a live no-authz surface with nothing in the
/// type system objecting. Here, an engine with no gate does not compile —
/// somebody has to type `NoAuthz`, and that word appears in review.
pub struct NoAuthz;

impl AuthzGate for NoAuthz {
    fn name(&self) -> &'static str {
        "no-authz"
    }

    fn check(
        &self,
        _contract: &ToolContract,
        _principal: &Principal,
        _input: &Value,
    ) -> Result<AuthzDecision, AuthzEvalError> {
        Ok(AuthzDecision::Allow)
    }
}

/// A gate that refuses anything graded above a ceiling — the built-in
/// implementation that makes [`RiskLevel`] do real work.
///
/// This exists to keep risk from becoming decorative. oxagen-platform's docs
/// promise that a high-risk capability requires approval while its code keys
/// only on a separate boolean, so the grade is written down, displayed, and
/// enforced nowhere. Here the grade has exactly one job and does it: a
/// principal's grant is a ceiling, and a contract above it does not run.
///
/// Note what this means for third-party tools without anyone writing a rule
/// about them: [`stella_protocol::ToolContract::declared`] grades every
/// unreviewed tool [`RiskLevel::High`], so a `Medium` ceiling refuses the lot
/// of them, and admitting them is a deliberate act.
pub struct RiskCeiling {
    ceiling: RiskLevel,
}

impl RiskCeiling {
    /// Refuse any contract graded above `ceiling`.
    #[must_use]
    pub fn new(ceiling: RiskLevel) -> Self {
        Self { ceiling }
    }
}

impl AuthzGate for RiskCeiling {
    fn name(&self) -> &'static str {
        "risk-ceiling"
    }

    fn check(
        &self,
        contract: &ToolContract,
        principal: &Principal,
        _input: &Value,
    ) -> Result<AuthzDecision, AuthzEvalError> {
        if contract.risk.within(self.ceiling) {
            return Ok(AuthzDecision::Allow);
        }
        Ok(AuthzDecision::Deny {
            reason: format!(
                "`{}` is graded `{}`, above the `{}` ceiling for {}",
                contract.name(),
                contract.risk.as_str(),
                self.ceiling.as_str(),
                principal.label()
            ),
        })
    }
}

/// Fold an operator posture and a gate evaluation into the verdict dispatch
/// acts on.
///
/// A thin adapter over [`resolve_precedence`] on purpose — the precedence
/// order and the fail-closed rule live there, shared with the bus chains and
/// the shell-hook surface, and this function exists so that the gate joins
/// that ladder instead of growing a parallel one.
#[must_use]
pub fn authz_verdict(
    operator: &OperatorPosture,
    evaluation: Result<AuthzDecision, AuthzEvalError>,
    enforcement_softened: bool,
) -> GateVerdict {
    match evaluation {
        Ok(decision) => {
            let decision: HookDecision = decision.into();
            resolve_precedence(operator, Ok(&decision), enforcement_softened)
        }
        Err(error) => {
            // Deliberately routed through the same `Err` arm the hook surface
            // uses: one fail-closed rule, one implementation.
            let detail = error.to_string();
            resolve_precedence(operator, Err(&detail), enforcement_softened)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::{Provenance, ToolSchema};

    fn contract(name: &str, risk: RiskLevel) -> ToolContract {
        ToolContract::builtin(
            ToolSchema {
                name: name.into(),
                description: "d".into(),
                input_schema: serde_json::json!({}),
                read_only: false,
                speculation_safe: false,
            },
            risk,
        )
    }

    struct BrokenGate;

    impl AuthzGate for BrokenGate {
        fn name(&self) -> &'static str {
            "broken"
        }
        fn check(
            &self,
            _contract: &ToolContract,
            _principal: &Principal,
            _input: &Value,
        ) -> Result<AuthzDecision, AuthzEvalError> {
            Err(AuthzEvalError::new("broken", "policy store unreachable"))
        }
    }

    #[test]
    fn no_authz_allows_everything_including_destructive() {
        let decision = NoAuthz
            .check(
                &contract("delete_file", RiskLevel::Destructive),
                &Principal::User,
                &serde_json::json!({}),
            )
            .unwrap();
        assert_eq!(decision, AuthzDecision::Allow);
    }

    #[test]
    fn a_risk_ceiling_admits_below_and_refuses_above() {
        let gate = RiskCeiling::new(RiskLevel::Medium);
        let input = serde_json::json!({});

        assert_eq!(
            gate.check(
                &contract("write_file", RiskLevel::Medium),
                &Principal::User,
                &input
            )
            .unwrap(),
            AuthzDecision::Allow
        );

        match gate
            .check(&contract("bash", RiskLevel::High), &Principal::User, &input)
            .unwrap()
        {
            AuthzDecision::Deny { reason } => {
                assert!(reason.contains("bash"), "names the tool: {reason}");
                assert!(reason.contains("high"), "names the grade: {reason}");
            }
            other => panic!("expected a deny, got {other:?}"),
        }
    }

    /// The trust asymmetry reaching the gate: nobody wrote a rule about this
    /// MCP tool, and a `Medium` ceiling still refuses it, because
    /// `ToolContract::declared` graded it `High` for being unreviewed.
    #[test]
    fn an_unreviewed_tool_is_refused_by_a_medium_ceiling_with_no_rule_about_it() {
        let declared = ToolContract::declared(ToolSchema {
            name: "mcp__vendor__do_thing".into(),
            description: "d".into(),
            input_schema: serde_json::json!({}),
            read_only: true,
            speculation_safe: false,
        });
        assert_eq!(declared.provenance, Provenance::Declared);

        let verdict = RiskCeiling::new(RiskLevel::Medium)
            .check(&declared, &Principal::User, &serde_json::json!({}))
            .unwrap();
        assert!(matches!(verdict, AuthzDecision::Deny { .. }));
    }

    /// **The OXA-2056 witness.** An evaluation *failure* denies even with
    /// enforcement softening turned on — the arm that separates "refused"
    /// from "could not tell".
    #[test]
    fn an_eval_error_denies_even_with_enforcement_softened() {
        let evaluation = BrokenGate.check(
            &contract("bash", RiskLevel::High),
            &Principal::User,
            &serde_json::json!({}),
        );
        assert!(evaluation.is_err());

        for softened in [false, true] {
            match authz_verdict(&OperatorPosture::NoOpinion, evaluation.clone(), softened) {
                GateVerdict::Deny { reason } => assert!(
                    reason.contains("failing closed"),
                    "must name the posture: {reason}"
                ),
                other => panic!("softened={softened} must still deny, got {other:?}"),
            }
        }
    }

    /// The other half: an operator deny outranks a gate that allows.
    #[test]
    fn an_operator_deny_outranks_a_gate_allow() {
        let verdict = authz_verdict(
            &OperatorPosture::Deny {
                reason: "org policy".into(),
            },
            Ok(AuthzDecision::Allow),
            false,
        );
        assert!(matches!(verdict, GateVerdict::Deny { .. }));
    }

    #[test]
    fn require_approval_folds_to_the_ask_verdict() {
        let verdict = authz_verdict(
            &OperatorPosture::NoOpinion,
            Ok(AuthzDecision::RequireApproval {
                reason: "pushes to a shared branch".into(),
            }),
            false,
        );
        match verdict {
            GateVerdict::RequireApproval { reason } => {
                assert_eq!(reason, "pushes to a shared branch");
            }
            other => panic!("expected an ask, got {other:?}"),
        }
    }
}
