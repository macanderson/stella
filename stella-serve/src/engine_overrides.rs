// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The per-turn engine surface of `POST /v1/turns` (#1167): which of
//! `EngineConfig`'s knobs a caller may set, and the operator ceilings that
//! bound them.
//!
//! Split from `routes.rs` for the 1500-line ratchet
//! (`scripts/check-file-size.sh`), and because it is a separate concern: the
//! routes own status codes and wire shapes, while everything here is policy —
//! which knob belongs to the caller, which to the operator, and what happens
//! when a request asks for more than the deployment will pay for.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use stella_core::EngineConfig;

/// The caller-policy slice of `EngineConfig`, settable per turn (#1167) as
/// the optional `engine` object on `POST /v1/turns` and
/// `POST /v1/sessions/{id}/turns`.
///
/// Every field is independently optional with "lower onto defaults" semantics:
/// `None` keeps the server's configured default, `Some` overrides it for this
/// turn only. What is deliberately **not** here: `retry_policy` and
/// `loop_detection` are operator policy — they bound what a single request can
/// cost this process, and a caller who could widen them could make a turn
/// effectively unbounded — and `max_steps` / `reverse_request_timeout_ms`
/// already ride the request top-level.
///
/// Unknown fields are refused rather than ignored: a typoed knob that parses
/// is a knob silently *not* honored, the same illegibility the ceilings exist
/// to avoid on the other side.
///
/// Session note: none of these knobs touch the message transcript, so a
/// per-turn override on `POST /v1/sessions/{id}/turns` cannot perturb the
/// byte-stable prompt prefix the session's cache contract depends on.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EngineOverrides {
    /// Output-token cap forwarded on every completion of this turn. Rejected
    /// at 0; clamped to [`MAX_OUTPUT_TOKENS_CEILING`]. Effort tier and this
    /// cap are one budget — raising effort against a small cap is how a model
    /// spends its tokens thinking and gets truncated before its tool call.
    #[serde(default)]
    max_output_tokens: Option<u32>,
    /// Sampling temperature. Rejected unless finite and non-negative; clamped
    /// to [`MAX_TEMPERATURE`].
    #[serde(default)]
    temperature: Option<f32>,
    /// Reasoning-effort tier (`CompletionRequest::effort` semantics).
    #[serde(default)]
    effort: Option<stella_protocol::ReasoningEffort>,
    /// Thinking-mode enable/disable (`CompletionRequest::reasoning`
    /// semantics; `None` = provider default).
    #[serde(default)]
    reasoning: Option<bool>,
    /// Sampling/routing overrides (`CompletionRequest::params` semantics —
    /// the host's adapter forwards the subset its dialect supports).
    #[serde(default)]
    params: Option<stella_protocol::GenerationParams>,
    /// Compaction trigger, in estimated tokens. Rejected at 0; clamped to
    /// [`MAX_COMPACTION_BUDGET_TOKENS`].
    #[serde(default)]
    compaction_budget_tokens: Option<u64>,
    /// Whether overflow beyond the compaction budget may be summarized away
    /// by a model call (`EngineConfig::summarize_overflow`).
    #[serde(default)]
    summarize_overflow: Option<bool>,
    /// Messages at the tail the summarizer never touches. Clamped to
    /// [`MAX_SUMMARIZE_KEEP_RECENT`].
    #[serde(default)]
    summarize_keep_recent: Option<usize>,
    /// Seconds of provider silence that end a single generation
    /// (`EngineConfig::model_timeout`). Clamped to
    /// [`MAX_MODEL_TIMEOUT_SECS`]; `0` disables the backstop.
    ///
    /// The partner of `max_output_tokens`, and unusable without it: the two are
    /// one budget, so a host that raises the cap and leaves this at the default
    /// has moved where its steps die rather than stopped them dying. Exposed
    /// for that reason — before this, a host could pin the output cap over the
    /// wire but not the timeout that has to scale with it (#1211 §6.2).
    #[serde(default)]
    model_timeout_secs: Option<u64>,
}

/// Ceiling on a caller-supplied `engine.max_output_tokens`: far above any
/// model's real output ceiling today, while keeping `u32::MAX` — a cap that
/// is no cap — unreachable from the wire. Same posture as `MAX_SERVED_STEPS`
/// in `routes.rs`: a hard operator-side constant, not a guess at what a
/// caller might want.
const MAX_OUTPUT_TOKENS_CEILING: u32 = 262_144;

/// Ceiling on a caller-supplied `engine.temperature`: the widest range any
/// current provider dialect accepts (OpenAI's 0..=2). Values above are
/// clamped and reported, values below 0 or non-finite are refused — there is
/// no sane reading of a NaN temperature to clamp toward.
const MAX_TEMPERATURE: f32 = 2.0;

/// Ceiling on a caller-supplied `engine.compaction_budget_tokens`. Compaction
/// work is CPU and memory *this* process pays for, so the operator bounds it:
/// two million tokens sits above every context window served today.
const MAX_COMPACTION_BUDGET_TOKENS: u64 = 2_000_000;

/// Ceiling on a caller-supplied `engine.summarize_keep_recent`. Beyond this
/// the knob is indistinguishable from disabling the summarizer, which
/// `summarize_overflow: false` already spells honestly.
const MAX_SUMMARIZE_KEEP_RECENT: usize = 1_024;

/// Ceiling on a caller-supplied `engine.model_timeout_secs`: six hours, far
/// above the longest single generation any model produces today, while keeping
/// a caller from parking a server worker on an effectively unbounded await by
/// accident. Saying so on purpose still works — `0` disables the backstop,
/// which is a different request from "a very large one" and is spelled
/// differently.
const MAX_MODEL_TIMEOUT_SECS: u64 = 21_600;

/// One knob the server lowered below what the caller asked for — reported in
/// the create response so the clamp is legible, never silently honored-as-if.
#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct ClampedKnob {
    /// The knob's wire name, e.g. `"max_output_tokens"`.
    pub(crate) knob: &'static str,
    /// What the caller asked for.
    pub(crate) requested: f64,
    /// What this turn will actually run with.
    pub(crate) effective: f64,
}

/// Lower a caller's `engine` object onto `config`.
///
/// Unusable values — a zero cap, a NaN temperature — are refused with a
/// message naming the knob (the caller meant *something*, and guessing which
/// something is worse than asking). Values past an operator ceiling are
/// clamped and reported back in the response, so a request is never silently
/// honored at a value it did not get.
pub(crate) fn apply_engine_overrides(
    config: &mut EngineConfig,
    overrides: &EngineOverrides,
) -> Result<Vec<ClampedKnob>, String> {
    let mut clamped = Vec::new();
    if let Some(temperature) = overrides.temperature {
        if !temperature.is_finite() || temperature < 0.0 {
            return Err(format!(
                "engine.temperature must be a finite, non-negative number \
                 (got {temperature}); values above {MAX_TEMPERATURE} are clamped"
            ));
        }
        let effective = if temperature > MAX_TEMPERATURE {
            clamped.push(ClampedKnob {
                knob: "temperature",
                requested: f64::from(temperature),
                effective: f64::from(MAX_TEMPERATURE),
            });
            MAX_TEMPERATURE
        } else {
            temperature
        };
        config.temperature = Some(effective);
    }
    if let Some(requested) = overrides.max_output_tokens {
        if requested == 0 {
            return Err("engine.max_output_tokens must be at least 1".to_string());
        }
        let effective = requested.min(MAX_OUTPUT_TOKENS_CEILING);
        if effective != requested {
            clamped.push(ClampedKnob {
                knob: "max_output_tokens",
                requested: f64::from(requested),
                effective: f64::from(effective),
            });
        }
        config.max_output_tokens = Some(effective);
    }
    if let Some(effort) = overrides.effort {
        config.effort = Some(effort);
    }
    if let Some(reasoning) = overrides.reasoning {
        config.reasoning = Some(reasoning);
    }
    if let Some(params) = overrides.params {
        config.params = Some(params);
    }
    if let Some(requested) = overrides.compaction_budget_tokens {
        if requested == 0 {
            return Err("engine.compaction_budget_tokens must be at least 1".to_string());
        }
        let effective = requested.min(MAX_COMPACTION_BUDGET_TOKENS);
        if effective != requested {
            clamped.push(ClampedKnob {
                knob: "compaction_budget_tokens",
                // Lossless: the ceiling (and so every reportable value) is
                // far inside f64's 2^53 exact-integer range.
                requested: requested as f64,
                effective: effective as f64,
            });
        }
        config.compaction_budget_tokens = effective;
    }
    if let Some(summarize) = overrides.summarize_overflow {
        config.summarize_overflow = summarize;
    }
    if let Some(requested) = overrides.summarize_keep_recent {
        let effective = requested.min(MAX_SUMMARIZE_KEEP_RECENT);
        if effective != requested {
            clamped.push(ClampedKnob {
                knob: "summarize_keep_recent",
                requested: requested as f64,
                effective: effective as f64,
            });
        }
        config.summarize_keep_recent = effective;
    }
    if let Some(requested) = overrides.model_timeout_secs {
        let effective = requested.min(MAX_MODEL_TIMEOUT_SECS);
        if effective != requested {
            clamped.push(ClampedKnob {
                knob: "model_timeout_secs",
                requested: requested as f64,
                effective: effective as f64,
            });
        }
        // Zero is "no backstop", not "time out immediately" — the reading that
        // makes every generation fail instantly is never what a caller meant,
        // and `Option::None` is the field's own spelling for unbounded.
        config.model_timeout = (effective > 0).then(|| Duration::from_secs(effective));
    }
    Ok(clamped)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default-preserving contract of #1167: an omitted knob keeps the
    /// server's default, and an empty `engine` object is a no-op — so a host
    /// that sends `{}` and a host that sends nothing run identical turns.
    #[test]
    fn an_empty_engine_object_changes_nothing() {
        let mut config = EngineConfig::default();
        let clamped = apply_engine_overrides(&mut config, &EngineOverrides::default())
            .expect("an empty override set is valid");
        assert!(clamped.is_empty());
        let default = EngineConfig::default();
        assert_eq!(config.temperature, default.temperature);
        assert_eq!(config.max_output_tokens, default.max_output_tokens);
        assert_eq!(
            config.compaction_budget_tokens,
            default.compaction_budget_tokens
        );
        assert_eq!(config.summarize_keep_recent, default.summarize_keep_recent);
        assert_eq!(config.model_timeout, default.model_timeout);
    }

    /// One turn at temperature 0 with a small cap, the next at high effort
    /// with a large one — the acceptance shape of #1167, minus the HTTP.
    #[test]
    fn engine_overrides_lower_onto_the_defaults() {
        let mut config = EngineConfig::default();
        let overrides: EngineOverrides = serde_json::from_value(serde_json::json!({
            "temperature": 0.0,
            "max_output_tokens": 1024,
            "effort": "xhigh",
            "reasoning": true,
            "compaction_budget_tokens": 200_000,
            "summarize_overflow": false,
            "summarize_keep_recent": 16,
            "model_timeout_secs": 1572,
            "params": { "top_p": 0.9 },
        }))
        .expect("a full override set parses");
        let clamped = apply_engine_overrides(&mut config, &overrides).expect("all values legal");
        assert!(clamped.is_empty(), "nothing here needed clamping");
        assert_eq!(config.temperature, Some(0.0));
        assert_eq!(config.max_output_tokens, Some(1024));
        assert_eq!(config.effort, Some(stella_protocol::ReasoningEffort::Xhigh));
        assert_eq!(config.reasoning, Some(true));
        assert_eq!(config.compaction_budget_tokens, 200_000);
        assert!(!config.summarize_overflow);
        assert_eq!(config.summarize_keep_recent, 16);
        assert_eq!(config.model_timeout, Some(Duration::from_secs(1572)));
        assert_eq!(config.params.and_then(|p| p.top_p), Some(0.9));
        // The knobs the caller did not send keep their defaults.
        assert_eq!(config.max_steps, EngineConfig::default().max_steps);
    }

    /// Ceilings are clamps, and every clamp is reported: a request is never
    /// silently honored at a value it did not get.
    #[test]
    fn values_past_a_ceiling_are_clamped_and_reported() {
        let mut config = EngineConfig::default();
        let overrides: EngineOverrides = serde_json::from_value(serde_json::json!({
            "temperature": 100.0,
            "max_output_tokens": u32::MAX,
            "compaction_budget_tokens": u64::MAX,
            "summarize_keep_recent": 1_000_000,
            "model_timeout_secs": u64::MAX,
        }))
        .expect("large values still parse");
        let clamped =
            apply_engine_overrides(&mut config, &overrides).expect("clamped, not refused");
        let knobs: Vec<&str> = clamped.iter().map(|c| c.knob).collect();
        assert_eq!(
            knobs,
            vec![
                "temperature",
                "max_output_tokens",
                "compaction_budget_tokens",
                "summarize_keep_recent",
                "model_timeout_secs",
            ],
            "every lowered knob must be named"
        );
        assert_eq!(config.temperature, Some(MAX_TEMPERATURE));
        assert_eq!(config.max_output_tokens, Some(MAX_OUTPUT_TOKENS_CEILING));
        assert_eq!(
            config.compaction_budget_tokens,
            MAX_COMPACTION_BUDGET_TOKENS
        );
        assert_eq!(config.summarize_keep_recent, MAX_SUMMARIZE_KEEP_RECENT);
        assert_eq!(
            config.model_timeout,
            Some(Duration::from_secs(MAX_MODEL_TIMEOUT_SECS))
        );
        for clamp in &clamped {
            assert!(
                clamp.requested > clamp.effective,
                "{}: a report must show the lowering",
                clamp.knob
            );
        }
    }

    /// Unusable values are refused with the knob named — a zero output cap or
    /// a NaN temperature has no sane clamp target, and guessing one would
    /// bury the caller's real mistake.
    #[test]
    fn unusable_engine_values_are_refused_by_name() {
        for (json, named) in [
            (
                serde_json::json!({ "max_output_tokens": 0 }),
                "max_output_tokens",
            ),
            (serde_json::json!({ "temperature": -1.0 }), "temperature"),
            (
                serde_json::json!({ "compaction_budget_tokens": 0 }),
                "compaction_budget_tokens",
            ),
        ] {
            let overrides: EngineOverrides = serde_json::from_value(json).expect("parses");
            let mut config = EngineConfig::default();
            let err = apply_engine_overrides(&mut config, &overrides)
                .expect_err("an unusable value must be refused");
            assert!(err.contains(named), "the refusal must name the knob: {err}");
        }
    }

    /// `0` is the one value here that means something other than "too small".
    ///
    /// The output cap refuses it because a zero-token generation cannot
    /// produce anything; this knob accepts it because `EngineConfig` already
    /// spells "no backstop" as `None`, and a host that wants an unbounded
    /// await has no other way to ask. Reading it as "time out immediately"
    /// would instead fail every generation on the turn.
    #[test]
    fn a_zero_model_timeout_is_no_backstop_rather_than_an_instant_trip() {
        let mut config = EngineConfig::default();
        let overrides: EngineOverrides =
            serde_json::from_value(serde_json::json!({ "model_timeout_secs": 0 })).expect("parses");
        let clamped =
            apply_engine_overrides(&mut config, &overrides).expect("zero is a legal request");
        assert!(clamped.is_empty(), "zero is under the ceiling, not past it");
        assert_eq!(config.model_timeout, None);
        assert!(
            EngineConfig::default().model_timeout.is_some(),
            "the default must be bounded, or this test proves nothing"
        );
    }

    /// A typoed knob is refused rather than silently ignored — the mirror
    /// image of the silent-honoring the ceilings exist to prevent.
    #[test]
    fn an_unknown_engine_knob_fails_the_parse() {
        let result: Result<EngineOverrides, _> =
            serde_json::from_value(serde_json::json!({ "temprature": 0.5 }));
        assert!(result.is_err(), "unknown engine fields must be refused");
    }

    /// The operator-policy knobs are not reachable through the engine object:
    /// a caller must not be able to widen retries or disable loop detection.
    #[test]
    fn operator_policy_knobs_are_not_caller_settable() {
        for json in [
            serde_json::json!({ "retry_policy": {} }),
            serde_json::json!({ "loop_detection": {} }),
            serde_json::json!({ "max_steps": 5 }),
        ] {
            let result: Result<EngineOverrides, _> = serde_json::from_value(json.clone());
            assert!(
                result.is_err(),
                "operator knob leaked into the wire: {json}"
            );
        }
    }
}
