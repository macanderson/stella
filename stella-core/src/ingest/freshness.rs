//! Freshness — what a re-run truth probe means for a record's retention.
//!
//! A context record is only worth injecting while its claim still holds. The
//! `05` node-version case is the whole reason the truth axis exists: CLAUDE.md
//! said Node 20, `.nvmrc` moved to 22, and a record kept when it was true would
//! steer the agent wrong on every turn after the world changed. Re-running the
//! record's ungated probe catches that; this module decides what the verdict
//! means.
//!
//! It is deliberately pure and I/O-free. Running the probe (filesystem) lives in
//! the CLI; the *policy* — refuted-plus-`on_expiry` → keep/drop/block — lives
//! here so the selection path (once records are rendered) and the on-demand
//! `stella context validate` command reach the same decision.

use super::record::Verdict;

/// What to do with a record after its truth probe was re-checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    /// The claim still holds, or nothing could judge it — keep selecting it.
    Keep,
    /// The claim was refuted, but the record stays (flagged) until a human acts.
    /// This is the `on_expiry = "stale"` default: a stale document is a reason to
    /// review, not to silently delete.
    Warn,
    /// Stop auto-selecting the record (`on_expiry = "drop"`).
    Drop,
    /// Stop auto-selecting and surface hard (`on_expiry = "block"`).
    Block,
}

impl Retention {
    /// The canonical `snake_case` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Warn => "warn",
            Self::Drop => "drop",
            Self::Block => "block",
        }
    }

    /// Whether this action removes the record from automatic selection. `Warn`
    /// does **not** — the record keeps steering, just flagged — so a refuted
    /// claim never silently vanishes without a decision.
    pub fn stops_selection(self) -> bool {
        matches!(self, Self::Drop | Self::Block)
    }
}

/// Map a re-run verdict and the record's `on_expiry` policy to a retention
/// action.
///
/// `Supported` keeps the record. `Unfalsifiable` also keeps it and is **never**
/// counted against it — folding "no probe could judge this" into a negative is
/// the same laundering the README forbids in the other direction. Only `Refuted`
/// applies `on_expiry`, and its default (unset, or `"stale"`) is `Warn`: keep
/// citing, flagged, because a refuted document should be reviewed, not deleted by
/// a machine.
pub fn retention_for(verdict: Verdict, on_expiry: Option<&str>) -> Retention {
    match verdict {
        Verdict::Supported | Verdict::Unfalsifiable => Retention::Keep,
        Verdict::Refuted => match on_expiry {
            Some("drop") => Retention::Drop,
            Some("block") => Retention::Block,
            // "stale" or anything unrecognized (and the unset default): keep, flagged.
            _ => Retention::Warn,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_and_unfalsifiable_always_keep() {
        for on_expiry in [None, Some("drop"), Some("block"), Some("stale")] {
            assert_eq!(
                retention_for(Verdict::Supported, on_expiry),
                Retention::Keep
            );
            assert_eq!(
                retention_for(Verdict::Unfalsifiable, on_expiry),
                Retention::Keep
            );
        }
    }

    #[test]
    fn refuted_honors_on_expiry_and_defaults_to_warn() {
        assert_eq!(retention_for(Verdict::Refuted, None), Retention::Warn);
        assert_eq!(
            retention_for(Verdict::Refuted, Some("stale")),
            Retention::Warn
        );
        assert_eq!(
            retention_for(Verdict::Refuted, Some("drop")),
            Retention::Drop
        );
        assert_eq!(
            retention_for(Verdict::Refuted, Some("block")),
            Retention::Block
        );
        // An unrecognized policy fails safe to Warn — never silently drops.
        assert_eq!(
            retention_for(Verdict::Refuted, Some("nonsense")),
            Retention::Warn
        );
    }

    #[test]
    fn only_drop_and_block_stop_selection() {
        assert!(!Retention::Keep.stops_selection());
        assert!(!Retention::Warn.stops_selection());
        assert!(Retention::Drop.stops_selection());
        assert!(Retention::Block.stops_selection());
    }
}
