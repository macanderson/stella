// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Driving the pipeline from a `[wrapper]` manifest, one stage boundary at a
//! time (#3408).
//!
//! [`crate::variant`] reads the manifest; this module is what
//! [`crate::pipeline::Pipeline::run`] actually *consults* at each of its own
//! stage-decision points, so the shipped `classic` order — and any variant
//! that ships beside it — is a declaration the run follows rather than a
//! description that merely happens to match a hardcoded branch.
//!
//! # What a [`Schedule`] is
//!
//! A thin, stateful wrapper over [`stella_plugin::ProgressiveResolver`]: it
//! holds the running [`SignalValues`] the host has learned so far and the
//! resolver's own position in the manifest's declared stage order, and
//! answers exactly one question, [`Schedule::decide`] — *does this stage run*
//! — against whichever [`SignalValues`] the host has handed it by the time
//! it asks. [`Schedule::update`] is how the host hands in a fact it has just
//! learned (a real triage classification, a real diff line count) without
//! deciding a stage.
//!
//! # The manifest decides the grammar-expressible half only
//!
//! Three of the pipeline's gates are conjunctions the closed condition
//! grammar cannot name in one clause (`doc:turn-loop-wrappers` §9.4: no
//! conjunction, on purpose): the conversational fast path is an early
//! *return*, not a stage a program can skip; `witness` also needs the
//! witness-author role enabled, [`crate::triage::TaskAssessment::wants_witness`],
//! and an independent author to be resolvable; `verify` also honours the
//! zero-diff guard for a simple lookup that unexpectedly touched files. Every
//! call site below still ANDs those host-internal facts onto the schedule's
//! answer, each commented with why it stays in Rust rather than in the
//! manifest — [`Schedule::decide`] answers only the part `classic.toml`
//! actually declares.
//!
//! # Why cloning matters for best-of-N
//!
//! `execute`, `witness` and `verify` are declared once in the manifest, but a
//! best-of-N turn runs several *candidates* through that same tail, each with
//! its own diff and its own witness. One [`Schedule`] cannot answer for all of
//! them — [`stella_plugin::ProgressiveResolver::advance`] consumes the stage
//! it decides, so a second candidate asking it "does `verify` run" would find
//! nothing left to decide. The pipeline therefore resolves the *shared*
//! prefix once (`triage` through `witness` — every one of which classic
//! decides from facts every candidate shares) and hands each candidate its
//! own [`Schedule::clone`] to carry forward: same position in the manifest,
//! same facts up to that point, independent from there. See
//! [`crate::pipeline::schedule_wiring`] for where the pipeline draws that
//! line.

use stella_plugin::{
    ManifestError, ProgressiveResolver, SignalValues, StageDecision, StageName, Wrapper,
};

/// The host facts known before triage runs — the seed every [`Schedule`]
/// starts from. Every triage-and-later signal starts at its *nothing-yet*
/// value and is filled in by [`Schedule::update`] once the fact exists.
#[derive(Debug, Clone, Copy)]
pub struct HostFacts {
    /// Whether the operator configured `--test-command` (`Signal::TestCommand`).
    pub test_command: bool,
    /// The candidate count this turn will run (`Signal::Candidates`).
    pub candidates: u32,
    /// Whether spend is gated rather than merely recorded
    /// (`Signal::BudgetMetered`).
    pub budget_metered: bool,
}

/// Why a [`Schedule`] could not decide a stage.
#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    /// This host asked the schedule to decide `expected` next, but the
    /// manifest's declared order has something else (or nothing) there.
    /// Unreachable for the shipped `classic` variant, whose declared order
    /// matches the pipeline's own call order exactly — this exists for a
    /// hand-authored variant that reorders stages relative to how the
    /// pipeline's fixed Rust control flow visits them.
    #[error(
        "the pipeline asked its schedule to decide {expected}, but the manifest's declared \
         order has {found} there"
    )]
    OutOfOrder {
        /// The stage the pipeline's control flow is at.
        expected: StageName,
        /// What the manifest declares next, rendered for the message ("no
        /// stage left" when the manifest is exhausted).
        found: FoundStage,
    },
    /// The stage's condition could not be resolved against the values handed
    /// in — the same failure [`stella_plugin::Wrapper::resolve`] would report,
    /// reached one boundary early instead of from a whole-program walk.
    #[error(transparent)]
    Resolve(#[from] ManifestError),
}

/// What [`ScheduleError::OutOfOrder`] found instead of the expected stage —
/// its own tiny type so the error message never has to guess between "wrong
/// stage" and "no stage at all".
#[derive(Debug)]
pub enum FoundStage {
    /// A different stage was next.
    Stage(StageName),
    /// The manifest has no more declared stages.
    None,
}

impl std::fmt::Display for FoundStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stage(name) => write!(f, "{name}"),
            Self::None => write!(f, "no stage left"),
        }
    }
}

/// Resolves one manifest's stage order against this turn's facts, a boundary
/// at a time. See the module docs for the shape and for why it is [`Clone`].
#[derive(Debug, Clone)]
pub struct Schedule<'a> {
    wrapper: &'a Wrapper,
    resolver: ProgressiveResolver<'a>,
    values: SignalValues,
}

impl<'a> Schedule<'a> {
    /// Begin scheduling `wrapper` from its first declared stage, seeded with
    /// what the host knows before triage runs. Every later signal starts at
    /// its nothing-yet value (`false`/`0`); [`Self::update`] fills each in
    /// once the fact it names actually exists.
    #[must_use]
    pub fn new(wrapper: &'a Wrapper, host: HostFacts) -> Self {
        Self {
            wrapper,
            resolver: ProgressiveResolver::new(wrapper),
            values: SignalValues {
                test_command: host.test_command,
                candidates: u64::from(host.candidates),
                budget_metered: host.budget_metered,
                conversational: false,
                questions: 0,
                plans: false,
                verifies: false,
                wants_witness: false,
                wants_verifier: false,
                mutating_actions: 0,
                diff_lines: 0,
                witness_authored: false,
                flip_achieved: false,
                tests_red: false,
                tests_green: false,
            },
        }
    }

    /// The variant this schedule is resolving — what the store's
    /// `pipeline_variant` column records for this run (#3388).
    #[must_use]
    pub fn variant_id(&self) -> &str {
        &self.wrapper.id
    }

    /// Record a fact the host has just learned (a real triage classification,
    /// a candidate's real diff) without deciding a stage.
    pub fn update(&mut self, mutate: impl FnOnce(&mut SignalValues)) {
        mutate(&mut self.values);
    }

    /// Does `name` run, for this turn, under this manifest?
    ///
    /// Must be called in the manifest's own declared order — the same
    /// discipline [`stella_plugin::ProgressiveResolver::advance`] already
    /// enforces for a *declared* stage. A `name` the manifest never declares
    /// at all answers `false`, the same answer an always-false condition
    /// would give: a variant is entitled to omit a stage outright, and a host
    /// that always asks about every [`StageName`] in its own fixed order (as
    /// [`crate::pipeline::Pipeline::run`] does) must not treat "this variant
    /// never mentions `research`" as a schedule failure.
    ///
    /// # Errors
    ///
    /// [`ScheduleError::OutOfOrder`] if `name` **is** declared somewhere in
    /// the manifest but not at the resolver's current position — a variant
    /// that reorders stages relative to how this host visits them, which the
    /// pipeline's fixed control flow cannot follow. [`ScheduleError::Resolve`]
    /// for the same failures a one-shot [`stella_plugin::Wrapper::resolve`]
    /// would report. Both are unreachable for the shipped `classic` manifest,
    /// asserted by `tests/variant_dispatch.rs` rather than assumed.
    pub fn decide(&mut self, name: StageName) -> Result<bool, ScheduleError> {
        match self.resolver.peek() {
            Some(next) if next == name => match self.resolver.advance(&self.values)? {
                StageDecision::Ran(_) => Ok(true),
                StageDecision::Skipped(_) => Ok(false),
                // `peek` just confirmed a pending stage, so `advance` cannot
                // have found the walk already `Done` — not runtime data, an
                // invariant of the two calls immediately above.
                StageDecision::Done => unreachable!("peek confirmed a pending stage"),
            },
            found if self.declares(name) => Err(ScheduleError::OutOfOrder {
                expected: name,
                found: found.map_or(FoundStage::None, FoundStage::Stage),
            }),
            _ => Ok(false),
        }
    }

    /// Whether `name` is declared anywhere in this manifest, regardless of
    /// whether the resolver has already walked past it.
    fn declares(&self, name: StageName) -> bool {
        self.wrapper.stages.iter().any(|stage| stage.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_plugin::{PluginManifest, WrapperStage};

    fn wrapper(stages: Vec<WrapperStage>) -> Wrapper {
        Wrapper {
            id: "probe".into(),
            stages,
        }
    }

    fn stage(name: StageName, condition: Option<&str>) -> WrapperStage {
        WrapperStage {
            name,
            condition: condition.map(str::to_string),
        }
    }

    fn host() -> HostFacts {
        HostFacts {
            test_command: false,
            candidates: 1,
            budget_metered: false,
        }
    }

    #[test]
    fn an_unconditional_stage_always_runs() {
        let w = wrapper(vec![stage(StageName::Triage, None)]);
        let mut schedule = Schedule::new(&w, host());
        assert!(schedule.decide(StageName::Triage).unwrap());
    }

    #[test]
    fn a_stage_this_variant_never_declares_answers_false_not_an_error() {
        let w = wrapper(vec![stage(StageName::Triage, None)]);
        let mut schedule = Schedule::new(&w, host());
        assert!(schedule.decide(StageName::Triage).unwrap());
        // `research` is not in this manifest at all — a variant is entitled
        // to omit a stage outright, same as an always-false condition.
        assert!(!schedule.decide(StageName::Research).unwrap());
    }

    #[test]
    fn asking_out_of_the_manifests_declared_order_is_a_named_error() {
        let w = wrapper(vec![
            stage(StageName::Triage, None),
            stage(StageName::Execute, None),
            stage(StageName::Research, Some("questions > 0")),
        ]);
        let mut schedule = Schedule::new(&w, host());
        assert!(schedule.decide(StageName::Triage).unwrap());
        // The manifest declares `execute` next, not `research` — even though
        // `research` DOES appear later in this manifest.
        let err = schedule
            .decide(StageName::Research)
            .expect_err("execute is next, not research");
        assert!(
            matches!(
                err,
                ScheduleError::OutOfOrder {
                    expected: StageName::Research,
                    found: FoundStage::Stage(StageName::Execute),
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn a_later_signal_reads_the_value_update_just_recorded() {
        let text = "name = \"probe\"\n\
             [loop]\n\
             participation = \"steering\"\n\
             [wrapper]\n\
             id = \"probe-v1\"\n\
             [[wrapper.stages]]\n\
             name = \"execute\"\n\
             [[wrapper.stages]]\n\
             name = \"verify\"\n\
             if = \"diff-lines > 0\"\n";
        let w = PluginManifest::from_toml_str(text)
            .expect("fixture must load")
            .wrapper
            .expect("[wrapper] declared");
        let mut schedule = Schedule::new(&w, host());
        assert!(schedule.decide(StageName::Execute).unwrap());
        schedule.update(|v| v.diff_lines = 12);
        assert!(schedule.decide(StageName::Verify).unwrap());

        let mut empty_run = Schedule::new(&w, host());
        assert!(empty_run.decide(StageName::Execute).unwrap());
        empty_run.update(|v| v.diff_lines = 0);
        assert!(!empty_run.decide(StageName::Verify).unwrap());
    }

    /// A per-candidate divergence after a shared prefix: two clones of one
    /// schedule, taken right after `execute` is decided, answer `verify`
    /// independently once each is updated with its OWN candidate's diff —
    /// the shape best-of-N needs (module docs, "Why cloning matters").
    #[test]
    fn cloned_schedules_diverge_independently_after_the_shared_prefix() {
        let text = "name = \"probe\"\n\
             [loop]\n\
             participation = \"steering\"\n\
             [wrapper]\n\
             id = \"probe-v1\"\n\
             [[wrapper.stages]]\n\
             name = \"triage\"\n\
             [[wrapper.stages]]\n\
             name = \"execute\"\n\
             [[wrapper.stages]]\n\
             name = \"verify\"\n\
             if = \"diff-lines > 0\"\n";
        let w = PluginManifest::from_toml_str(text)
            .expect("fixture must load")
            .wrapper
            .expect("[wrapper] declared");
        let mut prefix = Schedule::new(&w, host());
        assert!(prefix.decide(StageName::Triage).unwrap());
        assert!(prefix.decide(StageName::Execute).unwrap());

        let mut winner = prefix.clone();
        winner.update(|v| v.diff_lines = 40);
        let mut loser = prefix.clone();
        loser.update(|v| v.diff_lines = 0);

        assert!(winner.decide(StageName::Verify).unwrap());
        assert!(!loser.decide(StageName::Verify).unwrap());
    }
}
