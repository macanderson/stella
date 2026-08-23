//! z.ai's first-party GLM rows.
//!
//! Rows are seeded rather than left to `stella models refresh` because a
//! frozen container cannot refresh: an unseeded slug is unreachable there,
//! not merely unpriced.
//!
//! The worker plus the two roles a GLM run needs that its worker cannot fill
//! — a model does not corroborate itself, so verifier and triage seat
//! elsewhere in the family.

use crate::catalog::{CatalogEntry, Pricing, ToolDialect};

/// The rows this provider contributes to
/// [`Catalog::seed`](crate::catalog::Catalog::seed).
pub(super) fn rows() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry::new(
            "glm-5.2",
            "zai",
            "glm",
            200_000,
            ToolDialect::OpenaiJson,
            Pricing {
                input_usd_per_mtok: 0.60,
                output_usd_per_mtok: 2.20,
                cached_input_usd_per_mtok: 0.11,
                cache_write_usd_per_mtok: 0.60,
            },
        )
        .with_reasoning(Some(true))
        // models.dev `limit.output` for `zai/glm-5.2`, read 2026-08-03
        // (#1290) — the same authority `stella models refresh` merges
        // from, so a later refresh confirms this rather than fighting
        // it. Seeded so the offline floor is not the engine's 16384.
        .with_max_output_tokens(Some(131_072)),
        // The two roles a GLM pipeline needs that its *worker* cannot
        // fill. A head-to-head pins the worker to the opponent's model,
        // so verifier and triage have to be somebody else — and the
        // authored-witness tier refuses outright when the verifier
        // resolves to the worker, on the grounds that a model cannot
        // independently corroborate itself. Seeded for exactly the
        // reason given for `claude-sonnet-5` in `seed/anthropic.rs`:
        // unlisted here is
        // *unreachable* inside a benchmark container, not merely
        // unpriced, and a run configured with one dies at startup
        // having emitted nothing to say why.
        //
        // `glm-5.1` is the strongest z.ai model that is not the worker,
        // which is what a verifier and witness author ought to be.
        CatalogEntry::new(
            "glm-5.1",
            "zai",
            "glm",
            204_800,
            ToolDialect::OpenaiJson,
            // OpenRouter's published rates for `z-ai/glm-5.1`, read
            // 2026-08-04. Cache writes are not billed separately, so
            // that field carries the input rate rather than a guess.
            Pricing {
                input_usd_per_mtok: 0.966,
                output_usd_per_mtok: 3.036,
                cached_input_usd_per_mtok: 0.1794,
                cache_write_usd_per_mtok: 0.966,
            },
        )
        .with_reasoning(Some(true)),
        // `glm-4.5-air` is the cheap end of the family, which is the
        // right end for triage: it emits a three-line classification
        // and never touches the workspace, so a frontier model there
        // buys nothing and bills a reasoning pass per turn for a
        // decision that needs none.
        CatalogEntry::new(
            "glm-4.5-air",
            "zai",
            "glm",
            131_072,
            ToolDialect::OpenaiJson,
            Pricing {
                input_usd_per_mtok: 0.13,
                output_usd_per_mtok: 0.85,
                cached_input_usd_per_mtok: 0.025,
                cache_write_usd_per_mtok: 0.13,
            },
        )
        .with_reasoning(Some(true)),
    ]
}
