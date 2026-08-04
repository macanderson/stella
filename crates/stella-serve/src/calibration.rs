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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use stella_core::estimator::CalibrationMap;

/// The most distinct `provider_id`s one process will ever track.
///
/// The key is **host-supplied** — it arrives on the body of `POST /v1/turns`
/// and is never validated against a list of real providers, because there is
/// no such list: the point of the field is that the host chooses. So the key
/// space is whatever hosts type, while the entries live for the process, and
/// an unbounded map keyed on request input is a slow leak that a long-lived
/// sidecar pays for and a short-lived one never reveals.
///
/// Sixty-four is far above any real deployment — a host routes to a handful of
/// providers — and far below the point where the map or the report it feeds
/// costs anything. It bounds a mistake (an id built from a session or request
/// id, which is the realistic way this goes wrong), not a legitimate use.
const MAX_TRACKED_PROVIDERS: usize = 64;

/// Longest `provider_id` accepted as a registry key.
///
/// Separate from the count cap because the two failures are different: the
/// count bounds *how many* keys are retained, this bounds how much each one
/// costs. Without it a host could hold megabytes with a handful of very long
/// ids and never approach the count cap.
const MAX_PROVIDER_ID_LEN: usize = 128;

/// Every provider's drift state, keyed by the `provider_id` the turn declared.
#[derive(Default)]
pub(crate) struct CalibrationRegistry {
    maps: Mutex<HashMap<String, Arc<CalibrationMap>>>,
    /// Turns that asked for a map and were refused one, by either bound.
    ///
    /// Counted rather than dropped on the floor: a cap that silently stops
    /// measuring reads exactly like a provider with no drift, and "we stopped
    /// looking" must never be reported as "we looked and found nothing".
    untracked: AtomicU64,
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
    ///
    /// `None` once a bound is hit, which the caller already knows how to
    /// handle: it is the same `None` a pre-#1298 turn got, so the turn
    /// estimates uncorrected and contributes no samples. Refusing a *new*
    /// provider rather than evicting an old one is deliberate — eviction under
    /// a flood would discard exactly the well-converged maps a real deployment
    /// depends on, letting a bad id starve calibration for good ones.
    pub(crate) fn for_provider(&self, provider_id: &str) -> Option<Arc<CalibrationMap>> {
        if provider_id.is_empty() || provider_id.len() > MAX_PROVIDER_ID_LEN {
            self.untracked.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let mut maps = self.lock();
        if let Some(map) = maps.get(provider_id) {
            return Some(Arc::clone(map));
        }
        if maps.len() >= MAX_TRACKED_PROVIDERS {
            self.untracked.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        Some(Arc::clone(
            maps.entry(provider_id.to_string())
                .or_insert_with(|| Arc::new(CalibrationMap::new())),
        ))
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
        DriftReport {
            models,
            untracked_turns: self.untracked.load(Ordering::Relaxed),
        }
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
    /// Turns whose `provider_id` was refused a map by one of the registry's
    /// bounds, and which therefore ran uncalibrated and contributed no
    /// samples.
    ///
    /// Non-zero means the rows above are an incomplete picture, and says so
    /// on the same read rather than in a log nobody correlates. In a healthy
    /// deployment it stays `0` forever; a climbing count is a host minting
    /// per-request provider ids, which is a bug on the host's side that this
    /// number is the only way to see.
    pub(crate) untracked_turns: u64,
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

    /// The map a tracked provider is guaranteed to get.
    fn tracked(registry: &CalibrationRegistry, provider_id: &str) -> Arc<CalibrationMap> {
        registry
            .for_provider(provider_id)
            .expect("a provider within the registry's bounds is tracked")
    }

    /// Two providers serving the same model name must not blend — the whole
    /// reason the registry keys by provider before the map keys by model.
    #[test]
    fn providers_calibrate_independently_under_a_shared_model_name() {
        let registry = CalibrationRegistry::new();
        for _ in 0..3 {
            tracked(&registry, "vendor-a").record("shared-1", 1_000, 2_000);
            tracked(&registry, "vendor-b").record("shared-1", 1_000, 1_000);
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
        for _ in 0..3 {
            tracked(&registry, "vendor-a").record("m", 1_000, 1_500);
        }
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
        assert_eq!(
            json,
            serde_json::json!({ "models": [], "untracked_turns": 0 })
        );
    }

    /// The key is host-supplied, so the map it keys must not grow with it.
    ///
    /// Past the cap a new provider is refused rather than admitted, and the
    /// refusal is counted — the registry stops growing without ever pretending
    /// the turns it stopped measuring had no drift.
    #[test]
    fn a_flood_of_distinct_provider_ids_cannot_grow_the_registry() {
        let registry = CalibrationRegistry::new();
        for n in 0..(MAX_TRACKED_PROVIDERS * 4) {
            if let Some(map) = registry.for_provider(&format!("vendor-{n}")) {
                map.record("m", 1_000, 1_500);
            }
        }
        let report = registry.report();
        assert_eq!(
            report.models.len(),
            MAX_TRACKED_PROVIDERS,
            "the registry admitted more providers than its cap"
        );
        assert_eq!(
            report.untracked_turns,
            (MAX_TRACKED_PROVIDERS * 3) as u64,
            "every refused turn must be counted, or the cap truncates silently"
        );
    }

    /// A provider already being tracked keeps working once the cap is reached.
    ///
    /// The bound refuses *new* keys; it must never start refusing the real
    /// providers a deployment depends on, which is what evicting to make room
    /// would do.
    #[test]
    fn the_cap_refuses_newcomers_without_disturbing_the_tracked() {
        let registry = CalibrationRegistry::new();
        for n in 0..MAX_TRACKED_PROVIDERS {
            tracked(&registry, &format!("vendor-{n}"));
        }
        assert!(
            registry.for_provider("one-too-many").is_none(),
            "the cap admitted an extra provider"
        );
        tracked(&registry, "vendor-0").record("m", 1_000, 1_500);
        let report = registry.report();
        assert_eq!(report.models.len(), 1, "only vendor-0 recorded a sample");
        assert_eq!(report.models[0].provider_id, "vendor-0");
    }

    /// Length is bounded separately from count: a handful of very long ids
    /// would otherwise hold megabytes while never approaching the cap.
    #[test]
    fn an_oversized_or_empty_provider_id_is_refused() {
        let registry = CalibrationRegistry::new();
        assert!(registry.for_provider("").is_none(), "empty id was tracked");
        assert!(
            registry
                .for_provider(&"v".repeat(MAX_PROVIDER_ID_LEN + 1))
                .is_none(),
            "oversized id was tracked"
        );
        assert!(
            registry
                .for_provider(&"v".repeat(MAX_PROVIDER_ID_LEN))
                .is_some(),
            "an id exactly at the limit must still be tracked"
        );
        assert_eq!(registry.report().untracked_turns, 2);
    }
}
