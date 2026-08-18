// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The seam between [`crate::schedule`] — which knows how to walk a
//! manifest's declared stage order — and [`Pipeline::run`], which knows the
//! facts a turn actually has and when each one becomes available.
//!
//! Split from `pipeline.rs` for the reason every sibling in this directory
//! is: that file is a grandfathered god file closed to growth (AGENTS.md §
//! God files), so the machinery — and the error-handling boilerplate around
//! it — lives here, and `run` is left with two plain calls (#3408).
//!
//! # Why the pre-execute batch exists
//!
//! [`stella_plugin::ProgressiveResolver`] must be asked about each declared
//! stage in the manifest's own order — `triage`, `recall`, `research`,
//! `plan`, `scope`, `execute`, `witness`, `verify` for the shipped `classic`
//! variant. `Pipeline::run`'s *own* control flow does not visit those
//! decisions in that order: it needs the `witness` answer (`authored_witness`)
//! before it has run `research` or `plan` for real, because that answer
//! drives the single-shot/best-of-N and isolation choices research and plan
//! sit in front of. [`Pipeline::begin_turn_schedule`] resolves the resolver's
//! whole walk through `witness` in one batch, immediately once triage's real
//! facts are known — decoupling *when the schedule decides* a stage from
//! *when the pipeline does that stage's real work*, which stays exactly
//! where it always was. Nothing about the manifest's conditions changes this
//! — `research`/`plan`/`scope`/`witness` read only host facts or signals
//! `triage` publishes, so deciding them before `research_stage` or
//! `plan_with_review` ever runs answers them no differently than deciding
//! them in place would (asserted for the shipped variant by
//! `tests/variant_dispatch.rs`).
//!
//! `verify` is the one exception, and deliberately not part of the batch: its
//! condition may read `execute`'s real output, which does not exist until
//! this turn's candidate(s) have actually run — see [`crate::schedule`]'s
//! module docs for why that makes [`decide_verify`] a **per-candidate**
//! decision against a clone rather than a fourth batched one.

use stella_plugin::{StageName, Wrapper};

use crate::schedule::{HostFacts, Schedule, ScheduleError};
use crate::variant;

use super::*;

/// The manifest's answer to every stage decision `Pipeline::run` needs before
/// a single candidate executes.
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
    /// Whether `witness` runs — one of six conjuncts `authored_witness`
    /// takes; the other five (conversational, the witness-author role, the
    /// model's own `wants_witness`, the class's `verifies_unconditionally`,
    /// and independence) are host-internal and stay ANDed at that call site.
    pub(super) witness: bool,
}

impl Pipeline<'_> {
    /// The manifest this run follows: the configured variant, or the
    /// built-in `classic` order when none was configured.
    ///
    /// # Errors
    ///
    /// A [`PipelineError::InvalidVariant`] if the *built-in* fallback
    /// manifest fails to load — a build-time defect in the shipped
    /// `variants/classic.toml`, not a runtime condition, and asserted
    /// unreachable by `tests/variant_program.rs`. A configured variant is
    /// already a parsed, validated [`Wrapper`] by the time it reaches
    /// [`PipelineConfig`], so it cannot fail here at all.
    pub(super) fn effective_variant(
        &self,
        total_cost_usd: f64,
    ) -> Result<Wrapper, PipelineRunError> {
        match &self.config.variant {
            Some(wrapper) => Ok(wrapper.clone()),
            None => variant::classic()
                .map(Wrapper::clone)
                .map_err(|source| variant_error(source, total_cost_usd)),
        }
    }

    /// Begin this turn's [`Schedule`] against `variant` and resolve every
    /// stage decision `run` needs before any candidate executes — see this
    /// module's docs for why they are batched here rather than decided where
    /// each stage's real work happens.
    pub(super) fn begin_turn_schedule<'v>(
        &self,
        variant: &'v Wrapper,
        budget: &BudgetGuard,
        assessment: &TaskAssessment,
        research_questions: usize,
        total_cost_usd: f64,
    ) -> Result<(Schedule<'v>, PreExecuteSchedule), PipelineRunError> {
        let task_class = assessment.class;
        let mut schedule = Schedule::new(
            variant,
            HostFacts {
                test_command: self.config.test_command.is_some(),
                candidates: self.config.candidate_count(),
                budget_metered: budget.headroom_usd().is_some(),
            },
        );
        let decided: Result<PreExecuteSchedule, ScheduleError> = (|| {
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
            // Unconditional in every shipped manifest. Deciding it here,
            // before the real execution this turn will do, is what lets
            // `witness` — declared right after it — be decided at all: the
            // resolver walks the manifest in order, and `witness`'s own
            // condition never reads execute's output for the shipped variant
            // (asserted, not assumed, by `tests/variant_dispatch.rs`).
            schedule.decide(StageName::Execute)?;
            let witness = schedule.decide(StageName::Witness)?;
            Ok(PreExecuteSchedule {
                research,
                plan: plan && scope,
                witness,
            })
        })();
        match decided {
            Ok(pre) => Ok((schedule, pre)),
            Err(source) => Err(variant_error(source, total_cost_usd)),
        }
    }
}

/// This candidate's `verify` decision (#3408): update the running schedule
/// with what `execute` actually produced for THIS candidate, then decide.
/// Kept a free function, not a `Pipeline` method — it touches no pipeline
/// state, only the candidate's own [`Schedule`] clone and its [`CandidateState`].
pub(super) fn decide_verify(
    schedule: &mut Schedule<'_>,
    state: &CandidateState,
) -> Result<bool, ScheduleError> {
    schedule.update(|v| {
        v.mutating_actions = u64::from(state.signals.mutating_actions);
        v.diff_lines = u64::from(state.diff_lines);
    });
    schedule.decide(StageName::Verify)
}

impl ScheduleError {
    /// Turn a per-candidate schedule failure into the aborted result its
    /// call site returns, so `pipeline.rs` — closed to growth — spends one
    /// line on this rather than a whole match arm.
    pub(super) fn into_candidate_abort(self, messages: Vec<CompletionMessage>) -> CandidateResult {
        CandidateResult::aborted(
            messages,
            format!("this turn's wrapper variant could not schedule verify: {self}"),
            AbortKind::Failure,
        )
    }
}

/// Wrap a schedule failure as the hard pipeline error it is: unreachable for
/// the shipped `classic` variant (`tests/variant_program.rs`,
/// `tests/variant_dispatch.rs`), and for any configured variant a defect a
/// user can act on rather than a silently wrong stage order.
fn variant_error(source: impl std::fmt::Display, total_cost_usd: f64) -> PipelineRunError {
    PipelineRunError::new(
        PipelineError::InvalidVariant(source.to_string()),
        total_cost_usd,
    )
}
