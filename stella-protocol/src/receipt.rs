//! The context-receipts vocabulary: the block, manifest, and recall-accounting
//! types that ride [`crate::event::AgentEvent`].
//!
//! Split verbatim out of `event.rs` — no behavior change, no wire change — so
//! the receipts plane has a file of its own to grow in. Every type here is
//! re-exported from [`crate::event`] as well as from the crate root, so both
//! `stella_protocol::BlockKind` and `stella_protocol::event::BlockKind`
//! continue to resolve; a pure move that broke either path would not be pure.
//!
//! Why these belong together: they are the persisted receipt of one model
//! call. A block is registered once, content-addressed, and cited by a
//! manifest entry per step it is resident; a recall's frames and its
//! per-provider usage report account for how the volatile portion of that
//! receipt was produced. They share a forward-compatibility discipline —
//! every field added since the first release carries `serde(default)`, and
//! both closed enums carry a `#[serde(other)]` fallback — because these rows
//! are read back from journals written by older and newer binaries alike.

use serde::{Deserialize, Serialize};

/// The semantic kind of one context block — one durable, individually
/// attributable unit that can enter the model's prompt. Finer-grained than a
/// `CompletionMessage`: a tool message holding several results decomposes into
/// one `ToolResult` block per `call_id`. See the session-telemetry-receipts
/// spec (`docs/design/session-telemetry-receipts-spec.md`, §4). Forward-compat:
/// an unknown kind read from a newer emitter deserializes to [`BlockKind::Other`]
/// rather than failing the whole event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    /// Message index 0 — the stable system prefix, never compacted.
    SystemPrefix,
    /// The user's task text.
    UserGoal,
    /// A context frame injected by recall (memory/graph/file).
    RecalledFrame,
    /// Model prose.
    AssistantText,
    /// An assistant tool-call request.
    ToolCall,
    /// A tool output (Ok or Error).
    ToolResult,
    /// A mid-turn injected user (steering) message.
    Steered,
    /// An overflow-summarizer replacement span.
    Summary,
    /// A multimodal attachment (image/doc/audio/video).
    Attachment,
    /// A kind this reader does not recognize (written by a newer emitter).
    ///
    /// Tolerant on the way in, **lossy on the way out**: the original token is
    /// not preserved, so re-serializing an event that arrived with a future
    /// kind writes `"other"`. Unlike [`crate::event::AgentEvent::Unknown`], which keeps the
    /// whole original object, a proxy or `replay::to_jsonl` that round-trips a
    /// newer emitter's `block_registered` rewrites the kind. Widen this enum
    /// before relaying a stream whose kinds matter downstream.
    #[default]
    #[serde(other)]
    Other,
}

/// A block's cache position relative to the provider's prompt-cache
/// breakpoints. Stella keeps the system prefix byte-stable and places volatile
/// recall after it (L-E8), so a block's zone is computable from its position at
/// manifest time. A structural hint at emission; reconciled against reported
/// usage by cache attribution (spec §7). Forward-compat via [`CacheZone::Other`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CacheZone {
    /// At/before the system-block breakpoint — should cache-hit every step.
    StablePrefix,
    /// Before the conversation-tail breakpoint — cacheable across steps.
    #[default]
    Cacheable,
    /// After the last breakpoint — recomputed every step by construction.
    Volatile,
    /// A zone this reader does not recognize (written by a newer emitter).
    /// Lossy on re-serialization for the same reason [`BlockKind::Other`] is:
    /// the original token is not kept, so a relayed manifest entry comes back
    /// out as `"other"`.
    #[serde(other)]
    Other,
}

/// A context frame as cited in a `ContextRecall` event. `citation_label`
/// is mandatory and human-readable; the raw `id` (when the frame is
/// materialized at all) belongs only in inspectable detail views, never as
/// the primary identifier (L-C4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ContextFrameRef {
    /// The provider's own frame id, when the frame was materialized at all.
    /// A detail-view identifier only — never the primary one a surface shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The human-readable citation every surface renders, e.g.
    /// `engine step-driver (driver.rs)`. Mandatory by design (L-C4).
    pub citation_label: String,
    /// The CGP provider leg that returned the frame. Empty only when reading
    /// a stream recorded before provider provenance was added.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    /// The original source named by the frame's provenance chain. This is
    /// deliberately distinct from [`Self::provider`]: a host adapter may be
    /// `workspace-memory` while the record source remains `stella-context`.
    pub source: String,
    /// The protocol frame kind (`symbol`, `memory`, `graph`, ...).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// Canonical source URI when the frame supplied one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// The most-derived provenance method, when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// The engine's estimate of what injecting this frame cost the prompt.
    pub token_cost: u32,
    /// The registry id (`blk_…`) of this frame as a context block, when
    /// receipts are enabled. Joins the frame to its manifest membership and,
    /// for memory frames, to the write→citation loop (spec §5.3, §9). Absent on
    /// streams recorded before receipts existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_id: Option<String>,
    /// `"sha256:<hex>"` of the exact injected text. The digest rides the wire;
    /// the content itself is journaled only locally (never exported), closing
    /// the recall-content gap G1 without widening the content-free export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_digest: Option<String>,
}

/// Where a context block came from — the provenance stamped once at a block's
/// birth (spec §4). The join hub: a `RecalledFrame` carries the `memory_id` it
/// was recalled from; a `ToolResult`/`ToolCall` carries its `call_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BlockOrigin {
    /// Monotonic per session — the `run_turn` that produced the block.
    pub turn_instance: u32,
    /// The step within that turn.
    pub step: usize,
    /// Tool-call correlation id, for `ToolCall`/`ToolResult` blocks — the
    /// call that *first* minted this block. Block ids are content-addressed,
    /// so two distinct calls with byte-identical output share one id and only
    /// the first is registered: this is birth provenance, **not** the complete
    /// set of calls that carried the block. For "which calls did this block
    /// serve at step N", read [`ManifestEntry::call_id`], which is recorded
    /// per occurrence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// The `nod_…` memory node id, for a `RecalledFrame` that is a memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_id: Option<String>,
}

/// One block's membership in a step's manifest (spec §5): its id, its cache
/// zone at that step, its estimated token cost, and how long it has been
/// resident. Residency × cost is what makes cost-of-carry a real number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ManifestEntry {
    pub block_id: String,
    /// Cache position class relative to the last stable breakpoint. Defaults to
    /// [`CacheZone::Cacheable`] on a manifest recorded before the field existed
    /// — an assumption, not an observation, so a cache-attribution pass over
    /// pre-field manifests is reading the default rather than a measured zone.
    #[serde(default)]
    pub cache_zone: CacheZone,
    pub token_cost: u32,
    /// The step this block first entered a manifest — drives cost-of-carry.
    pub resident_since_step: usize,
    /// Which `CompletionMessage` this block belonged to, by position in the
    /// sent sequence. Event-granular blocks (a tool message's several results,
    /// an assistant message's text + calls) share one `message_index`, so
    /// reconstruction regroups them back into the exact message boundaries
    /// rather than inferring them from kinds (spec §5.1). `0` on manifests
    /// recorded before reconstruction existed.
    #[serde(default)]
    pub message_index: usize,
    /// The tool call this *occurrence* belongs to, for `ToolCall`/`ToolResult`
    /// blocks. Content-addressing collapses byte-identical blocks onto one
    /// `block_id`, so [`BlockOrigin::call_id`] only ever names the first call
    /// to mint it — two `git status` runs with identical output register once.
    /// Recording the id per manifest entry keeps the attribution complete:
    /// joining a compaction event's evicted/deduped/aged `block_id`s against
    /// the manifests that carried them answers "which *calls* left context"
    /// without under-reporting the duplicates. `None` for non-tool blocks, and
    /// on manifests recorded before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
}

/// One provider's share of a recall's frame mix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProviderShare {
    /// The CGP provider leg the frames came from.
    pub provider: String,
    /// How many of the recall's frames this provider contributed — the ones
    /// that won fusion and reached the prompt, not the ones it served.
    pub frames: u32,
}

/// One provider's contribution to a recall's cost, as the CGP usage report
/// defines it (`docs/context-reuse.md` §2 `ProviderUsage`).
///
/// Distinct from [`ProviderShare`], which counts only the frames that *won*
/// fusion and reached the prompt. This counts what the provider **served to
/// the host** and what the host **rejected** (a budget lie, a consent gate, a
/// failed leg), with the token cost behind each — the difference between "what
/// did the model see?" and "what did this turn cost, and who drove it?".
///
/// Content-free by construction: a provider id, three numbers. No frame
/// titles, bodies, URIs, or query text ever enter this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ContextProviderUsage {
    /// The host's routing/consent key for the serving provider.
    pub provider_id: String,
    /// Frames accepted — they passed consent, the timeout, and the
    /// budget-honesty audit.
    pub frames_served: u32,
    /// Frames the host dropped whole, e.g. a provider that misdeclared cost.
    pub frames_rejected: u32,
    /// This provider's contribution to `budget_consumed`.
    pub token_cost: u64,
}

/// The per-request roll-up of what one context recall cost
/// (`docs/context-reuse.md` §2 `UsageReport`) — the envelope a metering
/// pipeline bills from, and the answer to "what did this turn's context cost,
/// and which sources drove it?".
///
/// Deliberately **content-free**: budget scalars, an accounting timestamp, and
/// per-provider counts and costs. The spec's `served_frames` drill-down is
/// *not* duplicated here — the sibling `frames: Vec<ContextFrameRef>` on the
/// same [`crate::event::AgentEvent::ContextRecall`] already records the frame-granular
/// identities locally, so an auditor still walks from a total to its frames
/// without this type ever carrying one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ContextUsage {
    /// The query's `max_tokens` — the budget this recall was allowed.
    pub budget_requested: u32,
    /// The summed `token_cost` of every served frame.
    pub budget_consumed: u64,
    /// The report's accounting snapshot (RFC 3339), stamped by the host. This
    /// is the *accounting event* time, never the query's bi-temporal `as_of`
    /// retrieval pin — two different clocks.
    pub as_of: String,
    /// One entry per provider the query reached.
    pub providers: Vec<ContextProviderUsage>,
}

impl ContextUsage {
    /// Whether the report re-sums: `budget_consumed` must equal the summed
    /// per-provider `token_cost` (`docs/context-reuse.md` §2, the arithmetic
    /// identity). A metering pipeline checks this before trusting a total, so
    /// a corrupted number is a checkable failure rather than a silent misbill.
    ///
    /// The sum saturates rather than wrapping: this runs over journal bytes a
    /// consumer did not write, and an audit predicate must answer `false` on a
    /// corrupt report, never panic on a debug-build overflow.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        let mut total: u64 = 0;
        for provider in &self.providers {
            total = total.saturating_add(provider.token_cost);
        }
        self.budget_consumed == total
    }

    /// Total frames served across every provider the query reached.
    #[must_use]
    pub fn total_frames_served(&self) -> u64 {
        let mut total: u64 = 0;
        for provider in &self.providers {
            total = total.saturating_add(u64::from(provider.frames_served));
        }
        total
    }

    /// Whether `as_of` carries the RFC 3339 `date-time` shape its doc
    /// promises. The field is a bare `String`, so nothing on the wire stops a
    /// host from stamping a receipt with a value no downstream tool can sort
    /// by; this is the check a metering pipeline runs beside
    /// [`Self::is_consistent`] before trusting the accounting clock.
    ///
    /// Shape only, never calendar arithmetic — and "shape" is looser than the
    /// grammar. `2026-02-31T00:00:00Z` passes (no month has 31 February days),
    /// and so does `2026-07-24T99:99:99Z`, because the digit groups are checked
    /// for being digits, not for RFC 3339's `00-23`/`00-59` ranges. The point
    /// is to catch a receipt stamped with junk (an empty string, a locale date,
    /// a missing offset) before it reaches a tool that sorts by it; a real
    /// parser still has to reject the impossible values, and this crate carries
    /// no date dependency to become one.
    #[must_use]
    pub fn as_of_is_wellformed(&self) -> bool {
        is_rfc3339_shaped(&self.as_of)
    }
}

/// `full-date "T" full-time` per RFC 3339 §5.6: `YYYY-MM-DDThh:mm:ss`, an
/// optional `.`-led fraction, then `Z` or a `±hh:mm` offset.
///
/// Deliberately accepting of everything the grammar allows — a lowercase
/// `t`/`z`, fractional seconds, and a numeric offset are all legal, and a
/// predicate that flagged them would be a false-alarm generator on valid
/// receipts rather than a check anyone would keep running.
fn is_rfc3339_shaped(value: &str) -> bool {
    let bytes = value.as_bytes();
    // `full-date "T" partial-time` is exactly 19 bytes; the offset adds one
    // more at minimum (a bare `Z`).
    if bytes.len() < 20 {
        return false;
    }
    let all_digits = |slice: &[u8]| slice.iter().all(u8::is_ascii_digit);

    let date_shaped = all_digits(&bytes[0..4])
        && bytes[4] == b'-'
        && all_digits(&bytes[5..7])
        && bytes[7] == b'-'
        && all_digits(&bytes[8..10]);
    let time_shaped = all_digits(&bytes[11..13])
        && bytes[13] == b':'
        && all_digits(&bytes[14..16])
        && bytes[16] == b':'
        && all_digits(&bytes[17..19]);
    if !date_shaped || !matches!(bytes[10], b'T' | b't') || !time_shaped {
        return false;
    }

    let mut rest = &bytes[19..];
    if rest[0] == b'.' {
        let fraction = rest[1..].iter().take_while(|b| b.is_ascii_digit()).count();
        if fraction == 0 {
            return false;
        }
        rest = &rest[1 + fraction..];
    }
    match rest {
        [b'Z' | b'z'] => true,
        [b'+' | b'-', h1, h2, b':', m1, m2] => {
            h1.is_ascii_digit() && h2.is_ascii_digit() && m1.is_ascii_digit() && m2.is_ascii_digit()
        }
        _ => false,
    }
}
