//! Where authorization actually holds: one decorator over the whole tool
//! stack (#2716).
//!
//! [`stella_core::ports::AuthzGate`] is the seam an operator or an embedding
//! product implements; this is the single place it is *called*. The two are
//! deliberately separate crates — the port is a question the engine asks, and
//! this is the position in the composition where asking it covers everything.
//!
//! # Why a decorator and not registry-internal logic
//!
//! [`crate::registry::ToolRegistry`] only knows about the twelve built-ins.
//! Custom `.stella/tools/*.toml` tools ([`crate::custom::CustomToolSet`]) and
//! every connected MCP server's tools are layered *above* it — and since the
//! surface reduction (#3244) those two are where nearly every capability the
//! agent has now lives. A gate inside the registry would govern the smallest
//! and best-reviewed group while missing all the rest, which is precisely the
//! shape of gap this issue exists to close: the tools most worth governing
//! are the ones a third party supplied.
//!
//! So this composes like [`crate::policy::ToolPolicy`]'s enforcement point
//! does, and belongs **outermost** — above the operator's policy filter.
//! `stella-cli`'s `agent::tool_stack` assembles exactly this order for every
//! session driver (#3283):
//!
//! ```text
//! GatedToolSet        <- authorization: who is asking, and may they?
//!   PolicyToolSet     <- operator switches: is this tool on at all?
//!     CustomToolSet   <- .stella/tools/*.toml
//!       ToolRegistry / McpToolSet
//! ```
//!
//! The two layers answer different questions and neither subsumes the other:
//! a policy switch is one operator's session-wide "off", while a gate asks
//! whether *this* principal may make *this* call. Ordering the gate outermost
//! means an operator's "off" short-circuits before a gate is consulted, which
//! is the cheap direction and the correct precedence — the same one
//! [`stella_core::hooks::decision::resolve_precedence`] encodes.
//!
//! # One-sided on purpose
//!
//! [`crate::policy::ToolPolicy`]'s decorator is two-sided — a disabled tool is
//! both hidden from `schemas()` and refused by `execute()`. This one enforces
//! at `execute()` only, and the asymmetry is deliberate rather than an
//! oversight.
//!
//! An operator switch is input-independent: `bash` is off, so hiding it is
//! exactly as true as refusing it. An authorization decision generally is
//! *not* — a gate may allow `write_file` under `src/` and refuse it under
//! `/etc`, and there is no input to hand it while building the advertised
//! list. Filtering `schemas()` would mean asking every gate a question with a
//! fabricated empty input and treating the answer as final, which would hide
//! tools the gate would have allowed. Enforcement therefore lives where the
//! input exists. Narrowing the advertised set for gates that *are*
//! input-independent (a plain risk ceiling) is a real prompt-budget win and is
//! tracked separately — it is an optimization, not the guarantee.
//!
//! # Fail closed on names it does not know
//!
//! A call for a name this view has no schema for is authorized against an
//! untrusted `High` contract ([`crate::contracts::unknown_contract`]) rather
//! than waved through to the inner stack. Mid-session MCP reconnects, a model
//! inventing a name, and a cached view going stale all land here, and the only
//! defensible answer to "I have never heard of this tool" is to treat it as
//! the least trustworthy thing in the session.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use stella_core::hooks::decision::{GateVerdict, OperatorPosture};
use stella_core::ports::authz::authz_verdict;
use stella_core::ports::{AuthzGate, Principal, ToolExecutor};
use stella_protocol::tool::{ErrorClass, ToolOutput, ToolSchema};

use crate::contracts;

/// The wrapped executor, held either by borrow (a session's per-turn tool
/// chain) or owned via `Arc` (a candidate workspace whose chain is built
/// dynamically and outlives every borrow). Mirrors
/// [`crate::custom::CustomToolSet`]'s shape deliberately — these sit in the
/// same stacks.
enum Inner<'a> {
    Borrowed(&'a dyn ToolExecutor),
    Boxed(Box<dyn ToolExecutor + 'a>),
    Owned(Arc<dyn ToolExecutor>),
}

impl Inner<'_> {
    fn get(&self) -> &dyn ToolExecutor {
        match self {
            Inner::Borrowed(inner) => *inner,
            Inner::Boxed(inner) => inner.as_ref(),
            Inner::Owned(inner) => inner.as_ref(),
        }
    }
}

/// Wraps a tool surface and asks an [`AuthzGate`] before every dispatch.
pub struct GatedToolSet<'a> {
    inner: Inner<'a>,
    gate: Arc<dyn AuthzGate>,
    principal: Principal,
    /// Contracts for everything the inner stack advertised at construction,
    /// resolved once. `execute` is on the hot path of every call and
    /// re-materializing the full schema list per call would re-serialize
    /// every tool's whole JSON parameter document — the same reason
    /// [`stella_core::ports::ReadOnlyTools`] snapshots its name set. A name
    /// missing from this map is not trusted for being missing; see
    /// [`contracts::unknown_contract`].
    contracts: HashMap<String, stella_protocol::ToolContract>,
}

impl<'a> GatedToolSet<'a> {
    /// Gate `inner` for `principal`.
    ///
    /// The gate is a constructor argument and there is no nullable slot and
    /// no default: a session with no authorization plane passes
    /// [`stella_core::ports::NoAuthz`] **by name**, so "this deployment does
    /// not authorize tool calls" is a decision somebody typed and a reviewer
    /// can see.
    pub fn new(
        inner: &'a dyn ToolExecutor,
        gate: Arc<dyn AuthzGate>,
        principal: Principal,
    ) -> Self {
        let contracts = Self::snapshot(inner);
        Self {
            inner: Inner::Borrowed(inner),
            gate,
            principal,
            contracts,
        }
    }

    /// Own the inner executor by value — for an assembly function that builds
    /// the chain below (policy filter, customs, registry) and hands the whole
    /// stack back as one value, rather than a caller holding each layer as its
    /// own stack local. The lifetime rides along, so the boxed chain may still
    /// borrow the session's registry at its base.
    pub fn new_boxed(
        inner: Box<dyn ToolExecutor + 'a>,
        gate: Arc<dyn AuthzGate>,
        principal: Principal,
    ) -> Self {
        let contracts = Self::snapshot(inner.as_ref());
        Self {
            inner: Inner::Boxed(inner),
            gate,
            principal,
            contracts,
        }
    }

    fn snapshot(inner: &dyn ToolExecutor) -> HashMap<String, stella_protocol::ToolContract> {
        inner
            .schemas()
            .into_iter()
            .map(|schema| (schema.name.clone(), contracts::contract_for(&schema)))
            .collect()
    }

    /// The contract this view will authorize `name` against.
    fn contract(&self, name: &str) -> stella_protocol::ToolContract {
        self.contracts
            .get(name)
            .cloned()
            .unwrap_or_else(|| contracts::unknown_contract(name))
    }
}

impl GatedToolSet<'static> {
    /// Own the inner executor by `Arc` — for callers that hold the whole
    /// chain as one value (a boxed candidate workspace). Without this the
    /// gate would stop at the candidate boundary, and best-of-N would be a
    /// way around authorization.
    pub fn new_owned(
        inner: Arc<dyn ToolExecutor>,
        gate: Arc<dyn AuthzGate>,
        principal: Principal,
    ) -> Self {
        let contracts = Self::snapshot(inner.as_ref());
        Self {
            inner: Inner::Owned(inner),
            gate,
            principal,
            contracts,
        }
    }
}

#[async_trait]
impl ToolExecutor for GatedToolSet<'_> {
    /// Unfiltered — see the module docs on why this decorator is one-sided.
    fn schemas(&self) -> Vec<ToolSchema> {
        self.inner.get().schemas()
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        let contract = self.contract(name);
        let evaluation = self.gate.check(&contract, &self.principal, input);

        // Routed through the shared fold rather than matched here, so the
        // fail-closed rule (an `Err` denies whatever any softening flag says)
        // has exactly one implementation across the bus chains, the shell-hook
        // surface, and this gate. `NoOpinion`/`false` because the operator
        // plane is `PolicyToolSet` one layer down — it withholds before a call
        // ever reaches here — and no enforcement-softening switch is
        // configurable yet. Both are arguments this call site will pass
        // through when they exist, which is why they are passed at all.
        match authz_verdict(&OperatorPosture::NoOpinion, evaluation, false) {
            GateVerdict::Allow => self.inner.get().execute(name, input).await,
            GateVerdict::Deny { reason } => {
                ToolOutput::classified_error(ErrorClass::RefusedByPolicy, reason)
            }
            GateVerdict::RequireApproval { reason } => ToolOutput::classified_error(
                ErrorClass::RefusedByPolicy,
                format!(
                    "`{name}` needs a human's approval ({reason}), and this session has no \
                     approval route attached — it cannot be asked from here"
                ),
            ),
        }
    }

    /// Forwarded: a decorator that let the default `0.0` stand would silently
    /// drop sub-agent spend out of the parent's budget (see the port's
    /// contract).
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        self.inner.get().drain_sub_agent_spend_usd()
    }

    /// Forwarded: a swallowed wait request silently turns parked waits
    /// (#1471) back into model-step polling.
    fn drain_wait_request(&self) -> Option<stella_core::WaitRequest> {
        self.inner.get().drain_wait_request()
    }

    /// Forwarded: letting the empty default stand would serialize the inner
    /// stack's sibling spawns for every session composed through this view.
    ///
    /// Not narrowed by the gate, unlike [`crate::policy::ToolPolicy`]'s
    /// decorator, and for the same reason `schemas` is not: whether a call is
    /// authorized can depend on its input, so a name is not withheld here on
    /// the strength of a decision made without one.
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        self.inner.get().parallel_safe_names()
    }

    /// Forwarded: the inner stack owns the invocation spans, and live
    /// procedure text must survive summarization behind this view too.
    fn active_skill_slugs(&self) -> Vec<String> {
        self.inner.get().active_skill_slugs()
    }

    /// Forwarded: letting the empty default stand would turn the end-of-turn
    /// service assertion (#2764) off for every surface composed through this.
    fn live_services(&self) -> Vec<stella_core::LiveService> {
        self.inner.get().live_services()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_core::ports::{AuthzDecision, AuthzEvalError, NoAuthz, RiskCeiling};
    use stella_protocol::{RiskLevel, ToolContract};

    /// A leaf that advertises one built-in and one MCP tool, and records
    /// whether it was ever actually reached.
    struct Leaf {
        reached: std::sync::Mutex<Vec<String>>,
    }

    impl Leaf {
        fn new() -> Self {
            Self {
                reached: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn reached(&self) -> Vec<String> {
            self.reached.lock().unwrap().clone()
        }
    }

    /// The three tools every test here needs: a reviewed `Low` read, the one
    /// reviewed `High` built-in, and a third-party tool nobody reviewed.
    const READ: &str = "get_state";
    const SPENDS: &str = "task";
    const THIRD_PARTY: &str = "mcp__vendor__deploy";

    #[async_trait]
    impl ToolExecutor for Leaf {
        fn schemas(&self) -> Vec<ToolSchema> {
            [READ, SPENDS, THIRD_PARTY]
                .into_iter()
                .map(|name| ToolSchema {
                    name: name.into(),
                    description: "d".into(),
                    input_schema: serde_json::json!({}),
                    read_only: name == READ,
                    speculation_safe: false,
                })
                .collect()
        }
        async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
            self.reached.lock().unwrap().push(name.to_string());
            ToolOutput::Ok {
                content: format!("ran {name}"),
            }
        }
        fn drain_sub_agent_spend_usd(&self) -> f64 {
            2.5
        }
        fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
            std::collections::HashSet::from(["task".to_string()])
        }
    }

    struct DenyAll;

    impl AuthzGate for DenyAll {
        fn name(&self) -> &'static str {
            "deny-all"
        }
        fn check(
            &self,
            contract: &ToolContract,
            _principal: &Principal,
            _input: &Value,
        ) -> Result<AuthzDecision, AuthzEvalError> {
            Ok(AuthzDecision::Deny {
                reason: format!("`{}` is not permitted here", contract.name()),
            })
        }
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

    fn input() -> Value {
        serde_json::json!({})
    }

    /// Anti-vacuity: without a gate in the way, the same call runs.
    #[tokio::test]
    async fn no_authz_lets_every_call_through() {
        let leaf = Leaf::new();
        let gated = GatedToolSet::new(&leaf, Arc::new(NoAuthz), Principal::User);

        assert!(matches!(
            gated.execute(SPENDS, &input()).await,
            ToolOutput::Ok { .. }
        ));
        assert_eq!(leaf.reached(), vec![SPENDS.to_string()]);
    }

    /// **The #2716 witness.** A `DenyAll` gate blocks a call the default path
    /// allows, and the tool is never reached — refusal is enforcement, not a
    /// label on a result.
    #[tokio::test]
    async fn a_deny_all_gate_blocks_a_call_the_default_path_allows() {
        let leaf = Leaf::new();
        let gated = GatedToolSet::new(&leaf, Arc::new(DenyAll), Principal::User);

        match gated.execute(SPENDS, &input()).await {
            ToolOutput::Error { message, class } => {
                assert!(message.contains(SPENDS), "names the tool: {message}");
                assert_eq!(
                    class,
                    Some(ErrorClass::RefusedByPolicy),
                    "a policy refusal must not be counted as a tool defect"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(
            leaf.reached().is_empty(),
            "the tool must not have executed: {:?}",
            leaf.reached()
        );
    }

    /// **The OXA-2056 witness at the dispatch site.** A gate that cannot
    /// evaluate denies — the arm that separates "refused" from "could not
    /// tell", and the one oxagen-platform got backwards.
    #[tokio::test]
    async fn a_gate_that_cannot_evaluate_denies_the_call() {
        let leaf = Leaf::new();
        let gated = GatedToolSet::new(&leaf, Arc::new(BrokenGate), Principal::User);

        match gated.execute(READ, &input()).await {
            ToolOutput::Error { message, .. } => assert!(
                message.contains("failing closed"),
                "must say why it refused: {message}"
            ),
            other => panic!("an unevaluable gate must deny, got {other:?}"),
        }
        assert!(leaf.reached().is_empty());
    }

    /// The trust boundary reaching dispatch: one ceiling, and the third-party
    /// tool is refused while the reviewed read is not — with no rule written
    /// about either tool by name. The MCP tool here even *claims* `read_only`
    /// (see [`Leaf::schemas`]) and is refused anyway, because nobody reviewed
    /// the claim.
    #[tokio::test]
    async fn a_medium_ceiling_admits_a_reviewed_read_and_refuses_a_third_party_tool() {
        let leaf = Leaf::new();
        let gated = GatedToolSet::new(
            &leaf,
            Arc::new(RiskCeiling::new(RiskLevel::Medium)),
            Principal::User,
        );

        assert!(
            matches!(gated.execute(READ, &input()).await, ToolOutput::Ok { .. }),
            "a reviewed Low-risk read must still run"
        );
        assert!(
            matches!(
                gated.execute(THIRD_PARTY, &input()).await,
                ToolOutput::Error { .. }
            ),
            "an unreviewed tool is graded High and must not clear a Medium ceiling"
        );
        assert!(
            matches!(
                gated.execute(SPENDS, &input()).await,
                ToolOutput::Error { .. }
            ),
            "the one built-in that spends money is graded High too"
        );
        assert_eq!(leaf.reached(), vec![READ.to_string()]);
    }

    /// The fail-closed path for a name the snapshot never saw — a tool that
    /// appeared after construction, or one the model invented.
    #[tokio::test]
    async fn an_unknown_name_is_authorized_as_untrusted_rather_than_waved_through() {
        let leaf = Leaf::new();
        let gated = GatedToolSet::new(
            &leaf,
            Arc::new(RiskCeiling::new(RiskLevel::Medium)),
            Principal::User,
        );

        assert!(matches!(
            gated.execute("appeared_mid_session", &input()).await,
            ToolOutput::Error { .. }
        ));
        assert!(leaf.reached().is_empty());
    }

    /// The decorator contract the port documents twice: a wrapper that lets
    /// the defaults stand silently breaks budget accounting and dispatch
    /// grouping, with no compiler complaint.
    #[test]
    fn the_decorator_forwards_what_a_decorator_must() {
        let leaf = Leaf::new();
        let gated = GatedToolSet::new(&leaf, Arc::new(DenyAll), Principal::User);

        assert_eq!(
            gated.drain_sub_agent_spend_usd(),
            2.5,
            "sub-agent spend must not vanish through the gate"
        );
        assert!(
            gated.parallel_safe_names().contains("task"),
            "the inner concurrency claim must survive"
        );
        assert_eq!(
            gated.schemas().len(),
            3,
            "this decorator narrows execution, not the advertised set"
        );
    }
}
