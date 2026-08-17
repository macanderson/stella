//! Why a manifest was rejected, as a typed answer (invariant 5).
//!
//! Every variant names the rule it enforces and carries the identifiers a
//! caller needs to point at the offending declaration. A host surfacing one
//! of these to a plugin author should be able to print it verbatim and have
//! the fix be obvious.

use crate::manifest::{HookEvent, Participation};
use crate::wrapper::{Signal, SignalKind, StageName};

/// A manifest failed to parse or failed validation.
///
/// Parsing and validation are deliberately one error type: a caller loading a
/// manifest cannot act differently on "the TOML was malformed" versus "the
/// TOML was well-formed but claims a grant it may not have" — both mean the
/// plugin does not load, and both are the author's to fix. What a caller
/// *does* branch on is which rule failed, which is what the variants encode.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// The TOML itself was rejected — syntax, an unknown key (every table
    /// denies unknown fields, per the #1400 rule this crate inherits), a
    /// hook name outside the shipped set, or a grade outside the ladder.
    #[error("manifest is not a valid plugin manifest: {0}")]
    Parse(#[from] toml::de::Error),

    /// `[loop] hooks` listed the same event twice. A duplicate is always an
    /// editing mistake, and silently deduplicating would hide it.
    #[error("[loop] hooks declares {hook} more than once")]
    DuplicateHook {
        /// The event that appeared twice.
        hook: HookEvent,
    },

    /// A grade below `steering` declared hook points. `none` is a content
    /// bundle and `observer` may only watch the event stream — neither may
    /// act at a hook point (#3245 §2).
    #[error(
        "[loop] participation = \"{participation}\" may not declare hooks; \
         acting at a hook point requires \"steering\" or above"
    )]
    HooksRequireSteering {
        /// The declared grade, below the one the hooks need.
        participation: Participation,
    },

    /// A grade below `arbiter` declared the `Stop` hook. `Stop` is the
    /// completion gate; touching completion is exactly what separates
    /// `arbiter` from `steering` (#3245 §2).
    #[error(
        "[loop] hooks declares Stop, but participation = \"{participation}\"; \
         binding the Stop gate requires \"arbiter\""
    )]
    StopHookRequiresArbiter {
        /// The declared grade, below `arbiter`.
        participation: Participation,
    },

    /// An `arbiter` did not declare the `Stop` hook. An arbiter's entire
    /// additional power is the completion verdict, and an undeclared hook is
    /// never invoked — so an arbiter without `Stop` is a contradiction, not
    /// a quieter arbiter.
    #[error(
        "participation = \"arbiter\" requires the Stop hook in [loop] hooks: \
         the completion verdict is what the grade grants, and an undeclared \
         hook is never invoked"
    )]
    ArbiterMustDeclareStop,

    /// `max_holds` was declared below `arbiter`. Only an arbiter can hold a
    /// completion open, so the field has no meaning at any other grade and
    /// its presence signals a misunderstood manifest.
    #[error(
        "[loop] max_holds is only meaningful at participation = \"arbiter\" \
         (declared grade: \"{participation}\")"
    )]
    MaxHoldsRequiresArbiter {
        /// The declared grade, below `arbiter`.
        participation: Participation,
    },

    /// `max_holds = 0` — an arbiter that may never hold is a `steering`
    /// plugin wearing the wrong grade; declare the grade that is meant.
    #[error(
        "[loop] max_holds must be at least 1; an arbiter that can never hold is not an arbiter"
    )]
    ZeroMaxHolds,

    /// `[requirements]` was declared below `arbiter`. Requirements exist so
    /// a hold is attributable to a named definition-of-done entry; without
    /// the power to hold, there is nothing to attribute (#3245 §2).
    #[error(
        "[requirements] is only meaningful at participation = \"arbiter\" \
         (declared grade: \"{participation}\")"
    )]
    RequirementsRequireArbiter {
        /// The declared grade, below `arbiter`.
        participation: Participation,
    },

    /// An `arbiter` declared no `[requirements]`, or an empty table. Every
    /// hold must cite a named requirement, so an arbiter with none could
    /// never hold attributably — the definition of done is enumerable,
    /// never vibes (#3245 §2).
    #[error(
        "participation = \"arbiter\" requires a non-empty [requirements] \
         table: every hold must cite a named requirement"
    )]
    ArbiterRequiresRequirements,

    /// A `[requirements]` entry's description was empty. The description is
    /// what the deck and the completion report show a human when the
    /// requirement holds a turn open; an empty one is an unattributable hold.
    #[error("[requirements] entry \"{name}\" has an empty description")]
    EmptyRequirement {
        /// The requirement key with the empty value.
        name: String,
    },

    /// `[oracle]` was declared below `arbiter`. The oracle exists to decide
    /// requirements (the fail→pass flip the host tracks); below `arbiter`
    /// there are no requirements for it to decide. Conservative by design:
    /// accepting it later at a lower grade widens the contract compatibly,
    /// while rejecting it later would break shipped manifests.
    #[error(
        "[oracle] is only meaningful at participation = \"arbiter\" \
         (declared grade: \"{participation}\")"
    )]
    OracleRequiresArbiter {
        /// The declared grade, below `arbiter`.
        participation: Participation,
    },

    /// `[oracle] command.argv` was empty — there is no program to run.
    #[error("[oracle] command.argv must name a program: it is empty")]
    EmptyOracleArgv,

    /// `[oracle] command.timeout_secs = 0` — a zero timeout means the host
    /// would kill the oracle before it ran, which can only be a mistake.
    #[error("[oracle] command.timeout_secs must be at least 1")]
    ZeroOracleTimeout,

    /// An `[oracle] measurements` entry was blank. A measurement's name is
    /// what a check reads and what a hold message prints; a blank one can be
    /// neither.
    #[error("[oracle] measurements contains a blank name")]
    EmptyMeasurementName,

    /// `[oracle] measurements` named the same measurement twice. Always an
    /// editing mistake, and deduplicating silently would hide it — the
    /// [`ManifestError::DuplicateHook`] rule, for evidence.
    #[error("[oracle] measurements declares \"{measurement}\" more than once")]
    DuplicateMeasurement {
        /// The name that appeared twice.
        measurement: String,
    },

    /// A `[[oracle.checks]]` entry's text is outside the closed comparison
    /// grammar. The grammar is `<measurement> <op> <integer>` and nothing
    /// else — a richer expression syntax is a second program with no gate on
    /// it, which is the [`crate::Condition`] argument applied to evidence.
    #[error("[[oracle.checks]] for \"{requirement}\": cannot parse \"{check}\" — {reason}")]
    UnparsableCheck {
        /// The requirement whose check failed to parse.
        requirement: String,
        /// The check text as written.
        check: String,
        /// What was wrong with it, in the words an author needs.
        reason: String,
    },

    /// A check read a measurement the same `[oracle]` block did not declare.
    /// A rule over a number nothing reports would decide nothing at run
    /// time, which is exactly the manifest that quietly does nothing.
    #[error(
        "[[oracle.checks]] for \"{requirement}\" reads \"{measurement}\", \
         which [oracle] measurements does not declare (declared: {declared})"
    )]
    UnknownMeasurement {
        /// The requirement whose check reads the unknown measurement.
        requirement: String,
        /// The check text as written.
        check: String,
        /// The undeclared name.
        measurement: String,
        /// The declared measurements, for the "did you mean" half.
        declared: String,
    },

    /// A check named a requirement no `[requirements]` entry declares. Every
    /// hold cites a named requirement, so a check that decides an unnamed one
    /// can never be attributed.
    #[error(
        "[[oracle.checks]] names requirement \"{requirement}\", which \
         [requirements] does not declare"
    )]
    CheckWithoutRequirement {
        /// The requirement name the check cited.
        requirement: String,
    },

    /// `flip = "not-applicable"` with a requirement no check decides. Without
    /// a flip, a check is the only thing that can establish a requirement, so
    /// an unchecked one is a clause of the definition of done that nothing
    /// could ever meet — a hold with no way out.
    #[error(
        "[oracle] flip = \"not-applicable\" leaves requirement \
         \"{requirement}\" undecidable: with no flip, every requirement needs \
         a [[oracle.checks]] entry"
    )]
    UndecidableRequirement {
        /// The requirement nothing decides.
        requirement: String,
    },

    /// The oracle ran but reported no value for a measurement a check reads.
    /// A missing number is never read as a satisfied budget — the
    /// [`ManifestError::SignalNotProduced`] discipline, for evidence.
    #[error(
        "the oracle reported no value for \"{measurement}\", which the check \
         for requirement \"{requirement}\" reads"
    )]
    MeasurementNotReported {
        /// The requirement whose check could not be decided.
        requirement: String,
        /// The measurement that was missing from the reported set.
        measurement: String,
    },

    /// `[subloop]` was declared below `steering`. Subloop stages run as
    /// bounded child turns inside the host's loop — that is participation,
    /// which `none` and `observer` have disclaimed.
    #[error(
        "[subloop] is only meaningful at participation = \"steering\" or \
         above (declared grade: \"{participation}\")"
    )]
    SubloopRequiresSteering {
        /// The declared grade, below `steering`.
        participation: Participation,
    },

    /// `[subloop] stages` was empty. A subloop with no stages does nothing;
    /// omit the table instead.
    #[error("[subloop] stages must name at least one stage")]
    EmptyStages,

    /// `[subloop] stages` named the same stage twice. Order is the whole
    /// point of the list, and a duplicated name makes the order ambiguous.
    #[error("[subloop] stages declares \"{stage}\" more than once")]
    DuplicateStage {
        /// The stage name that appeared twice.
        stage: String,
    },

    /// A `[subloop] stages` entry was empty.
    #[error("[subloop] stages contains an empty stage name")]
    EmptyStageName,

    /// `[roles]` was declared without `[subloop]`. A role exists to be
    /// resolved for a subloop stage; with no stages it is dead config, and
    /// dead config in a consent document is a hazard, not clutter.
    #[error("[roles] requires a [subloop]: a role is only resolved for a subloop stage")]
    RolesRequireSubloop,

    /// A `[roles.<name>]` entry declared an empty tier. The tier is the
    /// intent the host resolves against the user's providers; an empty
    /// intent resolves to nothing.
    #[error("[roles.{name}] tier must not be empty")]
    EmptyRoleTier {
        /// The role whose tier was empty.
        name: String,
    },

    /// The manifest's `name` was empty. The name is the identity every
    /// grant, chip, and hold attribution hangs off.
    #[error("manifest name must not be empty")]
    EmptyName,

    /// A `[[capabilities]]` entry named no tool. The tool name is what a gate
    /// rule keys on and what the consent prompt shows; a blank one is a
    /// request for nothing that still reads as a request.
    #[error("[[capabilities]] entry must name a tool: `tool` is empty")]
    EmptyCapabilityTool,

    /// A `[[capabilities]]` entry stated no purpose. "This plugin asks for
    /// `bash`" is not something a human can consent to; the reason is the
    /// half that makes the grant reviewable, so it is required rather than
    /// optional.
    #[error(
        "[[capabilities]] entry for \"{tool}\" has an empty purpose: a capability \
         with no stated reason is not something a user can consent to"
    )]
    EmptyCapabilityPurpose {
        /// The tool whose purpose was blank.
        tool: String,
    },

    /// A `[[capabilities]]` entry declared a blank scope string. A blank
    /// limit renders as a limit and bounds nothing — the shape of claim this
    /// consent surface exists to prevent.
    #[error("[[capabilities]] entry for \"{tool}\" declares an empty scope entry")]
    EmptyCapabilityScope {
        /// The tool whose scope list contained a blank.
        tool: String,
    },

    /// The same tool was requested twice. One entry per tool, with its scope
    /// list carrying the qualifiers: two entries make the effective grant the
    /// union of two lines a user read separately, which is exactly how a wide
    /// grant hides inside a consent prompt.
    #[error(
        "[[capabilities]] requests \"{tool}\" more than once: declare one entry per \
         tool and list its qualifiers in `scope`"
    )]
    DuplicateCapability {
        /// The tool requested twice.
        tool: String,
    },

    /// `[wrapper]` was declared below `steering`. A wrapper intercepts the
    /// turn loop — it adds context, gathers evidence, and asks for another
    /// turn. That is participation, which `none` and `observer` have
    /// disclaimed.
    #[error(
        "[wrapper] is only meaningful at participation = \"steering\" or \
         above (declared grade: \"{participation}\")"
    )]
    WrapperRequiresSteering {
        /// The declared grade, below `steering`.
        participation: Participation,
    },

    /// `[wrapper] id` was empty. The id is what the store's
    /// `pipeline_variant` column records (#3388), so it is the join key of
    /// every per-variant comparison; a blank one makes a measured run
    /// indistinguishable from an unmeasured one.
    #[error("[wrapper] id must not be empty: it is the variant id every execution row records")]
    EmptyWrapperId,

    /// `[[wrapper.stages]]` declared no stages. A wrapper that runs no
    /// stages is the raw loop with extra ceremony; omit the block instead.
    #[error("[wrapper] must declare at least one [[wrapper.stages]] entry")]
    EmptyWrapperStages,

    /// The same stage was declared twice. Order is the whole point of the
    /// list, and a repeated stage makes both its position and its condition
    /// ambiguous.
    #[error("[wrapper] declares stage \"{stage}\" more than once")]
    DuplicateWrapperStage {
        /// The stage that appeared twice.
        stage: StageName,
    },

    /// A stage's `if` text is outside the closed condition grammar.
    ///
    /// Deliberately not a parser suggestion box: the grammar is two shapes
    /// and stays that way, because a Turing-complete condition in a manifest
    /// is a second program with no gate on it
    /// (`doc:turn-loop-wrappers` §9.4).
    #[error("[wrapper] stage \"{stage}\" has an unreadable condition \"{condition}\": {reason}")]
    UnparsableCondition {
        /// The stage carrying the condition.
        stage: StageName,
        /// The condition text as written.
        condition: String,
        /// Which grammar rule it failed, phrased as the fix.
        reason: String,
    },

    /// A condition named a signal the host does not publish.
    ///
    /// The rule #3381 carries from #3245 slice A, made mechanical: a
    /// manifest that quietly does nothing is worse than one that refuses to
    /// load, so an unpublished signal is a load error rather than a
    /// condition that silently never fires.
    #[error(
        "[wrapper] stage \"{stage}\" reads \"{signal}\" in condition \
         \"{condition}\", which the host does not publish; published \
         signals are: {published}"
    )]
    UnknownSignal {
        /// The stage carrying the condition.
        stage: StageName,
        /// The condition text as written.
        condition: String,
        /// The unpublished name.
        signal: String,
        /// The published set, comma-joined, so the fix needs no doc lookup.
        published: String,
    },

    /// A count signal was read as a boolean, or a boolean compared against a
    /// number. The typed half of "each stage has a typed input and a typed
    /// output": a mismatch is a load error, never a coercion.
    #[error(
        "[wrapper] stage \"{stage}\" reads \"{signal}\" as a {declared} \
         signal, but it is a {actual} signal"
    )]
    ConditionTypeMismatch {
        /// The stage carrying the condition.
        stage: StageName,
        /// The signal read at the wrong type.
        signal: Signal,
        /// The type the condition's shape implied.
        declared: SignalKind,
        /// The type the signal actually has.
        actual: SignalKind,
    },

    /// A condition read a signal that only a **later** stage publishes.
    ///
    /// The stage-graph check. Without it, a hand-written variant's failure
    /// mode is a wedged run at round three; with it, the same manifest is a
    /// rejection with a reason at load.
    #[error(
        "[wrapper] stage \"{stage}\" reads \"{signal}\", which \"{publisher}\" \
         publishes — but \"{publisher}\" is not declared before it"
    )]
    SignalNotYetPublished {
        /// The stage whose condition reads too early.
        stage: StageName,
        /// The signal read before it exists.
        signal: Signal,
        /// The stage that publishes it, declared later or not at all.
        publisher: StageName,
    },

    /// A condition read a signal published by an earlier stage that is itself
    /// **conditional**, so some resolutions skip the publisher and the read
    /// has no value at all.
    ///
    /// The half of the stage-graph check that declaration order alone cannot
    /// see, and it only became checkable once something resolved a manifest
    /// (`crate::program`): "declared earlier" answers *whether the publisher
    /// appears*, never *whether it ran*. The grammar has no conjunction, so a
    /// reader cannot say "when the publisher ran, and…" — which makes this
    /// shape unconditionally a hazard rather than a case an author could
    /// guard. Rejecting it at load is what lets
    /// [`Wrapper::resolve`](crate::Wrapper::resolve) be total for every
    /// possible set of signal values.
    #[error(
        "[wrapper] stage \"{stage}\" reads \"{signal}\", which \"{publisher}\" \
         publishes — but \"{publisher}\" is conditional, so a turn that skips \
         it would leave \"{signal}\" with no value; make \"{publisher}\" \
         unconditional or drop the condition that reads it"
    )]
    PublisherMayBeSkipped {
        /// The stage whose condition could read a fact nothing produced.
        stage: StageName,
        /// The signal whose existence depends on a conditional stage.
        signal: Signal,
        /// The conditional stage that publishes it.
        publisher: StageName,
    },

    /// Resolution reached a condition whose publishing stage did not run.
    ///
    /// Unreachable for a manifest that came from
    /// [`PluginManifest::from_toml_str`](crate::PluginManifest::from_toml_str)
    /// — [`PublisherMayBeSkipped`](Self::PublisherMayBeSkipped) rejects that
    /// shape at load. It exists so a hand-built [`Wrapper`](crate::Wrapper)
    /// that bypassed the constructor still cannot make the evaluator read an
    /// unproduced fact as `false`: the silent answer is the one failure mode
    /// this crate refuses.
    #[error(
        "[wrapper] stage \"{stage}\" reads \"{signal}\", but \"{publisher}\", \
         which publishes it, did not run in this resolution"
    )]
    SignalNotProduced {
        /// The stage whose condition could not be answered.
        stage: StageName,
        /// The signal nothing produced.
        signal: Signal,
        /// The stage that publishes it, skipped this turn.
        publisher: StageName,
    },
}
