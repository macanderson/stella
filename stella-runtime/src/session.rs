//! The composite: one call that assembles a session's whole resource stack.

use std::sync::Arc;

use stella_core::budget::BudgetGuard;
use stella_core::estimator::CalibrationMap;
use stella_protocol::{ModelRef, Provider};
use stella_store::Store;
use stella_tools::ToolRegistry;

use crate::error::RuntimeError;
use crate::parts;
use crate::spec::{Notice, RuntimeSpec};

/// One session's assembled resources: everything below the engine ports, with
/// nothing above them.
///
/// The line this type draws is the one the CLI never had. Below it sit the
/// resources — provider adapter, tool registry, store, budget, calibration —
/// which are pure functions of a [`RuntimeSpec`] and identical whether a TUI,
/// an SSE stream, or a test harness is driving. Above it sit the *ports*,
/// which are not: the approval gate is a terminal prompt in one host and a
/// reverse HTTP request in another, and the event sink is a renderer thread in
/// one and an SSE pump in the other. Only the bottom half is in here, which is
/// why the top half stayed in `stella-cli` and can differ per surface without
/// this crate knowing.
pub struct SessionRuntime {
    /// The workspace every resource below is rooted at.
    pub workspace_root: std::path::PathBuf,
    /// The constructed provider adapter.
    pub provider: Box<dyn Provider>,
    /// The provider/model pin this runtime was built for, for role routing.
    pub model_ref: ModelRef,
    /// The tool registry, `Arc`'d because the graph builder, the MCP set and
    /// the candidate workspaces all hold it alongside the driver.
    pub registry: Arc<ToolRegistry>,
    /// The workspace store, or `None` when persistence is off or it would not
    /// open. Never a startup failure — see [`parts::open_store`].
    pub store: Option<Arc<Store>>,
    /// Token-drift calibration, seeded from this provider/model's prior
    /// samples when a store was available.
    pub calibration: CalibrationMap,
    /// The session's budget guard, already in the right mode for the spec.
    pub budget: BudgetGuard,
    /// Diagnostics the construction produced, for the host to render. Empty
    /// on the ordinary path.
    pub notices: Vec<Notice>,
}

/// Assembles a [`SessionRuntime`] from a [`RuntimeSpec`].
///
/// A builder rather than a bare function so the steps stay individually
/// overridable as hosts diverge — the fleet already wants a registry rooted at
/// a task directory while its claims store stays at the session root, and a
/// test wants to substitute a provider without a credential. Today the only
/// override is [`RuntimeBuilder::with_provider`]; the shape is here so adding
/// the next one is not a signature break for every caller.
pub struct RuntimeBuilder {
    spec: RuntimeSpec,
    provider: Option<Box<dyn Provider>>,
}

impl RuntimeBuilder {
    /// Start from a spec.
    #[must_use]
    pub fn new(spec: RuntimeSpec) -> Self {
        Self {
            spec,
            provider: None,
        }
    }

    /// Use this provider instead of constructing one from the spec's parts.
    ///
    /// The seam a server needs: `stella-serve` remotes every model call back
    /// to its host, so it installs a `RemoteProvider` here and never holds a
    /// credential at all. Tests use it to install a scripted provider.
    #[must_use]
    pub fn with_provider(mut self, provider: Box<dyn Provider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Assemble the stack.
    ///
    /// The order is the order the CLI has always used and is not arbitrary:
    /// the store is opened before calibration because calibration is seeded
    /// *from* it, and the registry is built before anything that wraps it.
    /// Nothing here reads `std::env` or `current_dir()` — that is the property
    /// that makes N concurrent sessions in one process sound, and it is
    /// asserted by `no_ambient_reads` in this crate's tests.
    pub async fn build(self) -> Result<SessionRuntime, RuntimeError> {
        let spec = self.spec;
        if !spec.workspace_root.is_dir() {
            return Err(RuntimeError::WorkspaceRoot(spec.workspace_root));
        }

        let provider = match self.provider {
            Some(provider) => provider,
            None => parts::build_provider(&spec.provider)?,
        };
        let model_ref = ModelRef::new(spec.provider.id.clone(), spec.provider.model_id.clone());

        let registry = Arc::new(
            parts::tool_registry(
                spec.workspace_root.clone(),
                spec.registry_options,
                spec.persistence,
            )
            .await,
        );

        let mut notices = Vec::new();
        let (store, store_notice) = parts::open_store(&spec.workspace_root, spec.persistence);
        notices.extend(store_notice);

        let calibration =
            parts::seed_calibration(store.as_ref(), &spec.provider.id, &spec.provider.model_id);
        let budget = parts::budget_guard(spec.budget_limit_usd);

        Ok(SessionRuntime {
            workspace_root: spec.workspace_root,
            provider,
            model_ref,
            registry,
            store,
            calibration,
            budget,
            notices,
        })
    }
}
