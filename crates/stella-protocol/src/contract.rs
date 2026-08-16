//! The governance half of a tool's declaration (#2716): what a call can
//! *cost the world*, and whether the claim saying so is trustworthy.
//!
//! [`crate::ToolSchema`] answers "what may the model send, and may this run
//! concurrently" — questions about dispatch. It cannot answer the question an
//! authorization plane asks, which is **"if this call does what it is allowed
//! to do, how bad is that, and who says so?"** `read_only` is the closest
//! thing to an answer and it is the wrong axis twice over: a metered web
//! search mutates nothing and still spends the user's money, and an MCP
//! server's `read_only: false` is a self-report from a party the workspace
//! has no reason to trust.
//!
//! # Composition, not restatement
//!
//! [`ToolContract`] **contains** the schema rather than re-declaring its
//! fields. That is deliberate and load-bearing for invariant #7: the bytes
//! advertised to the model are literally today's [`crate::ToolSchema`],
//! serialized by today's code, so introducing contracts cannot perturb the
//! prompt-cache prefix. A future field added *here* is a governance fact that
//! never reaches the prompt; a field added to the schema is a deliberate
//! cache-invalidation event, and keeping the two types separate is what makes
//! the difference visible in review.
//!
//! # Three axes, three fields
//!
//! The one defect this design most wants to avoid is a conflated axis
//! (oxagen-platform's `sensitivity` smuggles "read-only" into a risk grade,
//! and its docs promise that `riskLevel: high` forces approval while the code
//! keys only on a separate boolean). So:
//!
//! - [`ToolSchema::read_only`] — can this mutate workspace state? *Dispatch.*
//! - [`RiskLevel`] — how bad is the worst honest outcome? *Policy input.*
//! - [`ToolContract::requires_approval`] — must a human say yes? *Its own
//!   boolean, never inferred from risk.*
//!
//! Risk does real work — a [`crate::contract::RiskLevel`] ceiling is what a
//! principal's grant is expressed in — but it never silently becomes an
//! approval prompt. If a tool needs asking, it says so.

use serde::{Deserialize, Serialize};

use crate::tool::ToolSchema;

/// How much damage one honest call of a tool can do.
///
/// "Honest" is the operative word: this grades what the tool does when it
/// works as designed, not what a defect or a hostile implementation could
/// achieve. A [`Provenance::Declared`] tool's *stated* risk is a claim like
/// any other and is treated as one by [`ToolContract::declared`].
///
/// Ordered, and the order is the point — a policy expresses a grant as a
/// ceiling (`risk <= Medium`), so the comparison has to mean something.
///
/// # Unknown tokens read as the maximum, deliberately
///
/// [`Self::Destructive`] carries `#[serde(other)]`, so a grade minted by a
/// newer build (`"catastrophic"`) deserializes here as `Destructive` rather
/// than failing the decode — the same forward-compat posture as
/// [`crate::ErrorClass`], pointed the one direction that is safe for a
/// security field. Degrading an unrecognized risk to `Low` would let a newer
/// emitter's most dangerous tools through an older reader's ceiling; degrading
/// to the maximum can only ever refuse too much, which is recoverable.
/// Lossy on the way out for the same reason `ErrorClass` is: re-serializing
/// writes `"destructive"`, not the token that came in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// Observes. Touches nothing outside the process except state the agent
    /// derives for itself — reading a file, listing the task board, a graph
    /// query that brings its own index up to date on the way to answering.
    Low,
    /// Real but bounded and locally reversible: writing inside the workspace,
    /// spending a metered API call, starting a process.
    Medium,
    /// Reaches outside the workspace or costs something a `git checkout`
    /// cannot undo — pushing a branch, filing an issue, running an arbitrary
    /// shell command.
    High,
    /// Irreversible by the agent that did it. Also the grade every
    /// unrecognized token reads as (see the type docs).
    #[serde(other)]
    Destructive,
}

impl RiskLevel {
    /// The wire/storage token — byte-identical to the serde `snake_case`
    /// spelling (pinned by test), so a stored grade and a wire grade can
    /// never disagree about what a level is called.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Destructive => "destructive",
        }
    }

    /// Whether this level sits at or below `ceiling` — the shape a grant is
    /// written in ("this principal may call up to `Medium`").
    #[must_use]
    pub fn within(self, ceiling: Self) -> bool {
        self <= ceiling
    }
}

/// Who is making the claims in a contract, and therefore whether they may be
/// believed.
///
/// The asymmetry this encodes is the whole reason a plugin platform can be
/// governed at all: a built-in's declaration was read by a human in review,
/// and a third party's was not. Without this field a hostile MCP server and a
/// reviewed built-in that make identical claims are indistinguishable to
/// every policy above them — which is exactly the state #2716 exists to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// Compiled into this binary and declared in the canonical catalog. Its
    /// claims are as trustworthy as the review that landed them.
    Builtin,
    /// Self-declared by something outside the binary — an MCP server's tool
    /// annotations, a `.stella/tools/*.toml` manifest. Every claim is a
    /// *claim*; see [`ToolContract::trusted_read_only`] for the one place
    /// that distinction is load-bearing today.
    ///
    /// Unknown tokens read as this variant: a provenance a newer build knows
    /// about is, from here, exactly "not one we can vouch for".
    #[serde(other)]
    Declared,
}

/// Why a contract is not internally consistent.
///
/// Typed rather than a `String` (invariant #5) because the registry branches
/// on which claim is contradictory when it refuses to register a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    /// `speculation_safe` without `read_only`. Speculation runs a call twice
    /// *before its step commits*; a tool that mutates cannot survive that, so
    /// the stronger claim structurally implies the weaker one (#923).
    SpeculativeButMutating {
        /// The offending tool.
        name: String,
    },
    /// A trusted read-only tool graded above [`RiskLevel::Medium`]. A
    /// built-in that mutates nothing cannot be irreversible; one of the two
    /// declarations is wrong, and a human has to say which.
    ReadOnlyButDangerous {
        /// The offending tool.
        name: String,
        /// The grade it declared.
        risk: RiskLevel,
    },
}

impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SpeculativeButMutating { name } => write!(
                f,
                "`{name}` declares speculation_safe without read_only: a speculated \
                 call may execute twice before its step commits"
            ),
            Self::ReadOnlyButDangerous { name, risk } => write!(
                f,
                "`{name}` declares read_only with risk `{}`: a built-in that mutates \
                 nothing cannot be graded above `medium`",
                risk.as_str()
            ),
        }
    }
}

impl std::error::Error for ContractError {}

/// One tool's complete declaration: what the model may send
/// ([`ToolSchema`]), plus what an authorization plane needs to decide whether
/// this caller may send it at all.
///
/// Serde-first (invariant #4) because it crosses crate boundaries — the
/// engine's [`crate::ToolSchema`] half already does, and the governance half
/// is what an embedding host reads to authorize a remoted call from metadata
/// instead of maintaining its own name → capability side-table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ToolContract {
    /// What is advertised to the model — unchanged, and serialized by the
    /// same code that always serialized it (see the module docs on why this
    /// is composition rather than restatement).
    pub schema: ToolSchema,
    /// How bad one honest call can be. Policy input, never an implicit
    /// approval prompt.
    pub risk: RiskLevel,
    /// Whether a human must say yes before this runs, independent of
    /// [`Self::risk`]. Separate on purpose: a docs-versus-code disagreement
    /// about which field gates approval is how oxagen-platform shipped
    /// high-risk capabilities that ran unprompted.
    #[serde(default)]
    pub requires_approval: bool,
    /// Whether the claims above were reviewed or merely asserted.
    pub provenance: Provenance,
    /// What a successful call's structured half ([`crate::tool::ToolOutput`]'s
    /// `Ok.data`) must look like, when the tool promises one (#3285). `None`
    /// means the tool's output is prose only — every tool written before
    /// this field existed. Deliberately *not* part of [`Self::schema`]: the
    /// model never reads it (it feeds the registry's post-execution check,
    /// not the prompt), so keeping it here is what keeps invariant #7
    /// structural. Optional and absent-when-`None` so every contract
    /// serialized before the field round-trips byte-identically
    /// (invariant #4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
}

impl ToolContract {
    /// A contract for a tool compiled into this binary, whose claims a human
    /// reviewed.
    #[must_use]
    pub fn builtin(schema: ToolSchema, risk: RiskLevel) -> Self {
        Self {
            schema,
            risk,
            requires_approval: false,
            provenance: Provenance::Builtin,
            output_schema: None,
        }
    }

    /// The same contract, promising that a successful call's structured half
    /// matches `output_schema` (#3285). Builder-shaped because most tools
    /// make no such promise, and the two constructors above should stay
    /// exactly the pre-#3285 shape.
    #[must_use]
    pub fn with_output_schema(mut self, output_schema: serde_json::Value) -> Self {
        self.output_schema = Some(output_schema);
        self
    }

    /// A contract for a tool that described *itself* — an MCP server's
    /// advertisement, a customer's TOML manifest.
    ///
    /// Graded [`RiskLevel::High`] regardless of what it claims, because the
    /// honest answer is "unknown" and this is the ceiling-shaped way to say
    /// so: an operator who wants third-party tools raises the ceiling to
    /// `High` deliberately, rather than discovering that an unreviewed tool
    /// inherited `Low` from a field it filled in itself. Not
    /// [`RiskLevel::Destructive`] — that grade means "irreversible", and
    /// spending it on "unknown" would leave nothing to say about an
    /// unreviewed tool that really does delete things.
    ///
    /// The schema's own `read_only`/`speculation_safe` claims are preserved
    /// verbatim: they are what the tool *said*, and a policy that wants to
    /// display or reason about the claim needs it. What they must never do is
    /// silently buy dispatch privileges — see [`Self::trusted_read_only`].
    #[must_use]
    pub fn declared(schema: ToolSchema) -> Self {
        Self {
            schema,
            risk: RiskLevel::High,
            requires_approval: false,
            provenance: Provenance::Declared,
            output_schema: None,
        }
    }

    /// The tool's dispatch name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.schema.name
    }

    /// Read-only **as a fact the engine may act on**, not as a claim someone
    /// made: `read_only` conjoined with reviewed provenance.
    ///
    /// This is #2716 §6's trust boundary reduced to one function. A manifest
    /// or MCP tool asserting `read_only = true` is asserting something nobody
    /// checked, and the read-only bit is not decorative — it admits a tool
    /// into concurrent dispatch alongside other reads, and fences a verifier
    /// into a set it cannot mutate the workspace from. A false claim there is
    /// a data race and a broken verification boundary, not a cosmetic error.
    ///
    /// Deliberately **not** used to filter what is advertised: an untrusted
    /// tool is still callable, still policy-governed, and still displays its
    /// own claim. It just cannot buy privileges with it.
    #[must_use]
    pub fn trusted_read_only(&self) -> bool {
        self.schema.read_only && matches!(self.provenance, Provenance::Builtin)
    }

    /// Check the claims against each other.
    ///
    /// The read-only/risk implication is asserted for [`Provenance::Builtin`]
    /// only, and that exemption is the design rather than a hole: a declared
    /// tool claiming `read_only` while graded `High` is the *expected* output
    /// of [`Self::declared`] — an unreviewed claim next to a cautious grade.
    /// Rejecting it would force the untrusted path to either trust the claim
    /// or discard it, and both directions lose the information a policy needs.
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema.speculation_safe && !self.schema.read_only {
            return Err(ContractError::SpeculativeButMutating {
                name: self.schema.name.clone(),
            });
        }
        if matches!(self.provenance, Provenance::Builtin)
            && self.schema.read_only
            && !self.risk.within(RiskLevel::Medium)
        {
            return Err(ContractError::ReadOnlyButDangerous {
                name: self.schema.name.clone(),
                risk: self.risk,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(name: &str, read_only: bool, speculation_safe: bool) -> ToolSchema {
        ToolSchema {
            name: name.into(),
            description: "d".into(),
            input_schema: serde_json::json!({}),
            read_only,
            speculation_safe,
        }
    }

    #[test]
    fn contract_roundtrips() {
        let contract = ToolContract::builtin(schema("read_file", true, true), RiskLevel::Low);
        let json = serde_json::to_string(&contract).unwrap();
        let back: ToolContract = serde_json::from_str(&json).unwrap();
        assert_eq!(back, contract);
    }

    #[test]
    fn a_contract_with_an_output_schema_roundtrips_and_a_bare_one_is_the_old_bytes() {
        // Invariant #4 for #3285, both directions: the new field survives
        // the wire, and its absence is byte-identical to the pre-#3285
        // serialization so no stored contract changes shape underneath a
        // reader.
        let promised = ToolContract::builtin(schema("list_state", true, true), RiskLevel::Low)
            .with_output_schema(serde_json::json!({
                "type": "object",
                "properties": { "keys": { "type": "array" } },
                "required": ["keys"]
            }));
        let json = serde_json::to_string(&promised).unwrap();
        let back: ToolContract = serde_json::from_str(&json).unwrap();
        assert_eq!(back, promised);

        let bare = ToolContract::builtin(schema("read_file", true, true), RiskLevel::Low);
        let json = serde_json::to_string(&bare).unwrap();
        assert!(
            !json.contains("output_schema"),
            "an absent promise must be absent bytes: {json}"
        );
        let old: ToolContract = serde_json::from_str(&json).unwrap();
        assert_eq!(old, bare);
    }

    #[test]
    fn risk_token_and_serde_spelling_agree() {
        for risk in [
            RiskLevel::Low,
            RiskLevel::Medium,
            RiskLevel::High,
            RiskLevel::Destructive,
        ] {
            let wire = serde_json::to_value(risk).unwrap();
            assert_eq!(wire, serde_json::Value::String(risk.as_str().into()));
        }
    }

    /// The safe direction for a security field: a grade this build has never
    /// heard of must read as the most restrictive one, never the least.
    #[test]
    fn an_unknown_risk_token_reads_as_the_maximum() {
        let risk: RiskLevel = serde_json::from_str("\"catastrophic\"").unwrap();
        assert_eq!(risk, RiskLevel::Destructive);
        assert!(
            !risk.within(RiskLevel::High),
            "must not clear a High ceiling"
        );
    }

    /// Same posture one axis over: a provenance we do not recognize is, by
    /// definition, not one whose claims we can vouch for.
    #[test]
    fn an_unknown_provenance_token_reads_as_untrusted() {
        let provenance: Provenance = serde_json::from_str("\"signed_by_someone\"").unwrap();
        assert_eq!(provenance, Provenance::Declared);
    }

    #[test]
    fn risk_is_ordered_so_a_ceiling_means_something() {
        assert!(RiskLevel::Low.within(RiskLevel::Medium));
        assert!(RiskLevel::Medium.within(RiskLevel::Medium));
        assert!(!RiskLevel::High.within(RiskLevel::Medium));
        assert!(!RiskLevel::Destructive.within(RiskLevel::High));
    }

    /// The trust asymmetry, stated as the test that would fail if someone
    /// "simplified" `trusted_read_only` to `self.schema.read_only`.
    #[test]
    fn a_declared_tools_read_only_claim_buys_it_nothing() {
        let claimed = ToolContract::declared(schema("mcp__x__peek", true, true));
        assert!(claimed.schema.read_only, "the claim is preserved verbatim");
        assert!(
            !claimed.trusted_read_only(),
            "an unreviewed claim must not admit a tool to the read-only set"
        );

        let reviewed = ToolContract::builtin(schema("read_file", true, true), RiskLevel::Low);
        assert!(reviewed.trusted_read_only());
    }

    #[test]
    fn a_declared_tool_is_graded_high_whatever_it_claims() {
        let contract = ToolContract::declared(schema("mcp__x__harmless", true, false));
        assert_eq!(contract.risk, RiskLevel::High);
        assert!(!contract.risk.within(RiskLevel::Medium));
        // And that shape must survive validation — it is the expected output
        // of the untrusted path, not a contradiction.
        assert!(contract.validate().is_ok());
    }

    #[test]
    fn speculation_without_read_only_is_rejected() {
        let contract = ToolContract::builtin(schema("bash", false, true), RiskLevel::High);
        assert_eq!(
            contract.validate(),
            Err(ContractError::SpeculativeButMutating {
                name: "bash".into()
            })
        );
    }

    #[test]
    fn a_trusted_read_only_tool_cannot_be_graded_dangerous() {
        let contract = ToolContract::builtin(schema("grep", true, true), RiskLevel::Destructive);
        assert_eq!(
            contract.validate(),
            Err(ContractError::ReadOnlyButDangerous {
                name: "grep".into(),
                risk: RiskLevel::Destructive,
            })
        );
    }

    /// Invariant #7's structural guarantee: the advertised half of a contract
    /// is byte-identical to the `ToolSchema` that existed before contracts,
    /// so nothing about this type can perturb the prompt-cache prefix.
    #[test]
    fn the_advertised_half_serializes_exactly_as_a_bare_schema() {
        let schema = schema("read_file", true, true);
        let contract = ToolContract::builtin(schema.clone(), RiskLevel::Low);
        assert_eq!(
            serde_json::to_string(&contract.schema).unwrap(),
            serde_json::to_string(&schema).unwrap()
        );
    }
}
