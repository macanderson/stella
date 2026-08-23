//! OpenRouter's gateway rows.
//!
//! Rows are seeded rather than left to `stella models refresh` because a
//! frozen container cannot refresh: an unseeded slug is unreachable there,
//! not merely unpriced.
//!
//! Several of these duplicate a slug seeded under its direct vendor, and the
//! duplication is required: `resolve_for` matches on `(provider, id)`
//! exactly, so a first-party row is invisible to a run routed through the
//! gateway. Prices are OpenRouter's own quotes and differ from vendor list
//! prices, so a row aliasing the first-party numbers would misreport spend on
//! every gateway turn.

use crate::catalog::{CatalogEntry, Pricing, ToolDialect};

/// The rows this provider contributes to
/// [`Catalog::seed`](crate::catalog::Catalog::seed).
pub(super) fn rows() -> Vec<CatalogEntry> {
    vec![
        // OpenRouter's fully-qualified slug for its own meta-router.
        // The gateway's model ids are ALL `vendor/model` — a bare
        // `auto` is not a model there, so this row must carry the
        // wire-true id (it is sent verbatim as the request's
        // `model`). A real, provider-native catalog entry, not our
        // internal `Option<ModelRef>` "auto" sentinel (L-M3 is
        // about OUR resolver never using a string for "no pin";
        // this is a third party's own product feature we pass
        // through verbatim).
        //
        // OpenRouter's `auto` meta-model routes to whichever
        // underlying model it picks, so the effective price varies
        // per request and the gateway reports it back on its own
        // usage/generation endpoint — we cannot know it from the
        // slug alone. Left at zero deliberately: a wrong fixed
        // estimate is worse than a zero the metering layer can flag
        // as "gateway-priced, reconcile from the provider's usage
        // record."
        CatalogEntry::new(
            "openrouter/auto",
            "openrouter",
            "openrouter",
            128_000,
            ToolDialect::OpenaiJson,
            Pricing {
                input_usd_per_mtok: 0.0,
                output_usd_per_mtok: 0.0,
                cached_input_usd_per_mtok: 0.0,
                cache_write_usd_per_mtok: 0.0,
            },
        ),
        // GLM through the gateway, for the one job the z.ai coding
        // plan cannot do: a *concurrent* head-to-head. That plan caps
        // requests per minute, and a pipeline agent spends ~3 calls
        // per step to a single-model agent's 1 — so under a shared cap
        // it throttles roughly 3x sooner and the scoreboard measures
        // the quota rather than the agents. Metered access removes the
        // cap; seeding the row is what makes it reachable, since a
        // benchmark container cannot refresh the catalog.
        //
        // Same slug both sides: OpenRouter also serves an
        // Anthropic-shaped `/v1/messages`, so Claude Code and Stella
        // can be pointed at one provider and one model id.
        CatalogEntry::new(
            "z-ai/glm-5.2",
            "openrouter",
            "glm",
            200_000,
            ToolDialect::OpenaiJson,
            // OpenRouter's published rates, read 2026-08-04. Superseded
            // per call by the gateway's own usage accounting; this is
            // the floor for frames that carry no cost. No separate
            // cache-write line is quoted, so a write bills as input.
            Pricing {
                input_usd_per_mtok: 0.76,
                output_usd_per_mtok: 2.42,
                cached_input_usd_per_mtok: 0.14,
                cache_write_usd_per_mtok: 0.76,
            },
        )
        .with_reasoning(Some(true))
        // Same ceiling as the direct `zai/glm-5.2` row: a route is not
        // a model property. Without its own ceiling this gateway row
        // silently falls to the engine's global 16384 and truncates
        // before the worker can emit a tool call.
        .with_max_output_tokens(Some(131_072)),
        // The verifier and triage seats for that same gateway route. A
        // pipeline is only a pipeline if all three roles resolve; seed
        // the worker alone and the run dies at startup exactly as it
        // does with no worker at all.
        CatalogEntry::new(
            "z-ai/glm-5.1",
            "openrouter",
            "glm",
            204_800,
            ToolDialect::OpenaiJson,
            Pricing {
                input_usd_per_mtok: 0.966,
                output_usd_per_mtok: 3.036,
                cached_input_usd_per_mtok: 0.1794,
                cache_write_usd_per_mtok: 0.966,
            },
        )
        .with_reasoning(Some(true)),
        CatalogEntry::new(
            "z-ai/glm-4.5-air",
            "openrouter",
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
        // The OpenRouter provider's default model. Unlike `auto`
        // above, this row names ONE model, so the numbers are
        // knowable and belong here: the entry is what clamps
        // `compaction_budget_tokens` to 3/4 of a real window rather
        // than leaving the engine's 150k default in place against a
        // 1M-token model, and what tells the effort clamp this model
        // reasons.
        //
        // Pricing is still superseded per call by the gateway's usage
        // accounting (`with_usage_accounting`) — this is the floor
        // for the path where the frame carries no cost. Cache writes
        // bill at the input rate: OpenRouter quotes kimi-k3 with a
        // discounted cache READ and no separate write line, which is
        // the usual "a write is an ordinary input token" shape.
        CatalogEntry::new(
            "moonshotai/kimi-k3",
            "openrouter",
            "moonshotai",
            1_048_576,
            ToolDialect::OpenaiJson,
            Pricing {
                input_usd_per_mtok: 3.0,
                output_usd_per_mtok: 15.0,
                cached_input_usd_per_mtok: 0.3,
                cache_write_usd_per_mtok: 3.0,
            },
        )
        .with_reasoning(Some(true))
        // models.dev `limit.output` for `moonshotai/kimi-k3`, read
        // 2026-08-03 (#1290). OpenRouter's own listing leaves
        // `top_provider.max_completion_tokens` null for this model, so
        // the gateway cannot answer and the master list is the source.
        // That asymmetry is why discovery reads BOTH and neither one
        // is treated as the only authority.
        .with_max_output_tokens(Some(131_072)),
        // The three Anthropic models as OpenRouter serves them. They
        // duplicate slugs already seeded under `anthropic`, and that
        // duplication is the point: `resolve_for` matches on
        // (provider, id) exactly, so the first-party row is invisible
        // to a run routed through the gateway. Without these, a
        // benchmark trial on `openrouter/anthropic/claude-sonnet-5`
        // resolves nothing, `max_output_tokens` is `None`, and the
        // engine falls back to its global 16384 — the zero-tool
        // truncation failure these ceilings exist to prevent,
        // reintroduced by the choice of route alone.
        //
        // Routing matters now because the first-party Anthropic key is
        // not always the one available; the gateway is. Prices are
        // OpenRouter's own quotes, which differ from Anthropic list
        // (Sonnet is $2/$10 here against $3/$15 direct), so a row that
        // merely aliased the first-party pricing would misreport spend
        // on every gateway turn.
        CatalogEntry::new(
            "anthropic/claude-sonnet-5",
            "openrouter",
            "claude",
            1_000_000,
            // OpenRouter speaks its OpenAI-compatible dialect for every
            // upstream, including Anthropic's own models — the same
            // reason `moonshotai/kimi-k3` above is `OpenaiJson` rather
            // than the vendor's native shape.
            ToolDialect::OpenaiJson,
            Pricing {
                input_usd_per_mtok: 2.00,
                output_usd_per_mtok: 10.00,
                cached_input_usd_per_mtok: 0.20,
                cache_write_usd_per_mtok: 2.50,
            },
        )
        .with_reasoning(Some(true))
        // Moves with the first-party row in `seed/anthropic.rs`
        // (#1290), and confirmed independently on this route:
        // OpenRouter's `/models` reports
        // `top_provider.max_completion_tokens = 128000` for
        // `anthropic/claude-sonnet-5` (checked 2026-08-03). Route is
        // not a model property — see
        // `a_models_ceiling_does_not_depend_on_the_route_it_is_reached_through`
        // in `catalog.rs`.
        //
        // This is the route the affordability refusal actually bites
        // on; the first-party row carries the argument for why the
        // ceiling stays 128000 anyway (#3757).
        .with_max_output_tokens(Some(128_000)),
        // Seeded so the head-to-head panel can seat Stella's *worker*
        // on Opus-tier through the gateway. Without a row here the
        // adapter refuses the run outright rather than falling back
        // (`STELLA_CATALOG_AUTO_REFRESH=0`), so a benchmark that pins
        // this slug fails at launch instead of quietly measuring a
        // different model — the right failure, but only if the row a
        // real match needs actually exists.
        //
        // Prices are OpenRouter's own quotes, read from its `/models`
        // endpoint on 2026-08-06, not aliased from the first-party
        // table: `input_cache_read` is $0.50/MTok and
        // `input_cache_write` $6.25/MTok, which is where a
        // cache-heavy agentic run actually spends. They happen to
        // match Anthropic list here — unlike the Sonnet row above,
        // where the gateway is cheaper — and that coincidence is
        // exactly why the number has to be checked rather than
        // assumed.
        CatalogEntry::new(
            "anthropic/claude-opus-5",
            "openrouter",
            "claude",
            1_000_000,
            ToolDialect::OpenaiJson,
            Pricing {
                input_usd_per_mtok: 5.00,
                output_usd_per_mtok: 25.00,
                cached_input_usd_per_mtok: 0.50,
                cache_write_usd_per_mtok: 6.25,
            },
        )
        .with_reasoning(Some(true))
        // `top_provider.max_completion_tokens = 128000` on the same
        // reading, matching the rest of the family on this route.
        .with_max_output_tokens(Some(128_000)),
        CatalogEntry::new(
            "anthropic/claude-fable-5",
            "openrouter",
            "claude",
            1_000_000,
            ToolDialect::OpenaiJson,
            Pricing {
                input_usd_per_mtok: 10.00,
                output_usd_per_mtok: 50.00,
                cached_input_usd_per_mtok: 1.00,
                cache_write_usd_per_mtok: 12.50,
            },
        )
        .with_reasoning(Some(true))
        // Same model, same ceiling, reached through a gateway. Both
        // rows move together or the arm's ceiling depends on which
        // route it was booked through (#1211 §6.2).
        .with_max_output_tokens(Some(128_000)),
        // Seeded for the `triage` role specifically: it classifies the
        // request and builds a prompt, never edits the workspace, so
        // the cheapest fast model in the family is the right one and
        // its reasoning stays off. The 200k window is Haiku's own, not
        // a trimmed copy of Sonnet's — `compaction_budget_tokens` is
        // clamped off this number, so overstating it would let triage
        // assemble a prompt the model cannot accept.
        CatalogEntry::new(
            "anthropic/claude-haiku-4.5",
            "openrouter",
            "claude",
            200_000,
            ToolDialect::OpenaiJson,
            Pricing {
                input_usd_per_mtok: 1.00,
                output_usd_per_mtok: 5.00,
                cached_input_usd_per_mtok: 0.10,
                cache_write_usd_per_mtok: 1.25,
            },
        )
        .with_reasoning(Some(false))
        // Haiku's own ceiling, and it is genuinely lower than the
        // Sonnet/Fable family's — 64000, not 128000. Both sources
        // agree (checked 2026-08-03, #1290): Anthropic reports
        // `"max_tokens": 64000` for `claude-haiku-4-5-20251001`, and
        // OpenRouter reports `max_completion_tokens = 64000` here.
        //
        // Carried even though triage never fills it: an unseeded row
        // is not "no opinion", it is the engine's global 16384, and a
        // row that silently caps 4x below the model is the same defect
        // as one that caps 2x below it. The number being lower than
        // its siblings' is exactly why it has to be looked up per
        // model rather than shared.
        .with_max_output_tokens(Some(64_000)),
    ]
}
