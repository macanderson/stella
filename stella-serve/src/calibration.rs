// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Token-drift calibration for served turns, and the read-only report behind
//! `GET /v1/calibration` (#1298).
//!
//! The engine estimates a request's input tokens before it makes the call and
//! compares that against what the provider says it actually billed
//! (`stella_core::estimator`). Two things come out of the comparison: a
//! bounded correction that feeds the compaction decision, and a *measurement*
//! of how wrong the estimate was. Until now a served turn got neither — every
//! served turn estimated uncorrected, and the gap was computed nowhere, so a
//! host embedding Stella believed its own cost numbers with nothing able to
//! contradict them.
//!
//! # Why the maps are keyed by provider
//!
//! [`CalibrationMap`] keys by the model string the provider reports, and says
//! so: the provider dimension is handled "where samples are persisted", which
//! for the CLI is `stella-store` keying telemetry by `(provider, model)`. This
//! crate deliberately has no store, so that half has to live here — hence a
//! map *per `provider_id`*, minted on first use. Without it two hosts pointing
//! `openai` and `openai-compatible-proxy` at the same model name would blend
//! their tokenizers' drift into one average that describes neither.
//!
//! # Lifetime, stated rather than discovered
//!
//! Process memory, for the process's life. There is nothing to seed from at
//! boot and nothing written at shutdown, so a redeployed sidecar starts
//! uncalibrated and re-converges — a handful of steps, since the EWMA weights
//! recent samples heavily. That is the honest consequence of a crate with no
//! persistence, and it is why the report carries `samples`: a row with three
//! samples and a row with three thousand are not the same claim, and a reader
//! must be able to tell them apart.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use stella_core::estimator::CalibrationMap;

/// Every provider's drift state, keyed by the `provider_id` the turn declared.
#[derive(Default)]
pub(crate) struct CalibrationRegistry {
    maps: Mutex<HashMap<String, Arc<CalibrationMap>>>,
}

impl CalibrationRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The map this provider's turns calibrate against, minted on first use.
    ///
    /// Handed out as an `Arc` because the engine borrows it for the whole turn
    /// on the session thread while `GET /v1/calibration` reads it from the
    /// server runtime — the map's own `Mutex` (never held across an await)
    /// makes that concurrent access sound.
    pub(crate) fn for_provider(&self, provider_id: &str) -> Arc<CalibrationMap> {
        Arc::clone(
            self.lock()
                .entry(provider_id.to_string())
                .or_insert_with(|| Arc::new(CalibrationMap::new())),
        )
    }

    /// The whole report, sorted by provider then model.
    ///
    /// Sorted for the same reason `CalibrationMap::report` sorts: two reads of
    /// an unchanged registry must produce identical bytes, or the endpoint is
    /// useless for diffing and awkward to test.
    pub(crate) fn report(&self) -> DriftReport {
        let snapshot: Vec<(String, Arc<CalibrationMap>)> = {
            let maps = self.lock();
            let mut pairs: Vec<(String, Arc<CalibrationMap>)> = maps
                .iter()
                .map(|(provider, map)| (provider.clone(), Arc::clone(map)))
                .collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            pairs
        };
        // The per-map reads happen with the registry lock released: a report
        // takes each model map's own lock, and holding both would put a
        // request handler on the inside of a lock a running turn takes on
        // every committed step.
        let models = snapshot
            .into_iter()
            .flat_map(|(provider_id, map)| {
                map.report().into_iter().map(move |row| ModelDriftWire {
                    provider_id: provider_id.clone(),
                    model: row.model,
                    samples: row.samples,
                    estimated_input_tokens: row.estimated_input_tokens,
                    actual_input_tokens: row.actual_input_tokens,
                    drift_ratio: row.drift_ratio,
                    applied_factor: row.applied_factor,
                })
            })
            .collect();
        DriftReport { models }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<CalibrationMap>>> {
        // Poisoning means a panic while minting a map. The state is a
        // `HashMap` of `Arc`s that cannot be left torn, and refusing to
        // calibrate afterwards would turn one panic into a permanently blind
        // server — the same call the map itself makes.
        self.maps.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// The `GET /v1/calibration` body.
///
/// Counts and identifiers only — model ids, sample counts, token totals and
/// two ratios. Nothing here is derived from transcript content, which is what
/// makes a read-only endpoint the right shape for it: the report describes
/// Stella's own estimator, not the conversation.
#[derive(Debug, Serialize)]
pub(crate) struct DriftReport {
    /// One row per `(provider_id, model)` the process has seen. Empty before
    /// any turn has committed a step — an honest "nothing measured yet",
    /// which is not the same as "no drift".
    pub(crate) models: Vec<ModelDriftWire>,
}

/// One `(provider_id, model)` row of [`DriftReport`].
#[derive(Debug, Serialize)]
pub(crate) struct ModelDriftWire {
    provider_id: String,
    model: String,
    samples: u32,
    estimated_input_tokens: u64,
    actual_input_tokens: u64,
    /// `actual / estimated` over this row's whole history — the gap itself,
    /// unsmoothed and unclamped. Above 1.0 means Stella under-estimated what
    /// the provider billed.
    drift_ratio: f64,
    /// The bounded correction actually multiplied into estimates, which is
    /// deliberately *not* `drift_ratio`: it is smoothed and clamped, and stays
    /// exactly 1.0 until the sample floor is cleared.
    applied_factor: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two providers serving the same model name must not blend — the whole
    /// reason the registry keys by provider before the map keys by model.
    #[test]
    fn providers_calibrate_independently_under_a_shared_model_name() {
        let registry = CalibrationRegistry::new();
        for _ in 0..3 {
            registry
                .for_provider("vendor-a")
                .record("shared-1", 1_000, 2_000);
            registry
                .for_provider("vendor-b")
                .record("shared-1", 1_000, 1_000);
        }
        let report = registry.report();
        assert_eq!(report.models.len(), 2);
        assert_eq!(report.models[0].provider_id, "vendor-a");
        assert!((report.models[0].drift_ratio - 2.0).abs() < 1e-9);
        assert_eq!(report.models[1].provider_id, "vendor-b");
        assert!((report.models[1].drift_ratio - 1.0).abs() < 1e-9);
    }

    /// The same `provider_id` must reach the same state across turns, or a
    /// session's calibration would reset every time a turn started.
    #[test]
    fn one_map_per_provider_is_shared_across_turns() {
        let registry = CalibrationRegistry::new();
        registry.for_provider("vendor-a").record("m", 1_000, 1_500);
        registry.for_provider("vendor-a").record("m", 1_000, 1_500);
        registry.for_provider("vendor-a").record("m", 1_000, 1_500);
        let report = registry.report();
        assert_eq!(report.models.len(), 1);
        assert_eq!(report.models[0].samples, 3);
        assert!((report.models[0].applied_factor - 1.5).abs() < 1e-9);
    }

    /// A server that has run nothing reports an empty list, not zeros that
    /// would read as "measured, and perfect".
    #[test]
    fn a_fresh_registry_reports_no_rows() {
        let json = serde_json::to_value(CalibrationRegistry::new().report()).unwrap();
        assert_eq!(json, serde_json::json!({ "models": [] }));
    }
}
