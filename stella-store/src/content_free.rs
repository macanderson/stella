//! The content-free egress guard — invariant #3, enforced instead of assumed.
//!
//! `AGENTS.md` invariant #3 ("Zero telemetry egress by default") says prompts,
//! paths, tool payloads/results, reasoning, errors, git state, memories, rules,
//! and local identifiers are **never exportable**. Today that holds *by
//! construction*: the hub `telemetry` schema (`~/.stella/usage.db`) has no
//! content columns, and every egress encoder enumerates its fields by hand.
//! Nothing **prevented** a future column, struct field, or OTLP attribute from
//! quietly carrying content. A regression there is a privacy incident, not a
//! bug — so this module makes the invariant a red gate (#466).
//!
//! It is the direct analogue of `stella-model`'s provider-parity matrix: a
//! declared table plus tests that enforce it from both sides.
//!
//! # The two halves
//!
//! 1. **Schema allowlist.** [`HUB_TELEMETRY_COLUMNS`] is the reviewed column
//!    set of the hub `telemetry` table — the only table a cloud drain reads.
//!    A test compares it against the live `PRAGMA table_info`, so adding a
//!    column fails the build until the allowlist is edited in the same PR.
//!    That edit is the forcing function: a human has to look at the new name
//!    and answer "is this content?".
//! 2. **Encoder sentinel harness.** Every encoder that puts bytes on the
//!    network implements [`ContentFreeEncoder`]. The harness owns the fixtures
//!    ([`poisoned_cloud_event`], [`poisoned_execution_rollup`]) and stamps
//!    every content-bearing or local-only source field with a [`Sentinel`].
//!    [`audit_encoder`] then serializes, and reports a [`Violation`] if a
//!    sentinel reached the wire, if an unreviewed key appeared, or if the
//!    encoder did not actually consume the poisoned fixture.
//!
//! # Why the fixtures are full struct literals
//!
//! [`poisoned_cloud_event`] and [`poisoned_execution_rollup`] construct their
//! source rows with **exhaustive struct literals**. Adding a field to
//! `CloudTelemetryEvent` or `ExecutionRollupRow` therefore fails to *compile*
//! here until an author decides what the fixture should put in it — the same
//! compile-time forcing function the `AgentEvent` exhaustiveness guard (#455)
//! uses for enums. If they stamp a sentinel, a leak is caught at runtime; if
//! they stamp a benign value and the encoder starts emitting it, the key
//! allowlist catches it. The two checks compose to cover both shapes of drift.
//!
//! # Registering an encoder
//!
//! [`registered_encoders`] is the enumeration `every_registered_encoder_is_content_free`
//! walks, and [`DRAIN_FORMATS`] maps each drain `format` discriminator named by
//! the epic (#403) to its guard. A format listed as
//! [`DrainFormatGuard::NotYetBuilt`] is a **declared gap**, visible in source:
//! building that encoder means moving it to [`DrainFormatGuard::Guarded`] and
//! registering it, or `every_built_drain_format_has_a_registered_encoder`
//! fails. Shipping an encoder without a guard cannot be a silent omission.
//!
//! # Scope note on `project_id` / `repo_id`
//!
//! The drain contract (#404) deliberately ships `project_id` — an FNV-1a/64
//! digest of the canonical workspace path, not the path itself. It is a
//! **pseudonym, not an anonymization**: 64 bits of non-cryptographic hash over
//! a guessable input is dictionary-attackable by a determined intake operator.
//! It is on the allowlist because the epic chose it as the cross-project join
//! key, and it is called out here so the choice keeps being re-reviewed rather
//! than forgotten. The raw path never egresses (see [`PATH_SENTINEL`]).

use std::collections::BTreeSet;

use serde_json::Value;

use crate::enterprise_telemetry::{
    ManagedModelDimension, OperationalEventContext, OperationalIdentity, StellaOperationalEventV1,
};
use crate::usage::{CloudTelemetryEvent, ExecutionRollupRow, ToolBucket};
use crate::{DrainBatch, Result, StoreError, TelemetryRow};

// ---------------------------------------------------------------------------
// Sentinels
// ---------------------------------------------------------------------------

/// One poisoned value the harness stamps into a source field that must never
/// leave the machine. `label` names the invariant-#3 category so a failure
/// reads as a privacy finding, not a diff mismatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sentinel {
    /// The invariant-#3 category this value stands in for.
    pub label: &'static str,
    /// The literal (or literal prefix) searched for in the encoded bytes.
    pub value: &'static str,
}

/// Free-text content: prompt previews/digests, project names, turn kinds, the
/// per-call `call_role`, tool identities. The fixtures suffix this prefix with
/// the field name (see `content_sentinel`) so a failure names the leak.
pub const CONTENT_SENTINEL: &str = "STELLA-SENTINEL-CONTENT";

/// A local filesystem path — the literal thing invariant #3 calls out first.
pub const PATH_SENTINEL: &str = "/stella-sentinel-path/must-never-egress";

/// The local installation identity (`~/.stella/installation-id`). Must be a
/// syntactically valid lowercase UUID to survive `OperationalIdentity::new`.
pub const INSTALLATION_UUID_SENTINEL: &str = "deadbeef-dead-4bee-8dea-dbeefdeadbee";

/// The per-store local identity. Same UUID-shape constraint.
pub const STORE_UUID_SENTINEL: &str = "feedface-feed-4ace-8fee-dfacefeedfac";

/// Every value that must not appear in any encoder's wire bytes.
pub const FORBIDDEN_SENTINELS: &[Sentinel] = &[
    Sentinel {
        label: "free-text content (prompt / name / role / tool identity)",
        value: CONTENT_SENTINEL,
    },
    Sentinel {
        label: "local filesystem path",
        value: PATH_SENTINEL,
    },
    Sentinel {
        label: "local installation identity",
        value: INSTALLATION_UUID_SENTINEL,
    },
    Sentinel {
        label: "local store identity",
        value: STORE_UUID_SENTINEL,
    },
];

/// A value that **is** allowlisted for egress, stamped into an identity field
/// every encoder passes through. Its presence in the output proves the encoder
/// actually consumed the harness fixture rather than a clean hand-rolled one —
/// without it, an encoder could pass every sentinel check vacuously.
///
/// Constrained to `[A-Za-z0-9._:-]` so it survives the enterprise sink's
/// `BoundedIdentifier` validation.
pub const PASSTHROUGH_MARKER: &str = "stella-harness-passthrough-4d7b";

/// The per-field poisoned value: `CONTENT_SENTINEL` plus the source field name,
/// so a leak report names which field escaped.
fn content_sentinel(field: &str) -> String {
    format!("{CONTENT_SENTINEL}.{field}")
}

// ---------------------------------------------------------------------------
// Hub schema allowlist
// ---------------------------------------------------------------------------

/// The reviewed column set of the hub `telemetry` table
/// (`~/.stella/usage.db`) — the only table a cloud drain reads.
///
/// **Editing this list is a privacy review.** Every entry has been eyeballed
/// and is identity, addressing, or a numeric/enumerated telemetry measure. No
/// entry holds prompt or completion text, a tool payload or result, a path, a
/// reasoning trace, an error string, or git state.
///
/// Note what is deliberately *absent*: the hub's sibling `execution_rollup`
/// table holds `prompt_preview`, `project_name`, and `root_path` — genuinely
/// content-bearing, deliberately **local-only**, and never drained. That table
/// is exactly why this guard exists: the drain must never be widened to it by
/// accident.
pub const HUB_TELEMETRY_COLUMNS: &[&str] = &[
    // --- identity / scope ---
    "org_id",
    "workspace_id",
    "repo_id",
    // FNV-1a/64 digest of the canonical workspace path — a pseudonym, not the
    // path. See the module docs' scope note.
    "project_id",
    // --- addressing ---
    "source_rowid",
    "execution_id",
    "step",
    "recorded_at",
    // --- content-free telemetry ---
    "provider",
    "call_role",
    "model",
    "input_tokens",
    "estimated_input_tokens",
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

/// Substrings that make a column or wire key *suspicious* on sight — a
/// cheap second opinion for the reviewer who is updating an allowlist in a
/// hurry. Naming a new field `prompt_preview` or `tool_result` trips this even
/// if the allowlist edit went in, forcing the "is this content?" conversation
/// to be explicit rather than a one-line diff nobody read.
///
/// Chosen to be non-overlapping with every currently allowlisted name; a false
/// positive is a deliberate speed bump, not a bug.
pub const SUSPICIOUS_KEY_SUBSTRINGS: &[&str] = &[
    "prompt",
    "content",
    "text",
    "snippet",
    "preview",
    "digest",
    "path",
    "root",
    "dir",
    "url",
    "payload",
    "result",
    "reasoning",
    "thinking",
    "message",
    "diff",
    "stdout",
    "stderr",
    "memory",
    "rule",
    "branch",
    "commit",
    "author",
    "email",
];

/// Whether `key` trips [`SUSPICIOUS_KEY_SUBSTRINGS`] — returns the substring
/// that matched, for the failure message.
pub fn suspicious_substring(key: &str) -> Option<&'static str> {
    let lowered = key.to_ascii_lowercase();
    SUSPICIOUS_KEY_SUBSTRINGS
        .iter()
        .copied()
        .find(|needle| lowered.contains(needle))
}

// ---------------------------------------------------------------------------
// The encoder seam
// ---------------------------------------------------------------------------

/// One encoder's serialized output, in the two shapes the harness inspects.
///
/// Deliberately format-agnostic: `bytes` is substring-searched for sentinels
/// (works for JSON, protobuf, or anything else), and `keys` is the flattened
/// name set compared against the allowlist. A future OTLP encoder (#427) that
/// emits protobuf supplies its attribute names as `keys` and the wire frame as
/// `bytes` — it does not have to be JSON to be guarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedSample {
    /// Exactly the bytes this encoder would put on the network.
    pub bytes: Vec<u8>,
    /// Every key/attribute name the encoding emits, flattened.
    pub keys: BTreeSet<String>,
}

impl EncodedSample {
    /// Build a sample from a JSON encoding, flattening nested object keys.
    pub fn from_json(value: &Value) -> Result<Self> {
        let bytes = serde_json::to_vec(value)
            .map_err(|err| StoreError(format!("cannot serialize encoder sample: {err}")))?;
        let mut keys = BTreeSet::new();
        collect_keys(value, &mut keys);
        Ok(Self { bytes, keys })
    }
}

fn collect_keys(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                out.insert(key.clone());
                collect_keys(nested, out);
            }
        }
        Value::Array(items) => {
            for nested in items {
                collect_keys(nested, out);
            }
        }
        _ => {}
    }
}

/// The seam every egress encoder must implement to be guarded.
///
/// Implementing this is **not optional** for anything that serializes store
/// rows for the network: [`registered_encoders`] is enumerated by a test, and
/// [`DRAIN_FORMATS`] makes an unguarded drain format a declared, visible gap.
pub trait ContentFreeEncoder {
    /// Stable id used in failure messages and by [`DRAIN_FORMATS`].
    fn encoder_id(&self) -> &'static str;

    /// The drain `format` discriminator (#404) this encoder implements, if it
    /// is a cloud-drain encoder. `None` for other egress paths (the enterprise
    /// operational spool).
    fn drain_format(&self) -> Option<&'static str>;

    /// Every key/attribute this encoder is reviewed to emit. Same contract as
    /// [`HUB_TELEMETRY_COLUMNS`]: editing it is a privacy review.
    fn allowed_keys(&self) -> &'static [&'static str];

    /// Encode the harness's poisoned fixtures exactly as they would go on the
    /// wire.
    ///
    /// Implementations **must** build their input from [`poisoned_cloud_event`]
    /// / [`poisoned_execution_rollup`] rather than a hand-rolled clean fixture.
    /// That is enforced, not merely asked: [`audit_encoder`] fails the encoder
    /// if [`PASSTHROUGH_MARKER`] is missing from the output.
    fn encode_poisoned_sample(&self) -> Result<EncodedSample>;
}

/// What the guard found wrong with one encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// The encoder could not serialize the poisoned fixture at all.
    EncodeFailed { encoder: String, error: String },
    /// A sentinel reached the wire — a privacy incident, not a test failure.
    SentinelLeaked {
        encoder: String,
        label: &'static str,
        excerpt: String,
    },
    /// The encoding emitted a key that is not on the encoder's allowlist.
    UnallowedKey { encoder: String, key: String },
    /// An allowlisted key looks content-bearing on its name alone.
    SuspiciousAllowlistEntry {
        encoder: String,
        key: String,
        matched: &'static str,
    },
    /// The output does not contain [`PASSTHROUGH_MARKER`], so the encoder did
    /// not actually consume the poisoned fixture and every other check above
    /// passed vacuously.
    FixtureNotUsed { encoder: String },
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EncodeFailed { encoder, error } => write!(
                f,
                "[{encoder}] could not encode the poisoned fixture: {error}"
            ),
            Self::SentinelLeaked {
                encoder,
                label,
                excerpt,
            } => write!(
                f,
                "[{encoder}] CONTENT LEAK — a source field holding {label} reached the wire \
                 (near: {excerpt}). AGENTS.md invariant #3 says prompts, paths, tool \
                 payloads/results, reasoning, errors, git state, memories, rules, and local \
                 identifiers are never exportable. This is a privacy incident, not a test \
                 failure: remove the field from the encoding — do not widen the allowlist"
            ),
            Self::UnallowedKey { encoder, key } => write!(
                f,
                "[{encoder}] emits unreviewed wire key `{key}`. This gate is deliberate: a new \
                 key must be added to this encoder's `allowed_keys()` in the SAME change, which \
                 forces a human to answer \"is this content?\" before it can ship"
            ),
            Self::SuspiciousAllowlistEntry {
                encoder,
                key,
                matched,
            } => write!(
                f,
                "[{encoder}] allowlists `{key}`, whose name contains `{matched}` — that reads \
                 content-bearing. If it genuinely is not, rename it or remove `{matched}` from \
                 SUSPICIOUS_KEY_SUBSTRINGS with a reviewer on the PR"
            ),
            Self::FixtureNotUsed { encoder } => write!(
                f,
                "[{encoder}] output does not contain PASSTHROUGH_MARKER, so it did not encode \
                 the harness's poisoned fixture — every sentinel check above passed vacuously. \
                 Build the encoder's input from poisoned_cloud_event() / \
                 poisoned_execution_rollup()"
            ),
        }
    }
}

/// Run the full content-free audit over one encoder. Returns every violation
/// found (empty means clean) rather than panicking, so the harness itself is
/// testable — see `harness_catches_a_leaking_encoder`.
pub fn audit_encoder(encoder: &dyn ContentFreeEncoder) -> Vec<Violation> {
    let id = encoder.encoder_id().to_string();
    let sample = match encoder.encode_poisoned_sample() {
        Ok(sample) => sample,
        Err(error) => {
            return vec![Violation::EncodeFailed {
                encoder: id,
                error: error.to_string(),
            }];
        }
    };
    let mut found = Vec::new();

    for sentinel in FORBIDDEN_SENTINELS {
        if let Some(at) = find_subslice(&sample.bytes, sentinel.value.as_bytes()) {
            found.push(Violation::SentinelLeaked {
                encoder: id.clone(),
                label: sentinel.label,
                excerpt: excerpt_at(&sample.bytes, at),
            });
        }
    }

    let allowed: BTreeSet<&str> = encoder.allowed_keys().iter().copied().collect();
    for key in &sample.keys {
        if !allowed.contains(key.as_str()) {
            found.push(Violation::UnallowedKey {
                encoder: id.clone(),
                key: key.clone(),
            });
        }
    }
    for key in encoder.allowed_keys() {
        if let Some(matched) = suspicious_substring(key) {
            found.push(Violation::SuspiciousAllowlistEntry {
                encoder: id.clone(),
                key: (*key).to_string(),
                matched,
            });
        }
    }

    if find_subslice(&sample.bytes, PASSTHROUGH_MARKER.as_bytes()).is_none() {
        found.push(Violation::FixtureNotUsed { encoder: id });
    }
    found
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// A short, lossy window around a leak so the failure names the offending key
/// without dumping the whole payload. Only ever fixture bytes, never real data.
fn excerpt_at(bytes: &[u8], at: usize) -> String {
    let start = at.saturating_sub(48);
    let end = (at + 96).min(bytes.len());
    String::from_utf8_lossy(&bytes[start..end]).into_owned()
}

// ---------------------------------------------------------------------------
// Harness-owned poisoned fixtures
// ---------------------------------------------------------------------------

/// A hub row whose every non-telemetry string is either a sentinel (must not
/// egress) or [`PASSTHROUGH_MARKER`] (allowlisted, proves the fixture was used).
///
/// Built as an exhaustive struct literal on purpose: adding a field to
/// `CloudTelemetryEvent` breaks this build until an author decides whether the
/// new field is content. See the module docs.
pub fn poisoned_cloud_event() -> CloudTelemetryEvent {
    CloudTelemetryEvent {
        // Internal cursor address — deliberately excluded from every wire
        // contract; distinct from `source_rowid` so a swap is visible.
        hub_rowid: 4_242,
        org_id: format!("{PASSTHROUGH_MARKER}-org"),
        workspace_id: Some(format!("{PASSTHROUGH_MARKER}-ws")),
        repo_id: format!("{PASSTHROUGH_MARKER}-repo"),
        project_id: format!("{PASSTHROUGH_MARKER}-proj"),
        execution_id: 7,
        source_rowid: 99,
        recorded_at: "2026-07-24T00:00:00Z".into(),
        telemetry: TelemetryRow {
            step: 3,
            provider: "zai".into(),
            // Excluded from the drain contract by design — poisoned so any
            // encoder that blanket-serializes the hub row is caught.
            call_role: content_sentinel("call_role"),
            model: "glm-5.2".into(),
            input_tokens: 100,
            estimated_input_tokens: 90,
            output_tokens: 20,
            cache_read_tokens: 10,
            cache_miss_tokens: 5,
            cache_write_tokens: 2,
            cost_usd: 0.01,
            duration_ms: 1_234,
            retries: 1,
            tool_calls: 4,
            usage_complete: true,
        },
    }
}

/// A finished-turn rollup whose genuinely content-bearing fields — the prompt
/// preview and digest, the project name, the workspace **path**, the turn kind,
/// and the per-tool identities — are all poisoned.
///
/// Exhaustive struct literal for the same reason as [`poisoned_cloud_event`].
pub fn poisoned_execution_rollup() -> ExecutionRollupRow {
    ExecutionRollupRow {
        project_id: format!("{PASSTHROUGH_MARKER}-proj"),
        project_name: content_sentinel("project_name"),
        project_root: PATH_SENTINEL.into(),
        execution_id: 11,
        kind: content_sentinel("kind"),
        prompt_digest: content_sentinel("prompt_digest"),
        prompt_preview: content_sentinel("prompt_preview"),
        model: "glm-5.2".into(),
        provider: "zai".into(),
        // Must parse to a known operational outcome; the enum is closed, so it
        // cannot carry free text and needs no sentinel.
        outcome: "completed".into(),
        cost_usd: 0.05,
        input_tokens: 61_000,
        output_tokens: 8_192,
        duration_ms: 133_700,
        tool_calls: 3,
        files_written: 2,
        produced_output: true,
        usage_complete: true,
        self_rating: None,
        started_at: "2026-07-24T13:00:00Z".into(),
        day: "2026-07-24".into(),
        tool_histogram: vec![ToolBucket {
            tool: content_sentinel("tool_histogram.tool"),
            surface: content_sentinel("tool_histogram.surface"),
            calls: 3,
            errors: 1,
        }],
    }
}

// ---------------------------------------------------------------------------
// Registered encoders
// ---------------------------------------------------------------------------

/// Encoder id of the native (`format = "stella"`) cloud-drain payload.
pub const NATIVE_DRAIN_ENCODER: &str = "drain:stella";

/// Encoder id of the Oxagen Enterprise operational spool event.
pub const ENTERPRISE_OPERATIONAL_ENCODER: &str = "enterprise:operational.v1";

/// The native drain encoder: [`DrainBatch::from_events`] over hub rows (#404's
/// wire payload, versioned in #468).
pub struct NativeDrainGuard;

impl ContentFreeEncoder for NativeDrainGuard {
    fn encoder_id(&self) -> &'static str {
        NATIVE_DRAIN_ENCODER
    }

    fn drain_format(&self) -> Option<&'static str> {
        Some("stella")
    }

    fn allowed_keys(&self) -> &'static [&'static str] {
        NATIVE_DRAIN_KEYS
    }

    fn encode_poisoned_sample(&self) -> Result<EncodedSample> {
        let batch = DrainBatch::from_events(&[poisoned_cloud_event()]);
        let value = serde_json::to_value(&batch)
            .map_err(|err| StoreError(format!("cannot encode drain batch: {err}")))?;
        EncodedSample::from_json(&value)
    }
}

/// Every key the native drain payload may emit: the envelope plus the frozen
/// v1 row contract (`crate::drain`).
const NATIVE_DRAIN_KEYS: &[&str] = &[
    // envelope
    "schema_version",
    "rows",
    // identity
    "org_id",
    "workspace_id",
    "repo_id",
    "project_id",
    // addressing / idempotency
    "source_rowid",
    "recorded_at",
    // content-free telemetry
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

/// Encoder id of the OTLP (`format = "otel"`) cloud-drain payload (#427).
pub const OTEL_DRAIN_ENCODER: &str = "drain:otel";

/// The OTLP drain encoder: [`crate::drain::otel::otlp_logs_payload`] over the
/// same native batch. Its attribute *names* are string values in the OTLP
/// JSON, not object keys, so [`EncodedSample::from_json`]'s key walk would
/// miss them — the guard folds them into the key set explicitly, keeping the
/// "is this content?" review gate as strong as the native wire's.
pub struct OtelDrainGuard;

impl ContentFreeEncoder for OtelDrainGuard {
    fn encoder_id(&self) -> &'static str {
        OTEL_DRAIN_ENCODER
    }

    fn drain_format(&self) -> Option<&'static str> {
        Some("otel")
    }

    fn allowed_keys(&self) -> &'static [&'static str] {
        OTEL_DRAIN_KEYS
    }

    fn encode_poisoned_sample(&self) -> Result<EncodedSample> {
        let batch = DrainBatch::from_events(&[poisoned_cloud_event()]);
        let payload = crate::drain::otel::otlp_logs_payload(&batch);
        let mut sample = EncodedSample::from_json(&payload)?;
        sample
            .keys
            .extend(crate::drain::otel::attribute_names(&payload));
        Ok(sample)
    }
}

/// Every key the OTLP payload may emit: the OTLP structural envelope plus the
/// `stella.*` attribute names mirroring the frozen v1 row contract.
const OTEL_DRAIN_KEYS: &[&str] = &[
    // OTLP structure
    "resourceLogs",
    "resource",
    "scopeLogs",
    "scope",
    "name",
    "version",
    "logRecords",
    "timeUnixNano",
    "body",
    "attributes",
    "key",
    "value",
    "stringValue",
    "intValue",
    "doubleValue",
    "boolValue",
    // resource/scope attributes
    "service.name",
    "stella.schema_version",
    // identity
    "stella.org_id",
    "stella.workspace_id",
    "stella.repo_id",
    "stella.project_id",
    // addressing / idempotency
    "stella.source_rowid",
    "stella.recorded_at",
    // content-free telemetry
    "stella.provider",
    "stella.model",
    "stella.input_tokens",
    "stella.output_tokens",
    "stella.cache_read_tokens",
    "stella.cache_miss_tokens",
    "stella.cache_write_tokens",
    "stella.cost_usd",
    "stella.duration_ms",
    "stella.retries",
    "stella.tool_calls",
    "stella.usage_complete",
];

/// The enterprise operational spool encoder: the *other* egress path in this
/// crate, under the same invariant. It is the harness's strongest member —
/// its source row (`ExecutionRollupRow`) genuinely carries a prompt preview
/// and a filesystem path, so the sentinels prove real suppression rather than
/// the absence of a field.
pub struct EnterpriseOperationalGuard;

impl ContentFreeEncoder for EnterpriseOperationalGuard {
    fn encoder_id(&self) -> &'static str {
        ENTERPRISE_OPERATIONAL_ENCODER
    }

    fn drain_format(&self) -> Option<&'static str> {
        None
    }

    fn allowed_keys(&self) -> &'static [&'static str] {
        ENTERPRISE_OPERATIONAL_KEYS
    }

    fn encode_poisoned_sample(&self) -> Result<EncodedSample> {
        let rollup = poisoned_execution_rollup();
        let context = OperationalEventContext::new(
            format!("{PASSTHROUGH_MARKER}-enrollment"),
            format!("{PASSTHROUGH_MARKER}-org"),
            format!("{PASSTHROUGH_MARKER}-ws"),
            OperationalIdentity::new(INSTALLATION_UUID_SENTINEL, STORE_UUID_SENTINEL)?,
            "0123456789abcdef0123456789abcdef",
            [ManagedModelDimension::new(&rollup.provider, &rollup.model)?],
        )?;
        let event = StellaOperationalEventV1::from_finalized_rollup(&context, &rollup)?;
        let value = serde_json::to_value(&event)
            .map_err(|err| StoreError(format!("cannot encode operational event: {err}")))?;
        EncodedSample::from_json(&value)
    }
}

/// Every key the closed operational schema may emit.
const ENTERPRISE_OPERATIONAL_KEYS: &[&str] = &[
    "schema",
    "event_class",
    "event_id",
    "enrollment_id",
    "organization_id",
    "workspace_id",
    "provider",
    "model",
    "outcome",
    "duration_ms",
    "input_tokens",
    "output_tokens",
    "cost_microusd",
    "tool_call_count",
    "changed_file_count",
    "produced_output",
];

/// Every egress encoder under the content-free guard. Adding an encoder means
/// adding it here — `every_registered_encoder_is_content_free` walks this list.
pub fn registered_encoders() -> Vec<Box<dyn ContentFreeEncoder>> {
    vec![
        Box::new(NativeDrainGuard),
        Box::new(OtelDrainGuard),
        Box::new(EnterpriseOperationalGuard),
    ]
}

/// Guard status of one cloud-drain `format` discriminator (#404).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainFormatGuard {
    /// Built and guarded — `encoder` must appear in [`registered_encoders`].
    Guarded {
        format: &'static str,
        encoder: &'static str,
    },
    /// Named by the epic (#403) but not implemented yet. A **declared gap**:
    /// building it means moving the entry to `Guarded` and registering an
    /// encoder, or the drain-format test fails.
    NotYetBuilt {
        format: &'static str,
        issue: &'static str,
    },
}

/// Every drain `format` the epic names. Keep in lockstep with the drain
/// config's `format` discriminator once #404 lands its enum.
pub const DRAIN_FORMATS: &[DrainFormatGuard] = &[
    DrainFormatGuard::Guarded {
        format: "stella",
        encoder: NATIVE_DRAIN_ENCODER,
    },
    DrainFormatGuard::Guarded {
        format: "otel",
        encoder: OTEL_DRAIN_ENCODER,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::UsageStore;

    // -- schema allowlist ---------------------------------------------------

    /// The hub `telemetry` columns as SQLite actually creates them.
    fn live_hub_telemetry_columns() -> Vec<String> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("usage.db");
        let store = UsageStore::open_at(&path).expect("open hub");
        drop(store);
        let conn = rusqlite::Connection::open(&path).expect("reopen hub");
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info('telemetry')")
            .expect("pragma");
        let names = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("rows");
        assert!(!names.is_empty(), "hub telemetry table must exist");
        names
    }

    #[test]
    fn hub_telemetry_schema_matches_the_reviewed_allowlist() {
        let mut live = live_hub_telemetry_columns();
        live.sort();
        let mut allowed: Vec<String> = HUB_TELEMETRY_COLUMNS
            .iter()
            .map(|c| (*c).to_string())
            .collect();
        allowed.sort();

        let added: Vec<&String> = live.iter().filter(|c| !allowed.contains(c)).collect();
        let removed: Vec<&String> = allowed.iter().filter(|c| !live.contains(c)).collect();
        assert!(
            added.is_empty() && removed.is_empty(),
            "hub `telemetry` schema drifted from the reviewed content-free allowlist.\n  \
             new columns not on the allowlist: {added:?}\n  \
             allowlisted columns no longer in the schema: {removed:?}\n\n\
             This gate is deliberate (#466). The hub is the only table a cloud drain reads, \
             so every column here is a candidate for egress. Adding one means editing \
             HUB_TELEMETRY_COLUMNS in the SAME change — which is the point: a human has to \
             look at the new name and answer \"is this content?\". AGENTS.md invariant #3: \
             prompts, paths, tool payloads/results, reasoning, errors, git state, memories, \
             rules, and local identifiers are never exportable."
        );
    }

    #[test]
    fn no_allowlisted_hub_column_reads_as_content() {
        let flagged: Vec<(&str, &str)> = HUB_TELEMETRY_COLUMNS
            .iter()
            .filter_map(|c| suspicious_substring(c).map(|m| (*c, m)))
            .collect();
        assert!(
            flagged.is_empty(),
            "hub telemetry columns whose names read content-bearing: {flagged:?} — \
             a column may not be allowlisted just because someone edited the list"
        );
    }

    #[test]
    fn native_drain_only_ships_columns_the_hub_actually_has() {
        // The wire cannot invent a field from nowhere: every native drain key
        // (minus the envelope) must exist as a reviewed hub column.
        let hub: BTreeSet<&str> = HUB_TELEMETRY_COLUMNS.iter().copied().collect();
        let envelope: BTreeSet<&str> = ["schema_version", "rows"].into_iter().collect();
        let invented: Vec<&&str> = NATIVE_DRAIN_KEYS
            .iter()
            .filter(|k| !envelope.contains(*k) && !hub.contains(*k))
            .collect();
        assert!(
            invented.is_empty(),
            "native drain ships keys with no hub column behind them: {invented:?}"
        );
    }

    // -- encoder harness ----------------------------------------------------

    #[test]
    fn every_registered_encoder_is_content_free() {
        let mut all = Vec::new();
        for encoder in registered_encoders() {
            all.extend(audit_encoder(encoder.as_ref()));
        }
        assert!(
            all.is_empty(),
            "content-free guard failed:\n{}",
            all.iter()
                .map(|v| format!("  - {v}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn registered_encoder_ids_are_unique() {
        let encoders = registered_encoders();
        let ids: BTreeSet<&str> = encoders.iter().map(|e| e.encoder_id()).collect();
        assert_eq!(
            ids.len(),
            encoders.len(),
            "two encoders share an id — failure messages would be ambiguous"
        );
    }

    #[test]
    fn every_built_drain_format_has_a_registered_encoder() {
        let registered: BTreeSet<&str> = registered_encoders()
            .iter()
            .map(|e| e.encoder_id())
            .collect();
        for entry in DRAIN_FORMATS {
            match entry {
                DrainFormatGuard::Guarded { format, encoder } => assert!(
                    registered.contains(encoder),
                    "drain format `{format}` is declared Guarded by encoder `{encoder}`, but \
                     `{encoder}` is not in registered_encoders() — the guard is a claim, not a \
                     check, until it is registered"
                ),
                DrainFormatGuard::NotYetBuilt { format, issue } => {
                    // A declared gap. When the encoder for `format` ships,
                    // move this entry to `Guarded` and register it.
                    assert!(
                        !issue.is_empty(),
                        "drain format `{format}` is an undeclared gap: NotYetBuilt must cite the \
                         issue that builds it"
                    );
                }
            }
        }
    }

    /// Every hub column an encoder does *not* ship is a deliberate exclusion,
    /// not an oversight — this pins the current native-drain exclusion set so
    /// quietly widening it is a visible diff.
    #[test]
    fn native_drain_exclusions_are_deliberate() {
        let shipped: BTreeSet<&str> = NATIVE_DRAIN_KEYS.iter().copied().collect();
        let excluded: Vec<&&str> = HUB_TELEMETRY_COLUMNS
            .iter()
            .filter(|c| !shipped.contains(*c))
            .collect();
        assert_eq!(
            excluded,
            vec![
                &"execution_id",
                &"step",
                &"call_role",
                &"estimated_input_tokens"
            ],
            "the native drain's hub-column exclusion set changed. Widening it ships more per \
             row to a remote intake — say so explicitly and bump DRAIN_SCHEMA_VERSION"
        );
    }

    // -- the harness itself bites -------------------------------------------

    /// A deliberately leaky encoder: blanket-serializes the poisoned hub row.
    /// This is the shape #427's OTLP encoder would take if someone reached for
    /// `derive(Serialize)` over the internal struct.
    struct LeakyEncoder;

    impl ContentFreeEncoder for LeakyEncoder {
        fn encoder_id(&self) -> &'static str {
            "test:leaky"
        }

        fn drain_format(&self) -> Option<&'static str> {
            None
        }

        fn allowed_keys(&self) -> &'static [&'static str] {
            &["org_id", "call_role", "prompt_preview"]
        }

        fn encode_poisoned_sample(&self) -> Result<EncodedSample> {
            let event = poisoned_cloud_event();
            let value = serde_json::json!({
                "org_id": event.org_id,
                "call_role": event.telemetry.call_role,
                "prompt_preview": content_sentinel("prompt_preview"),
            });
            EncodedSample::from_json(&value)
        }
    }

    #[test]
    fn harness_catches_a_leaking_encoder() {
        let found = audit_encoder(&LeakyEncoder);
        assert!(
            found
                .iter()
                .any(|v| matches!(v, Violation::SentinelLeaked { .. })),
            "harness must catch a sentinel on the wire: {found:?}"
        );
        assert!(
            found
                .iter()
                .any(|v| matches!(v, Violation::SuspiciousAllowlistEntry { .. })),
            "harness must flag a content-shaped allowlist entry: {found:?}"
        );
    }

    /// An encoder that emits nothing cannot pass by being empty.
    struct VacuousEncoder;

    impl ContentFreeEncoder for VacuousEncoder {
        fn encoder_id(&self) -> &'static str {
            "test:vacuous"
        }

        fn drain_format(&self) -> Option<&'static str> {
            None
        }

        fn allowed_keys(&self) -> &'static [&'static str] {
            &[]
        }

        fn encode_poisoned_sample(&self) -> Result<EncodedSample> {
            EncodedSample::from_json(&serde_json::json!({}))
        }
    }

    #[test]
    fn harness_catches_an_encoder_that_ignores_the_fixture() {
        let found = audit_encoder(&VacuousEncoder);
        assert_eq!(
            found,
            vec![Violation::FixtureNotUsed {
                encoder: "test:vacuous".into()
            }],
            "an encoder that never touches the poisoned fixture passes every sentinel check \
             vacuously — the passthrough marker is what makes the audit mean something"
        );
    }

    /// An encoder that emits an unreviewed key is caught even when the key is
    /// innocuously named — the allowlist is the gate, not the guess.
    struct UnreviewedKeyEncoder;

    impl ContentFreeEncoder for UnreviewedKeyEncoder {
        fn encoder_id(&self) -> &'static str {
            "test:unreviewed"
        }

        fn drain_format(&self) -> Option<&'static str> {
            None
        }

        fn allowed_keys(&self) -> &'static [&'static str] {
            &["org_id"]
        }

        fn encode_poisoned_sample(&self) -> Result<EncodedSample> {
            let event = poisoned_cloud_event();
            EncodedSample::from_json(&serde_json::json!({
                "org_id": event.org_id,
                "hostname": "workstation-7",
            }))
        }
    }

    #[test]
    fn harness_catches_an_unreviewed_key() {
        let found = audit_encoder(&UnreviewedKeyEncoder);
        assert_eq!(
            found,
            vec![Violation::UnallowedKey {
                encoder: "test:unreviewed".into(),
                key: "hostname".into(),
            }]
        );
    }

    // -- fixture integrity --------------------------------------------------

    #[test]
    fn fixtures_actually_carry_the_sentinels() {
        let event = poisoned_cloud_event();
        assert!(event.telemetry.call_role.starts_with(CONTENT_SENTINEL));
        assert!(event.org_id.contains(PASSTHROUGH_MARKER));

        let rollup = poisoned_execution_rollup();
        assert!(rollup.prompt_preview.starts_with(CONTENT_SENTINEL));
        assert!(rollup.prompt_digest.starts_with(CONTENT_SENTINEL));
        assert!(rollup.project_name.starts_with(CONTENT_SENTINEL));
        assert_eq!(rollup.project_root, PATH_SENTINEL);
        assert!(rollup.tool_histogram[0].tool.starts_with(CONTENT_SENTINEL));
        assert!(rollup.project_id.contains(PASSTHROUGH_MARKER));
    }

    #[test]
    fn sentinels_are_distinguishable_from_each_other() {
        let values: BTreeSet<&str> = FORBIDDEN_SENTINELS.iter().map(|s| s.value).collect();
        assert_eq!(
            values.len(),
            FORBIDDEN_SENTINELS.len(),
            "two sentinels share a value — a leak report could name the wrong category"
        );
        assert!(
            !FORBIDDEN_SENTINELS
                .iter()
                .any(|s| s.value.contains(PASSTHROUGH_MARKER)),
            "a sentinel that contains the passthrough marker would make every audit vacuous"
        );
    }
}
