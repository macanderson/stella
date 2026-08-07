//! Derive [`stella_tui::Inbound::CacheInsight`] from a committed
//! `AgentEvent::StepUsage` (issues #267/#269) — split out of
//! `command_deck.rs` (already oversized; not a file to
//! grow) so `spawn_forwarder`, the one seam shared by every deck lane (the
//! lead's turns and every `crate::subsession` worker), only needs a call
//! site.
//!
//! `StepUsage` already carries the raw token counts the deck folds into the
//! CACHE cell; this adds the three figures that need list pricing, the TTL
//! table, and the cache-posture taxonomy — `stella-model` concerns the
//! model-tier-free `stella-tui` cannot reach — computed once here from the
//! provider id the forwarder already owns. Without this seam the
//! SAVED/WARMTH statline cells are permanently "—" outside a hand-built test
//! fixture: `Inbound::CacheInsight` had no producer anywhere on the live
//! path. `is_opt_in_provider` similarly lets
//! [`stella_tui::AgentEntry::cache_diagnosis`] name
//! `CacheCause::OptInNeverEngaged` without the deck knowing which providers
//! require an explicit cache marker.

use stella_model::provider_parity::{CachePosture, cache_posture};
use stella_model::{CacheTtl, Catalog, configured_cache_ttl_secs};
use stella_protocol::{AgentEvent, CompletionUsage};
use stella_tui::Inbound;

/// The session facts the forwarder needs to derive a `CacheInsight`: the
/// provider id it already carried, plus the configured prompt-cache window
/// (#1839) — the deck's warmth countdown and SAVED cell must reflect the
/// window the session actually asked for, not assert the 5-minute default.
/// One struct rather than a second `spawn_forwarder` parameter so the deck's
/// call sites (in a file closed to growth) stay one argument wide.
#[derive(Clone)]
pub(crate) struct InsightScope {
    pub(crate) provider_id: String,
    pub(crate) cache_ttl: CacheTtl,
}

impl InsightScope {
    pub(crate) fn from_config(cfg: &crate::config::Config) -> Self {
        Self {
            provider_id: cfg.provider.id.to_string(),
            cache_ttl: cfg.effective_cache_ttl(),
        }
    }
}

/// `None` for every event variant but `StepUsage`. An unresolvable
/// `(provider, model)` — a retired or custom catalog entry — reports `0.0`
/// savings rather than dropping the insight: the TTL half (from the
/// provider id and configured window alone) still stands, and a silent $0
/// delta is honest where a missing warmth countdown would not be.
pub(crate) fn cache_insight_for(
    scope: &InsightScope,
    lane: &str,
    event: &AgentEvent,
) -> Option<Inbound> {
    let AgentEvent::StepUsage {
        model,
        input_tokens,
        output_tokens,
        cached_input_tokens,
        cache_write_tokens,
        complete,
        ..
    } = event
    else {
        return None;
    };
    let usage = CompletionUsage {
        reported: *complete,
        input_tokens: *input_tokens,
        output_tokens: *output_tokens,
        cached_input_tokens: *cached_input_tokens,
        cache_write_tokens: *cache_write_tokens,
        // Not carried through, and not a loss: this envelope exists only to
        // feed `cache_savings_usd_for`, and reasoning tokens are already
        // inside `output_tokens` — a diagnostic split, never their own cost
        // line. `None` is also the honest value here: nothing on this path
        // observed a reasoning breakdown.
        reasoning_tokens: None,
    };
    let provider_id = scope.provider_id.as_str();
    let savings_usd_delta = Catalog::current()
        .resolve_for(provider_id, model)
        .map(|entry| {
            entry
                .pricing
                .cache_savings_usd_configured(provider_id, &usage, scope.cache_ttl)
        })
        .unwrap_or(0.0);
    let is_opt_in_provider = matches!(cache_posture(provider_id), Some(CachePosture::OptIn { .. }));
    Some(Inbound::CacheInsight {
        agent: lane.to_string(),
        savings_usd_delta,
        ttl_secs: configured_cache_ttl_secs(provider_id, scope.cache_ttl).unwrap_or(0),
        is_opt_in_provider,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default-window scope most tests observe through.
    fn scope(provider_id: &str) -> InsightScope {
        InsightScope {
            provider_id: provider_id.to_string(),
            cache_ttl: CacheTtl::FiveMinutes,
        }
    }

    fn step_usage(model: &str, input: u64, cached: u64, write: u64) -> AgentEvent {
        AgentEvent::StepUsage {
            reasoning_tokens: None,
            output_text: None,
            step: 1,
            role: stella_protocol::event::ModelCallRole::Worker,
            provider: "test".into(),
            model: model.to_string(),
            input_tokens: input,
            output_tokens: 0,
            cached_input_tokens: cached,
            cache_write_tokens: write,
            estimated_input_tokens: 0,
            cost_usd: 0.0,
            duration_ms: 1,
            retries: 0,
            tool_calls: 0,
            complete: true,
            finish_reason: None,
        }
    }

    #[test]
    fn ignores_every_non_step_usage_event() {
        let text = AgentEvent::Text { text: "hi".into() };
        assert!(cache_insight_for(&scope("anthropic"), "lead", &text).is_none());
    }

    #[test]
    fn matches_the_pricing_witness_from_stella_model() {
        // What this pins is the WIRING — that a `StepUsage` event reaches the
        // catalog and comes back with that model's real saving. The arithmetic
        // itself is pinned separately, against a hand-built `Pricing`, by
        // stella-model's `savings_matches_catalog_pricing_math_by_hand`.
        //
        // So the expectation is derived from the catalog rather than copied out
        // of it. A hardcoded figure here fails the moment a price is corrected
        // — as it did when `claude-fable-5` was carrying Sonnet's $3/$15
        // instead of its actual $10/$50 — which frames a fix to real data as a
        // broken test.
        let event = step_usage("claude-fable-5", 1_000_000, 400_000, 100_000);
        let pricing = Catalog::current()
            .resolve_for("anthropic", "claude-fable-5")
            .expect("the seed catalog carries this model")
            .pricing;
        let expected = pricing.cache_savings_usd(
            &CompletionUsage {
                reasoning_tokens: None,
                reported: true,
                input_tokens: 1_000_000,
                output_tokens: 0,
                cached_input_tokens: 400_000,
                cache_write_tokens: 100_000,
            },
            pricing.input_usd_per_mtok * 0.25,
        );
        assert!(
            expected > 0.0,
            "the fixture must exercise a model whose cache actually pays"
        );
        let insight = cache_insight_for(&scope("anthropic"), "lead", &event)
            .expect("StepUsage yields an insight");
        match insight {
            Inbound::CacheInsight {
                agent,
                savings_usd_delta,
                ttl_secs,
                is_opt_in_provider,
            } => {
                assert_eq!(agent, "lead");
                assert!(
                    (savings_usd_delta - expected).abs() < 1e-9,
                    "got {savings_usd_delta}, catalog says {expected}"
                );
                // Anthropic's default prompt-cache TTL is 5 minutes.
                assert_eq!(ttl_secs, 300);
                // Anthropic requires the explicit cache_control marker.
                assert!(is_opt_in_provider);
            }
            other => panic!("expected CacheInsight, got {other:?}"),
        }
    }

    /// **Witness (#1839).** A session that configured the 1-hour window must
    /// see it in the insight — the warmth countdown counts down 3600s, and
    /// the SAVED figure charges the 2x write premium instead of the catalog
    /// row's 5-minute rate. Before the knob existed this seam asserted 300s
    /// unconditionally.
    #[test]
    fn a_configured_one_hour_window_reaches_ttl_and_savings() {
        let one_hour = InsightScope {
            provider_id: "anthropic".to_string(),
            cache_ttl: CacheTtl::OneHour,
        };
        let event = step_usage("claude-fable-5", 1_000_000, 400_000, 100_000);
        let pricing = Catalog::current()
            .resolve_for("anthropic", "claude-fable-5")
            .expect("the seed catalog carries this model")
            .pricing;
        let expected = pricing.cache_savings_usd(
            &CompletionUsage {
                reasoning_tokens: None,
                reported: true,
                input_tokens: 1_000_000,
                output_tokens: 0,
                cached_input_tokens: 400_000,
                cache_write_tokens: 100_000,
            },
            // 1-hour writes bill 2x input: the premium over base is 1x.
            pricing.input_usd_per_mtok,
        );
        let insight =
            cache_insight_for(&one_hour, "lead", &event).expect("StepUsage yields an insight");
        match insight {
            Inbound::CacheInsight {
                savings_usd_delta,
                ttl_secs,
                ..
            } => {
                assert_eq!(ttl_secs, 3_600, "the configured window, not the default");
                assert!(
                    (savings_usd_delta - expected).abs() < 1e-9,
                    "got {savings_usd_delta}, want the 2x-write-premium figure {expected}"
                );
            }
            other => panic!("expected CacheInsight, got {other:?}"),
        }
    }

    #[test]
    fn implicit_provider_is_not_marked_opt_in() {
        // zai auto-caches with no marker — the opt-in-never-engaged diagnosis
        // must never fire for it, however low the hit rate runs.
        let event = step_usage("glm-5.2", 1_000_000, 0, 0);
        let insight =
            cache_insight_for(&scope("zai"), "lead", &event).expect("StepUsage yields an insight");
        match insight {
            Inbound::CacheInsight {
                is_opt_in_provider, ..
            } => assert!(!is_opt_in_provider),
            other => panic!("expected CacheInsight, got {other:?}"),
        }
    }

    #[test]
    fn reports_zero_savings_for_an_unresolvable_model_but_keeps_the_ttl() {
        // A retired/custom slug the catalog cannot price: savings is an
        // honest $0 (never a guess), but the TTL still resolves from the
        // provider id alone, so the warmth countdown does not go dark too.
        let event = step_usage("made-up-model-9000", 1_000_000, 400_000, 100_000);
        let insight = cache_insight_for(&scope("anthropic"), "lead", &event)
            .expect("StepUsage yields an insight");
        match insight {
            Inbound::CacheInsight {
                savings_usd_delta,
                ttl_secs,
                ..
            } => {
                assert_eq!(savings_usd_delta, 0.0);
                assert_eq!(ttl_secs, 300);
            }
            other => panic!("expected CacheInsight, got {other:?}"),
        }
    }

    #[test]
    fn ttl_is_zero_for_a_provider_with_no_documented_cache_window() {
        let event = step_usage("glm-5.2", 1_000_000, 400_000, 0);
        let insight =
            cache_insight_for(&scope("zai"), "lead", &event).expect("StepUsage yields an insight");
        match insight {
            Inbound::CacheInsight { ttl_secs, .. } => assert_eq!(ttl_secs, 0),
            other => panic!("expected CacheInsight, got {other:?}"),
        }
    }
}
