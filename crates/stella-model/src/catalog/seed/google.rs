//! Google's rows, on both surfaces that serve them.
//!
//! Rows are seeded rather than left to `stella models refresh` because a
//! frozen container cannot refresh: an unseeded slug is unreachable there,
//! not merely unpriced.
//!
//! `gemini` and `vertex` are one model reached two ways, which is why
//! uniqueness is keyed on `(provider, id)` rather than `id` alone. They share
//! a file so a ceiling correction cannot land on one surface and miss the
//! other.

use crate::catalog::{CatalogEntry, Pricing, ToolDialect};

/// The rows this provider contributes to
/// [`Catalog::seed`](crate::catalog::Catalog::seed).
pub(super) fn rows() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry::new(
            "gemini-3-pro",
            "gemini",
            "gemini",
            1_000_000,
            ToolDialect::GeminiFunctions,
            Pricing {
                input_usd_per_mtok: 1.25,
                output_usd_per_mtok: 10.00,
                cached_input_usd_per_mtok: 0.31,
                cache_write_usd_per_mtok: 1.25,
            },
        )
        .with_reasoning(Some(true))
        // models.dev `limit.output` for `google/gemini-3-pro-preview`,
        // read 2026-08-03 (#1290) — the pre-GA name for this same
        // model, which is the closest citable source; models.dev has
        // no bare `gemini-3-pro` row. Gemini's own listing publishes
        // `outputTokenLimit`, which `parse_gemini_page` already reads,
        // so a refresh replaces this with the provider's own number.
        .with_max_output_tokens(Some(65_536)),
        // The same Google model surfaced through Vertex AI — one
        // model genuinely existing on two providers is why
        // uniqueness (and `resolve_for`) is keyed on
        // (provider, id), not id alone. Same list price as the
        // Gemini-direct row above.
        CatalogEntry::new(
            "gemini-3-pro",
            "vertex",
            "gemini",
            1_000_000,
            ToolDialect::GeminiFunctions,
            Pricing {
                input_usd_per_mtok: 1.25,
                output_usd_per_mtok: 10.00,
                cached_input_usd_per_mtok: 0.31,
                cache_write_usd_per_mtok: 1.25,
            },
        )
        .with_reasoning(Some(true))
        // Same model, same ceiling, different surface — Vertex serves
        // Google's model, not a different one. Moves with the row
        // above for the same reason the Anthropic direct/gateway pair
        // does: route is not a model property (#1290).
        .with_max_output_tokens(Some(65_536)),
    ]
}
