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
    /// [`crate::PipelineConfig::require_independent_verifier`] is on and the
    /// verdict call would resolve to the worker's own model — or to no model
    /// at all (#1795).
    ///
    /// Same shape and same before-spend placement as the witness refusal
    /// above, for the same caller: one that has published the claim that an
    /// independent reviewer grades the work. The ordinary posture keeps the
    /// soft path — the verdict runs self-graded, records the fact on its
    /// ladder snapshot, and the router's caveat says so in prose.
    #[error(
        "an independent verifier was required for the verdict but {0} — refusing before spend rather than letting the worker grade its own work under a configuration that claims otherwise"
    )]
    VerifierNotIndependent(String),
    /// A required role (worker) could not be resolved at all.
    #[error(transparent)]
    Routing(#[from] RouterError),
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

/// What [`super::Pipeline::witness_author_independence`] found. Three states,
/// not a bool: "the verifier is the worker" and "the worker itself will not
/// resolve" are different facts with different owners, and collapsing them is
/// how a routing failure starts reading as a witness verdict.
pub(super) enum WitnessAuthorIndependence {
    /// A verifier distinct from the worker resolves — the author exists.
    Independent,
    /// No author independent of the worker, with the reason to announce.
    Unavailable(String),
    /// The worker role itself will not resolve. Not a witness verdict at all:
    /// the run fails on its own terms a few steps later, with the routing
    /// error that actually explains it.
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
