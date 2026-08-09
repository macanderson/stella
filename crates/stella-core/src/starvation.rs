//! Reasoning starvation — the budget arithmetic that keeps a bounded model
//! call from spending its whole allowance on thinking (#2128, #2174).
//!
//! `max_output_tokens` is **one number on the wire**, and a reasoning model
//! bills its reasoning stream against it. So a cap sized for what a call is
//! asked to *write* is a cap the model can spend entirely on thinking, and the
//! response comes back empty with `finish_reason: length` — a shape every
//! caller then misreads as "the model had nothing to say".
//!
//! Two mechanisms, in that order:
//!
//! 1. [`with_reasoning_headroom`] pads a written-output contract so the common
//!    case never starves. It is a heuristic and makes starvation rare.
//! 2. [`starved_of_output`] + [`starved_retry_cap`] recover the calls that
//!    starve anyway. That pair is the actual guarantee.
//!
//! # Why this lives in `stella-core`
//!
//! It is pure arithmetic over owned data with no I/O, and it has **two**
//! dispatch chokepoints to serve: `stella-pipeline`'s `metered_raw_call`,
//! which drives the staged pipeline's management roles, and `stella-cli`'s
//! `complete_standalone`, which drives the four standalone paid calls
//! (reflection, agent authoring, skill authoring, domain inference). #2128 fixed
//! the first and left the second running on a bare cap; post-turn reflection
//! then hit `finish_reason: length` at exactly its 2,048-token allowance and
//! froze the whole context lifecycle for nine days without a word in any
//! artifact (#2174). One copy of these numbers is what makes the next fix
//! reach both callers.

use stella_protocol::{CompletionResult, FinishReason};

/// Headroom added to a bounded call's *visible-output* budget so a reasoning
/// model can think before it answers (#2128).
///
/// The number is a heuristic bound, and this is the honest place to say so: no
/// provider-parity posture axis reports a model's reasoning budget, and the
/// real figure varies by model and prompt. It is deliberately generous next to
/// the output contracts it pads (two to six lines, or a three-element JSON
/// array) and still an order of magnitude under a 64k engine base, so a
/// runaway call stays bounded. [`starved_retry_cap`] is the guarantee;
/// headroom only makes it rare.
pub const REASONING_HEADROOM_TOKENS: u32 = 4_096;

/// The cap a call that provably starved is retried at (#2128).
///
/// Reached only after a response came back empty with `finish_reason:
/// length`, which is the provider stating that the budget ran out before the
/// first visible token. Sized so one retry is enough for any reasoning budget
/// observed in the wild, rather than sized to a model.
pub const STARVED_RETRY_CAP: u32 = 32_768;

/// A written-output contract plus room to think.
///
/// Saturating, so a caller that already asks for the whole `u32` range gets a
/// cap rather than a wrap — a wrapped cap would be *smaller* than the
/// contract, which is the one direction this must never fail in.
#[must_use]
pub fn with_reasoning_headroom(output_contract: u32) -> u32 {
    output_contract.saturating_add(REASONING_HEADROOM_TOKENS)
}

/// Whether a completion is the cap-starvation signature (#2128): the provider
/// stopped at the token limit and no visible text was produced.
///
/// Both halves are required. `Length` with text is an ordinary truncation (the
/// parsers handle a clipped reply), and empty text with any other finish
/// reason is a model declining to answer — neither is fixed by more room.
#[must_use]
pub fn starved_of_output(result: &CompletionResult) -> bool {
    result.text.trim().is_empty() && result.finish_reason == Some(FinishReason::Length)
}

/// The cap to retry a provably starved call at, or `None` when a retry could
/// not change the outcome (#2128).
///
/// `None` when the call already had at least [`STARVED_RETRY_CAP`] of room:
/// the response was then empty for some reason more room cannot fix, and
/// buying a second identical call to discover that again is waste. `None` for
/// an unset cap too — the budget was never the binding constraint, so there is
/// nothing to raise.
#[must_use]
pub fn starved_retry_cap(previous: Option<u32>) -> Option<u32> {
    match previous {
        Some(cap) if cap < STARVED_RETRY_CAP => Some(STARVED_RETRY_CAP),
        Some(_) | None => None,
    }
}

#[cfg(test)]
mod tests {
    use stella_protocol::CompletionUsage;

    use super::*;

    fn result(text: &str, finish_reason: Option<FinishReason>) -> CompletionResult {
        CompletionResult {
            text: text.to_string(),
            tool_calls: Vec::new(),
            usage: CompletionUsage::default(),
            model: "scripted".into(),
            cost_usd: 0.0,
            finish_reason,
        }
    }

    /// The starvation signature requires BOTH halves: a truncated reply with
    /// text is ordinary (the parsers cope), and an empty reply that stopped
    /// for any other reason is a model declining, which more room cannot fix.
    #[test]
    fn only_an_empty_length_stop_counts_as_starvation() {
        let length = Some(FinishReason::Length);
        assert!(starved_of_output(&result("", length)));
        assert!(
            starved_of_output(&result("   \n ", length)),
            "whitespace is not an answer"
        );
        assert!(!starved_of_output(&result("PASS — looks right", length)));
        assert!(!starved_of_output(&result("", Some(FinishReason::Stop))));
        assert!(!starved_of_output(&result("", None)));
    }

    /// A retry is bought only when more room could change the answer.
    #[test]
    fn a_retry_is_declined_when_room_was_never_the_constraint() {
        assert_eq!(starved_retry_cap(Some(1024)), Some(STARVED_RETRY_CAP));
        assert_eq!(starved_retry_cap(Some(STARVED_RETRY_CAP)), None);
        assert_eq!(starved_retry_cap(Some(64_000)), None);
        assert_eq!(starved_retry_cap(None), None);
    }

    /// Headroom is strictly additive and never wraps below the contract it is
    /// supposed to protect.
    #[test]
    fn headroom_is_additive_and_saturating() {
        assert_eq!(
            with_reasoning_headroom(512),
            512 + REASONING_HEADROOM_TOKENS
        );
        assert_eq!(with_reasoning_headroom(u32::MAX), u32::MAX);
        assert!(with_reasoning_headroom(2048) > 2048);
    }

    /// The retry ceiling has to sit above a padded contract, or a starved call
    /// would be "retried" at a cap no larger than the one that starved it.
    #[test]
    fn the_retry_ceiling_clears_a_padded_contract() {
        assert!(
            STARVED_RETRY_CAP > with_reasoning_headroom(4096),
            "the retry must buy real room over the largest padded contract in \
             the tree, or `starved_retry_cap` would decline every recovery"
        );
    }
}
