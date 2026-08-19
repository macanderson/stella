//! The `[wrapper]` block — a turn-loop wrapper's stage order, declared.
//!
//! Slice of #3381. The conditional stage order the built-in staged pipeline
//! ran was hardcoded branches inside `crates/stella-pipeline/src/pipeline.rs`,
//! a file on the god-file list and closed to growth — so that design could not
//! absorb another variant even if we wanted one. A wrapper plugin declares the
//! order instead, and the conditions become a line you can read and change.
//! The crate those branches lived in has since been deleted from the
//! workspace (#3865), which leaves this the only place a stage order is
//! expressed at all.
//!
//! Two rules carried from #3245 slice A, not re-derived here:
//!
//! - An unknown key is a **load error** (every table denies unknown fields).
//! - A condition naming a signal the host does not publish is **also a load
//!   error**. A manifest that quietly does nothing is worse than one that
//!   refuses to load.
//!
//! What this module adds is the machinery that makes the second rule
//! mechanically checkable rather than aspirational (`doc:turn-loop-wrappers`
//! §9.4). The `if` field is a **closed** grammar over a **published** signal
//! set, evaluated by a pure function — never an expression language. A
//! Turing-complete condition in a manifest is a second program with no gate on
//! it.
//!
//! It also load-checks the stage graph: a condition reading a signal that some
//! *later* stage publishes is rejected at load, and so is one reading a signal
//! whose publisher is declared earlier but **conditional** — that stage
//! produces nothing on the turns it is skipped. So the failure mode of a
//! hand-written variant is a rejection with a reason instead of a wedged run at
//! round three. [`crate::program`] is what makes the second rule necessary and
//! what proves the pair sufficient: a manifest that loads resolves for every
//! possible set of signal values.
//!
//! # Scope — what this module deliberately does not do
//!
//! Nothing here binds a stage to the loop. The four wrapper interception
//! points (`before_turn` / `after_turn` / `judge` / `again?`) are #3380 and do
//! not exist yet; until they do, a [`StageName`] is a declared name that
//! load-checks, not a dispatch target. This crate is a leaf by contract — pure
//! parsing and validation over borrowed text — so it can be complete and
//! correct ahead of the socket it will eventually describe.

use serde::{Deserialize, Serialize};
use stella_protocol::StageKind;

use crate::error::ManifestError;

/// The `[wrapper]` block — an ordered, conditional stage list under one id.
///
/// The id is what the store's `pipeline_variant` column records (#3388), so
/// it is the join key of the whole A/B setup: cost, outcome, and
/// verified-versus-unverified rate per variant is one `GROUP BY` once the
/// wrapper that ran wrote its id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wrapper {
    /// The variant id — `"staged-v1"` in #3381's sketch, `"classic"` for the
    /// order that ships today. Non-empty, and written to the execution row
    /// **only when this manifest was the thing that ran**: a default or
    /// fallback path writes the default's id, never a blank, because a
    /// missing measurement must not render as a negative one.
    pub id: String,
    /// The stages, in execution order. Non-empty, no duplicates.
    #[serde(default)]
    pub stages: Vec<WrapperStage>,
}

/// One `[[wrapper.stages]]` entry — a stage name and the condition under
/// which it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WrapperStage {
    /// Which stage. A closed vocabulary: an unknown name is a load error,
    /// because a stage the host cannot dispatch is exactly the manifest that
    /// quietly does nothing.
    pub name: StageName,
    /// The condition under which this stage runs; absent means
    /// unconditional.
    ///
    /// Kept as the author's own text so the manifest round-trips
    /// byte-for-byte (invariant 4) while [`WrapperStage::condition`] hands
    /// back the parsed form. A value that came from
    /// [`PluginManifest::from_toml_str`] has already had this parsed and
    /// graph-checked, so that accessor cannot fail for such a value.
    ///
    /// [`PluginManifest::from_toml_str`]: crate::PluginManifest::from_toml_str
    #[serde(rename = "if", default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

impl WrapperStage {
    /// The parsed condition, or `None` for an unconditional stage.
    ///
    /// Infallible in the sense that matters: validation already rejected any
    /// manifest whose condition text does not parse, so a stage reached
    /// through a validated [`Wrapper`] always answers `Ok`.
    ///
    /// # Errors
    ///
    /// Returns the same [`ManifestError`] validation would, for a
    /// hand-constructed value that never went through the constructor.
    pub fn condition(&self) -> Result<Option<Condition>, ManifestError> {
        match &self.condition {
            None => Ok(None),
            Some(text) => Condition::parse(self.name, text).map(Some),
        }
    }
}

/// A stage the host can dispatch.
///
/// Closed by design, the [`FlipPolicy`] reasoning applied to stages: an
/// unknown value must be a load error rather than a silently shorter run, and
/// a future stage adds a variant here. The names and their order were taken
/// from `stage_rank` in `crates/stella-pipeline/src/replay.rs`, which was the
/// canonical ordering when this was written; that crate was deleted from the
/// workspace (#3865), so this enum is now the canonical ordering itself.
/// [`StageName::kind`] keeps it tied to [`StageKind`]'s twelve mechanically
/// rather than as a claim, since it is one-to-one onto them.
///
/// **A name here is not a promise that every host runs it.** The vocabulary
/// mirrors [`StageKind`] because a wrapper that cannot spell a boundary
/// cannot describe the run it wraps; which hosts emit which boundary today
/// differs per stage, and each variant below says so rather than leaving a
/// manifest author to discover it from a run that quietly did nothing.
///
/// [`FlipPolicy`]: crate::FlipPolicy
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StageName {
    /// Classify the task and name what must be researched.
    Triage,
    /// Pull prior context for the goal.
    Recall,
    /// Answer triage's questions with read-only sub-agents.
    Research,
    /// Turn the goal into steps.
    Plan,
    /// Review the plan's scope before spending on it.
    Scope,
    /// Run the turn.
    Execute,
    /// Author the failing test that will measure the turn.
    Witness,
    /// Turn evidence into a verdict.
    Verify,
    /// The verdict boundary — and **a staged run does not dispatch it**,
    /// which is the one thing a manifest author has to know before writing the
    /// name down.
    ///
    /// The built-in staged pipeline emitted no [`StageKind::Verdict`] at all:
    /// its verify stage emitted the `Verdict` *event* directly from
    /// `ladder_decision`, and `crates/stella-pipeline/src/pipeline/tests.rs`
    /// asserted the stage never appeared in that stream. That crate has since
    /// been deleted (#3865), and the shape carries over to a verification
    /// plugin that ports the same ladder (`doc:pipeline-as-plugins` §8). The
    /// hosts that do emit the boundary are the goal loops —
    /// `crates/stella-cli/src/agent/goal.rs` and
    /// `crates/stella-serve/src/goal.rs` run-scoped,
    /// `crates/stella-core/src/goal.rs` turn-scoped — so a wrapper naming
    /// `verdict` describes a goal-loop run and not a staged one.
    Verdict,
    /// Post-verdict self-reflection: mine the finished turn for lessons and
    /// record them for later recall.
    ///
    /// Emitted run-scoped today by `crates/stella-cli/src/agent.rs`, gated
    /// there on the turn having done real work — the branch
    /// [`Signal::MutatingActions`] transcribes.
    Reflect,
    /// Context write-back: episode summaries and fact upserts landing in the
    /// context plane.
    ///
    /// A **declared gap**: the work happens (`crates/stella-cli/src/agent.rs`
    /// records the episode under the same "did real work" gate as reflection)
    /// but no host emits this boundary today, so nothing yet reports it.
    /// Declarable so a wrapper can order the write-back it will drive once
    /// the socket exists (#3380), and named as a gap here rather than
    /// discovered as a silence.
    ContextWrite,
    /// The run is done — the last boundary a run emits.
    ///
    /// Emitted run-scoped today by `stella-core`'s
    /// `EventSender::pairing_stage_complete` and by `stella-cli`'s command-deck
    /// forwarder.
    Complete,
}

impl StageName {
    /// The signals this stage publishes for later stages to read.
    ///
    /// This is the typed-output half of the graph check: a condition may only
    /// name a signal that the host publishes or that an **earlier** stage
    /// produces.
    ///
    /// A stage publishing nothing is the common case and not a gap: it is a
    /// stage the pipeline branches *into* rather than *on*. Every signal
    /// listed here transcribes a live branch — see [`Signal`] for the line
    /// each one came from.
    #[must_use]
    pub fn publishes(self) -> &'static [Signal] {
        match self {
            Self::Triage => &[
                Signal::Conversational,
                Signal::Questions,
                Signal::Plans,
                Signal::Verifies,
                Signal::WantsWitness,
                Signal::WantsVerifier,
            ],
            Self::Execute => &[Signal::MutatingActions, Signal::DiffLines],
            Self::Witness => &[Signal::WitnessAuthored],
            Self::Verify => &[Signal::FlipAchieved, Signal::TestsRed, Signal::TestsGreen],
            Self::Recall
            | Self::Research
            | Self::Plan
            | Self::Scope
            | Self::Verdict
            | Self::Reflect
            | Self::ContextWrite
            | Self::Complete => &[],
        }
    }

    /// The name this stage is written as in a manifest.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Triage => "triage",
            Self::Recall => "recall",
            Self::Research => "research",
            Self::Plan => "plan",
            Self::Scope => "scope",
            Self::Execute => "execute",
            Self::Witness => "witness",
            Self::Verify => "verify",
            Self::Verdict => "verdict",
            Self::Reflect => "reflect",
            Self::ContextWrite => "contextwrite",
            Self::Complete => "complete",
        }
    }

    /// The workspace-wide stage vocabulary this name denotes.
    ///
    /// One vocabulary exists (`stella-protocol`'s [`StageKind`]) and this
    /// crate does not get a second one — the mapping is total and one-to-one,
    /// so "the manifest vocabulary mirrors `StageKind`" is a fact a test can
    /// check instead of a sentence that drifts. It is also what a host needs
    /// at dispatch time: a declared stage becomes the boundary it emits.
    ///
    /// The manifest spelling and the wire spelling deliberately differ where
    /// the shorter word is the one people say — `recall` for
    /// [`StageKind::ContextRecall`], `scope` for [`StageKind::ScopeReview`].
    #[must_use]
    pub fn kind(self) -> StageKind {
        match self {
            Self::Triage => StageKind::Triage,
            Self::Recall => StageKind::ContextRecall,
            Self::Research => StageKind::Research,
            Self::Plan => StageKind::Plan,
            Self::Scope => StageKind::ScopeReview,
            Self::Execute => StageKind::Execute,
            Self::Witness => StageKind::Witness,
            Self::Verify => StageKind::Verify,
            Self::Verdict => StageKind::Verdict,
            Self::Reflect => StageKind::Reflect,
            Self::ContextWrite => StageKind::ContextWrite,
            Self::Complete => StageKind::Complete,
        }
    }
}

impl std::fmt::Display for StageName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A fact a condition may read.
///
/// The **published set** — naming anything outside it is a load error. Every
/// entry transcribes a branch the pipeline takes today, at a named line; a
/// fact with no live branch behind it is not added, because the grammar's
/// whole defence is that a new name is a reviewable decision rather than a
/// dial someone might find a use for. Growing this set is how a wrapper gains
/// expressiveness — never a richer condition syntax.
///
/// Host facts, readable by any stage:
///
/// - `test-command` — `Pipeline::config.test_command`, the fact gating
///   witness authoring (`pipeline.rs`, the `authored_witness` conjunction).
/// - `candidates` — `PipelineConfig::candidate_count()`. `n == 1` is the
///   single-shot/best-of-N split in `pipeline.rs::run`.
/// - `budget-metered` — `budget.mode() != BudgetMode::Off`, the first thing
///   `pipeline/repair_gate.rs::repair_headroom` asks before a repair round
///   may be bought. Metered, not the amount: dollars are a float, the grammar
///   compares against whole numbers only, and a threshold silently coarser
///   than the guard's would be worse than no threshold.
///
/// Triage's assessment, which decides the conversational fast path, whether
/// research runs, and what the class is owed:
///
/// - `conversational`, `questions`, `plans`, `verifies` — as before.
/// - `wants-witness` — `assessment.wants_witness()`, a conjunct of the
///   authored-witness decision in `pipeline.rs::run`.
/// - `wants-verifier` — `assessment.wants_verifier()`, read by the
///   `LadderDecision::Unverified` arm of `pipeline.rs::verify_candidate`.
///
/// What execution produced, read by the ladder in `verify.rs`:
///
/// - `mutating-actions` — `mutating_actions == 0`, a conjunct of
///   `LadderInputs::nothing_was_attempted`.
/// - `diff-lines` — `diff_lines <= diff_budget`, the diff-budget conjunct of
///   the `SubmitFast` rung.
///
/// What the witness stage produced:
///
/// - `witness-authored` — `witness.is_some()`, which decides the effective
///   test command, the tamper sweep, and the mutation audit in
///   `pipeline.rs::verify_candidate`.
///
/// What verification observed. The two test signals are **both** false when no
/// test ran, and that is deliberate: `touched_tests_passed` is an
/// `Option<bool>`, and one `tests-passed` boolean would report a suite that
/// never ran identically to one that went red. Two total predicates keep the
/// third state visible, the way [`FlipOutcome::is_achieved`] and
/// [`FlipOutcome::was_observed`] do for the flip:
///
/// - `flip-achieved` — `flip.is_achieved()`, the receipt half of the
///   `SubmitFast` rung.
/// - `tests-red` — `touched_tests_passed == Some(false)`, the ladder's
///   deterministic-failure rung.
/// - `tests-green` — `touched_tests_passed == Some(true)`, the corroboration
///   the pipeline's own flip needs beside it.
///
/// [`FlipOutcome::is_achieved`]: stella_protocol::FlipOutcome::is_achieved
/// [`FlipOutcome::was_observed`]: stella_protocol::FlipOutcome::was_observed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Signal {
    /// Boolean, host: a `--test-command` is configured for this run.
    TestCommand,
    /// Count, host: how many candidates this run executes (best-of-N).
    Candidates,
    /// Boolean, host: spend is actually gated, not merely recorded.
    BudgetMetered,
    /// Boolean, triage: this turn is chat, not a software task.
    Conversational,
    /// Count, triage: how many research questions triage named.
    Questions,
    /// Boolean, triage: this task class plans.
    Plans,
    /// Boolean, triage: this task class verifies unconditionally.
    Verifies,
    /// Boolean, triage: this turn warrants an authored witness test.
    WantsWitness,
    /// Boolean, triage: an inconclusive ladder warrants a model verifier.
    WantsVerifier,
    /// Count, execute: calls dispatched that were able to change the tree.
    MutatingActions,
    /// Count, execute: lines of diff the turn produced.
    DiffLines,
    /// Boolean, witness: a witness test was authored and accepted.
    WitnessAuthored,
    /// Boolean, verify: the tracked command went fail→pass and held. `false`
    /// covers "no flip" **and** "nothing observed one" — pair it with
    /// `tests-green` rather than reading it as a negative finding.
    FlipAchieved,
    /// Boolean, verify: a touched test ran and failed. `false` means "no test
    /// ran red", which includes no test having run at all.
    TestsRed,
    /// Boolean, verify: a touched test ran and passed. `false` means "no test
    /// ran green", which includes no test having run at all.
    TestsGreen,
}

impl Signal {
    /// Every published signal, for the "did you mean" half of an error and
    /// for tests that must fail when the set grows without a decision.
    pub const ALL: &'static [Signal] = &[
        Signal::TestCommand,
        Signal::Candidates,
        Signal::BudgetMetered,
        Signal::Conversational,
        Signal::Questions,
        Signal::Plans,
        Signal::Verifies,
        Signal::WantsWitness,
        Signal::WantsVerifier,
        Signal::MutatingActions,
        Signal::DiffLines,
        Signal::WitnessAuthored,
        Signal::FlipAchieved,
        Signal::TestsRed,
        Signal::TestsGreen,
    ];

    /// Resolve a wire name to a signal. `None` is the load error the caller
    /// turns into [`ManifestError::UnknownSignal`].
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|s| s.as_str() == name)
    }

    /// The name this signal is written as in a condition.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TestCommand => "test-command",
            Self::Candidates => "candidates",
            Self::BudgetMetered => "budget-metered",
            Self::Conversational => "conversational",
            Self::Questions => "questions",
            Self::Plans => "plans",
            Self::Verifies => "verifies",
            Self::WantsWitness => "wants-witness",
            Self::WantsVerifier => "wants-verifier",
            Self::MutatingActions => "mutating-actions",
            Self::DiffLines => "diff-lines",
            Self::WitnessAuthored => "witness-authored",
            Self::FlipAchieved => "flip-achieved",
            Self::TestsRed => "tests-red",
            Self::TestsGreen => "tests-green",
        }
    }

    /// Whether this signal is a boolean or a count — the typed half of "each
    /// stage has a typed input". A count in boolean position (or the reverse)
    /// is a load error, not a coercion.
    #[must_use]
    pub fn kind(self) -> SignalKind {
        match self {
            Self::Questions | Self::Candidates | Self::MutatingActions | Self::DiffLines => {
                SignalKind::Count
            }
            Self::TestCommand
            | Self::BudgetMetered
            | Self::Conversational
            | Self::Plans
            | Self::Verifies
            | Self::WantsWitness
            | Self::WantsVerifier
            | Self::WitnessAuthored
            | Self::FlipAchieved
            | Self::TestsRed
            | Self::TestsGreen => SignalKind::Boolean,
        }
    }

    /// Which stage publishes this signal, or `None` when the host does.
    ///
    /// The other half of the graph check: a host fact is readable by any
    /// stage, a stage-published fact only by stages declared after its
    /// publisher.
    #[must_use]
    pub fn publisher(self) -> Option<StageName> {
        match self {
            Self::TestCommand | Self::Candidates | Self::BudgetMetered => None,
            Self::Conversational
            | Self::Questions
            | Self::Plans
            | Self::Verifies
            | Self::WantsWitness
            | Self::WantsVerifier => Some(StageName::Triage),
            Self::MutatingActions | Self::DiffLines => Some(StageName::Execute),
            Self::WitnessAuthored => Some(StageName::Witness),
            Self::FlipAchieved | Self::TestsRed | Self::TestsGreen => Some(StageName::Verify),
        }
    }
}

impl std::fmt::Display for Signal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a signal carries a truth value or a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    /// Readable bare (`conversational`) or negated (`no-conversational`).
    Boolean,
    /// Readable only through a comparison (`questions > 0`).
    Count,
}

impl std::fmt::Display for SignalKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Boolean => "boolean",
            Self::Count => "count",
        })
    }
}

/// How a count is compared against a literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    /// `>`
    Greater,
    /// `>=`
    GreaterOrEqual,
    /// `<`
    Less,
    /// `<=`
    LessOrEqual,
    /// `==`
    Equal,
    /// `!=`
    NotEqual,
}

impl CompareOp {
    /// Every operator, in the order an error message lists them.
    pub const ALL: &'static [CompareOp] = &[
        CompareOp::Greater,
        CompareOp::GreaterOrEqual,
        CompareOp::Less,
        CompareOp::LessOrEqual,
        CompareOp::Equal,
        CompareOp::NotEqual,
    ];

    /// Resolve a wire operator, shared with the evidence grammar so both
    /// halves of the manifest compare with one vocabulary.
    pub(crate) fn from_wire(text: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|op| op.as_str() == text)
    }

    /// The operator as written in a condition.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Greater => ">",
            Self::GreaterOrEqual => ">=",
            Self::Less => "<",
            Self::LessOrEqual => "<=",
            Self::Equal => "==",
            Self::NotEqual => "!=",
        }
    }

    /// Apply the operator. Pure — this is the whole evaluator for the count
    /// half of the grammar.
    #[must_use]
    pub fn apply(self, left: u64, right: u64) -> bool {
        match self {
            Self::Greater => left > right,
            Self::GreaterOrEqual => left >= right,
            Self::Less => left < right,
            Self::LessOrEqual => left <= right,
            Self::Equal => left == right,
            Self::NotEqual => left != right,
        }
    }
}

/// A parsed condition — the whole closed grammar, in two shapes.
///
/// ```text
/// condition := boolean | comparison
/// boolean   := [ "no-" ] <boolean-signal>
/// comparison:= <count-signal> <op> <integer>
/// op        := ">" | ">=" | "<" | "<=" | "==" | "!="
/// ```
///
/// That is the entire language, and it is meant to stay small enough to read
/// in one sitting. Anything a wrapper wants that this cannot express is a new
/// [`Signal`] the host publishes — a reviewable addition — never a richer
/// expression syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    /// A boolean signal, optionally negated.
    Boolean {
        /// The signal read.
        signal: Signal,
        /// `true` when the condition was written with the `no-` prefix, so
        /// the stage runs when the signal is **false**.
        negated: bool,
    },
    /// A count signal compared against a literal.
    Compare {
        /// The signal read.
        signal: Signal,
        /// The comparison applied.
        op: CompareOp,
        /// The literal compared against.
        value: u64,
    },
}

impl Condition {
    /// Parse condition text against the closed grammar.
    ///
    /// `stage` is carried only so a rejection can name where it was written;
    /// it does not affect what parses.
    ///
    /// # Errors
    ///
    /// [`ManifestError::UnparsableCondition`] for text outside the grammar,
    /// [`ManifestError::UnknownSignal`] for a signal the host does not
    /// publish, and [`ManifestError::ConditionTypeMismatch`] for a count read
    /// as a boolean or a boolean compared against a number.
    pub fn parse(stage: StageName, text: &str) -> Result<Self, ManifestError> {
        let unparsable = |reason: &str| ManifestError::UnparsableCondition {
            stage,
            condition: text.to_string(),
            reason: reason.to_string(),
        };

        // Whitespace-separated, which is the only tokenisation the grammar
        // needs: every accepted comparison writes its operator as its own
        // token. That leaves `questions>0` — an attempted comparison with the
        // spaces left out — landing in the one-token arm, where it would
        // otherwise be reported as the unpublished signal `"questions>0"`.
        // That rejection is correct and useless, so the arm names the real
        // mistake instead. The grammar is unchanged: this recognises a
        // failure, it does not accept a second spelling.
        let tokens: Vec<&str> = text.split_whitespace().collect();
        match tokens.as_slice() {
            [bare] if bare.contains(['<', '>', '=', '!']) => Err(unparsable(
                "a comparison must separate its operator from its operands \
                 with spaces, as in \"questions > 0\"",
            )),
            [bare] => {
                let (name, negated) = match bare.strip_prefix("no-") {
                    Some(rest) => (rest, true),
                    None => (*bare, false),
                };
                let signal = Self::resolve(stage, text, name)?;
                if signal.kind() != SignalKind::Boolean {
                    return Err(ManifestError::ConditionTypeMismatch {
                        stage,
                        signal,
                        declared: SignalKind::Boolean,
                        actual: signal.kind(),
                    });
                }
                Ok(Self::Boolean { signal, negated })
            }
            [name, op, value] => {
                let signal = Self::resolve(stage, text, name)?;
                if signal.kind() != SignalKind::Count {
                    return Err(ManifestError::ConditionTypeMismatch {
                        stage,
                        signal,
                        declared: SignalKind::Count,
                        actual: signal.kind(),
                    });
                }
                let op = CompareOp::from_wire(op).ok_or_else(|| {
                    unparsable(&format!(
                        "unknown comparison \"{op}\"; the operators are {}",
                        joined(CompareOp::ALL.iter().map(|o| o.as_str()))
                    ))
                })?;
                let value = value.parse::<u64>().map_err(|_| {
                    unparsable(&format!("\"{value}\" is not a non-negative whole number"))
                })?;
                Ok(Self::Compare { signal, op, value })
            }
            _ => Err(unparsable(
                "expected either a boolean signal (optionally prefixed \"no-\") \
                 or a comparison of the form \"<signal> <op> <number>\"",
            )),
        }
    }

    fn resolve(stage: StageName, text: &str, name: &str) -> Result<Signal, ManifestError> {
        Signal::from_wire(name).ok_or_else(|| ManifestError::UnknownSignal {
            stage,
            condition: text.to_string(),
            signal: name.to_string(),
            published: joined(Signal::ALL.iter().map(|s| s.as_str())),
        })
    }

    /// The signal this condition reads — what the graph check needs to know
    /// without caring which shape the condition took.
    #[must_use]
    pub fn signal(self) -> Signal {
        match self {
            Self::Boolean { signal, .. } | Self::Compare { signal, .. } => signal,
        }
    }
}

/// Render a name list for an error message. Small enough to inline, but it is
/// used from three places and a comma-join written three times drifts.
fn joined<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names.collect::<Vec<_>>().join(", ")
}

impl Wrapper {
    /// The cross-field rules for `[wrapper]`, called from the manifest's
    /// `validate`.
    ///
    /// Three checks, in the order a reader meets a problem: the block itself
    /// is well-formed, each condition parses against the closed grammar, and
    /// the stage graph is satisfiable — no condition reads a signal that only
    /// a later stage publishes, or that an earlier but *conditional* stage
    /// might not publish at all.
    pub(crate) fn validate(&self) -> Result<(), ManifestError> {
        if self.id.trim().is_empty() {
            return Err(ManifestError::EmptyWrapperId);
        }
        if self.stages.is_empty() {
            return Err(ManifestError::EmptyWrapperStages);
        }

        // Walked in declaration order, accumulating what is readable so far:
        // the host's facts always, plus each stage's outputs once that stage
        // has been declared. Checking against this running set — rather than
        // against "is the publisher anywhere in the list" — is what makes an
        // out-of-order manifest a load error instead of a run that reads a
        // fact nothing has produced yet.
        //
        // `at_risk` is the second half of the same question, and it is the
        // half declaration order cannot answer: a *conditional* publisher is
        // declared earlier and still produces nothing on the turns its own
        // condition is false. Kept as its own set so the rejection can say
        // which of the two mistakes was made — the fixes differ.
        let mut published: Vec<Signal> = Vec::new();
        let mut at_risk: Vec<Signal> = Vec::new();
        let mut seen: Vec<StageName> = Vec::with_capacity(self.stages.len());
        for stage in &self.stages {
            if seen.contains(&stage.name) {
                return Err(ManifestError::DuplicateWrapperStage { stage: stage.name });
            }
            seen.push(stage.name);

            let condition = stage.condition()?;
            if let Some(condition) = condition {
                let signal = condition.signal();
                if let Some(publisher) = signal.publisher() {
                    if at_risk.contains(&signal) {
                        return Err(ManifestError::PublisherMayBeSkipped {
                            stage: stage.name,
                            signal,
                            publisher,
                        });
                    }
                    if !published.contains(&signal) {
                        return Err(ManifestError::SignalNotYetPublished {
                            stage: stage.name,
                            signal,
                            publisher,
                        });
                    }
                }
            }

            if condition.is_some() {
                at_risk.extend_from_slice(stage.name.publishes());
            } else {
                published.extend_from_slice(stage.name.publishes());
            }
        }

        Ok(())
    }
}
