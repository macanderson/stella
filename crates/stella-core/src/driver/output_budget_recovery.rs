// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Reactive recovery from a provider refusing the requested output ceiling.
//!
//! The output-side mirror of [`super::overflow_recovery`], and it exists for
//! the same reason: a rejection the engine can repair was being surfaced as
//! a terminal error that ended the turn.
//!
//! A gateway prices a request against the account balance using the ceiling
//! the caller *asks for*, not the tokens it will actually spend. So a
//! session that asks for `max_output_tokens: 128000` and typically emits a
//! few thousand is refused the moment the balance drops below the price of
//! the ask — while still holding credit enough for dozens of real calls.
//! OpenRouter says so in as many words:
//!
//! ```text
//! This request requires more credits, or fewer max_tokens. You requested
//! up to 128000 tokens, but can only afford 117676.
//! ```
//!
//! That is an instruction, not a wall. Before this module it arrived as
//! `ProviderError::Terminal` and killed the turn: three benchmark runs were
//! lost or maimed that way — every trial dead against a balance that could
//! have funded them all at a ceiling the provider itself named.
//!
//! # The ladder
//!
//! When a call fails with
//! [`stella_protocol::ProviderError::OutputBudgetExceeded`], the engine
//! clamps `max_output_tokens` for the rest of the turn and re-runs the step.
//! The clamp is the provider's own stated affordable ceiling less
//! [`SAFETY_MARGIN_PERCENT`] — a margin rather than the bare figure because
//! the balance is falling as the turn spends, so the number that was exactly
//! affordable when the provider computed it is not affordable by the time
//! the retry lands. When the provider names no figure, the ask is halved:
//! the engine reduces its own request rather than inventing a number the
//! provider never stated.
//!
//! # Bounds and the death-spiral guard
//!
//! Identical in shape to the overflow ladder, deliberately: a **per-turn
//! down-only latch**, at most [`MAX_RECOVERY_RUNGS`] rungs, the counter never
//! resetting within the turn, and each clamp monotonically tighter than the
//! last. The clamp never loosens once set, even after a later call commits,
//! so a configured ceiling above what the account can fund cannot oscillate
//! ask → refuse → recover for the rest of the turn. [`FLOOR_TOKENS`] stops
//! the ladder before the ceiling gets too small to hold an answer: below
//! that the account genuinely cannot fund the call, and the rejection
//! surfaces exactly as an unrecovered terminal failure does.
//!
//! Like the overflow latch, this is not checkpointed: a resumed turn starts
//! the allowance over, which only re-permits a bounded amount of work.

/// How many clamp rungs may fire per turn. Three, one more than the overflow
/// ladder's two, because each rung here answers a *different* stated figure
/// rather than a fixed schedule — the balance falls as the turn spends, so
/// the provider's second answer is legitimately smaller than its first, and
/// stopping at two would abandon a turn the third rung would have carried.
pub(crate) const MAX_RECOVERY_RUNGS: u8 = 3;

/// How far below the provider's stated affordable ceiling to clamp, in
/// percent. The stated figure is computed against a balance that keeps
/// falling while the turn runs, so asking for exactly it is asking to be
/// refused again one rung later — spending a rung to learn nothing.
pub(crate) const SAFETY_MARGIN_PERCENT: u32 = 10;

/// The ceiling below which recovery stops. An answer that must fit in fewer
/// tokens than this is not an answer worth buying a rung for, and a balance
/// that cannot fund this much output is out of credit in every sense that
/// matters — which is the terminal failure, correctly surfaced.
pub(crate) const FLOOR_TOKENS: u32 = 1024;

/// Per-turn latch and output-ceiling clamp. Pure data, no I/O (invariant 2);
/// lives on [`crate::step::TurnState`] and dies with the turn.
#[derive(Debug, Default)]
pub(crate) struct OutputBudgetRecovery {
    /// Rungs fired this turn. Monotone — never reset, even by a committed
    /// call, so the ladder cannot re-arm (the death-spiral guard above).
    rungs_fired: u8,
    /// The standing output ceiling, once any rung has fired. Monotone
    /// downward for the rest of the turn.
    clamp_tokens: Option<u32>,
}

impl OutputBudgetRecovery {
    /// Apply the standing clamp to a configured ceiling. The identity until
    /// a rung fires, and a `min` after — never a raise, so a config value
    /// below the clamp keeps its own smaller number.
    ///
    /// `None` (no configured ceiling — let the provider decide) becomes the
    /// clamp itself once armed: the whole point of the rung is that the
    /// provider's own default is what it just refused to fund.
    pub(crate) fn apply(&self, configured: Option<u32>) -> Option<u32> {
        match (self.clamp_tokens, configured) {
            (None, configured) => configured,
            (Some(clamp), Some(configured)) => Some(clamp.min(configured)),
            (Some(clamp), None) => Some(clamp),
        }
    }

    /// Arm the next rung against what the provider said it could afford, or
    /// `None` when the ladder is spent — the caller then surfaces the
    /// rejection terminally.
    ///
    /// `affordable` is the provider's stated ceiling when it named one.
    /// `asked` is what this turn actually requested, so a provider that
    /// named nothing can still be answered with half of the ask.
    pub(crate) fn arm(&mut self, affordable: Option<u32>, asked: Option<u32>) -> Option<u32> {
        if self.rungs_fired >= MAX_RECOVERY_RUNGS {
            return None;
        }
        let proposed = match affordable {
            Some(stated) => stated.saturating_sub(stated / 100 * SAFETY_MARGIN_PERCENT),
            // No figure from the provider: halve whatever ceiling is
            // currently in force. `asked` is the configured value, already
            // narrowed by any standing clamp, so successive rungs keep
            // halving rather than re-halving the original.
            None => self.apply(asked).map(|current| current / 2)?,
        };
        // Monotonically tighter, always: a provider that re-states a figure
        // *larger* than the standing clamp is answering about a balance that
        // has since changed, and loosening on it is the oscillation the
        // death-spiral guard exists to prevent.
        let clamped = match self.clamp_tokens {
            Some(standing) => proposed.min(standing.saturating_sub(1)),
            None => proposed,
        };
        if clamped < FLOOR_TOKENS {
            return None;
        }
        self.rungs_fired += 1;
        self.clamp_tokens = Some(clamped);
        Some(clamped)
    }

    /// Rungs fired so far — the `n` in the "recovery n of MAX" notice.
    pub(crate) fn fired(&self) -> u8 {
        self.rungs_fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recorded bench failure, replayed: a 128K ask against a balance
    /// that could afford 117676 must become a smaller ask, not a dead turn.
    #[test]
    fn the_stated_ceiling_becomes_a_clamp_below_it() {
        let mut recovery = OutputBudgetRecovery::default();
        let clamp = recovery.arm(Some(117_676), Some(128_000)).unwrap();
        assert!(clamp < 117_676, "must sit below the stated figure: {clamp}");
        assert_eq!(recovery.apply(Some(128_000)), Some(clamp));
        assert_eq!(recovery.fired(), 1);
    }

    /// A margin, not the bare figure: the balance keeps falling while the
    /// turn runs, so asking for exactly what was affordable a moment ago
    /// spends a rung to be refused again.
    #[test]
    fn the_clamp_keeps_a_margin_under_the_stated_figure() {
        let mut recovery = OutputBudgetRecovery::default();
        let clamp = recovery.arm(Some(100_000), Some(128_000)).unwrap();
        assert_eq!(clamp, 90_000);
    }

    /// A provider that names no figure still gets a smaller ask — the engine
    /// halves its own, rather than inventing one the provider never stated.
    #[test]
    fn an_unnamed_ceiling_halves_the_ask() {
        let mut recovery = OutputBudgetRecovery::default();
        assert_eq!(recovery.arm(None, Some(64_000)), Some(32_000));
        assert_eq!(recovery.arm(None, Some(64_000)), Some(16_000));
    }

    /// Down-only: a later, larger stated figure is about a balance that has
    /// since changed, and honouring it is the ask → refuse → recover
    /// oscillation the latch exists to prevent.
    #[test]
    fn the_clamp_never_loosens() {
        let mut recovery = OutputBudgetRecovery::default();
        let first = recovery.arm(Some(50_000), Some(128_000)).unwrap();
        let second = recovery.arm(Some(120_000), Some(128_000)).unwrap();
        assert!(second < first, "{second} must be under {first}");
    }

    /// The ladder is bounded, and a spent ladder surfaces terminally.
    #[test]
    fn the_ladder_is_spent_after_max_rungs() {
        let mut recovery = OutputBudgetRecovery::default();
        for _ in 0..MAX_RECOVERY_RUNGS {
            assert!(recovery.arm(Some(100_000), Some(128_000)).is_some());
        }
        assert_eq!(recovery.arm(Some(100_000), Some(128_000)), None);
    }

    /// A balance that cannot fund a usable answer is out of credit in the
    /// sense that matters, and must surface as the terminal failure it is
    /// rather than buying rungs down to nothing.
    #[test]
    fn a_ceiling_under_the_floor_does_not_arm() {
        let mut recovery = OutputBudgetRecovery::default();
        assert_eq!(recovery.arm(Some(200), Some(128_000)), None);
        assert_eq!(recovery.fired(), 0);
    }

    /// The clamp narrows a configured ceiling and never raises one: a
    /// caller already asking for less than the clamp keeps its own figure.
    #[test]
    fn apply_narrows_but_never_raises() {
        let mut recovery = OutputBudgetRecovery::default();
        recovery.arm(Some(100_000), Some(128_000)).unwrap();
        assert_eq!(recovery.apply(Some(4_096)), Some(4_096));
        assert_eq!(recovery.apply(None), Some(90_000));
    }
}
