// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The run-level USD ceiling `--spend-limit` promises, and the accounting that
//! makes it true (#4353).
//!
//! # What was wrong
//!
//! `--spend-limit` is declared once, globally, and its help says it bounds "the
//! whole session … cumulative spend across every turn and goal round". `drive`
//! handed that number to each child unchanged — triage turns, work turns, retry
//! turns — and every child is its own `stella run` session with a fresh
//! ceiling. `stella self-driving drive --max-issues 10 --spend-limit 30` could
//! therefore spend ten times thirty, plus a triage turn per issue on top; the
//! only way to honour the brief was an external watchdog summing
//! `executions.cost_usd` out of `store.db` and killing the process.
//!
//! # Why the budget owns the flags
//!
//! Because the narrowing and the accounting are two halves of one fact, and a
//! design that separates them is a design where the next caller pays a turn
//! against the original cap. Every child turn in this loop is spawned by
//! [`super::work::run_turn`], so that function takes the budget rather than the
//! flags: it cannot narrow the ceiling without also folding the cost back in,
//! and it cannot be called at all with a plain [`TurnFlags`] that has forgotten
//! what the run already spent.
//!
//! # Where the loop stops
//!
//! Between units, never mid-unit — the same discipline as invariant 6, and for
//! the same reason: a turn cut down where it stands leaves a worktree, a branch
//! and possibly a pull request that nothing has recorded. [`RunBudget::spent`]
//! is checked at the top of the loop, so a run that is out of money reports
//! *budget reached* and returns rather than claiming another issue. The refusal
//! inside `run_turn` is the second line: a work unit whose triage turn spent
//! the remainder must not then spawn a work turn with a ceiling of zero.
//!
//! # The measurement
//!
//! A child turn's `--output-format json` summary carries `cost_usd` at its root
//! (`agent::summary::print_json_summary`), for a completed turn and an aborted
//! one alike. That number is the child's own accounting of what it spent, which
//! is the same accounting `store.db` records — not a re-derivation from token
//! counts here. A summary this build cannot parse contributes nothing, and that
//! is the unsafe direction: an unparseable summary under-counts the run's
//! spend, so the ceiling is a ceiling on *measured* spend. What bounds the gap
//! is the child's own per-turn ceiling, which is still handed down.

use super::turn_flags::TurnFlags;

/// One drive run's ceiling and what has gone against it.
///
/// `None` for the cap is the observed mode `--spend-limit`'s help already
/// describes: spend is still summed, so the loop can report what a run cost,
/// and nothing is ever refused.
#[derive(Debug, Clone)]
pub(super) struct RunBudget {
    /// The flags every child turn inherits, ceiling included.
    flags: TurnFlags,
    /// What the child turns have reported spending, in USD.
    spent: f64,
}

/// Why a turn was not started.
///
/// A type rather than a `String` because the caller branches on it: `drive`
/// ends the run and reports *budget reached*, while the same condition inside a
/// work unit defers that issue. A message would make both read the prose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Exhausted {
    /// The run's ceiling.
    pub cap: f64,
    /// What it had already spent when the turn was refused.
    pub spent: f64,
}

impl std::fmt::Display for Exhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the run's ${:.2} spend limit is reached (${:.2} spent); no further turn was started",
            self.cap, self.spent
        )
    }
}

impl RunBudget {
    /// A budget over the flags this invocation parsed.
    pub(super) fn new(flags: TurnFlags) -> Self {
        Self { flags, spent: 0.0 }
    }

    /// The run's ceiling, when one was set.
    pub(super) fn cap(&self) -> Option<f64> {
        self.flags.spend_limit
    }

    /// What this run's turns have reported spending.
    pub(super) fn spent(&self) -> f64 {
        self.spent
    }

    /// What is left of the ceiling, or `None` in observed mode.
    pub(super) fn remaining(&self) -> Option<f64> {
        self.cap().map(|cap| (cap - self.spent).max(0.0))
    }

    /// Has the run reached its ceiling?
    ///
    /// Never true in observed mode: a run with no ceiling cannot exhaust one.
    pub(super) fn exhausted(&self) -> Option<Exhausted> {
        let cap = self.cap()?;
        (self.spent >= cap).then_some(Exhausted {
            cap,
            spent: self.spent,
        })
    }

    /// The flags the next child turn gets: the same routing, and the ceiling
    /// narrowed to what is left of the run's.
    ///
    /// The child still gets a ceiling of its own — this hands it the remainder
    /// rather than removing it — so a single runaway turn is stopped by the
    /// engine's own budget guard at a safe boundary rather than by this loop
    /// discovering the overspend after the fact.
    pub(super) fn next_turn_flags(&self) -> Result<TurnFlags, Exhausted> {
        if let Some(exhausted) = self.exhausted() {
            return Err(exhausted);
        }
        let mut flags = self.flags.clone();
        flags.spend_limit = self.remaining();
        Ok(flags)
    }

    /// Fold a finished turn's reported spend in.
    ///
    /// Takes the turn's whole stdout rather than a number, because the parse is
    /// the part that can be wrong and it belongs beside the contract it reads.
    pub(super) fn record(&mut self, summary: &str) {
        self.spent += turn_cost(summary).unwrap_or(0.0);
    }
}

/// The `cost_usd` a child turn's JSON summary reports.
///
/// The root field only. A summary also carries the turn's whole event journal,
/// and every `step_usage` in it carries a `cost_usd` of its own — summing those
/// instead would double-count against the root total, which is already their
/// sum. `None` for anything that does not parse or does not carry the field: a
/// guess here would be a number the run's ceiling was then enforced against.
fn turn_cost(summary: &str) -> Option<f64> {
    serde_json::from_str::<serde_json::Value>(summary)
        .ok()?
        .get("cost_usd")?
        .as_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(cost: f64) -> String {
        serde_json::json!({
            "schema_version": 1,
            "status": "completed",
            "cost_usd": cost,
            "events": [{ "type": "step_usage", "cost_usd": cost }],
        })
        .to_string()
    }

    /// **Witness (#4353).** The second turn of a run is offered what is left of
    /// the cap, not the cap.
    ///
    /// Fails on the base, where `drive` handed the same `TurnFlags` to every
    /// child and nothing summed what came back: ten issues under
    /// `--spend-limit 30` could spend $300, plus a triage turn each.
    #[test]
    fn the_next_turn_is_offered_what_is_left_not_the_cap() {
        let mut budget = RunBudget::new(TurnFlags {
            spend_limit: Some(30.0),
            ..TurnFlags::default()
        });
        assert_eq!(budget.next_turn_flags().unwrap().spend_limit, Some(30.0));

        budget.record(&summary(12.0));
        assert_eq!(
            budget.next_turn_flags().unwrap().spend_limit,
            Some(18.0),
            "the second turn is bounded by the remainder"
        );

        budget.record(&summary(18.0));
        assert_eq!(
            budget.next_turn_flags(),
            Err(Exhausted {
                cap: 30.0,
                spent: 30.0
            }),
            "and the run refuses to start a turn it cannot pay for"
        );
    }

    /// Overspending the last turn does not hand the next one a negative
    /// ceiling — it hands it nothing at all.
    #[test]
    fn an_overspent_run_refuses_rather_than_offering_a_negative_ceiling() {
        let mut budget = RunBudget::new(TurnFlags {
            spend_limit: Some(5.0),
            ..TurnFlags::default()
        });
        budget.record(&summary(7.5));
        assert_eq!(budget.remaining(), Some(0.0));
        assert!(budget.next_turn_flags().is_err());
    }

    /// Observed mode: spend is still summed, and nothing is ever refused.
    #[test]
    fn a_run_with_no_ceiling_meters_without_blocking() {
        let mut budget = RunBudget::new(TurnFlags::default());
        budget.record(&summary(4.0));
        budget.record(&summary(6.0));
        assert_eq!(budget.spent(), 10.0);
        assert_eq!(budget.exhausted(), None);
        assert_eq!(budget.next_turn_flags().unwrap().spend_limit, None);
    }

    /// The other routing flags survive the narrowing. A budget that rebuilt the
    /// flags from the ceiling alone would silently undo #4352 — the model,
    /// the endpoint and the write authority would stop reaching the child.
    #[test]
    fn narrowing_the_ceiling_keeps_every_other_flag() {
        let flags = TurnFlags {
            model: Some("openrouter/moonshotai/kimi-k3".into()),
            base_url: Some("https://example.invalid/v1".into()),
            upstream_pin: vec!["together".into()],
            allow_dir: vec!["/tmp/scratch".into()],
            spend_limit: Some(9.0),
            turn_timeout: Some(std::time::Duration::from_secs(600)),
            max_output_tokens: Some(8192),
        };
        let mut budget = RunBudget::new(flags.clone());
        budget.record(&summary(3.0));

        let next = budget.next_turn_flags().expect("still inside the ceiling");
        assert_eq!(next.spend_limit, Some(6.0));
        assert_eq!(
            next,
            TurnFlags {
                spend_limit: Some(6.0),
                ..flags
            },
            "only the ceiling moves"
        );
    }

    /// An aborted turn still spent money, and the summary it printed still
    /// carries the number. Dropping it would let a run of failing turns spend
    /// without bound.
    #[test]
    fn an_aborted_turns_spend_still_counts() {
        let mut budget = RunBudget::new(TurnFlags {
            spend_limit: Some(10.0),
            ..TurnFlags::default()
        });
        budget.record(
            &serde_json::json!({
                "schema_version": 1,
                "status": "aborted",
                "reason": "the budget guard stopped the turn",
                "cost_usd": 4.25,
            })
            .to_string(),
        );
        assert_eq!(budget.spent(), 4.25);
    }

    /// A summary this build cannot read contributes nothing rather than a
    /// guess. Stated in the module docs as the unsafe direction: it
    /// under-counts, and the child's own per-turn ceiling is what bounds it.
    #[test]
    fn an_unreadable_summary_contributes_no_spend() {
        let mut budget = RunBudget::new(TurnFlags::default());
        budget.record("not json at all");
        budget.record(r#"{"status":"completed"}"#);
        assert_eq!(budget.spent(), 0.0);
    }

    /// The root total, never the sum of the events under it — those are its
    /// components, and adding both would bill the run twice.
    #[test]
    fn the_root_total_is_read_not_the_per_step_components() {
        let summary = serde_json::json!({
            "cost_usd": 5.0,
            "events": [
                { "type": "step_usage", "cost_usd": 2.0 },
                { "type": "step_usage", "cost_usd": 3.0 },
            ],
        })
        .to_string();
        assert_eq!(turn_cost(&summary), Some(5.0));
    }
}
