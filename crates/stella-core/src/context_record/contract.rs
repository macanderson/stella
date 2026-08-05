//! Artifact-contract and contract-validation value types (lifecycle §8.12–8.13).
//!
//! Two spec points are handled per the flagged-decisions issue (#483) with a
//! documented, non-freezing choice: **requirement kinds and the validation
//! `method` are extensible strings**, not closed enums (the spec never names the
//! semantic-verifier method token, and requirement kinds are explicitly extensible
//! with "unknown kinds fail closed"). `validation_status` keeps its four named
//! values; `requirement_status` gets its own enum.
//!
//! The §8.13 coverage rule — "exactly one result for every requirement in the
//! referenced contract version, with no duplicate or unknown requirement IDs;
//! missing, duplicate, or unknown result IDs make validation_status error" —
//! spans two records, but it is still **pure**: it needs only the contract value
//! beside the validation value, never a repository. It therefore lives here as
//! [`ContractValidation::validate_against`], with the id-only half a validation
//! can check alone in [`ContractValidation::validate`].

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::RecordValidationError;
use super::context_use::EvidenceLink;
use super::kind::Origin;
use super::scope::{Scope, SharingScope};

/// A requirement kind. Extensible: the ten recognized core kinds are exposed as
/// constants; an unrecognized kind is non-executable and fails closed when
/// required (enforced by the executor in a later phase).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct RequirementKind(String);

impl RequirementKind {
    /// A file exists at a path.
    pub const FILE_EXISTS: &'static str = "file_exists";
    /// A directory exists at a path.
    pub const DIRECTORY_EXISTS: &'static str = "directory_exists";
    /// At least N files match a glob.
    pub const GLOB_MIN_COUNT: &'static str = "glob_min_count";
    /// A file has an expected MIME type.
    pub const MIME_TYPE: &'static str = "mime_type";
    /// An image has expected dimensions.
    pub const IMAGE_DIMENSIONS: &'static str = "image_dimensions";
    /// A file is within a size bound.
    pub const FILE_SIZE: &'static str = "file_size";
    /// A JSON document matches a schema.
    pub const JSON_SCHEMA: &'static str = "json_schema";
    /// A Markdown document has required sections.
    pub const MARKDOWN_SECTIONS: &'static str = "markdown_sections";
    /// A command exits successfully (needs `execution_approval_ref`).
    pub const COMMAND: &'static str = "command";
    /// A semantic verifier scores against a rubric.
    pub const SEMANTIC_VERIFIER: &'static str = "semantic_verifier";

    const RECOGNIZED: [&'static str; 10] = [
        Self::FILE_EXISTS,
        Self::DIRECTORY_EXISTS,
        Self::GLOB_MIN_COUNT,
        Self::MIME_TYPE,
        Self::IMAGE_DIMENSIONS,
        Self::FILE_SIZE,
        Self::JSON_SCHEMA,
        Self::MARKDOWN_SECTIONS,
        Self::COMMAND,
        Self::SEMANTIC_VERIFIER,
    ];

    /// Construct a requirement kind, rejecting an empty identifier.
    pub fn new(identifier: impl Into<String>) -> Result<Self, RecordValidationError> {
        let identifier = identifier.into();
        if identifier.is_empty() {
            return Err(RecordValidationError::invariant(
                "requirement_kind.must_be_nonempty",
            ));
        }
        Ok(Self(identifier))
    }

    /// The identifier string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is one of the ten recognized core kinds. An unrecognized
    /// kind is non-executable (fails closed when required).
    pub fn is_recognized(&self) -> bool {
        Self::RECOGNIZED.contains(&self.0.as_str())
    }

    /// Whether this is the `command` kind (which forces
    /// `execution_approval_ref` on the contract).
    pub fn is_command(&self) -> bool {
        self.0 == Self::COMMAND
    }
}

impl<'de> Deserialize<'de> for RequirementKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let identifier = String::deserialize(deserializer)?;
        RequirementKind::new(identifier).map_err(serde::de::Error::custom)
    }
}

/// One requirement of an [`ArtifactContract`]. Kind-specific fields
/// (`path`, `glob`/`minimum`, `argv`/`timeout_ms`, `rubric_ref`, …) sit
/// **inline beside** the three common fields on the wire (lifecycle §8.12), so
/// `params` is `#[serde(flatten)]`ed rather than nested — the canonical record
/// bytes must match the spec's shape exactly, since they enter the `record_hash`
/// preimage. The executor interprets them in a later phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    /// Stable id, unique within the contract.
    pub requirement_id: String,
    /// The requirement kind.
    pub requirement_kind: RequirementKind,
    /// Whether the deliverable must satisfy this to be complete.
    pub required: bool,
    /// Kind-specific fields, inline on the wire (opaque here).
    #[serde(flatten)]
    pub params: serde_json::Map<String, serde_json::Value>,
}

/// The selection predicate for a contract — when it applies (lifecycle §8.12).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AppliesWhen {
    /// Task intents this contract applies to.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub task_intents: Vec<String>,
}

/// Presentation hints for a contract's output (lifecycle §8.12). Advisory: it
/// orders how a satisfied deliverable is shown, never what is required.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Presentation {
    /// The order output directories are presented in.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub directory_order: Vec<String>,
}

/// A contract that a produced artifact must satisfy (lifecycle §8.12).
///
/// A contract is **data, never execution authorization**: `execution_approval_ref`
/// is a required *reference* for a `command` requirement, and resolving it is a
/// separate authorization step that this type layer does not perform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactContract {
    /// Human name.
    pub name: String,
    /// Contract version — an integer on the wire (lifecycle §8.12), so the
    /// canonical bytes are `"version":3`, never `"version":"3"`.
    pub version: u32,
    /// What it produces / checks.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub description: Option<String>,
    /// Provenance.
    pub origin: Origin,
    /// Where it applies.
    pub scope: Scope,
    /// Who it is shared with.
    pub sharing_scope: SharingScope,
    /// When this contract applies.
    #[serde(default)]
    pub applies_when: AppliesWhen,
    /// Root under which outputs are checked.
    pub output_root: String,
    /// The requirements.
    #[serde(default)]
    pub requirements: Vec<Requirement>,
    /// Presentation hints for the produced output.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub presentation: Option<Presentation>,
    /// Required when any requirement is `command`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub execution_approval_ref: Option<String>,
    /// When it was observed (RFC 3339 UTC).
    pub observed_at: String,
    /// Validity start (RFC 3339 UTC).
    pub valid_from: String,
}

impl ArtifactContract {
    /// Validate intra-record invariants: `requirement_id` is unique within the
    /// contract, and a `command` requirement forces an `execution_approval_ref`.
    pub fn validate(&self) -> Result<(), RecordValidationError> {
        let mut seen = HashSet::new();
        if self
            .requirements
            .iter()
            .any(|r| !seen.insert(r.requirement_id.as_str()))
        {
            return Err(RecordValidationError::invariant(
                "contract.requirement_ids_must_be_unique",
            ));
        }
        let has_command = self
            .requirements
            .iter()
            .any(|r| r.requirement_kind.is_command());
        if has_command && self.execution_approval_ref.is_none() {
            return Err(RecordValidationError::invariant(
                "contract.command_requires_execution_approval_ref",
            ));
        }
        Ok(())
    }

    /// The ids of `required` requirements whose kind is **not** recognized.
    /// Such a requirement is non-executable, so it fails closed: it can never be
    /// reported `passed` (lifecycle §8.12, enforced by
    /// [`ContractValidation::validate_against`]).
    pub fn non_executable_required_ids(&self) -> Vec<&str> {
        self.requirements
            .iter()
            .filter(|r| r.required && !r.requirement_kind.is_recognized())
            .map(|r| r.requirement_id.as_str())
            .collect()
    }
}

/// The verdict of a whole contract validation (lifecycle §8.13). Four named
/// values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    Passed,
    Failed,
    Error,
    Skipped,
}

impl ValidationStatus {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Error => "error",
            Self::Skipped => "skipped",
        }
    }
}

/// The verdict for a single requirement. Its own enum (per #483): `error` is a
/// validation-level aggregate, not a per-requirement verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementStatus {
    Passed,
    Failed,
    Skipped,
}

impl RequirementStatus {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

/// One requirement's result within a [`ContractValidation`]. `method` is an
/// extensible identifier (e.g. `deterministic`, or a semantic-verifier token).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementResult {
    /// The requirement this result is for.
    pub requirement_id: String,
    /// The per-requirement verdict.
    pub requirement_status: RequirementStatus,
    /// How it was checked (extensible identifier).
    pub method: String,
    /// Optional human message.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub message: Option<String>,
}

/// A validation of an [`ArtifactContract`] (lifecycle §8.13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractValidation {
    /// The contract this validated.
    pub contract_record_id: String,
    /// The exact contract version validated (integer, matching
    /// [`ArtifactContract::version`]).
    pub contract_version: u32,
    /// The hash of the exact contract revision validated.
    pub contract_hash: String,
    /// The overall verdict.
    pub validation_status: ValidationStatus,
    /// Per-requirement results.
    #[serde(default)]
    pub results: Vec<RequirementResult>,
    /// Evidence supporting the verdict.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub evidence_links: Vec<EvidenceLink>,
    /// Where the validation applies.
    pub scope: Scope,
    /// Who it is shared with.
    pub sharing_scope: SharingScope,
    /// When it was observed (RFC 3339 UTC).
    pub observed_at: String,
}

impl ContractValidation {
    /// The half of the §8.13 coverage rule a validation can check **alone**:
    /// duplicate result ids force `validation_status == error`. Missing and
    /// unknown ids need the contract — see [`Self::validate_against`].
    pub fn validate(&self) -> Result<(), RecordValidationError> {
        let mut seen = HashSet::new();
        let has_duplicate = self
            .results
            .iter()
            .any(|r| !seen.insert(r.requirement_id.as_str()));
        if has_duplicate && self.validation_status != ValidationStatus::Error {
            return Err(RecordValidationError::invariant(
                "contract_validation.duplicate_result_requires_error",
            ));
        }
        Ok(())
    }

    /// The **whole** §8.13 coverage rule, against the contract this validation
    /// references: exactly one result for every requirement, with no duplicate
    /// or unknown requirement ids. Missing, duplicate, or unknown result ids
    /// make `validation_status` `error` — any other status with broken coverage
    /// is invalid.
    ///
    /// Also enforces fail-closed non-executability: a `required` requirement
    /// whose kind is unrecognized cannot be reported `passed`, and its presence
    /// keeps the whole validation from passing (lifecycle §8.12).
    ///
    /// Pure: it compares two values and touches no repository. The caller is
    /// responsible for supplying the contract revision that
    /// `contract_record_id`/`contract_hash` actually name.
    pub fn validate_against(
        &self,
        contract: &ArtifactContract,
    ) -> Result<(), RecordValidationError> {
        if self.contract_version != contract.version {
            return Err(RecordValidationError::invariant(
                "contract_validation.version_must_match_contract",
            ));
        }

        let required_ids: HashSet<&str> = contract
            .requirements
            .iter()
            .map(|r| r.requirement_id.as_str())
            .collect();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut coverage_broken = false;
        for result in &self.results {
            let id = result.requirement_id.as_str();
            // Duplicate, or naming a requirement the contract does not have.
            if !seen.insert(id) || !required_ids.contains(id) {
                coverage_broken = true;
            }
        }
        // Missing: a requirement with no result at all.
        if seen.len() != required_ids.len() {
            coverage_broken = true;
        }
        if coverage_broken && self.validation_status != ValidationStatus::Error {
            return Err(RecordValidationError::invariant(
                "contract_validation.broken_coverage_requires_error",
            ));
        }

        let non_executable = contract.non_executable_required_ids();
        if !non_executable.is_empty() {
            let passed_non_executable = self.results.iter().any(|r| {
                r.requirement_status == RequirementStatus::Passed
                    && non_executable.contains(&r.requirement_id.as_str())
            });
            if passed_non_executable {
                return Err(RecordValidationError::invariant(
                    "contract_validation.unrecognized_required_kind_cannot_pass",
                ));
            }
            if self.validation_status == ValidationStatus::Passed {
                return Err(RecordValidationError::invariant(
                    "contract_validation.non_executable_requirement_fails_closed",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn contract(kinds: &[&str]) -> ArtifactContract {
        ArtifactContract {
            name: "brand-kit".into(),
            version: 1,
            description: None,
            origin: Origin::User,
            scope: Scope {
                repository_id: Some("repo_1".into()),
                ..Default::default()
            },
            sharing_scope: SharingScope::Repository,
            applies_when: AppliesWhen {
                task_intents: vec!["create_brand_kit".into()],
            },
            output_root: "out/".into(),
            requirements: kinds
                .iter()
                .enumerate()
                .map(|(i, k)| Requirement {
                    requirement_id: format!("req_{i}"),
                    requirement_kind: RequirementKind::new(*k).unwrap(),
                    required: true,
                    params: serde_json::Map::new(),
                })
                .collect(),
            presentation: None,
            execution_approval_ref: None,
            observed_at: "2026-07-20T18:30:00Z".into(),
            valid_from: "2026-07-20T18:30:00Z".into(),
        }
    }

    /// A validation covering every requirement of `contract`, all passed.
    fn validation_for(contract: &ArtifactContract) -> ContractValidation {
        ContractValidation {
            contract_record_id: "ctr_1".into(),
            contract_version: contract.version,
            contract_hash: "sha256:contract".into(),
            validation_status: ValidationStatus::Passed,
            results: contract
                .requirements
                .iter()
                .map(|r| RequirementResult {
                    requirement_id: r.requirement_id.clone(),
                    requirement_status: RequirementStatus::Passed,
                    method: "deterministic".into(),
                    message: None,
                })
                .collect(),
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
    fn requirement_kind_recognition_and_extensibility() {
        assert!(RequirementKind::new("file_exists").unwrap().is_recognized());
        assert!(RequirementKind::new("command").unwrap().is_command());
        assert!(!RequirementKind::new("acme.custom").unwrap().is_recognized());
        assert!(RequirementKind::new("").is_err());
    }

    #[test]
    fn command_requirement_forces_execution_approval_ref() {
        let mut c = contract(&["file_exists", "command"]);
        assert_eq!(
            c.validate(),
            Err(RecordValidationError::invariant(
                "contract.command_requires_execution_approval_ref"
            ))
        );
        c.execution_approval_ref = Some("approval_1".into());
        assert!(c.validate().is_ok());
        // No command → no approval needed.
        assert!(contract(&["file_exists", "json_schema"]).validate().is_ok());
    }

    #[test]
    fn duplicate_requirement_ids_are_rejected() {
        let mut c = contract(&["file_exists", "file_exists"]);
        c.requirements[1].requirement_id = "req_0".into();
        assert_eq!(
            c.validate(),
            Err(RecordValidationError::invariant(
                "contract.requirement_ids_must_be_unique"
            ))
        );
    }

    #[test]
    fn duplicate_result_ids_force_error_status() {
        let c = contract(&["file_exists"]);
        let dup = |status| {
            let mut v = validation_for(&c);
            v.validation_status = status;
            v.results.push(RequirementResult {
                requirement_id: "req_0".into(),
                requirement_status: RequirementStatus::Failed,
                method: "deterministic".into(),
                message: None,
            });
            v
        };
        assert_eq!(
            dup(ValidationStatus::Passed).validate(),
            Err(RecordValidationError::invariant(
                "contract_validation.duplicate_result_requires_error"
            ))
        );
        assert!(dup(ValidationStatus::Error).validate().is_ok());
    }

    #[test]
    fn coverage_requires_exactly_one_result_per_requirement() {
        let c = contract(&["file_exists", "json_schema"]);
        // The complete, correct covering validation passes.
        assert!(validation_for(&c).validate_against(&c).is_ok());

        // Missing a result for req_1.
        let mut missing = validation_for(&c);
        missing.results.pop();
        assert_eq!(
            missing.validate_against(&c),
            Err(RecordValidationError::invariant(
                "contract_validation.broken_coverage_requires_error"
            ))
        );
        // ...unless the status already admits the breakage.
        missing.validation_status = ValidationStatus::Error;
        assert!(missing.validate_against(&c).is_ok());

        // A result naming a requirement the contract does not have.
        let mut unknown = validation_for(&c);
        unknown.results[1].requirement_id = "req_does_not_exist".into();
        assert_eq!(
            unknown.validate_against(&c),
            Err(RecordValidationError::invariant(
                "contract_validation.broken_coverage_requires_error"
            ))
        );

        // A duplicate that also leaves a requirement uncovered.
        let mut duplicate = validation_for(&c);
        duplicate.results[1].requirement_id = "req_0".into();
        assert_eq!(
            duplicate.validate_against(&c),
            Err(RecordValidationError::invariant(
                "contract_validation.broken_coverage_requires_error"
            ))
        );
    }

    #[test]
    fn a_validation_must_name_the_contract_version_it_validated() {
        let c = contract(&["file_exists"]);
        let mut v = validation_for(&c);
        v.contract_version = 99;
        assert_eq!(
            v.validate_against(&c),
            Err(RecordValidationError::invariant(
                "contract_validation.version_must_match_contract"
            ))
        );
    }

    #[test]
    fn an_unrecognized_required_kind_fails_closed() {
        let c = contract(&["acme.custom"]);
        assert_eq!(c.non_executable_required_ids(), vec!["req_0"]);

        // It can never be reported passed.
        let v = validation_for(&c);
        assert_eq!(
            v.validate_against(&c),
            Err(RecordValidationError::invariant(
                "contract_validation.unrecognized_required_kind_cannot_pass"
            ))
        );

        // Even reported failed, the whole validation cannot pass.
        let mut failed_requirement = validation_for(&c);
        failed_requirement.results[0].requirement_status = RequirementStatus::Failed;
        assert_eq!(
            failed_requirement.validate_against(&c),
            Err(RecordValidationError::invariant(
                "contract_validation.non_executable_requirement_fails_closed"
            ))
        );
        failed_requirement.validation_status = ValidationStatus::Failed;
        assert!(failed_requirement.validate_against(&c).is_ok());

        // An unrecognized kind that is NOT required does not fail closed.
        let mut optional = contract(&["acme.custom"]);
        optional.requirements[0].required = false;
        assert!(optional.non_executable_required_ids().is_empty());
        assert!(
            validation_for(&optional)
                .validate_against(&optional)
                .is_ok()
        );
    }

    #[test]
    fn requirement_kind_fields_are_inline_on_the_wire() {
        // The spec puts kind-specific fields beside the common ones, not nested
        // under `params` — the canonical bytes must match.
        let mut c = contract(&["file_exists"]);
        c.requirements[0]
            .params
            .insert("path".into(), json!("README.md"));
        let wire = serde_json::to_value(&c.requirements[0]).unwrap();
        assert_eq!(
            wire,
            json!({
                "requirement_id": "req_0",
                "requirement_kind": "file_exists",
                "required": true,
                "path": "README.md"
            })
        );
        let back: Requirement = serde_json::from_value(wire).unwrap();
        assert_eq!(back, c.requirements[0]);
    }

    #[test]
    fn contract_version_is_an_integer_on_the_wire() {
        let wire = serde_json::to_value(contract(&["file_exists"])).unwrap();
        assert_eq!(wire["version"], json!(1));
        assert_eq!(
            wire["applies_when"],
            json!({"task_intents": ["create_brand_kit"]})
        );
        // Absent presentation is omitted, not null.
        assert!(wire.get("presentation").is_none());
    }

    #[test]
    fn status_strings_are_canonical() {
        assert_eq!(
            serde_json::to_value(ValidationStatus::Error).unwrap(),
            json!("error")
        );
        assert_eq!(
            serde_json::to_value(RequirementStatus::Skipped).unwrap(),
            json!("skipped")
        );
    }
}
