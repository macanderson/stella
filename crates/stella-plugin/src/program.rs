//! Resolving a `[wrapper]` manifest into the stage order one turn will run.
//!
//! [`crate::wrapper`] declares the order and load-checks it; this module is
//! the reader that turns a declaration into an answer. Given a [`Wrapper`] and
//! the host's published signal values, [`Wrapper::resolve`] produces the
//! ordered [`StageProgram`] — the stages that run, in the order they run.
//!
//! Three properties, each of which is why this lives here rather than in the
//! host:
//!
//! - **Pure and synchronous, over owned data** (invariant 2). Nothing here
//!   reads a file, a clock, or an environment variable: the *host* gathers the
//!   facts and hands them in as [`SignalValues`]. That is what makes the
//!   resolution property-testable and identical between a real run, a replay,
//!   and a test.
//! - **The published set is total, by the compiler.** [`SignalValues`] carries
//!   one field per [`Signal`] and derives no `Default`, so a host cannot leave
//!   a signal unfilled and it is not *representable* for the evaluator to read
//!   an unpublished signal as `false`. Adding a `Signal` variant is an `E0004`
//!   here — the same "declare the consumer or fail the build" discipline
//!   `stella-protocol`'s event-consumer ledger applies to emitted signals,
//!   pointed at published ones.
//! - **Only stages that actually ran publish.** A skipped stage produces
//!   nothing, which is the one thing the load-time graph check could not see
//!   on its own — see [`ManifestError::PublisherMayBeSkipped`], the load rule
//!   this module's existence made necessary.
//!
//! # Scope
//!
//! Resolving is not dispatching. A [`StageProgram`] is an ordered list of
//! names a host may consult, not a schedule this crate executes — the staged
//! pipeline's `variant` module was the first consumer
//! (`crates/stella-pipeline`, deleted in #3865), and `stella-runtime`'s
//! `WrapperDispatch` (#3380) is the one that survives it.
//! `doc:turn-loop-wrappers` §5 and §9.4.

use crate::error::ManifestError;
use crate::wrapper::{Condition, Signal, SignalKind, StageName, Wrapper};

/// Every published signal's value for one turn — the host's half of the
/// contract.
///
/// One field per [`Signal`], deliberately: the manifest's rule is that a
/// condition naming a signal the host does not publish is a **load error**, and
/// the evaluator must not be able to weaken that into "an unpublished signal
/// reads false". A struct whose fields are the published set makes the weaker
/// reading unrepresentable — there is no absent case to default.
///
/// It derives no `Default` for the same reason: a host must answer every
/// signal, and adding a signal must break every construction site loudly
/// rather than silently reading `false` at one of them.
///
/// The two accessors below are exhaustive `match`es over [`Signal`], so a new
/// variant is a compile error in this file — the totality the [`Signal`] set
/// needs to stay honest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalValues {
    /// [`Signal::TestCommand`] — a `--test-command` is configured for this run.
    pub test_command: bool,
    /// [`Signal::Candidates`] — how many candidates this run executes.
    pub candidates: u64,
    /// [`Signal::BudgetMetered`] — spend is gated, not merely recorded.
    pub budget_metered: bool,
    /// [`Signal::Conversational`] — triage called this chat, not a task.
    pub conversational: bool,
    /// [`Signal::Questions`] — how many research questions triage named.
    pub questions: u64,
    /// [`Signal::Plans`] — this task class plans.
    pub plans: bool,
    /// [`Signal::Verifies`] — this task class verifies unconditionally.
    pub verifies: bool,
    /// [`Signal::WantsWitness`] — this turn warrants an authored witness.
    pub wants_witness: bool,
    /// [`Signal::WantsVerifier`] — an inconclusive ladder warrants a verifier.
    pub wants_verifier: bool,
    /// [`Signal::MutatingActions`] — calls dispatched that could change the
    /// tree.
    pub mutating_actions: u64,
    /// [`Signal::DiffLines`] — lines of diff the turn produced.
    pub diff_lines: u64,
    /// [`Signal::WitnessAuthored`] — a witness test was authored and accepted.
    pub witness_authored: bool,
    /// [`Signal::FlipAchieved`] — the tracked command went fail→pass and held.
    pub flip_achieved: bool,
    /// [`Signal::TestsRed`] — a touched test ran and failed.
    ///
    /// With [`Self::tests_green`], the host's projection of the ladder's
    /// `Option<bool>`: both `false` is "no test ran", which is why one
    /// `tests_passed` field would be a lie rather than a simplification.
    pub tests_red: bool,
    /// [`Signal::TestsGreen`] — a touched test ran and passed.
    pub tests_green: bool,
}

impl SignalValues {
    /// This signal's value, when it is a boolean one; `None` when the signal
    /// is a count and the caller asked for the wrong type.
    ///
    /// Not a coercion in either direction: the type mismatch is a named error
    /// at the call site, matching how the same mismatch is reported at load
    /// ([`ManifestError::ConditionTypeMismatch`]).
    #[must_use]
    pub fn boolean(&self, signal: Signal) -> Option<bool> {
        match signal {
            Signal::TestCommand => Some(self.test_command),
            Signal::BudgetMetered => Some(self.budget_metered),
            Signal::Conversational => Some(self.conversational),
            Signal::Plans => Some(self.plans),
            Signal::Verifies => Some(self.verifies),
            Signal::WantsWitness => Some(self.wants_witness),
            Signal::WantsVerifier => Some(self.wants_verifier),
            Signal::WitnessAuthored => Some(self.witness_authored),
            Signal::FlipAchieved => Some(self.flip_achieved),
            Signal::TestsRed => Some(self.tests_red),
            Signal::TestsGreen => Some(self.tests_green),
            Signal::Questions
            | Signal::Candidates
            | Signal::MutatingActions
            | Signal::DiffLines => None,
        }
    }

    /// This signal's value, when it is a count; `None` when it is a boolean.
    #[must_use]
    pub fn count(&self, signal: Signal) -> Option<u64> {
        match signal {
            Signal::Questions => Some(self.questions),
            Signal::Candidates => Some(self.candidates),
            Signal::MutatingActions => Some(self.mutating_actions),
            Signal::DiffLines => Some(self.diff_lines),
            Signal::TestCommand
            | Signal::BudgetMetered
            | Signal::Conversational
            | Signal::Plans
            | Signal::Verifies
            | Signal::WantsWitness
            | Signal::WantsVerifier
            | Signal::WitnessAuthored
            | Signal::FlipAchieved
            | Signal::TestsRed
            | Signal::TestsGreen => None,
        }
    }
}

impl Condition {
    /// Evaluate this condition against the host's published values.
    ///
    /// The whole evaluator for the closed grammar: a boolean read (optionally
    /// negated) or [`CompareOp::apply`]. `stage` is carried only so a rejection
    /// can name where the condition was written.
    ///
    /// # Errors
    ///
    /// [`ManifestError::ConditionTypeMismatch`] when the condition reads a
    /// signal at the wrong type. Validation already rejects that at load, so
    /// this can only fire for a [`Condition`] built by hand rather than parsed
    /// — the same contract [`WrapperStage::condition`] documents.
    ///
    /// [`CompareOp::apply`]: crate::CompareOp::apply
    /// [`WrapperStage::condition`]: crate::WrapperStage::condition
    pub fn evaluate(self, stage: &StageName, values: &SignalValues) -> Result<bool, ManifestError> {
        let mismatch = |declared: SignalKind| ManifestError::ConditionTypeMismatch {
            stage: stage.clone(),
            signal: self.signal(),
            declared,
            actual: self.signal().kind(),
        };
        match self {
            Self::Boolean { signal, negated } => {
                let value = values
                    .boolean(signal)
                    .ok_or(mismatch(SignalKind::Boolean))?;
                Ok(value != negated)
            }
            Self::Compare { signal, op, value } => {
                let published = values.count(signal).ok_or(mismatch(SignalKind::Count))?;
                Ok(op.apply(published, value))
            }
        }
    }
}

/// The stages one turn will run, in order, under one variant id.
///
/// The resolved form of a [`Wrapper`]: conditions are gone, because they have
/// been answered. A host consults this instead of re-deciding a stage's
/// condition at each stage boundary, so the whole shape of the turn is
/// decidable — and loggable — before the first stage runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageProgram {
    variant: String,
    stages: Vec<StageName>,
}

impl StageProgram {
    /// Assemble a program directly from its resolved parts.
    ///
    /// `pub(crate)` because this bypasses [`Wrapper::resolve`]'s own walk:
    /// [`crate::progressive::ProgressiveResolver`] is today's one caller,
    /// building the same shape one boundary at a time (#3491) rather than
    /// from a single [`SignalValues`] snapshot.
    pub(crate) fn from_parts(variant: String, stages: Vec<StageName>) -> Self {
        Self { variant, stages }
    }

    /// The variant id this program resolved from — what the store's
    /// `pipeline_variant` column records (#3388), and the join key of every
    /// per-variant comparison.
    #[must_use]
    pub fn variant(&self) -> &str {
        &self.variant
    }

    /// The stages that run, in execution order.
    #[must_use]
    pub fn stages(&self) -> &[StageName] {
        &self.stages
    }

    /// Whether this turn runs `stage`.
    #[must_use]
    pub fn runs(&self, stage: &StageName) -> bool {
        self.stages.contains(stage)
    }
}

impl Wrapper {
    /// Resolve this wrapper's stage list against the host's published values.
    ///
    /// Walks the declared order once, evaluating each stage's condition and
    /// accumulating what the stages that **ran** have published. A skipped
    /// stage publishes nothing — which is the whole reason
    /// [`ManifestError::PublisherMayBeSkipped`] is a load error: without it,
    /// a manifest could load and then resolve into a read of a fact that was
    /// never produced.
    ///
    /// # Errors
    ///
    /// Only for a [`Wrapper`] that did not come from
    /// [`PluginManifest::from_toml_str`]: an unparsable condition, a condition
    /// reading a signal at the wrong type, or
    /// [`ManifestError::SignalNotProduced`] for a read of a skipped stage's
    /// output. Validation rejects all three at load, so a validated manifest
    /// resolves for every possible [`SignalValues`] — which
    /// `tests/wrapper_program.rs` asserts as a property rather than a promise.
    ///
    /// [`PluginManifest::from_toml_str`]: crate::PluginManifest::from_toml_str
    pub fn resolve(&self, values: &SignalValues) -> Result<StageProgram, ManifestError> {
        let mut produced: Vec<Signal> = Vec::new();
        let mut stages: Vec<StageName> = Vec::with_capacity(self.stages.len());

        for stage in &self.stages {
            let runs = match stage.condition()? {
                None => true,
                Some(condition) => {
                    let signal = condition.signal();
                    // A stage-published signal whose publisher did not run has
                    // no value at all, and reading it as `false` would be the
                    // silent answer this crate exists to refuse.
                    if let Some(publisher) = signal.publisher()
                        && !produced.contains(&signal)
                    {
                        return Err(ManifestError::SignalNotProduced {
                            stage: stage.name.clone(),
                            signal,
                            publisher,
                        });
                    }
                    condition.evaluate(&stage.name, values)?
                }
            };
            if runs {
                produced.extend_from_slice(stage.name.publishes());
                stages.push(stage.name.clone());
            }
        }

        Ok(StageProgram {
            variant: self.id.clone(),
            stages,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HostStage, PluginManifest, WrapperStage};

    /// Signal values for a full multi-step turn with no configured test
    /// command — the shape every case below varies one field of.
    fn multi_step() -> SignalValues {
        SignalValues {
            test_command: false,
            candidates: 1,
            budget_metered: true,
            conversational: false,
            questions: 2,
            plans: true,
            verifies: true,
            wants_witness: true,
            wants_verifier: true,
            mutating_actions: 4,
            diff_lines: 37,
            witness_authored: true,
            flip_achieved: true,
            tests_red: false,
            tests_green: true,
        }
    }

    fn wrapper(stages: &str) -> Wrapper {
        let text = format!(
            "name = \"probe\"\n\
             [loop]\n\
             participation = \"steering\"\n\
             [wrapper]\n\
             id = \"probe-v1\"\n\
             {stages}"
        );
        PluginManifest::from_toml_str(&text)
            .expect("fixture must load")
            .wrapper
            .expect("[wrapper] declared")
    }

    #[test]
    fn an_unconditional_stage_always_runs_and_the_program_keeps_the_variant_id() {
        let program = wrapper(
            "[[wrapper.stages]]\n\
             name = \"execute\"\n",
        )
        .resolve(&multi_step())
        .unwrap();
        assert_eq!(program.variant(), "probe-v1");
        assert_eq!(program.stages(), [StageName::Host(HostStage::Execute)]);
        assert!(program.runs(&StageName::Host(HostStage::Execute)));
        assert!(!program.runs(&StageName::Host(HostStage::Verify)));
    }

    #[test]
    fn a_condition_decides_the_stage_in_both_directions() {
        let w = wrapper(
            "[[wrapper.stages]]\n\
             name = \"triage\"\n\
             [[wrapper.stages]]\n\
             name = \"research\"\n\
             if = \"questions > 0\"\n\
             [[wrapper.stages]]\n\
             name = \"witness\"\n\
             if = \"no-test-command\"\n",
        );

        let asked = w.resolve(&multi_step()).unwrap();
        assert_eq!(
            asked.stages(),
            [
                StageName::Host(HostStage::Triage),
                StageName::Host(HostStage::Research),
                StageName::Host(HostStage::Witness)
            ]
        );

        let none = w
            .resolve(&SignalValues {
                questions: 0,
                test_command: true,
                ..multi_step()
            })
            .unwrap();
        assert_eq!(none.stages(), [StageName::Host(HostStage::Triage)]);
    }

    /// The negation is a property of the condition, not of the value: a `no-`
    /// prefixed read must invert, and only invert.
    #[test]
    fn negation_inverts_exactly_the_boolean_it_reads() {
        for value in [false, true] {
            let values = SignalValues {
                test_command: value,
                ..multi_step()
            };
            let bare = Condition::parse(&StageName::Host(HostStage::Witness), "test-command")
                .unwrap()
                .evaluate(&StageName::Host(HostStage::Witness), &values)
                .unwrap();
            let negated = Condition::parse(&StageName::Host(HostStage::Witness), "no-test-command")
                .unwrap()
                .evaluate(&StageName::Host(HostStage::Witness), &values)
                .unwrap();
            assert_eq!(bare, value);
            assert_eq!(negated, !value);
        }
    }

    /// The evaluator must never read a fact nothing produced. A hand-built
    /// wrapper is the only way to reach this — the load rule rejects the same
    /// manifest with [`ManifestError::PublisherMayBeSkipped`] — but "the
    /// constructor was bypassed" must still not mean "silently false".
    #[test]
    fn a_skipped_publishers_signal_is_an_error_not_a_false() {
        let hand_built = Wrapper {
            id: "hand-built".into(),
            stages: vec![
                WrapperStage {
                    name: StageName::Host(HostStage::Triage),
                    condition: Some("no-test-command".into()),
                },
                WrapperStage {
                    name: StageName::Host(HostStage::Research),
                    condition: Some("questions > 0".into()),
                },
            ],
        };
        let err = hand_built
            .resolve(&SignalValues {
                test_command: true,
                ..multi_step()
            })
            .expect_err("triage was skipped, so `questions` does not exist");
        assert!(
            matches!(
                err,
                ManifestError::SignalNotProduced {
                    stage: StageName::Host(HostStage::Research),
                    signal: Signal::Questions,
                    publisher: HostStage::Triage,
                }
            ),
            "got {err:?}"
        );

        // ...and when the publisher does run, the same wrapper resolves.
        let ok = hand_built
            .resolve(&SignalValues {
                test_command: false,
                ..multi_step()
            })
            .unwrap();
        assert_eq!(
            ok.stages(),
            [
                StageName::Host(HostStage::Triage),
                StageName::Host(HostStage::Research)
            ]
        );
    }

    /// Reading a count as a boolean is rejected at load; a hand-built
    /// condition reaches the evaluator, and must be rejected there too rather
    /// than coerced.
    #[test]
    fn a_type_mismatch_is_an_error_at_evaluation_too() {
        let err = Condition::Boolean {
            signal: Signal::Questions,
            negated: false,
        }
        .evaluate(&StageName::Host(HostStage::Research), &multi_step())
        .expect_err("a count is not a boolean");
        assert!(
            matches!(
                err,
                ManifestError::ConditionTypeMismatch {
                    signal: Signal::Questions,
                    declared: SignalKind::Boolean,
                    actual: SignalKind::Count,
                    ..
                }
            ),
            "got {err:?}"
        );

        let reverse = Condition::Compare {
            signal: Signal::Plans,
            op: crate::CompareOp::Greater,
            value: 0,
        }
        .evaluate(&StageName::Host(HostStage::Plan), &multi_step())
        .expect_err("a boolean is not a count");
        assert!(
            matches!(
                reverse,
                ManifestError::ConditionTypeMismatch {
                    signal: Signal::Plans,
                    declared: SignalKind::Count,
                    ..
                }
            ),
            "got {reverse:?}"
        );
    }

    /// Every published signal must be answerable at exactly one type. This is
    /// the runtime half of the compile-time totality: if a `Signal` variant is
    /// added and given a field but forgotten in one accessor, both answers
    /// come back `None` and this fails.
    #[test]
    fn every_signal_is_readable_at_exactly_one_type() {
        let values = multi_step();
        for signal in Signal::ALL {
            let boolean = values.boolean(*signal).is_some();
            let count = values.count(*signal).is_some();
            assert!(
                boolean ^ count,
                "{signal} must be readable at exactly one type (boolean: \
                 {boolean}, count: {count})"
            );
            assert_eq!(
                boolean,
                signal.kind() == SignalKind::Boolean,
                "{signal}'s value type must match the kind it declares"
            );
        }
    }
}
