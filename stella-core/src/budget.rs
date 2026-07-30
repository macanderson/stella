//! USD spend metering — the *money* meter. This is
//! a distinct concern from `crate::compaction`'s *token*-budget eviction:
//! compaction manages context-window pressure, `budget` manages dollars
//! spent against a turn and/or session cap. Ported from the TS per-turn
//! budget, generalized to also cover a session scope and media spend.
//!
//! # The mid-tool-kill lesson
//!
//! `enforced` mode is "a hard stop with a
//! clean turn abort — never a mid-tool kill." This module cannot enforce
//! *when* the caller checks the outcome — that discipline belongs to the
//! `driver.rs` step-driver, which only consults
//! [`BudgetGuard::record_spend`]'s (or [`BudgetGuard::evaluate`]'s) return
//! value **between steps**, after a model/media call has fully completed and
//! before the next one is dispatched — never while a tool call is
//! in-flight. Killing a tool mid-execution to enforce a budget was a TS-era
//! defect: it leaves the workspace and the model's view of it inconsistent.
//! [`BudgetOutcome::AbortTurn`] is a *recommendation* the driver acts on at
//! that safe boundary; this module has zero I/O and cannot itself abort
//! anything.
//!
//! # Money vs. tokens
//!
//! [`BudgetGuard::record_spend`] takes a bare `cost_usd: f64` and does not
//! care what produced it — a text completion, an image job, or a video job
//! all settle through the same call ("media counts"), which is what lets
//! `stella-media`'s spend meter into the same guard with no special-casing.
//!
//! # Precision
//!
//! Spend accumulates via plain `f64` addition. This guard is an in-memory
//! running total for gating and HUD display (`BudgetTick`) — it is not the
//! billing system of record (that
//! lives in adapter-reported usage plus, for the platform's own metered
//! products, ClickHouse/Stripe). Summation drift across a session-length
//! number of calls is negligible at USD-cent granularity; callers needing
//! exact reconciliation should do so against the authoritative usage log,
//! not this guard.

use stella_protocol::BudgetMode;

/// Which scope a [`BudgetOutcome`] is reporting against. A guard configured
/// with both a turn and a session limit checks them independently — the
/// turn axis first (the more local, more frequently-reset scope), then the
/// session axis — so a caller under its turn limit but over its session
/// limit still gets a session-scoped outcome rather than a false "all
/// clear."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetAxis {
    /// Spend since the last [`BudgetGuard::begin_turn`] call.
    Turn,
    /// Spend since the guard was constructed (accumulates across turns).
    Session,
}

/// The result of a spend check, computed after [`BudgetGuard::record_spend`]
/// or on demand via [`BudgetGuard::evaluate`].
///
/// See the module-level docs for the mid-tool-kill contract: `AbortTurn` is
/// a signal for the driver to consult **between steps**, never a directive
/// this module enforces itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BudgetOutcome {
    /// Under all configured limits (or metering is off, or no limit is
    /// configured on any axis). Safe to start the next step.
    Continue,
    /// Over a configured limit in `observed` mode: keep going, but the
    /// driver should surface a warning (e.g. in the TUI HUD). The guard
    /// keeps accepting further `record_spend` calls — `observed` never
    /// blocks.
    Warn {
        axis: BudgetAxis,
        spent_usd: f64,
        limit_usd: f64,
    },
    /// Over a configured limit in `enforced` mode: the driver must not
    /// dispatch another step this turn. Per the module contract, this must
    /// only be checked at a step boundary — never used to interrupt a tool
    /// already in flight.
    AbortTurn {
        axis: BudgetAxis,
        spent_usd: f64,
        limit_usd: f64,
    },
}

/// Tracks USD spend against an optional per-turn limit and an optional
/// per-session limit, and reports a [`BudgetOutcome`] after every recorded
/// cost.
///
/// A `None` limit on an axis means that axis never triggers, regardless of
/// mode. A session accumulates spend across multiple turns; call
/// [`begin_turn`](Self::begin_turn) when a new turn starts to reset the
/// turn-scoped counter while the session-scoped total keeps accumulating.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BudgetGuard {
    mode: BudgetMode,
    turn_limit_usd: Option<f64>,
    session_limit_usd: Option<f64>,
    turn_spent_usd: f64,
    session_spent_usd: f64,
}

impl BudgetGuard {
    /// Construct a new guard. `mode` governs whether breaches ever produce a
    /// [`BudgetOutcome`] other than `Continue` (`Off` never does); the two
    /// limits are independent and either or both may be `None`.
    pub fn new(
        mode: BudgetMode,
        turn_limit_usd: Option<f64>,
        session_limit_usd: Option<f64>,
    ) -> Self {
        Self {
            mode,
            turn_limit_usd,
            session_limit_usd,
            turn_spent_usd: 0.0,
            session_spent_usd: 0.0,
        }
    }

    /// Record one call's cost (a text completion, an image job, a video
    /// job — this guard is generic over the source, see module docs) and
    /// return the resulting [`BudgetOutcome`]. Always accumulates on both
    /// the turn and session axes, in every mode, so `spent_usd`/
    /// `session_spent_usd` remain a truthful running total even when
    /// `mode` is `Off` — only *gating* is mode-dependent, accounting never
    /// is.
    pub fn record_spend(&mut self, cost_usd: f64) -> BudgetOutcome {
        self.turn_spent_usd += cost_usd;
        self.session_spent_usd += cost_usd;
        self.evaluate()
    }

    /// Evaluate the current spend against the configured limits without
    /// recording anything new. This is the "safe boundary" check the
    /// `driver.rs` step-driver runs between steps — see module docs.
    pub fn evaluate(&self) -> BudgetOutcome {
        self.check_axis(BudgetAxis::Turn, self.turn_spent_usd, self.turn_limit_usd)
            .or_else(|| {
                self.check_axis(
                    BudgetAxis::Session,
                    self.session_spent_usd,
                    self.session_limit_usd,
                )
            })
            .unwrap_or(BudgetOutcome::Continue)
    }

    fn check_axis(
        &self,
        axis: BudgetAxis,
        spent_usd: f64,
        limit_usd: Option<f64>,
    ) -> Option<BudgetOutcome> {
        let limit_usd = limit_usd?;
        if spent_usd <= limit_usd {
            return None;
        }
        match self.mode {
            BudgetMode::Off => None,
            BudgetMode::Observed => Some(BudgetOutcome::Warn {
                axis,
                spent_usd,
                limit_usd,
            }),
            BudgetMode::Enforced => Some(BudgetOutcome::AbortTurn {
                axis,
                spent_usd,
                limit_usd,
            }),
        }
    }

    /// Reset the turn-scoped counter to zero at the start of a new turn.
    /// The session-scoped total is untouched — a session accumulates across
    /// every turn it contains.
    pub fn begin_turn(&mut self) {
        self.turn_spent_usd = 0.0;
    }

    /// Set the session-scoped accumulator to exactly `spent_usd`, replacing
    /// the running total. The turn-scoped counter is untouched.
    ///
    /// This is the resume/switch seam: when the driver reopens a session
    /// (`stella resume` at startup, or an in-deck session switch), it
    /// reseeds the guard from the target session's journaled spend so a
    /// configured `--budget` always means "this session" — the one on
    /// screen — never "this process". Within one session spend stays
    /// monotone ([`record_spend`](Self::record_spend) only ever adds);
    /// across sessions monotonicity is deliberately the caller's concern —
    /// switching to a cheaper session legitimately lowers the accumulator.
    pub fn reseed_session_spend(&mut self, spent_usd: f64) {
        self.session_spent_usd = spent_usd;
    }

    /// Current spend for the active turn, in USD.
    pub fn spent_usd(&self) -> f64 {
        self.turn_spent_usd
    }

    /// Current spend for the whole session, in USD (accumulates across
    /// turns; see [`begin_turn`](Self::begin_turn)).
    pub fn session_spent_usd(&self) -> f64 {
        self.session_spent_usd
    }

    /// The configured per-turn limit, if any.
    pub fn turn_limit_usd(&self) -> Option<f64> {
        self.turn_limit_usd
    }

    /// The configured per-session limit, if any.
    pub fn session_limit_usd(&self) -> Option<f64> {
        self.session_limit_usd
    }

    /// The mode this guard was constructed with.
    pub fn mode(&self) -> BudgetMode {
        self.mode
    }

    /// Remaining headroom before the *tightest* configured limit trips, or
    /// `None` when no axis bounds this guard at all.
    ///
    /// Both axes are consulted because a caller under its session limit but
    /// already over its turn limit has no headroom either — taking only the
    /// session axis is how a "hard ceiling" quietly becomes a soft one.
    /// Never negative: an already-breached guard reports `Some(0.0)`.
    #[must_use]
    pub fn headroom_usd(&self) -> Option<f64> {
        let turn = self
            .turn_limit_usd
            .map(|limit| (limit - self.turn_spent_usd).max(0.0));
        let session = self
            .session_limit_usd
            .map(|limit| (limit - self.session_spent_usd).max(0.0));
        match (turn, session) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (only, None) | (None, only) => only,
        }
    }

    /// Carve a child allowance out of this guard's remaining headroom — the
    /// budget half of a sub-agent spawn (`crate::subagent`).
    ///
    /// The returned guard is independent: the child spends against it and
    /// aborts its own turn when it trips, without ever mutating the parent.
    /// The parent learns the cost exactly once, at the end, via
    /// [`settle_child`](Self::settle_child).
    ///
    /// # The ceiling is a minimum, not the request
    ///
    /// `min(requested, headroom)`. A child can therefore never spend the
    /// parent past a configured cap even if the caller asks for more than is
    /// left, which is what keeps `--budget` a *hard* ceiling once nested
    /// turns exist. A caller who asks for `None` still inherits the parent's
    /// headroom as its ceiling — "unbounded child" is not expressible while
    /// the parent is bounded, deliberately.
    ///
    /// # Mode is inherited, never escalated
    ///
    /// `Observed` means the operator asked for warnings, not hard stops; a
    /// child that hard-stopped under an observed session would violate that
    /// stated posture, and `Off` means no gating anywhere. So a requested cap
    /// under `Off`/`Observed` meters and warns but does not abort — the same
    /// contract the parent turn runs under, one level down. Only `Enforced`
    /// makes the carve a wall.
    ///
    /// # Turn axis vs session axis
    ///
    /// The ceiling lands on the child's SESSION axis, not its turn axis: a
    /// child's whole life is one turn, and nothing calls
    /// [`begin_turn`](Self::begin_turn) on it, so a session-axis limit is the
    /// one that cannot be reset out from under the guarantee.
    #[must_use]
    pub fn carve(&self, requested_usd: Option<f64>) -> BudgetGuard {
        let ceiling = match (requested_usd, self.headroom_usd()) {
            (Some(requested), Some(headroom)) => Some(requested.min(headroom)),
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        };
        BudgetGuard {
            mode: self.mode,
            turn_limit_usd: None,
            session_limit_usd: ceiling.map(|c| c.max(0.0)),
            turn_spent_usd: 0.0,
            session_spent_usd: 0.0,
        }
    }

    /// Whether a carve has room to do any work at all.
    ///
    /// A carve of exactly zero is what a parent that has already breached an
    /// enforced cap produces. Spawning against it would pay for one model
    /// call (budget is checked BETWEEN steps — the first call always lands)
    /// and then abort, so the caller refuses the spawn instead and the child
    /// costs nothing. Under a non-enforcing mode a zero carve is not a wall,
    /// so it stays viable.
    #[must_use]
    pub fn is_viable_carve(&self) -> bool {
        if self.mode != BudgetMode::Enforced {
            return true;
        }
        self.session_limit_usd.is_none_or(|limit| limit > 0.0)
    }

    /// Fold a finished child's total spend back into this guard, on both
    /// axes, and report the parent's resulting outcome.
    ///
    /// This is what keeps a nested turn from being a hole in the accounting:
    /// the child metered every call into its own carve, and this is the one
    /// place that money becomes the parent's. Call it exactly once per
    /// child, on every path including a failed one — a child that aborted
    /// still spent what it spent.
    ///
    /// The returned outcome is the PARENT's, evaluated after the fold, so a
    /// caller whose child pushed it over a cap sees `AbortTurn` at its own
    /// next step boundary rather than one call later.
    pub fn settle_child(&mut self, child: &BudgetGuard) -> BudgetOutcome {
        self.record_spend(child.session_spent_usd())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spend_is_monotonically_non_decreasing_and_exact() {
        // Property: turn_spent_usd/session_spent_usd never go down as calls
        // accumulate, and the running total equals the exact sum of every
        // recorded cost_usd (within float tolerance — see module docs on
        // precision).
        let mut guard = BudgetGuard::new(BudgetMode::Observed, None, None);
        let costs = [0.001, 0.02, 0.0003, 1.5, 0.0, 0.25, 0.0001];

        let mut running_total = 0.0f64;
        let mut previous_turn = 0.0f64;
        let mut previous_session = 0.0f64;
        for cost in costs {
            guard.record_spend(cost);
            running_total += cost;

            assert!(
                guard.spent_usd() >= previous_turn,
                "turn spend must never decrease"
            );
            assert!(
                guard.session_spent_usd() >= previous_session,
                "session spend must never decrease"
            );
            previous_turn = guard.spent_usd();
            previous_session = guard.session_spent_usd();
        }

        assert!(
            (guard.spent_usd() - running_total).abs() < 1e-9,
            "turn total {} must equal sum of recorded costs {}",
            guard.spent_usd(),
            running_total
        );
        assert!(
            (guard.session_spent_usd() - running_total).abs() < 1e-9,
            "session total {} must equal sum of recorded costs {}",
            guard.session_spent_usd(),
            running_total
        );
    }

    #[test]
    fn off_mode_never_warns_or_aborts_even_wildly_over_limit() {
        let mut guard = BudgetGuard::new(BudgetMode::Off, Some(1.0), Some(1.0));
        for _ in 0..10 {
            let outcome = guard.record_spend(1000.0);
            assert_eq!(outcome, BudgetOutcome::Continue);
        }
        // Off still meters (accounting is not mode-dependent), just never gates.
        assert!(guard.spent_usd() > 1.0);
    }

    #[test]
    fn observed_mode_warns_over_limit_but_never_blocks_further_recording() {
        let mut guard = BudgetGuard::new(BudgetMode::Observed, Some(1.0), None);
        assert_eq!(guard.record_spend(0.5), BudgetOutcome::Continue);

        let outcome = guard.record_spend(0.6); // now 1.1, over the 1.0 turn limit
        match outcome {
            BudgetOutcome::Warn {
                axis: BudgetAxis::Turn,
                spent_usd,
                limit_usd,
            } => {
                assert!((spent_usd - 1.1).abs() < 1e-9);
                assert_eq!(limit_usd, 1.0);
            }
            other => panic!("expected Warn, got {other:?}"),
        }

        // Observed never blocks — further recording keeps working.
        let outcome2 = guard.record_spend(5.0);
        assert!(matches!(outcome2, BudgetOutcome::Warn { .. }));
        assert!((guard.spent_usd() - 6.1).abs() < 1e-9);
    }

    #[test]
    fn enforced_mode_continues_under_limit_and_aborts_once_exceeded() {
        let mut guard = BudgetGuard::new(BudgetMode::Enforced, Some(2.0), None);
        assert_eq!(guard.record_spend(1.0), BudgetOutcome::Continue);
        assert_eq!(guard.record_spend(0.9), BudgetOutcome::Continue); // 1.9, still under

        let outcome = guard.record_spend(0.2); // 2.1, over
        match outcome {
            BudgetOutcome::AbortTurn {
                axis: BudgetAxis::Turn,
                spent_usd,
                limit_usd,
            } => {
                assert!((spent_usd - 2.1).abs() < 1e-9);
                assert_eq!(limit_usd, 2.0);
            }
            other => panic!("expected AbortTurn, got {other:?}"),
        }
    }

    #[test]
    fn none_limit_never_triggers_regardless_of_mode() {
        for mode in [BudgetMode::Off, BudgetMode::Observed, BudgetMode::Enforced] {
            let mut guard = BudgetGuard::new(mode, None, None);
            let outcome = guard.record_spend(1_000_000.0);
            assert_eq!(
                outcome,
                BudgetOutcome::Continue,
                "mode {mode:?} with no configured limits must never trigger"
            );
        }
    }

    #[test]
    fn turn_and_session_limits_are_checked_independently() {
        // Under the turn limit but over the session limit must still
        // trigger, and it must report the SESSION axis's numbers, not the
        // turn axis's.
        let mut guard = BudgetGuard::new(BudgetMode::Enforced, Some(10.0), Some(2.0));
        guard.record_spend(1.0); // turn=1.0 (under 10.0), session=1.0 (under 2.0)
        let outcome = guard.record_spend(1.5); // turn=2.5 (under 10.0), session=2.5 (over 2.0)

        match outcome {
            BudgetOutcome::AbortTurn {
                axis: BudgetAxis::Session,
                spent_usd,
                limit_usd,
            } => {
                assert!((spent_usd - 2.5).abs() < 1e-9);
                assert_eq!(limit_usd, 2.0);
            }
            other => panic!("expected a session-axis AbortTurn, got {other:?}"),
        }
        assert!(guard.spent_usd() < guard.turn_limit_usd().unwrap());
    }

    #[test]
    fn turn_axis_is_reported_when_only_the_turn_limit_is_breached() {
        let mut guard = BudgetGuard::new(BudgetMode::Enforced, Some(1.0), Some(100.0));
        let outcome = guard.record_spend(1.5); // turn over, session nowhere near its limit

        match outcome {
            BudgetOutcome::AbortTurn {
                axis: BudgetAxis::Turn,
                spent_usd,
                limit_usd,
            } => {
                assert!((spent_usd - 1.5).abs() < 1e-9);
                assert_eq!(limit_usd, 1.0);
            }
            other => panic!("expected a turn-axis AbortTurn, got {other:?}"),
        }
    }

    #[test]
    fn begin_turn_resets_turn_spend_but_not_session_spend() {
        let mut guard = BudgetGuard::new(BudgetMode::Observed, Some(5.0), None);
        guard.record_spend(3.0);
        assert!((guard.spent_usd() - 3.0).abs() < 1e-9);
        assert!((guard.session_spent_usd() - 3.0).abs() < 1e-9);

        guard.begin_turn();
        assert_eq!(guard.spent_usd(), 0.0);
        assert!(
            (guard.session_spent_usd() - 3.0).abs() < 1e-9,
            "session total must survive a turn reset"
        );

        guard.record_spend(1.0);
        assert!((guard.spent_usd() - 1.0).abs() < 1e-9);
        assert!(
            (guard.session_spent_usd() - 4.0).abs() < 1e-9,
            "session total accumulates across turns"
        );
    }

    #[test]
    fn evaluate_reports_without_recording_anything() {
        let mut guard = BudgetGuard::new(BudgetMode::Enforced, Some(1.0), None);
        guard.record_spend(2.0);
        assert!(matches!(guard.evaluate(), BudgetOutcome::AbortTurn { .. }));
        // Calling evaluate() repeatedly must not change the running total.
        let before = guard.spent_usd();
        let _ = guard.evaluate();
        let _ = guard.evaluate();
        assert_eq!(guard.spent_usd(), before);
    }

    #[test]
    fn reseed_replaces_the_session_total_and_record_spend_accumulates_from_it() {
        let mut guard = BudgetGuard::new(BudgetMode::Observed, None, None);
        guard.record_spend(5.0);
        assert!((guard.session_spent_usd() - 5.0).abs() < 1e-9);

        // The resume/switch seam: the session accumulator becomes exactly
        // the seed — down as well as up (switching to a cheaper session
        // legitimately lowers it) — and the turn counter is untouched.
        guard.reseed_session_spend(1.25);
        assert!((guard.session_spent_usd() - 1.25).abs() < 1e-9);
        assert!(
            (guard.spent_usd() - 5.0).abs() < 1e-9,
            "turn spend must survive a session reseed"
        );

        // Subsequent spend accumulates from the seed, not from zero and not
        // from the pre-reseed total.
        guard.record_spend(0.5);
        assert!((guard.session_spent_usd() - 1.75).abs() < 1e-9);
    }

    #[test]
    fn enforced_mode_gates_relative_to_the_reseeded_session_total() {
        let mut guard = BudgetGuard::new(BudgetMode::Enforced, None, Some(2.0));
        guard.record_spend(1.9); // near the session limit
        assert_eq!(guard.evaluate(), BudgetOutcome::Continue);

        // Reseeding DOWN (adopting a cheaper session) restores headroom…
        guard.reseed_session_spend(0.5);
        assert_eq!(guard.evaluate(), BudgetOutcome::Continue);
        assert_eq!(guard.record_spend(1.0), BudgetOutcome::Continue); // 1.5
        // …until real spend crosses the limit relative to the seed.
        match guard.record_spend(0.6) {
            // 2.1
            BudgetOutcome::AbortTurn {
                axis: BudgetAxis::Session,
                spent_usd,
                limit_usd,
            } => {
                assert!((spent_usd - 2.1).abs() < 1e-9);
                assert_eq!(limit_usd, 2.0);
            }
            other => panic!("expected a session-axis AbortTurn, got {other:?}"),
        }

        // Reseeding UP (adopting a session already over budget) gates
        // immediately at the next safe-boundary check.
        let mut over = BudgetGuard::new(BudgetMode::Enforced, None, Some(2.0));
        over.reseed_session_spend(3.0);
        assert!(matches!(
            over.evaluate(),
            BudgetOutcome::AbortTurn {
                axis: BudgetAxis::Session,
                ..
            }
        ));
    }

    #[test]
    fn exactly_at_the_limit_is_not_a_breach() {
        let mut guard = BudgetGuard::new(BudgetMode::Enforced, Some(1.0), None);
        let outcome = guard.record_spend(1.0);
        assert_eq!(
            outcome,
            BudgetOutcome::Continue,
            "spend == limit must not abort"
        );
    }

    // ---- carve / settle (sub-agent budgets, #922) --------------------

    #[test]
    fn headroom_takes_the_tightest_axis_and_never_goes_negative() {
        // Session has plenty left; the turn axis is what actually binds.
        let mut guard = BudgetGuard::new(BudgetMode::Enforced, Some(1.0), Some(10.0));
        guard.record_spend(0.75);
        assert!((guard.headroom_usd().unwrap() - 0.25).abs() < 1e-9);

        // Breached: headroom clamps at zero rather than reporting a debt,
        // so `carve` can never mint a negative ceiling.
        guard.record_spend(5.0);
        assert_eq!(guard.headroom_usd(), Some(0.0));

        // No limits configured anywhere: genuinely unbounded.
        assert_eq!(
            BudgetGuard::new(BudgetMode::Enforced, None, None).headroom_usd(),
            None
        );
    }

    #[test]
    fn a_carve_can_never_exceed_the_parents_remaining_headroom() {
        // THE hard-ceiling property: whatever a caller asks for, the child's
        // ceiling is bounded by what the parent has left. Swept across
        // requests both under and wildly over the parent's remaining room.
        let mut parent = BudgetGuard::new(BudgetMode::Enforced, None, Some(1.0));
        parent.record_spend(0.9);
        let headroom = parent.headroom_usd().expect("session limit is set");

        for requested in [None, Some(0.01), Some(0.05), Some(0.1), Some(50.0)] {
            let child = parent.carve(requested);
            let ceiling = child
                .session_limit_usd()
                .expect("a bounded parent always bounds its child");
            assert!(
                ceiling <= headroom + 1e-9,
                "carve for {requested:?} was {ceiling}, above the parent's {headroom} headroom"
            );
            if let Some(requested) = requested {
                assert!(
                    ceiling <= requested + 1e-9,
                    "carve must also honor the smaller request"
                );
            }
        }
    }

    #[test]
    fn an_unbounded_parent_passes_the_request_through_and_stays_unbounded_when_none() {
        let parent = BudgetGuard::new(BudgetMode::Enforced, None, None);
        assert_eq!(parent.carve(Some(0.25)).session_limit_usd(), Some(0.25));
        assert_eq!(parent.carve(None).session_limit_usd(), None);
    }

    #[test]
    fn a_carve_lands_on_the_session_axis_so_begin_turn_cannot_reset_it() {
        // The child's ceiling must survive anything that resets a turn
        // counter, or the guarantee evaporates the moment someone calls
        // `begin_turn` on a child guard.
        let parent = BudgetGuard::new(BudgetMode::Enforced, Some(2.0), None);
        let mut child = parent.carve(Some(0.5));
        assert_eq!(child.turn_limit_usd(), None);
        assert_eq!(child.session_limit_usd(), Some(0.5));

        child.record_spend(0.6);
        child.begin_turn();
        assert!(
            matches!(
                child.evaluate(),
                BudgetOutcome::AbortTurn {
                    axis: BudgetAxis::Session,
                    ..
                }
            ),
            "begin_turn must not clear a carve breach"
        );
    }

    #[test]
    fn carve_inherits_mode_and_never_escalates_gating() {
        // Observed/Off asked for no hard stops. A child that aborts under
        // them would be a stricter contract than the operator chose.
        for mode in [BudgetMode::Off, BudgetMode::Observed] {
            let parent = BudgetGuard::new(mode, None, Some(10.0));
            let mut child = parent.carve(Some(0.01));
            assert_eq!(child.mode(), mode);
            let outcome = child.record_spend(5.0);
            assert!(
                !matches!(outcome, BudgetOutcome::AbortTurn { .. }),
                "{mode:?} must not produce a hard stop, got {outcome:?}"
            );
        }
        // Enforced is the one mode where the carve is a wall.
        let parent = BudgetGuard::new(BudgetMode::Enforced, None, Some(10.0));
        let mut child = parent.carve(Some(0.01));
        assert!(matches!(
            child.record_spend(5.0),
            BudgetOutcome::AbortTurn { .. }
        ));
    }

    #[test]
    fn settling_a_child_moves_its_whole_spend_onto_the_parent_exactly_once() {
        // The accounting property: after settlement the parent's totals equal
        // what it would have shown had it spent the child's money itself.
        let mut parent = BudgetGuard::new(BudgetMode::Enforced, Some(1.0), Some(10.0));
        parent.record_spend(0.10);

        let mut child = parent.carve(Some(0.30));
        child.record_spend(0.07);
        child.record_spend(0.05);

        // Nothing has moved yet — the child spends against its own carve.
        assert!((parent.spent_usd() - 0.10).abs() < 1e-9);

        parent.settle_child(&child);
        assert!((parent.spent_usd() - 0.22).abs() < 1e-9, "turn axis");
        assert!(
            (parent.session_spent_usd() - 0.22).abs() < 1e-9,
            "session axis"
        );
    }

    #[test]
    fn settlement_reports_the_parents_own_breach_not_the_childs() {
        // A child that stays inside its carve can still push the parent over.
        // The parent must learn that at settlement, not one paid call later.
        let mut parent = BudgetGuard::new(BudgetMode::Enforced, None, Some(1.0));
        parent.record_spend(0.95);
        let mut child = parent.carve(Some(0.30)); // clamped to 0.05 headroom
        child.record_spend(0.05);
        assert_eq!(
            child.evaluate(),
            BudgetOutcome::Continue,
            "child is exactly at its ceiling, which is not a breach"
        );

        // 0.95 + 0.05 == 1.0, still not a breach. One more cent tips it.
        assert_eq!(parent.settle_child(&child), BudgetOutcome::Continue);
        let mut child2 = parent.carve(Some(0.30));
        child2.record_spend(0.01);
        assert!(matches!(
            parent.settle_child(&child2),
            BudgetOutcome::AbortTurn {
                axis: BudgetAxis::Session,
                ..
            }
        ));
    }

    #[test]
    fn a_zero_carve_is_only_unviable_when_the_mode_would_enforce_it() {
        let mut broke = BudgetGuard::new(BudgetMode::Enforced, None, Some(1.0));
        broke.record_spend(1.5);
        assert!(
            !broke.carve(Some(0.25)).is_viable_carve(),
            "an enforced parent with no headroom must refuse the spawn"
        );

        // Same arithmetic under observed: nothing would gate, so the child
        // is free to run and simply warns.
        let mut observed = BudgetGuard::new(BudgetMode::Observed, None, Some(1.0));
        observed.record_spend(1.5);
        assert!(observed.carve(Some(0.25)).is_viable_carve());

        // Unbounded is always viable.
        assert!(
            BudgetGuard::new(BudgetMode::Enforced, None, None)
                .carve(None)
                .is_viable_carve()
        );
    }
}
