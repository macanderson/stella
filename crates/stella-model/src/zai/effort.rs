// SPDX-License-Identifier: AGPL-3.0-only
//! Who speaks the top-level `reasoning_effort` field, and what they mean by it.
//!
//! Two identities on this shared chat-completions adapter carry reasoning depth
//! in OpenAI's top-level `reasoning_effort` string, for different reasons:
//!
//! * **xAI** takes it as its ONLY reasoning control, and only some Grok models
//!   accept it at all.
//! * **Z.ai** takes it as a depth control *alongside* GLM's `thinking` on/off
//!   object, so the two fields divide the job rather than duplicate it.
//!
//! OpenRouter is deliberately absent: it carries both on/off and depth in a
//! structured `reasoning` object instead, which lives with the request body in
//! the parent module.
//!
//! Split out of `zai.rs` for the file-size gate, which asks that new code go in
//! its own module rather than grow an already-oversized one.

use stella_protocol::ReasoningEffort;

/// Whether an xAI model accepts the `reasoning_effort` parameter. Verified
/// against xAI docs (2026-07): the ORIGINAL `grok-4` (and its dated snapshots,
/// `grok-4-<date>`) reasons but rejects the param with a hard 400 ("does not
/// support parameter reasoning_effort") — while grok-3-mini, the grok-4 point
/// releases (`grok-4.1`/`.3`/`.5`), and the `grok-4-fast*` variants all accept
/// it. Sending it to the original grok-4 (the currently-seeded xai default,
/// deprecated and retiring 2026-08-15) would 400 every reasoning turn, so it is
/// gated out here — grok-4 keeps the pre-wiring behavior (reasons at its own
/// fixed depth, effort dropped) instead of erroring. Fail-safe direction is to
/// send: the fleet has trended toward universal support, and only the retiring
/// original is denied.
pub(super) fn xai_supports_reasoning_effort(model: &str) -> bool {
    if model == "grok-4" {
        return false;
    }
    // A dated snapshot of the original (`grok-4-0709`) is digits after the
    // `grok-4-` stem; named variants (`grok-4-fast-reasoning`) are not, and the
    // point releases (`grok-4.5`) don't match the `grok-4-` prefix at all.
    if let Some(rest) = model.strip_prefix("grok-4-") {
        let dated_snapshot_of_the_original =
            !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit());
        return !dated_snapshot_of_the_original;
    }
    true
}

/// Map the engine's one `ReasoningEffort` enum to xAI's chat-completions
/// `reasoning_effort`, which documents `low`/`medium`/`high`. Same collapse
/// posture as `openai.rs::map_reasoning_effort`: never drop the hint, never
/// panic on a variant Grok doesn't model — the finer `xhigh`/`max` tiers are
/// model-dependent on xAI, so they collapse to the universally-safe `high`.
pub(crate) fn map_xai_effort(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High | ReasoningEffort::Xhigh | ReasoningEffort::Max => "high",
    }
}

/// Build xAI's `reasoning_effort` value from the request's reasoning/effort
/// pair — the mirror of the OpenAI adapter's own reasoning match, since xAI's
/// chat-completions dialect is OpenAI-compatible. Rules: an explicit off
/// (`reasoning == Some(false)`) suppresses the field entirely even when an
/// effort is pinned (an explicit off wins, and the docs' `none` value would
/// 400 on always-reasoning Grok models); a pinned effort maps as
/// [`map_xai_effort`]; a bare `Some(true)` with no effort turns thinking on at
/// the middle tier; and neither set keeps the field off the wire (provider
/// default, byte-stable with the pre-field body).
pub(super) fn xai_reasoning_effort(
    reasoning: Option<bool>,
    effort: Option<ReasoningEffort>,
) -> Option<&'static str> {
    match (reasoning, effort) {
        (Some(false), _) => None,
        (_, Some(effort)) => Some(map_xai_effort(effort)),
        (Some(true), None) => Some("medium"),
        (None, None) => None,
    }
}

/// Map the engine's `ReasoningEffort` to Z.ai's chat-completions
/// `reasoning_effort`. GLM accepts `minimal`/`low`/`medium`/`high`.
///
/// **`minimal` is deliberately unreachable from an effort tier.** Measured live
/// against `glm-5.2` on the coding-plan endpoint (2026-08-04) it returns
/// `reasoning_tokens: 0` — it is an OFF switch wearing a tier's name, not a
/// cheap-but-still-thinking rung. Mapping `Low` there would silently stop a
/// caller who asked for shallow thinking from thinking at all; turning
/// reasoning off is the `thinking` object's job, and only when the caller says
/// so. Same collapse posture as [`map_xai_effort`]: `xhigh`/`max` fold into the
/// ceiling GLM models rather than inventing a rung it does not have.
pub(crate) fn map_zai_effort(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High | ReasoningEffort::Xhigh | ReasoningEffort::Max => "high",
    }
}

/// Build Z.ai's `reasoning_effort` from the request's reasoning/effort pair.
///
/// Z.ai is the one identity here that already has a *separate* on/off switch —
/// the `thinking` object — so this field carries depth only, and the two cases
/// where the caller expressed no depth preference must stay off the wire:
///
/// * an explicit off (`reasoning == Some(false)`) suppresses it, because
///   `thinking: {"type": "disabled"}` has already said so and a second,
///   differently-spelled off is a contradiction waiting to be resolved by
///   whichever field the server reads last;
/// * a bare `Some(true)` with no effort stays absent — unlike xAI, `thinking`
///   already turned reasoning on, so inventing `medium` here would *change*
///   what every existing z.ai caller sends, silently re-pinning models that
///   were deliberately left at their own default depth.
///
/// Only a pinned effort puts the field on the wire. That is precisely the case
/// that was being dropped: before this, `effort` was accepted, hashed into the
/// published posture, and then never transmitted on the direct z.ai route — so
/// a benchmark arm pinned to `medium` ran at GLM's own (deeper) default and
/// spent ~2x the tokens of an OpenRouter arm nominally configured the same way.
pub(super) fn zai_reasoning_effort(
    reasoning: Option<bool>,
    effort: Option<ReasoningEffort>,
) -> Option<&'static str> {
    match (reasoning, effort) {
        (Some(false), _) => None,
        (_, Some(effort)) => Some(map_zai_effort(effort)),
        (Some(true) | None, None) => None,
    }
}
