//! OpenAI's first-party rows.
//!
//! Rows are seeded rather than left to `stella models refresh` because a
//! frozen container cannot refresh: an unseeded slug is unreachable there,
//! not merely unpriced.
//!
//! `OpenaiResponses` is the Responses API's item-based dialect, structurally
//! distinct from the `OpenaiJson` Chat Completions shape every
//! OpenAI-compatible gateway speaks.

use crate::catalog::{CatalogEntry, Pricing, ToolDialect};

/// The rows this provider contributes to
/// [`Catalog::seed`](crate::catalog::Catalog::seed).
pub(super) fn rows() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry::new(
            "gpt-5.5",
            "openai",
            "gpt",
            400_000,
            ToolDialect::OpenaiResponses,
            Pricing {
                input_usd_per_mtok: 1.25,
                output_usd_per_mtok: 10.00,
                cached_input_usd_per_mtok: 0.125,
                cache_write_usd_per_mtok: 1.25,
            },
        )
        .with_reasoning(Some(true))
        // models.dev `limit.output` for `openai/gpt-5.5`, read
        // 2026-08-03 (#1290).
        .with_max_output_tokens(Some(128_000)),
    ]
}
