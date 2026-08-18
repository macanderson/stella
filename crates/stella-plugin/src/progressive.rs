//! Progressive resolution of a `[wrapper]` stage order (#3491).
//!
//! [`Wrapper::resolve`] answers every declared stage's condition against one
//! [`SignalValues`] snapshot taken before the first stage runs. That is
//! exactly right for a host whose signals are all known up front — every
//! host fact, plus any published signal the host can compute in advance —
//! and it stays the crate's primary reader: nothing about it changes here.
//!
//! It is the wrong tool the moment a later stage's condition reads a signal
//! that an *earlier* stage publishes. `execute` publishes [`Signal::DiffLines`]
//! (the diff a turn actually produced); a `witness` stage gated on
//! `"diff-lines > 0"` cannot be answered honestly by a snapshot taken before
//! `execute` has run — the value does not exist yet. The host either guesses
//! (wrong by construction) or resolves once execute is done, which is what
//! [`ProgressiveResolver`] is for: it walks the same declared order as
//! [`Wrapper::resolve`], one stage at a time, and takes a fresh
//! [`SignalValues`] from the host at *each* boundary instead of one snapshot
//! for the whole turn.
//!
//! # When to use which
//!
//! - [`Wrapper::resolve`] — the host can state every signal a stage might
//!   read before the turn starts (a fixed set of host facts, or a program
//!   with no stage conditioned on another stage's output). One call, one
//!   [`StageProgram`].
//! - [`ProgressiveResolver`] — a later stage's condition reads a signal an
//!   earlier stage publishes, and the host wants to hand in that stage's
//!   *real* output rather than a value computed before it ran. The host
//!   calls [`ProgressiveResolver::advance`] once per declared stage, in
//!   order, supplying the [`SignalValues`] it actually knows at that
//!   boundary — which need not (and for an unpublished later signal, cannot)
//!   be the values [`Wrapper::resolve`] would need for the whole run.
//!
//! [`ProgressiveResolver`] is [`Clone`] because a host that fans one turn out
//! into several independent continuations — best-of-N candidates, each with
//! its own execute/witness/verify — needs one resolver *per* continuation
//! from the point they diverge, not one resolver shared by several writers.
//! Cloning at that boundary and letting each continuation advance its own
//! copy is the host's job (`stella-pipeline`'s scheduler, #3408); the clone
//! itself is cheap (a slice pointer plus two small `Vec`s).
//!
//! Both share every property that makes resolution safe to trust:
//!
//! - **Pure and synchronous, over owned data** (invariant 2). Neither reads
//!   a file, a clock, or an environment variable — the host gathers the
//!   facts, once per boundary here rather than once per turn, and hands them
//!   in.
//! - **Totality by the compiler.** [`SignalValues`] still derives no
//!   `Default`, so a boundary the host has not yet reached still cannot be
//!   read as "false" — the host must construct a real value for every field,
//!   even one it plans to overwrite once the stage that publishes it runs.
//! - **A skipped publisher's signal is [`ManifestError::SignalNotProduced`],
//!   never a silent `false`.** [`ProgressiveResolver`] tracks exactly which
//!   declared stages have run so far, the same running set
//!   [`Wrapper::resolve`] accumulates, and answers a read of an unproduced
//!   signal with the same error — evaluated one boundary early instead of
//!   from a full walk, but never weaker.

use crate::error::ManifestError;
use crate::program::{SignalValues, StageProgram};
use crate::wrapper::{Signal, StageName, Wrapper, WrapperStage};

/// What [`ProgressiveResolver::advance`] decided for the stage it just
/// considered, or that there was no stage left to consider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageDecision {
    /// The stage's condition held (or it declared none); it runs and its
    /// published signals, if any, become readable to later stages.
    Ran(StageName),
    /// The stage's condition did not hold; it does not run and publishes
    /// nothing.
    Skipped(StageName),
    /// Every declared stage has already been decided.
    Done,
}

/// Resolves one [`Wrapper`]'s stage order a boundary at a time.
///
/// See the module docs for when this belongs over [`Wrapper::resolve`]. A
/// resolver is single-use: construct one with [`ProgressiveResolver::new`] at
/// the start of a turn, call [`ProgressiveResolver::advance`] once per
/// declared stage in order, and read [`ProgressiveResolver::program`] for the
/// [`StageProgram`] built so far (complete once [`ProgressiveResolver::peek`]
/// returns `None`).
#[derive(Debug, Clone)]
pub struct ProgressiveResolver<'a> {
    wrapper: &'a Wrapper,
    remaining: &'a [WrapperStage],
    produced: Vec<Signal>,
    decided: Vec<StageName>,
}

impl<'a> ProgressiveResolver<'a> {
    /// Begin resolving `wrapper`'s declared stages from the first one.
    #[must_use]
    pub fn new(wrapper: &'a Wrapper) -> Self {
        Self {
            wrapper,
            remaining: &wrapper.stages,
            produced: Vec::new(),
            decided: Vec::with_capacity(wrapper.stages.len()),
        }
    }

    /// The next stage awaiting a decision, or `None` once every declared
    /// stage has been decided. Lets a host know which boundary it is about
    /// to cross before it gathers the [`SignalValues`] to hand to
    /// [`Self::advance`].
    #[must_use]
    pub fn peek(&self) -> Option<StageName> {
        self.remaining.first().map(|stage| stage.name)
    }

    /// Decide the next declared stage against `values` — the host's answer
    /// as of *this* boundary, not necessarily the values a whole-turn
    /// snapshot would need.
    ///
    /// Advances past the decided stage only on success: an error leaves this
    /// resolver exactly as it was before the call, matching
    /// [`Wrapper::resolve`]'s contract that resolution aborts rather than
    /// producing a partial answer.
    ///
    /// # Errors
    ///
    /// The same three that reject a hand-built [`Wrapper`] validation would
    /// otherwise have caught — an unparsable condition, a condition reading a
    /// signal at the wrong type — plus
    /// [`ManifestError::SignalNotProduced`] when this stage's condition reads
    /// a signal whose publisher was already decided [`StageDecision::Skipped`]
    /// (or has not run yet at all). A wrapper that came from
    /// [`PluginManifest::from_toml_str`] cannot reach any of the three: load
    /// already rejects the shapes that would.
    ///
    /// [`PluginManifest::from_toml_str`]: crate::PluginManifest::from_toml_str
    pub fn advance(&mut self, values: &SignalValues) -> Result<StageDecision, ManifestError> {
        let Some(stage) = self.remaining.first() else {
            return Ok(StageDecision::Done);
        };

        let runs = match stage.condition()? {
            None => true,
            Some(condition) => {
                let signal = condition.signal();
                // Same rule as `Wrapper::resolve`: a stage-published signal
                // whose publisher has not (yet) run has no value, and this
                // resolver must refuse it exactly as loudly as the one-shot
                // reader does rather than answer from a snapshot that
                // happens to hold a leftover `false`.
                if let Some(publisher) = signal.publisher()
                    && !self.produced.contains(&signal)
                {
                    return Err(ManifestError::SignalNotProduced {
                        stage: stage.name,
                        signal,
                        publisher,
                    });
                }
                condition.evaluate(stage.name, values)?
            }
        };

        let name = stage.name;
        self.remaining = &self.remaining[1..];
        if runs {
            self.decided.push(name);
            self.produced.extend_from_slice(name.publishes());
            Ok(StageDecision::Ran(name))
        } else {
            Ok(StageDecision::Skipped(name))
        }
    }

    /// The [`StageProgram`] resolved so far: every stage decided
    /// [`StageDecision::Ran`], in declared order.
    ///
    /// Meaningful before every stage has been decided — a host may dispatch
    /// or log from a program in progress — and, once [`Self::peek`] returns
    /// `None`, the same answer [`Wrapper::resolve`] would have given had it
    /// been handed a single snapshot agreeing with every value this resolver
    /// was fed one boundary at a time.
    #[must_use]
    pub fn program(&self) -> StageProgram {
        StageProgram::from_parts(self.wrapper.id.clone(), self.decided.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PluginManifest, WrapperStage};

    /// Every signal at its emptiest, matching `tests/wrapper_program.rs`'s
    /// `bare()` — spelled out because [`SignalValues`] refuses `Default` on
    /// purpose.
    fn bare() -> SignalValues {
        SignalValues {
            test_command: false,
            candidates: 1,
            budget_metered: false,
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

    /// The witness for #3491: a stage gated on a signal `execute` publishes
    /// resolves to different outcomes depending on the real value handed in
    /// at the execute boundary — not a value guessed before execute ran.
    #[test]
    fn a_later_stage_reads_the_real_value_execute_produced_at_its_own_boundary() {
        let w = wrapper(
            "[[wrapper.stages]]\n\
             name = \"execute\"\n\
             [[wrapper.stages]]\n\
             name = \"witness\"\n\
             if = \"diff-lines > 0\"\n",
        );

        // A run whose execute produced 40 lines of diff.
        let mut resolver = ProgressiveResolver::new(&w);
        assert_eq!(resolver.peek(), Some(StageName::Execute));
        // The value at this boundary is irrelevant to execute's own
        // (unconditional) decision, and diff_lines is not yet known — this
        // is the field the next boundary will answer for real.
        assert_eq!(
            resolver.advance(&bare()).unwrap(),
            StageDecision::Ran(StageName::Execute)
        );
        assert_eq!(resolver.peek(), Some(StageName::Witness));
        let after_execute = SignalValues {
            diff_lines: 40,
            ..bare()
        };
        assert_eq!(
            resolver.advance(&after_execute).unwrap(),
            StageDecision::Ran(StageName::Witness)
        );
        assert_eq!(resolver.peek(), None);
        assert_eq!(
            resolver.advance(&after_execute).unwrap(),
            StageDecision::Done
        );
        assert_eq!(
            resolver.program().stages(),
            [StageName::Execute, StageName::Witness]
        );

        // The same manifest, resolved progressively again, but this time
        // execute produced no diff at all: the outcome flips.
        let mut empty_run = ProgressiveResolver::new(&w);
        assert_eq!(
            empty_run.advance(&bare()).unwrap(),
            StageDecision::Ran(StageName::Execute)
        );
        assert_eq!(
            empty_run.advance(&bare()).unwrap(),
            StageDecision::Skipped(StageName::Witness)
        );
        assert_eq!(empty_run.program().stages(), [StageName::Execute]);
    }

    /// The evaluator must never read a fact nothing produced, evaluated
    /// progressively: the same hazard `program.rs`'s
    /// `a_skipped_publishers_signal_is_an_error_not_a_false` covers for
    /// `Wrapper::resolve`, reached one boundary at a time instead of from a
    /// full walk.
    #[test]
    fn a_skipped_publishers_signal_errors_at_its_own_boundary_too() {
        let hand_built = Wrapper {
            id: "hand-built".into(),
            stages: vec![
                WrapperStage {
                    name: StageName::Triage,
                    condition: Some("no-test-command".into()),
                },
                WrapperStage {
                    name: StageName::Research,
                    condition: Some("questions > 0".into()),
                },
            ],
        };

        let mut resolver = ProgressiveResolver::new(&hand_built);
        let values = SignalValues {
            test_command: true,
            ..bare()
        };
        assert_eq!(
            resolver.advance(&values).unwrap(),
            StageDecision::Skipped(StageName::Triage),
            "test_command is true, so \"no-test-command\" is false and triage does not run"
        );
        let err = resolver
            .advance(&values)
            .expect_err("triage was skipped, so `questions` does not exist");
        assert!(
            matches!(
                err,
                ManifestError::SignalNotProduced {
                    stage: StageName::Research,
                    signal: Signal::Questions,
                    publisher: StageName::Triage,
                }
            ),
            "got {err:?}"
        );

        // ...and when the publisher does run, the same wrapper resolves
        // progressively without error.
        let mut ok = ProgressiveResolver::new(&hand_built);
        assert_eq!(
            ok.advance(&SignalValues {
                test_command: false,
                ..bare()
            })
            .unwrap(),
            StageDecision::Ran(StageName::Triage)
        );
        assert_eq!(
            ok.advance(&SignalValues {
                questions: 2,
                ..bare()
            })
            .unwrap(),
            StageDecision::Ran(StageName::Research)
        );
        assert_eq!(
            ok.program().stages(),
            [StageName::Triage, StageName::Research]
        );
    }

    /// A resolver that agrees, boundary by boundary, with one fixed snapshot
    /// must land on exactly the program `Wrapper::resolve` would have
    /// produced from that snapshot in one call — progressive resolution is a
    /// different walk over the same rule, not a different rule.
    #[test]
    fn agreeing_with_one_snapshot_at_every_boundary_matches_resolve() {
        let w = wrapper(
            "[[wrapper.stages]]\n\
             name = \"triage\"\n\
             [[wrapper.stages]]\n\
             name = \"research\"\n\
             if = \"questions > 0\"\n\
             [[wrapper.stages]]\n\
             name = \"execute\"\n\
             [[wrapper.stages]]\n\
             name = \"verify\"\n\
             if = \"verifies\"\n",
        );
        let values = SignalValues {
            questions: 3,
            verifies: true,
            ..bare()
        };

        let one_shot = w.resolve(&values).unwrap();

        let mut resolver = ProgressiveResolver::new(&w);
        while resolver.peek().is_some() {
            resolver.advance(&values).unwrap();
        }

        assert_eq!(resolver.program().stages(), one_shot.stages());
        assert_eq!(resolver.program().variant(), one_shot.variant());
    }
}
