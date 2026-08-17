//! The `[wrapper]` block — a turn-loop wrapper's stage order, declared.
//!
//! Slice of #3381: today the conditional stage order the staged pipeline runs
//! is hardcoded branches inside `crates/stella-pipeline/src/pipeline.rs`, a
//! file on the god-file list and closed to growth — so the design cannot
//! absorb another variant even if we wanted one. A wrapper plugin declares the
//! order instead, and the conditions become a line you can read and change.
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
/// a future stage adds a variant here. The names and their order mirror
/// `stage_rank` in `crates/stella-pipeline/src/replay.rs`, which is the
/// canonical ordering today.
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
}

impl StageName {
    /// The signals this stage publishes for later stages to read.
    ///
    /// This is the typed-output half of the graph check: a condition may only
    /// name a signal that the host publishes or that an **earlier** stage
    /// produces.
    #[must_use]
    pub fn publishes(self) -> &'static [Signal] {
        match self {
            Self::Triage => &[
                Signal::Conversational,
                Signal::Questions,
                Signal::Plans,
                Signal::Verifies,
            ],
            Self::Recall
            | Self::Research
            | Self::Plan
            | Self::Scope
            | Self::Execute
            | Self::Witness
            | Self::Verify => &[],
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
/// entry is a fact the staged pipeline already branches on today, so this is a
/// description of live behaviour rather than a vocabulary invented for the
/// manifest:
///
/// - `test-command` — `Pipeline::config.test_command`, the host fact gating
///   witness authoring.
/// - `conversational`, `questions`, `plans`, `verifies` — triage's assessment,
///   which decides the conversational fast path, whether the research stage
///   runs at all, and whether the class plans and verifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Signal {
    /// Boolean, host: a `--test-command` is configured for this run.
    TestCommand,
    /// Boolean, triage: this turn is chat, not a software task.
    Conversational,
    /// Count, triage: how many research questions triage named.
    Questions,
    /// Boolean, triage: this task class plans.
    Plans,
    /// Boolean, triage: this task class verifies unconditionally.
    Verifies,
}

impl Signal {
    /// Every published signal, for the "did you mean" half of an error and
    /// for tests that must fail when the set grows without a decision.
    pub const ALL: &'static [Signal] = &[
        Signal::TestCommand,
        Signal::Conversational,
        Signal::Questions,
        Signal::Plans,
        Signal::Verifies,
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
            Self::Conversational => "conversational",
            Self::Questions => "questions",
            Self::Plans => "plans",
            Self::Verifies => "verifies",
        }
    }

    /// Whether this signal is a boolean or a count — the typed half of "each
    /// stage has a typed input". A count in boolean position (or the reverse)
    /// is a load error, not a coercion.
    #[must_use]
    pub fn kind(self) -> SignalKind {
        match self {
            Self::Questions => SignalKind::Count,
            Self::TestCommand | Self::Conversational | Self::Plans | Self::Verifies => {
                SignalKind::Boolean
            }
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
            Self::TestCommand => None,
            Self::Conversational | Self::Questions | Self::Plans | Self::Verifies => {
                Some(StageName::Triage)
            }
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

    fn from_wire(text: &str) -> Option<Self> {
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
