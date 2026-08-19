use stella_core::router::RouterError;
use stella_protocol::ModelRef;

/// A hard, named failure of a pipeline run (as opposed to a clean
/// [`super::PipelineStatus::Aborted`], which is a normal outcome).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PipelineError {
    /// A user-supplied test command did not fit the typed test vocabulary.
    #[error("invalid test command: {0}")]
    InvalidTestCommand(String),
    /// A plan crossed the scope-review thresholds while running headless with
    /// no approval bypass configured (L-E5): never silently auto-approve.
    #[error(
        "scope review is required for this plan, but the run is headless without an approval bypass — re-run interactively or enable the scope-review bypass"
    )]
    ScopeReviewRequiredHeadless,
    /// The router resolved a role to a model no configured adapter serves.
    #[error(
        "no provider adapter is configured for the resolved model `{0}` — configure the provider or refresh the catalog"
    )]
    NoProviderForModel(String),
    /// [`crate::PipelineConfig::require_independent_witness`] is on and the
    /// wiring cannot supply a witness author independent of the worker.
    ///
    /// The ordinary posture degrades here instead — losing the author costs
    /// the run its authored witness, never the task. This variant exists for
    /// the caller that has already PUBLISHED the claim that an independent
    /// author exists (a benchmark arm whose posture digest names a second
    /// model): for them a degraded run is not a weaker result, it is a number
    /// described by the wrong posture, and refusing is the only honest outcome.
    #[error(
        "an independent witness author was required but {0} — refusing rather than running as the single-model arm under a configuration that claims otherwise"
    )]
    WitnessAuthorUnavailable(String),
    /// The responsibility roster (#2381) says something that cannot be
    /// honoured — a key naming no responsibility, a binding naming no agent,
    /// a disabled worker.
    ///
    /// Refused before spend, and refused rather than repaired, for the reason
    /// the roster exists: its rows are how a measurement declares which stages
    /// ran. Quietly dropping an unparseable row would produce a run whose
    /// posture says "triage ablated" and whose trace shows triage running,
    /// which is worse than no run at all. Carries every problem at once, since
    /// a hand-written block usually has more than one.
    #[error("the responsibility roster cannot be honoured: {0}")]
    InvalidRoster(String),
    /// A required role (worker) could not be resolved at all.
    #[error(transparent)]
    Routing(#[from] RouterError),
    /// The turn's `[wrapper]` variant could not schedule a stage: the
    /// built-in `classic` manifest failed to load (a build-time defect), or
    /// a configured variant's declared order does not match how this host
    /// visits its own stages (#3408). Unreachable for `classic`, asserted by
    /// `tests/variant_program.rs` and `tests/variant_dispatch.rs`.
    #[error("the pipeline's wrapper variant could not schedule this turn: {0}")]
    InvalidVariant(String),
}

/// A hard pipeline failure paired with every paid stage that settled before
/// the failure boundary.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{cause}")]
pub struct PipelineRunError {
    pub cause: PipelineError,
    pub total_cost_usd: f64,
}

impl PipelineRunError {
    pub(super) fn new(cause: PipelineError, total_cost_usd: f64) -> Self {
        Self {
            cause,
            total_cost_usd,
        }
    }
}

/// What [`super::Pipeline::independence_of`] found about one responsibility.
///
/// Four states, not a bool, and not three: "the assigned agent resolves to the
/// worker's model", "the operator turned this responsibility off" and "the
/// worker itself will not resolve" are different facts with different owners,
/// and collapsing any two of them is how a routing failure starts reading as a
/// verdict about independence — or, since #2381 made enablement configurable,
/// how a deliberate ablation starts reading as an outage.
pub(super) enum Independence {
    /// The assigned agent resolves to a model the worker does not use.
    Independent,
    /// The roster does not run this responsibility at all.
    ///
    /// An ablation the operator asked for, which
    /// [`super::Pipeline::report_roster_posture`] has already stated once,
    /// before any spend. A consumer must therefore neither report it a second
    /// time nor describe it as a fault — but a caller that *required*
    /// independence must still refuse, because "the stage did not run" fails
    /// the claim it made just as surely as "the stage ran on the worker's
    /// model".
    Withheld,
    /// The responsibility runs, but nothing independent of the worker backs
    /// it, with the reason to announce.
    ///
    /// The reason names the assigned agent and never the responsibility: each
    /// consumer wraps it in a sentence that already supplies its own role
    /// (#1795), so a reason that named one too would misname the others.
    Unavailable(String),
    /// The worker role itself will not resolve. Not a verdict about
    /// independence at all: the run fails on its own terms a few steps later,
    /// with the routing error that actually explains it.
    WorkerUnresolvable,
}

pub(super) enum RoleResolveError {
    Router(RouterError),
    NoProvider(ModelRef),
}

impl RoleResolveError {
    pub(super) fn into_pipeline_error(self) -> PipelineError {
        match self {
            Self::Router(error) => PipelineError::Routing(error),
            Self::NoProvider(model) => PipelineError::NoProviderForModel(model.to_string()),
        }
    }
}
