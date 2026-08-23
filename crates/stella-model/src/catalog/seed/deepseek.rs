//! DeepSeek's first-party rows.
//!
//! Rows are seeded rather than left to `stella models refresh` because a
//! frozen container cannot refresh: an unseeded slug is unreachable there,
//! not merely unpriced.

use crate::catalog::{CatalogEntry, Pricing, ToolDialect};

/// The rows this provider contributes to
/// [`Catalog::seed`](crate::catalog::Catalog::seed).
pub(super) fn rows() -> Vec<CatalogEntry> {
    vec![
        // The non-thinking chat model (`deepseek-reasoner` is the
        // reasoning one) — the seed's one honest `Some(false)`.
        CatalogEntry::new(
            "deepseek-chat",
            "deepseek",
            "deepseek",
            128_000,
            ToolDialect::OpenaiJson,
            Pricing {
                input_usd_per_mtok: 0.27,
                output_usd_per_mtok: 1.10,
                cached_input_usd_per_mtok: 0.07,
                cache_write_usd_per_mtok: 0.27,
            },
        )
        .with_reasoning(Some(false))
        // models.dev `limit.output` for `deepseek/deepseek-chat`, read
        // 2026-08-03 (#1290). Larger than the Anthropic family's, which
        // is the point of reading it per model instead of sharing one.
        .with_max_output_tokens(Some(384_000)),
    ]
}
