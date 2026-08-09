// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The PROOF panel: whether this turn's work is proven, in one word and one
//! sentence, folded live from `AgentEvent::Proof` and the verdict that closes
//! it.
//!
//! # Why this is a panel and not transcript lines
//!
//! Every other narration in the TUI scrolls. Proof cannot: the interesting
//! question is never "what did the warrant say four screens ago", it is
//! *"right now, is this run proving what it is doing"* — a small state machine
//! whose current value matters and whose history does not. Rendered as
//! transcript rows the answer is buried under whatever the worker printed
//! since; rendered as a panel it is one glance, beside the work, the whole
//! time.
//!
//! # Why one [`Standing`] and not five rows
//!
//! This panel used to render the pipeline's five internal stages as five
//! labelled rows — `warrant`, `witness`, `oracle`, `tamper`, `verdict` — under
//! the title DONE VERIFICATION. Every one of those is a role name out of
//! `stella-pipeline`, and not one of them is a word the person reading the
//! screen would use. The mechanism was on screen; the answer was not.
//!
//! Three things followed, and together they are the case for this shape:
//!
//! 1. **The stage names crowded out the payoff.** The rail is ~38 columns and
//!    the label column took nine of them, so the row that finally said
//!    something — `✗ fails on base → ✓ passes on new` — is truncated to
//!    `✓ passes…` in this repository's own committed golden frame. The panel
//!    spent its width naming a stage and then cut off its result.
//! 2. **The rows contradicted the ladder, in the safe-looking direction.**
//!    `witness_intact`, `flip_refused` and `unstable_flip` were folded off the
//!    verdict ladder and then rendered by nothing at all, while the `oracle` row
//!    derived its own verdict from the raw `ProofStep::Oracle` stream. Those two
//!    readings disagree exactly where it costs the most: when the ladder refuses
//!    a flip because the passing run fixed a *different* failure (#867), the
//!    stream still shows baseline-fail then candidate-pass, so the panel painted
//!    a green `✗ fails on base → ✓ passes on new` over a flip the pipeline had
//!    already declined to credit. A false green on the one claim this product
//!    exists to make is the worst defect available to this surface.
//! 3. **Five stage names are not five facts.** Four of the five are steps
//!    toward a single claim. A reader wants the claim — and the reason, when it
//!    is not the good one.
//!
//! So the fold now produces exactly that: a [`Standing`] (one word, one tone)
//! and [`ProofState::explain`] (the plain sentences behind it). The stage
//! detail did not move to a smaller font; it moved to the traces view, which is
//! where a history belongs.
//!
//! # The invariant: a turn that ended never claims to still be working
//!
//! **Once a turn has ended, the standing is settled.** `checking` and `proving`
//! are promises, and a tool that leaves one up on a dead turn is silence
//! wearing the costume of progress.
//!
//! Holding that is a *design* problem, not a diligence problem. `run_candidate`
//! alone has eighteen terminal exits, and a rule of "every exit must remember to
//! report" is one refactor away from being false, and false invisibly. So the
//! invariant lives here, in the fold, in two halves:
//!
//! 1. **The plan is declared up front.** [`ProofStep::Assurance`] is emitted the
//!    moment triage decides, before any stage can fail or decline — so the panel
//!    carries a stated intent from the first second rather than an absence that
//!    only becomes meaningful in hindsight.
//! 2. **Unsettled plus finished is terminal.** [`ProofState::standing`] maps any
//!    unsettled standing to [`Standing::NeverReported`] once
//!    [`ProofState::finish`] has run. That is an honest and different claim from
//!    a spinner: the pipeline never said, and the panel says so rather than
//!    implying more is coming. It is now **one branch over the whole state
//!    machine** rather than a rule each of five rows had to remember
//!    individually, so a pipeline path nobody has written yet cannot regress it.
//!
//! `a_finished_turn_never_claims_to_still_be_working` proves it over arbitrary
//! event sequences rather than a handful of examples — the property IS the
//! contract.
//!
//! # Copy law (D6)
//!
//! The words on this surface are the reader's: *proved*, *not proved*, *not
//! needed*, *a test*, *this change*. Never `warrant`, `witness`, `oracle`,
//! `tamper`, `flip` or `verdict` — those name pipeline stages, and a stage name
//! on a user-facing surface is this panel's original defect, not its house
//! style.
//!
//! `no_pipeline_stage_name_reaches_the_panel` enforces it over arbitrary event
//! sequences, because the leak to catch is a future branch reaching for the
//! nearest available string — which, in this module, is always a stage name.
//!
//! Pure: this module folds and phrases, and hands plain strings to a renderer to
//! style. No ratatui types, so the fold is unit-testable without a terminal
//! (L-T1, and the buffer-not-ANSI discipline in `deck_ui::tests`).

use stella_protocol::{ProofStep, ProofTree, VerdictEvidence};

use crate::textline::Tone;

/// How the flip oracle stands: what the tracked command did against each of
/// the two code states a witness must span.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flip {
    /// The normalized command the oracle locked onto.
    pub command: Option<String>,
    /// What it did on the pre-execution tree; `None` until observed.
    pub baseline_passed: Option<bool>,
    /// What it did on the executed tree; `None` until observed.
    pub candidate_passed: Option<bool>,
    /// How many candidate replays have been observed so far. Counts
    /// observations, so it is honest even when the emitter carries no explicit
    /// `run` ordinal.
    pub candidate_runs: u32,
    /// How many passing candidate replays the flip requires, when the emitter
    /// stated one (`ProofStep::Oracle::runs_required`).
    pub runs_required: Option<u32>,
    /// The deterministic seed the replay pinned, when one was pinned.
    pub seed: Option<u64>,
}

impl Flip {
    /// Whether a genuine fail→pass flip has been observed across the two trees.
    /// Deliberately stricter than "the tests pass": a command that only ever
    /// passed proves the code reacts to nothing.
    pub fn achieved(&self) -> bool {
        self.baseline_passed == Some(false) && self.candidate_passed == Some(true)
    }
}

/// What the witness half of the proof came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WitnessStanding {
    /// An independent model wrote the failing test, and its bytes are pinned.
    Authored { path: String, fingerprint: String },
    /// One was warranted and could not be produced. The work stands; it is
    /// unproven, and the reason is shown rather than swallowed.
    Unavailable { reason: String },
}

/// The verdict that closes the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerdictStanding {
    pub passed: bool,
    /// `true` when the deterministic ladder decided it, `false` for a model
    /// verifier. Never conflated — the panel says which one spoke (L-E11).
    pub deterministic: bool,
    pub summary: String,
}

/// What triage decided this turn would buy, before any of it happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assurance {
    pub witness: bool,
    pub verifier: bool,
}

/// Where this turn's proof stands, in one word.
///
/// The variants are the reader's questions, not the pipeline's stages. Several
/// distinct mechanical outcomes collapse into [`Self::NotProved`] on purpose:
/// a test that was never written, one that passes without the change, one the
/// worker edited, and one that failed its confirmation re-run are four
/// mechanisms with one consequence — *this work is not proven* — and the
/// consequence is what belongs in the headline. Which of them happened is the
/// job of [`ProofState::explain`], one line below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Nothing established yet, and the turn is still running.
    Checking,
    /// Nothing was owed: this change has no behavior a test could pin down.
    NothingToProve,
    /// A test is owed and the machinery is working on it.
    Proving,
    /// A test fails without this change and passes with it.
    Proved,
    /// Something was owed and could not be established.
    NotProved,
    /// The verifier rejected the work.
    Rejected,
    /// Accepted — but by a model reading the change, not a test running against
    /// it. Deliberately not [`Self::Proved`]: "nothing objected" and "something
    /// was proven" are precisely the conflation this panel exists to break.
    Accepted,
    /// Every channel that could have checked this was blind, so the run cannot
    /// say either way. Distinct from [`Self::NotProved`], which is a finding.
    CouldNotCheck,
    /// The turn ended without the pipeline ever reporting. A gap in the tool,
    /// and it is shown as one.
    NeverReported,
}

impl Standing {
    /// Every variant, for the surfaces that must prove something about all of
    /// them. Hand-maintained; `every_variant_is_listed` makes the compiler the
    /// thing that notices when it falls behind the enum.
    pub const ALL: [Standing; 9] = [
        Self::Checking,
        Self::NothingToProve,
        Self::Proving,
        Self::Proved,
        Self::NotProved,
        Self::Rejected,
        Self::Accepted,
        Self::CouldNotCheck,
        Self::NeverReported,
    ];

    /// The word the panel title carries, completing the title's own sentence:
    /// `PROOF ✓ proved`, `PROOF ○ not needed`, `PROOF ⚠ can't check`.
    ///
    /// **These are budgeted, not merely chosen.** A ratatui block title clips
    /// with no ellipsis at all, and the rail is drawn as narrow as
    /// `plan_rail::RAIL_MIN_W`, so a word a few characters too long does not
    /// wrap or elide — it silently becomes a different word.
    /// `every_standing_fits_the_narrowest_rail` holds the ceiling, and it is the
    /// reason these read `not needed` rather than `nothing to prove`.
    pub fn word(self) -> &'static str {
        match self {
            Self::Checking => "checking",
            Self::NothingToProve => "not needed",
            Self::Proving => "proving",
            Self::Proved => "proved",
            Self::NotProved => "not proved",
            Self::Rejected => "rejected",
            Self::Accepted => "accepted",
            Self::CouldNotCheck => "can't check",
            Self::NeverReported => "no report",
        }
    }

    /// The tone the word and its light render in.
    pub fn tone(self) -> Tone {
        match self {
            Self::Proved => Tone::Success,
            Self::Rejected => Tone::Error,
            Self::NotProved | Self::CouldNotCheck | Self::NeverReported => Tone::Warn,
            // In flight, and "accepted but unproven" — both are states a reader
            // notes without acting on.
            Self::Proving | Self::Accepted => Tone::Info,
            Self::Checking | Self::NothingToProve => Tone::Muted,
        }
    }

    /// Whether this is an answer rather than a promise. The half of the
    /// module's invariant that [`ProofState::standing`] enforces.
    pub fn is_settled(self) -> bool {
        !matches!(self, Self::Checking | Self::Proving)
    }
}

/// The whole panel state for one turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProofState {
    /// The turn's declared plan, from triage. `None` before triage answers and
    /// on conversational turns, which buy no assurance because there is no work
    /// to assure.
    pub assurance: Option<Assurance>,
    /// `Some((required, reason, diff_lines))` once the warrant has read the
    /// diff. `reason` is populated only when no test is warranted.
    pub warrant: Option<(bool, Option<String>, u32)>,
    pub witness: Option<WitnessStanding>,
    pub flip: Flip,
    pub verdict: Option<VerdictStanding>,
    /// Set when the ladder abstained because every evidence channel was blind.
    /// Outranks the verdict: a turn nothing could observe must not read
    /// `proved` just because nothing failed it either.
    pub unverifiable: Option<String>,
    /// The witness-tamper check's stated result, from the verdict's ladder
    /// snapshot: `Some(true)` = every witness artifact matched its pinned
    /// identity. `None` = the check never ran or the verdict predates the
    /// snapshot.
    pub witness_intact: Option<bool>,
    /// A would-be flip was refused because the pass demonstrably fixed a
    /// *different* failure.
    pub flip_refused: bool,
    /// A flip was observed but its confirmation re-run did not pass.
    pub unstable_flip: bool,
    /// Whether the turn has ended. Settles an in-flight standing into
    /// [`Standing::NeverReported`] — see the module docs; this is the half of
    /// the invariant that does not depend on any pipeline path cooperating.
    finished: bool,
}

impl ProofState {
    // There is deliberately no `is_empty`. One existed, for surfaces that would
    // rather hide than show a panel about a turn that never reaches
    // verification — and it had no callers, because the panel became
    // unconditional and "nothing observed yet" now has a word of its own
    // (`checking`). A predicate whose only remaining purpose is to be available
    // is a second answer to a question this module already answers once.

    /// The turn ended. Anything still in flight becomes *never reported* — a
    /// statement, where a spinner would be a promise the turn cannot keep.
    ///
    /// Deliberately not `apply`-driven off a proof step: the backstop has to
    /// hold for turns that emitted no proof steps at all, including ones that
    /// aborted before the pipeline reached any of them.
    pub fn finish(&mut self) {
        self.finished = true;
    }

    /// Fold one proof step.
    pub fn apply(&mut self, step: &ProofStep) {
        match step {
            ProofStep::Assurance { witness, verifier } => {
                self.assurance = Some(Assurance {
                    witness: *witness,
                    verifier: *verifier,
                });
            }
            ProofStep::Warrant {
                required,
                reason,
                diff_lines,
            } => self.warrant = Some((*required, reason.clone(), *diff_lines)),
            ProofStep::WitnessAuthored {
                path,
                command,
                fingerprint,
            } => {
                self.witness = Some(WitnessStanding::Authored {
                    path: path.clone(),
                    fingerprint: fingerprint.clone(),
                });
                self.flip.command = Some(command.clone());
            }
            ProofStep::WitnessUnavailable { reason } => {
                self.witness = Some(WitnessStanding::Unavailable {
                    reason: reason.clone(),
                });
            }
            ProofStep::VerificationUnavailable { reason } => {
                self.unverifiable = Some(reason.clone());
            }
            ProofStep::Oracle {
                command,
                passed,
                tree,
                run,
                runs_required,
                seed,
            } => {
                self.flip.command = Some(command.clone());
                if let Some(required) = runs_required {
                    self.flip.runs_required = Some(*required);
                }
                if let Some(seed) = seed {
                    self.flip.seed = Some(*seed);
                }
                match tree {
                    ProofTree::Baseline => self.flip.baseline_passed = Some(*passed),
                    ProofTree::Candidate => {
                        self.flip.candidate_passed = Some(*passed);
                        // The stated ordinal wins when present (a re-emitted
                        // observation must not double-count); otherwise each
                        // candidate observation is one more run.
                        self.flip.candidate_runs = match run {
                            Some(n) => (*n).max(self.flip.candidate_runs),
                            None => self.flip.candidate_runs + 1,
                        };
                    }
                }
            }
            // Per-candidate provenance for the traces view and replay (#1787);
            // the winning verdict's own summary already states any heuristic
            // degradation, so no standing folds from it here.
            ProofStep::VerdictDegraded { .. } => {}
            // Provenance for the traces view and the event stream (#2414), like
            // `VerdictDegraded` above. The panel states what assurance the turn
            // bought, and a triage that fell to the keyword floor still declares
            // its `Assurance` plan — what degraded is *where the plan came
            // from*, which is a question for the trace, not the headline.
            ProofStep::TriageDegraded { .. } => {}
        }
    }

    /// Fold the verdict that closes the turn — and the ladder facts riding it:
    /// tamper exclusion, and a refused or unstable flip.
    pub fn apply_verdict(&mut self, passed: bool, evidence: &VerdictEvidence) {
        self.verdict = Some(VerdictStanding {
            passed,
            deterministic: evidence.deterministic,
            summary: evidence.summary.clone(),
        });
        if let Some(ladder) = &evidence.ladder {
            self.witness_intact = ladder.witness_intact;
            self.flip_refused = ladder.flip_refused_different_failure;
            self.unstable_flip = ladder.unstable_flip;
        }
    }

    /// Where the proof stands, in one word.
    ///
    /// Every row resolves: `checking` and `proving` only while the turn runs,
    /// and never once [`Self::finish`] has been called — see the module docs.
    pub fn standing(&self) -> Standing {
        let live = self.live_standing();
        // The invariant, in one branch over the whole state machine: a turn
        // that has ended cannot still be working on it.
        if self.finished && !live.is_settled() {
            return Standing::NeverReported;
        }
        live
    }

    /// The plain sentences behind [`Self::standing`], most important first.
    ///
    /// Empty only for [`Standing::Checking`], where there is genuinely nothing
    /// to say yet and the title already says it. Every settled standing
    /// explains itself — asserted as a property, because a headline with no
    /// reason under it is the failure this panel replaced.
    pub fn explain(&self) -> Vec<String> {
        match self.standing() {
            Standing::Checking => Vec::new(),
            Standing::NothingToProve => vec![self.nothing_owed_because()],
            Standing::Proving => vec![self.proving_because()],
            Standing::Proved => self.proved_because(),
            Standing::NotProved => vec![self.not_proved_because()],
            Standing::Rejected => self.rejected_because(),
            Standing::Accepted => self.accepted_because(),
            Standing::CouldNotCheck => vec![
                self.unverifiable
                    .clone()
                    .unwrap_or_else(|| "there was no way to check this change".into()),
            ],
            Standing::NeverReported => {
                vec!["the run ended without saying whether it proved anything".into()]
            }
        }
    }

    /// The standing before the finished-turn backstop. Ordered by what a reader
    /// most needs to know, **not** by pipeline order: a warranted-but-unproven
    /// turn outranks a passing verdict, because the verdict on such a turn is
    /// precisely the thing that must not be read alone.
    fn live_standing(&self) -> Standing {
        // Nothing could observe this turn. Checked first, and before the
        // verdict, because the pipeline emits BOTH: an abstention still closes
        // the turn with a `passed: true` verdict — a run is not failed by the
        // absence of a way to check it — and read off the verdict alone the
        // panel would say `proved`.
        if self.unverifiable.is_some() {
            return Standing::CouldNotCheck;
        }
        // The worker edited the very test meant to judge it. Outranks
        // everything below including a green flip, because a flip observed on a
        // tampered artifact is not evidence at all.
        //
        // Defensive today and deliberately kept: `LadderSnapshot::witness_intact`
        // documents that `Some(false)` never reaches a verdict, because tamper
        // aborts the candidate before one is issued. So this branch encodes what
        // the panel must say *if* that ever changes, rather than leaving the
        // most dangerous value in the type to fall through to `Proved`.
        if self.witness_intact == Some(false) {
            return Standing::NotProved;
        }
        if matches!(self.witness, Some(WitnessStanding::Unavailable { .. })) {
            return Standing::NotProved;
        }
        if let Some(v) = &self.verdict
            && !v.passed
        {
            return Standing::Rejected;
        }
        // A flip the ladder refused, or one that did not survive its
        // confirmation re-run. Both look like a pass from the oracle's side and
        // are not one.
        if self.flip_refused || self.unstable_flip {
            return Standing::NotProved;
        }
        if self.flip.achieved() {
            return Standing::Proved;
        }
        // A test already green on the untouched tree reacts to nothing; a test
        // still red on the changed one has not been made to pass; a pass with
        // no baseline observation spans nothing. None of the three is a proof.
        if self.flip.baseline_passed == Some(true)
            || self.flip.candidate_passed == Some(false)
            || (self.flip.baseline_passed.is_none() && self.flip.candidate_passed.is_some())
        {
            return Standing::NotProved;
        }
        if let Some(v) = &self.verdict {
            return if v.deterministic {
                Standing::Proved
            } else {
                Standing::Accepted
            };
        }
        if self.bought_nothing() || self.warrant_says_not_required() {
            return Standing::NothingToProve;
        }
        if self.witness.is_some() || self.warrant_says_required() {
            return Standing::Proving;
        }
        Standing::Checking
    }

    /// Triage declared this turn buys neither a test nor a verifier, *and*
    /// nothing arrived regardless.
    ///
    /// Both halves are load-bearing. The first alone would let a turn that
    /// bought nothing but then failed a verdict anyway suppress the failure;
    /// requiring that every channel is also empty means anything that actually
    /// happened is still judged on its own.
    fn bought_nothing(&self) -> bool {
        self.assurance.is_some_and(|a| !a.witness && !a.verifier)
            && self.witness.is_none()
            && self.verdict.is_none()
            && self.unverifiable.is_none()
            && self.flip == Flip::default()
    }

    fn nothing_owed_because(&self) -> String {
        // The warrant reads the diff and states its reason as a sentence, so
        // when it has one it is already better than anything phrased here.
        if let Some((false, Some(reason), _)) = &self.warrant {
            return reason.clone();
        }
        if self.warrant_says_not_required() {
            return "nothing in this change alters how the code behaves".into();
        }
        "this turn changed no behavior a test could pin down".into()
    }

    fn proving_because(&self) -> String {
        match (self.flip.baseline_passed, self.flip.candidate_passed) {
            (Some(false), None) => {
                "a test fails without this change — running it with the change now".into()
            }
            _ if self.witness.is_some() => {
                "wrote a test that should fail without this change".into()
            }
            _ => "this change needs a test to prove it".into(),
        }
    }

    fn proved_because(&self) -> Vec<String> {
        // A deterministic verdict with no flip behind it (a configured
        // `--test-command`, say) proved the work some other way, and its own
        // summary is the honest account of how.
        if !self.flip.achieved() {
            return self
                .verdict
                .as_ref()
                .map(|v| vec![v.summary.clone()])
                .unwrap_or_default();
        }
        let mut lines = vec!["a test fails without this change and passes with it".to_string()];
        if let Some(WitnessStanding::Authored { path, .. }) = &self.witness {
            lines.push(path.clone());
        }
        // Only when the replay policy asked for more than one pass: a lone run
        // is the ordinary case and saying "run 1 of 1" is noise.
        if let Some(required) = self.flip.runs_required
            && required > 1
        {
            lines.push(format!(
                "confirmed on {} of {required} runs",
                self.flip.candidate_runs
            ));
        }
        lines
    }

    /// The several distinct ways a turn arrives at *not proved*, in the same
    /// precedence [`Self::live_standing`] used to get there — so the headline
    /// and the reason under it can never disagree about which one happened.
    fn not_proved_because(&self) -> String {
        if self.witness_intact == Some(false) {
            return "the change edited its own test, so the test proves nothing".into();
        }
        if let Some(WitnessStanding::Unavailable { reason }) = &self.witness {
            return format!("no test could be written — {reason}");
        }
        if self.flip_refused {
            return "the test passes now, but it was fixing a different failure".into();
        }
        if self.unstable_flip {
            return "the test passed once, then failed when re-run to confirm".into();
        }
        if self.flip.baseline_passed == Some(true) {
            return "the test passes even without this change, so it proves nothing".into();
        }
        if self.flip.candidate_passed == Some(false) {
            return "the test still fails with this change".into();
        }
        "the test passes, but it was never run without the change".into()
    }

    fn rejected_because(&self) -> Vec<String> {
        let Some(v) = &self.verdict else {
            return Vec::new();
        };
        let mut lines = vec![v.summary.clone()];
        // L-E11: who decided is never left to inference. A model's rejection
        // and a failing test are different kinds of news.
        if !v.deterministic {
            lines.push("a model reviewed this — no test ran".into());
        }
        lines
    }

    fn accepted_because(&self) -> Vec<String> {
        let mut lines = vec!["a model reviewed this change — no test ran".to_string()];
        if let Some(v) = &self.verdict
            && !v.summary.is_empty()
        {
            lines.push(v.summary.clone());
        }
        lines
    }

    fn warrant_says_not_required(&self) -> bool {
        matches!(self.warrant, Some((false, _, _)))
    }

    fn warrant_says_required(&self) -> bool {
        matches!(self.warrant, Some((true, _, _)))
    }
}

/// One-line trace summary of a [`stella_protocol::ProofStep`] for the traces
/// view — the scrolling counterpart to the panel this module folds: the panel
/// answers "is this run proving what it is doing *now*", a trace row records
/// what each step said when it happened.
///
/// This is the surface that keeps the pipeline's own vocabulary, deliberately:
/// a trace is read by someone debugging the pipeline, for whom `oracle` and
/// `warrant` are the precise words, and it is where the stage detail the panel
/// stopped rendering now lives.
pub(crate) fn proof_trace(step: &stella_protocol::ProofStep) -> String {
    use stella_protocol::{ProofStep, ProofTree};
    match step {
        ProofStep::Assurance { witness, verifier } => format!(
            "assurance: witness {}, verifier {}",
            if *witness { "on" } else { "waived" },
            if *verifier { "on" } else { "waived" }
        ),
        ProofStep::Warrant {
            required: true,
            diff_lines,
            ..
        } => format!("warrant: required ({diff_lines} lines)"),
        ProofStep::Warrant { reason, .. } => format!(
            "warrant: {}",
            reason.as_deref().unwrap_or("no test warranted")
        ),
        ProofStep::WitnessAuthored { path, .. } => format!("witness authored: {path}"),
        ProofStep::WitnessUnavailable { reason } => format!("witness unavailable: {reason}"),
        ProofStep::VerificationUnavailable { reason } => {
            format!("verification unavailable: {reason}")
        }
        ProofStep::Oracle { passed, tree, .. } => format!(
            "oracle: {} on {}",
            if *passed { "pass" } else { "fail" },
            match tree {
                ProofTree::Baseline => "base",
                ProofTree::Candidate => "new",
            }
        ),
        ProofStep::VerdictDegraded { candidate, reason } => {
            format!("verdict degraded (candidate {candidate}): {reason}")
        }
        ProofStep::TriageDegraded { reason } => format!("triage degraded: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use stella_protocol::LadderSnapshot;

    fn authored() -> ProofStep {
        ProofStep::WitnessAuthored {
            path: "tests/clear_reset.rs".into(),
            command: "cargo test clear_reset".into(),
            fingerprint: "sha256:9f3c1d2e4a5b6c7d8e9f".into(),
        }
    }

    fn oracle(passed: bool, tree: ProofTree) -> ProofStep {
        ProofStep::Oracle {
            command: "cargo test clear_reset".into(),
            passed,
            tree,
            run: None,
            runs_required: None,
            seed: None,
        }
    }

    /// A ladder snapshot with every finding clear — the base the tamper /
    /// refused / unstable tests each flip one field of.
    fn clean_ladder() -> LadderSnapshot {
        LadderSnapshot {
            rung: None,
            tracked_command: None,
            oracle_trace: Vec::new(),
            flip_achieved: true,
            unstable_flip: false,
            flip_refused_different_failure: false,
            touched_tests_passed: None,
            test_infra: None,
            diff_lines: 0,
            diff_budget: 0,
            diff_available: false,
            file_change_events: 0,
            mutating_actions: 0,
            new_diag_errors: 0,
            new_diag_warnings: 0,
            witness_intact: None,
            witness_mutation: None,
            diff_coverage: None,
            verify_done_flip: false,
            no_test_surface: false,
            errored_commands: 0,
            verifier_independent: None,
        }
    }

    /// A passing verdict carrying a ladder snapshot — the shape whose findings
    /// used to be folded and then rendered by nothing.
    fn passed_with(ladder: LadderSnapshot) -> VerdictEvidence {
        VerdictEvidence {
            summary: "the change looks correct".into(),
            deterministic: true,
            evidence_refs: vec![],
            ladder: Some(Box::new(ladder)),
        }
    }

    /// The event shape a refused or unstable flip arrives in: the oracle stream
    /// says baseline-fail then candidate-pass either way, and only the ladder
    /// snapshot riding the verdict knows better.
    fn green_flip() -> ProofState {
        let mut state = ProofState::default();
        state.apply(&authored());
        state.apply(&oracle(false, ProofTree::Baseline));
        state.apply(&oracle(true, ProofTree::Candidate));
        state
    }

    /// [`Standing::ALL`] is hand-maintained, and the exhaustive match below is
    /// what makes adding a variant a compile error here rather than a silent
    /// hole in every property that iterates it.
    #[test]
    fn every_variant_is_listed() {
        for standing in Standing::ALL {
            match standing {
                Standing::Checking
                | Standing::NothingToProve
                | Standing::Proving
                | Standing::Proved
                | Standing::NotProved
                | Standing::Rejected
                | Standing::Accepted
                | Standing::CouldNotCheck
                | Standing::NeverReported => {}
            }
        }
    }

    #[test]
    fn a_fresh_state_has_nothing_to_report() {
        let state = ProofState::default();
        assert_eq!(state.standing(), Standing::Checking);
        assert!(state.explain().is_empty(), "no headline needs no reason");
    }

    #[test]
    fn a_warranted_turn_reads_as_proving_and_says_what_it_needs() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Warrant {
            required: true,
            reason: None,
            diff_lines: 41,
        });
        assert_eq!(state.standing(), Standing::Proving);
        assert_eq!(
            state.explain(),
            vec!["this change needs a test to prove it"]
        );
    }

    #[test]
    fn a_flip_reads_as_proved_and_names_the_test() {
        let mut state = ProofState::default();
        state.apply(&authored());
        state.apply(&oracle(false, ProofTree::Baseline));
        assert_eq!(state.standing(), Standing::Proving, "still mid-flight");
        assert!(state.explain()[0].contains("running it with the change"));

        state.apply(&oracle(true, ProofTree::Candidate));
        assert_eq!(state.standing(), Standing::Proved);
        assert_eq!(state.standing().tone(), Tone::Success);
        let why = state.explain();
        assert_eq!(
            why[0],
            "a test fails without this change and passes with it"
        );
        assert_eq!(why[1], "tests/clear_reset.rs");
    }

    /// A test already green on the untouched tree proves the code reacts to
    /// nothing — the panel must not dress that as a pass.
    #[test]
    fn a_test_green_on_the_baseline_is_not_a_flip() {
        let mut state = ProofState::default();
        state.apply(&oracle(true, ProofTree::Baseline));
        state.apply(&oracle(true, ProofTree::Candidate));
        assert_eq!(state.standing(), Standing::NotProved);
        assert!(state.explain()[0].contains("even without this change"));
    }

    // ---- the ladder findings the five-row panel folded and never rendered ----

    /// **The witness for this change.** The oracle stream says baseline-fail →
    /// candidate-pass, so the old panel painted a green
    /// `✗ fails on base → ✓ passes on new` — while the ladder riding the same
    /// verdict had already refused that flip, because the passing run fixed a
    /// *different* failure than the one the baseline showed (#867). The panel
    /// claimed a proof the pipeline declined to credit.
    #[test]
    fn a_flip_the_ladder_refused_is_never_shown_as_proved() {
        let mut state = green_flip();
        assert_eq!(
            state.standing(),
            Standing::Proved,
            "the raw oracle stream alone reads as a clean flip"
        );

        state.apply_verdict(
            true,
            &passed_with(LadderSnapshot {
                flip_achieved: false,
                flip_refused_different_failure: true,
                ..clean_ladder()
            }),
        );
        assert_eq!(
            state.standing(),
            Standing::NotProved,
            "the ladder's refusal outranks the stream it contradicts"
        );
        assert_eq!(state.standing().tone(), Tone::Warn);
        assert!(
            state.explain()[0].contains("different failure"),
            "{:?}",
            state.explain()
        );
    }

    /// The same shape for a flip that did not survive its confirmation re-run.
    /// The old panel could describe this only as a test that never passed,
    /// which is a different — and less alarming — event than one that passed
    /// and then stopped.
    #[test]
    fn an_unstable_flip_says_the_confirmation_run_failed() {
        let mut state = green_flip();
        state.apply_verdict(
            true,
            &passed_with(LadderSnapshot {
                flip_achieved: false,
                unstable_flip: true,
                ..clean_ladder()
            }),
        );
        assert_eq!(state.standing(), Standing::NotProved);
        assert!(
            state.explain()[0].contains("re-run to confirm"),
            "{:?}",
            state.explain()
        );
    }

    /// Defensive, and stated as such: the protocol documents that a tampered
    /// witness aborts its candidate before any verdict is issued, so this value
    /// does not arrive today. The test pins what the panel would say if it ever
    /// did — the alternative is leaving the most dangerous value in the type
    /// falling through to `proved`.
    #[test]
    fn a_change_that_edited_its_own_test_would_not_be_proved() {
        let mut state = green_flip();
        state.apply_verdict(
            true,
            &passed_with(LadderSnapshot {
                witness_intact: Some(false),
                ..clean_ladder()
            }),
        );
        assert_eq!(state.standing(), Standing::NotProved);
        assert!(
            state.explain()[0].contains("edited its own test"),
            "{:?}",
            state.explain()
        );
    }

    // ---- the rest of the state machine ----

    /// A warranted test that could not be produced is the one standing that
    /// must shout: the work is finished and NOT proven, and the reason reaches
    /// the panel rather than being swallowed.
    #[test]
    fn a_test_that_could_not_be_written_states_its_reason() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Warrant {
            required: true,
            reason: None,
            diff_lines: 12,
        });
        state.apply(&ProofStep::WitnessUnavailable {
            reason: "no author independent of the worker".into(),
        });
        assert_eq!(state.standing(), Standing::NotProved);
        assert_eq!(
            state.explain()[0],
            "no test could be written — no author independent of the worker"
        );
    }

    #[test]
    fn a_model_pass_is_accepted_and_never_reads_as_proved() {
        let mut state = ProofState::default();
        state.apply_verdict(
            true,
            &VerdictEvidence {
                summary: "looks right".into(),
                deterministic: false,
                evidence_refs: vec![],
                ladder: None,
            },
        );
        assert_eq!(state.standing(), Standing::Accepted);
        assert_eq!(state.standing().tone(), Tone::Info);
        assert!(state.explain()[0].contains("no test ran"));
    }

    #[test]
    fn a_change_with_nothing_to_prove_says_why_in_the_warrants_own_words() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Warrant {
            required: false,
            reason: Some("documentation only; prose has no runtime behavior to flip".into()),
            diff_lines: 4,
        });
        assert_eq!(state.standing(), Standing::NothingToProve);
        assert_eq!(state.standing().tone(), Tone::Muted);
        assert_eq!(
            state.explain()[0],
            "documentation only; prose has no runtime behavior to flip"
        );
    }

    /// The regression that started the panel's redesign: triage waives the
    /// test, nothing downstream is emitted, and the old row hung on `pending`
    /// for the whole turn and then forever. It must say so immediately —
    /// before the turn ends, not only after.
    #[test]
    fn a_triage_waived_turn_says_so_while_it_is_still_running() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Assurance {
            witness: false,
            verifier: false,
        });
        assert_eq!(state.standing(), Standing::NothingToProve);
        assert!(!state.explain().is_empty());
        // And nothing about it turns into a false promise at the end.
        state.finish();
        assert_eq!(state.standing(), Standing::NothingToProve);
    }

    /// A turn that dies before reporting must still say what it does not know.
    #[test]
    fn a_turn_that_reported_nothing_says_it_reported_nothing() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Assurance {
            witness: true,
            verifier: true,
        });
        state.finish();
        assert_eq!(state.standing(), Standing::NeverReported);
        assert_eq!(
            state.standing().tone(),
            Tone::Warn,
            "a reporting gap looks like a gap"
        );
        assert!(state.explain()[0].contains("without saying"));
    }

    #[test]
    fn an_unobservable_turn_outranks_the_passing_verdict_that_closes_it() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::VerificationUnavailable {
            reason: "every evidence channel was blind".into(),
        });
        state.apply_verdict(
            true,
            &VerdictEvidence {
                summary: "nothing objected".into(),
                deterministic: true,
                evidence_refs: vec![],
                ladder: None,
            },
        );
        assert_eq!(state.standing(), Standing::CouldNotCheck);
        assert_eq!(state.explain()[0], "every evidence channel was blind");
    }

    // ---- the invariants ----

    /// Every step shape the protocol can produce, as a proptest strategy. New
    /// `ProofStep` variants must be added here — an invariant is only worth
    /// what its input space covers.
    fn any_step() -> impl Strategy<Value = ProofStep> {
        prop_oneof![
            (any::<bool>(), any::<bool>())
                .prop_map(|(witness, verifier)| ProofStep::Assurance { witness, verifier }),
            (any::<bool>(), any::<Option<String>>(), any::<u32>()).prop_map(
                |(required, reason, diff_lines)| ProofStep::Warrant {
                    required,
                    reason,
                    diff_lines,
                }
            ),
            (any::<String>(), any::<String>(), any::<String>()).prop_map(
                |(path, command, fingerprint)| ProofStep::WitnessAuthored {
                    path,
                    command,
                    fingerprint,
                }
            ),
            any::<String>().prop_map(|reason| ProofStep::WitnessUnavailable { reason }),
            (any::<String>(), any::<bool>(), any::<bool>()).prop_map(
                |(command, passed, baseline)| ProofStep::Oracle {
                    command,
                    passed,
                    tree: if baseline {
                        ProofTree::Baseline
                    } else {
                        ProofTree::Candidate
                    },
                    run: None,
                    runs_required: None,
                    seed: None,
                }
            ),
        ]
    }

    proptest! {
        /// **The invariant.** However a turn went — no steps at all, steps in
        /// any order, contradictory steps, an abort partway — once it has
        /// ended, the panel states an outcome rather than a promise.
        ///
        /// A property rather than examples because the failure this guards is
        /// a *path nobody thought of*: the original bug was triage waiving the
        /// test, a case that emitted no steps and so appeared in no
        /// example-based test. Enumerating states by hand is how it was missed
        /// the first time.
        #[test]
        fn a_finished_turn_never_claims_to_still_be_working(
            steps in prop::collection::vec(any_step(), 0..8)
        ) {
            let mut state = ProofState::default();
            for step in &steps {
                state.apply(step);
            }
            state.finish();
            prop_assert!(
                state.standing().is_settled(),
                "still `{}` after the turn ended (steps: {:?})",
                state.standing().word(),
                steps
            );
        }

        /// A headline with no reason under it is the failure this panel
        /// replaced, so every settled standing explains itself. `Checking` is
        /// the sole exception, and only because it is unreachable once the turn
        /// is over.
        #[test]
        fn every_settled_standing_explains_itself(
            steps in prop::collection::vec(any_step(), 0..8),
            finished: bool
        ) {
            let mut state = ProofState::default();
            for step in &steps {
                state.apply(step);
            }
            if finished {
                state.finish();
            }
            let standing = state.standing();
            if standing != Standing::Checking {
                prop_assert!(
                    !state.explain().is_empty(),
                    "`{}` with no reason under it (steps: {:?})",
                    standing.word(),
                    steps
                );
            }
        }

        /// The words are the reader's, not the pipeline's (D6). Asserted over
        /// arbitrary event sequences because the leak this catches is a future
        /// branch that reaches for the nearest available string — which, in
        /// this module, is always a stage name.
        #[test]
        fn no_pipeline_stage_name_reaches_the_panel(
            steps in prop::collection::vec(any_step(), 0..8),
            finished: bool
        ) {
            let mut state = ProofState::default();
            for step in &steps {
                state.apply(step);
            }
            if finished {
                state.finish();
            }
            // The warrant's own reason is model-authored prose passed through
            // verbatim, so it is not this module's word choice to police.
            let authored_reason = matches!(&state.warrant, Some((false, Some(_), _)));
            let mut phrased = state.standing().word().to_string();
            if !authored_reason {
                phrased.push_str(&state.explain().join(" "));
            }
            for banned in ["warrant", "witness", "oracle", "tamper", "verdict", "flip"] {
                prop_assert!(
                    !phrased.to_lowercase().contains(banned),
                    "`{banned}` reached the panel in {phrased:?}"
                );
            }
        }
    }
}
