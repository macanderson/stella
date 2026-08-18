// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The seam between [`crate::schedule`] — which knows how to walk a
//! manifest's declared stage order — and [`Pipeline::run`], which knows the
//! facts a turn actually has and when each one becomes available.
//!
//! Split from `pipeline.rs` for the reason every sibling in this directory
//! is: that file is a grandfathered god file closed to growth (AGENTS.md §
//! God files), so the machinery — and the error-handling boilerplate around
//! it — lives here (#3408).
//!
//! # Two decisions, at two different times
//!
//! [`stella_plugin::ProgressiveResolver`] must be asked about each declared
//! stage in the manifest's own order — `triage`, `recall`, `research`,
//! `plan`, `scope`, `execute`, `witness`, `verify` for the shipped `classic`
//! variant. `Pipeline::run`'s own control flow needs two of those answers at
//! two genuinely different times, and [`Pipeline::begin_turn_schedule`]
//! answers only the first:
//!
//! - **`witness`, pre-execute.** `authored_witness` has to be known before
//!   the user message is assembled — it decides whether the worker is told
//!   its own failing test is the run's only deterministic evidence
//!   ([`Pipeline::run`]'s `VerificationContract` assembly). So `witness` is
//!   decided here, immediately once triage's real facts exist, and
//!   [`refuse_witness_reading_post_triage`](crate::schedule::refuse_witness_reading_post_triage)
//!   rejects any variant whose `witness` condition could not honestly be
//!   answered this early — one reading `execute`'s, `witness`'s own, or
//!   `verify`'s published signals. `classic.toml`'s `if = "no-test-command"`
//!   is a host fact, so it always passes.
//! - **`verify`, post-execute, per candidate.** A verify condition MAY read
//!   what `execute` produced (`classic.toml`'s own `if = "verifies"` reads a
//!   triage-published signal, but a leaner variant's need not) — and that
//!   value does not exist until a candidate has actually run, and differs
//!   candidate to candidate in a best-of-N turn. This function stops at
//!   `scope`, one stage short of `execute`, and returns the [`Schedule`]
//!   still positioned there. Each candidate clones it
//!   ([`super::task_frame::TaskFrame::schedule`]) and decides `execute`,
//!   re-decides `witness` (asserted to agree with the answer computed here —
//!   both read the same triage-boundary facts, so
//!   [`refuse_witness_reading_post_triage`](crate::schedule::refuse_witness_reading_post_triage)
//!   having accepted the variant makes them the same computation run twice),
//!   and only then decides `verify` against its own real diff —
//!   [`Pipeline::decide_candidate_verify`], called from
//!   `Pipeline::run_candidate`.
//!
//! Before #3408 both were folded into one pre-execute batch, using a
//! `diff_lines: 0` / `mutating_actions: 0` placeholder for `verify` — sound
//! only because no shipped variant's `verify` condition read anything but a
//! triage-published signal, and silently wrong the moment one did (a
//! `diff-lines > 0` condition could never observe a real diff). See
//! `tests/variant_dispatch.rs`'s end-to-end witness for the failing case this
//! replaces.

use stella_plugin::{StageName, Wrapper};

use crate::schedule::{HostFacts, Schedule, ScheduleError, refuse_witness_reading_post_triage};
use crate::variant::{self, VariantError};

use super::*;

/// The manifest's PRE-execute answers `Pipeline::run` needs before any
/// candidate exists — `research`/`plan` (both decided fully here, since
/// nothing about them can vary per candidate) and `authored_witness` (see
/// this module's docs for why `witness` must be decided this early). `verify`
/// is deliberately absent: it is a per-candidate, post-execute decision now
/// (`Pipeline::decide_candidate_verify`).
pub(super) struct PreExecuteSchedule {
    /// Whether `research` runs — ANDed with the roster's own resolution of
    /// [`ModelCallRole::Research`] at its call site (host-internal, #3408).
    pub(super) research: bool,
    /// Whether `plan` AND `scope` both run. The two share one manifest
    /// condition (`classic.toml`'s `if = "plans"` on each) and one Rust
    /// branch — `scope_review` runs nested inside `plan_with_review` — so a
    /// variant that somehow disagreed between the two would still only ever
    /// gate the one branch the pipeline has to offer it.
    pub(super) plan: bool,
    /// Whether this turn authors its own witness test (L-E11 front half):
    /// `witness`'s manifest decision (`classic.toml`'s `if = "no-test-
    /// command"`) ANDed with five host-internal facts the closed condition
    /// grammar has no conjunction to fold into one clause (#3408): not
    /// conversational, the witness-author role enabled, the model's own
    /// `wants_witness`, the class verifying unconditionally, and an author
    /// independent of the worker actually being resolvable.
    pub(super) authored_witness: bool,
    /// `witness`'s bare manifest decision, before the five conjuncts above —
    /// carried only so each candidate's own re-decision of `witness` (see
    /// this module's docs) can assert it agrees.
    pub(super) witness_scheduled: bool,
}

impl Pipeline<'_> {
    /// The manifest this run follows: the configured variant, or the
    /// built-in `classic` order when none was configured. Owned, so the
    /// caller can hold it for as long as a [`Schedule`] borrowed from it
    /// needs to live.
    ///
    /// Build-time defect only: unreachable for a configured variant (already
    /// a parsed, validated [`Wrapper`] by the time it reaches
    /// [`PipelineConfig`]) and asserted unreachable for the built-in
    /// fallback by `tests/variant_program.rs`. `total_cost_usd` is only for
    /// the error path, so the two call sites need no separate `map_err`.
    pub(super) fn effective_variant(
        &self,
        total_cost_usd: f64,
    ) -> Result<Wrapper, PipelineRunError> {
        let resolved: Result<Wrapper, VariantError> = match &self.config.variant {
            Some(wrapper) => Ok(wrapper.clone()),
            None => variant::classic().cloned(),
        };
        resolved.map_err(|source| variant_error(source, total_cost_usd))
    }

    /// Resolve the PRE-execute half of `variant`'s schedule against triage's
    /// real facts — `research`, `plan`, and `authored_witness` — and return
    /// the [`Schedule`] positioned right after `scope`, ready for each
    /// candidate to clone and carry into `execute`/`witness`/`verify`. See
    /// this module's docs for why the split falls exactly there.
    pub(super) fn begin_turn_schedule<'v>(
        &self,
        variant: &'v Wrapper,
        budget: &BudgetGuard,
        assessment: &TaskAssessment,
        research_questions: usize,
        total_cost_usd: f64,
    ) -> Result<(PreExecuteSchedule, Schedule<'v>), PipelineRunError> {
        let task_class = assessment.class;
        let decided: Result<(PreExecuteSchedule, Schedule<'v>), ScheduleError> = (|| {
            refuse_witness_reading_post_triage(variant)?;
            let mut schedule = Schedule::new(
                variant,
                HostFacts {
                    test_command: self.config.test_command.is_some(),
                    candidates: self.config.candidate_count(),
                    budget_metered: budget.headroom_usd().is_some(),
                },
            );
            schedule.decide(StageName::Triage)?;
            schedule.decide(StageName::Recall)?;
            schedule.update(|v| {
                v.conversational = assessment.conversational;
                // Saturating: a question count past `u64::MAX` cannot exist.
                v.questions = u64::try_from(research_questions).unwrap_or(u64::MAX);
                v.plans = task_class.plans();
                v.verifies = task_class.verifies_unconditionally();
                v.wants_witness = assessment.wants_witness();
                v.wants_verifier = assessment.wants_verifier();
            });
            let research = schedule.decide(StageName::Research)?;
            let plan = schedule.decide(StageName::Plan)?;
            let scope = schedule.decide(StageName::Scope)?;
            // The shared prefix ends here. A throwaway CLONE answers
            // `witness` once more, right now, only because `authored_witness`
            // has to exist before any candidate does; `schedule` itself stays
            // at `scope` for every candidate to pick up from (module docs).
            let mut witness_probe = schedule.clone();
            witness_probe.decide(StageName::Execute)?;
            let witness_scheduled = witness_probe.decide(StageName::Witness)?;
            let authored_witness = !assessment.conversational
                && witness_scheduled
                && self.responsibility_enabled(ModelCallRole::WitnessAuthor)
                && assessment.wants_witness()
                && task_class.verifies_unconditionally()
                && self.can_author_independent_witness();
            Ok((
                PreExecuteSchedule {
                    research,
                    plan: plan && scope,
                    authored_witness,
                    witness_scheduled,
                },
                schedule,
            ))
        })();
        decided.map_err(|source| variant_error(source, total_cost_usd))
    }

    /// Per-candidate, post-execute: decide `verify` against what THIS
    /// candidate actually produced, not the pre-execute placeholder the old
    /// batch used (#3408 — the bug this whole module exists to fix).
    ///
    /// `schedule` must be [`super::task_frame::TaskFrame::schedule`]'s own
    /// clone, positioned at `scope` by [`Pipeline::begin_turn_schedule`].
    /// `mutating_actions` and `diff_lines` are this candidate's real,
    /// already-measured values (`state.signals.mutating_actions`,
    /// `state.diff_lines` in `Pipeline::run_candidate` — reused, not
    /// recomputed). `witness_authored` is deliberately always `false` here:
    /// authoring happens later in `run_candidate` than this decision does,
    /// so no shipped or tested variant may condition `verify` on
    /// `witness-authored` yet — closing that gap is
    /// [#3915](https://github.com/macanderson/stella/issues/3915).
    ///
    /// # Panics
    /// `.expect()`s that `execute`/`witness`/`verify` resolve without a
    /// [`ScheduleError`], and a debug-only `debug_assert_eq!` that this
    /// candidate's `witness` re-decision agrees with `witness_scheduled`.
    /// Both are meant to be unreachable: `begin_turn_schedule` already
    /// accepted this variant (`execute` is unconditional in every accepted
    /// manifest, and `witness`'s condition — refused otherwise — can only
    /// read facts identical at both decision points), so a panic here is a
    /// genuine defect in this pairing, not a reachable runtime condition.
    pub(super) fn decide_candidate_verify(
        &self,
        mut schedule: Schedule<'_>,
        witness_scheduled: bool,
        mutating_actions: u32,
        diff_lines: u32,
    ) -> bool {
        schedule
            .decide(StageName::Execute)
            .expect("execute is unconditional in every variant begin_turn_schedule accepts");
        let witness_ran = schedule
            .decide(StageName::Witness)
            .expect("accepted by refuse_witness_reading_post_triage (#3408)");
        debug_assert_eq!(
            witness_ran, witness_scheduled,
            "a candidate's schedule disagreed with begin_turn_schedule's witness decision — \
             both read only triage-boundary facts, so this should be impossible (#3408)"
        );
        schedule.update(|v| {
            v.mutating_actions = u64::from(mutating_actions);
            v.diff_lines = u64::from(diff_lines);
            v.witness_authored = false;
        });
        schedule
            .decide(StageName::Verify)
            .expect("accepted by refuse_witness_reading_post_triage (#3408)")
    }
}

/// Wrap a schedule failure as the hard pipeline error it is: unreachable for
/// the shipped `classic` variant (`tests/variant_program.rs`,
/// `tests/variant_dispatch.rs`), and for any configured variant a defect a
/// user can act on rather than a silently wrong stage order.
pub(super) fn variant_error(
    source: impl std::fmt::Display,
    total_cost_usd: f64,
) -> PipelineRunError {
    PipelineRunError::new(
        PipelineError::InvalidVariant(source.to_string()),
        total_cost_usd,
    )
}
