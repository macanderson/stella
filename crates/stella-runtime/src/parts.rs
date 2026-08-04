//! The individual construction steps, each callable on its own.
//!
//! These are the functions that were re-typed at seven call sites in
//! `stella-cli` — `agent.rs` twice, `agent/goal.rs` twice, `command_deck.rs`,
//! `fleet_cmd.rs`, and `subsession.rs`. They are exposed individually as well
//! as through [`crate::RuntimeBuilder`] because the drivers do not all want
//! the same subset: the fleet wants a registry rooted at a *task* directory
//! while its claims store stays rooted at the session's, and the deck builds
//! its stack incrementally as the user turns surfaces on.
//!
//! Every one of them is a pure function of its arguments. Where the CLI
//! version consulted a process global or wrote to stderr, the argument list
//! grew a parameter or the return type grew a [`Notice`].

use std::path::Path;
use std::sync::Arc;

use stella_core::budget::BudgetGuard;
use stella_core::estimator::CalibrationMap;
use stella_protocol::{BudgetMode, Provider};
use stella_store::Store;
use stella_tools::{RegistryOptions, ToolRegistry};

use crate::error::RuntimeError;
use crate::spec::{Notice, NoticeSubject, Persistence, ProviderParts};

/// How many recent drift samples to replay into a fresh session's
/// calibration. With the estimator's EWMA weight (0.3) anything past ~20
/// samples has negligible influence, and 20 rows is a trivial query.
const DRIFT_SEED_SAMPLES: usize = 20;

/// Build the session's tool registry.
///
/// `persistence` decides whether the filesystem-backed backends are detected
/// at all: a claim-mode trial gets a registry with no backends rather than one
/// pointed at state it must not read. This is the parameter that replaces
/// `stella-cli`'s `settings::filesystem_settings_disabled()` call — the same
/// decision, made by the caller instead of by a process-wide environment
/// variable, which is what lets two sessions in one process disagree about it.
pub async fn tool_registry(
    workspace_root: std::path::PathBuf,
    options: RegistryOptions,
    persistence: Persistence,
) -> ToolRegistry {
    if persistence.is_disabled() {
        ToolRegistry::with_backends_and_options(workspace_root, None, None, options)
    } else {
        ToolRegistry::new_detected(workspace_root, options).await
    }
}

/// Open the workspace SQLite store (`.stella/private/store.db`).
///
/// Persistence is observability, not a work dependency: a store that will not
/// open yields `(None, Some(notice))` and the session runs on without it —
/// never a startup failure. The caller decides how loudly to say so, which is
/// the only difference from the CLI original.
#[must_use]
pub fn open_store(
    workspace_root: &Path,
    persistence: Persistence,
) -> (Option<Arc<Store>>, Option<Notice>) {
    if persistence.is_disabled() {
        return (None, None);
    }
    match Store::open(workspace_root) {
        Ok(store) => (Some(Arc::new(store)), None),
        Err(e) => (
            None,
            Some(Notice {
                subject: NoticeSubject::Store,
                message: format!(
                    "local store unavailable ({e}) — executions/telemetry will not be persisted \
                     this session"
                ),
            }),
        ),
    }
}

/// The session's budget guard.
///
/// A limit arms enforcement; its absence runs in observed mode, where spend is
/// measured and reported on every settled call but never blocks a step. The
/// distinction is load-bearing for a server: a host that forgot to set a
/// ceiling must still get accurate accounting.
#[must_use]
pub fn budget_guard(budget_limit_usd: Option<f64>) -> BudgetGuard {
    match budget_limit_usd {
        Some(limit) => BudgetGuard::new(BudgetMode::Enforced, None, Some(limit)),
        None => BudgetGuard::new(BudgetMode::Observed, None, None),
    }
}

/// Build the session's token-drift calibration, seeded from prior sessions'
/// telemetry for this provider/model so the estimator starts already corrected
/// instead of re-learning each session.
///
/// Best-effort like all persistence: no store, or a failed query, just means
/// starting uncalibrated at factor 1.0 — the pre-drift behavior.
#[must_use]
pub fn seed_calibration(
    store: Option<&Arc<Store>>,
    provider_id: &str,
    model_id: &str,
) -> CalibrationMap {
    let calibration = CalibrationMap::new();
    if let Some(store) = store
        && let Ok(samples) = store.drift_samples(provider_id, model_id, DRIFT_SEED_SAMPLES)
        && !samples.is_empty()
    {
        calibration.seed(model_id, &samples);
    }
    calibration
}

/// Construct the provider adapter for these parts.
///
/// The per-dialect match itself lives in `stella_model::factory`, which also
/// enforces the catalog seed floor, so a phantom model slug is a named error
/// before any wire call. What this adds is nothing — deliberately. The
/// *synced-catalog* escalation and its suggestions stay in `stella-cli`,
/// because that layer owns the on-disk catalog; a caller that wants the
/// richer diagnostic runs its own validation first and then calls this. That
/// split is why this function takes resolved parts and not a `Config`.
pub fn build_provider(parts: &ProviderParts) -> Result<Box<dyn Provider>, RuntimeError> {
    stella_model::factory::build_provider(
        &parts.factory_spec(),
        &parts.model_id,
        parts.api_key.clone(),
        parts.base_url.clone(),
        parts.base_url_override.as_deref(),
        &parts.aux,
    )
    .map_err(|reason| RuntimeError::Provider {
        provider: parts.id.clone(),
        model: parts.model_id.clone(),
        reason,
    })
}
