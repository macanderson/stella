//! The versioned drain wire contract — what a cloud syncer ships off the hub.
//!
//! The hub (`~/.stella/usage.db`) stages org-scoped, content-free telemetry
//! rows for upload (`UsageStore::cloud_pending`). A drain — the native syncer
//! built in #404, or the OTLP encoder in #427 — pages those rows and ships them
//! to a remote intake. This module defines the *native* (`format = "stella"`)
//! wire payload and stamps it with a **schema version** so the intake can tell
//! client versions apart as the payload evolves.
//!
//! # Why version now
//!
//! The [`DrainRow`] field set is a contract between every deployed client and
//! the intake. Once real clients are in the field, renaming a key or changing a
//! field's meaning silently breaks older/newer peers. A version stamp is cheap
//! to add before the transport lands (#404) and expensive to retrofit after —
//! the same reasoning that shipped the drain config's `format` discriminator
//! ahead of the OTLP encoder (#427). Ship the seam now, fill it in later.
//!
//! # Compatibility contract
//!
//! - Every native batch carries [`DrainBatch::schema_version`]; the version
//!   this build speaks is [`DRAIN_SCHEMA_VERSION`].
//! - The OTLP export (#427) carries the same value as a resource/scope
//!   attribute keyed [`OTEL_SCHEMA_VERSION_ATTR`], so a collector reads the
//!   version the same way regardless of format.
//! - The intake accepts a **known version range** — [`schema_version_supported`]
//!   is the single source of truth. An unknown version is a **terminal,
//!   non-poison reject**: the batch is malformed-by-contract, not a transient
//!   failure, so the drain loop (#467) must classify it as terminal (do not
//!   retry forever) — yet distinct from a poison *row*, because the whole batch
//!   shares one version and bisecting rows cannot rescue it. That distinction
//!   is now [`RejectionClass::TerminalBatch`] vs [`RejectionClass::TerminalRow`].
//!
//! # Poison-row resilience (#467)
//!
//! The cursor is monotonic and advances only on a confirmed ack, so one row the
//! intake refuses *forever* would wedge every newer row for that org — an
//! offline-forever org from one bad row. [`drain_org`] closes that hole:
//! rejections are classified ([`DrainRejection`]), a terminal row-attributable
//! rejection is bisected down to the exact offending row, that row is
//! dead-lettered with its reason
//! ([`crate::usage::UsageStore::quarantine_cloud_row`]), and the cursor steps
//! over it so the org keeps draining. The intake is an injected port
//! ([`CloudIntake`]) — the transport itself lands with #404.
//!
//! # Bump-on-change discipline
//!
//! Any change to [`DrainRow`]'s field set or a field's semantics **must**
//! increment [`DRAIN_SCHEMA_VERSION`] and widen [`schema_version_supported`]
//! to match. The `wire_contract_is_frozen` test pins the serialized key set to
//! this version: change the fields without bumping and it fails, forcing the
//! version bump into the same change rather than a silent wire break.
//!
//! # Content-free by construction (#466)
//!
//! [`DrainRow`] enumerates only identity + addressing + telemetry keys — never
//! prompt or completion text. It deliberately drops internal-only columns the
//! hub row carries: the cursor address (`hub_rowid`), execution grouping
//! (`execution_id`, `step`, `call_role`), and the local drift-analysis estimate
//! (`estimated_input_tokens`) are not part of the enterprise rollup contract.
//! Adding any of them is a v2 decision made on purpose, not a side effect of a
//! blanket `derive(Serialize)` over the internal struct.

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::usage::{CloudTelemetryEvent, QuarantineReason, UsageStore};

// The OTLP (`format = "otel"`) encoding of the same staged rows (#427).
pub mod otel;

/// The native drain wire-format version this build speaks. Increment on any
/// change to [`DrainRow`]'s field set or a field's semantics (see the module
/// docs' bump-on-change discipline).
pub const DRAIN_SCHEMA_VERSION: u32 = 1;

/// Oldest wire version this build's intake contract still accepts.
pub const MIN_SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Newest wire version this build understands — always [`DRAIN_SCHEMA_VERSION`].
pub const MAX_SUPPORTED_SCHEMA_VERSION: u32 = DRAIN_SCHEMA_VERSION;

/// OTLP resource/scope attribute key carrying [`DrainBatch::schema_version`] on
/// the `format = "otel"` export (#427). The native and OTLP encoders stamp the
/// same value under this key so a collector can tell client versions apart.
pub const OTEL_SCHEMA_VERSION_ATTR: &str = "stella.schema_version";

/// Whether the intake accepts wire version `v` — the single source of truth for
/// the "known version range" half of the compatibility contract. `false` is a
/// **terminal, non-poison** reject (see the module docs), never a transient
/// error the drain loop should retry.
pub fn schema_version_supported(v: u32) -> bool {
    (MIN_SUPPORTED_SCHEMA_VERSION..=MAX_SUPPORTED_SCHEMA_VERSION).contains(&v)
}

/// The native (`format = "stella"`) batch envelope: a schema-versioned run of
/// content-free telemetry rows. This is the wire payload a cloud syncer (#404)
/// POSTs to the org intake; the intake reads [`Self::schema_version`] first and
/// rejects an unsupported version before decoding [`Self::rows`].
///
/// The version is an **integer** so the intake can accept a *range*
/// ([`schema_version_supported`]) and bump-on-change is a simple increment —
/// distinct from the enterprise spool's string `schema` discriminator
/// (`"stella.operational.batch.v1"`), a different sink with a different
/// (single-id, not range) contract. `deny_unknown_fields` matches that sink's
/// wire posture: within a supported version the field set is exact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrainBatch {
    /// Wire-format version — see [`DRAIN_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The staged hub rows, oldest first (the order `cloud_pending` returns).
    pub rows: Vec<DrainRow>,
}

impl DrainBatch {
    /// Wrap a page of hub rows in the current-version envelope — the native
    /// encoder the `format = "stella"` drain (#404) serializes and POSTs.
    pub fn from_events(events: &[CloudTelemetryEvent]) -> Self {
        Self {
            schema_version: DRAIN_SCHEMA_VERSION,
            rows: events.iter().map(DrainRow::from).collect(),
        }
    }
}

/// One telemetry row on the wire: org/workspace/repo/project identity, the
/// `(workspace_id, source_rowid)` idempotency key, `recorded_at`, and the
/// content-free per-call telemetry. Content-free by construction (#466) — this
/// field set *is* what [`DRAIN_SCHEMA_VERSION`] pins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrainRow {
    // --- identity ---
    pub org_id: String,
    pub workspace_id: Option<String>,
    pub repo_id: String,
    pub project_id: String,

    // --- addressing / idempotency ---
    /// Per-project source rowid. `(workspace_id, source_rowid)` is the intake's
    /// dedup key, so a re-send after a lost ack lands idempotently (#404).
    pub source_rowid: i64,
    pub recorded_at: String,

    // --- telemetry (content-free) ---
    pub provider: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_miss_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: f64,
    pub duration_ms: u64,
    pub retries: u32,
    pub tool_calls: u64,
    pub usage_complete: bool,
}

impl From<&CloudTelemetryEvent> for DrainRow {
    fn from(e: &CloudTelemetryEvent) -> Self {
        let t = &e.telemetry;
        Self {
            org_id: e.org_id.clone(),
            workspace_id: e.workspace_id.clone(),
            repo_id: e.repo_id.clone(),
            project_id: e.project_id.clone(),
            source_rowid: e.source_rowid,
            recorded_at: e.recorded_at.clone(),
            provider: t.provider.clone(),
            model: t.model.clone(),
            input_tokens: t.input_tokens,
            output_tokens: t.output_tokens,
            cache_read_tokens: t.cache_read_tokens,
            cache_miss_tokens: t.cache_miss_tokens,
            cache_write_tokens: t.cache_write_tokens,
            cost_usd: t.cost_usd,
            duration_ms: t.duration_ms,
            retries: t.retries,
            tool_calls: t.tool_calls,
            usage_complete: t.usage_complete,
        }
    }
}

// ---------------------------------------------------------------------------
// Poison-row resilience (#467): rejection classification, the intake port, and
// the bisecting drain loop.
// ---------------------------------------------------------------------------

/// How the drain must react to an intake refusing a batch.
///
/// Typed rather than stringly-typed because the three arms have *opposite*
/// correct behaviors, and picking the wrong one is a data-loss or
/// availability bug:
///
/// - [`Self::Transient`] — retry with the existing backoff. Quarantining here
///   would dead-letter perfectly good rows over a blip.
/// - [`Self::TerminalBatch`] — permanent, but a property of the *batch or the
///   client*, not of any row: bad credentials, wrong endpoint, an unsupported
///   [`DrainBatch::schema_version`]. Bisecting cannot rescue it and every row
///   would look equally "poison", so the drain stops and surfaces it instead of
///   silently dead-lettering the org's whole backlog.
/// - [`Self::TerminalRow`] — permanent and attributable to row content the
///   intake validated and refused. This is the poison row: bisect, quarantine,
///   step the cursor over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionClass {
    /// Retry later with the existing backoff (5xx, network, timeout, throttle).
    Transient,
    /// Permanent and batch-wide — never attributable to one row.
    TerminalBatch,
    /// Permanent and attributable to one row's contents (validation reject).
    TerminalRow,
}

impl RejectionClass {
    /// Whether the drain should keep retrying this batch unchanged.
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::Transient)
    }
}

/// One intake refusal: its class plus the diagnostic to surface or persist.
///
/// `detail` is the intake's own message — data, not an error type — and is the
/// only free-form field; the *decision* is carried by the typed
/// [`Self::class`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainRejection {
    pub class: RejectionClass,
    /// HTTP status, when the refusal arrived over HTTP. `None` for transport
    /// failures (DNS, TLS, connect/read timeout) that never produced a status.
    pub http_status: Option<u16>,
    /// The intake's diagnostic.
    pub detail: String,
}

impl DrainRejection {
    /// A refusal to retry with backoff — 5xx, throttle, or a transport failure
    /// that never produced a status.
    pub fn transient(http_status: Option<u16>, detail: impl Into<String>) -> Self {
        Self {
            class: RejectionClass::Transient,
            http_status,
            detail: detail.into(),
        }
    }

    /// A permanent refusal of the whole batch — auth, endpoint, or an
    /// unsupported [`DrainBatch::schema_version`]. Never quarantines a row.
    pub fn terminal_batch(http_status: Option<u16>, detail: impl Into<String>) -> Self {
        Self {
            class: RejectionClass::TerminalBatch,
            http_status,
            detail: detail.into(),
        }
    }

    /// A permanent refusal attributable to row contents — the poison row the
    /// drain bisects for and dead-letters.
    pub fn terminal_row(http_status: Option<u16>, detail: impl Into<String>) -> Self {
        Self {
            class: RejectionClass::TerminalRow,
            http_status,
            detail: detail.into(),
        }
    }

    /// Classify an HTTP status the intake returned.
    ///
    /// The split is deliberately conservative in both directions, because both
    /// mistakes are expensive:
    ///
    /// - `408` / `429` / `5xx` are **transient**. A 4xx that means "slow down"
    ///   or "try again" must never dead-letter a row.
    /// - `400` / `422` are **terminal-row**: the intake parsed the payload and
    ///   refused its contents. This is the only class that quarantines.
    /// - Every other permanent 4xx (`401`, `403`, `404`, `409`, `413`, `426`, …)
    ///   is **terminal-batch**. A revoked token or a mistyped endpoint returns
    ///   4xx for *every* batch; treating that as a poison row would quietly
    ///   dead-letter the org's entire backlog one row at a time. Stopping and
    ///   surfacing it is the safe failure.
    /// - `1xx`/`2xx`/`3xx` are not refusals; a caller that reaches here with one
    ///   has an unexpected response, classified transient so nothing is dropped.
    pub fn from_http_status(status: u16, detail: impl Into<String>) -> Self {
        let class = match status {
            408 | 429 => RejectionClass::Transient,
            400 | 422 => RejectionClass::TerminalRow,
            500..=599 => RejectionClass::Transient,
            401..=499 => RejectionClass::TerminalBatch,
            _ => RejectionClass::Transient,
        };
        Self {
            class,
            http_status: Some(status),
            detail: detail.into(),
        }
    }

    /// The dead-letter reason for this rejection — `Some` **only** for
    /// [`RejectionClass::TerminalRow`].
    ///
    /// This is the single gate between "the intake said no" and "a row is
    /// quarantined and the cursor steps over it": a transient blip or a
    /// batch-wide refusal cannot produce a [`QuarantineReason`], so it cannot
    /// reach [`crate::usage::UsageStore::quarantine_cloud_row`] by accident.
    pub fn quarantine_reason(&self) -> Option<QuarantineReason> {
        (self.class == RejectionClass::TerminalRow)
            .then(|| QuarantineReason::terminal(self.http_status, &self.detail))
    }
}

/// The remote org intake, as a **port**.
///
/// The transport (HTTPS, auth, backoff) lands with #404; this trait is the seam
/// the drain loop talks to, so poison-row handling is real, tested behavior
/// today — a test drives terminal and transient refusals with no network at
/// all, and #404 slots its client in as one more adapter.
///
/// `Ok(())` is a **confirmed ack** for every row in the batch: the drain treats
/// it as durable receipt and advances the cursor. An implementation that
/// returns `Ok` optimistically breaks at-least-once delivery.
///
/// Batches are re-sent during bisection, so an implementation must tolerate
/// duplicates — the wire contract's `(workspace_id, source_rowid)` dedup key
/// makes that safe by construction.
pub trait CloudIntake {
    /// Deliver one batch, or say why it was refused.
    fn deliver(&self, batch: &DrainBatch) -> std::result::Result<(), DrainRejection>;
}

/// What one [`drain_org`] pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrainOutcome {
    /// Rows the intake confirmed. Counts each hub row once even though
    /// bisection may have re-sent it (the intake dedups).
    pub delivered: u64,
    /// Rows dead-lettered as poison and stepped over.
    pub quarantined: u64,
    /// Batches the intake accepted (excludes bisection probes).
    pub batches: u64,
    /// Why the pass stopped short of draining the backlog, if it did. `None`
    /// means the org has no pending rows left. A [`RejectionClass::Transient`]
    /// stop is the caller's cue to back off and retry; a
    /// [`RejectionClass::TerminalBatch`] stop needs an operator.
    pub stopped_by: Option<DrainRejection>,
}

impl DrainOutcome {
    /// Whether the backlog was fully drained (nothing left, no stop).
    pub fn is_complete(&self) -> bool {
        self.stopped_by.is_none()
    }
}

/// Drain one org's pending hub rows to `intake`, isolating poison rows so a
/// permanently-rejected row can never stall the cursor (#467).
///
/// Pages `batch_size` rows at a time from
/// [`UsageStore::cloud_pending`](crate::usage::UsageStore::cloud_pending) and
/// ships each page. On success the cursor advances to the page's last row. On
/// refusal:
///
/// - **transient** → stop and report; the caller retries with its backoff.
///   Nothing is quarantined and the cursor does not move.
/// - **terminal-batch** → stop and report. No row is attributable, so
///   dead-lettering any would be silent loss.
/// - **terminal-row** → `bisect` the page down to the exact row, dead-letter
///   it with its reason, and advance the cursor past both the delivered prefix
///   and the poison row — in one transaction — then keep draining. The org's
///   newer rows flow again.
///
/// The cursor invariant is untouched: every advance goes through the same
/// monotonic `MAX()` upsert, so it never rewinds.
///
/// `batch_size` is clamped to at least 1.
pub fn drain_org(
    hub: &UsageStore,
    org_id: &str,
    intake: &dyn CloudIntake,
    batch_size: usize,
) -> Result<DrainOutcome> {
    let batch_size = batch_size.max(1);
    let mut outcome = DrainOutcome::default();
    loop {
        let page = hub.cloud_pending(org_id, batch_size)?;
        if page.is_empty() {
            return Ok(outcome);
        }
        match intake.deliver(&DrainBatch::from_events(&page)) {
            Ok(()) => {
                let last = page[page.len() - 1].hub_rowid;
                hub.ack_cloud_synced(org_id, last)?;
                outcome.delivered += page.len() as u64;
                outcome.batches += 1;
            }
            Err(rejection) => {
                let Some(reason) = rejection.quarantine_reason() else {
                    // Transient or batch-wide: nothing is attributable to a
                    // row, so nothing is dead-lettered and the cursor holds.
                    outcome.stopped_by = Some(rejection);
                    return Ok(outcome);
                };
                match bisect(&page, intake, reason) {
                    Bisection::Poison { index, reason } => {
                        // `index` is also the confirmed-delivered prefix length,
                        // and rows are rowid-ascending, so the poison row's own
                        // `hub_rowid` is the single monotone advance that steps
                        // over the prefix *and* the poison row. One transaction:
                        // the row can never be skipped without a record.
                        hub.quarantine_cloud_row(&page[index], &reason, page[index].hub_rowid)?;
                        outcome.delivered += index as u64;
                        outcome.quarantined += 1;
                    }
                    Bisection::Aborted {
                        rejection,
                        delivered_prefix,
                    } => {
                        // Bank whatever the probes confirmed before banking the
                        // failure, so a mid-bisect blip still makes progress.
                        if delivered_prefix > 0 {
                            hub.ack_cloud_synced(org_id, page[delivered_prefix - 1].hub_rowid)?;
                            outcome.delivered += delivered_prefix as u64;
                        }
                        outcome.stopped_by = Some(rejection);
                        return Ok(outcome);
                    }
                }
            }
        }
    }
}

/// The result of narrowing a terminally-rejected page down to one row.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Bisection {
    /// `events[index]` is the poison row. `index` doubles as the length of the
    /// prefix the intake confirmed while searching.
    Poison {
        index: usize,
        reason: QuarantineReason,
    },
    /// A probe hit a transient or batch-wide failure; the search cannot
    /// conclude, so nothing is dead-lettered.
    Aborted {
        rejection: DrainRejection,
        delivered_prefix: usize,
    },
}

/// Pinpoint the first poison row in a page the intake terminally row-rejected.
///
/// Binary-searches the **prefix length**: the smallest `k` in `1..=n` for which
/// `events[..k]` is still terminally row-rejected. Row validation is per-row, so
/// rejection is monotone in prefix length — a prefix containing the poison row
/// rejects, and every longer one does too — which makes the search valid at
/// `O(log n)` probes instead of `n`.
///
/// Searching prefixes rather than arbitrary halves is what makes the answer
/// usable by a **monotonic** cursor. The loop invariant is that `lo - 1` is
/// always a prefix length the intake just confirmed, so on termination
/// `events[..k-1]` are delivered and `events[k-1]` is the poison row — one
/// contiguous, rowid-ascending run the cursor steps over in a single advance.
/// Re-sending an already-accepted prefix is safe by the wire contract's
/// `(workspace_id, source_rowid)` dedup key.
///
/// `initial_reason` is the full-page refusal, which is the caller's
/// precondition: `events` as a whole was terminally row-rejected. It stands as
/// the answer when `events` holds a single row (no probe needed).
fn bisect(
    events: &[CloudTelemetryEvent],
    intake: &dyn CloudIntake,
    initial_reason: QuarantineReason,
) -> Bisection {
    debug_assert!(!events.is_empty(), "bisect requires a non-empty page");
    let mut lo = 1usize;
    let mut hi = events.len();
    // The refusal that pinpointed the current `hi` prefix.
    let mut culprit = initial_reason;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        match intake.deliver(&DrainBatch::from_events(&events[..mid])) {
            // `lo - 1` stays a confirmed-delivered prefix length.
            Ok(()) => lo = mid + 1,
            Err(r) if r.class == RejectionClass::TerminalRow => {
                culprit = QuarantineReason::terminal(r.http_status, &r.detail);
                hi = mid;
            }
            Err(rejection) => {
                return Bisection::Aborted {
                    rejection,
                    delivered_prefix: lo - 1,
                };
            }
        }
    }
    // `lo == hi == k`, and `k <= events.len()` because every probe uses
    // `mid < hi`: `events[..k-1]` were confirmed, `events[k-1]` is poison.
    Bisection::Poison {
        index: lo - 1,
        reason: culprit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TelemetryRow;
    use serde_json::Value;

    /// A hub row whose internal-only ids differ from its wire ids, so the tests
    /// can prove the wire carries `source_rowid` (the dedup key), not the
    /// internal `hub_rowid`.
    fn sample_event() -> CloudTelemetryEvent {
        CloudTelemetryEvent {
            hub_rowid: 42,
            org_id: "acme".into(),
            workspace_id: Some("ws-1".into()),
            repo_id: "repo-1".into(),
            project_id: "proj-1".into(),
            execution_id: 7,
            source_rowid: 99,
            recorded_at: "2026-07-23T00:00:00Z".into(),
            telemetry: TelemetryRow {
                step: 3,
                provider: "zai".into(),
                call_role: "lead".into(),
                model: "glm-5.2".into(),
                input_tokens: 100,
                estimated_input_tokens: 90,
                output_tokens: 20,
                cache_read_tokens: 10,
                cache_miss_tokens: 5,
                cache_write_tokens: 2,
                cost_usd: 0.01,
                duration_ms: 1234,
                retries: 1,
                tool_calls: 4,
                usage_complete: true,
            },
        }
    }

    /// The frozen wire-contract field set for schema v1. Editing this list
    /// without bumping [`DRAIN_SCHEMA_VERSION`] is exactly what
    /// `wire_contract_is_frozen` catches. Compared as a set (order-independent).
    const V1_ROW_KEYS: &[&str] = &[
        "org_id",
        "workspace_id",
        "repo_id",
        "project_id",
        "source_rowid",
        "recorded_at",
        "provider",
        "model",
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_miss_tokens",
        "cache_write_tokens",
        "cost_usd",
        "duration_ms",
        "retries",
        "tool_calls",
        "usage_complete",
    ];

    #[test]
    fn batch_carries_schema_version() {
        let batch = DrainBatch::from_events(&[sample_event()]);
        assert_eq!(batch.schema_version, DRAIN_SCHEMA_VERSION);
        let v: Value = serde_json::to_value(&batch).unwrap();
        assert_eq!(v["schema_version"], DRAIN_SCHEMA_VERSION);
        assert_eq!(v["rows"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn wire_contract_is_frozen() {
        // Serialize a row and assert its key set equals the frozen v1 allowlist.
        // This is the bump-on-change guard: add / remove / rename a DrainRow
        // field and this fails until V1_ROW_KEYS is updated *and* the version
        // bumped in the same change.
        let row = DrainRow::from(&sample_event());
        let obj = serde_json::to_value(&row).unwrap();
        let obj = obj.as_object().unwrap();
        let mut got: Vec<&str> = obj.keys().map(String::as_str).collect();
        got.sort_unstable();
        let mut want: Vec<&str> = V1_ROW_KEYS.to_vec();
        want.sort_unstable();
        assert_eq!(
            got, want,
            "DrainRow wire keys drifted from the v1 contract — bump \
             DRAIN_SCHEMA_VERSION and update V1_ROW_KEYS in the same change"
        );
    }

    #[test]
    fn drain_row_excludes_internal_fields() {
        // The internal columns the hub row carries but the contract excludes
        // must never leak onto the wire (cursor address, execution grouping,
        // local drift estimate).
        let obj = serde_json::to_value(DrainRow::from(&sample_event())).unwrap();
        let obj = obj.as_object().unwrap();
        for excluded in [
            "hub_rowid",
            "execution_id",
            "step",
            "call_role",
            "estimated_input_tokens",
        ] {
            assert!(
                !obj.contains_key(excluded),
                "internal field `{excluded}` leaked onto the wire"
            );
        }
    }

    #[test]
    fn source_rowid_is_the_dedup_key() {
        // The wire carries source_rowid (the intake's (workspace_id,
        // source_rowid) dedup key), not the hub's internal rowid.
        let e = sample_event();
        assert_ne!(
            e.source_rowid, e.hub_rowid,
            "fixture must distinguish the two ids for this test to mean anything"
        );
        assert_eq!(DrainRow::from(&e).source_rowid, e.source_rowid);
    }

    #[test]
    fn round_trips_through_json() {
        let batch = DrainBatch::from_events(&[sample_event(), sample_event()]);
        let bytes = serde_json::to_vec(&batch).unwrap();
        let back: DrainBatch = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(batch, back);
    }

    #[test]
    fn version_support_range_is_closed_and_current() {
        assert!(schema_version_supported(DRAIN_SCHEMA_VERSION));
        assert!(!schema_version_supported(0));
        assert!(
            !schema_version_supported(DRAIN_SCHEMA_VERSION + 1),
            "an unknown newer version must be an explicit (terminal) reject"
        );
    }

    // ---- poison-row resilience (#467) --------------------------------------

    mod poison {
        use std::cell::RefCell;
        use std::collections::VecDeque;

        use super::*;
        use crate::identity::TelemetryScope;
        use crate::usage::UsageStore;

        const ORG: &str = "acme";

        /// An intake driven entirely from memory — the injected port that lets
        /// these tests exercise terminal and transient refusals with no network
        /// and no #404 transport. Its refusal message deliberately does **not**
        /// name the offending row, so the bisect has to actually find it.
        struct FakeIntake {
            /// `source_rowid`s this intake permanently refuses as invalid.
            poison: Vec<i64>,
            /// Refusals returned ahead of any row inspection, oldest first —
            /// how a test scripts a transient blip or an auth failure.
            scripted: RefCell<VecDeque<DrainRejection>>,
            /// `source_rowid`s the intake confirmed, in delivery order
            /// (duplicates included — bisection re-sends prefixes).
            accepted: RefCell<Vec<i64>>,
            /// Every batch size the intake was handed, in order.
            batch_sizes: RefCell<Vec<usize>>,
        }

        impl FakeIntake {
            fn new(poison: &[i64]) -> Self {
                Self {
                    poison: poison.to_vec(),
                    scripted: RefCell::new(VecDeque::new()),
                    accepted: RefCell::new(Vec::new()),
                    batch_sizes: RefCell::new(Vec::new()),
                }
            }

            fn scripting(poison: &[i64], scripted: Vec<DrainRejection>) -> Self {
                let intake = Self::new(poison);
                *intake.scripted.borrow_mut() = scripted.into();
                intake
            }

            /// Distinct `source_rowid`s the intake confirmed, ascending.
            fn confirmed(&self) -> Vec<i64> {
                let mut v = self.accepted.borrow().clone();
                v.sort_unstable();
                v.dedup();
                v
            }

            fn calls(&self) -> usize {
                self.batch_sizes.borrow().len()
            }
        }

        impl CloudIntake for FakeIntake {
            fn deliver(&self, batch: &DrainBatch) -> std::result::Result<(), DrainRejection> {
                self.batch_sizes.borrow_mut().push(batch.rows.len());
                if let Some(scripted) = self.scripted.borrow_mut().pop_front() {
                    return Err(scripted);
                }
                if !schema_version_supported(batch.schema_version) {
                    return Err(DrainRejection::terminal_batch(
                        Some(400),
                        format!("unsupported schema_version {}", batch.schema_version),
                    ));
                }
                if batch
                    .rows
                    .iter()
                    .any(|r| self.poison.contains(&r.source_rowid))
                {
                    // A real intake reports *that* validation failed, not which
                    // row — pinpointing it is the drain's job.
                    return Err(DrainRejection::from_http_status(
                        400,
                        "schema validation failed for one or more rows",
                    ));
                }
                self.accepted
                    .borrow_mut()
                    .extend(batch.rows.iter().map(|r| r.source_rowid));
                Ok(())
            }
        }

        fn hub_with(source_rowids: &[i64]) -> UsageStore {
            let hub = UsageStore::in_memory().expect("hub");
            let scope = TelemetryScope {
                org_id: Some(ORG.into()),
                workspace_id: Some("ws-1".into()),
                repo_id: "repo01".into(),
                project_id: "proj_a".into(),
            };
            let rows: Vec<crate::SourceTelemetryRow> = source_rowids
                .iter()
                .map(|id| crate::SourceTelemetryRow {
                    source_rowid: *id,
                    execution_id: 1,
                    recorded_at: "2026-07-23T10:00:00Z".into(),
                    telemetry: TelemetryRow {
                        step: *id as u64,
                        provider: "zai".into(),
                        call_role: "engine".into(),
                        model: "glm-5.2".into(),
                        input_tokens: 1_000,
                        estimated_input_tokens: 900,
                        output_tokens: 100,
                        cache_read_tokens: 0,
                        cache_miss_tokens: 0,
                        cache_write_tokens: 0,
                        cost_usd: 0.01,
                        duration_ms: 1_200,
                        retries: 0,
                        tool_calls: 0,
                        usage_complete: true,
                    },
                })
                .collect();
            hub.replicate_telemetry(&scope, &rows).expect("replicate");
            hub
        }

        /// Hub `source_rowid`s still awaiting the cloud for this org.
        fn pending(hub: &UsageStore) -> Vec<i64> {
            hub.cloud_pending(ORG, 100)
                .expect("pending")
                .iter()
                .map(|e| e.source_rowid)
                .collect()
        }

        /// ACCEPTANCE (a) + (b): one terminally-rejected row must not stall the
        /// org's newer rows, the quarantined row must be visible with its
        /// reason, and the cursor must have advanced past it.
        ///
        /// On the pre-#467 drain this hangs the org forever: the cursor only
        /// moves on a confirmed ack, so rows 3–5 never ship.
        #[test]
        fn a_poison_row_does_not_stall_the_org_cursor() {
            let hub = hub_with(&[1, 2, 3, 4, 5]);
            let intake = FakeIntake::new(&[2]);

            let outcome = drain_org(&hub, ORG, &intake, 5).expect("drain");

            assert_eq!(outcome.quarantined, 1);
            assert_eq!(outcome.delivered, 4, "every non-poison row is delivered");
            assert!(outcome.is_complete(), "the backlog drained: {outcome:?}");
            assert_eq!(
                intake.confirmed(),
                vec![1, 3, 4, 5],
                "the org's newer rows keep draining past the poison row"
            );

            // (b) the cursor advanced past the poison row — nothing is left
            // pending, and it never rewinds on a re-drain.
            assert!(pending(&hub).is_empty(), "cursor advanced past the poison");
            let again = drain_org(&hub, ORG, &intake, 5).expect("re-drain");
            assert_eq!(again.delivered, 0);
            assert_eq!(again.quarantined, 0);

            // (b) the quarantined row is visible: count + reason.
            assert_eq!(hub.cloud_quarantine_count(Some(ORG)).unwrap(), 1);
            let reasons = hub.cloud_quarantine_reasons(Some(ORG)).unwrap();
            assert_eq!(reasons.len(), 1);
            assert_eq!(reasons[0].1, Some(400), "the intake's status is retained");
            assert_eq!(reasons[0].2, 1);
            assert!(
                reasons[0].0.contains("schema validation failed"),
                "the intake's reason is retained: {:?}",
                reasons[0].0
            );

            // ...and it is retained for inspection, not silently dropped.
            let rows = hub.cloud_quarantined(Some(ORG), 10).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].source_rowid, 2, "the exact offending row");
            assert_eq!(rows[0].org_id, ORG);
            assert_eq!(rows[0].workspace_id.as_deref(), Some("ws-1"));
            assert_eq!(rows[0].provider, "zai");
        }

        /// The bisect must pinpoint the *right* row when a batch has several
        /// good rows on both sides of one bad one — and must not dead-letter
        /// its neighbors.
        #[test]
        fn bisect_pinpoints_the_poison_row_among_good_neighbors() {
            let hub = hub_with(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
            let intake = FakeIntake::new(&[7]);

            let outcome = drain_org(&hub, ORG, &intake, 12).expect("drain");

            assert_eq!(outcome.quarantined, 1);
            assert_eq!(outcome.delivered, 11);
            let quarantined = hub.cloud_quarantined(Some(ORG), 10).unwrap();
            assert_eq!(
                quarantined
                    .iter()
                    .map(|q| q.source_rowid)
                    .collect::<Vec<_>>(),
                vec![7],
                "only the offending row is dead-lettered"
            );
            assert_eq!(
                intake.confirmed(),
                vec![1, 2, 3, 4, 5, 6, 8, 9, 10, 11, 12],
                "every good neighbor still reaches the intake"
            );
            // Bisection is logarithmic, not linear: the full page + ~log2(12)
            // probes + the tail re-send, nowhere near one call per row.
            assert!(
                intake.calls() <= 8,
                "bisect probed {} times for 12 rows — that is not a bisect",
                intake.calls()
            );
        }

        /// Several poison rows in one page are each isolated in turn, and the
        /// good rows between them still ship.
        #[test]
        fn multiple_poison_rows_are_isolated_one_at_a_time() {
            let hub = hub_with(&[1, 2, 3, 4, 5, 6, 7, 8]);
            let intake = FakeIntake::new(&[3, 6]);

            let outcome = drain_org(&hub, ORG, &intake, 8).expect("drain");

            assert_eq!(outcome.quarantined, 2);
            assert_eq!(outcome.delivered, 6);
            assert!(outcome.is_complete());
            let mut ids: Vec<i64> = hub
                .cloud_quarantined(Some(ORG), 10)
                .unwrap()
                .iter()
                .map(|q| q.source_rowid)
                .collect();
            ids.sort_unstable();
            assert_eq!(ids, vec![3, 6]);
            assert_eq!(intake.confirmed(), vec![1, 2, 4, 5, 7, 8]);
            assert!(pending(&hub).is_empty());
        }

        /// A transient refusal must NOT quarantine anything and must NOT move
        /// the cursor: the rows stay pending for the caller's backoff to retry,
        /// and the same rows drain cleanly once the intake recovers.
        #[test]
        fn transient_rejection_never_quarantines_and_holds_the_cursor() {
            let hub = hub_with(&[1, 2, 3]);
            let intake = FakeIntake::scripting(
                &[],
                vec![
                    DrainRejection::from_http_status(503, "upstream unavailable"),
                    DrainRejection::transient(None, "connect timeout"),
                ],
            );

            let first = drain_org(&hub, ORG, &intake, 3).expect("drain");
            assert_eq!(first.delivered, 0);
            assert_eq!(first.quarantined, 0);
            assert_eq!(
                first.stopped_by.as_ref().map(|r| r.class),
                Some(RejectionClass::Transient),
                "a 5xx is a retry, not a poison row"
            );
            assert_eq!(hub.cloud_quarantine_count(Some(ORG)).unwrap(), 0);
            assert_eq!(pending(&hub), vec![1, 2, 3], "the cursor did not move");

            // A bare transport failure (no HTTP status) is transient too.
            let second = drain_org(&hub, ORG, &intake, 3).expect("drain");
            assert_eq!(
                second.stopped_by.as_ref().map(|r| r.class),
                Some(RejectionClass::Transient)
            );
            assert_eq!(hub.cloud_quarantine_count(Some(ORG)).unwrap(), 0);

            // Script exhausted — the same rows now drain untouched.
            let third = drain_org(&hub, ORG, &intake, 3).expect("drain");
            assert_eq!(third.delivered, 3);
            assert_eq!(third.quarantined, 0);
            assert!(third.is_complete());
            assert_eq!(intake.confirmed(), vec![1, 2, 3]);
        }

        /// A permanent but batch-wide refusal (revoked token, wrong endpoint,
        /// unsupported schema version) must stop the drain rather than
        /// dead-letter the org's whole backlog one row at a time.
        #[test]
        fn terminal_batch_rejection_stops_without_quarantining() {
            let hub = hub_with(&[1, 2, 3, 4]);
            let intake = FakeIntake::scripting(
                &[],
                vec![DrainRejection::from_http_status(401, "token revoked")],
            );

            let outcome = drain_org(&hub, ORG, &intake, 4).expect("drain");

            assert_eq!(outcome.quarantined, 0, "auth failure is not a poison row");
            assert_eq!(outcome.delivered, 0);
            assert_eq!(
                outcome.stopped_by.as_ref().map(|r| r.class),
                Some(RejectionClass::TerminalBatch)
            );
            assert_eq!(hub.cloud_quarantine_count(None).unwrap(), 0);
            assert_eq!(pending(&hub), vec![1, 2, 3, 4], "the cursor did not move");
            assert_eq!(intake.calls(), 1, "a batch-wide refusal is never bisected");
        }

        /// A transient blip *during* bisection aborts the search without
        /// dead-lettering a row — but banks the prefix the probes confirmed, so
        /// the next pass restarts from a shorter backlog.
        #[test]
        fn a_transient_blip_mid_bisect_quarantines_nothing_but_keeps_progress() {
            let hub = hub_with(&[1, 2, 3, 4]);
            // Full page rejects (row 4 is poison); the first *probe* blips.
            let intake = FakeIntake::scripting(
                &[4],
                vec![
                    DrainRejection::from_http_status(400, "schema validation failed"),
                    DrainRejection::from_http_status(503, "upstream unavailable"),
                ],
            );

            let outcome = drain_org(&hub, ORG, &intake, 4).expect("drain");
            assert_eq!(
                outcome.quarantined, 0,
                "an aborted bisect dead-letters nothing"
            );
            assert_eq!(
                outcome.stopped_by.as_ref().map(|r| r.class),
                Some(RejectionClass::Transient)
            );
            assert_eq!(hub.cloud_quarantine_count(None).unwrap(), 0);

            // Retrying finds the poison row and the org drains out.
            let outcome = drain_org(&hub, ORG, &intake, 4).expect("re-drain");
            assert_eq!(outcome.quarantined, 1);
            assert!(pending(&hub).is_empty());
            assert_eq!(
                hub.cloud_quarantined(Some(ORG), 10).unwrap()[0].source_rowid,
                4
            );
        }

        /// Paging: a poison row in a later page is isolated just the same, and
        /// each page's cursor advance is monotone.
        #[test]
        fn poison_row_in_a_later_page_is_isolated_too() {
            let hub = hub_with(&[1, 2, 3, 4, 5, 6]);
            let intake = FakeIntake::new(&[5]);

            let outcome = drain_org(&hub, ORG, &intake, 2).expect("drain");

            assert_eq!(outcome.quarantined, 1);
            assert_eq!(outcome.delivered, 5);
            assert!(pending(&hub).is_empty());
            assert_eq!(intake.confirmed(), vec![1, 2, 3, 4, 6]);
        }

        #[test]
        fn http_status_classification_splits_retry_from_poison_from_operator() {
            let class = |s: u16| DrainRejection::from_http_status(s, "x").class;
            // Retry with backoff.
            for s in [408, 429, 500, 502, 503, 504] {
                assert_eq!(class(s), RejectionClass::Transient, "{s} must retry");
            }
            // The only statuses that may ever dead-letter a row.
            for s in [400, 422] {
                assert_eq!(class(s), RejectionClass::TerminalRow, "{s} is a poison row");
            }
            // Permanent, but never attributable to a row.
            for s in [401, 403, 404, 409, 413, 426] {
                assert_eq!(
                    class(s),
                    RejectionClass::TerminalBatch,
                    "{s} must not dead-letter rows"
                );
            }
            assert!(RejectionClass::Transient.is_retryable());
            assert!(!RejectionClass::TerminalRow.is_retryable());
            assert!(!RejectionClass::TerminalBatch.is_retryable());
        }

        /// The gate between "refused" and "dead-lettered": only a terminal,
        /// row-attributable rejection can produce a [`QuarantineReason`].
        #[test]
        fn only_a_terminal_row_rejection_can_become_a_quarantine_reason() {
            assert!(
                DrainRejection::transient(Some(503), "blip")
                    .quarantine_reason()
                    .is_none()
            );
            assert!(
                DrainRejection::terminal_batch(Some(401), "token revoked")
                    .quarantine_reason()
                    .is_none()
            );
            let reason = DrainRejection::terminal_row(Some(422), "bad model id")
                .quarantine_reason()
                .expect("terminal-row rejections are quarantinable");
            assert_eq!(reason.http_status, Some(422));
            assert_eq!(reason.detail(), "bad model id");
        }

        /// An unsupported wire version is terminal but batch-wide — the whole
        /// batch shares one `schema_version`, so bisecting rows cannot rescue
        /// it and no row may be blamed for it.
        #[test]
        fn unsupported_schema_version_is_terminal_but_never_a_poison_row() {
            let hub = hub_with(&[1, 2, 3]);
            struct VersionPickyIntake;
            impl CloudIntake for VersionPickyIntake {
                fn deliver(&self, batch: &DrainBatch) -> std::result::Result<(), DrainRejection> {
                    // Simulate an intake that only speaks a *newer* range.
                    if schema_version_supported(batch.schema_version) {
                        return Err(DrainRejection::terminal_batch(
                            Some(400),
                            format!("unsupported schema_version {}", batch.schema_version),
                        ));
                    }
                    Ok(())
                }
            }
            let outcome = drain_org(&hub, ORG, &VersionPickyIntake, 3).expect("drain");
            assert_eq!(outcome.quarantined, 0);
            assert_eq!(
                outcome.stopped_by.as_ref().map(|r| r.class),
                Some(RejectionClass::TerminalBatch)
            );
            assert_eq!(pending(&hub), vec![1, 2, 3]);
        }

        /// An empty backlog is a clean no-op — the drain never calls the intake
        /// with an empty batch.
        #[test]
        fn draining_an_empty_backlog_never_touches_the_intake() {
            let hub = UsageStore::in_memory().unwrap();
            let intake = FakeIntake::new(&[]);
            let outcome = drain_org(&hub, ORG, &intake, 10).expect("drain");
            assert_eq!(outcome, DrainOutcome::default());
            assert_eq!(intake.calls(), 0);
        }
    }
}
