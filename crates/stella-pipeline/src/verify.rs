//! Deterministic verification (L-E11): the design that stops
//! plausible-but-unverified work from being called done. Two pure pieces live
//! here — the flip-oracle state machine and the evidence ladder. The async part
//! (running the test command) lives in [`crate::pipeline`]; everything in this
//! module is a synchronous function over owned data.
//!
//! There is no third piece any more. Verifier-response parsing and the
//! heuristic that stood in when that response never came were both removed with
//! the call they served: nothing here asks a model anything, so nothing here
//! has to decide how much to believe one.
//!
//! # The flip oracle ([`FlipOracle`])
//!
//! Only a **fail→pass flip of the same normalized test command** counts as
//! verification. A test that never failed proves nothing; a pass on a
//! *different* command proves nothing. The oracle is a `none → failing →
//! flipped` state machine keyed on a normalized command string: it locks onto
//! the first command it sees *fail*, and only a later *pass of that same
//! normalized command* moves it to flipped. This structurally excludes the
//! "it passed, ship it" false positive.
//!
//! # The evidence ladder ([`ladder_decision`])
//!
//! With the flip result plus touched-tests status and diff size, the ladder
//! decides — *before any model verifier runs*:
//! - **submit fast** (verifier skipped) when flip + touched-tests-green + diff
//!   within budget all hold;
//! - **revise** on a clear failure (touched tests red), or on a turn that
//!   never attempted anything (`NothingAttempted`);
//! - **abstain** (`Unverifiable`) when every channel was blind — no flip, no
//!   test result, and an unreadable working tree;
//! - **report it unproven** (`Unverified`) on genuinely inconclusive evidence.
//!
//! That last rung used to escalate to a model verifier. It does not, and the
//! reason is the same one the abstain rung exists for: a turn nothing could
//! observe once fell through to the verifier, which was handed an empty record
//! and answered `FAIL … the file likely does not exist` about a file that was on
//! disk (#973). "I cannot see the tree" and "the file is not there" are opposite
//! claims, and a model asked to settle evidence that does not settle will
//! produce one of them. Over an 89-task Terminal-Bench run it agreed with the
//! grader 46% of the time. So the ladder now says what it knows and stops.
//!
//! # Abstaining is not a place to hide a no-op
//!
//! The abstain rung has one failure mode of its own, and it is the mirror of
//! the one it fixed: a turn that did *nothing* looks exactly like a turn whose
//! work nothing could see. Both show no flip, no test result, an unreadable
//! tree and a zero touch count — so both abstained, and abstaining reported a
//! pass. Eleven Terminal-Bench 2.1 trials ended that way: `glm-5.2` reasoned
//! for a while, called no tool at all, and the run declared success on a task
//! it had not touched. Every one scored 0.0.
//!
//! [`LadderInputs::mutating_actions`] separates them, and it is the one input
//! here that can never be blind. Every other channel is a *probe into the
//! world* that can fail to see; the dispatch count is the pipeline's record of
//! **what it itself ran**. Zero mutating calls is not "I could not tell whether
//! anything changed", it is "nothing was ever asked to change" — evidence of
//! absence, which the ladder is otherwise built never to infer. So it gets its
//! own rung ([`LadderDecision::NothingAttempted`]) above the blind check, and
//! that rung fails closed while abstain keeps failing open, because the case
//! abstain exists for is real: one of those eleven trials' siblings did the
//! work entirely through shell redirects, recorded no touch, could not be
//! diffed — and passed its Harbor verifier.
//!
//! # Git is the only authority on "the tree changed"
//!
//! Two streams in this codebase answer to that description and they disagree.
//! `AgentEvent::FileChange` is emitted by tools whose *input* names a path, so
//! it records what the agent touched **through tools** and misses every shell
//! redirect, `patch` and `make`; the git diff of the tree misses nothing. Only
//! the second may back a ladder decision. `LadderInputs::file_change_events`
//! carries the first and is read by nothing here (#2873) — the ladder's
//! channels are the flip receipt, the touched tests, and git.
//!
//! Linters and typecheckers are deliberately **excluded** from the flip
//! oracle (L-E11): only a real test command's fail→pass counts. The pipeline
//! never feeds a lint/typecheck command to [`FlipOracle::observe`].

pub mod command_errors;
pub mod coverage;
pub mod diff_render;
pub mod fingerprint;
pub mod mutation;
pub mod standing;

use std::collections::BTreeSet;

use stella_protocol::{FlipOutcome, LadderRung, VerdictEvidence};

/// The flip oracle's state. `None` = no failing observation yet; `Failing` =
/// the tracked command has been seen failing; `Flipped` = the tracked command
/// was seen failing and then passing; `Unstable` = the command flipped but
/// its confirmation re-run failed (#859), so the flip is not trusted as
/// deterministic evidence.
///
/// The invariant the whole design rests on: **`Flipped` is reachable only by
/// passing through `Failing` for the same normalized command** — proven by
/// `tests::flip_requires_a_prior_failing_observation`. The confirmation run
/// (#859) strengthens it at the moment it matters: a deterministic pass is
/// only credited when the tracked command also passed a second time on the
/// same sealed tree, so a flaky test that failed for an unrelated reason on
/// the baseline cannot buy a `DeterministicPass` with one lucky pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FlipState {
    #[default]
    None,
    Failing,
    /// A flip was observed but its confirmation re-run failed. Not `Flipped`
    /// (no deterministic credit) and not `Failing` (a pass *was* seen) — the
    /// distinction reaches the verifier as `unstable_flip=true`, which reads
    /// "the pass could not be reproduced", a different fact from "the test
    /// never passed".
    Unstable,
    Flipped,
}

/// What one [`FlipOracle::observe`] call did — "advanced the oracle" vs "a
/// pass with nothing to prove" vs "a different command, ignored".
///
/// The pipeline acts on the oracle's cumulative *state*
/// ([`FlipOracle::is_flipped`]), never on this per-call value — within one
/// candidate the observed command is stable, so the distinctions here carry
/// no decision the state does not already carry. The return value exists so
/// the transition table below is directly assertable in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObserveOutcome {
    /// The observation changed or reinforced the tracked command's state.
    Advanced,
    /// A pass observed before any failure — proves nothing, no state change.
    NoEvidence,
    /// A different normalized command than the one being tracked — ignored.
    Ignored,
}

/// The deterministic flip oracle (L-E11). Construct empty; feed it
/// `(command, passed)` observations. It locks onto the first command it sees
/// *fail* and thereafter only reasons about that one normalized command.
///
/// Keyed on the *normalized* command ([`normalize_command`]) so incidental
/// whitespace differences between two runs of the same command don't look
/// like two different commands — but token/flag reordering is intentionally
/// NOT normalized away, because reordering can change a command's meaning
/// (a pass on `cargo test -p a` must never be credited to a failure of
/// `cargo test -p b`).
#[derive(Debug, Clone, Default)]
pub struct FlipOracle {
    /// The normalized command the oracle locked onto (set on first failure).
    tracked: Option<String>,
    state: FlipState,
    /// The failing test names the tracked command's most recent failing
    /// observation reported (#867), when its output named any. Empty when
    /// the runner dialect parsed to nothing — and then the fingerprint guard
    /// stands down entirely.
    baseline_failures: BTreeSet<String>,
    /// A flip credit was refused because the passing run named its tests and
    /// none of the baseline's failures were among them — the pass
    /// demonstrably fixed a *different* failure (#867). Sticky until a pass
    /// earns the credit, so verifier evidence can say why the flip is absent.
    refused_different_failure: bool,
}

impl FlipOracle {
    /// A fresh oracle in the `None` state, tracking no command yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The oracle's current state.
    pub fn state(&self) -> FlipState {
        self.state
    }

    /// Whether the oracle has observed a genuine fail→pass flip of the same
    /// normalized command. This is the *only* deterministic "verified" signal
    /// the ladder trusts. `Unstable` — a flip whose confirmation re-run
    /// failed (#859) — is NOT flipped: the pass could not be reproduced.
    pub fn is_flipped(&self) -> bool {
        matches!(self.state, FlipState::Flipped)
    }

    /// The oracle's finding as the tri-state the ladder and the wire both
    /// carry (#2556).
    ///
    /// The distinction a bool could not make is drawn here, at the one place
    /// that holds both facts: `Unobserved` is *the oracle never locked onto a
    /// command*, which is the same condition [`Self::tracked_command`] reports
    /// as `None` and the same one the verifier prompt has rendered as
    /// `unobserved` since #2531. Everything downstream reads this rather than
    /// re-deriving it, so the prompt and the telemetry cannot drift apart.
    pub fn outcome(&self) -> FlipOutcome {
        if self.is_flipped() {
            FlipOutcome::Achieved
        } else if self.tracked.is_none() {
            FlipOutcome::Unobserved
        } else {
            FlipOutcome::NotAchieved
        }
    }

    /// Whether the oracle reached `Unstable`: a flip was observed but its
    /// confirmation re-run failed (#859). Surfaced in verifier evidence so the
    /// model verifier weighs "the pass was not reproducible" rather than
    /// mistaking the state for an ordinary never-passed failure.
    pub fn is_unstable(&self) -> bool {
        matches!(self.state, FlipState::Unstable)
    }

    /// The normalized command the oracle is tracking, if it has locked onto
    /// one (i.e. once it has seen a first failure).
    pub fn tracked_command(&self) -> Option<&str> {
        self.tracked.as_deref()
    }

    /// Observe one run of a test `command` with its `passed` result. Returns
    /// what the observation did. Transition table:
    ///
    /// | state      | observation                          | next       |
    /// |------------|--------------------------------------|------------|
    /// | None       | pass (any cmd)                       | None (NoEvidence) |
    /// | None       | fail (cmd C)                        | Failing, tracked=C |
    /// | Failing/Flipped/Unstable | different cmd than tracked | unchanged (Ignored) |
    /// | Failing    | fail (tracked cmd)                  | Failing    |
    /// | Failing    | pass (tracked cmd)                  | Flipped    |
    /// | Flipped    | pass (tracked cmd)                  | Flipped    |
    /// | Flipped    | fail (tracked cmd)                  | Failing (honest regression) |
    /// | Unstable   | pass (tracked cmd)                  | Flipped (a fresh flip, re-confirmed before credit) |
    /// | Unstable   | fail (tracked cmd)                  | Unstable (the pass that *was* seen stays on record) |
    ///
    /// The honest `Flipped → Failing` regression edge keeps the oracle
    /// truthful if a "fixed" test starts failing again on re-run; it never
    /// violates the core invariant (reaching `Flipped` still required a prior
    /// `Failing` of the same command). `Unstable` (#859) is entered only
    /// through [`Self::confirm`], never through an observation — and leaving
    /// it through a pass puts the oracle back at `Flipped`, where the
    /// pipeline's pre-submit audit will demand a fresh confirmation before
    /// any deterministic credit is spent.
    pub fn observe(&mut self, command: &str, passed: bool) -> ObserveOutcome {
        let norm = normalize_command(command);
        match &self.tracked {
            None => {
                if passed {
                    // A pass with no prior failure proves nothing — do not
                    // even lock the command (L-E11).
                    ObserveOutcome::NoEvidence
                } else {
                    self.tracked = Some(norm);
                    self.state = FlipState::Failing;
                    ObserveOutcome::Advanced
                }
            }
            Some(tracked) => {
                if *tracked != norm {
                    return ObserveOutcome::Ignored;
                }
                self.state = match (self.state, passed) {
                    (FlipState::Failing, true)
                    | (FlipState::Flipped, true)
                    | (FlipState::Unstable, true) => FlipState::Flipped,
                    (FlipState::Failing, false) | (FlipState::Flipped, false) => FlipState::Failing,
                    // The confirmation already failed once; another failure
                    // adds nothing, and the pass that WAS observed stays on
                    // record for the verifier.
                    (FlipState::Unstable, false) => FlipState::Unstable,
                    // `None` with a tracked command is unreachable (they are
                    // set together), but stay total rather than panic.
                    (FlipState::None, true) => FlipState::None,
                    (FlipState::None, false) => FlipState::Failing,
                };
                ObserveOutcome::Advanced
            }
        }
    }

    /// [`Self::observe`] with the run's output tail attached — the entry
    /// point the pipeline uses, and the home of the same-*failure* rule
    /// (#867). Two additions over the plain observation:
    ///
    /// - A **failing** run of the tracked command refreshes
    ///   `baseline_failures` with the test names its output reports (the
    ///   latest failure is the one a flip must fix).
    /// - A **passing** run that would earn a flip is first checked against
    ///   them: if the pass names its tests and NONE of the baseline's
    ///   failures appear among them, the pass demonstrably fixed a
    ///   *different* failure — most concretely, the failing test was deleted
    ///   or renamed and the suite exits 0 around its absence. That pass is
    ///   `NoEvidence`: no state change, no deterministic credit, and
    ///   [`Self::refused_different_failure`] turns on for the verifier
    ///   evidence.
    ///
    /// Degrades open on every dark input: no baseline names, an unparseable
    /// passing tail, or a passing tail that names nothing all leave the
    /// exit-code behavior exactly as it was. The refusal needs positive
    /// evidence of a different fix; absence of evidence never withholds.
    pub fn observe_run(&mut self, command: &str, passed: bool, output: &str) -> ObserveOutcome {
        let norm = normalize_command(command);
        let tracked = self.tracked.as_deref() == Some(norm.as_str());
        if passed
            && tracked
            && !self.is_flipped()
            && !self.baseline_failures.is_empty()
            && let Some(results) = fingerprint::parse_test_results(output)
            // The listing must be demonstrably COMPLETE (the runner's own
            // summary count matches the names parsed) — a truncated tail
            // that dropped the one `ok` line that mattered must not fail an
            // honest fix.
            && results.pass_listing_complete()
            && !results.passed.is_empty()
            && self.baseline_failures.is_disjoint(&results.passed)
        {
            self.refused_different_failure = true;
            return ObserveOutcome::NoEvidence;
        }
        let outcome = self.observe(command, passed);
        if passed {
            if self.is_flipped() {
                // The credit was earned (or the fingerprint guard had
                // nothing to say) — a stale refusal must not haunt the
                // evidence.
                self.refused_different_failure = false;
            }
        } else if self.tracked.as_deref() == Some(norm.as_str())
            && let Some(results) = fingerprint::parse_test_results(output)
            && !results.failed.is_empty()
        {
            // Recorded after `observe` so the very first failure — which is
            // what locks `tracked` — contributes its names too.
            self.baseline_failures = results.failed;
        }
        outcome
    }

    /// Whether the last would-be flip was refused for fixing a different
    /// failure than the one observed (#867) — surfaced in verifier evidence.
    pub fn refused_different_failure(&self) -> bool {
        self.refused_different_failure
    }

    /// The confirmation verdict (#859), fed by the pipeline's pre-submit
    /// audit: with the oracle at `Flipped` and a deterministic fast-submit
    /// imminent, the tracked command is re-run once on the same sealed tree
    /// and the result lands here.
    ///
    /// - `passed = true` — the flip is confirmed; the oracle stays `Flipped`.
    /// - `passed = false` — the pass could not be reproduced (a flake, or an
    ///   infra outcome that observed nothing); the oracle moves to
    ///   [`FlipState::Unstable`] and `is_flipped()` turns false, so the
    ///   ladder escalates instead of crediting a `DeterministicPass`.
    ///
    /// A no-op in any state but `Flipped`: confirmation is only meaningful
    /// where a flip stands to be credited.
    pub fn confirm(&mut self, passed: bool) {
        if self.state == FlipState::Flipped && !passed {
            self.state = FlipState::Unstable;
        }
    }
}

/// Normalize a test command for the flip oracle's identity check: trim, and
/// collapse every run of ASCII whitespace to a single space. This makes
/// `"cargo   test  -p x"` and `"cargo test -p x"` the same tracked command
/// while leaving token order — which can be semantically load-bearing —
/// untouched.
pub fn normalize_command(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The five ways the evidence ladder resolves a turn — all of them terminal,
/// and all of them decided from deterministic observations alone (L-E11).
///
/// There is deliberately no "ask a model" arm. Every variant here is a
/// conclusion the oracle reached itself; a turn the oracle cannot settle
/// resolves to [`Self::Unverified`], which is an honest "not proven" rather
/// than a second model's opinion wearing a verdict's clothes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LadderDecision {
    /// Deterministic pass: flip achieved + touched-tests-green + diff within
    /// budget. Submit fast; the model verifier is SKIPPED and a deterministic
    /// `Verdict { passed: true }` is emitted.
    SubmitFast,
    /// Clear failure (touched tests are red): feed the evidence back into a
    /// revision turn. No verifier call — the failure is already deterministic.
    Revise,
    /// The authored witness failed **the same way** before and after the work
    /// (#2540). It does not discriminate, so its red says nothing about the
    /// change and no revision can make it say anything.
    ///
    /// A claim about the *witness*, not the work — the same `-able` / `-ed`
    /// split [`Self::Unverifiable`] and [`Self::Unverified`] carry. Terminal
    /// and outranking [`Self::Revise`], because `Revise` blames the worker for
    /// the failure it is handed, and the worker cannot repair an instrument.
    ///
    /// See [`LadderInputs::witness_unmoved_by_revision`] for why the trigger is
    /// fingerprint equality and not "it failed twice".
    WitnessUnsatisfiable,
    /// The turn dispatched nothing that could change the workspace, and no
    /// channel saw anything change — see [`LadderInputs::nothing_was_attempted`].
    /// A determinate finding, not an abstention: revise, and report `passed:
    /// false` if the revisions run out.
    NothingAttempted,
    /// The turn went unobserved, by either route: **every** evidence channel
    /// was unavailable ([`LadderInputs::evidence_is_blind`]), or the channels
    /// were available and saw nothing of work that was demonstrably dispatched
    /// ([`LadderInputs::effects_escaped_collection`]). The ladder abstains: no
    /// verifier call, and the run is scored as unverified rather than passed
    /// or failed.
    Unverifiable,
    /// Inconclusive: no flip evidence, or diff over budget, or tests couldn't
    /// be run — but at least one channel could still see something.
    ///
    /// **Terminal.** This is where the escalation to a model verifier used to
    /// begin, and the reason it no longer does is that the escalation could not
    /// answer the question it was asked. The evidence that reaches this rung is
    /// by construction the evidence no oracle could settle; handing it to a
    /// model does not add an observation, it adds an opinion, and the opinion
    /// was measured: over an 89-task Terminal-Bench run it agreed with the
    /// benchmark's grader 46% of the time, and 17 of its false passes cost 5
    /// tasks outright.
    ///
    /// The cost was not only wrong answers. A verdict is prose, prose fed back
    /// to a worker is an instruction, and on `fix-git` a reviewer's
    /// unsubstantiated claim made the worker reset `master` and destroy a
    /// correctly-recovered commit — twice. A rung that can do that has negative
    /// value even when its accuracy is a coin flip.
    ///
    /// So the ladder stops here and says so. Scored `Unverified`: not a pass,
    /// and explicitly not a failure — the work may well be correct, and nothing
    /// available proved it either way.
    Unverified,
}

/// The evidence gathered after execution, over which [`ladder_decision`]
/// reasons. All fields are owned plain data — the ladder is a pure function.
///
/// `Default` is the all-dark input: nothing observed, nothing dispatched,
/// budget zero. Tests name the fields they exercise and take the rest from
/// it, so adding an evidence channel does not rewrite every literal.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LadderInputs {
    /// What the flip oracle found for its tracked test command — including
    /// whether it tracked one at all (#2556). The ladder's arithmetic asks
    /// only [`FlipOutcome::is_achieved`]; the third state is carried so the
    /// snapshot this becomes can state it.
    pub flip: FlipOutcome,
    /// Whether the touched tests passed after execution. `None` when no test
    /// command was available/run — an *inconclusive* signal, not a pass.
    pub touched_tests_passed: Option<bool>,
    /// The **authored witness** failed with the same
    /// [`airlock::FailureFingerprint`] as the baseline that armed it, *after*
    /// the worker had already been told about that failure and revised (#2540).
    ///
    /// # Two conditions, and both are load-bearing
    ///
    /// **Fingerprint equality**, first, because "it failed twice" is not a
    /// substitute. Every armed witness fails on the baseline by construction —
    /// the witness stage rejects one that does not — so "red on both trees" is
    /// true of every failing witness and discriminates nothing. A witness whose
    /// failure *moved* has demonstrably observed the change, even if it is not
    /// yet green, and that is real feedback the worker can act on. The
    /// fingerprint normalizes timings, paths, pids and line/column churn
    /// ([`airlock::normalize_failure`]), so this is "the same failure", not
    /// "byte-identical bytes".
    ///
    /// **A revision already spent**, second, and this is the condition the
    /// obvious design omits. Equality between the baseline and the *first*
    /// candidate run is consistent with two different worlds: the witness is
    /// deaf to the change, or the change simply did not do the thing the
    /// witness asks about. Both produce the identical observation, so acting on
    /// it would be reading evidence that cannot distinguish the claim from its
    /// opposite — and the arm it feeds is terminal, so getting it wrong costs
    /// the worker the whole repair loop on a turn that just needed a second
    /// attempt.
    ///
    /// Requiring a revision first turns the reading into an actual experiment:
    /// the worker was handed the failure, changed the work in response, and the
    /// witness said *exactly the same thing*. That is invariance to the work,
    /// which is what "the instrument does not discriminate" means. It costs one
    /// revision — and one revision, not a whole budget, is the entire harm
    /// #2540 reported.
    ///
    /// `false` covers "the failure moved", "no revision has been spent yet",
    /// "no authored witness was in play" (a configured `--test-command` is the
    /// operator's instrument, not one the pipeline commissioned) and "nothing
    /// could be compared". One-way, like every other probe on this struct: it
    /// can only ever withhold blame, never assign it.
    ///
    /// [`airlock::FailureFingerprint`]: crate::witness::airlock::FailureFingerprint
    /// [`airlock::normalize_failure`]: crate::witness::airlock::normalize_failure
    pub witness_unmoved_by_revision: bool,
    /// Lines changed by the turn (from the diff command).
    pub diff_lines: u32,
    /// The diff-size budget; a diff at or under this is "small enough" to
    /// trust deterministic evidence without a verifier.
    pub diff_budget: u32,
    /// Whether the diff probe could **read the working tree at all**. `false`
    /// is "I could not look", which is a different claim from `diff_lines == 0`
    /// ("I looked and saw nothing") — conflating the two is what let a blind
    /// probe be reported as a confident absence (#973). A Terminal-Bench task
    /// directory is not a git repository, so `git diff` there can only ever
    /// answer `false`.
    pub diff_available: bool,
    /// Mutating file touches the recorder observed this turn, from the registry
    /// that emitted the `FileChange` events (`FileTouchPort`).
    ///
    /// **Recorded only: [`ladder_decision`] does not read it** (#2873). It is a
    /// tally of what the agent touched **through tools**, which is a different
    /// question from whether the tree changed — a `bash` heredoc mutates the
    /// workspace and emits nothing here — and the ladder's authority on the
    /// second question is git ([`Self::diff_available`] with
    /// [`Self::diff_lines`]).
    ///
    /// Three predicates conjoined on it until #2873, each documented as if it
    /// were a live channel, and it is **zero wherever the ladder actually
    /// runs**. Measured, not inferred: on the 2026-08-11 Terminal-Bench panel,
    /// across 53 pipeline trials that reached a verdict, the 49 that ran in a
    /// candidate workspace **all** recorded `0` before the verdict and the 4
    /// that did not **all** recorded a non-zero count — 53/53 — with
    /// [`Self::mutating_actions`] between 6 and 147 beside it. Both operands of
    /// the `max` in `observed_mutations` therefore read zero on every one of
    /// those 49 runs; the tree's own account of the event half is
    /// `verify_probes::DIFF_PROBE_FAILED` ("the engine emits no `FileChange`
    /// events … inside a best-of-N or witness candidate the count is always
    /// zero"), which `pipeline::tests::warrant` had already written down.
    ///
    /// It is deliberately not deleted with the read, for the same reason
    /// [`Self::no_test_surface`] was not: the pipeline still sets it, the
    /// snapshot still puts it on the wire, and `replay` still renders it,
    /// because "the tools declared this many touches" is a fact worth reading
    /// off a corpus of traces. Whether it should regain a decision role — which
    /// needs a signal a candidate can actually feed, not a wider read of this
    /// one — or be retired is #2873; what it must not do is keep claiming an
    /// effect it never had.
    pub file_change_events: u32,
    /// Tool calls this turn that were *capable* of changing the workspace:
    /// every dispatched call except those whose tool the registry advertises
    /// as `read_only`.
    ///
    /// Unlike every other field here this is not a probe into the world, so it
    /// cannot come back blind — it is the pipeline's tally of the calls it
    /// itself dispatched. That is what makes `0` mean "nothing was attempted"
    /// rather than "nothing was seen", and it is the only input the ladder is
    /// willing to read as evidence of *absence*.
    ///
    /// A call whose tool is unknown counts as mutating. Shell tools are the
    /// reason: `bash` is not `read_only`, and on Terminal-Bench it is how
    /// nearly all real work lands — so an unrecognized name must never be the
    /// reason a turn is written off as a no-op.
    pub mutating_actions: u32,
    /// Lint/typecheck ERRORS the candidate introduced over the baseline
    /// snapshot (#861) — the regression veto's trigger. `0` both when the
    /// tree is clean and when the probe is unavailable: lint can only ever
    /// *withhold* a fast-submit (rung 4), never grant one, so an absent
    /// probe degrades to the pre-veto ladder rather than fabricating a
    /// regression.
    pub new_diag_errors: u32,
    /// New lint/typecheck warnings over the baseline. Vetoes only when
    /// [`Self::veto_warnings`] opts in — a fresh warning is real signal but
    /// blocking on it by default would tax every workspace with a chatty
    /// linter.
    pub new_diag_warnings: u32,
    /// Configuration, not evidence: whether new warnings also veto the
    /// fast-submit (errors always do). Carried here so the ladder stays a
    /// pure function of one input value.
    pub veto_warnings: bool,
    /// What the trivial-mutation audit found about the authored witness
    /// (#870): does it constrain the changed lines, or merely react to them?
    /// A [`mutation::MutationAudit::Tautological`] witness may not buy a
    /// deterministic pass.
    ///
    /// Tri-state rather than a `bool` (#2607). The downgrade still requires
    /// positive evidence of tautology and never its absence — but "the audit
    /// cleared this witness" and "no audit ever ran" are now different
    /// values, so the code that feeds this field cannot be deleted into a
    /// finding of innocence. See [`mutation::MutationAudit`].
    pub witness_mutation: mutation::MutationAudit,
    /// Whether the passing test run actually executed the lines the change
    /// added (#1291). [`coverage::DiffCoverage::Unmeasured`] — the default —
    /// is "nobody could tell", which is a different answer from "it did not"
    /// and is treated as one.
    pub diff_coverage: coverage::DiffCoverage,
    /// Configuration, not evidence: whether an *unmeasured* overlap also
    /// withholds the fast-submit (a measured non-overlap always does).
    /// Carried here so the ladder stays a pure function of one input value,
    /// exactly like [`Self::veto_warnings`].
    pub require_diff_coverage: bool,
    /// The worker's own `verify_done` tool run printed `WITNESS CONFIRMED`
    /// this candidate (#2129): a deterministic fail-on-baseline / pass-on-new
    /// shadow run, observed off the turn's `ToolResult` stream. Distinct from
    /// [`Self::flip`] because the pipeline's oracle tracks only its
    /// own command; before this field, a confirmed `verify_done` flip and a
    /// failing "no flip" fallback verdict coexisted in one trace.
    ///
    /// A completion receipt, not telemetry: [`ladder_decision`] credits it
    /// through [`Self::has_flip_receipt`], and it can carry `SubmitFast` on
    /// its own (#2618). It cannot be forged — the harvest is call-id
    /// correlated to a dispatched `verify_done` call *and* requires the result
    /// to start with `WITNESS CONFIRMED`, so neither an MCP tool shadowing the
    /// name nor a shell `echo` reaches this field.
    pub verify_done_flip: bool,
    /// Positive claim that this round had NO tracked test command at all —
    /// neither a configured `--test-command` nor an authored witness — so
    /// "no flip" is a demand the task structurally cannot meet (#2129: a
    /// one-line `answer.txt` deliverable has no tests to flip).
    ///
    /// **Recorded only: [`ladder_decision`] does not read it.** It used to be
    /// the one field that could turn a fallback FAIL into an abstention, and
    /// that description outlived the machinery — #2584 deleted
    /// `verify::heuristic_fallback` along with the model verdict, and nothing
    /// took over the read. Two `LadderInputs` differing only here return the
    /// identical decision today.
    ///
    /// It is deliberately not deleted with the read. The pipeline still sets
    /// it, the pipeline's `ladder_snapshot` still puts it on the
    /// wire, and `replay` still renders it, because "the task had no test
    /// surface" is the fact that separates an unprovable task from an unproven
    /// one when a corpus of traces is read after the fact — see
    /// [`stella_protocol::LadderSnapshot::no_test_surface`]. Whether it should
    /// regain a decision role or be retired is #2638; what it must not do is
    /// keep claiming an effect it lost.
    pub no_test_surface: bool,
    /// Command chains this turn that reported an error while exiting 0
    /// ([`command_errors`], #2125) — the shape a cited measurement can
    /// silently stand on.
    ///
    /// Carried for the model verifier to weigh and read by nothing in
    /// [`ladder_decision`], on purpose: an errored probe makes a quantity
    /// *unsubstantiated*, not *disproven*, so it may inform an opinion and
    /// must never withhold a deterministic pass. Like every probe here except
    /// [`Self::mutating_actions`] it is one-way — `0` is "the closed signature
    /// vocabulary matched nothing", never "this run's commands were clean".
    pub errored_commands: u32,
}

impl LadderInputs {
    /// Whether any deterministic red→green receipt exists for this turn.
    ///
    /// Two channels can produce one, and they are deliberately not merged into
    /// a single field because they are not interchangeable.
    /// [`Self::flip`] is the pipeline's own tracked command measured
    /// against the pre-execution snapshot; [`Self::verify_done_flip`] is the
    /// `verify_done` tool's shadow run against the baseline *it* pinned
    /// (`WITNESS_BASELINE_WORKTREE_REF`, else the first-parent walk past the
    /// pipeline's seal commits). Both are deterministic observations of a test
    /// failing before the change and passing after it, and neither is a
    /// model's opinion — so both belong on this ladder. A caller that must
    /// report *which* one carried a turn reads the two fields directly, as
    /// [`unverified_evidence`] does.
    ///
    /// #2618: the ladder read only the first for as long as this field
    /// existed, so the strongest proof this toolchain can produce — the
    /// worker's own witness, confirmed against a pinned baseline — could not
    /// reach a passing rung, and every turn on a task with no configured test
    /// command was told "no test command was tracked, so no fail→pass flip
    /// could be observed" while one sat in the trace.
    pub fn has_flip_receipt(&self) -> bool {
        self.flip.is_achieved() || self.verify_done_flip
    }

    /// Whether every channel the ladder has was unable to observe anything:
    /// no flip receipt, no test result, and a diff probe that could not read
    /// the tree.
    ///
    /// This is *not* "the turn changed nothing" — it is "nothing here can tell
    /// you either way", and the distinction is the whole value of the ladder.
    /// A verifier that reports absence of evidence as evidence of absence is
    /// worse than one that abstains, because a downstream reader cannot tell
    /// the two apart: on Terminal-Bench all four went dark at once and the
    /// verifier asserted a file "likely does not exist" while it sat in the
    /// container.
    ///
    /// Note the asymmetry: a *red* test is real evidence, so it cannot be
    /// blind. Only the total absence qualifies.
    ///
    /// Reads [`Self::has_flip_receipt`] rather than [`Self::flip`]
    /// for the reason the abstention sits above the credit: a turn whose only
    /// observation was a `verify_done` receipt satisfies every dark channel on
    /// the narrower predicate, so this rung would swallow it before step 4
    /// could ever look (#2618).
    ///
    /// Every conjunct is an **observation** channel. [`Self::mutating_actions`]
    /// is absent because it is a dispatch record, not a look at the world, and
    /// the rung above ([`Self::nothing_was_attempted`]) is where the ladder
    /// reads it. [`Self::file_change_events`] was a fourth conjunct until
    /// #2873 and belongs to that same dispatch side — it counts what the tools
    /// declared, never what the tree did, and inside a candidate workspace it
    /// is pinned at zero, so it contributed a constant `true` to a predicate
    /// whose doc advertised it as a channel.
    pub fn evidence_is_blind(&self) -> bool {
        !self.has_flip_receipt() && self.touched_tests_passed.is_none() && !self.diff_available
    }

    /// Whether a model verifier's `passed` would be the *only* thing standing
    /// behind the claim — no flip, and no test that ran green.
    ///
    /// Deliberately narrow. A recorded touch or a readable diff proves the
    /// tree **changed**; neither says the change is **correct**, and only the
    /// second claim is the one a pass makes. So they are excluded here even
    /// though they are real evidence elsewhere on the ladder.
    ///
    /// This exists because the verifier's authority was measured and found
    /// wanting. Across an 89-task Terminal-Bench run the authored-witness
    /// rung never fired — the posture pins one model for every role, and
    /// Stella will not let the worker write the test that proves the worker,
    /// so the verifier was reasoning from a diff and its own opinion. It agreed
    /// with the benchmark's grader 46% of the time, and 17 of its false
    /// passes cost 5 tasks outright.
    ///
    /// The response is asymmetric trust rather than removal. A verifier that
    /// says "not yet" is still useful with weak evidence — being wrong costs
    /// one more revision. A verifier that says "done" on the same evidence ends
    /// the run, so that direction has to be earned. When it is not, the turn
    /// is scored **unverified**, never failed: a run is not broken by the
    /// absence of a way to check it, and a Terminal-Bench trial that scored
    /// 1.0 against its own verifier has taken exactly this path.
    ///
    /// A worker-run `verify_done` confirmation counts as corroboration
    /// (#2129): it is a deterministic tool observation — the witness failed
    /// on the pinned baseline and passed on the change — not another model's
    /// opinion, which is exactly the class of evidence this predicate exists
    /// to demand.
    pub fn verifier_pass_stands_alone(&self) -> bool {
        !self.flip.is_achieved()
            && self.touched_tests_passed != Some(true)
            && !self.verify_done_flip
    }

    /// Whether this turn provably did nothing: it dispatched no call that
    /// could change the workspace, and no channel that *did* report saw any
    /// change.
    ///
    /// Deliberately says nothing about [`Self::diff_available`]. Every other
    /// rung has to care whether the probe could look; this one does not,
    /// because it does not rest on looking. If the model never asked for a
    /// mutating action, the workspace cannot have changed, and an unreadable
    /// tree is not a reason to doubt that — which is exactly why this must be
    /// checked *before* [`Self::evidence_is_blind`], whose four dark channels
    /// this state would otherwise satisfy on the way to reporting a pass.
    ///
    /// The remaining conjuncts are there so a single positive observation
    /// always wins. A flip, a test result or a non-empty diff each mean
    /// *something* happened, whoever caused it — and a turn with something to
    /// explain deserves a verdict, not this shortcut.
    ///
    /// [`Self::file_change_events`] was one of them until #2873, where it was
    /// pure redundancy: every `FileChange` is emitted from a mutating tool
    /// call, so a non-zero count implies a non-zero
    /// [`Self::mutating_actions`], which the first conjunct already excludes.
    pub fn nothing_was_attempted(&self) -> bool {
        self.mutating_actions == 0
            && self.diff_lines == 0
            && !self.flip.is_achieved()
            && self.touched_tests_passed.is_none()
    }

    /// Whether this turn's effects landed somewhere this run does not collect:
    /// it dispatched calls able to change the workspace, the diff probe *could*
    /// read the tree and found it unchanged, and no other channel saw anything
    /// either.
    ///
    /// The exact inverse of [`Self::nothing_was_attempted`] on its one
    /// load-bearing conjunct, and the reason both exist. A readable, empty diff
    /// is a confident observation — but only of the tree the probe was pointed
    /// at. When the dispatch record says calls were made, "the tree did not
    /// move" and "the work happened elsewhere" produce identical readings, and
    /// nothing available here separates them. Neither may be asserted, so the
    /// ladder abstains.
    ///
    /// Named after the failure it ends (#1701): a Terminal-Bench
    /// system-configuration task installed and configured nginx against the
    /// real `/etc/nginx` — which is the only place `service nginx restart`
    /// will ever read — while the run collected a candidate root that stayed
    /// empty. Ten mutating calls, a readable zero-line diff, and a verdict of
    /// `passed: true, deterministic: true`. Note what does *not* reach here: a
    /// flip or a green test each corroborate the work, and either one sends
    /// the turn on to the rungs that can credit it.
    ///
    /// [`Self::file_change_events`] used to sit beside them as a third
    /// corroborator, and it was pointing the wrong way (#2873). The premise of
    /// this predicate is that git *looked at the collected tree and found it
    /// unchanged*. A tool tally saying "I wrote files" against that reading is
    /// evidence **for** the effects having landed somewhere this run does not
    /// collect, not against it — so counting it as corroboration let a
    /// tool-call tally overrule the one channel that had actually observed the
    /// tree.
    pub fn effects_escaped_collection(&self) -> bool {
        self.mutating_actions > 0
            && self.diff_available
            && self.diff_lines == 0
            && !self.flip.is_achieved()
            && self.touched_tests_passed.is_none()
    }
}

/// The evidence ladder (L-E11). Decides submit/revise/abstain/escalate from
/// deterministic evidence alone. Ordering of the checks matters:
///
/// 0. **The witness cannot discriminate → `WitnessUnsatisfiable`.** The
///    authored witness failed identically before and after the work, so its
///    red is a fact about the instrument. Above `Revise` on purpose: `Revise`
///    hands the failure back as the thing to fix, and the worker cannot fix
///    it (#2540).
/// 1. **Touched tests red → `Revise`.** A red test is a clear, deterministic
///    failure; never spend a verifier call to "confirm" it.
/// 2. **Nothing attempted → `NothingAttempted`.** The turn dispatched no
///    mutating call and nothing observed a change. Checked *above* the blind
///    rung, which it would otherwise satisfy — and does not fall through to
///    it, because "no action was taken" is knowledge, not an absence of it.
/// 3. **The turn went unobserved → `Unverifiable`.** Two routes, one state.
///    Either every channel was blind, or every channel could look and none of
///    them saw the work this run demonstrably dispatched. Nothing may be
///    claimed about it — in particular not a failure.
/// 4. **A corroborated flip receipt, within budget → `SubmitFast`.** The full
///    deterministic pass. Either the pipeline's own flip with the touched
///    tests green beside it, or a `verify_done` confirmation, which carries
///    its own corroboration (#2618).
/// 5. **Otherwise → `Unverified`.** Genuinely inconclusive: no flip, or the
///    diff is over budget, or tests couldn't be run — but something could still
///    see. Terminal, and never an escalation: see [`LadderDecision::Unverified`]
///    for why a model's opinion is not a rung on an evidence ladder.
pub fn ladder_decision(inputs: &LadderInputs) -> LadderDecision {
    // 0. The authored witness failed the same way on both trees (#2540). It
    //    is red, but its red does not depend on the change, so it is evidence
    //    about the instrument and nothing else. Ordered above the `Revise`
    //    below because that arm's whole content is "here is what to fix", and
    //    a witness the worker cannot dispose of turns a solved task into a
    //    deadline: on `fix-git` the worker diagnosed the contamination
    //    correctly and every revision minted another snapshot commit for the
    //    witness to count.
    if inputs.touched_tests_passed == Some(false) && inputs.witness_unmoved_by_revision {
        return LadderDecision::WitnessUnsatisfiable;
    }
    // 1. A red touched-test is a deterministic failure — revise, no verifier.
    if inputs.touched_tests_passed == Some(false) {
        return LadderDecision::Revise;
    }
    // 2. The turn never acted. Ordered above the blind rung on purpose: this
    //    state satisfies all four of its dark channels, so abstaining would
    //    absorb it and report the pass that shipped eleven untouched
    //    Terminal-Bench tasks as successes.
    if inputs.nothing_was_attempted() {
        return LadderDecision::NothingAttempted;
    }
    // 3. Nothing could observe the turn. Buying a verifier call here spends money
    //    to ask a model to guess from an empty record, and the answer it
    //    produced in the wild was a confident FAIL naming a file that existed.
    if inputs.evidence_is_blind() {
        return LadderDecision::Unverifiable;
    }
    // 3b. The probe could look, looked, and found an unchanged tree — while
    //     this pipeline's own record says calls able to write it were
    //     dispatched. That is not a clean turn; it is a turn whose effects
    //     landed outside what the run collects, and the two are
    //     indistinguishable from here (#1701). Same abstention as above, for
    //     the same reason: what cannot be observed cannot be claimed, in
    //     either direction. Deliberately NOT `Revise` — the work may be
    //     entirely correct and merely uncollected, and no revision can make
    //     an un-snapshot-able workspace observable.
    if inputs.effects_escaped_collection() {
        return LadderDecision::Unverifiable;
    }
    // 4. Full deterministic pass — submit fast, verifier skipped. The
    //    diagnostics conjuncts are the regression veto (#861): a flipped
    //    witness plus a fresh type error in an untested module is exactly
    //    the inconclusive case a second opinion existed for, so new errors
    //    (and, opted-in, new warnings) drop this rung through to
    //    `Unverified`. Lint stays excluded from the oracle — it can veto a
    //    submit, never verify one.
    //
    //    Two receipts can carry this rung (#2618), and they demand different
    //    corroboration because they observe different things. The pipeline's
    //    own flip needs the touched-test run beside it: that oracle proves
    //    only that *its* command went red→green, and says nothing about the
    //    rest of the suite. A `verify_done` confirmation already contains
    //    both halves — the tool ran the witness on the baseline it pinned and
    //    again on the change, so the red→green observation and the
    //    green-on-the-change observation are one run. Demanding a second,
    //    pipeline-side green on top would withhold the credit on precisely
    //    the tasks the field was added for: the ones with no configured test
    //    command at all (#2129), where `touched_tests_passed` is structurally
    //    `None` and no amount of correct work can make it `Some(true)`.
    //
    //    A red touched test still outranks both. It returned `Revise` at step
    //    1, above every receipt here, so neither channel can talk over a test
    //    that is failing now.
    let receipt_is_corroborated = (inputs.flip.is_achieved()
        && inputs.touched_tests_passed == Some(true))
        || inputs.verify_done_flip;
    if receipt_is_corroborated
        && inputs.diff_lines <= inputs.diff_budget
        && inputs.new_diag_errors == 0
        && (!inputs.veto_warnings || inputs.new_diag_warnings == 0)
        // #870: a witness that stays green under every observed mutant of the
        // changed lines reacts to the change without constraining it. An
        // audit that never ran credits the pass — the probe is optional and
        // degrades open — but it is a distinguishable value (#2607), not the
        // same `false` as a cleared witness.
        && inputs.witness_mutation.credits_a_deterministic_pass()
        // #1291: a test that never executed the changed lines passed for some
        // other reason. Withholding the deterministic credit sends the turn to
        // the verifier — "unproven" — and is never a failure; an *unmeasured*
        // overlap withholds only when the operator asked for strictness.
        && inputs
            .diff_coverage
            .credits_a_deterministic_pass(inputs.require_diff_coverage)
    {
        return LadderDecision::SubmitFast;
    }
    // 5. Inconclusive, and that is the answer. Nothing below this line asks a
    //    model: the evidence that reaches here is precisely the evidence no
    //    oracle could settle, so a second model would be guessing at it too —
    //    only with the authority of a verdict attached.
    LadderDecision::Unverified
}

impl From<LadderDecision> for LadderRung {
    /// The wire name of a decision (#1043).
    ///
    /// One-way on purpose, and the missing direction is still the point,
    /// though for a narrower reason than it once was: the wire vocabulary
    /// keeps [`LadderRung::Waived`], which describes a review nobody bought
    /// rather than a decision this ladder reached, so a
    /// `LadderRung -> LadderDecision` conversion would have to invent that
    /// history backwards.
    ///
    /// Every other rung is now one-to-one with a decision, because the ladder
    /// no longer has an arm whose outcome depends on something that happens
    /// *after* it decides. That used to be the whole gap: `model_verdict` and
    /// `heuristic_fallback` were two records of how one escalation resolved.
    fn from(decision: LadderDecision) -> Self {
        match decision {
            LadderDecision::SubmitFast => LadderRung::SubmitFast,
            LadderDecision::Revise => LadderRung::Revise,
            LadderDecision::WitnessUnsatisfiable => LadderRung::WitnessUnsatisfiable,
            LadderDecision::NothingAttempted => LadderRung::NothingAttempted,
            LadderDecision::Unverifiable => LadderRung::Unverifiable,
            LadderDecision::Unverified => LadderRung::Unverified,
        }
    }
}

/// Build the deterministic `VerdictEvidence` for a `SubmitFast` verdict — the
/// evidence attached to the emitted `Verdict { passed: true,
/// evidence: { deterministic: true, .. } }`.
///
/// The coverage status rides along (#1291) even when it credited the pass,
/// and it *leads* when it did not. The deterministic badge means "a test went
/// fail→pass"; whether that test executed the changed lines is a separate
/// question, and an unmeasured answer scores the candidate `Unverified` (see
/// the `SubmitFast` arm in [`crate::pipeline`]). A reader must not have to
/// reach the end of a sentence to learn the pass is unproven.
pub fn deterministic_pass_evidence(
    tracked_cmd: Option<&str>,
    diff_lines: u32,
    diff_coverage: coverage::DiffCoverage,
) -> VerdictEvidence {
    let observed = match tracked_cmd {
        Some(cmd) => format!(
            "flip oracle: fail→pass of `{cmd}`; touched tests green; diff {diff_lines} lines within budget"
        ),
        None => format!(
            "touched tests green; diff {diff_lines} lines within budget (no flip command tracked)"
        ),
    };
    let summary = if diff_coverage == coverage::DiffCoverage::Unmeasured {
        format!("UNPROVEN — {}; {observed}", diff_coverage.explain())
    } else {
        format!("{observed}; {}", diff_coverage.explain())
    };
    VerdictEvidence {
        summary,
        deterministic: true,
        evidence_refs: Vec::new(),
        ladder: None,
    }
}

/// Build the `VerdictEvidence` for a [`LadderDecision::Unverifiable`] turn: the
/// ladder abstained because the turn went unobserved.
///
/// `deterministic: false` — this is the *absence* of a deterministic result,
/// and marking it `true` would let an unobserved turn wear the ladder's
/// strongest badge. The summary names each dark channel rather than
/// summarizing, because the only actionable content here is *why* nothing
/// could be seen: on Terminal-Bench the answer is "the task directory is not a
/// git repository", which no amount of re-running will change.
///
/// Which is also why the two routes to this rung get two summaries. Telling a
/// reader "the diff probe could not read the working tree" when it read the
/// tree fine and found it unchanged is a second verification lie in the
/// sentence written to end the first one — and it points at the wrong repair
/// (fix the probe, rather than collect the workspace the work actually landed
/// in).
pub fn unverifiable_evidence(inputs: &LadderInputs) -> VerdictEvidence {
    let summary = if inputs.effects_escaped_collection() {
        format!(
            "UNVERIFIABLE — this turn dispatched {} call(s) able to change the workspace, and the \
             diff probe then read the tree and found it unchanged, so nothing here observed the \
             work and nothing is claimed about it (this is NOT a finding that the work is absent \
             or wrong): the effects landed outside what this run collects. No fail→pass flip was \
             observed; no touched-test result. Verify the result on its own merits.",
            inputs.mutating_actions
        )
    } else {
        format!(
            "UNVERIFIABLE — no evidence channel could observe this turn, so nothing is claimed \
             about it (this is NOT a finding that the work is absent or wrong): flip oracle not \
             armed (no test command); touched tests not run; the diff probe could not read the \
             working tree ({} mutating call(s) were dispatched). Verify the result on its own \
             merits.",
            inputs.mutating_actions
        )
    };
    VerdictEvidence {
        summary,
        deterministic: false,
        evidence_refs: Vec::new(),
        ladder: None,
    }
}

/// Build the `VerdictEvidence` for a [`LadderDecision::Unverified`] turn: the
/// probes could look, they looked, and what they returned did not prove the
/// outcome either way.
///
/// The distinction from [`unverifiable_evidence`] is the whole point of having
/// two functions. That one reports *blindness* — no channel could observe the
/// turn — and the repair it implies is to fix the probes. This one reports
/// *insufficiency*: the channels worked and the evidence they produced does
/// not add up to a proof, and the repair it implies is to produce the missing
/// observation (a failing test that then passes). Telling a reader the probes
/// were blind when they were not sends them to the wrong repair, which is the
/// same class of error that made a verifier assert a file "likely does not
/// exist" while it sat on disk (#973).
///
/// `deterministic: false`, because no deterministic result was reached. The
/// summary leads with UNVERIFIED and names the specific channel that fell
/// short, since "which one" is the only actionable content: a missing flip
/// wants a test, an over-budget diff wants a smaller change, and an unrun test
/// command wants a working toolchain.
pub fn unverified_evidence(inputs: &LadderInputs, tracked_cmd: Option<&str>) -> VerdictEvidence {
    // Which channel carried the receipt, in the words of the channel that
    // made the observation (#2618). The two flips pin different baselines —
    // the pipeline's pre-execution snapshot versus `verify_done`'s own
    // `WITNESS_BASELINE_WORKTREE_REF` — so a reader deciding what to re-run
    // has to be told which one stood behind the turn, and "a flip was
    // observed" alone does not say.
    let receipt = if inputs.flip.is_achieved() {
        "a flip was observed"
    } else {
        "a `verify_done` witness confirmation was observed"
    };
    let shortfall = if !inputs.has_flip_receipt() {
        // Reads the receipt predicate, not `flip`. Saying "no
        // fail→pass flip could be observed" on a turn holding a confirmed
        // `verify_done` receipt told the operator the exact opposite of what
        // the trace recorded, and it was the commonest phrasing to hit,
        // because a task with no configured test command takes the `None`
        // arm — which is the same task `verify_done` exists to prove.
        match tracked_cmd {
            Some(cmd) => format!(
                "no fail→pass flip was observed for `{cmd}` — the only thing that can prove this \
                 change is a test that failed before it and passes after it"
            ),
            None => "no test command was tracked, so no fail→pass flip could be observed — the \
                     only thing that can prove this change is a test that failed before it and \
                     passes after it"
                .to_string(),
        }
    } else if inputs.diff_lines > inputs.diff_budget {
        format!(
            "{receipt}, but the change is {} lines against a {}-line budget, so the flip does not \
             account for all of it",
            inputs.diff_lines, inputs.diff_budget
        )
    } else if inputs.touched_tests_passed.is_none() {
        // Only reachable for a pipeline flip: a `verify_done` receipt carries
        // its own green run, so step 4 credits it without consulting this
        // field and a turn holding one never lands here.
        "a flip was observed, but the touched tests could not be run, so nothing confirmed the \
         change left the rest of the suite green"
            .to_string()
    } else if !inputs
        .diff_coverage
        .credits_a_deterministic_pass(inputs.require_diff_coverage)
    {
        // #2607: the two mechanical guards are the commonest reason a flipped
        // candidate lands here, and until now the reader was told only that
        // "the deterministic checks did not combine into a pass" — a sentence
        // that names neither the guard nor its finding, and reads identically
        // whichever one objected. Each states itself, in the guard's own
        // words, so a withheld credit is legible without opening a snapshot.
        // Coverage first, matching the order the audits actually run in.
        format!("{receipt}, but {}", inputs.diff_coverage.explain())
    } else if !inputs.witness_mutation.credits_a_deterministic_pass() {
        format!("{receipt}, but {}", inputs.witness_mutation.explain())
    } else {
        "the deterministic checks did not combine into a pass".to_string()
    };
    VerdictEvidence {
        summary: format!(
            "UNVERIFIED — {shortfall}. This is NOT a finding that the work is absent or wrong: no \
             model was asked for an opinion, because an opinion is not evidence. Verify the \
             result on its own merits."
        ),
        deterministic: false,
        evidence_refs: Vec::new(),
        ladder: None,
    }
}

/// Build the `VerdictEvidence` for a [`LadderDecision::WitnessUnsatisfiable`]
/// turn: the authored witness failed identically before and after the work
/// (#2540).
///
/// `deterministic: false`, and the contrast with
/// [`deterministic_fail_evidence`] is the whole reason this exists. That one
/// reports a *test* that went red over the change, which is a determinate
/// finding about the work. This one reports an *instrument* whose red does not
/// depend on the change at all — so nothing is claimed about the work, in
/// either direction, and the summary says so in the same words the abstaining
/// rungs use.
///
/// The summary names the shared fingerprint. It is the only actionable content
/// here: a reader auditing the run needs to see that the two failures were the
/// same failure, because that — and not the mere fact of a red witness — is
/// what makes the demand unmeetable.
///
/// Verification-side only, like every fingerprint: the digest identifies a
/// failure without quoting it, so naming it discloses nothing sealed.
pub fn witness_unsatisfiable_evidence(
    tracked_cmd: Option<&str>,
    fingerprint: &str,
) -> VerdictEvidence {
    let command = match tracked_cmd {
        Some(cmd) => format!("`{cmd}`"),
        None => "the authored witness".to_string(),
    };
    VerdictEvidence {
        summary: format!(
            "WITNESS UNSATISFIABLE — {command} failed on the baseline and failed again on the \
             change with the same failure fingerprint ({fingerprint}), so it does not \
             discriminate between them and no revision of the work can turn it green. This is \
             NOT a finding that the work is absent or wrong: the instrument is at fault, not the \
             change, and the turn is reported unproven rather than failed. Verify the result on \
             its own merits."
        ),
        deterministic: false,
        evidence_refs: vec![format!("witness_fingerprint:{fingerprint}")],
        ladder: None,
    }
}

/// Build the `VerdictEvidence` for a [`LadderDecision::NothingAttempted`] turn.
///
/// `deterministic: true`, and the contrast with [`unverifiable_evidence`] is
/// the entire point: that one is marked `false` because it reports the absence
/// of a result, while this one *is* a result. The pipeline counted the calls it
/// dispatched and none of them could write anything — no probe was consulted
/// and none could have changed the answer.
///
/// The summary leads with what was and was not done rather than with a channel
/// list, because unlike the blind case there is nothing here to diagnose: no
/// instrument failed, and re-running the probes would report the same nothing.
pub fn nothing_attempted_evidence(inputs: &LadderInputs) -> VerdictEvidence {
    VerdictEvidence {
        summary: format!(
            "NO WORK ATTEMPTED — the turn ended without dispatching a single tool call that \
             could change the workspace ({} mutating call(s), {} diff line(s)). This is not an \
             unverifiable result: nothing was asked to change, so nothing did, whatever the \
             other channels could or could not see.",
            inputs.mutating_actions, inputs.diff_lines
        ),
        deterministic: true,
        evidence_refs: Vec::new(),
        ladder: None,
    }
}

/// Build the deterministic `VerdictEvidence` for a `Revise` verdict (touched
/// tests red) — a `passed: false`, `deterministic: true` verdict.
pub fn deterministic_fail_evidence(tail: &str) -> VerdictEvidence {
    VerdictEvidence {
        summary: format!("touched tests failed after execution: {}", tail.trim()),
        deterministic: true,
        evidence_refs: Vec::new(),
        ladder: None,
    }
}

/// A diff with the authored witness's own file chunks removed, plus the
/// paths that were removed — see [`strip_witness_hunks`].
pub struct StrippedDiff {
    pub diff: String,
    pub omitted: Vec<String>,
}

/// Remove the authored witness's file chunks from a diff bound for a
/// verifier-facing prompt.
///
/// The witness artifact is grafted into the candidate tree before
/// verification, so the working-tree diff carries the verifier's OWN test
/// under the prompt's "worker-authored data" heading — misattributed, and
/// billed against the diff budget on every escalated verdict and guidance
/// call. What the verifier needs to know about the witness it already gets
/// from the trusted evidence summary (`witness_tamper_check`,
/// `witness_mutation`, the oracle trace); the test's text says nothing
/// about the change under review.
///
/// Paths are matched the way [`coverage::changed_lines`] excludes them: each
/// chunk's first `+++ ` header, `b/`-stripped, compared exactly. The omitted
/// paths are RETURNED, not written into the diff — the caller names them in
/// the evidence summary, which is the trusted zone; an in-band note would sit
/// in the region the verifier is told to treat as forgeable worker data.
pub fn strip_witness_hunks(diff: &str, witness_paths: &[String]) -> StrippedDiff {
    if witness_paths.is_empty() || diff.is_empty() {
        return StrippedDiff {
            diff: diff.to_string(),
            omitted: Vec::new(),
        };
    }
    let mut out = String::with_capacity(diff.len());
    let mut omitted: Vec<String> = Vec::new();
    let mut chunk = String::new();
    let mut chunk_path: Option<String> = None;

    fn flush(
        out: &mut String,
        omitted: &mut Vec<String>,
        witness_paths: &[String],
        chunk: &mut String,
        chunk_path: &mut Option<String>,
    ) {
        if chunk.is_empty() {
            return;
        }
        let dropped = chunk_path
            .as_deref()
            .is_some_and(|path| witness_paths.iter().any(|w| w == path));
        if dropped {
            omitted.push(chunk_path.take().expect("dropped chunks carry a path"));
        } else {
            out.push_str(chunk);
        }
        chunk.clear();
        *chunk_path = None;
    }

    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            flush(
                &mut out,
                &mut omitted,
                witness_paths,
                &mut chunk,
                &mut chunk_path,
            );
        }
        // First `+++ ` per chunk only: the real new-file header lands right
        // after `--- `, before any content line can start with `+++ `.
        if chunk_path.is_none()
            && let Some(path) = line.strip_prefix("+++ ")
        {
            let path = path.strip_prefix("b/").unwrap_or(path).trim();
            if path != "/dev/null" {
                chunk_path = Some(path.to_string());
            }
        }
        chunk.push_str(line);
        chunk.push('\n');
    }
    flush(
        &mut out,
        &mut omitted,
        witness_paths,
        &mut chunk,
        &mut chunk_path,
    );
    StrippedDiff { diff: out, omitted }
}

/// Whether a standalone verifier pass is worth one revision spent demanding
/// corroboration (#1295) — the pure decision behind that branch of
/// `Pipeline::verify_candidate`, so it is directly testable and so the
/// pipeline module states only what it *does*.
///
/// Assumes the caller has already established the standalone pass; this
/// answers the second question only. Four conditions, and the interesting one
/// is `tracked_command`:
///
/// * the ask is enabled ([`crate::PipelineConfig::verifier_evidence_demand`]);
/// * it has not already been spent on this candidate — once is the cap,
///   because the second ask goes to a worker that has already answered;
/// * a revision remains in the same budget a real failure spends;
/// * **a tracked command exists.** The two facts that would clear
///   [`LadderInputs::verifier_pass_stands_alone`] — a fail→pass flip, and touched
///   tests green — are both observations of that command. With none resolved
///   neither is reachable, so the ask cannot be satisfied by any worker on any
///   turn and the turn it costs is pure loss. That is the shape the feature's
///   first measurement found and was reverted for (#1211 §1): on Terminal-Bench
///   the condition held on most turns *precisely because* most turns had no
///   command, and buying turns against a 900-second wall cost more tasks than
///   it recovered.
#[must_use]
pub fn evidence_demand_is_worth_a_turn(
    config: &crate::PipelineConfig,
    demands_spent: u32,
    revisions_spent: u32,
    tracked_command: Option<&str>,
) -> bool {
    config.verifier_evidence_demand
        && demands_spent == 0
        && revisions_spent < config.max_revisions
        && tracked_command.is_some()
}

/// The feedback a turn carries back to the WORKER when the ladder reached the
/// end of its deterministic channels without settling (#1295) — no flip, no
/// green test — and the run has a tracked command that could still carry that
/// evidence.
///
/// **It states only what was observed, and observation here is exhausted.**
/// Nothing reviewed this change: the model verdict was removed in #2584 and
/// [`stella_protocol::ModelCallRole::Verdict`] is unassignable, so the arm that
/// sends this text (`Pipeline::verify_candidate`'s
/// [`LadderDecision::Unverified`]) is reached only after every deterministic
/// channel has come back empty. An opening that told the worker its work "was
/// reviewed and looks correct" therefore asserted a reviewer that cannot
/// exist (#2619) — and asserted it in the worker's own context, where it reads
/// as an instruction not to re-examine the change at the exact moment nothing
/// backs it. That is the shape this module's docs argue against above: on
/// `fix-git` a reviewer's unsubstantiated claim made a worker reset `master`
/// and destroy a correctly-recovered commit, twice.
///
/// Not a verifier prompt: this text goes to the worker as a revision reason, so
/// it names the one thing the next turn has to produce rather than asking for
/// a verdict. The wording is deliberately narrow about what counts, because
/// the ladder is: a diff and a file touch prove the tree *changed* and are
/// already recorded — only `command` going green proves the change is right.
///
/// It also states the escape hatch. A worker that cannot make the command
/// observe its change should say so and stop rather than invent a test that
/// asserts nothing, which is the failure mode a bare "add a test" ask buys:
/// the run pays a turn *and* ends up with a tautological witness.
pub fn evidence_demand_prompt(command: &str) -> String {
    format!(
        "NOTHING deterministic backs up this change: \
         `{command}` has not gone from failing to passing, and no test that covers your change \
         has been observed green. That is not a finding that the work is wrong — it is that \
         nothing here can tell either way, and an unchecked change is not something to \
         finish on.\n\n\
         Spend this turn producing that evidence, and nothing else:\n\
         - Run `{command}` and make it pass over the change you already made.\n\
         - If it does not cover your change, extend it (or add a test it runs) so that it FAILS \
           without your change and PASSES with it. Check both directions.\n\
         - Do not rewrite working code to make a check easier, and do not add an assertion that \
           would pass either way.\n\n\
         If the change genuinely cannot be observed by `{command}` — no test surface exists for \
         it — say so in one line and stop. An honest \"unverified\" is the correct outcome there; \
         a test that cannot fail is worse than none."
    )
}

#[cfg(test)]
mod tests;
