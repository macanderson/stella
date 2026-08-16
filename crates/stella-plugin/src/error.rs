//! Why a manifest was rejected, as a typed answer (invariant 5).
//!
//! Every variant names the rule it enforces and carries the identifiers a
//! caller needs to point at the offending declaration. A host surfacing one
//! of these to a plugin author should be able to print it verbatim and have
//! the fix be obvious.

use crate::manifest::{HookEvent, Participation};

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
}
