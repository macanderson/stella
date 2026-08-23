use super::*;

/// Every provider's `default_model` here must resolve against
/// `stella_model::catalog::Catalog::seed()`, or `build_provider`'s
/// catalog check (`agent.rs`) would hard-error on first use of a
/// provider whose default was never added to the seed. Uses the
/// provider-scoped resolver, same as `build_provider`, so a default that
/// only exists under a *different* provider's row still fails here.
#[test]
fn every_provider_default_model_resolves_against_the_catalog_seed() {
    let catalog = stella_model::catalog::Catalog::seed();
    for provider in PROVIDERS {
        catalog
            .resolve_for(provider.id, provider.default_model)
            .unwrap_or_else(|e| {
                panic!(
                    "provider `{}`'s default_model `{}` is not in the catalog seed: {e}",
                    provider.id, provider.default_model
                )
            });
    }
}

#[test]
fn provider_ids_are_unique() {
    let mut ids: Vec<&str> = PROVIDERS.iter().map(|p| p.id).collect();
    ids.push(LOCAL_PROVIDER.id);
    let before = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), before, "duplicate provider id in PROVIDERS");
}

/// Every seeded provider must declare its prompt-cache posture in
/// stella-model's parity matrix — the guard born from OpenRouter
/// silently running Claude with zero caching. A new provider cannot
/// land without stating how caching is engaged and naming the witness
/// test that proves it.
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
/// deliberate. A new provider cannot land without stating its reasoning
/// posture and naming the witness that proves a `Controllable` control on the
/// wire.
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

/// The output-budget-axis sibling: every seeded provider must declare
/// whether its refusal to fund the *requested output ceiling* is recognised.
/// Without a row, such a rejection silently misses the engine's clamp ladder
/// and kills the turn — which is what happened to three benchmark runs
/// against a balance that could still fund the work at a smaller ask.
#[test]
fn every_seeded_provider_declares_an_output_budget_posture() {
    for provider in PROVIDERS.iter().chain(std::iter::once(&LOCAL_PROVIDER)) {
        assert!(
            stella_model::provider_parity::output_budget_posture(provider.id).is_some(),
            "provider `{}` has no OutputBudgetPosture row in \
             stella-model/src/provider_parity.rs — add it (with a witness test for a \
             Detected signature, or a note for a BestEffort row) in this PR",
            provider.id
        );
    }
}

/// The stream-fallback-axis sibling. This axis shipped with neither half of
/// the both-sides enforcement AGENTS.md describes — no completeness test here
/// and no witness-existence test in `stella-model` (that half landed in the
/// same PR). A provider with no row silently declares nothing about how it
/// behaves when its streaming path breaks before the first byte, which is the
/// fault #2686 exists to recover from.
#[test]
fn every_seeded_provider_declares_a_stream_fallback_posture() {
    for provider in PROVIDERS.iter().chain(std::iter::once(&LOCAL_PROVIDER)) {
        assert!(
            stella_model::provider_parity::stream_fallback_posture(provider.id).is_some(),
            "provider `{}` has no StreamFallbackPosture row in \
             stella-model/src/provider_parity.rs — add it (with a witness test for a \
             UnaryFallback, or a note for a StreamingOnly/AlwaysUnary row) in this PR",
            provider.id
        );
    }
}

/// The parallel-tool-call-axis sibling (#4163): every seeded provider must
/// declare whether several tool calls ride one assistant message, and name
/// the test proving this adapter fans several of them in.
///
/// The engine dispatches consecutive read-only calls concurrently and the
/// system prompt asks the model to send independent calls together — so this
/// is a capability the working surface *already depends on*, and until this
/// axis existed no provider stated it and no test pinned it.
#[test]
fn every_seeded_provider_declares_a_parallel_tool_call_posture() {
    for provider in PROVIDERS.iter().chain(std::iter::once(&LOCAL_PROVIDER)) {
        assert!(
            stella_model::provider_parity::parallel_tool_call_posture(provider.id).is_some(),
            "provider `{}` has no ParallelToolCallPosture row in \
             stella-model/src/provider_parity.rs — add it (naming the fan-in witness, plus \
             either an observation that parallel calls are admitted by default or a note \
             saying the admission question is undetermined) in this PR",
            provider.id
        );
    }
}
