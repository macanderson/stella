//! Content-free Oxagen Enterprise operational events and their host-owned spool.
//!
//! This module deliberately accepts only a finalized [`ExecutionRollupRow`]
//! and projects it into a closed schema. Raw store events, prompts, paths,
//! tool arguments/results, reasoning, errors, git state, memories, rules, and
//! local identifiers have no representable field. Delivery is owned by a CLI
//! adapter; this crate only provides deterministic records and bounded,
//! at-least-once persistence.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::usage::ExecutionRollupRow;
use crate::{Result, StoreError};

const IDENTIFIER_MAX_BYTES: usize = 128;
const DIMENSION_MAX_BYTES: usize = 160;
const MAX_CLAIM_EVENTS: usize = 1_000;
const MAX_CLAIM_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
const RETRY_BASE_MS: i64 = 1_000;
const RETRY_MAX_MS: i64 = 5 * 60 * 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
struct BoundedIdentifier(String);

impl BoundedIdentifier {
    fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= IDENTIFIER_MAX_BYTES
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            });
        if !valid {
            return Err(StoreError(format!(
                "enterprise telemetry identifier must be 1..={IDENTIFIER_MAX_BYTES} ASCII bytes from [A-Za-z0-9._:-]"
            )));
        }
        Ok(Self(value))
    }
}

impl TryFrom<String> for BoundedIdentifier {
    type Error = StoreError;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(value)
    }
}

impl From<BoundedIdentifier> for String {
    fn from(value: BoundedIdentifier) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
struct EventId(String);

impl TryFrom<String> for EventId {
    type Error = StoreError;

    fn try_from(value: String) -> Result<Self> {
        let valid = value.len() == 68
            && value.starts_with("evt_")
            && value[4..].bytes().all(|byte| byte.is_ascii_hexdigit());
        if valid {
            Ok(Self(value))
        } else {
            Err(StoreError("invalid operational event id".into()))
        }
    }
}

impl From<EventId> for String {
    fn from(value: EventId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
struct ModelDimension(String);

impl ModelDimension {
    fn model(value: &str) -> Result<Self> {
        let valid = !value.is_empty()
            && value.len() <= DIMENSION_MAX_BYTES
            && !value.starts_with('/')
            && !value.ends_with('/')
            && value.split('/').count() <= 2
            && value.split('/').all(valid_dimension_segment);
        if valid {
            Ok(Self(value.to_string()))
        } else {
            Err(StoreError("invalid operational model dimension".into()))
        }
    }
}

fn valid_dimension_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= DIMENSION_MAX_BYTES
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

impl TryFrom<String> for ModelDimension {
    type Error = StoreError;

    fn try_from(value: String) -> Result<Self> {
        Self::model(&value)
    }
}

impl From<ModelDimension> for String {
    fn from(value: ModelDimension) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
struct ProviderDimension(String);

impl ProviderDimension {
    fn parse(value: &str) -> Result<Self> {
        if valid_dimension_segment(value) {
            Ok(Self(value.to_string()))
        } else {
            Err(StoreError("invalid operational provider dimension".into()))
        }
    }
}

impl TryFrom<String> for ProviderDimension {
    type Error = StoreError;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}

impl From<ProviderDimension> for String {
    fn from(value: ProviderDimension) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OperationalSchema {
    #[serde(rename = "stella.operational.v1")]
    V1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OperationalEventClass {
    ExecutionRollup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OperationalOutcome {
    Completed,
    Failed,
    Aborted,
    Cancelled,
    Indeterminate,
    VerificationFailed,
}

impl OperationalOutcome {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "aborted" => Ok(Self::Aborted),
            "cancelled" => Ok(Self::Cancelled),
            "indeterminate" => Ok(Self::Indeterminate),
            "verification_failed" => Ok(Self::VerificationFailed),
            "" => Err(StoreError(
                "enterprise telemetry requires a finalized execution rollup".into(),
            )),
            other => Err(StoreError(format!(
                "unsupported finalized execution outcome `{other}`"
            ))),
        }
    }
}

/// Managed identifiers attached to every operational event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationalEventContext {
    enrollment_id: BoundedIdentifier,
    organization_id: BoundedIdentifier,
    workspace_id: BoundedIdentifier,
}

impl OperationalEventContext {
    /// Validate the three managed identifiers before a local rollup is mapped.
    pub fn new(
        enrollment_id: impl Into<String>,
        organization_id: impl Into<String>,
        workspace_id: impl Into<String>,
    ) -> Result<Self> {
        Ok(Self {
            enrollment_id: BoundedIdentifier::parse(enrollment_id)?,
            organization_id: BoundedIdentifier::parse(organization_id)?,
            workspace_id: BoundedIdentifier::parse(workspace_id)?,
        })
    }
}

/// Closed, content-free enterprise event derived after local finalization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StellaOperationalEventV1 {
    schema: OperationalSchema,
    event_class: OperationalEventClass,
    event_id: EventId,
    enrollment_id: BoundedIdentifier,
    organization_id: BoundedIdentifier,
    workspace_id: BoundedIdentifier,
    provider: ProviderDimension,
    model: ModelDimension,
    outcome: OperationalOutcome,
    duration_ms: u64,
    input_tokens: u64,
    output_tokens: u64,
    cost_microusd: u64,
    tool_call_count: u64,
    changed_file_count: u64,
    produced_output: bool,
}

impl StellaOperationalEventV1 {
    /// Project a finalized local rollup into the strict operational schema.
    pub fn from_finalized_rollup(
        context: &OperationalEventContext,
        rollup: &ExecutionRollupRow,
    ) -> Result<Self> {
        let outcome = OperationalOutcome::parse(&rollup.outcome)?;
        let provider = ProviderDimension::parse(&rollup.provider)?;
        let model = ModelDimension::model(&rollup.model)?;
        let cost_microusd = finite_nonnegative_microusd(rollup.cost_usd)?;
        let duration_ms = nonnegative_u64("duration_ms", rollup.duration_ms)?;
        let input_tokens = nonnegative_u64("input_tokens", rollup.input_tokens)?;
        let output_tokens = nonnegative_u64("output_tokens", rollup.output_tokens)?;
        let tool_call_count = nonnegative_u64("tool_calls", rollup.tool_calls)?;
        let changed_file_count = nonnegative_u64("files_written", rollup.files_written)?;

        let mut hash = Sha256::new();
        hash_part(&mut hash, context.enrollment_id.0.as_bytes());
        hash_part(&mut hash, rollup.project_id.as_bytes());
        hash_part(&mut hash, &rollup.execution_id.to_be_bytes());
        let mut event_id = String::from("evt_");
        for byte in hash.finalize() {
            write!(&mut event_id, "{byte:02x}")
                .map_err(|_| StoreError("cannot format operational event id".into()))?;
        }
        let event_id = EventId(event_id);

        Ok(Self {
            schema: OperationalSchema::V1,
            event_class: OperationalEventClass::ExecutionRollup,
            event_id,
            enrollment_id: context.enrollment_id.clone(),
            organization_id: context.organization_id.clone(),
            workspace_id: context.workspace_id.clone(),
            provider,
            model,
            outcome,
            duration_ms,
            input_tokens,
            output_tokens,
            cost_microusd,
            tool_call_count,
            changed_file_count,
            produced_output: rollup.produced_output,
        })
    }

    /// Deterministic delivery/idempotency key. It contains no local identity.
    pub fn event_id(&self) -> &str {
        &self.event_id.0
    }
}

fn hash_part(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

fn nonnegative_u64(name: &str, value: i64) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| StoreError(format!("enterprise telemetry {name} must be non-negative")))
}

fn finite_nonnegative_microusd(value: f64) -> Result<u64> {
    let scaled = value * 1_000_000.0;
    if !scaled.is_finite() || scaled < 0.0 || scaled > u64::MAX as f64 {
        return Err(StoreError(
            "enterprise telemetry cost must be finite and non-negative".into(),
        ));
    }
    Ok(scaled.round() as u64)
}

/// Hard capacity limits for the separate host-data spool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpoolLimits {
    pub max_rows: u64,
    pub max_bytes: u64,
}

impl Default for SpoolLimits {
    fn default() -> Self {
        Self {
            max_rows: 10_000,
            max_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Durable operational health visible through `stella telemetry status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpoolStatus {
    pub pending_rows: u64,
    pub pending_bytes: u64,
    pub dropped_rows: u64,
}

/// One transactionally leased event awaiting delivery.
#[derive(Debug, Clone)]
pub struct ClaimedOperationalEvent {
    pub event: StellaOperationalEventV1,
    attempts: u32,
}

/// Bounded at-least-once SQLite spool stored outside model-writable workspaces.
pub struct EnterpriseTelemetrySpool {
    conn: Mutex<Connection>,
    limits: SpoolLimits,
}

impl EnterpriseTelemetrySpool {
    /// Open a host-owned spool at an already policy-checked path.
    pub fn open_at(path: &Path, limits: SpoolLimits) -> Result<Self> {
        if limits.max_rows == 0 || limits.max_bytes == 0 {
            return Err(StoreError(
                "enterprise telemetry spool limits must be non-zero".into(),
            ));
        }
        if let Some(parent) = path.parent() {
            crate::ensure_private_dir(parent)?;
        }
        let conn = crate::open_private_sqlite(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=100;
             CREATE TABLE IF NOT EXISTS operational_spool (
                 event_id       TEXT PRIMARY KEY,
                 payload        BLOB NOT NULL,
                 payload_bytes  INTEGER NOT NULL CHECK(payload_bytes >= 0),
                 created_at_ms  INTEGER NOT NULL,
                 attempts       INTEGER NOT NULL DEFAULT 0,
                 next_attempt_ms INTEGER NOT NULL DEFAULT 0,
                 leased_by      TEXT,
                 lease_until_ms INTEGER
             );
             CREATE INDEX IF NOT EXISTS operational_spool_ready
                 ON operational_spool(next_attempt_ms, lease_until_ms, created_at_ms);
             CREATE TABLE IF NOT EXISTS operational_spool_meta (
                 singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                 dropped_rows INTEGER NOT NULL DEFAULT 0
             );
             INSERT OR IGNORE INTO operational_spool_meta(singleton, dropped_rows)
                 VALUES (1, 0);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            limits,
        })
    }

    /// Insert once by deterministic event id, then enforce hard row/byte bounds.
    pub fn enqueue(&self, event: &StellaOperationalEventV1, created_at_ms: i64) -> Result<bool> {
        let payload = serde_json::to_vec(event)
            .map_err(|error| StoreError(format!("cannot serialize operational event: {error}")))?;
        let payload_bytes = i64::try_from(payload.len())
            .map_err(|_| StoreError("operational event is too large".into()))?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO operational_spool
             (event_id, payload, payload_bytes, created_at_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![event.event_id(), payload, payload_bytes, created_at_ms],
        )? == 1;
        if inserted {
            enforce_limits(&tx, self.limits)?;
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Claim a bounded, disjoint batch. Expired leases are eligible again.
    pub fn claim_batch(
        &self,
        owner: &str,
        now_ms: i64,
        lease_ms: i64,
        max_events: usize,
        max_payload_bytes: usize,
    ) -> Result<Vec<ClaimedOperationalEvent>> {
        if owner.is_empty()
            || owner.len() > IDENTIFIER_MAX_BYTES
            || now_ms < 0
            || lease_ms <= 0
            || max_events == 0
            || max_events > MAX_CLAIM_EVENTS
            || max_payload_bytes == 0
            || max_payload_bytes > MAX_CLAIM_PAYLOAD_BYTES
        {
            return Err(StoreError(
                "invalid enterprise telemetry claim limits".into(),
            ));
        }
        let sql_limit = i64::try_from(max_events)
            .map_err(|_| StoreError("invalid enterprise telemetry claim limits".into()))?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let selected = {
            let mut stmt = tx.prepare(
                "SELECT event_id, payload, payload_bytes, attempts
                 FROM operational_spool
                 WHERE next_attempt_ms <= ?1
                   AND (lease_until_ms IS NULL OR lease_until_ms <= ?1)
                 ORDER BY created_at_ms, event_id LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![now_ms, sql_limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, u32>(3)?,
                ))
            })?;
            let mut selected = Vec::new();
            let mut bytes = 0usize;
            for row in rows {
                let (id, payload, stored_bytes, attempts) = row?;
                let size = usize::try_from(stored_bytes)
                    .map_err(|_| StoreError("negative spool payload size".into()))?;
                if bytes.saturating_add(size) > max_payload_bytes {
                    break;
                }
                bytes += size;
                selected.push((id, payload, attempts));
            }
            selected
        };
        let lease_until_ms = now_ms.saturating_add(lease_ms);
        for (id, _, _) in &selected {
            tx.execute(
                "UPDATE operational_spool SET leased_by = ?1, lease_until_ms = ?2
                 WHERE event_id = ?3",
                params![owner, lease_until_ms, id],
            )?;
        }
        tx.commit()?;

        selected
            .into_iter()
            .map(|(_, payload, attempts)| {
                let event = serde_json::from_slice(&payload).map_err(|error| {
                    StoreError(format!("invalid operational event in spool: {error}"))
                })?;
                Ok(ClaimedOperationalEvent { event, attempts })
            })
            .collect()
    }

    /// Acknowledge only records held by this lease owner.
    pub fn ack(&self, owner: &str, claimed: &[ClaimedOperationalEvent]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for item in claimed {
            tx.execute(
                "DELETE FROM operational_spool WHERE event_id = ?1 AND leased_by = ?2",
                params![item.event.event_id(), owner],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Release a failed delivery with capped exponential backoff.
    pub fn retry(
        &self,
        owner: &str,
        claimed: &[ClaimedOperationalEvent],
        now_ms: i64,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for item in claimed {
            let exponent = item.attempts.min(8);
            let delay = RETRY_BASE_MS
                .saturating_mul(1_i64 << exponent)
                .min(RETRY_MAX_MS);
            tx.execute(
                "UPDATE operational_spool
                 SET attempts = attempts + 1, next_attempt_ms = ?1,
                     leased_by = NULL, lease_until_ms = NULL
                 WHERE event_id = ?2 AND leased_by = ?3",
                params![now_ms.saturating_add(delay), item.event.event_id(), owner],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Current bounded queue and durable operational drop count.
    pub fn status(&self) -> Result<SpoolStatus> {
        let conn = self.lock();
        let (rows, bytes): (i64, i64) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(payload_bytes), 0) FROM operational_spool",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let dropped: i64 = conn.query_row(
            "SELECT dropped_rows FROM operational_spool_meta WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(SpoolStatus {
            pending_rows: u64::try_from(rows).unwrap_or(0),
            pending_bytes: u64::try_from(bytes).unwrap_or(0),
            dropped_rows: u64::try_from(dropped).unwrap_or(0),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn enforce_limits(tx: &rusqlite::Transaction<'_>, limits: SpoolLimits) -> Result<()> {
    loop {
        let (rows, bytes): (i64, i64) = tx.query_row(
            "SELECT COUNT(*), COALESCE(SUM(payload_bytes), 0) FROM operational_spool",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let rows = u64::try_from(rows).unwrap_or(u64::MAX);
        let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
        if rows <= limits.max_rows && bytes <= limits.max_bytes {
            return Ok(());
        }
        let oldest: Option<String> = tx
            .query_row(
                "SELECT event_id FROM operational_spool WHERE leased_by IS NULL
                 ORDER BY created_at_ms, event_id LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(oldest) = oldest else {
            return Ok(());
        };
        tx.execute(
            "DELETE FROM operational_spool WHERE event_id = ?1",
            params![oldest],
        )?;
        tx.execute(
            "UPDATE operational_spool_meta SET dropped_rows = dropped_rows + 1
             WHERE singleton = 1",
            [],
        )?;
    }
}
