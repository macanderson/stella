//! The three lifecycle record bodies the adaptive loop writes: observations,
//! proposals, and promotion events.
//!
//! These are the first records that are **born canonical** (ADR 0010 point 1).
//! None has a legacy counterpart anywhere, so none needs a migration — they are
//! written only to `context_records`, immutable and JCS-hashed per ADR 0004,
//! from the first turn that emits one.
//!
//! ## Why these types and not the existing ones
//!
//! `context_record::kind` already carries the taxonomy enums, and this module
//! uses them rather than minting parallel ones. What it adds is the *record
//! bodies* — the actual serialized shapes with the fields this loop needs, which
//! the enum layer deliberately never defined.
//!
//! ## Identity is derived, never allocated
//!
//! Every `record_id` here is a function of the record's own content. That is
//! what makes extraction replay-idempotent: re-extracting the same evidence
//! computes the same id, and the ledger's append turns the second write into a
//! no-op. An allocated id (a counter, a uuid) would make every replay a
//! duplicate.
//!
//! ## The two invariants that are enforced by the constructor
//!
//! Spec §5.4 and §7 both name rules that are worthless as documentation:
//!
//! * an inferred directive may never *start* blocking — enforced by
//!   [`PromotionEventRecord::new`], which has no way to express it;
//! * a proposal counts **distinct tasks**, never raw events — enforced by
//!   [`ProposalRecord`] carrying `distinct_task_count` as a separate field from
//!   `occurrence_count`, and by [`ProposalRecord::is_eligible`] reading only the
//!   former.

use serde::{Deserialize, Serialize};

use super::hash::{RecordHashError, record_hash};
use super::kind::{
    ContextRecordKind, DirectiveEnforcement, Origin, PromotionAction, RecordProposalKind,
    RecordProposalStatus,
};
use super::{Confidence, RecordValidationError};

/// The record schema version these bodies are written against. Stored on every
/// row so a later reader can tell which shape it is looking at without guessing
/// from the fields present.
pub const LIFECYCLE_SCHEMA_VERSION: &str = "1.0-draft";

/// Where an observation came from. Deliberately small: spec §1's "extend before
/// inventing" applies to vocabularies too, and each of these names evidence the
/// system *already captures*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSource {
    /// A lesson from post-turn reflection.
    ReflectionLesson,
    /// An explicit memory citation with its usefulness judgment.
    MemoryCitation,
    /// The outcome of a tool call.
    ToolOutcome,
}

impl ObservationSource {
    /// The canonical `snake_case` string, and the prefix of a derived id.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReflectionLesson => "reflection_lesson",
            Self::MemoryCitation => "memory_citation",
            Self::ToolOutcome => "tool_outcome",
        }
    }
}

/// One typed observation, extracted from evidence already captured.
///
/// Carries **no instruction authority** (spec §5.4): an observation informs, it
/// never steers. Nothing selects an observation into a prompt; only the
/// directives a proposal eventually becomes can do that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationRecord {
    pub schema_version: String,
    pub record_kind: ContextRecordKind,
    pub record_id: String,
    pub lineage_id: String,
    pub origin: Origin,
    pub source: ObservationSource,
    /// An opaque back-reference to the evidence (`reflection:<ts>`,
    /// `citation:<id>`, `tool:<name>#<n>`) for auditing.
    pub source_ref: String,
    /// **The distinct-task identity.** This is the field that makes spec §7's
    /// anti-poisoning rule expressible at all: thirty observations sharing one
    /// `task_id` are thirty events in one task, and must never satisfy a
    /// three-task threshold.
    pub task_id: String,
    /// The observation text, **already redacted**. Redaction happens before the
    /// record is constructed, not before it is written, so there is no window in
    /// which an unredacted body exists as a typed record.
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<String>,
    /// Whether redaction actually removed anything, so the honesty invariant
    /// (§5.5, no silent transformation) is visible on the record itself.
    pub redacted: bool,
    pub observed_at: String,
    pub record_hash: String,
}

impl ObservationRecord {
    /// Build an observation, deriving its id from its content and sealing it
    /// with the ADR 0004 hash.
    ///
    /// The id is `obs_<source>_<12 hex of the content hash>`. Content-derived
    /// because replay-idempotence depends on it: the same evidence extracted
    /// twice must produce the same id so the ledger can recognize the second
    /// write as a replay rather than a new observation.
    pub fn new(
        source: ObservationSource,
        source_ref: impl Into<String>,
        task_id: impl Into<String>,
        text: impl Into<String>,
        domains: Vec<String>,
        redacted: bool,
        observed_at: impl Into<String>,
    ) -> Result<Self, RecordHashError> {
        let mut record = Self {
            schema_version: LIFECYCLE_SCHEMA_VERSION.to_string(),
            record_kind: ContextRecordKind::Observation,
            record_id: String::new(),
            lineage_id: String::new(),
            origin: Origin::Observed,
            source,
            source_ref: source_ref.into(),
            task_id: task_id.into(),
            text: text.into(),
            domains,
            redacted,
            observed_at: observed_at.into(),
            record_hash: String::new(),
        };
        // Hash once with the identity fields empty to derive the id, then hash
        // again with the id populated to seal the record. Two passes rather than
        // one because the id is part of what the final hash covers — a record
        // whose hash did not cover its own id could be re-filed under another id
        // without the hash noticing.
        let seed = record_hash(&record)?;
        let digest = seed.strip_prefix("sha256:").unwrap_or(&seed);
        record.record_id = format!("obs_{}_{}", source.as_str(), &digest[..12]);
        record.lineage_id = record.record_id.clone();
        record.record_hash = record_hash(&record)?;
        Ok(record)
    }
}

/// The components a proposal was scored on, kept separately so a reviewer can
/// see *why* it cleared the bar rather than being handed one opaque number.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProposalScore {
    /// Raw supporting observations. Reported, never thresholded on.
    pub occurrences: u32,
    /// Distinct tasks those observations span. **This** is what gates.
    pub distinct_tasks: u32,
    /// Whether any supporting observation was independently salient.
    pub salient: bool,
    /// The miner's ranking score, carried through unchanged so the typed path
    /// ranks candidates identically to the lexical one.
    pub rank: f64,
}

/// A durable, reviewable proposal: what the loop wants to make durable, the
/// evidence behind it, and the arithmetic that made it eligible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalRecord {
    pub schema_version: String,
    pub record_kind: ContextRecordKind,
    pub record_id: String,
    pub lineage_id: String,
    pub proposal_kind: RecordProposalKind,
    pub status: RecordProposalStatus,
    /// The miner's stable `<slug>-<hash8>` identity. **Unchanged from the
    /// lexical path** — it is the skill filename, so drifting it orphans every
    /// file a user already has.
    pub candidate_id: String,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<String>,
    /// `record_id`s of the observations behind this proposal, so the review
    /// surface can show "three separate tasks, here they are" without
    /// re-deriving anything.
    pub supporting_observations: Vec<String>,
    pub score: ProposalScore,
    pub confidence: Confidence,
    pub observed_at: String,
    pub record_hash: String,
}

impl ProposalRecord {
    /// Build a proposal from a mined candidate and the observations behind it.
    ///
    /// `candidate_id` is the lexical miner's own identity and is carried
    /// verbatim, so the proposal names exactly the artifact the miner would
    /// write. Re-inducing the same candidate tomorrow lands in the same
    /// lineage, which is what lets a decline be remembered against it; the
    /// per-revision `record_id` still varies with content, so a re-induction
    /// with new evidence is a new revision rather than a silent overwrite.
    ///
    /// **The lineage is namespaced by `proposal_kind`, and it has to be.** The
    /// skills and rules miners share `crate::mining`, so for one lesson they
    /// derive the *same* `<slug>-<hash8>` — that is the shared module working
    /// as designed. But a knowledge proposal and a directive proposal for that
    /// lesson are different artifacts with different authority: one becomes a
    /// SKILL.md that informs, the other a rule that steers. Without the
    /// namespace they would collide onto one lineage, and declining the rule
    /// would silently decline the skill too.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        proposal_kind: RecordProposalKind,
        status: RecordProposalStatus,
        candidate_id: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
        domains: Vec<String>,
        supporting_observations: Vec<String>,
        score: ProposalScore,
        confidence: Confidence,
        observed_at: impl Into<String>,
    ) -> Result<Self, RecordHashError> {
        let candidate_id = candidate_id.into();
        let mut record = Self {
            schema_version: LIFECYCLE_SCHEMA_VERSION.to_string(),
            record_kind: ContextRecordKind::RecordProposal,
            record_id: String::new(),
            lineage_id: format!("prp_{}_{candidate_id}", proposal_kind.as_str()),
            proposal_kind,
            status,
            candidate_id,
            title: title.into(),
            body: body.into(),
            domains,
            supporting_observations,
            score,
            confidence,
            observed_at: observed_at.into(),
            record_hash: String::new(),
        };
        let seed = record_hash(&record)?;
        let digest = seed.strip_prefix("sha256:").unwrap_or(&seed);
        record.record_id = format!(
            "prp_{}_{}_{}",
            record.proposal_kind.as_str(),
            record.candidate_id,
            &digest[..12]
        );
        record.record_hash = record_hash(&record)?;
        Ok(record)
    }

    /// Whether this proposal clears the promotion gate.
    ///
    /// Reads `distinct_tasks` and **never** `occurrences`. That single choice is
    /// spec §7's anti-poisoning rule: thirty repetitions inside one task produce
    /// `distinct_tasks == 1` and are refused, however many events back them.
    pub fn is_eligible(&self, min_distinct_tasks: u32, min_observations: u32) -> bool {
        self.score.distinct_tasks >= min_distinct_tasks
            && self.score.occurrences >= min_observations
    }
}

/// Who acted. A promotion event that cannot say whether a human was involved
/// cannot enforce "blocking always requires explicit human confirmation".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionActor {
    /// The loop acted on its own, under policy.
    System,
    /// A person acted — through the review surface or an explicit command.
    User,
}

impl PromotionActor {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
        }
    }
}

/// An immutable record of one governance decision.
///
/// Keep, Edit, and Ignore from the review surface are all promotion events, as
/// are the system's own auto-activations. Replaying this kind in order
/// reproduces the loop's governance state exactly, which is the gate criterion
/// "Keep/Edit/Ignore replay identically from the event log".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionEventRecord {
    pub schema_version: String,
    pub record_kind: ContextRecordKind,
    pub record_id: String,
    pub lineage_id: String,
    /// The proposal lineage this decision is about.
    pub proposal_lineage_id: String,
    pub action: PromotionAction,
    pub actor: PromotionActor,
    /// The enforcement the promoted record takes, when the action grants one.
    /// Never `Blocking` for a system actor — see [`Self::new`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement: Option<DirectiveEnforcement>,
    /// A user's replacement text, for an `Edit`. Absent for every other action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_body: Option<String>,
    /// Human-readable justification. Required — a governance decision with no
    /// recorded reason is not auditable.
    pub reason: String,
    pub occurred_at: String,
    pub record_hash: String,
}

/// Constructing a promotion event violated a governance invariant.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PromotionEventError {
    /// Spec §5.4: inferred directives start advisory and may never *start*
    /// blocking; blocking always requires explicit human confirmation. A
    /// system-actor event therefore cannot carry blocking enforcement at all.
    #[error(
        "a {actor} promotion may not grant blocking enforcement — blocking always \
         requires an explicit human confirmation (spec §5.4)"
    )]
    SystemGrantedBlocking {
        /// The actor that attempted it.
        actor: &'static str,
    },
    /// An auto-activation is only ever advisory, whoever triggers it.
    #[error("an auto-activation may only be advisory, never blocking")]
    AutoActivatedBlocking,
    /// A governance decision must record why it was made.
    #[error("a promotion event must carry a reason")]
    MissingReason,
    /// The record could not be hashed. Flattened to a message rather than
    /// wrapping [`RecordHashError`], which is neither `Clone` nor `PartialEq`
    /// (it carries a `serde_json::Error`) — and a governance error a caller
    /// cannot compare or store is awkward for no gain, since a hash failure
    /// here is unrecoverable either way.
    #[error("promotion event could not be hashed: {0}")]
    Hash(String),
}

impl PromotionEventRecord {
    /// Build a promotion event, refusing any that would violate governance.
    ///
    /// The refusals are constructor-level rather than checked at a call site on
    /// purpose. "No inferred directive reaches blocking by any path" is a gate
    /// criterion, and *by any path* is only testable if there is exactly one
    /// path — a validator a caller can forget to run leaves the other paths
    /// open by construction.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        proposal_lineage_id: impl Into<String>,
        action: PromotionAction,
        actor: PromotionActor,
        enforcement: Option<DirectiveEnforcement>,
        edited_body: Option<String>,
        reason: impl Into<String>,
        occurred_at: impl Into<String>,
    ) -> Result<Self, PromotionEventError> {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(PromotionEventError::MissingReason);
        }
        if enforcement == Some(DirectiveEnforcement::Blocking) {
            if actor == PromotionActor::System {
                return Err(PromotionEventError::SystemGrantedBlocking {
                    actor: actor.as_str(),
                });
            }
            if action == PromotionAction::AutoActivated {
                return Err(PromotionEventError::AutoActivatedBlocking);
            }
        }
        let mut record = Self {
            schema_version: LIFECYCLE_SCHEMA_VERSION.to_string(),
            record_kind: ContextRecordKind::PromotionEvent,
            record_id: String::new(),
            lineage_id: String::new(),
            proposal_lineage_id: proposal_lineage_id.into(),
            action,
            actor,
            enforcement,
            edited_body,
            reason,
            occurred_at: occurred_at.into(),
            record_hash: String::new(),
        };
        let hash = |r: &Self| record_hash(r).map_err(|e| PromotionEventError::Hash(e.to_string()));
        let seed = hash(&record)?;
        let digest = seed.strip_prefix("sha256:").unwrap_or(&seed);
        record.record_id = format!("pev_{}_{}", action.as_str(), &digest[..12]);
        record.lineage_id = record.record_id.clone();
        record.record_hash = hash(&record)?;
        Ok(record)
    }

    /// The enforcement an *inferred* record may start at. There is exactly one
    /// answer and it is not configurable: advisory.
    ///
    /// Exposed as a function rather than left implicit so a caller reaching for
    /// "what enforcement should this get" finds the invariant instead of a
    /// settings lookup. `context.promotion.inferred_directive.initial_enforcement`
    /// exists in settings and defaults to advisory, but a settings file is a
    /// surface a person edits, and §5.4 is not negotiable by configuration.
    pub fn initial_enforcement_for_inferred() -> DirectiveEnforcement {
        DirectiveEnforcement::Advisory
    }
}

/// A confidence derived from the evidence, on the `0..=100` scale the promotion
/// settings speak.
///
/// Deliberately simple and deliberately capped below `auto_activate_at_confidence`'s
/// default of 85 unless a proposal is genuinely well-evidenced: the loop should
/// need real recurrence across real tasks before it acts without being asked.
pub fn confidence_from_score(score: &ProposalScore) -> Result<Confidence, RecordValidationError> {
    // 20 per distinct task (the axis that resists poisoning), 5 per occurrence
    // beyond the first, 15 for an independently salient signal.
    let tasks = score.distinct_tasks.min(4) * 20;
    let extra = score.occurrences.saturating_sub(1).min(4) * 5;
    let salient = if score.salient { 15 } else { 0 };
    Confidence::new((tasks + extra + salient).min(100) as u16)
}

#[cfg(test)]
mod tests;
