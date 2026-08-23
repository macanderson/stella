//! xAI's first-party rows.
//!
//! Rows are seeded rather than left to `stella models refresh` because a
//! frozen container cannot refresh: an unseeded slug is unreachable there,
//! not merely unpriced.
//!
//! xAI speaks the OpenAI-compatible listing shape, so
//! `parse_openai_compatible` raises these ceilings to the provider's own
//! figures on the first refresh that finds one.

use crate::catalog::{CatalogEntry, Pricing, ToolDialect};

/// The rows this provider contributes to
/// [`Catalog::seed`](crate::catalog::Catalog::seed).
pub(super) fn rows() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry::new(
            "grok-4",
            "xai",
            "grok",
            256_000,
            ToolDialect::OpenaiJson,
            Pricing {
                input_usd_per_mtok: 3.00,
                output_usd_per_mtok: 15.00,
                cached_input_usd_per_mtok: 0.75,
                cache_write_usd_per_mtok: 3.00,
            },
        )
        .with_reasoning(Some(true))
        // 30000 — the highest cap reported for ANY grok-4-generation
        // model, and deliberately not the highest number in the wider
        // family (#1290).
        //
        // models.dev carries no bare `grok-4` row. Its generation
        // siblings agree at 30000 (`grok-4.3`, and all three
        // `grok-4.20-*` variants); `grok-4.5` reports 500000, but that
        // is a later generation and taking its number would be reading
        // a different model's ceiling.
        //
        // The two directions of being wrong here are NOT symmetric,
        // which is what decides the value. Guessing LOW costs some
        // unused headroom on long answers. Guessing HIGH costs a
        // provider-side rejection on every single request — the model
        // never runs, the developer pays for the round trip, and the
        // work needs another call to retry. So where the family
        // disagrees, the ceiling has to be the one no member of the
        // generation would refuse.
        //
        // Left unseeded this row inherited the engine's global 16384,
        // which truncates real work — so "no number" was never the
        // safe option it looks like. xAI speaks the OpenAI-compatible
        // listing shape, so `parse_openai_compatible` raises this to
        // the provider's own figure on the first refresh that finds
        // one; this is the offline floor, not a claim to have looked
        // it up.
        .with_max_output_tokens(Some(30_000)),
    ]
}
