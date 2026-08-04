//! Self-tuning — the eval-driven policy selector: pure synchronous analysis of
//! A/B outcomes that decides whether one variant of a decision knob is
//! *confidently* better than the current baseline, and only then recommends
//! promoting it.
//!
//! This is the first slice of self-improvement issue #831 ("a bandit over
//! stella's own prompts, policies, and routing"): **one knob (worker reasoning
//! effort) A/B'd with automatic winner selection and a rollback record.** The
//! knob here is opaque — an arm is just an id, a human-readable `value`, and a
//! bag of per-trial reward samples — so the same engine generalizes to any
//! future knob (prompt templates, routing, effort defaults) without change.
//!
//! Like [`crate::scoreboard`] and [`crate::loop_detect`], this module does no
//! I/O: the caller (the CLI) runs the benchmark, folds each trial into a reward
//! ([`reward`]), and hands the per-arm samples to [`select_winner`]. The result
//! is a typed [`Decision`] the caller acts on — write the winning overlay to
//! settings and append a [`RollbackRecord`], or keep the baseline with a stated
//! reason. The arithmetic is a deterministic function of the samples and is
//! unit-testable without a store, a provider, or a benchmark run.
//!
//! **"Improvement proven, not asserted."** A promotion is gated on two things,
//! both required:
//!
//! 1. **Enough evidence** — every arm needs at least
//!    [`SelectionConfig::min_samples_per_arm`] trials, so a lucky run of two
//!    can't flip the policy.
//! 2. **A confident lift** — the winner's mean must beat the baseline's by a
//!    margin large relative to the noise in *both* arms. The test is a
//!    Welch-style two-sample z on the difference of means
//!    (`z = (mean_w - mean_b) / sqrt(se_w² + se_b²)`); the lift is credited
//!    only when `z >= confidence_z` **and** the raw lift clears
//!    [`SelectionConfig::min_effect`].
//!
//! Because a promotion fires *only* when the candidate is confidently better,
//! "no regression" is a property of the rule, not a separate check: the
//! baseline is never replaced by something merely tied or noisily different.
//! The conservative direction is deliberate — this changes stella's own
//! behavior, so the bar to move it is high and the move is always reversible
//! via the [`RollbackRecord`].

use serde::{Deserialize, Serialize};

/// One task's outcome, distilled from receipts into the ingredients the reward
/// function weighs. The caller builds this from whatever outcome record it has
/// (a benchmark trial, a stored execution rollup); the reward is a pure
/// function of it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TaskOutcome {
    /// Did the task succeed (verifier passed, goal met)? The dominant signal.
    pub succeeded: bool,
    /// USD spent on the task — a cost the reward can penalize.
    pub cost_usd: f64,
    /// Total tokens spent on the task.
    pub tokens: u64,
    /// How many times the task had to retry / was detected looping — a
    /// friction signal distinct from outright failure.
    pub retries: u32,
}

/// Weights turning a [`TaskOutcome`] into a scalar reward (higher is better).
/// The default is **pure success** — reward is `1.0` for a solved task and
/// `0.0` otherwise — so an A/B over the default weights is a straight
/// pass-rate comparison. Callers dial in cost/token/retry penalties to make
/// the reward cost-aware.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RewardWeights {
    /// Reward added for a successful task.
    pub success: f64,
    /// Reward subtracted per USD spent.
    pub cost_usd: f64,
    /// Reward subtracted per 1,000 tokens spent.
    pub per_1k_tokens: f64,
    /// Reward subtracted per retry / loop event.
    pub per_retry: f64,
}

impl Default for RewardWeights {
    fn default() -> Self {
        Self {
            success: 1.0,
            cost_usd: 0.0,
            per_1k_tokens: 0.0,
            per_retry: 0.0,
        }
    }
}

/// Score one task outcome into a scalar reward under `weights`. Pure and
/// total: success contributes `weights.success`; cost, tokens, and retries
/// each subtract their weighted amount. With [`RewardWeights::default`] this
/// is exactly the success indicator (`1.0` / `0.0`).
pub fn reward(outcome: &TaskOutcome, weights: &RewardWeights) -> f64 {
    let success = if outcome.succeeded {
        weights.success
    } else {
        0.0
    };
    success
        - weights.cost_usd * outcome.cost_usd
        - weights.per_1k_tokens * (outcome.tokens as f64 / 1000.0)
        - weights.per_retry * outcome.retries as f64
}

/// One arm of the experiment: a knob variant and the reward samples observed
/// for it (one per benchmark trial).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArmSamples {
    /// Stable id of the arm (e.g. `"baseline"` / `"candidate"`).
    pub id: String,
    /// The knob value this arm represents, for display and the rollback record
    /// (e.g. an effort label `"high"`).
    pub value: String,
    /// One reward per trial, in any order.
    pub rewards: Vec<f64>,
}

/// The summary statistics of one arm's samples. Serialized into the A/B
/// receipt so a promotion's evidence is auditable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArmStats {
    /// The arm id.
    pub id: String,
    /// The knob value.
    pub value: String,
    /// Number of samples.
    pub n: usize,
    /// Mean reward.
    pub mean: f64,
    /// Sample standard deviation (Bessel-corrected; `0.0` for `n < 2`).
    pub std_dev: f64,
    /// Standard error of the mean (`std_dev / sqrt(n)`).
    pub std_error: f64,
}

impl ArmStats {
    /// Fold an arm's samples into its summary statistics. Never panics: an
    /// empty arm yields `n = 0`, `mean = 0.0`, and zero spread.
    pub fn from_samples(arm: &ArmSamples) -> Self {
        let n = arm.rewards.len();
        let mean = if n == 0 {
            0.0
        } else {
            arm.rewards.iter().sum::<f64>() / n as f64
        };
        let std_dev = if n >= 2 {
            let ss: f64 = arm.rewards.iter().map(|x| (x - mean).powi(2)).sum();
            (ss / (n as f64 - 1.0)).sqrt()
        } else {
            0.0
        };
        let std_error = if n >= 1 {
            std_dev / (n as f64).sqrt()
        } else {
            0.0
        };
        Self {
            id: arm.id.clone(),
            value: arm.value.clone(),
            n,
            mean,
            std_dev,
            std_error,
        }
    }
}

/// Thresholds for [`select_winner`]. `Default` is a conservative bar suited to
/// auto-promotion of stella's own behavior.
///
/// Serializable because a verdict without its thresholds cannot be audited:
/// [`crate::comparison::ComparisonReport`] publishes the bar a promotion
/// cleared beside the promotion itself, so a reader can see it was the
/// conservative default rather than one lowered for the occasion.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SelectionConfig {
    /// Minimum trials required in **every** arm before any promotion. A knob
    /// change is never made on a handful of samples.
    pub min_samples_per_arm: usize,
    /// The z multiplier the difference of means must clear. `1.96` is roughly a
    /// 97.5% one-sided bound — the winner is very likely genuinely better, not
    /// noise.
    pub confidence_z: f64,
    /// Minimum raw lift (winner mean − baseline mean) required regardless of
    /// significance, so a statistically-real-but-trivial gain is not promoted.
    /// `0.0` leaves the z-test as the only gate.
    pub min_effect: f64,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self {
            min_samples_per_arm: 5,
            confidence_z: 1.96,
            min_effect: 0.0,
        }
    }
}

/// The winning arm and the evidence that it beat the baseline confidently.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Promotion {
    /// Stats of the winning arm.
    pub winner: ArmStats,
    /// Stats of the baseline it beat.
    pub baseline: ArmStats,
    /// Raw lift: `winner.mean - baseline.mean`.
    pub lift: f64,
    /// The observed Welch z of the difference of means. `None` when both arms
    /// had zero variance and the arms separated cleanly — an unbounded
    /// (deterministic) confidence that has no finite z. (JSON has no infinity,
    /// so `None`/`null` is the round-trippable spelling of "certain".)
    pub observed_z: Option<f64>,
    /// The z bar it had to clear (`SelectionConfig::confidence_z`).
    pub threshold_z: f64,
}

/// Why [`select_winner`] declined to promote. Every reason names the evidence
/// so the caller can surface an honest "kept the baseline because …".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum KeepReason {
    /// An arm had fewer than `required` samples.
    InsufficientSamples {
        arm: String,
        n: usize,
        required: usize,
    },
    /// The named baseline arm was not present in the input.
    MissingBaseline { baseline_id: String },
    /// The baseline already has the highest mean — nothing to promote.
    BaselineIsBest { baseline_mean: f64 },
    /// A candidate led on the mean, but the lift did not clear `min_effect`.
    BelowMinEffect { lift: f64, min_effect: f64 },
    /// A candidate led, but the lift was not statistically confident.
    LiftNotConfident {
        lift: f64,
        observed_z: f64,
        threshold_z: f64,
    },
}

/// The outcome of a selection: promote the winner, or keep the baseline with a
/// stated reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// Promote the winning arm.
    Promote(Promotion),
    /// Keep the baseline; no confident improvement.
    Keep(KeepReason),
}

impl Decision {
    /// `true` only for [`Decision::Promote`].
    pub fn is_promotion(&self) -> bool {
        matches!(self, Decision::Promote(_))
    }
}

/// Decide whether any arm confidently beats `baseline_id`.
///
/// `arms` are the experiment's arms (two for the first slice, but N is fine).
/// Returns [`Decision::Promote`] only when the highest-mean arm is not the
/// baseline **and** its lift over the baseline is both large enough
/// ([`SelectionConfig::min_effect`]) and statistically confident
/// ([`SelectionConfig::confidence_z`]); otherwise [`Decision::Keep`] with the
/// reason. Never panics — empty arms, a missing baseline, under-sampled arms,
/// and non-finite samples all resolve to a `Keep`.
pub fn select_winner(arms: &[ArmSamples], baseline_id: &str, config: &SelectionConfig) -> Decision {
    let stats: Vec<ArmStats> = arms.iter().map(ArmStats::from_samples).collect();

    let Some(baseline) = stats.iter().find(|s| s.id == baseline_id).cloned() else {
        return Decision::Keep(KeepReason::MissingBaseline {
            baseline_id: baseline_id.to_string(),
        });
    };

    // Every arm must clear the sample floor before we trust any comparison.
    for s in &stats {
        if s.n < config.min_samples_per_arm {
            return Decision::Keep(KeepReason::InsufficientSamples {
                arm: s.id.clone(),
                n: s.n,
                required: config.min_samples_per_arm,
            });
        }
    }

    // Winner = highest mean. `partial_cmp` guards against NaN (treated equal),
    // so a NaN mean can never win by accident.
    let Some(winner) = stats
        .iter()
        .max_by(|a, b| {
            a.mean
                .partial_cmp(&b.mean)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .cloned()
    else {
        // Unreachable given a present baseline, but stay total.
        return Decision::Keep(KeepReason::MissingBaseline {
            baseline_id: baseline_id.to_string(),
        });
    };

    if winner.id == baseline.id {
        return Decision::Keep(KeepReason::BaselineIsBest {
            baseline_mean: baseline.mean,
        });
    }

    let lift = winner.mean - baseline.mean;
    // A non-finite lift (NaN samples slipped through) is never a promotion.
    if !lift.is_finite() || lift < config.min_effect {
        return Decision::Keep(KeepReason::BelowMinEffect {
            lift,
            min_effect: config.min_effect,
        });
    }

    let se_diff = (winner.std_error.powi(2) + baseline.std_error.powi(2)).sqrt();
    let (confident, observed_z) = if se_diff > 0.0 {
        let z = lift / se_diff;
        (z >= config.confidence_z, Some(z))
    } else if lift > 0.0 {
        // Zero combined variance with a positive lift is a clean, deterministic
        // separation — maximally confident, and with no finite z.
        (true, None)
    } else {
        // Zero variance and a zero lift is a tie, not a win.
        (false, Some(0.0))
    };

    if !confident {
        return Decision::Keep(KeepReason::LiftNotConfident {
            lift,
            observed_z: observed_z.unwrap_or(0.0),
            threshold_z: config.confidence_z,
        });
    }

    Decision::Promote(Promotion {
        winner,
        baseline,
        lift,
        observed_z,
        threshold_z: config.confidence_z,
    })
}

/// A durable, reversible record of a promotion: what knob changed, from what to
/// what, and the evidence that justified it. The caller persists this (a JSONL
/// line under `.stella/private/`) so a promotion can be rolled back to
/// `previous_value` and the A/B evidence is auditable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RollbackRecord {
    /// The settings path the knob lives at (e.g.
    /// `"agent_engine_config.agents.worker.effort"`).
    pub knob: String,
    /// The value before promotion (`None` when the knob was unset / on auto).
    pub previous_value: Option<String>,
    /// The value promoted to.
    pub promoted_value: String,
    /// Baseline mean reward at promotion time.
    pub baseline_mean: f64,
    /// Winner mean reward at promotion time.
    pub winner_mean: f64,
    /// The credited lift.
    pub lift: f64,
    /// The observed confidence z (`None` for a zero-variance clean separation).
    pub observed_z: Option<f64>,
    /// Samples the baseline was measured on.
    pub baseline_samples: usize,
    /// Samples the winner was measured on.
    pub winner_samples: usize,
}

impl RollbackRecord {
    /// Build a rollback record from a [`Promotion`] and the knob's prior value.
    pub fn from_promotion(
        knob: impl Into<String>,
        previous_value: Option<String>,
        promotion: &Promotion,
    ) -> Self {
        Self {
            knob: knob.into(),
            previous_value,
            promoted_value: promotion.winner.value.clone(),
            baseline_mean: promotion.baseline.mean,
            winner_mean: promotion.winner.mean,
            lift: promotion.lift,
            observed_z: promotion.observed_z,
            baseline_samples: promotion.baseline.n,
            winner_samples: promotion.winner.n,
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn arm(id: &str, value: &str, rewards: &[f64]) -> ArmSamples {
        ArmSamples {
            id: id.into(),
            value: value.into(),
            rewards: rewards.to_vec(),
        }
    }

    #[test]
    fn reward_default_is_the_success_indicator() {
        let w = RewardWeights::default();
        let solved = TaskOutcome {
            succeeded: true,
            cost_usd: 3.0,
            tokens: 50_000,
            retries: 4,
        };
        let failed = TaskOutcome {
            succeeded: false,
            cost_usd: 0.0,
            tokens: 0,
            retries: 0,
        };
        assert_eq!(reward(&solved, &w), 1.0);
        assert_eq!(reward(&failed, &w), 0.0);
    }

    #[test]
    fn reward_penalizes_cost_tokens_and_retries() {
        let w = RewardWeights {
            success: 1.0,
            cost_usd: 0.5,
            per_1k_tokens: 0.01,
            per_retry: 0.1,
        };
        let outcome = TaskOutcome {
            succeeded: true,
            cost_usd: 1.0,
            tokens: 2_000,
            retries: 3,
        };
        // 1.0 - 0.5*1.0 - 0.01*2 - 0.1*3 = 1.0 - 0.5 - 0.02 - 0.3 = 0.18
        assert!((reward(&outcome, &w) - 0.18).abs() < 1e-9);
        // More retries strictly lowers reward; success strictly raises it.
        let more_retries = TaskOutcome {
            retries: 4,
            ..outcome
        };
        assert!(reward(&more_retries, &w) < reward(&outcome, &w));
        let unsolved = TaskOutcome {
            succeeded: false,
            ..outcome
        };
        assert!(reward(&unsolved, &w) < reward(&outcome, &w));
    }

    #[test]
    fn a_confident_lift_is_promoted() {
        // Candidate solves every task, baseline solves none: a clean, maximal
        // separation.
        let arms = vec![
            arm("baseline", "medium", &[0.0, 0.0, 0.0, 0.0, 0.0]),
            arm("candidate", "high", &[1.0, 1.0, 1.0, 1.0, 1.0]),
        ];
        let decision = select_winner(&arms, "baseline", &SelectionConfig::default());
        match decision {
            Decision::Promote(p) => {
                assert_eq!(p.winner.id, "candidate");
                assert_eq!(p.winner.value, "high");
                assert!((p.lift - 1.0).abs() < 1e-9);
                // Zero-variance clean separation → unbounded confidence.
                assert!(p.observed_z.is_none());
            }
            other => panic!("expected promotion, got {other:?}"),
        }
    }

    #[test]
    fn a_marginal_noisy_lift_is_not_promoted() {
        // Candidate leads on the mean, but both arms are noisy and overlapping:
        // the difference is not confident.
        let arms = vec![
            arm("baseline", "medium", &[1.0, 0.0, 1.0, 0.0, 1.0, 0.0]),
            arm("candidate", "high", &[1.0, 0.0, 1.0, 1.0, 0.0, 1.0]),
        ];
        let decision = select_winner(&arms, "baseline", &SelectionConfig::default());
        assert!(
            !decision.is_promotion(),
            "a noisy overlapping lead must not promote: {decision:?}"
        );
    }

    #[test]
    fn insufficient_samples_keeps_the_baseline() {
        let arms = vec![
            arm("baseline", "medium", &[0.0, 0.0]),
            arm("candidate", "high", &[1.0, 1.0]),
        ];
        let decision = select_winner(&arms, "baseline", &SelectionConfig::default());
        match decision {
            Decision::Keep(KeepReason::InsufficientSamples { required, .. }) => {
                assert_eq!(required, 5)
            }
            other => panic!("expected InsufficientSamples, got {other:?}"),
        }
    }

    #[test]
    fn baseline_already_best_is_kept() {
        let arms = vec![
            arm("baseline", "medium", &[1.0, 1.0, 1.0, 1.0, 1.0]),
            arm("candidate", "high", &[0.0, 0.0, 0.0, 0.0, 0.0]),
        ];
        let decision = select_winner(&arms, "baseline", &SelectionConfig::default());
        assert!(matches!(
            decision,
            Decision::Keep(KeepReason::BaselineIsBest { .. })
        ));
    }

    #[test]
    fn a_tie_is_kept_not_promoted() {
        let arms = vec![
            arm("baseline", "medium", &[1.0, 0.0, 1.0, 0.0, 1.0, 0.0]),
            arm("candidate", "high", &[1.0, 0.0, 1.0, 0.0, 1.0, 0.0]),
        ];
        let decision = select_winner(&arms, "baseline", &SelectionConfig::default());
        assert!(!decision.is_promotion(), "an exact tie must not promote");
    }

    #[test]
    fn missing_baseline_is_reported() {
        let arms = vec![arm("candidate", "high", &[1.0, 1.0, 1.0, 1.0, 1.0])];
        let decision = select_winner(&arms, "baseline", &SelectionConfig::default());
        assert!(matches!(
            decision,
            Decision::Keep(KeepReason::MissingBaseline { .. })
        ));
    }

    #[test]
    fn min_effect_gate_blocks_a_trivial_confident_lift() {
        // A large, perfectly-clean but tiny-magnitude lift: confident yet below
        // a min_effect bar.
        let arms = vec![
            arm("baseline", "medium", &[0.50; 8]),
            arm("candidate", "high", &[0.51; 8]),
        ];
        let strict = SelectionConfig {
            min_effect: 0.05,
            ..SelectionConfig::default()
        };
        match select_winner(&arms, "baseline", &strict) {
            Decision::Keep(KeepReason::BelowMinEffect { min_effect, .. }) => {
                assert!((min_effect - 0.05).abs() < 1e-9)
            }
            other => panic!("expected BelowMinEffect, got {other:?}"),
        }
        // With the default (min_effect 0.0) the same clean separation promotes.
        assert!(select_winner(&arms, "baseline", &SelectionConfig::default()).is_promotion());
    }

    #[test]
    fn nan_samples_never_promote() {
        let arms = vec![
            arm("baseline", "medium", &[0.0, 0.0, 0.0, 0.0, 0.0]),
            arm("candidate", "high", &[f64::NAN, 1.0, 1.0, 1.0, 1.0]),
        ];
        let decision = select_winner(&arms, "baseline", &SelectionConfig::default());
        assert!(!decision.is_promotion(), "NaN must not produce a promotion");
    }

    #[test]
    fn rollback_record_round_trips_and_captures_evidence() {
        let arms = vec![
            arm("baseline", "medium", &[0.0, 0.0, 0.0, 0.0, 0.0]),
            arm("candidate", "high", &[1.0, 1.0, 1.0, 1.0, 1.0]),
        ];
        let Decision::Promote(p) = select_winner(&arms, "baseline", &SelectionConfig::default())
        else {
            panic!("expected promotion");
        };
        let record = RollbackRecord::from_promotion(
            "agent_engine_config.agents.worker.effort",
            Some("medium".to_string()),
            &p,
        );
        assert_eq!(record.previous_value.as_deref(), Some("medium"));
        assert_eq!(record.promoted_value, "high");
        assert_eq!(record.baseline_samples, 5);
        assert_eq!(record.winner_samples, 5);

        // Invariant #4: serde round-trip is byte-stable.
        let json = serde_json::to_string(&record).unwrap();
        let back: RollbackRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, back);
    }

    #[test]
    fn decision_serializes_with_a_tag() {
        let keep = Decision::Keep(KeepReason::MissingBaseline {
            baseline_id: "baseline".into(),
        });
        let json = serde_json::to_string(&keep).unwrap();
        assert!(json.contains("\"decision\":\"keep\""));
        assert!(json.contains("\"reason\":\"missing_baseline\""));
    }

    /// A small overlapping alphabet so property runs exercise real comparisons
    /// (including under-sampled and lopsided arms) rather than trivially
    /// distinct ones.
    fn arb_arm(id: &'static str) -> impl Strategy<Value = ArmSamples> {
        proptest::collection::vec(0.0f64..1.0, 0..12).prop_map(move |rewards| arm(id, id, &rewards))
    }

    proptest! {
        /// `select_winner` never panics for any pair of arms and any config,
        /// including empty arms and degenerate thresholds.
        #[test]
        fn select_never_panics(
            a in arb_arm("baseline"),
            b in arb_arm("candidate"),
            min_samples in 0usize..10,
            confidence_z in 0.0f64..4.0,
            min_effect in 0.0f64..1.0,
        ) {
            let config = SelectionConfig { min_samples_per_arm: min_samples, confidence_z, min_effect };
            let _ = select_winner(&[a, b], "baseline", &config);
        }

        /// A promotion is only ever the arm with the strictly higher mean, and
        /// always carries a lift at or above `min_effect`.
        #[test]
        fn promotions_are_well_formed(
            a in arb_arm("baseline"),
            b in arb_arm("candidate"),
        ) {
            let config = SelectionConfig::default();
            if let Decision::Promote(p) = select_winner(&[a, b], "baseline", &config) {
                prop_assert!(p.winner.mean > p.baseline.mean);
                prop_assert!(p.lift >= config.min_effect);
                // A finite z must clear the bar; `None` means unbounded (certain).
                if let Some(z) = p.observed_z {
                    prop_assert!(z >= config.confidence_z);
                }
                prop_assert!(p.winner.n >= config.min_samples_per_arm);
                prop_assert!(p.baseline.n >= config.min_samples_per_arm);
            }
        }

        /// Selection is deterministic: same input, same decision.
        #[test]
        fn select_is_deterministic(
            a in arb_arm("baseline"),
            b in arb_arm("candidate"),
        ) {
            let config = SelectionConfig::default();
            let d1 = select_winner(&[a.clone(), b.clone()], "baseline", &config);
            let d2 = select_winner(&[a, b], "baseline", &config);
            prop_assert_eq!(d1, d2);
        }
    }
}
