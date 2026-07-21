//! Content-free Oxagen Enterprise operational events and their host-owned spool.
//!
//! This module deliberately accepts only a finalized [`ExecutionRollupRow`]
//! and projects it into a closed schema. Raw store events, prompts, paths,
//! tool arguments/results, reasoning, errors, git state, memories, rules, and
//! local identifiers have no representable field. Delivery is owned by a CLI
//! adapter; this crate only provides deterministic records and bounded,
//! at-least-once persistence.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rand::Rng as _;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::usage::ExecutionRollupRow;
use crate::{Result, StoreError};

const IDENTIFIER_MAX_BYTES: usize = 128;
const DIMENSION_MAX_BYTES: usize = 160;
const MAX_CLAIM_EVENTS: usize = 1_000;
const MAX_CLAIM_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
const MAX_LEASE_MS: i64 = 5 * 60 * 1_000;
const RETRY_BASE_MS: i64 = 1_000;
const RETRY_MAX_MS: i64 = 5 * 60 * 1_000;
const EVENT_ID_DOMAIN: &[u8] = b"stella.enterprise.operational.event-id.v1";
const LEGACY_UNBOUND_SINK: &str =
    "sink_0000000000000000000000000000000000000000000000000000000000000000";

pub(crate) const STORE_EXPORT_TABLES_DDL: &str = "
CREATE TABLE IF NOT EXISTS enterprise_export_identity (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    store_uuid TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS enterprise_export_enrollment (
    sink_fingerprint TEXT PRIMARY KEY,
    enrolled_after_execution_id INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS enterprise_export_ledger (
    sink_fingerprint TEXT NOT NULL,
    execution_id INTEGER NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending', 'spooled')),
    PRIMARY KEY(sink_fingerprint, execution_id)
);";

impl crate::Store {
    pub fn enterprise_store_uuid(&self) -> Result<String> {
        let conn = self.lock();
        if let Some(value) = conn
            .query_row(
                "SELECT store_uuid FROM enterprise_export_identity WHERE singleton = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            return Ok(value);
        }
        let generated = random_uuid_v4();
        conn.execute(
            "INSERT OR IGNORE INTO enterprise_export_identity(singleton, store_uuid) VALUES (1, ?1)",
            params![generated],
        )?;
        conn.query_row(
            "SELECT store_uuid FROM enterprise_export_identity WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    pub fn begin_enterprise_enrollment(&self, sink_fingerprint: &str) -> Result<()> {
        validate_sink_fingerprint(sink_fingerprint)?;
        self.lock().execute(
            "INSERT OR IGNORE INTO enterprise_export_enrollment
             (sink_fingerprint, enrolled_after_execution_id)
             VALUES (?1, (SELECT COALESCE(MAX(id), 0) FROM executions))",
            params![sink_fingerprint],
        )?;
        Ok(())
    }

    pub fn mark_enterprise_export_pending(
        &self,
        sink_fingerprint: &str,
        execution_id: i64,
    ) -> Result<bool> {
        validate_sink_fingerprint(sink_fingerprint)?;
        let conn = self.lock();
        let eligible: bool = conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM executions e JOIN enterprise_export_enrollment x
                  ON x.sink_fingerprint = ?1
                WHERE e.id = ?2 AND e.id > x.enrolled_after_execution_id
                  AND e.finished_at IS NOT NULL AND e.outcome IS NOT NULL)",
            params![sink_fingerprint, execution_id],
            |row| row.get(0),
        )?;
        if !eligible {
            return Ok(false);
        }
        Ok(conn.execute(
            "INSERT OR IGNORE INTO enterprise_export_ledger
             (sink_fingerprint, execution_id, status) VALUES (?1, ?2, 'pending')",
            params![sink_fingerprint, execution_id],
        )? == 1)
    }

    pub fn pending_enterprise_exports(&self, sink_fingerprint: &str) -> Result<Vec<i64>> {
        validate_sink_fingerprint(sink_fingerprint)?;
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT execution_id FROM enterprise_export_ledger
             WHERE sink_fingerprint = ?1 AND status = 'pending' ORDER BY execution_id",
        )?;
        let rows = stmt.query_map(params![sink_fingerprint], |row| row.get(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn mark_enterprise_export_spooled(
        &self,
        sink_fingerprint: &str,
        execution_id: i64,
    ) -> Result<()> {
        validate_sink_fingerprint(sink_fingerprint)?;
        self.lock().execute(
            "UPDATE enterprise_export_ledger SET status = 'spooled'
             WHERE sink_fingerprint = ?1 AND execution_id = ?2",
            params![sink_fingerprint, execution_id],
        )?;
        Ok(())
    }
}

pub(crate) fn random_uuid_v4() -> String {
    let mut bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

/// Load or create the owner-only random identity for this Stella installation.
pub fn load_or_create_installation_uuid(data_dir: &Path) -> Result<String> {
    ensure_trusted_host_data_dir(data_dir)?;
    let path = data_dir.join("installation-id");
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return create_installation_uuid(&path);
        }
        Err(error) => {
            return Err(StoreError(format!(
                "cannot inspect installation identity: {error}"
            )));
        }
    }
    let value = crate::read_private_to_string(&path)?;
    let value = value.trim().to_string();
    if valid_uuid(&value) {
        Ok(value)
    } else {
        Err(StoreError(
            "invalid enterprise installation identity".into(),
        ))
    }
}

/// Create/validate the host-owned telemetry root.  It must not be a link or
/// be controlled by another local account.
pub fn ensure_trusted_host_data_dir(data_dir: &Path) -> Result<()> {
    crate::ensure_private_dir(data_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = std::fs::symlink_metadata(data_dir).map_err(|error| {
            StoreError(format!(
                "cannot inspect enterprise host data directory: {error}"
            ))
        })?;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(StoreError(
                "enterprise host data directory is not owner-controlled".into(),
            ));
        }
    }
    Ok(())
}

fn create_installation_uuid(path: &Path) -> Result<String> {
    use std::io::Write as _;
    let generated = random_uuid_v4();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    match crate::private::open_private_file(path, options) {
        Ok(mut file) => {
            file.write_all(generated.as_bytes()).map_err(|error| {
                StoreError(format!("cannot write installation identity: {error}"))
            })?;
            file.sync_data().map_err(|error| {
                StoreError(format!("cannot sync installation identity: {error}"))
            })?;
            Ok(generated)
        }
        Err(_) => {
            let value = crate::read_private_to_string(path)?;
            let value = value.trim().to_string();
            if valid_uuid(&value) {
                Ok(value)
            } else {
                Err(StoreError(
                    "invalid enterprise installation identity".into(),
                ))
            }
        }
    }
}

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
            && value[4..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

/// One exact provider/model pair admitted by the managed enrollment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedModelDimension {
    provider: ProviderDimension,
    model: ModelDimension,
}

impl ManagedModelDimension {
    pub fn new(provider: &str, model: &str) -> Result<Self> {
        Ok(Self {
            provider: ProviderDimension::parse(provider)?,
            model: ModelDimension::model(model)?,
        })
    }

    pub fn provider(&self) -> &str {
        &self.provider.0
    }

    pub fn model(&self) -> &str {
        &self.model.0
    }
}

/// Persistent random host/store identities used only in event-id derivation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationalIdentity {
    installation_uuid: String,
    store_uuid: String,
}

impl OperationalIdentity {
    pub fn new(installation_uuid: &str, store_uuid: &str) -> Result<Self> {
        if !valid_uuid(installation_uuid) || !valid_uuid(store_uuid) {
            return Err(StoreError(
                "enterprise telemetry identities must be lowercase UUIDs".into(),
            ));
        }
        Ok(Self {
            installation_uuid: installation_uuid.to_string(),
            store_uuid: store_uuid.to_string(),
        })
    }
}

fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
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
    Error,
    Failed,
    Aborted,
    Cancelled,
    Indeterminate,
    VerificationFailed,
    GoalMet,
    GoalUnmet,
}

impl OperationalOutcome {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "completed" => Ok(Self::Completed),
            "error" => Ok(Self::Error),
            "failed" => Ok(Self::Failed),
            "aborted" => Ok(Self::Aborted),
            "cancelled" => Ok(Self::Cancelled),
            "indeterminate" => Ok(Self::Indeterminate),
            "verification_failed" => Ok(Self::VerificationFailed),
            "goal_met" => Ok(Self::GoalMet),
            "goal_unmet" => Ok(Self::GoalUnmet),
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
    identity: OperationalIdentity,
    model_catalog: BTreeSet<ManagedModelDimension>,
}

impl OperationalEventContext {
    /// Validate the three managed identifiers before a local rollup is mapped.
    pub fn new<I>(
        enrollment_id: impl Into<String>,
        organization_id: impl Into<String>,
        workspace_id: impl Into<String>,
        identity: OperationalIdentity,
        model_catalog: I,
    ) -> Result<Self>
    where
        I: IntoIterator<Item = ManagedModelDimension>,
    {
        Ok(Self {
            enrollment_id: BoundedIdentifier::parse(enrollment_id)?,
            organization_id: BoundedIdentifier::parse(organization_id)?,
            workspace_id: BoundedIdentifier::parse(workspace_id)?,
            identity,
            model_catalog: model_catalog.into_iter().collect(),
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
        let requested = ManagedModelDimension::new(&rollup.provider, &rollup.model).ok();
        let admitted = requested
            .as_ref()
            .filter(|dimension| context.model_catalog.contains(*dimension));
        let (provider, model) = admitted.map_or_else(
            || {
                (
                    ProviderDimension("other".into()),
                    ModelDimension("other".into()),
                )
            },
            |dimension| (dimension.provider.clone(), dimension.model.clone()),
        );
        let cost_microusd = finite_nonnegative_microusd(rollup.cost_usd)?;
        let duration_ms = nonnegative_u64("duration_ms", rollup.duration_ms)?;
        let input_tokens = nonnegative_u64("input_tokens", rollup.input_tokens)?;
        let output_tokens = nonnegative_u64("output_tokens", rollup.output_tokens)?;
        let tool_call_count = nonnegative_u64("tool_calls", rollup.tool_calls)?;
        let changed_file_count = nonnegative_u64("files_written", rollup.files_written)?;

        let mut hash = Sha256::new();
        hash_part(&mut hash, EVENT_ID_DOMAIN);
        hash_part(&mut hash, b"stella.operational.v1");
        hash_part(&mut hash, b"execution_rollup");
        hash_part(&mut hash, context.enrollment_id.0.as_bytes());
        hash_part(&mut hash, context.organization_id.0.as_bytes());
        hash_part(&mut hash, context.workspace_id.0.as_bytes());
        hash_part(&mut hash, context.identity.installation_uuid.as_bytes());
        hash_part(&mut hash, context.identity.store_uuid.as_bytes());
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

/// Result of one bounded enqueue attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueOutcome {
    Retained,
    Duplicate,
    DroppedNew,
}

/// Durable operational health visible through `stella telemetry status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpoolStatus {
    pub pending_rows: u64,
    pub pending_payload_bytes: u64,
    pub stranded_rows: u64,
    pub stranded_payload_bytes: u64,
    pub dropped_rows: u64,
    pub rollover_discarded_rows: u64,
    pub physical_bytes: u64,
}

/// One transactionally leased event awaiting delivery.
#[derive(Debug, Clone)]
pub struct ClaimedOperationalEvent {
    pub event: StellaOperationalEventV1,
    sink_fingerprint: String,
    attempts: u32,
}

/// Bounded at-least-once SQLite spool stored outside model-writable workspaces.
pub struct EnterpriseTelemetrySpool {
    conn: Mutex<Connection>,
    limits: SpoolLimits,
    path: PathBuf,
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
        let mut conn = crate::open_private_sqlite(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=100;",
        )?;
        migrate_spool_schema(&mut conn)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS operational_spool (
                 insertion_seq  INTEGER PRIMARY KEY AUTOINCREMENT,
                 event_id       TEXT NOT NULL,
                 sink_fingerprint TEXT NOT NULL,
                 payload        BLOB NOT NULL,
                 payload_bytes  INTEGER NOT NULL CHECK(payload_bytes >= 0),
                 created_at_ms  INTEGER NOT NULL,
                 attempts       INTEGER NOT NULL DEFAULT 0,
                 next_attempt_ms INTEGER NOT NULL DEFAULT 0,
                 leased_by      TEXT,
                 lease_until_ms INTEGER,
                 UNIQUE(sink_fingerprint, event_id)
             );
             CREATE INDEX IF NOT EXISTS operational_spool_ready
                 ON operational_spool(sink_fingerprint, next_attempt_ms,
                                      lease_until_ms, insertion_seq);
             CREATE TABLE IF NOT EXISTS operational_spool_meta (
                 singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                 dropped_rows INTEGER NOT NULL DEFAULT 0,
                 rollover_discarded_rows INTEGER NOT NULL DEFAULT 0
             );
             INSERT OR IGNORE INTO operational_spool_meta
                 (singleton, dropped_rows, rollover_discarded_rows)
                 VALUES (1, 0, 0);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            limits,
            path: path.to_path_buf(),
        })
    }

    /// Insert once by deterministic event id, then enforce hard row/byte bounds.
    pub fn enqueue(
        &self,
        sink_fingerprint: &str,
        event: &StellaOperationalEventV1,
        created_at_ms: i64,
    ) -> Result<EnqueueOutcome> {
        validate_sink_fingerprint(sink_fingerprint)?;
        if created_at_ms < 0 {
            return Err(StoreError(
                "enterprise telemetry enqueue time must be non-negative".into(),
            ));
        }
        let payload = serde_json::to_vec(event)
            .map_err(|error| StoreError(format!("cannot serialize operational event: {error}")))?;
        let payload_bytes = i64::try_from(payload.len())
            .map_err(|_| StoreError("operational event is too large".into()))?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO operational_spool
             (event_id, sink_fingerprint, payload, payload_bytes, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.event_id(),
                sink_fingerprint,
                payload,
                payload_bytes,
                created_at_ms
            ],
        )? == 1;
        if !inserted {
            tx.commit()?;
            return Ok(EnqueueOutcome::Duplicate);
        }
        enforce_limits(&tx, self.limits)?;
        let retained: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM operational_spool
             WHERE sink_fingerprint = ?1 AND event_id = ?2)",
            params![sink_fingerprint, event.event_id()],
            |row| row.get(0),
        )?;
        tx.commit()?;
        Ok(if retained {
            EnqueueOutcome::Retained
        } else {
            EnqueueOutcome::DroppedNew
        })
    }

    /// Claim a bounded, disjoint batch. Expired leases are eligible again.
    pub fn claim_batch(
        &self,
        sink_fingerprint: &str,
        owner: &str,
        now_ms: i64,
        lease_ms: i64,
        max_events: usize,
        max_payload_bytes: usize,
    ) -> Result<Vec<ClaimedOperationalEvent>> {
        validate_sink_fingerprint(sink_fingerprint)?;
        if owner.is_empty()
            || owner.len() > IDENTIFIER_MAX_BYTES
            || now_ms < 0
            || lease_ms <= 0
            || lease_ms > MAX_LEASE_MS
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
        let future_deadline = now_ms.saturating_add(RETRY_MAX_MS.max(MAX_LEASE_MS));
        let clock_rolled_back: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM operational_spool
             WHERE sink_fingerprint = ?1 AND created_at_ms > ?2)",
            params![sink_fingerprint, now_ms],
            |row| row.get(0),
        )?;
        if clock_rolled_back {
            tx.execute(
                "UPDATE operational_spool SET next_attempt_ms = ?1,
                 leased_by = NULL, lease_until_ms = NULL
                 WHERE sink_fingerprint = ?2",
                params![now_ms, sink_fingerprint],
            )?;
        } else {
            tx.execute(
                "UPDATE operational_spool SET next_attempt_ms = ?1
                 WHERE sink_fingerprint = ?2 AND next_attempt_ms > ?3",
                params![now_ms, sink_fingerprint, future_deadline],
            )?;
            tx.execute(
                "UPDATE operational_spool SET leased_by = NULL, lease_until_ms = NULL
                 WHERE sink_fingerprint = ?1 AND lease_until_ms > ?2",
                params![sink_fingerprint, future_deadline],
            )?;
        }
        let selected = {
            let mut stmt = tx.prepare(
                "SELECT event_id, payload, payload_bytes, attempts
                 FROM operational_spool
                 WHERE sink_fingerprint = ?1 AND next_attempt_ms <= ?2
                   AND (lease_until_ms IS NULL OR lease_until_ms <= ?2)
                 ORDER BY insertion_seq LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![sink_fingerprint, now_ms, sql_limit], |row| {
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
            let changed = tx.execute(
                "UPDATE operational_spool SET leased_by = ?1, lease_until_ms = ?2
                 WHERE sink_fingerprint = ?3 AND event_id = ?4",
                params![owner, lease_until_ms, sink_fingerprint, id],
            )?;
            if changed != 1 {
                return Err(StoreError(
                    "operational telemetry claim does not match its exact sink row".into(),
                ));
            }
        }
        tx.commit()?;

        selected
            .into_iter()
            .map(|(_, payload, attempts)| {
                let event = serde_json::from_slice(&payload).map_err(|error| {
                    StoreError(format!("invalid operational event in spool: {error}"))
                })?;
                Ok(ClaimedOperationalEvent {
                    event,
                    sink_fingerprint: sink_fingerprint.to_string(),
                    attempts,
                })
            })
            .collect()
    }

    /// Acknowledge only records held by this lease owner.
    pub fn ack(
        &self,
        sink_fingerprint: &str,
        owner: &str,
        claimed: &[ClaimedOperationalEvent],
    ) -> Result<()> {
        validate_sink_fingerprint(sink_fingerprint)?;
        if claimed
            .iter()
            .any(|item| item.sink_fingerprint != sink_fingerprint)
        {
            return Err(StoreError("claimed event belongs to another sink".into()));
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for item in claimed {
            let changed = tx.execute(
                "DELETE FROM operational_spool
                 WHERE sink_fingerprint = ?1 AND event_id = ?2 AND leased_by = ?3",
                params![sink_fingerprint, item.event.event_id(), owner],
            )?;
            if changed != 1 {
                return Err(StoreError(
                    "operational telemetry acknowledgement does not match its exact lease".into(),
                ));
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Release a failed delivery with capped exponential backoff.
    pub fn retry(
        &self,
        sink_fingerprint: &str,
        owner: &str,
        claimed: &[ClaimedOperationalEvent],
        now_ms: i64,
    ) -> Result<()> {
        validate_sink_fingerprint(sink_fingerprint)?;
        if claimed
            .iter()
            .any(|item| item.sink_fingerprint != sink_fingerprint)
        {
            return Err(StoreError("claimed event belongs to another sink".into()));
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for item in claimed {
            let exponent = item.attempts.min(8);
            let delay = RETRY_BASE_MS
                .saturating_mul(1_i64 << exponent)
                .min(RETRY_MAX_MS);
            let jitter = retry_jitter(item.event.event_id(), item.attempts, delay);
            let changed = tx.execute(
                "UPDATE operational_spool
                 SET attempts = attempts + 1, next_attempt_ms = ?1,
                     leased_by = NULL, lease_until_ms = NULL
                 WHERE sink_fingerprint = ?2 AND event_id = ?3 AND leased_by = ?4",
                params![
                    now_ms.saturating_add(delay.saturating_add(jitter)),
                    sink_fingerprint,
                    item.event.event_id(),
                    owner
                ],
            )?;
            if changed != 1 {
                return Err(StoreError(
                    "operational telemetry retry does not match its exact lease".into(),
                ));
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Current bounded queue and durable operational drop count.
    pub fn status(&self) -> Result<SpoolStatus> {
        self.status_where(None)
    }

    /// Queue health for one active sink plus rows stranded under old sinks.
    pub fn status_for_sink(&self, sink_fingerprint: &str) -> Result<SpoolStatus> {
        validate_sink_fingerprint(sink_fingerprint)?;
        self.status_where(Some(sink_fingerprint))
    }

    fn status_where(&self, sink_fingerprint: Option<&str>) -> Result<SpoolStatus> {
        let conn = self.lock();
        let totals = |matches: bool| -> Result<(i64, i64)> {
            match sink_fingerprint {
                Some(sink) => conn
                    .query_row(
                        if matches {
                            "SELECT COUNT(*), COALESCE(SUM(payload_bytes), 0)
                             FROM operational_spool WHERE sink_fingerprint = ?1"
                        } else {
                            "SELECT COUNT(*), COALESCE(SUM(payload_bytes), 0)
                             FROM operational_spool WHERE sink_fingerprint <> ?1"
                        },
                        params![sink],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(Into::into),
                None if matches => conn
                    .query_row(
                        "SELECT COUNT(*), COALESCE(SUM(payload_bytes), 0)
                         FROM operational_spool",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(Into::into),
                None => Ok((0, 0)),
            }
        };
        let (rows, bytes) = totals(true)?;
        let (stranded_rows, stranded_bytes) = totals(false)?;
        let (dropped, rollover_discarded): (i64, i64) = conn.query_row(
            "SELECT dropped_rows, rollover_discarded_rows
             FROM operational_spool_meta WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(SpoolStatus {
            pending_rows: u64::try_from(rows).unwrap_or(0),
            pending_payload_bytes: u64::try_from(bytes).unwrap_or(0),
            stranded_rows: u64::try_from(stranded_rows).unwrap_or(0),
            stranded_payload_bytes: u64::try_from(stranded_bytes).unwrap_or(0),
            dropped_rows: u64::try_from(dropped).unwrap_or(0),
            rollover_discarded_rows: u64::try_from(rollover_discarded).unwrap_or(0),
            physical_bytes: physical_size(&self.path),
        })
    }

    /// Explicitly discard rows belonging to prior sink fingerprints.
    pub fn discard_stranded(&self, active_sink: &str) -> Result<u64> {
        validate_sink_fingerprint(active_sink)?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let discarded = tx.execute(
            "DELETE FROM operational_spool WHERE sink_fingerprint <> ?1",
            params![active_sink],
        )?;
        tx.execute(
            "UPDATE operational_spool_meta
             SET rollover_discarded_rows = rollover_discarded_rows + ?1
             WHERE singleton = 1",
            params![i64::try_from(discarded).unwrap_or(i64::MAX)],
        )?;
        tx.commit()?;
        u64::try_from(discarded)
            .map_err(|_| StoreError("rollover discard count exceeds u64".into()))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn migrate_spool_schema(conn: &mut Connection) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master
         WHERE type = 'table' AND name = 'operational_spool')",
        [],
        |row| row.get(0),
    )?;
    if exists {
        let has_sink: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('operational_spool')
             WHERE name = 'sink_fingerprint')",
            [],
            |row| row.get(0),
        )?;
        let has_sequence: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('operational_spool')
             WHERE name = 'insertion_seq')",
            [],
            |row| row.get(0),
        )?;
        if !has_sink || !has_sequence {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(
                "ALTER TABLE operational_spool RENAME TO operational_spool_legacy;
                 CREATE TABLE operational_spool (
                     insertion_seq INTEGER PRIMARY KEY AUTOINCREMENT,
                     event_id TEXT NOT NULL,
                     sink_fingerprint TEXT NOT NULL,
                     payload BLOB NOT NULL,
                     payload_bytes INTEGER NOT NULL CHECK(payload_bytes >= 0),
                     created_at_ms INTEGER NOT NULL,
                     attempts INTEGER NOT NULL DEFAULT 0,
                     next_attempt_ms INTEGER NOT NULL DEFAULT 0,
                     leased_by TEXT,
                     lease_until_ms INTEGER,
                     UNIQUE(sink_fingerprint, event_id)
                 );",
            )?;
            tx.execute(
                "INSERT INTO operational_spool
                 (event_id, sink_fingerprint, payload, payload_bytes, created_at_ms,
                  attempts, next_attempt_ms, leased_by, lease_until_ms)
                 SELECT event_id, ?1, payload, payload_bytes, created_at_ms,
                        attempts, next_attempt_ms, leased_by, lease_until_ms
                 FROM operational_spool_legacy ORDER BY rowid",
                params![LEGACY_UNBOUND_SINK],
            )?;
            tx.execute_batch("DROP TABLE operational_spool_legacy;")?;
            tx.commit()?;
        }
    }
    let meta_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master
         WHERE type = 'table' AND name = 'operational_spool_meta')",
        [],
        |row| row.get(0),
    )?;
    if meta_exists {
        let has_rollover: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('operational_spool_meta')
             WHERE name = 'rollover_discarded_rows')",
            [],
            |row| row.get(0),
        )?;
        if !has_rollover {
            conn.execute_batch(
                "ALTER TABLE operational_spool_meta
                 ADD COLUMN rollover_discarded_rows INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
    }
    Ok(())
}

pub(crate) fn validate_sink_fingerprint(value: &str) -> Result<()> {
    let valid = value.len() == 69
        && value.starts_with("sink_")
        && value[5..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if valid {
        Ok(())
    } else {
        Err(StoreError(
            "invalid enterprise telemetry sink fingerprint".into(),
        ))
    }
}

fn retry_jitter(event_id: &str, attempts: u32, delay: i64) -> i64 {
    let mut hash = Sha256::new();
    hash_part(&mut hash, b"stella.enterprise.telemetry.retry-jitter.v1");
    hash_part(&mut hash, event_id.as_bytes());
    hash_part(&mut hash, &attempts.to_be_bytes());
    let digest = hash.finalize();
    let raw = u64::from_be_bytes(digest[..8].try_into().unwrap_or([0; 8]));
    let cap = u64::try_from((delay / 4).max(1)).unwrap_or(1);
    i64::try_from(raw % cap).unwrap_or(0)
}

fn physical_size(path: &Path) -> u64 {
    [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ]
    .iter()
    .filter_map(|candidate| std::fs::metadata(candidate).ok())
    .map(|metadata| metadata.len())
    .sum()
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
        let oldest: Option<i64> = tx
            .query_row(
                "SELECT insertion_seq FROM operational_spool WHERE leased_by IS NULL
                 ORDER BY insertion_seq LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(oldest) = oldest else {
            return Ok(());
        };
        tx.execute(
            "DELETE FROM operational_spool WHERE insertion_seq = ?1",
            params![oldest],
        )?;
        tx.execute(
            "UPDATE operational_spool_meta SET dropped_rows = dropped_rows + 1
             WHERE singleton = 1",
            [],
        )?;
    }
}
