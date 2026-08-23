//! Amazon Bedrock's rows.
//!
//! Rows are seeded rather than left to `stella models refresh` because a
//! frozen container cannot refresh: an unseeded slug is unreachable there,
//! not merely unpriced.
//!
//! Bedrock's listing is not one of the shapes `provider_listing` can read, so
//! these rows cannot self-correct from a refresh and the seeded numbers are
//! the only ones a user gets.

use crate::catalog::{CatalogEntry, Pricing, ToolDialect};

/// The rows this provider contributes to
/// [`Catalog::seed`](crate::catalog::Catalog::seed).
pub(super) fn rows() -> Vec<CatalogEntry> {
    vec![
        // A cross-region inference profile, not a bare model id —
        // Bedrock rejects on-demand invocation of newer Anthropic
        // models without one. Priced as Claude Sonnet 4.5 (Bedrock
        // on-demand list price).
        CatalogEntry::new(
            "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
            "bedrock",
            "claude",
            200_000,
            ToolDialect::BedrockConverse,
            Pricing {
                input_usd_per_mtok: 3.00,
                output_usd_per_mtok: 15.00,
                cached_input_usd_per_mtok: 0.30,
                cache_write_usd_per_mtok: 3.75,
            },
        )
        .with_reasoning(Some(true))
        // The ceiling belongs to the MODEL, so it is the same 64000
        // Anthropic reports for `claude-sonnet-4-5-20250929` on its own
        // `/v1/models` (checked 2026-08-03, #1290) — Bedrock serves
        // that model behind an inference profile, it does not serve a
        // different one with a different ceiling. Seeded because
        // Bedrock's own listing is not one of the shapes
        // `provider_listing` can read, so this row cannot self-correct
        // from a refresh the way the others now can.
        .with_max_output_tokens(Some(64_000)),
    ]
}
