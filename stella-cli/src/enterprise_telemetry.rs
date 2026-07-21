//! Managed-only Oxagen Enterprise operational telemetry adapter.
//!
//! Community/default construction returns before resolving a spool path or
//! constructing an HTTP client. An enrolled deployment must supply a signed
//! managed document, a pinned verification-secret environment reference, an
//! exact HTTPS endpoint allowlist, and a bearer-token environment reference.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use hmac::{Hmac, KeyInit, Mac};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use stella_store::enterprise_telemetry::{
    EnterpriseTelemetrySpool, OperationalEventContext, SpoolLimits, SpoolStatus,
    StellaOperationalEventV1,
};
#[cfg(test)]
use stella_store::usage::ExecutionRollupRow;

use crate::TelemetryCmd;

const ENROLLMENT_SCHEMA: &str = "stella.enterprise.telemetry.enrollment.v1";
const MAX_POLICY_ENTRIES: usize = 16;
const MAX_ENV_REF_BYTES: usize = 128;
const MAX_SECRET_BYTES: usize = 4 * 1024;
const MAX_BEARER_BYTES: usize = 8 * 1024;
const MAX_ENROLLMENT_LIFETIME_S: i64 = 90 * 24 * 60 * 60;
const MAX_CLOCK_SKEW_S: i64 = 5 * 60;
const MAX_BATCH_EVENTS: usize = 50;
const MAX_REQUEST_BYTES: usize = 256 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const LEASE_MS: i64 = 30_000;
static PROJECT_ENV_NAMES: OnceLock<RwLock<BTreeSet<String>>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedTelemetrySettings {
    verification_secret_env: String,
    allowed_issuers: Vec<String>,
    allowed_audiences: Vec<String>,
    allowed_endpoints: Vec<String>,
    enrollment: SignedEnrollment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedEnrollment {
    claims: EnrollmentClaims,
    signature_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollmentClaims {
    schema: String,
    issuer: String,
    audience: String,
    enrollment_id: String,
    organization_id: String,
    workspace_id: String,
    endpoint: String,
    credential_env: String,
    event_classes: Vec<EnrollmentEventClass>,
    issued_at_unix_s: i64,
    expires_at_unix_s: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EnrollmentEventClass {
    ExecutionRollup,
    ComplianceAudit,
}

pub(crate) struct VerifiedEnrollment {
    context: OperationalEventContext,
    endpoint: Url,
    credential_env: String,
}

/// Verify one managed enrollment without constructing persistence or HTTP.
pub(crate) fn verify_managed_enrollment(
    raw: &Value,
    now_unix_s: i64,
) -> Result<VerifiedEnrollment, String> {
    register_declared_sensitive_env_refs(raw);
    let managed: ManagedTelemetrySettings = serde_json::from_value(raw.clone())
        .map_err(|error| format!("invalid managed enterprise telemetry settings: {error}"))?;
    validate_env_ref(&managed.verification_secret_env, "verification secret")?;
    validate_policy_list(&managed.allowed_issuers, "issuer")?;
    validate_policy_list(&managed.allowed_audiences, "audience")?;
    if managed.allowed_endpoints.is_empty() || managed.allowed_endpoints.len() > MAX_POLICY_ENTRIES
    {
        return Err("managed telemetry endpoint allowlist must contain 1..=16 entries".into());
    }

    let claims = &managed.enrollment.claims;
    validate_env_ref(&claims.credential_env, "bearer credential")?;
    let project_names = project_env_names();
    if project_names.contains(&managed.verification_secret_env)
        || project_names.contains(&claims.credential_env)
    {
        return Err(
            "enterprise telemetry credentials must come from the host environment, not project dotenv"
                .into(),
        );
    }
    if claims.schema != ENROLLMENT_SCHEMA {
        return Err("unsupported enterprise telemetry enrollment schema".into());
    }
    if !managed
        .allowed_issuers
        .iter()
        .any(|item| item == &claims.issuer)
    {
        return Err("enterprise telemetry enrollment issuer is not allowlisted".into());
    }
    if !managed
        .allowed_audiences
        .iter()
        .any(|item| item == &claims.audience)
    {
        return Err("enterprise telemetry enrollment audience is not allowlisted".into());
    }
    if claims.issued_at_unix_s > now_unix_s.saturating_add(MAX_CLOCK_SKEW_S)
        || claims.expires_at_unix_s <= now_unix_s
        || claims.expires_at_unix_s <= claims.issued_at_unix_s
        || claims
            .expires_at_unix_s
            .saturating_sub(claims.issued_at_unix_s)
            > MAX_ENROLLMENT_LIFETIME_S
    {
        return Err("enterprise telemetry enrollment is not currently valid".into());
    }
    if claims.event_classes != [EnrollmentEventClass::ExecutionRollup] {
        if claims
            .event_classes
            .contains(&EnrollmentEventClass::ComplianceAudit)
        {
            return Err("compliance_audit telemetry is unsupported in this phase".into());
        }
        return Err("enterprise telemetry enrollment event class is not supported".into());
    }

    let endpoint = strict_https_url(&claims.endpoint)?;
    let allowed_endpoints = managed
        .allowed_endpoints
        .iter()
        .map(|allowed| strict_https_url(allowed))
        .collect::<Result<Vec<_>, _>>()?;
    let endpoint_allowed = allowed_endpoints.iter().any(|allowed| allowed == &endpoint);
    if !endpoint_allowed {
        return Err("enterprise telemetry endpoint is not exactly allowlisted".into());
    }

    let secret = std::env::var(&managed.verification_secret_env)
        .map_err(|_| "enterprise telemetry verification secret is unavailable".to_string())?;
    if secret.len() < 32 || secret.len() > MAX_SECRET_BYTES {
        return Err("enterprise telemetry verification secret must be 32..=4096 bytes".into());
    }
    let signature = decode_signature(&managed.enrollment.signature_hex)?;
    let canonical = serde_json::to_vec(claims)
        .map_err(|error| format!("cannot canonicalize telemetry enrollment: {error}"))?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| "invalid telemetry verification secret".to_string())?;
    mac.update(&canonical);
    mac.verify_slice(&signature)
        .map_err(|_| "enterprise telemetry enrollment signature mismatch".to_string())?;

    let context = OperationalEventContext::new(
        claims.enrollment_id.clone(),
        claims.organization_id.clone(),
        claims.workspace_id.clone(),
    )
    .map_err(|error| error.to_string())?;
    Ok(VerifiedEnrollment {
        context,
        endpoint,
        credential_env: claims.credential_env.clone(),
    })
}

fn project_env_names() -> std::sync::RwLockReadGuard<'static, BTreeSet<String>> {
    PROJECT_ENV_NAMES
        .get_or_init(|| RwLock::new(BTreeSet::new()))
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Record which environment values came from model-writable project files.
pub(crate) fn register_project_env_names<I>(names: I)
where
    I: IntoIterator<Item = String>,
{
    PROJECT_ENV_NAMES
        .get_or_init(|| RwLock::new(BTreeSet::new()))
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .extend(names);
}

fn register_declared_sensitive_env_refs(raw: &Value) {
    let verification = raw
        .get("verification_secret_env")
        .and_then(Value::as_str)
        .filter(|value| validate_env_ref(value, "verification secret").is_ok());
    let credential = raw
        .pointer("/enrollment/claims/credential_env")
        .and_then(Value::as_str)
        .filter(|value| validate_env_ref(value, "bearer credential").is_ok());
    stella_tools::exec::register_sensitive_env_names(
        verification
            .into_iter()
            .chain(credential)
            .map(str::to_string),
    );
}

fn validate_policy_list(values: &[String], label: &str) -> Result<(), String> {
    if values.is_empty() || values.len() > MAX_POLICY_ENTRIES {
        return Err(format!(
            "managed telemetry {label} allowlist must contain 1..={MAX_POLICY_ENTRIES} entries"
        ));
    }
    if values.iter().any(|value| {
        value.is_empty()
            || value.len() > 128
            || !value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
    }) {
        return Err(format!("managed telemetry {label} allowlist is invalid"));
    }
    Ok(())
}

fn validate_env_ref(value: &str, label: &str) -> Result<(), String> {
    let valid = !value.is_empty()
        && value.len() <= MAX_ENV_REF_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        && value.as_bytes()[0].is_ascii_uppercase();
    if valid {
        Ok(())
    } else {
        Err(format!(
            "enterprise telemetry {label} env reference is invalid"
        ))
    }
}

fn strict_https_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "enterprise telemetry endpoint is not a URL")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "enterprise telemetry endpoint must be credential-free HTTPS without query or fragment"
                .into(),
        );
    }
    Ok(url)
}

fn decode_signature(hex: &str) -> Result<[u8; 32], String> {
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("enterprise telemetry signature must be 64 hexadecimal characters".into());
    }
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)
            .map_err(|_| "enterprise telemetry signature is invalid")?;
    }
    Ok(bytes)
}

/// Resolve the separate host spool, refusing any path addressable from the workspace.
pub(crate) fn host_spool_path(workspace_root: &Path) -> Result<PathBuf, String> {
    let workspace = workspace_root
        .canonicalize()
        .map_err(|error| format!("cannot resolve workspace for telemetry: {error}"))?;
    let data = std::path::absolute(stella_store::usage::data_dir())
        .map_err(|error| format!("cannot resolve host data directory: {error}"))?;
    if data.starts_with(&workspace) {
        return Err("enterprise telemetry host data directory is inside the workspace".into());
    }
    std::fs::create_dir_all(&data)
        .map_err(|error| format!("cannot create enterprise telemetry host data: {error}"))?;
    let data = data
        .canonicalize()
        .map_err(|error| format!("cannot canonicalize enterprise telemetry host data: {error}"))?;
    if data.starts_with(&workspace) {
        return Err("enterprise telemetry host data resolves inside the workspace".into());
    }
    Ok(data.join("enterprise-telemetry.db"))
}

#[async_trait]
pub(crate) trait BatchSender: Send + Sync {
    async fn send(
        &self,
        endpoint: &Url,
        bearer_token: &str,
        events: &[StellaOperationalEventV1],
    ) -> Result<(), String>;
}

struct ReqwestBatchSender {
    client: Client,
}

impl ReqwestBatchSender {
    fn new() -> Result<Self, String> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .user_agent(concat!("stella/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| {
                format!("cannot construct enterprise telemetry HTTP client: {error}")
            })?;
        Ok(Self { client })
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct OperationalBatch<'a> {
    schema: &'static str,
    events: &'a [StellaOperationalEventV1],
}

#[async_trait]
impl BatchSender for ReqwestBatchSender {
    async fn send(
        &self,
        endpoint: &Url,
        bearer_token: &str,
        events: &[StellaOperationalEventV1],
    ) -> Result<(), String> {
        if events.is_empty() || events.len() > MAX_BATCH_EVENTS {
            return Err("enterprise telemetry batch count is out of bounds".into());
        }
        let body = serde_json::to_vec(&OperationalBatch {
            schema: "stella.operational.batch.v1",
            events,
        })
        .map_err(|error| format!("cannot serialize enterprise telemetry batch: {error}"))?;
        if body.len() > MAX_REQUEST_BYTES {
            return Err("enterprise telemetry request body exceeds 256 KiB".into());
        }
        let response = self
            .client
            .post(endpoint.clone())
            .bearer_auth(bearer_token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|error| format!("enterprise telemetry request failed: {error}"))?;
        let status = response.status();
        validate_response_status(status)?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err("enterprise telemetry response exceeds 64 KiB".into());
        }
        let mut received = 0usize;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|error| format!("telemetry response read failed: {error}"))?;
            received = received.saturating_add(chunk.len());
            if received > MAX_RESPONSE_BYTES {
                return Err("enterprise telemetry response exceeds 64 KiB".into());
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_response_status(status: reqwest::StatusCode) -> Result<(), String> {
    if status.is_redirection() {
        return Err("enterprise telemetry redirects are refused".into());
    }
    if !status.is_success() {
        return Err(format!(
            "enterprise telemetry endpoint returned HTTP {status}"
        ));
    }
    Ok(())
}

/// Verified enrollment plus its bounded spool and delivery adapter.
pub(crate) struct EnterpriseTelemetryRuntime {
    enrollment: VerifiedEnrollment,
    spool: EnterpriseTelemetrySpool,
    sender: Arc<dyn BatchSender>,
}

/// Build only after a managed enrollment exists; `None` performs zero I/O.
pub(crate) fn build_runtime_from_managed<F>(
    managed: Option<&Value>,
    workspace_root: &Path,
    now_unix_s: i64,
    build_sender: F,
) -> Result<Option<EnterpriseTelemetryRuntime>, String>
where
    F: FnOnce() -> Result<Arc<dyn BatchSender>, String>,
{
    let Some(managed) = managed else {
        return Ok(None);
    };
    let enrollment = verify_managed_enrollment(managed, now_unix_s)?;
    let spool_path = host_spool_path(workspace_root)?;
    let spool = EnterpriseTelemetrySpool::open_at(&spool_path, SpoolLimits::default())
        .map_err(|error| error.to_string())?;
    let sender = build_sender()?;
    Ok(Some(EnterpriseTelemetryRuntime {
        enrollment,
        spool,
        sender,
    }))
}

impl EnterpriseTelemetryRuntime {
    #[cfg(test)]
    pub(crate) fn enqueue_rollup(
        &self,
        rollup: &ExecutionRollupRow,
        now_ms: i64,
    ) -> Result<bool, String> {
        let event =
            StellaOperationalEventV1::from_finalized_rollup(&self.enrollment.context, rollup)
                .map_err(|error| error.to_string())?;
        self.spool
            .enqueue(&event, now_ms)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn status(&self) -> Result<SpoolStatus, String> {
        self.spool.status().map_err(|error| error.to_string())
    }

    pub(crate) async fn flush(&self, now_ms: i64) -> Result<usize, String> {
        static CLAIM_SEQUENCE: AtomicU64 = AtomicU64::new(1);
        let sequence = CLAIM_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let owner = format!("pid-{}-{now_ms}-{sequence}", std::process::id());
        let claimed = self
            .spool
            .claim_batch(
                &owner,
                now_ms,
                LEASE_MS,
                MAX_BATCH_EVENTS,
                MAX_REQUEST_BYTES,
            )
            .map_err(|error| error.to_string())?;
        if claimed.is_empty() {
            return Ok(0);
        }
        let token = match std::env::var(&self.enrollment.credential_env) {
            Ok(value) if !value.is_empty() && value.len() <= MAX_BEARER_BYTES => value,
            _ => {
                self.spool
                    .retry(&owner, &claimed, now_ms)
                    .map_err(|error| error.to_string())?;
                return Err(
                    "enterprise telemetry bearer credential is unavailable or invalid".into(),
                );
            }
        };
        let events: Vec<_> = claimed.iter().map(|item| item.event.clone()).collect();
        match self
            .sender
            .send(&self.enrollment.endpoint, &token, &events)
            .await
        {
            Ok(()) => {
                self.spool
                    .ack(&owner, &claimed)
                    .map_err(|error| error.to_string())?;
                Ok(claimed.len())
            }
            Err(error) => {
                self.spool
                    .retry(&owner, &claimed, now_ms)
                    .map_err(|retry_error| {
                        format!("{error}; retry persistence failed: {retry_error}")
                    })?;
                Err(error)
            }
        }
    }
}

fn production_sender() -> Result<Arc<dyn BatchSender>, String> {
    Ok(Arc::new(ReqwestBatchSender::new()?))
}

fn unix_time() -> Result<(i64, i64), String> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "system clock precedes the Unix epoch".to_string())?;
    let seconds = i64::try_from(duration.as_secs())
        .map_err(|_| "system clock is outside telemetry range".to_string())?;
    let millis = i64::try_from(duration.as_millis())
        .map_err(|_| "system clock is outside telemetry range".to_string())?;
    Ok((seconds, millis))
}

fn enrolled_spool(
    managed: Option<&Value>,
    workspace_root: &Path,
    now_unix_s: i64,
) -> Result<Option<(VerifiedEnrollment, EnterpriseTelemetrySpool)>, String> {
    let Some(managed) = managed else {
        return Ok(None);
    };
    let enrollment = verify_managed_enrollment(managed, now_unix_s)?;
    let path = host_spool_path(workspace_root)?;
    let spool = EnterpriseTelemetrySpool::open_at(&path, SpoolLimits::default())
        .map_err(|error| error.to_string())?;
    Ok(Some((enrollment, spool)))
}

/// Provider-free `stella telemetry status|flush` entry point.
pub(crate) fn run_command(command: TelemetryCmd) -> Result<(), String> {
    let workspace =
        std::env::current_dir().map_err(|error| format!("cannot determine workspace: {error}"))?;
    let settings = crate::settings::Settings::load(&workspace)?;
    let (now_s, now_ms) = unix_time()?;
    match command {
        TelemetryCmd::Status => {
            let Some((_, spool)) =
                enrolled_spool(settings.managed_enterprise_telemetry(), &workspace, now_s)?
            else {
                println!("enterprise telemetry: disabled (no managed enrollment)");
                return Ok(());
            };
            let status = spool.status().map_err(|error| error.to_string())?;
            println!(
                "enterprise telemetry: enrolled; pending={} ({} bytes); dropped={}",
                status.pending_rows, status.pending_bytes, status.dropped_rows
            );
            Ok(())
        }
        TelemetryCmd::Flush => {
            let Some(runtime) = build_runtime_from_managed(
                settings.managed_enterprise_telemetry(),
                &workspace,
                now_s,
                production_sender,
            )?
            else {
                println!("enterprise telemetry: disabled (no managed enrollment)");
                return Ok(());
            };
            let runtime_handle = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("cannot start telemetry runtime: {error}"))?;
            let sent = runtime_handle.block_on(runtime.flush(now_ms))?;
            let status = runtime.status()?;
            println!(
                "enterprise telemetry: flushed={sent}; pending={}; dropped={}",
                status.pending_rows, status.dropped_rows
            );
            Ok(())
        }
    }
}

/// Fail-open post-finalization derivation. No HTTP client or socket is built.
pub(crate) fn enqueue_finalized_execution(
    store: &stella_store::Store,
    execution_id: i64,
) -> Result<bool, String> {
    let Some(workspace) = store.workspace_root() else {
        return Ok(false);
    };
    let settings = crate::settings::Settings::load(workspace)?;
    let (now_s, now_ms) = unix_time()?;
    let Some((enrollment, spool)) =
        enrolled_spool(settings.managed_enterprise_telemetry(), workspace, now_s)?
    else {
        return Ok(false);
    };
    let Some(rollup) = store
        .execution_rollup(execution_id, workspace)
        .map_err(|error| error.to_string())?
    else {
        return Ok(false);
    };
    let event = StellaOperationalEventV1::from_finalized_rollup(&enrollment.context, &rollup)
        .map_err(|error| error.to_string())?;
    spool
        .enqueue(&event, now_ms)
        .map_err(|error| error.to_string())
}

/// Close the deck's direct cancellation path, then export fail-open.
pub(crate) fn finish_cancelled_execution(
    store: &stella_store::Store,
    execution_id: i64,
    cost_usd: f64,
) -> Result<(), String> {
    store
        .finish_execution(execution_id, "cancelled", cost_usd)
        .map_err(|error| error.to_string())?;
    let _ = enqueue_finalized_execution(store, execution_id);
    Ok(())
}

/// Startup-only detached flush. It cannot delay execution or process exit.
pub(crate) fn start_best_effort_flush() {
    let Ok(workspace) = std::env::current_dir() else {
        return;
    };
    let Ok(settings) = crate::settings::Settings::load(&workspace) else {
        return;
    };
    let Some(managed) = settings.managed_enterprise_telemetry().cloned() else {
        return;
    };
    let Ok((startup_s, _)) = unix_time() else {
        return;
    };
    // Verify synchronously so sensitive env names are registered before any
    // model-controlled tool or hook can spawn. Network remains detached.
    if let Err(error) = verify_managed_enrollment(&managed, startup_s) {
        eprintln!("warning: enterprise telemetry enrollment is inactive: {error}");
        return;
    }
    std::thread::spawn(move || {
        let Ok((now_s, now_ms)) = unix_time() else {
            return;
        };
        let Ok(Some(runtime)) =
            build_runtime_from_managed(Some(&managed), &workspace, now_s, production_sender)
        else {
            return;
        };
        let Ok(handle) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        let _ = handle.block_on(runtime.flush(now_ms));
    });
}
