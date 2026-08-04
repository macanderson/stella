//! The one token-estimation rule, and the only place it is written down.
//!
//! # Why this lives in the protocol crate
//!
//! `token_cost` is a protocol field ([`crate::receipt::ManifestEntry`],
//! [`crate::event::AgentEvent::BlockRegistered`]) and it participates in the
//! compiled frame's hash. A field whose *unit* is part of the wire contract
//! cannot have its rule defined by whichever caller happens to compute it, so
//! the rule lives next to the field rather than in any one consumer.
//!
//! It is also the only home both sides of the receipts plane can reach:
//! `stella-core` mints these numbers and `stella-store` migrates the ones
//! already at rest, and `stella-protocol` is the crate they share. A private
//! copy in either would be exactly the divergence this module exists to end
//! (#925).
//!
//! # Why bytes, not characters
//!
//! Counting UTF-8 **bytes**. For ASCII the two rules agree exactly, which is
//! why a char-based copy went unnoticed for so long; for anything multi-byte
//! they diverge by up to 4x per character. Three independent reasons pick
//! bytes:
//!
//! 1. **The Context Graph Protocol already ruled.** `contextgraph_types::
//!    budget_tokens` is `ceil(utf8_bytes / BYTES_PER_BUDGET_TOKEN)` and its
//!    source rejects `chars().count()` in as many words — a character count
//!    "would make the count depend on Unicode normalization and diverge across
//!    implementations". Stella conforms to that protocol; the receipts plane
//!    contradicting it on what a token is made of is not a defensible split.
//!    (The CGP function is a *different quantity* — a wire-level frame budget
//!    on its own divisor — so this is agreement on the counting unit, not a
//!    call to reuse the function.)
//! 2. **Normalization independence.** Two byte-identical payloads always cost
//!    the same. A character count makes cost a function of how a string was
//!    normalized, which is not a property the payload's own bytes carry — and
//!    every id and digest around it *is* taken over those bytes.
//! 3. **The safe direction for compaction.** The estimate drives when the
//!    transcript is compacted, and over-estimating compacts earlier — the safe
//!    error, since the failure mode being prevented is silent truncation by
//!    the provider. Bytes over characters can only raise the estimate. For CJK
//!    it is also the *more accurate* rule: at 3 bytes per character it yields
//!    ~0.86 tokens/char against a real tokenizer's ~1, where a character count
//!    yields ~0.29.
//!
//! # What this is not
//!
//! A heuristic, deliberately biased high — not a tokenizer. Callers that need
//! the provider's real count read it back from `StepUsage`;
//! `stella_core::estimator::Calibration` closes the remaining gap by learning
//! the observed actual/estimated ratio per model. This function is the shared
//! *baseline* that correction is applied on top of, never by mutating it.

/// Bytes per estimated token.
///
/// 4 is the classic English-prose heuristic; code and JSON run denser (more
/// tokens per byte), so 3.5 biases the estimate high — see the module docs on
/// why high is the safe direction.
pub const BYTES_PER_TOKEN: f64 = 3.5;

/// The rule, over a byte count that has already been summed.
///
/// Exists alongside [`estimate_tokens`] so a caller adding up several pieces
/// divides **once** over the total rather than once per piece: `ceil` is not
/// additive, so summing per-piece estimates would drift upward with the number
/// of pieces rather than with the content. Also lets a caller estimate from a
/// stored byte length with no payload in hand.
///
/// Takes `u64` rather than `usize`: byte counts here come from attachment
/// lengths and SQLite columns as much as from in-memory strings, and a byte
/// count is not a pointer-width quantity.
pub fn estimate_tokens_for_bytes(bytes: u64) -> u64 {
    (bytes as f64 / BYTES_PER_TOKEN).ceil() as u64
}

/// The rule, over a string's UTF-8 bytes.
///
/// `str::len` is the UTF-8 byte length — the quantity the module docs argue
/// for, and deliberately not `chars().count()`.
pub fn estimate_tokens(content: &str) -> u64 {
    estimate_tokens_for_bytes(content.len() as u64)
}

/// [`estimate_tokens`] narrowed to the `u32` the protocol's `token_cost`
/// fields carry, saturating rather than wrapping.
///
/// Saturation is unreachable in practice — `u32::MAX` tokens is ~15 GB of
/// content in one block — but a silent wrap would turn an enormous block into
/// a tiny one, and a receipt understating a cost by 4 billion is a worse
/// failure than one clamping it.
pub fn estimate_token_cost(content: &str) -> u32 {
    u32::try_from(estimate_tokens(content)).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_the_case_where_bytes_and_chars_agree() {
        // The reason #925 hid for so long: on ASCII the rule this module
        // replaced returns the identical number, so no ASCII test could have
        // caught the divergence.
        let ascii = "the quick brown fox";
        assert_eq!(ascii.len(), ascii.chars().count());
        assert_eq!(estimate_tokens(ascii), 6);
    }

    /// The regression that is the whole point: multi-byte content must cost
    /// more than its character count suggests, or a receipt's own arithmetic
    /// stops adding up in exactly the languages it was not tested in.
    #[test]
    fn multibyte_content_costs_its_bytes_not_its_characters() {
        // 5 CJK characters, 3 bytes each.
        let cjk = "日本語です";
        assert_eq!(cjk.chars().count(), 5);
        assert_eq!(cjk.len(), 15);
        // The old char rule: ceil(5/3.5) = 2. The rule: ceil(15/3.5) = 5.
        assert_eq!(estimate_tokens(cjk), 5);

        // A 4-byte emoji is one character and the widest possible divergence.
        let emoji = "🚀";
        assert_eq!(emoji.chars().count(), 1);
        assert_eq!(estimate_tokens(emoji), 2);
    }

    #[test]
    fn empty_content_costs_nothing() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens_for_bytes(0), 0);
    }

    /// Pins the reason [`estimate_tokens_for_bytes`] exists: a caller summing
    /// bytes first gets a smaller, content-proportional answer than one
    /// ceiling each piece, and the receipts plane sums many small blocks.
    #[test]
    fn summing_bytes_first_is_not_the_same_as_summing_estimates() {
        let pieces = ["a", "b", "c", "d"];
        let total_bytes: u64 = pieces.iter().map(|p| p.len() as u64).sum();
        let per_piece: u64 = pieces.iter().map(|p| estimate_tokens(p)).sum();
        assert_eq!(estimate_tokens_for_bytes(total_bytes), 2);
        assert_eq!(per_piece, 4, "ceil per piece drifts with the piece count");
    }

    #[test]
    fn the_cost_narrowing_saturates_rather_than_wrapping() {
        assert_eq!(estimate_token_cost("日本語です"), 5);
        // Reached only via the saturating path, which cannot be exercised with
        // a real allocation; pin the conversion itself.
        assert_eq!(
            u32::try_from(u64::from(u32::MAX) + 1).unwrap_or(u32::MAX),
            u32::MAX
        );
    }
}
