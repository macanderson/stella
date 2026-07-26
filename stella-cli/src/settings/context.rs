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
    pub retrieval: RetrievalSettings,
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

/// How recall ranks, diversifies, and budgets — the knobs that were `const`s.
///
/// Unlike the rest of this block, these are **live**, and they are live
/// regardless of [`LifecycleSettings::enabled`]: they configure the retrieval
/// plane that already runs for every user, not the adaptive loop. Every default
/// is exactly the value that shipped hard-coded, so a workspace that configures
/// nothing behaves identically (#712 deliverable 8).
///
/// Out-of-range values are clamped by `stella_context::RecallTuning::sanitized`
/// rather than rejected. This is a file a person edits, so the invalid values
/// are reachable, and failing a turn over a typo in a tuning knob is a worse
/// answer than ignoring it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RetrievalSettings {
    /// How many frames a prompt recall asks for.
    pub max_frames: u32,
    /// The token budget those frames are packed into.
    pub max_tokens: u32,
    /// Reciprocal-rank-fusion constant. Higher flattens the ranking.
    pub rrf_k: f64,
    /// Weight of the recency signal relative to vector similarity. Damped on
    /// purpose: at parity the newest rows occupy every slot no matter what was
    /// asked. Raising it re-opens that.
    pub recency_weight: f64,
    /// MMR relevance/diversity trade-off, `0.0..=1.0`. Higher favors relevance.
    pub mmr_lambda: f32,
    /// Mean top-k cosine below which recall falls back to labeled lexical
    /// search rather than dressing weak hits up as grounding.
    pub min_coverage: f32,
    /// How many top vector hits define that coverage estimate.
    pub coverage_topk: usize,
    /// Graph-expansion seeds taken from the strongest vector hits.
    pub max_vector_seeds: usize,
    /// Cap on lexical-fallback frames.
    pub lexical_limit: usize,
    /// Shortlist size as a multiple of `max_frames` — what the budget chooses
    /// between, and the denominator of the drop report.
    pub mmr_candidate_multiple: usize,
}

impl Default for RetrievalSettings {
    fn default() -> Self {
        let t = stella_context::RecallTuning::default();
        Self {
            // The per-query budgets, previously literals at the one call site
            // that builds a prompt recall.
            max_frames: DEFAULT_RECALL_MAX_FRAMES,
            max_tokens: DEFAULT_RECALL_MAX_TOKENS,
            rrf_k: t.rrf_k,
            recency_weight: t.recency_weight,
            mmr_lambda: t.mmr_lambda,
            min_coverage: t.min_coverage,
            coverage_topk: t.coverage_topk,
            max_vector_seeds: t.max_vector_seeds,
            lexical_limit: t.lexical_limit,
            mmr_candidate_multiple: t.mmr_candidate_multiple,
        }
    }
}

/// Frames a prompt recall asks for — the value that shipped as a literal.
pub const DEFAULT_RECALL_MAX_FRAMES: u32 = 5;
/// Token budget those frames are packed into — likewise.
pub const DEFAULT_RECALL_MAX_TOKENS: u32 = 1200;

impl RetrievalSettings {
    /// The store-level knobs, as the context plane's own type.
    ///
    /// `max_frames`/`max_tokens` are deliberately not here: they are per-query,
    /// travel on `ContextQuery`, and a store-wide copy of them would be a
    /// second place for the same number to live.
    #[must_use]
    pub fn tuning(&self) -> stella_context::RecallTuning {
        stella_context::RecallTuning {
            rrf_k: self.rrf_k,
            recency_weight: self.recency_weight,
            mmr_lambda: self.mmr_lambda,
            min_coverage: self.min_coverage,
            coverage_topk: self.coverage_topk,
            max_vector_seeds: self.max_vector_seeds,
            lexical_limit: self.lexical_limit,
            mmr_candidate_multiple: self.mmr_candidate_multiple,
        }
        .sanitized()
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
            },
            "retrieval":{
                "max_frames":11,
                "max_tokens":2222,
                "rrf_k":33.0,
                "recency_weight":0.44,
                "mmr_lambda":0.55,
                "min_coverage":0.66,
                "coverage_topk":7,
                "max_vector_seeds":13,
                "lexical_limit":17,
                "mmr_candidate_multiple":6
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

        let r = &ctx.retrieval;
        assert_eq!(r.max_frames, 11);
        assert_eq!(r.max_tokens, 2222);
        assert_eq!(r.rrf_k, 33.0);
        assert_eq!(r.recency_weight, 0.44);
        assert_eq!(r.mmr_lambda, 0.55);
        assert_eq!(r.min_coverage, 0.66);
        assert_eq!(r.coverage_topk, 7);
        assert_eq!(r.max_vector_seeds, 13);
        assert_eq!(r.lexical_limit, 17);
        assert_eq!(r.mmr_candidate_multiple, 6);
    }

    /// The defaults must reproduce the values that shipped as `const`s, or
    /// "wire them, keeping current values as defaults" is not what happened.
    #[test]
    fn retrieval_defaults_are_the_constants_they_replaced() {
        let d = RetrievalSettings::default();
        assert_eq!(
            d.max_frames, 5,
            "the literal at the prompt-recall call site"
        );
        assert_eq!(d.max_tokens, 1200, "likewise");
        assert_eq!(d.rrf_k, stella_context::DEFAULT_RRF_K);
        assert_eq!(d.recency_weight, stella_context::DEFAULT_RECENCY_WEIGHT);
        assert_eq!(d.mmr_lambda, stella_context::DEFAULT_MMR_LAMBDA);
        assert_eq!(d.min_coverage, stella_context::DEFAULT_MIN_COVERAGE);
        assert_eq!(d.coverage_topk, stella_context::DEFAULT_COVERAGE_TOPK);
        assert_eq!(d.max_vector_seeds, stella_context::DEFAULT_MAX_VECTOR_SEEDS);
        assert_eq!(d.lexical_limit, stella_context::DEFAULT_LEXICAL_LIMIT);
        assert_eq!(
            d.mmr_candidate_multiple,
            stella_context::DEFAULT_MMR_CANDIDATE_MULTIPLE
        );
        assert_eq!(
            d.tuning(),
            stella_context::RecallTuning::default(),
            "an unconfigured workspace hands the plane exactly its own defaults"
        );
    }

    /// A nonsense value degrades retrieval; it must never break it. This is a
    /// file a person edits, so zero and negative are reachable.
    #[test]
    fn out_of_range_tuning_is_clamped_not_rejected() {
        let json = r#"{"context":{"retrieval":{
            "rrf_k":-1.0,
            "mmr_lambda":9.0,
            "min_coverage":-4.0,
            "coverage_topk":0,
            "lexical_limit":0,
            "mmr_candidate_multiple":0
        }}}"#;
        let s: Settings = serde_json::from_str(json).expect("parse");
        let t = s.context.expect("present").retrieval.tuning();
        assert_eq!(
            t.rrf_k,
            stella_context::DEFAULT_RRF_K,
            "a non-positive fusion constant inverts the ranking"
        );
        assert_eq!(t.mmr_lambda, 1.0, "clamped into [0,1]");
        assert_eq!(t.min_coverage, 0.0);
        assert_eq!(t.coverage_topk, 1, "zero would divide by zero");
        assert_eq!(t.lexical_limit, 1);
        assert_eq!(
            t.mmr_candidate_multiple, 1,
            "zero would make every recall empty"
        );
    }
}
