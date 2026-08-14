//! Seeded-provider completeness for the parity matrix, one test per axis.
//!
//! A sibling module rather than more lines in [the parent](super) — same
//! reason as `trusted_engine.rs`: the parent sits against the 1500-line
//! ratchet, and these tests are one subject. Each asserts that every provider
//! id the CLI can construct declares a row on one axis of
//! `stella-model/src/provider_parity.rs` (AGENTS.md invariant #8), so a new
//! provider cannot land without stating its posture — the enforcement half
//! this side of the crate boundary; the witness-existence half lives in
//! `stella-model`'s own parity tests.

use super::*;

/// Every seeded provider must declare its prompt-cache posture — the guard
/// born from OpenRouter silently running Claude with zero caching. A new
/// provider cannot land without stating how caching is engaged and naming
/// the witness test that proves it.
#[test]
fn every_seeded_provider_declares_a_cache_posture() {
    for provider in PROVIDERS.iter().chain(std::iter::once(&LOCAL_PROVIDER)) {
        assert!(
            stella_model::provider_parity::cache_posture(provider.id).is_some(),
            "provider `{}` has no row in stella-model/src/provider_parity.rs — \
             add its CachePosture (with a witness test) in this PR",
            provider.id
        );
    }
}

/// The reasoning-axis sibling of the cache-posture guard: every seeded
/// provider must declare how its reasoning/thinking budget is controlled (or
/// that the shared adapter deliberately drops it). Born from the same silent
/// per-provider divergence — a pinned effort reaching only Z.ai and OpenRouter
/// and being dropped everywhere else with nothing enforcing the omission stays
/// deliberate.
#[test]
fn every_seeded_provider_declares_a_reasoning_posture() {
    for provider in PROVIDERS.iter().chain(std::iter::once(&LOCAL_PROVIDER)) {
        assert!(
            stella_model::provider_parity::reasoning_posture(provider.id).is_some(),
            "provider `{}` has no ReasoningPosture row in \
             stella-model/src/provider_parity.rs — add it (with a witness test for a \
             Controllable control, or a note for a no-control posture) in this PR",
            provider.id
        );
    }
}

/// The overflow-axis sibling (#2680): every seeded provider must declare how
/// it signals a context-window overflow — either a verified wire signature
/// with a witness test (`Detected`), or an explicit note that detection is
/// best-effort through the shared funnel (`BestEffort`). Without a row, a
/// provider's overflow rejections silently miss the engine's reactive
/// recovery and abort the turn with nothing enforcing that the omission was
/// deliberate.
#[test]
fn every_seeded_provider_declares_an_overflow_posture() {
    for provider in PROVIDERS.iter().chain(std::iter::once(&LOCAL_PROVIDER)) {
        assert!(
            stella_model::provider_parity::overflow_posture(provider.id).is_some(),
            "provider `{}` has no OverflowPosture row in \
             stella-model/src/provider_parity.rs — add it (with a witness test for a \
             Detected signature, or a note for a BestEffort row) in this PR",
            provider.id
        );
    }
}

/// The stream-fallback-axis sibling (#2686): every seeded provider must
/// declare how it behaves when its streaming path breaks — a unary fallback
/// with a witness, or an explicit note that the dialect streams only / is
/// already unary. This axis shipped without a completeness guard while the
/// other three had one; added alongside the billing guard below so no axis
/// can gain a provider hole silently.
#[test]
fn every_seeded_provider_declares_a_stream_fallback_posture() {
    for provider in PROVIDERS.iter().chain(std::iter::once(&LOCAL_PROVIDER)) {
        assert!(
            stella_model::provider_parity::stream_fallback_posture(provider.id).is_some(),
            "provider `{}` has no StreamFallbackPosture row in \
             stella-model/src/provider_parity.rs — add it (with a witness test for a \
             UnaryFallback row, or a note for a no-fallback posture) in this PR",
            provider.id
        );
    }
}

/// The billing-axis sibling: every seeded provider must declare how an
/// out-of-credit rejection behaves — an afford-hint retry with a witness
/// (OpenRouter's 402 names the output ceiling the balance can still fund),
/// or an explicit note that billing rejections stay terminal. Born from
/// three bench runs lost whole to recoverable 402s (h2h891, fivetools5,
/// gate89high1).
#[test]
fn every_seeded_provider_declares_a_billing_posture() {
    for provider in PROVIDERS.iter().chain(std::iter::once(&LOCAL_PROVIDER)) {
        assert!(
            stella_model::provider_parity::billing_posture(provider.id).is_some(),
            "provider `{}` has no BillingPosture row in \
             stella-model/src/provider_parity.rs — add it (with a witness test for an \
             AffordHintRetry row, or a note for a TerminalAbort row) in this PR",
            provider.id
        );
    }
}
