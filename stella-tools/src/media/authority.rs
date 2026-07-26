//! The spend-authority plumbing behind the paid media tools: who may approve
//! money, what identifies one paid invocation, and how a half-finished one is
//! reported.
//!
//! Nothing here talks to a provider. It exists so `generate_image` /
//! `generate_video` / `poll_video` share ONE definition of the two rules that
//! keep a paid call honest: spend authority is host-owned (never a tool
//! argument — see [`HostDataIsolation`]), and an invocation's identity is
//! host-minted and retry-stable (see [`MediaOperationIdSource`]) so a retried
//! call replays through the journal instead of paying twice. Every failure
//! after the provider may have been reached becomes `reconciliation_required`,
//! which refuses to resubmit rather than risk a second charge.

use std::sync::atomic::{AtomicU64, Ordering};

use stella_media::{ArtifactStore, JobStore, MediaJob, MediaKind, MediaSpendRequest};
use stella_protocol::tool::ToolOutput;

/// Host attestation that paid-media tools run in a process-free executor.
///
/// Selecting this mode makes [`crate::ToolRegistry`] omit every built-in
/// tool that launches a child process, delegates to another agent, or reaches
/// a process-backed issue adapter. The host must preserve that boundary when
/// composing external MCP/custom tools around the registry; the marker is not
/// a filesystem-path sandbox and must never be inferred from path checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostDataIsolation {
    ProcessFree,
}

pub(crate) fn process_free(value: Option<HostDataIsolation>) -> bool {
    value == Some(HostDataIsolation::ProcessFree)
}

/// Host-owned invocation identity. It never comes from model arguments and
/// remains stable when the host retries one call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostMediaOperation {
    pub opaque_id: String,
    pub expires_at: u64,
}

pub trait MediaOperationIdSource: Send + Sync {
    /// Both fields are part of one operation's identity, so a host retrying
    /// the same call must return the same `expires_at` as well as the same
    /// `opaque_id`. Deriving the expiry from a clock read *per call* looks
    /// stable and is not: two calls that straddle a second boundary present
    /// `now + ttl` and then `now + ttl + 1`, and the journal refuses the
    /// second as a different request wearing the same key. Mint the expiry
    /// once, when the operation is created, and hand back the same value.
    fn operation_id(&self) -> HostMediaOperation;
}

pub(super) struct DeniedOperationIds;

impl MediaOperationIdSource for DeniedOperationIds {
    fn operation_id(&self) -> HostMediaOperation {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let now = unix_now();
        HostMediaOperation {
            opaque_id: format!(
                "{}-{}-{}",
                std::process::id(),
                now,
                NEXT.fetch_add(1, Ordering::Relaxed)
            ),
            expires_at: now + 60,
        }
    }
}

pub(super) fn operation_key(
    source: &dyn MediaOperationIdSource,
    kind: MediaKind,
    provider: &str,
) -> (String, u64) {
    let kind = match kind {
        MediaKind::Image => "image",
        MediaKind::Video => "video",
        MediaKind::Svg => "svg",
    };
    let operation = source.operation_id();
    let identity = format!(
        "stella-media-v1\0{kind}\0{provider}\0{}",
        operation.opaque_id
    );
    (
        format!("mop_{}", crate::staleness::hex_sha256(identity.as_bytes())),
        operation.expires_at,
    )
}

pub(super) fn open_store(root: &std::path::Path) -> Result<ArtifactStore, ToolOutput> {
    ArtifactStore::open(root.join(".stella/artifacts")).map_err(|error| ToolOutput::Error {
        message: format!("artifact store unavailable: {error}"),
    })
}

pub(super) fn open_jobs(root: &std::path::Path) -> JobStore {
    JobStore::open(root.join(".stella/artifacts"))
}

pub(super) fn spend_denied(request: &MediaSpendRequest) -> ToolOutput {
    let estimate = request
        .estimated_usd
        .map_or_else(|| "unknown cost".to_string(), |usd| format!("${usd:.4}"));
    ToolOutput::Error {
        message: format!(
            "media spend requires host approval: {} submission to {} ({estimate}; {})",
            match request.kind {
                MediaKind::Image => "image",
                MediaKind::Video => "video",
                MediaKind::Svg => "SVG",
            },
            request.provider_id,
            request.detail
        ),
    }
}

pub(super) fn reconciliation_required(
    operation_id: &str,
    detail: impl std::fmt::Display,
) -> ToolOutput {
    ToolOutput::Error {
        message: format!(
            "reconciliation_required: paid media operation `{operation_id}` may have reached the provider; refusing automatic resubmission ({detail})"
        ),
    }
}

pub(super) fn video_submitted(job: &MediaJob) -> ToolOutput {
    ToolOutput::Ok {
        content: format!(
            "submitted video job `{}` (model {}, ~${:.4}) — the job runs asynchronously; check it with poll_video",
            job.provider_job_id, job.model, job.estimated_cost_usd
        ),
    }
}

pub(super) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
