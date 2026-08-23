//! Anthropic's first-party Claude rows.
//!
//! Rows are seeded rather than left to `stella models refresh` because a
//! frozen container cannot refresh: an unseeded slug is unreachable there,
//! not merely unpriced.
//!
//! Every row here has a gateway twin in [`super::openrouter`], and the two
//! must agree on the model's own ceiling:
//! `a_models_ceiling_does_not_depend_on_the_route_it_is_reached_through`
//! fails when only one side moves.

use crate::catalog::{CatalogEntry, Pricing, ToolDialect};

/// The rows this provider contributes to
/// [`Catalog::seed`](crate::catalog::Catalog::seed).
pub(super) fn rows() -> Vec<CatalogEntry> {
    vec![
        // Anthropic's mainstream model, and the one a head-to-head is
        // most likely to be run on. Its absence was not a gap in
        // coverage so much as a hard stop: the seed is the OFFLINE
        // floor, `stella models refresh` is the only way to grow the
        // runtime catalog, and a sandboxed benchmark container runs
        // with `STELLA_CATALOG_AUTO_REFRESH=0` — so an unlisted model
        // there is unreachable, not merely unpriced.
        CatalogEntry::new(
            "claude-sonnet-5",
            "anthropic",
            "claude",
            1_000_000,
            ToolDialect::AnthropicTools,
            // List price. An introductory rate runs to 2026-08-31
            // ($2.00/$10.00); the seed carries the durable number, so
            // cost telemetry over-states slightly until then. That is
            // the safe direction for a spend guard and it corrects
            // itself without an edit.
            Pricing {
                input_usd_per_mtok: 3.00,
                output_usd_per_mtok: 15.00,
                cached_input_usd_per_mtok: 0.30,
                cache_write_usd_per_mtok: 3.75,
            },
        )
        .with_reasoning(Some(true))
        // The model's own completion ceiling. Seeded, not left to a
        // refresh: the runtime catalog only carries card data after
        // `stella models refresh`, and a benchmark container runs
        // frozen (`STELLA_CATALOG_AUTO_REFRESH=0`), so an unseeded
        // ceiling is `None` everywhere it matters and the engine
        // silently falls back to its global 16384. Measured off the
        // wire before this line existed: a trial-shaped run sent
        // `max_tokens: 16384` for a model whose ceiling is 64000.
        //
        // 128000, from the provider, on 2026-08-03 (#1290). This row
        // read 64000 until then, and 64000 was never the model's
        // ceiling — it was where Claude Code's steps were *measured*
        // stopping, which is a fact about the comparator's posture,
        // not about Sonnet 5. Two independent sources say 128000:
        // Anthropic's own `GET /v1/models` reports
        // `"max_tokens": 128000` for `claude-sonnet-5`, and
        // OpenRouter reports `top_provider.max_completion_tokens =
        // 128000` for the same model on the gateway route.
        //
        // This is the user-facing half of #1290 and it was not a
        // benchmark detail: the catalog ceiling is the cap EVERY
        // stella user gets on this model, so while it read 64000 every
        // long answer was cut at half the height the model can write,
        // for no reason anyone had chosen. The benchmark's own 64000
        // survives on purpose and is now declared in `posture.py`
        // rather than inherited from here.
        //
        // Why 128000 is worth the gateway refusal risk (#3757). A
        // gateway prices the credit *check* against the ceiling the
        // caller asks for rather than the tokens it will spend, which
        // is why OpenRouter refuses this row with `can only afford M`
        // against a balance that would fund the real call many times
        // over — the one `OutputBudgetPosture::Detected` row in
        // `provider_parity.rs`.
        //
        // That is a recovered failure now, not a terminal one, and the
        // mechanism is documented where it lives rather than restated
        // here: see `stella_core::driver::output_budget_recovery`'s
        // module doc for the clamp-and-re-run and how often a session
        // re-probes. The trade is that bounded round-trip against
        // permanently halving what every user's long answers may hold.
        //
        // The measurement in #3757 cannot decide this: a ceiling
        // bounds the longest answer, not the median, so "299 output
        // tokens per call" is evidence either way. Pinned by
        // `an_unconfigured_run_asks_for_the_models_whole_output_budget`
        // in `stella-cli`, so lowering it argues with an assertion
        // rather than slipping through a struct literal.
        .with_max_output_tokens(Some(128_000)),
        // The previous-generation mainstream Sonnet. Seeded for the
        // same hard-stop reason as its successor above: a frozen
        // container cannot refresh, so an unlisted slug is
        // unreachable rather than unpriced — and Sonnet 4.6 is what
        // an account still awaiting current-generation enablement
        // (Bedrock entitlements, staged first-party rollouts) can
        // actually run. Same list price as Sonnet 5.
        CatalogEntry::new(
            "claude-sonnet-4-6",
            "anthropic",
            "claude",
            1_000_000,
            ToolDialect::AnthropicTools,
            Pricing {
                input_usd_per_mtok: 3.00,
                output_usd_per_mtok: 15.00,
                cached_input_usd_per_mtok: 0.30,
                cache_write_usd_per_mtok: 3.75,
            },
        )
        .with_reasoning(Some(true))
        // Seeded for the same reason as its successor above, and
        // corrected in the same sweep: this row also read 64000,
        // copied from the same place. Anthropic's `GET /v1/models`
        // reports `"max_tokens": 128000` for `claude-sonnet-4-6`
        // (checked 2026-08-03, #1290). Not benchmarked, but it is a
        // model a user can select, and the ceiling is what they get.
        .with_max_output_tokens(Some(128_000)),
        CatalogEntry::new(
            "claude-fable-5",
            "anthropic",
            "claude",
            // Was 200_000 with Sonnet's prices — wrong on both counts.
            // A catalog entry is not documentation: `cost_usd` is
            // computed from it, so a stale row silently under-reports
            // spend by 3.3x on every Fable turn, and the context
            // figure decides when compaction fires.
            1_000_000,
            ToolDialect::AnthropicTools,
            Pricing {
                input_usd_per_mtok: 10.00,
                output_usd_per_mtok: 50.00,
                cached_input_usd_per_mtok: 1.00,
                cache_write_usd_per_mtok: 12.50,
            },
        )
        .with_reasoning(Some(true))
        // Fable's own ceiling, which is NOT Sonnet's. This row read
        // 64000 because it was copied from the Sonnet 5 row above,
        // and the copy was wrong: Anthropic's `/v1/models` reports
        // 128000 for Fable 5. The benchmark posture is pinned to this
        // number (`TestOutputCeilingParity`), so while it was low
        // every Fable trial stopped at half the height the comparator
        // is allowed to fill — a self-imposed handicap the score then
        // reports as a capability difference. Approved as the Fable
        // ceiling set (#1211 §6.2).
        .with_max_output_tokens(Some(128_000)),
        // Opus 5 was absent from this catalog on both routes, so any
        // role pinned to it refused the run rather than falling back —
        // correct behaviour, and useless to a benchmark that wants to
        // seat a worker on Opus-tier. Seeded here and on `openrouter`
        // together, because `catalog.rs`'s parity tests assert the two
        // rows agree and a one-sided add is the failure they exist to
        // catch.
        //
        // Half Fable's price at the same 1M window: $5/$25 per MTok
        // against Fable's $10/$50 (checked 2026-08-06). Copying the
        // Fable row would have doubled the reported spend on every
        // Opus turn — the same class of error the Fable row's own
        // comment above records, in the other direction.
        CatalogEntry::new(
            "claude-opus-5",
            "anthropic",
            "claude",
            1_000_000,
            ToolDialect::AnthropicTools,
            Pricing {
                input_usd_per_mtok: 5.00,
                output_usd_per_mtok: 25.00,
                cached_input_usd_per_mtok: 0.50,
                cache_write_usd_per_mtok: 6.25,
            },
        )
        .with_reasoning(Some(true))
        .with_max_output_tokens(Some(128_000)),
    ]
}
