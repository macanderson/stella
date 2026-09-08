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
//! AGENTS.md #7: anything feeding the model must be byte-stable, because
//! prompt-cache hits are a feature and nondeterminism there is a cost
//! regression. A wall clock stamped into every step would change the
//! transcript on every call, and no two calls in the turn would share a
//! prefix.
//!
//! So the signal is discrete and bounded: at most one notice per rung of
//! [`thresholds_for`], each fired at most once per turn, appended as an
//! ordinary user message at the step boundary the steering drain already
//! uses. Everything before it stays byte-identical, and the whole turn can
//! add at most three short messages. When several thresholds are crossed at
//! once — a long tool call, a parked retry — only the tightest fires: the
//! model needs to know how long it has, not the history of how it found out.

/// Upper bounds on the remaining wall clock, in milliseconds, at which the
/// model is told. Coarse on purpose: these are the points where the
/// *strategy* should change — stop opening new lines of investigation, then
/// start writing down what is already known, then finish the sentence you are
/// on.
///
/// These are ceilings, not the thresholds themselves. A turn with hours left
/// does not want its first warning an hour out. Past about twenty minutes of
/// headroom the advice is noise. So the caps take over from
/// [`THRESHOLD_FRACTIONS`], and the ladder sits a fixed distance from the end
/// however long the turn is.
pub(crate) const THRESHOLD_CAPS_MS: [u64; 3] = [600_000, 300_000, 120_000];

/// The same three points as a percentage of the turn's *own* budget, which is
/// what a short budget needs.
///
/// On a short turn, absolute thresholds are not a ladder. They are a verdict
/// at the starting line. A 300s budget begins with 300s left, which is already
/// under two of the caps above. So the turn opened by telling the model to
/// "start writing up what you have" before it had run one tool.
///
/// Against the 840s benchmark budget these fractions land almost on the caps
/// (588s/294s/126s against 600s/300s/120s). The shape tuned on long turns is
/// kept; only short turns change.
pub(crate) const THRESHOLD_FRACTIONS: [u64; 3] = [70, 35, 15];

/// A whole budget at or under this is an emergency from its first instant,
/// and the model is told so at the starting line.
///
/// The proportional ladder alone would be silent here — every band sits below
/// a budget this short, so the first reading crosses nothing — and silence is
/// the wrong answer for a turn that has two minutes to live. This is the one
/// case where speaking immediately is right, and it is why the fix for the
/// short-budget defect is "the correct rung", not "never speak first".
pub(crate) const EMERGENCY_BUDGET_MS: u64 = 120_000;

/// The three thresholds for a turn whose whole budget is `budget_ms`: the
/// proportional point, never later than its cap.
///
/// An emergency budget collapses the ladder onto its own first reading, so
/// the tightest rung is due immediately.
fn thresholds_for(budget_ms: u64) -> [u64; 3] {
    if budget_ms <= EMERGENCY_BUDGET_MS {
        return [budget_ms; 3];
    }
    let mut bands = [0_u64; 3];
    for (band, (fraction, cap)) in bands
        .iter_mut()
        .zip(THRESHOLD_FRACTIONS.iter().zip(THRESHOLD_CAPS_MS.iter()))
    {
        *band = (budget_ms.saturating_mul(*fraction) / 100).min(*cap);
    }
    bands
}

/// Per-turn latch: which deadline notices have already been delivered.
///
/// Pure data, no I/O (AGENTS.md #2); lives on [`crate::step::TurnState`] and
/// dies with the turn. Not checkpointed, for the same reason the recovery
/// latches are not — a resumed turn re-warning once is cheap and being
/// silent about a deadline is not.
#[derive(Debug, Default)]
pub(crate) struct DeadlineNotices {
    /// How many thresholds have fired. Monotone: the ladder is descending, so
    /// this doubles as "the tightest threshold already delivered", and a clock
    /// that appears to go backwards (a re-armed deadline, a resumed turn) can
    /// never re-fire a notice.
    delivered: usize,
    /// The turn's whole budget, latched from the first remaining time this
    /// ever saw.
    ///
    /// The engine consults the deadline at every step boundary and the first
    /// of those happens before the first tool runs, so the first reading is
    /// the budget. Latched rather than plumbed because
    /// [`crate::budget::BudgetGuard`] stores the deadline as an `Instant` and
    /// never the span that led to it — there is no total to ask for.
    budget_ms: Option<u64>,
}

impl DeadlineNotices {
    /// The notice due at `remaining_ms`, or `None` when none is.
    ///
    /// Returns the text for the *tightest* newly-crossed threshold and marks
    /// every threshold at or above it as delivered, so a step that skips past
    /// two of them costs one message rather than two.
    pub(crate) fn due(&mut self, remaining_ms: u64) -> Option<String> {
        let budget_ms = *self.budget_ms.get_or_insert(remaining_ms);
        let bands = thresholds_for(budget_ms);
        let crossed = bands
            .iter()
            .filter(|threshold| remaining_ms <= **threshold)
            .count();
        if crossed <= self.delivered {
            return None;
        }
        self.delivered = crossed;
        Some(notice_text(remaining_ms, crossed - 1))
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

/// The notice's marker prefix — one entry of
/// [`crate::engine_markers::ENGINE_MARKERS`]. The notice rides the wire as a
/// `User`-role message, and an unmarked one is a genuine user turn to every
/// marker consumer: `turn_start_index` reset the loop-detection and
/// confident-zero windows on it — erasing a stuck loop's accumulated evidence
/// exactly when a deadline-bounded run needs the detector — and receipts
/// attributed the engine's own text to the person.
pub(crate) const DEADLINE_MARKER_PREFIX: &str = "[time remaining";

/// What the model is actually told. Names the remaining time and the change
/// of strategy it implies — a bare number invites the model to note it and
/// carry on doing what it was doing, which is the behaviour this exists to
/// interrupt.
///
/// Keyed on the *band* rather than the millisecond value: the thresholds move
/// with the turn's budget now, so the same rung means the same advice whether
/// it fell at ten minutes or at forty-five seconds.
fn notice_text(remaining_ms: u64, band: usize) -> String {
    let advice = match band {
        0 => {
            "Budget the rest of your work accordingly: prefer finishing what you \
             have started over widening the search."
        }
        1 => {
            "Start writing up what you have. Finish the step you are on, then \
             deliver — do not open a new line of investigation."
        }
        _ => {
            "Stop investigating now and submit your best answer from what you \
             already know, even if it is incomplete — say plainly what is unverified. \
             An incomplete answer delivered beats a complete one you never send."
        }
    };
    format!(
        "{DEADLINE_MARKER_PREFIX}] {} {advice}",
        remaining_phrase(remaining_ms)
    )
}

/// How the remaining clock is spoken.
///
/// Minutes round *up* so a notice never overstates the time left, and a turn
/// short enough to be measured in seconds says so: "about 1 minute(s)" on a
/// 20-second remainder is the one phrasing that could make the model finish
/// something it does not have time to finish.
fn remaining_phrase(remaining_ms: u64) -> String {
    if remaining_ms < 60_000 {
        format!(
            "About {} seconds of wall clock remain for this task.",
            remaining_ms / 1_000
        )
    } else {
        format!(
            "About {} minute(s) of wall clock remain for this task.",
            remaining_ms.div_ceil(60_000)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A turn opens with its whole budget in hand, and the ladder must say
    /// nothing at the starting line however short that budget is.
    ///
    /// The witness for this module's own defect: against absolute thresholds
    /// a turn whose entire budget sits at or under one of them crosses it on
    /// the first reading. A 300s turn was told to "start writing up what you
    /// have" before it had run a single tool — the
    /// opposite of the behaviour the ladder exists to produce. Measured on
    /// the benchmark's own shorter budgets; the 840s trials never saw it.
    #[test]
    fn a_short_budget_says_nothing_at_the_starting_line() {
        for budget_ms in [300_000_u64, 180_000, 150_000] {
            let mut notices = DeadlineNotices::default();
            assert!(
                notices.due(budget_ms).is_none(),
                "a {budget_ms}ms turn was warned before it had done anything"
            );
        }
    }

    /// The exception the rule above is drawn around: a turn with
    /// two minutes or less to live is an emergency at its first instant, and
    /// hears the tightest advice immediately. Silence would be the same
    /// "still digging at the wall" failure this module exists to end.
    #[test]
    fn an_emergency_budget_speaks_immediately() {
        for budget_ms in [EMERGENCY_BUDGET_MS, 90_000, 30_000] {
            let mut notices = DeadlineNotices::default();
            let text = notices
                .due(budget_ms)
                .unwrap_or_else(|| panic!("a {budget_ms}ms turn must be told at once"));
            assert!(text.contains("submit your best answer"), "{text}");
        }
    }

    /// The witness for the whole module: at a threshold the model is told,
    /// and told something actionable rather than a bare number.
    #[test]
    fn crossing_a_threshold_produces_one_notice() {
        let mut notices = DeadlineNotices::default();
        assert!(
            notices.due(840_000).is_none(),
            "the starting line is silent"
        );
        let text = notices.due(580_000).expect("the first band must fire");
        assert!(text.contains("minute"), "{text}");
        assert!(text.contains("prefer finishing"), "{text}");
    }

    /// A short turn still gets all three rungs, proportionally placed — the
    /// point of the fractions. On a 300s budget the bands are 210s/105s/45s.
    #[test]
    fn a_short_budget_still_gets_the_whole_ladder() {
        let mut notices = DeadlineNotices::default();
        assert!(notices.due(300_000).is_none(), "starting line");
        assert!(notices.due(200_000).is_some(), "first band at 210s");
        assert!(notices.due(100_000).is_some(), "second band at 105s");
        let last = notices.due(40_000).expect("third band at 45s");
        assert!(last.contains("submit your best answer"), "{last}");
        assert!(
            last.contains("seconds"),
            "a sub-minute remainder is spoken in seconds: {last}"
        );
    }

    /// A long turn is not warned absurdly early: the caps take over, so the
    /// first notice still lands ten minutes out rather than at 70% of a
    /// two-hour budget.
    #[test]
    fn a_long_budget_is_capped_not_proportional() {
        let bands = thresholds_for(7_200_000);
        assert_eq!(bands, THRESHOLD_CAPS_MS, "the caps must win on a long turn");
    }

    /// The benchmark's own budget keeps the shape the ladder was tuned on.
    #[test]
    fn the_benchmark_budget_reproduces_the_tuned_shape() {
        let bands = thresholds_for(840_000);
        assert_eq!(bands, [588_000, 294_000, 120_000]);
    }

    /// The notice is engine text riding a `User`-role message, so it must
    /// open with a marker the engine's table knows — an unmarked notice is a
    /// genuine user turn to `turn_start_index`, which then resets the
    /// loop-detection and confident-zero windows mid-turn (#2837's class).
    #[test]
    fn the_notice_opens_with_a_registered_engine_marker() {
        let mut notices = DeadlineNotices::default();
        notices.due(840_000);
        let text = notices.due(580_000).expect("the first band must fire");
        assert!(
            crate::engine_markers::ENGINE_MARKERS
                .iter()
                .any(|marker| text.starts_with(marker)),
            "the deadline notice carries no registered engine marker: {text}"
        );
    }

    /// Byte-stability (AGENTS.md #7): a turn that keeps ticking inside one
    /// band must add nothing to the transcript.
    #[test]
    fn the_same_band_never_fires_twice() {
        let mut notices = DeadlineNotices::default();
        notices.due(840_000);
        assert!(notices.due(580_000).is_some());
        assert!(notices.due(500_000).is_none());
        assert!(notices.due(400_000).is_none());
    }

    /// Each tighter band is its own notice — the advice changes, so the
    /// model has to hear it again.
    #[test]
    fn each_tighter_band_fires_once() {
        let mut notices = DeadlineNotices::default();
        notices.due(840_000);
        assert!(notices.due(580_000).is_some());
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
        notices.due(840_000);
        let text = notices.due(90_000).expect("must fire");
        assert!(text.contains("submit your best answer"), "{text}");
        assert!(notices.due(80_000).is_none());
    }

    /// Nothing fires while there is plenty of time — a deadline hours away
    /// must not put a clock in the prompt.
    #[test]
    fn a_distant_deadline_says_nothing() {
        let mut notices = DeadlineNotices::default();
        notices.due(7_200_000);
        assert!(notices.due(3_600_000).is_none());
    }

    /// A clock that appears to go backwards (a re-armed deadline) must not
    /// re-fire a notice the turn has already heard.
    #[test]
    fn a_clock_going_backwards_cannot_re_fire() {
        let mut notices = DeadlineNotices::default();
        notices.due(840_000);
        assert!(notices.due(110_000).is_some());
        assert!(notices.due(580_000).is_none());
    }
}
