// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Telling the model how much wall clock it has left.
//!
//! The engine has always known: [`crate::budget::BudgetGuard::task_deadline`]
//! is consulted at every step boundary and its remaining headroom rides every
//! `BudgetTick` as `deadline_remaining_ms`. The model was never told any of
//! it. So a turn against a deadline behaved exactly like a turn without one,
//! right up to the moment it was cut off mid-investigation — with a partial
//! answer it would happily have written down, had anything asked it to.
//!
//! Three benchmark failures were of exactly this shape: still digging at the
//! wall, nothing wrong with the work, no answer submitted. It is not a
//! benchmark artifact either — a user who says "I need this in ten minutes"
//! is describing the same constraint, and an agent that spends the tenth
//! minute reading one more file has failed them in the same way.
//!
//! # Why threshold notices and not a per-step clock
//!
//! Invariant 7: anything feeding the model must be byte-stable, because
//! prompt-cache hits are a feature and nondeterminism there is a cost
//! regression. A wall clock stamped into every step would change the
//! transcript on every call, and no two calls in the turn would share a
//! prefix.
//!
//! So the signal is discrete and bounded: at most one notice per threshold in
//! [`THRESHOLDS_MS`], each fired at most once per turn, appended as an
//! ordinary user message at the step boundary the steering drain already
//! uses. Everything before it stays byte-identical, and the whole turn can
//! add at most three short messages. When several thresholds are crossed at
//! once — a long tool call, a parked retry — only the tightest fires: the
//! model needs to know how long it has, not the history of how it found out.

/// Remaining wall clock, in milliseconds, at which the model is told. Coarse
/// on purpose: these are the points where the *strategy* should change —
/// stop opening new lines of investigation, then start writing down what is
/// already known, then finish the sentence you are on.
pub(crate) const THRESHOLDS_MS: [u64; 3] = [600_000, 300_000, 120_000];

/// Per-turn latch: which deadline notices have already been delivered.
///
/// Pure data, no I/O (invariant 2); lives on [`crate::step::TurnState`] and
/// dies with the turn. Not checkpointed, for the same reason the recovery
/// latches are not — a resumed turn re-warning once is cheap and being
/// silent about a deadline is not.
#[derive(Debug, Default)]
pub(crate) struct DeadlineNotices {
    /// How many thresholds have fired. Monotone: `THRESHOLDS_MS` is
    /// descending, so this doubles as "the tightest threshold already
    /// delivered", and a clock that appears to go backwards (a re-armed
    /// deadline, a resumed turn) can never re-fire a notice.
    delivered: usize,
}

impl DeadlineNotices {
    /// The notice due at `remaining_ms`, or `None` when none is.
    ///
    /// Returns the text for the *tightest* newly-crossed threshold and marks
    /// every threshold at or above it as delivered, so a step that skips past
    /// two of them costs one message rather than two.
    pub(crate) fn due(&mut self, remaining_ms: u64) -> Option<String> {
        let crossed = THRESHOLDS_MS
            .iter()
            .filter(|threshold| remaining_ms <= **threshold)
            .count();
        if crossed <= self.delivered {
            return None;
        }
        let threshold = THRESHOLDS_MS[crossed - 1];
        self.delivered = crossed;
        Some(notice_text(remaining_ms, threshold))
    }
}

/// Append the deadline notice due at `now`, if one is.
///
/// The whole effect lives here rather than at the call site so `driver.rs`
/// carries one line: it is a god file closed to growth, and this module is
/// where the decision belongs anyway.
pub(crate) fn push_if_due(state: &mut crate::step::TurnState, now: std::time::Instant) {
    let Some(remaining) = state.budget.deadline_remaining(now) else {
        return;
    };
    let remaining_ms = u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX);
    if let Some(notice) = state.deadline_notices.due(remaining_ms) {
        state
            .messages
            .push(stella_protocol::CompletionMessage::user(notice));
    }
}

/// What the model is actually told. Names the remaining time and the change
/// of strategy it implies — a bare number invites the model to note it and
/// carry on doing what it was doing, which is the behaviour this exists to
/// interrupt.
fn notice_text(remaining_ms: u64, threshold_ms: u64) -> String {
    let minutes = remaining_ms.div_ceil(60_000);
    let advice = if threshold_ms <= 120_000 {
        "Stop investigating now and submit your best answer from what you \
         already know, even if it is incomplete — say plainly what is unverified. \
         An incomplete answer delivered beats a complete one you never send."
    } else if threshold_ms <= 300_000 {
        "Start writing up what you have. Finish the step you are on, then \
         deliver — do not open a new line of investigation."
    } else {
        "Budget the rest of your work accordingly: prefer finishing what you \
         have started over widening the search."
    };
    format!("[time] About {minutes} minute(s) of wall clock remain for this task. {advice}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The witness for the whole module: at a threshold the model is told,
    /// and told something actionable rather than a bare number.
    #[test]
    fn crossing_a_threshold_produces_one_notice() {
        let mut notices = DeadlineNotices::default();
        let text = notices.due(590_000).expect("10-minute threshold must fire");
        assert!(text.contains("10 minute"), "{text}");
    }

    /// Byte-stability (invariant 7): a turn that keeps ticking inside one
    /// band must add nothing to the transcript.
    #[test]
    fn the_same_band_never_fires_twice() {
        let mut notices = DeadlineNotices::default();
        assert!(notices.due(590_000).is_some());
        assert!(notices.due(500_000).is_none());
        assert!(notices.due(400_000).is_none());
    }

    /// Each tighter band is its own notice — the advice changes, so the
    /// model has to hear it again.
    #[test]
    fn each_tighter_band_fires_once() {
        let mut notices = DeadlineNotices::default();
        assert!(notices.due(590_000).is_some());
        assert!(notices.due(290_000).is_some());
        assert!(notices.due(110_000).is_some());
        assert!(notices.due(10_000).is_none(), "the ladder is spent");
    }

    /// A step that skips two bands (a long tool call, a parked retry) costs
    /// one message, not two: the model needs how long it has, not the
    /// history of how it found out.
    #[test]
    fn skipping_bands_costs_one_message() {
        let mut notices = DeadlineNotices::default();
        let text = notices.due(90_000).expect("must fire");
        assert!(text.contains("submit your best answer"), "{text}");
        assert!(notices.due(80_000).is_none());
    }

    /// Nothing fires while there is plenty of time — a deadline hours away
    /// must not put a clock in the prompt.
    #[test]
    fn a_distant_deadline_says_nothing() {
        let mut notices = DeadlineNotices::default();
        assert!(notices.due(3_600_000).is_none());
    }

    /// A clock that appears to go backwards (a re-armed deadline) must not
    /// re-fire a notice the turn has already heard.
    #[test]
    fn a_clock_going_backwards_cannot_re_fire() {
        let mut notices = DeadlineNotices::default();
        assert!(notices.due(110_000).is_some());
        assert!(notices.due(590_000).is_none());
    }
}
