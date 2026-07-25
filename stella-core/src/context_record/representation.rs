//! Frame representation and content-fidelity value types (lifecycle §10.1–10.3).
//! Three **separate** enums — do not collapse them. These complete the Phase 1
//! type layer that Phase 4 (compaction) wires into the compiler.
//!
//! Per the flagged-decisions issue (#483): the "blocking or **guarded** directive
//! requires exact minimum fidelity" invariant is implemented only for `blocking`
//! — both spec extractions agree `guarded` is a fidelity-policy category (a
//! "guarded rule" defaults to exact), not an enforcement value. No `guarded`
//! enforcement variant is introduced; the guarded-rule default policy is Phase 4.

use serde::{Deserialize, Serialize};

use super::RecordValidationError;
use super::kind::DirectiveEnforcement;

/// How a frame carries its content (lifecycle §10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Representation {
    /// Complete inline content (the legacy default).
    #[default]
    Full,
    /// Shorter inline content linked to the canonical source.
    Compact,
    /// A stable reference + hash, no inline content.
    Reference,
}

impl Representation {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Compact => "compact",
            Self::Reference => "reference",
        }
    }
}

/// The fidelity of a frame's content to its canonical source (lifecycle §10.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentFidelity {
    /// Byte/order exact — cannot be paraphrased.
    Exact,
    /// Lossless canonical reformatting.
    Normalized,
    /// A faithful shorter representation.
    Summarized,
    /// No inline content (used by `reference`).
    Omitted,
}

impl ContentFidelity {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Normalized => "normalized",
            Self::Summarized => "summarized",
            Self::Omitted => "omitted",
        }
    }
}

/// The minimum acceptable fidelity for an item (lifecycle §10.3). A **distinct**
/// enum from [`ContentFidelity`]: `omitted` is not a legal minimum, so the type
/// makes it unrepresentable (no validator needed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MinimumContentFidelity {
    Exact,
    Normalized,
    Summarized,
}

impl MinimumContentFidelity {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Normalized => "normalized",
            Self::Summarized => "summarized",
        }
    }
}

/// Whether inline content is required or a resolvable reference is allowed
/// (lifecycle §10.3). An availability choice, not a fidelity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InlineContentRequirement {
    Required,
    ResolvableReferenceAllowed,
}

impl InlineContentRequirement {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::ResolvableReferenceAllowed => "resolvable_reference_allowed",
        }
    }
}

/// The content-carrying fields of a frame, for validating the representation
/// requirement matrix (lifecycle §10.2 table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameContentSpec {
    /// How content is carried.
    pub representation: Representation,
    /// Inline content (absent for `reference`; never an empty placeholder).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub content: Option<String>,
    /// The fidelity of the carried content.
    pub content_fidelity: ContentFidelity,
    /// Hash of the inline content as carried (absent for `reference`, which
    /// carries none).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub content_hash: Option<String>,
    /// Hash of the canonical source content. Required for every representation
    /// — it is what makes a compacted or referenced item verifiable.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub canonical_content_hash: Option<String>,
    /// A resolvable reference to the canonical content.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub content_ref: Option<String>,
    /// The transform applied (required for `compact`, absent otherwise).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub transform: Option<String>,
}

impl FrameContentSpec {
    /// Validate the §10.2 representation requirement matrix — **both** its
    /// *required* and its *omitted* cells — plus the "a reference representation
    /// has no inline-content placeholder" rule.
    ///
    /// | property | full | compact | reference |
    /// |---|---|---|---|
    /// | `content_fidelity` | exact | exact/normalized/summarized | omitted |
    /// | `content` | required | required | omitted |
    /// | `content_hash` | required | required | omitted |
    /// | `canonical_content_hash` | required | required | required |
    /// | `content_ref` | optional | required | required |
    /// | `transform` | omitted | required | omitted |
    pub fn validate(&self) -> Result<(), RecordValidationError> {
        let inv = RecordValidationError::invariant;
        // Required for every representation.
        if self.canonical_content_hash.is_none() {
            return Err(inv("representation.requires_canonical_content_hash"));
        }
        match self.representation {
            Representation::Full => {
                if self.content_fidelity != ContentFidelity::Exact {
                    return Err(inv("representation.full_requires_exact_fidelity"));
                }
                if self.content.is_none() {
                    return Err(inv("representation.full_requires_inline_content"));
                }
                if self.content_hash.is_none() {
                    return Err(inv("representation.full_requires_content_hash"));
                }
                if self.transform.is_some() {
                    return Err(inv("representation.full_forbids_transform"));
                }
            }
            Representation::Compact => {
                if self.content_fidelity == ContentFidelity::Omitted {
                    return Err(inv("representation.compact_fidelity_not_omitted"));
                }
                if self.content.is_none() {
                    return Err(inv("representation.compact_requires_inline_content"));
                }
                if self.content_hash.is_none() {
                    return Err(inv("representation.compact_requires_content_hash"));
                }
                if self.content_ref.is_none() {
                    return Err(inv("representation.compact_requires_content_ref"));
                }
                if self.transform.is_none() {
                    return Err(inv("representation.compact_requires_transform"));
                }
            }
            Representation::Reference => {
                // Never encode a reference as an empty inline content string.
                if self.content.is_some() {
                    return Err(inv("representation.reference_forbids_inline_content"));
                }
                if self.content_fidelity != ContentFidelity::Omitted {
                    return Err(inv("representation.reference_requires_omitted_fidelity"));
                }
                if self.content_hash.is_some() {
                    return Err(inv("representation.reference_forbids_content_hash"));
                }
                if self.content_ref.is_none() {
                    return Err(inv("representation.reference_requires_content_ref"));
                }
                if self.transform.is_some() {
                    return Err(inv("representation.reference_forbids_transform"));
                }
            }
        }
        Ok(())
    }
}

/// A blocking directive requires an exact minimum fidelity (lifecycle §10.3
/// default-policy table). See the module docs for the deferred `guarded` case.
pub fn validate_directive_minimum_fidelity(
    enforcement: DirectiveEnforcement,
    minimum: MinimumContentFidelity,
) -> Result<(), RecordValidationError> {
    if enforcement == DirectiveEnforcement::Blocking && minimum != MinimumContentFidelity::Exact {
        return Err(RecordValidationError::invariant(
            "directive.blocking_requires_exact_minimum_fidelity",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn representation_missing_on_the_wire_is_full() {
        // Absent representation ⇒ Full (legacy compatibility).
        assert_eq!(Representation::default(), Representation::Full);
        assert_eq!(
            serde_json::to_value(Representation::Reference).unwrap(),
            json!("reference")
        );
    }

    #[test]
    fn minimum_fidelity_cannot_be_omitted() {
        // `omitted` is not a variant of MinimumContentFidelity — unrepresentable.
        assert!(serde_json::from_value::<MinimumContentFidelity>(json!("omitted")).is_err());
        assert!(serde_json::from_value::<ContentFidelity>(json!("omitted")).is_ok());
    }

    fn full() -> FrameContentSpec {
        FrameContentSpec {
            representation: Representation::Full,
            content: Some("hello".into()),
            content_fidelity: ContentFidelity::Exact,
            content_hash: Some("sha256:bb".into()),
            canonical_content_hash: Some("sha256:aa".into()),
            content_ref: None,
            transform: None,
        }
    }

    fn compact() -> FrameContentSpec {
        FrameContentSpec {
            representation: Representation::Compact,
            content: Some("short".into()),
            content_fidelity: ContentFidelity::Summarized,
            content_hash: Some("sha256:bb".into()),
            canonical_content_hash: Some("sha256:aa".into()),
            content_ref: Some("ref://x".into()),
            transform: Some("truncate".into()),
        }
    }

    fn reference() -> FrameContentSpec {
        FrameContentSpec {
            representation: Representation::Reference,
            content: None,
            content_fidelity: ContentFidelity::Omitted,
            content_hash: None,
            canonical_content_hash: Some("sha256:aa".into()),
            content_ref: Some("ref://x".into()),
            transform: None,
        }
    }

    #[test]
    fn full_representation_matrix() {
        assert!(full().validate().is_ok());
        let mut f = full();
        f.content_fidelity = ContentFidelity::Summarized;
        assert_eq!(
            f.validate(),
            Err(RecordValidationError::invariant(
                "representation.full_requires_exact_fidelity"
            ))
        );
        let mut f = full();
        f.content = None;
        assert_eq!(
            f.validate(),
            Err(RecordValidationError::invariant(
                "representation.full_requires_inline_content"
            ))
        );
        let mut f = full();
        f.content_hash = None;
        assert_eq!(
            f.validate(),
            Err(RecordValidationError::invariant(
                "representation.full_requires_content_hash"
            ))
        );
    }

    #[test]
    fn reference_has_no_inline_content_placeholder() {
        assert!(reference().validate().is_ok());
        // An empty-string content is the exact anti-pattern the rule forbids.
        let mut bad = reference();
        bad.content = Some(String::new());
        assert_eq!(
            bad.validate(),
            Err(RecordValidationError::invariant(
                "representation.reference_forbids_inline_content"
            ))
        );
    }

    #[test]
    fn compact_requires_refs_and_transform() {
        assert!(compact().validate().is_ok());
        let mut no_transform = compact();
        no_transform.transform = None;
        assert_eq!(
            no_transform.validate(),
            Err(RecordValidationError::invariant(
                "representation.compact_requires_transform"
            ))
        );
        let mut no_hash = compact();
        no_hash.content_hash = None;
        assert_eq!(
            no_hash.validate(),
            Err(RecordValidationError::invariant(
                "representation.compact_requires_content_hash"
            ))
        );
        // Compact keeps its fidelity freedom: exact is as legal as summarized.
        let mut exact = compact();
        exact.content_fidelity = ContentFidelity::Exact;
        assert!(exact.validate().is_ok());
    }

    #[test]
    fn the_matrix_omitted_cells_are_enforced_not_just_the_required_ones() {
        // A cell the table marks "omitted" must actually be rejected when
        // present — otherwise half the matrix is unenforced.
        let mut full_with_transform = full();
        full_with_transform.transform = Some("truncate".into());
        assert_eq!(
            full_with_transform.validate(),
            Err(RecordValidationError::invariant(
                "representation.full_forbids_transform"
            ))
        );

        let mut reference_with_transform = reference();
        reference_with_transform.transform = Some("truncate".into());
        assert_eq!(
            reference_with_transform.validate(),
            Err(RecordValidationError::invariant(
                "representation.reference_forbids_transform"
            ))
        );

        let mut reference_with_content_hash = reference();
        reference_with_content_hash.content_hash = Some("sha256:bb".into());
        assert_eq!(
            reference_with_content_hash.validate(),
            Err(RecordValidationError::invariant(
                "representation.reference_forbids_content_hash"
            ))
        );
    }

    #[test]
    fn every_representation_requires_a_canonical_content_hash() {
        for mut spec in [full(), compact(), reference()] {
            let representation = spec.representation;
            spec.canonical_content_hash = None;
            assert_eq!(
                spec.validate(),
                Err(RecordValidationError::invariant(
                    "representation.requires_canonical_content_hash"
                )),
                "{representation:?} must require canonical_content_hash"
            );
        }
    }

    #[test]
    fn blocking_directive_requires_exact_minimum_fidelity() {
        assert_eq!(
            validate_directive_minimum_fidelity(
                DirectiveEnforcement::Blocking,
                MinimumContentFidelity::Summarized
            ),
            Err(RecordValidationError::invariant(
                "directive.blocking_requires_exact_minimum_fidelity"
            ))
        );
        assert!(
            validate_directive_minimum_fidelity(
                DirectiveEnforcement::Blocking,
                MinimumContentFidelity::Exact
            )
            .is_ok()
        );
        // Advisory has no fidelity floor.
        assert!(
            validate_directive_minimum_fidelity(
                DirectiveEnforcement::Advisory,
                MinimumContentFidelity::Summarized
            )
            .is_ok()
        );
    }
}
