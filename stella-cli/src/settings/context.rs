//! `context.*` — adaptive-context lifecycle configuration (Phase 0 scaffold).
//!
//! This block is **entirely inert in Phase 0**: every field deserializes and
//! round-trips, but no code reads it yet. The single value that preserves
//! current behavior is [`LifecycleSettings::enabled`], which defaults to
//! `false`; while it is off, the learning, governance, promotion, efficacy,
//! and retention knobs are ignored. The schema exists now so a later phase can
//! turn the loop on without a settings migration, and so the vocabulary is
//! pinned by round-trip tests.
//!
//! Two dimensions are kept deliberately separate (do not collapse them):
//!
//! * **learning mode** — `off` | `record_only` | `advisory`. `off` disables
//!   mining, proposal induction, and efficacy learning; `record_only` captures
//!   observations, proposals, uses, and outcomes without selecting or promoting
//!   inferred records; `advisory` enables governed inferred *advisory* use.
//! * **governance mode** — `solo` | `team` | `regulated`.
//!
//! Enums are "loud": an unrecognized value is a hard parse error (as with
//! [`crate::settings::Toggle`]), never a silent fallback. Omitted fields fall
//! back to the documented defaults below.

use serde::{Deserialize, Serialize};

/// The `context` block of `settings.json`. All fields default, so `"context":
/// {}` — or an absent block — yields the behavior-preserving defaults
/// (lifecycle disabled, learning off, governance solo).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ContextSettings {
    pub lifecycle: LifecycleSettings,
    pub learning: LearningSettings,
    pub governance: GovernanceSettings,
    pub promotion: PromotionSettings,
    pub efficacy: EfficacySettings,
    pub retention: RetentionSettings,
}

/// Master switch for the whole adaptive-context lifecycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LifecycleSettings {
    /// `false` (default) preserves all pre-adaptive-context behavior: every
    /// other field in the `context` block is ignored while this is off.
    pub enabled: bool,
}

/// How much of the learning loop runs. Orthogonal to [`GovernanceMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LearningMode {
    /// Disables mining, proposal induction, and efficacy learning.
    #[default]
    Off,
    /// Captures observations, proposals, uses, and outcomes without selecting
    /// or promoting inferred records.
    RecordOnly,
    /// Enables governed inferred *advisory* use.
    Advisory,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LearningSettings {
    pub mode: LearningMode,
}

/// Who governs promotions. Orthogonal to [`LearningMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GovernanceMode {
    #[default]
    Solo,
    Team,
    Regulated,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GovernanceSettings {
    pub mode: GovernanceMode,
}

/// The enforcement a directive carries. Only two states exist (`advisory` and
/// `blocking`); an inferred directive may only *start* advisory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InitialEnforcement {
    #[default]
    Advisory,
    Blocking,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PromotionSettings {
    pub inferred_directive: InferredDirectivePromotion,
    pub blocking_directive: BlockingDirectivePromotion,
}

/// Thresholds gating when a set of observations may become an inferred
/// directive. `confidence` values are on the `0..=100` scale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InferredDirectivePromotion {
    pub min_observations: u32,
    pub min_distinct_tasks: u32,
    /// `0..=100`.
    pub auto_activate_at_confidence: u8,
    /// An inferred directive can never *start* blocking; the default and only
    /// sensible value here is `advisory`.
    pub initial_enforcement: InitialEnforcement,
}

impl Default for InferredDirectivePromotion {
    fn default() -> Self {
        Self {
            min_observations: 3,
            min_distinct_tasks: 3,
            auto_activate_at_confidence: 85,
            initial_enforcement: InitialEnforcement::Advisory,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BlockingDirectivePromotion {
    /// A blocking directive always requires an explicit human confirmation;
    /// this is `true` by default and should not be turned off lightly.
    pub requires_explicit_confirmation: bool,
}

impl Default for BlockingDirectivePromotion {
    fn default() -> Self {
        Self {
            requires_explicit_confirmation: true,
        }
    }
}

/// Efficacy-attribution thresholds. `confidence` values are on the `0..=100`
/// scale; `not_helpful_ratio_threshold` is a `0.0..=1.0` ratio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EfficacySettings {
    pub min_attributable_uses: u32,
    pub not_helpful_ratio_threshold: f64,
    /// `0..=100`.
    pub min_attribution_confidence: u8,
    /// `0..=100`.
    pub receipt_display_min_attribution_confidence: u8,
}

impl Default for EfficacySettings {
    fn default() -> Self {
        Self {
            min_attributable_uses: 5,
            not_helpful_ratio_threshold: 0.8,
            min_attribution_confidence: 80,
            receipt_display_min_attribution_confidence: 80,
        }
    }
}

/// How long raw observations, proposals, and inferred directives are retained
/// before review/expiry (in days).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RetentionSettings {
    pub raw_observation_days: u32,
    pub proposal_days: u32,
    pub inferred_directive_review_days: u32,
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            raw_observation_days: 30,
            proposal_days: 30,
            inferred_directive_review_days: 180,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    /// The canonical committed fixture (`.stella/settings.json` is gitignored,
    /// so the canonical example lives as a test fixture).
    const FIXTURE: &str = include_str!("../../tests/fixtures/context_settings.json");

    #[test]
    fn absent_context_block_is_none_and_preserves_behavior() {
        // No `context` key at all — the field is absent, not defaulted-on.
        let s: Settings = serde_json::from_str("{}").expect("empty settings parse");
        assert_eq!(s.context, None, "absent context must stay None");
        // And the workspace default carries no context block either.
        assert_eq!(Settings::default().context, None);
    }

    #[test]
    fn empty_context_block_yields_behavior_preserving_defaults() {
        let s: Settings = serde_json::from_str(r#"{"context":{}}"#).expect("empty context parse");
        let ctx = s.context.expect("context present");
        // The one field that gates behavior: disabled by default.
        assert!(!ctx.lifecycle.enabled, "lifecycle must default disabled");
        assert_eq!(ctx.learning.mode, LearningMode::Off);
        assert_eq!(ctx.governance.mode, GovernanceMode::Solo);
        assert_eq!(ctx, ContextSettings::default());
    }

    #[test]
    fn canonical_fixture_deserializes_to_the_documented_defaults() {
        let s: Settings = serde_json::from_str(FIXTURE).expect("fixture parse");
        let ctx = s.context.expect("fixture has a context block");
        // Disabled-by-default lifecycle is what keeps behavior unchanged.
        assert!(!ctx.lifecycle.enabled);
        assert_eq!(ctx.learning.mode, LearningMode::Off);
        assert_eq!(ctx.governance.mode, GovernanceMode::Solo);
        assert_eq!(ctx.promotion.inferred_directive.min_observations, 3);
        assert_eq!(ctx.promotion.inferred_directive.min_distinct_tasks, 3);
        assert_eq!(
            ctx.promotion.inferred_directive.auto_activate_at_confidence,
            85
        );
        assert_eq!(
            ctx.promotion.inferred_directive.initial_enforcement,
            InitialEnforcement::Advisory
        );
        assert!(
            ctx.promotion
                .blocking_directive
                .requires_explicit_confirmation
        );
        assert_eq!(ctx.efficacy.min_attributable_uses, 5);
        assert_eq!(ctx.efficacy.not_helpful_ratio_threshold, 0.8);
        assert_eq!(ctx.efficacy.min_attribution_confidence, 80);
        assert_eq!(ctx.efficacy.receipt_display_min_attribution_confidence, 80);
        assert_eq!(ctx.retention.raw_observation_days, 30);
        assert_eq!(ctx.retention.proposal_days, 30);
        assert_eq!(ctx.retention.inferred_directive_review_days, 180);
        // The whole block equals the code-level defaults: the fixture and the
        // Default impls cannot silently drift apart.
        assert_eq!(ctx, ContextSettings::default());
    }

    #[test]
    fn context_round_trips_through_json() {
        let original = ContextSettings::default();
        let json = serde_json::to_string(&original).expect("serialize");
        let back: ContextSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, back);
    }

    #[test]
    fn modes_are_loud_on_unknown_values() {
        // A typo'd mode is a hard error, not a silent fallback.
        let err =
            serde_json::from_str::<Settings>(r#"{"context":{"learning":{"mode":"advisary"}}}"#);
        assert!(err.is_err(), "unknown learning mode must fail to parse");
        let err = serde_json::from_str::<Settings>(r#"{"context":{"governance":{"mode":"duo"}}}"#);
        assert!(err.is_err(), "unknown governance mode must fail to parse");
    }

    #[test]
    fn every_key_binds_to_its_field() {
        // Because every field is `#[serde(default)]` and the structs do not
        // `deny_unknown_fields`, a misspelled key would be silently ignored and
        // default to its usual value — so a fixture whose values equal the
        // defaults cannot catch a typo. This test gives EVERY key a DISTINCT
        // non-default value and asserts it reads back, proving each JSON key
        // actually reaches its field (and none reads another's value).
        let json = r#"{"context":{
            "lifecycle":{"enabled":true},
            "learning":{"mode":"record_only"},
            "governance":{"mode":"regulated"},
            "promotion":{
                "inferred_directive":{
                    "min_observations":7,
                    "min_distinct_tasks":4,
                    "auto_activate_at_confidence":42,
                    "initial_enforcement":"blocking"
                },
                "blocking_directive":{"requires_explicit_confirmation":false}
            },
            "efficacy":{
                "min_attributable_uses":9,
                "not_helpful_ratio_threshold":0.25,
                "min_attribution_confidence":70,
                "receipt_display_min_attribution_confidence":60
            },
            "retention":{
                "raw_observation_days":15,
                "proposal_days":45,
                "inferred_directive_review_days":200
            }
        }}"#;
        let s: Settings = serde_json::from_str(json).expect("parse");
        let ctx = s.context.expect("present");

        assert!(ctx.lifecycle.enabled);
        assert_eq!(ctx.learning.mode, LearningMode::RecordOnly);
        assert_eq!(ctx.governance.mode, GovernanceMode::Regulated);

        let inf = &ctx.promotion.inferred_directive;
        assert_eq!(inf.min_observations, 7);
        assert_eq!(inf.min_distinct_tasks, 4);
        assert_eq!(inf.auto_activate_at_confidence, 42);
        // Deserializing "blocking" here is a schema check; the "inferred may
        // not START blocking" invariant is a Phase 1 validator, not a parse
        // constraint.
        assert_eq!(inf.initial_enforcement, InitialEnforcement::Blocking);
        assert!(
            !ctx.promotion
                .blocking_directive
                .requires_explicit_confirmation
        );

        assert_eq!(ctx.efficacy.min_attributable_uses, 9);
        assert_eq!(ctx.efficacy.not_helpful_ratio_threshold, 0.25);
        assert_eq!(ctx.efficacy.min_attribution_confidence, 70);
        assert_eq!(ctx.efficacy.receipt_display_min_attribution_confidence, 60);

        assert_eq!(ctx.retention.raw_observation_days, 15);
        assert_eq!(ctx.retention.proposal_days, 45);
        assert_eq!(ctx.retention.inferred_directive_review_days, 200);
    }
}
