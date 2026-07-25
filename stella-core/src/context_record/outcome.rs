//! Outcome-assessment value types (lifecycle §6.8 / §8.14). Two independent
//! dimensions — completion and correctness — each carrying a status and the
//! evidential level behind it.
//!
//! The completion dimension is not free-standing when a contract governs the
//! task: §8.13 ("completion cannot pass") and §8.12 ("do not declare completion
//! until the artifact contract passes") tie it to the contract verdict. That
//! cross-record rule is pure — two values, no repository — and lives here as
//! [`OutcomeAssessment::validate_against`].

use serde::{Deserialize, Serialize};

use super::RecordValidationError;
use super::context_use::EvidenceLink;
use super::contract::{ContractValidation, ValidationStatus};
use super::scope::{Scope, SharingScope};

/// The evidential strength behind an assessment (lifecycle §6.8). Agent
/// reflection alone supports only `inferred`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeAssessmentLevel {
    Verified,
    UserConfirmed,
    ExternallyConfirmed,
    Inferred,
    Unknown,
}

impl OutcomeAssessmentLevel {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::UserConfirmed => "user_confirmed",
            Self::ExternallyConfirmed => "externally_confirmed",
            Self::Inferred => "inferred",
            Self::Unknown => "unknown",
        }
    }
}

/// Whether a task's deliverable was completed (lifecycle §).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionStatus {
    Complete,
    Incomplete,
    Unknown,
}

impl CompletionStatus {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
            Self::Unknown => "unknown",
        }
    }
}

/// Whether a task's deliverable was correct (lifecycle §). Independent of
/// [`CompletionStatus`] — a task can be complete but incorrect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectnessStatus {
    Correct,
    Incorrect,
    Unknown,
}

impl CorrectnessStatus {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Correct => "correct",
            Self::Incorrect => "incorrect",
            Self::Unknown => "unknown",
        }
    }
}

/// One assessed dimension: a status plus the level of evidence behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionAssessment {
    /// The completion verdict.
    pub status: CompletionStatus,
    /// The evidence level.
    pub assessment_level: OutcomeAssessmentLevel,
}

/// One assessed dimension: a status plus the level of evidence behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectnessAssessment {
    /// The correctness verdict.
    pub status: CorrectnessStatus,
    /// The evidence level.
    pub assessment_level: OutcomeAssessmentLevel,
}

/// Why an outcome was assessed the way it was (lifecycle §8.14). `reason_kind`
/// is an extensible identifier (e.g. `contract_failure`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeReason {
    /// The reason kind (extensible identifier).
    pub reason_kind: String,
    /// The record this reason points at (e.g. a `ContractValidation`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub record_ref: Option<String>,
}

impl OutcomeReason {
    /// The reason kind naming a contract validation that did not pass.
    pub const CONTRACT_FAILURE: &'static str = "contract_failure";
}

/// A post-task assessment of an outcome across the two independent dimensions
/// (lifecycle §8.14). Referenced by a `ContextUseFeedback` when the outcome
/// relation is supported/contradicted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeAssessment {
    /// The task assessed.
    pub task_id: String,
    /// The completion dimension.
    pub completion_assessment: CompletionAssessment,
    /// The correctness dimension.
    pub correctness_assessment: CorrectnessAssessment,
    /// Why it was assessed this way.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub reasons: Vec<OutcomeReason>,
    /// Evidence supporting the assessment.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub evidence_links: Vec<EvidenceLink>,
    /// Where the assessment applies.
    pub scope: Scope,
    /// Who it is shared with.
    pub sharing_scope: SharingScope,
    /// When it was observed (RFC 3339 UTC).
    pub observed_at: String,
}

impl OutcomeAssessment {
    /// Completion cannot pass on a contract that did not (lifecycle §8.13:
    /// broken coverage "make[s] validation_status error, and completion cannot
    /// pass"; §8.12: "do not declare completion until the artifact contract
    /// passes").
    ///
    /// `error` and `failed` both block a `complete` verdict. `skipped` does not:
    /// a contract that did not apply says nothing about completion. Correctness
    /// is untouched — the two dimensions stay independent.
    pub fn validate_against(
        &self,
        validation: &ContractValidation,
    ) -> Result<(), RecordValidationError> {
        let blocks_completion = matches!(
            validation.validation_status,
            ValidationStatus::Error | ValidationStatus::Failed
        );
        if blocks_completion && self.completion_assessment.status == CompletionStatus::Complete {
            return Err(RecordValidationError::invariant(
                "outcome.completion_cannot_pass_an_unpassed_contract",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn level_and_status_strings_are_canonical() {
        assert_eq!(
            serde_json::to_value(OutcomeAssessmentLevel::ExternallyConfirmed).unwrap(),
            json!("externally_confirmed")
        );
        assert_eq!(
            serde_json::to_value(OutcomeAssessmentLevel::UserConfirmed).unwrap(),
            json!("user_confirmed")
        );
        assert_eq!(
            serde_json::to_value(CompletionStatus::Incomplete).unwrap(),
            json!("incomplete")
        );
        assert_eq!(
            serde_json::to_value(CorrectnessStatus::Incorrect).unwrap(),
            json!("incorrect")
        );
    }

    fn assessment(completion: CompletionStatus) -> OutcomeAssessment {
        OutcomeAssessment {
            task_id: "task_1".into(),
            completion_assessment: CompletionAssessment {
                status: completion,
                assessment_level: OutcomeAssessmentLevel::Verified,
            },
            correctness_assessment: CorrectnessAssessment {
                status: CorrectnessStatus::Incorrect,
                assessment_level: OutcomeAssessmentLevel::Inferred,
            },
            reasons: Vec::new(),
            evidence_links: Vec::new(),
            scope: Scope {
                repository_id: Some("repo_1".into()),
                ..Default::default()
            },
            sharing_scope: SharingScope::Repository,
            observed_at: "2026-07-20T18:30:00Z".into(),
        }
    }

    fn validation(status: ValidationStatus) -> ContractValidation {
        ContractValidation {
            contract_record_id: "ctr_1".into(),
            contract_version: 1,
            contract_hash: "sha256:contract".into(),
            validation_status: status,
            results: Vec::new(),
            evidence_links: Vec::new(),
            scope: Scope {
                repository_id: Some("repo_1".into()),
                ..Default::default()
            },
            sharing_scope: SharingScope::Repository,
            observed_at: "2026-07-20T18:30:00Z".into(),
        }
    }

    #[test]
    fn the_two_dimensions_are_independent() {
        // Complete but incorrect is a representable, valid combination.
        let a = assessment(CompletionStatus::Complete);
        let round: OutcomeAssessment =
            serde_json::from_value(serde_json::to_value(&a).unwrap()).unwrap();
        assert_eq!(round, a);
    }

    #[test]
    fn completion_cannot_pass_an_unpassed_contract() {
        for blocking in [ValidationStatus::Error, ValidationStatus::Failed] {
            assert_eq!(
                assessment(CompletionStatus::Complete).validate_against(&validation(blocking)),
                Err(RecordValidationError::invariant(
                    "outcome.completion_cannot_pass_an_unpassed_contract"
                )),
                "{blocking:?} must block a complete verdict"
            );
            // Not complete is fine against the same verdict.
            assert!(
                assessment(CompletionStatus::Incomplete)
                    .validate_against(&validation(blocking))
                    .is_ok()
            );
            assert!(
                assessment(CompletionStatus::Unknown)
                    .validate_against(&validation(blocking))
                    .is_ok()
            );
        }
        // A passed contract permits completion; a skipped one says nothing.
        for permitted in [ValidationStatus::Passed, ValidationStatus::Skipped] {
            assert!(
                assessment(CompletionStatus::Complete)
                    .validate_against(&validation(permitted))
                    .is_ok(),
                "{permitted:?} must not block completion"
            );
        }
    }

    #[test]
    fn correctness_is_unconstrained_by_the_contract_verdict() {
        // The dimensions stay independent: a passing contract does not make the
        // outcome correct, and an incorrect outcome is still representable.
        let mut a = assessment(CompletionStatus::Complete);
        a.correctness_assessment.status = CorrectnessStatus::Incorrect;
        assert!(
            a.validate_against(&validation(ValidationStatus::Passed))
                .is_ok()
        );
    }
}
