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
    /// Legacy error retained so stored/public values remain source-compatible.
    /// Live orchestration never emits it because witness model work is retired.
    #[error(
        "an independent witness author was required but {0} — refusing rather than running as the single-model arm under a configuration that claims otherwise"
    )]
    WitnessAuthorUnavailable(String),
    /// Legacy error retained so stored/public values remain source-compatible.
    /// Live orchestration never emits it because model verdicts are retired.
    #[error(
        "an independent verifier was required for the verdict but {0} — refusing before spend rather than letting the worker grade its own work under a configuration that claims otherwise"
    )]
    VerifierNotIndependent(String),
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
