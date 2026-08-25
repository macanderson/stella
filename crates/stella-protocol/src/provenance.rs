// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! How strong the evidence behind a self-improvement artifact is, carried as a
//! value rather than reconstructed by whoever promotes it (#2782).
//!
//! # The defect this exists to make impossible
//!
//! Stella turns observations into artifacts through a chain — reflection →
//! proposal → skill / rule / tool. Until this module, the strength of the
//! original evidence was not carried along it. By the time something was a
//! published skill or an executable tool, the record that it originated in a
//! model's opinion rather than in a passing test was gone, and every artifact
//! at the end of the chain looked alike.
//!
//! The concrete hazard is **laundering by aggregation**: several model
//! critiques agreeing with each other is still zero deterministic evidence,
//! but a promotion gate that counts votes reads agreement as strength.
//! #2569/#2570 is this repository's live proof that a plausible consensus
//! signal can be structurally worthless — ablating the verifier turned every
//! run into a PASS, and the vote count never noticed.
//!
//! Two rules answer it, and both are mechanical here rather than editorial:
//!
//! 1. **No aggregation promotes a grade.** [`ProvenanceGrade::weakest`] is the
//!    only way to combine evidence, and it is a `min`. N model critiques
//!    remain a model critique however many agree. Only re-deriving a claim
//!    from a stronger source promotes it, which means constructing a new
//!    record against that source.
//! 2. **Impact sets the required grade.** [`ImpactClass::required_grade`] says
//!    what each blast radius costs, and [`authorises`] is the single gate.
//!    A prompt hint may be trialled from a mined trajectory; a blocking guard
//!    or an executable tool may not, because those two can break a teammate's
//!    session.
//!
//! # Two axes, not one
//!
//! [`ProvenanceGrade`] answers *how reproducible is the evidence*.
//! [`PublicationAuthority`] answers *who may publish it*. #2782's own rule
//! keeps them apart — a guard or a tool needs "deterministic proof **plus**
//! publication authority" — so collapsing them into one ladder would let an
//! approver's signature stand in for a witness test, which is the substitution
//! this repository refuses everywhere else.
//!
//! That is also why [`ProvenanceGrade::HumanReview`] does not sit at the top of
//! the grade order. A person signing off is accountable, and that is what
//! [`PublicationAuthority::LocalHuman`] records; it is not evidence that the
//! change works. CLAUDE.md states the same thing about this repository's own
//! reviews — a witness test or a golden diff someone read is evidence, and a
//! sentence in a PR description is not. Human review ranks above
//! [`ProvenanceGrade::ModelCritique`] because a judgement someone is
//! accountable for outranks a judgement nobody is, and below
//! [`ProvenanceGrade::TrajectoryAbstraction`] because a measured pattern
//! survives its author's confidence.
//!
//! # Where it lives
//!
//! `stella-core` derives a grade from the observation records it owns,
//! `stella-parity`'s evolution ledger types its `evidence` column as one, and
//! `stella-cli` renders it where a human decides. A type crossing crate
//! boundaries belongs in this crate by invariant #1, and by invariant #4 it
//! round-trips through `serde_json` byte-for-byte — see this module's tests.
//!
//! Every variant is a closed, `'static`-nameable tag: a grade emitted into a
//! `stella-diag` field has to be an enum, never a `String` carrying model
//! output, and [`ProvenanceGrade::as_str`] is what makes that possible without
//! an allocation. The grade is metadata and may cross an egress encoder; the
//! evidence it points at is content and may not (invariant #3,
//! `stella-store/src/content_free.rs`).

use serde::{Deserialize, Serialize};

/// How reproducible the evidence behind an artifact is.
///
/// **Ordered weakest-first**, so the derived [`Ord`] is the strength order and
/// `min` is the aggregation rule. Adding a variant means placing it in that
/// order deliberately — the position is the semantics, not a formatting
/// choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceGrade {
    /// A model's judgement about a run. The weakest grade, and the one the
    /// aggregation rule exists to contain: agreement between critiques is not
    /// evidence, it is correlated opinion.
    ModelCritique,
    /// A person read it and signed off. Accountable, and still a judgement —
    /// it does not become a measurement by being human.
    HumanReview,
    /// A pattern mined across runs: statistical, not proven. Enough to trial a
    /// hint, never enough to publish something that can fail a build.
    TrajectoryAbstraction,
    /// A command's exit status, a build result, a number measured out of
    /// `stella-events.jsonl`. The environment answered, rather than a model
    /// or a person reporting what it would have said.
    EnvironmentObservation,
    /// A witness test that went fail → pass, or a guard that fails the gate.
    /// The only grade that authorises a blocking guard or an executable tool.
    DeterministicProof,
}

impl ProvenanceGrade {
    /// Every grade, weakest first — the enumeration a sweep iterates so a new
    /// variant reaches each exhaustiveness check rather than being missed by a
    /// hand-written list.
    pub const ALL: &'static [Self] = &[
        Self::ModelCritique,
        Self::HumanReview,
        Self::TrajectoryAbstraction,
        Self::EnvironmentObservation,
        Self::DeterministicProof,
    ];

    /// The canonical `snake_case` tag — identical to what `serde` writes, so a
    /// diagnostic field, a stored record, and a rendered surface all name a
    /// grade the same way. Allocation-free, which is what lets it be a
    /// `stella-diag` field value at all.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ModelCritique => "model_critique",
            Self::HumanReview => "human_review",
            Self::TrajectoryAbstraction => "trajectory_abstraction",
            Self::EnvironmentObservation => "environment_observation",
            Self::DeterministicProof => "deterministic_proof",
        }
    }

    /// Prose for a human deciding, in the vocabulary #2782 wrote the table in.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::ModelCritique => "a model's judgement about a run",
            Self::HumanReview => "a person read it and signed off",
            Self::TrajectoryAbstraction => "a pattern mined across runs, statistical rather than proven",
            Self::EnvironmentObservation => "a command exit status, a build result, or a measured number",
            Self::DeterministicProof => "a witness test that went fail to pass, or a guard that fails the gate",
        }
    }

    /// **The aggregation rule: combining evidence can only weaken it.**
    ///
    /// Returns the weakest grade in `grades`, or `None` for an empty pool —
    /// which is the honest answer, because no evidence is not the same as weak
    /// evidence and must not round up to one.
    ///
    /// This is the whole of #2782's rule 1. A caller that wants a stronger
    /// grade has to re-derive the claim against a stronger source and build a
    /// new record; there is deliberately no `promote`, no vote count, and no
    /// confidence threshold that crosses a grade boundary.
    #[must_use]
    pub fn weakest(grades: impl IntoIterator<Item = Self>) -> Option<Self> {
        grades.into_iter().min()
    }
}

/// Who may publish a change — the second axis, kept separate from evidence so
/// that an approver's signature can never stand in for a witness test.
///
/// Ordered weakest-first like [`ProvenanceGrade`], so `min` composes the same
/// way when a publication path has more than one actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PublicationAuthority {
    /// The agent acting on its own. Enough to propose and to trial, never
    /// enough to publish something that can fail a build — the same line
    /// `PromotionEventRecord` already draws when it refuses a `System` actor a
    /// blocking grant.
    Agent,
    /// A person on this machine approved it.
    LocalHuman,
    /// An org-managed policy document authorised it.
    OrgPolicy,
}

impl PublicationAuthority {
    /// Every authority, weakest first.
    pub const ALL: &'static [Self] = &[Self::Agent, Self::LocalHuman, Self::OrgPolicy];

    /// The canonical `snake_case` tag, matching `serde`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::LocalHuman => "local_human",
            Self::OrgPolicy => "org_policy",
        }
    }
}

/// What a change can break if it is wrong — the blast radius the grade is
/// rationing.
///
/// Ordered least-dangerous-first, so a reviewer reading the enum reads the
/// escalation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ImpactClass {
    /// Text added to a prompt. Wrong, it wastes tokens and misleads one turn.
    PromptHint,
    /// A weight on what gets recalled. Wrong, it surfaces the wrong record.
    RecallBias,
    /// A published record that informs and never steers — a knowledge skill.
    /// Wrong, a human reads something untrue and can tell.
    AdvisoryRecord,
    /// A rule that steers the agent. Wrong, it changes behaviour without the
    /// human who wrote it being in the loop for the turn it changes.
    SteeringDirective,
    /// A guard that can refuse a turn or fail a gate. Wrong, it blocks work
    /// that should have shipped — in someone else's session.
    BlockingGuard,
    /// Code that runs in a teammate's session. The largest radius there is.
    ExecutableTool,
}

impl ImpactClass {
    /// Every impact class, least-dangerous first.
    pub const ALL: &'static [Self] = &[
        Self::PromptHint,
        Self::RecallBias,
        Self::AdvisoryRecord,
        Self::SteeringDirective,
        Self::BlockingGuard,
        Self::ExecutableTool,
    ];

    /// The canonical `snake_case` tag, matching `serde`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PromptHint => "prompt_hint",
            Self::RecallBias => "recall_bias",
            Self::AdvisoryRecord => "advisory_record",
            Self::SteeringDirective => "steering_directive",
            Self::BlockingGuard => "blocking_guard",
            Self::ExecutableTool => "executable_tool",
        }
    }

    /// The weakest evidence that may publish this impact class.
    ///
    /// #2782 states the two ends: a prompt hint or a recall bias may be
    /// trialled from trajectory evidence, and a blocking guard or an executable
    /// tool requires deterministic proof. The middle two are placed against
    /// the authority this repository already enforces — a directive steers the
    /// agent, so it is held above a record that only informs.
    #[must_use]
    pub fn required_grade(self) -> ProvenanceGrade {
        match self {
            Self::PromptHint | Self::RecallBias | Self::AdvisoryRecord => {
                ProvenanceGrade::TrajectoryAbstraction
            }
            Self::SteeringDirective => ProvenanceGrade::EnvironmentObservation,
            Self::BlockingGuard | Self::ExecutableTool => ProvenanceGrade::DeterministicProof,
        }
    }

    /// The weakest authority that may publish this impact class.
    ///
    /// The two classes that can break a teammate's session need a person or a
    /// policy behind them, which is #2782's "plus publication authority" and
    /// the same line `PromotionEventRecord::SystemGrantedBlocking` already
    /// draws for blocking grants.
    #[must_use]
    pub fn required_authority(self) -> PublicationAuthority {
        match self {
            Self::PromptHint
            | Self::RecallBias
            | Self::AdvisoryRecord
            | Self::SteeringDirective => PublicationAuthority::Agent,
            Self::BlockingGuard | Self::ExecutableTool => PublicationAuthority::LocalHuman,
        }
    }
}

/// Why a promotion was refused — typed, because a caller has to branch on
/// which half was short (invariant #5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case", tag = "refusal")]
pub enum PromotionRefusal {
    /// The evidence is weaker than this impact class requires.
    EvidenceTooWeak {
        impact: ImpactClass,
        required: ProvenanceGrade,
        actual: ProvenanceGrade,
    },
    /// The evidence is strong enough, but nobody with standing published it.
    AuthorityTooLow {
        impact: ImpactClass,
        required: PublicationAuthority,
        actual: PublicationAuthority,
    },
    /// There was no evidence at all. Distinct from weak evidence on purpose:
    /// an empty pool is a bug in the caller, not a weak claim.
    NoEvidence { impact: ImpactClass },
}

impl PromotionRefusal {
    /// One line a human can act on, naming both what was required and what was
    /// offered — a refusal that does not say what would fix it is a dead end.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::EvidenceTooWeak {
                impact,
                required,
                actual,
            } => format!(
                "{} requires {} ({}); the evidence is {} ({})",
                impact.as_str(),
                required.as_str(),
                required.describe(),
                actual.as_str(),
                actual.describe(),
            ),
            Self::AuthorityTooLow {
                impact,
                required,
                actual,
            } => format!(
                "{} must be published by {} or stronger; it was published by {}",
                impact.as_str(),
                required.as_str(),
                actual.as_str(),
            ),
            Self::NoEvidence { impact } => format!(
                "{} was promoted with no supporting evidence at all",
                impact.as_str()
            ),
        }
    }
}

/// **The gate.** Whether evidence of this grade, published by this authority,
/// may become an artifact of this impact class.
///
/// Both halves are checked and the evidence half is checked first, so a
/// refusal names the missing witness rather than the missing signature when
/// both are short — the more useful of the two answers.
///
/// # Errors
///
/// Returns the typed [`PromotionRefusal`] naming which half fell short.
pub fn authorises(
    grade: Option<ProvenanceGrade>,
    authority: PublicationAuthority,
    impact: ImpactClass,
) -> Result<(), PromotionRefusal> {
    let Some(grade) = grade else {
        return Err(PromotionRefusal::NoEvidence { impact });
    };
    let required = impact.required_grade();
    if grade < required {
        return Err(PromotionRefusal::EvidenceTooWeak {
            impact,
            required,
            actual: grade,
        });
    }
    let required_authority = impact.required_authority();
    if authority < required_authority {
        return Err(PromotionRefusal::AuthorityTooLow {
            impact,
            required: required_authority,
            actual: authority,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
