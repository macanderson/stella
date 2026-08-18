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
//! # Why every declared stage is decided in one batch
//!
//! [`stella_plugin::ProgressiveResolver`] must be asked about each declared
//! stage in the manifest's own order — `triage`, `recall`, `research`,
//! `plan`, `scope`, `execute`, `witness`, `verify` for the shipped `classic`
//! variant. `Pipeline::run`'s *own* control flow does not visit those
//! decisions in that order: it needs the `witness` answer (`authored_witness`)
//! before it has run `research` or `plan` for real, and it needs `verify`'s
//! answer before a single candidate has executed. [`Pipeline::begin_turn_schedule`]
//! resolves the resolver's whole declared walk in one batch, immediately once
//! triage's real facts are known — decoupling *when the schedule decides* a
//! stage from *when the pipeline does that stage's real work*, which stays
//! exactly where it always was. This is sound because **every one of
//! `classic.toml`'s conditions reads only a host fact or a signal `triage`
//! itself publishes** — `verify`'s `if = "verifies"` included, since
//! [`Signal::Verifies`](stella_plugin::Signal::Verifies) is triage-published,
//! not execute-published — asserted for the shipped variant by
//! `tests/variant_dispatch.rs` rather than assumed.
//!
//! A variant whose `verify` (or any stage) instead reads what `execute`
//! actually produced — [`crate::schedule`]'s module docs use exactly that
//! example — needs a **second**, per-candidate decision against a
//! [`Schedule::clone`] taken after this batch, fed that candidate's real
//! diff. Nothing here does that yet: no shipped or configurable variant
//! reads a post-execute signal, so wiring it is deferred rather than
//! speculative — tracked in #3674. [`crate::schedule::Schedule`] and
//! `tests/variant_dispatch.rs` already prove the mechanism handles it
//! correctly; what remains is `Pipeline::run_candidate` actually asking.

use stella_plugin::{StageName, Wrapper};

use crate::schedule::{HostFacts, Schedule, ScheduleError};
use crate::variant::{self, VariantError};

use super::*;

/// The manifest's answer to every stage decision `Pipeline::run` needs before
/// any candidate executes — every declared stage the shipped `classic`
/// variant has, since none of its conditions read what `execute` produces
/// (see this module's docs).
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
    /// Whether `verify` runs — ORed at its call site with the host-internal
    /// zero-diff guard for a simple lookup that unexpectedly touched files,
    /// which no published signal names (#3408).
    pub(super) verify: bool,
    /// Whether this turn authors its own witness test (L-E11 front half):
    /// `witness`'s manifest decision (`classic.toml`'s `if = "no-test-
    /// command"`) ANDed with five host-internal facts the closed condition
    /// grammar has no conjunction to fold into one clause (#3408): not
    /// conversational, the witness-author role enabled, the model's own
    /// `wants_witness`, the class verifying unconditionally, and an author
    /// independent of the worker actually being resolvable. `witness`'s own
    /// bare decision is not kept beside it — nothing outside this function
    /// reads it unadorned by those five conjuncts.
    pub(super) authored_witness: bool,
}

impl Pipeline<'_> {
    /// The manifest this run follows: the configured variant, or the
    /// built-in `classic` order when none was configured.
    ///
    /// Build-time defect only: unreachable for a configured variant (already
    /// a parsed, validated [`Wrapper`] by the time it reaches
    /// [`PipelineConfig`]) and asserted unreachable for the built-in
    /// fallback by `tests/variant_program.rs`.
    fn effective_variant(&self) -> Result<Wrapper, VariantError> {
        match &self.config.variant {
            Some(wrapper) => Ok(wrapper.clone()),
            None => variant::classic().cloned(),
        }
    }

    /// Resolve every stage decision `run` needs, against this turn's
    /// effective variant and triage's real facts — see this module's docs
    /// for why one batch, right here, answers every declared stage rather
    /// than only some of them.
    pub(super) fn begin_turn_schedule(
        &self,
        budget: &BudgetGuard,
        assessment: &TaskAssessment,
        research_questions: usize,
        total_cost_usd: f64,
    ) -> Result<PreExecuteSchedule, PipelineRunError> {
        let variant = self
            .effective_variant()
            .map_err(|source| variant_error(source, total_cost_usd))?;
        let task_class = assessment.class;
        let decided: Result<PreExecuteSchedule, ScheduleError> = (|| {
            let mut schedule = Schedule::new(
                &variant,
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
            // Unconditional in every shipped manifest — decided here, before
            // this turn's real execution, purely so `witness` and `verify`
            // (declared after it) can be reached at all.
            schedule.decide(StageName::Execute)?;
            let witness = schedule.decide(StageName::Witness)?;
            let verify = schedule.decide(StageName::Verify)?;
            let authored_witness = !assessment.conversational
                && witness
                && self.responsibility_enabled(ModelCallRole::WitnessAuthor)
                && assessment.wants_witness()
                && task_class.verifies_unconditionally()
                && self.can_author_independent_witness();
            Ok(PreExecuteSchedule {
                research,
                plan: plan && scope,
                verify,
                authored_witness,
            })
        })();
        decided.map_err(|source| variant_error(source, total_cost_usd))
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
