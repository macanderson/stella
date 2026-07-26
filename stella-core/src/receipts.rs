//! Context-receipts emission (spec §4/§5), kept out of the driver body so the
//! step loop calls one helper and the emission logic stays independently
//! testable. Produces, per step: a `BlockRegistered` event for each context
//! block the model has not seen before (content-addressed, emitted once), then
//! a `StepManifest` naming the blocks it saw this step in wire order.
//!
//! Content-free by construction: blocks carry a `content_digest`, never the
//! payload bytes — the preimage already lives in the originating event in the
//! journal, so this is an index over the fold, not a second content store.
//!
//! Granularity is event-level (increment 1): the engine decomposes what it can
//! see in the message vec — the system prefix, each assistant text and
//! tool-call, and each tool result by `call_id`. Splitting the recalled user
//! message into per-frame `RecalledFrame` blocks (with `memory_id`) is the
//! memory-join increment (spec §9), where the pipeline participates.

use std::borrow::Cow;
use std::collections::HashMap;

use serde::Serialize;
use sha2::{Digest, Sha256};
use stella_protocol::{
    AgentEvent, BlockKind, BlockOrigin, CacheZone, CompiledContextFrameBuilt, CompletionMessage,
    ManifestEntry, MessageRole, ModelCallRole, ToolOutput,
};

use crate::estimator::{CHARS_PER_TOKEN, estimate_conversation_tokens};
use crate::event_sender::EventSender;

/// Who served the call a receipt describes.
///
/// These three travel together and are only known at the settled boundary — the
/// served model can differ from the requested one — so they are one parameter
/// rather than three positional strings a caller can transpose.
#[derive(Debug, Clone, Copy)]
pub struct ServedBy<'a> {
    /// Which role made the call (worker, summarizer, a pipeline management role).
    pub role: ModelCallRole,
    /// The provider id that answered.
    pub provider: &'a str,
    /// The model the provider reported actually serving.
    pub model: &'a str,
}

/// `StepManifest::call_seq` of the engine's own worker call — the step loop's
/// model call, the one that already had a receipt.
pub const RECEIPT_SEQ_WORKER: u64 = 0;

/// `StepManifest::call_seq` of the overflow summarizer. A constant rather than
/// a counter because compaction runs at most once per step, so the summarizer
/// can collide with nothing else at its `(turn_instance, step)`.
pub const RECEIPT_SEQ_SUMMARIZER: u64 = 1;

/// First `call_seq` available to callers that must allocate their own — the
/// pipeline's management roles, which have no step loop to key against and so
/// draw a monotonic sequence per execution. Starts above the reserved worker
/// and summarizer seats.
pub const RECEIPT_SEQ_ALLOCATED_BASE: u64 = 2;

// Counts SHA-256 passes over block content. Every hash this module performs
// funnels through `sha256_hex_parts` — `block_id`, `content_digest` and
// `tool_result_block_id` all reach it — so this is the one choke point that can
// witness the per-step hashing cost.
//
// Each pass is Θ(content), so on a long turn the difference between hashing a
// block once and re-hashing it on every step is the difference between linear
// and quadratic work in the turn's length. Test-only; thread-local so it measures
// one turn's own work rather than whatever else the test binary is doing.
#[cfg(test)]
thread_local! {
    static HASH_PASSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Zero this thread's hash-pass counter and return the previous value.
#[cfg(test)]
pub(crate) fn take_hash_passes() -> usize {
    HASH_PASSES.with(|c| c.replace(0))
}

/// `sha256` hex over the concatenation of `parts`, hashed as a stream. Feeding
/// the parts one `update` at a time is byte-identical to hashing their joined
/// bytes, so a caller with a multi-part preimage gets the same digest without
/// allocating the join — which matters because these run over the whole
/// transcript on every committed step. Byte-wise hex (the sha2 0.11 output type
/// does not implement `LowerHex` directly), matching the context store's
/// `to_hex`.
fn sha256_hex_parts(parts: &[&[u8]]) -> String {
    use std::fmt::Write as _;
    #[cfg(test)]
    HASH_PASSES.with(|c| c.set(c.get() + 1));
    let mut h = Sha256::new();
    for part in parts {
        h.update(part);
    }
    let mut hex = String::with_capacity(64);
    for byte in h.finalize() {
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

/// `sha256` hex of a string.
fn sha256_hex(s: &str) -> String {
    sha256_hex_parts(&[s.as_bytes()])
}

/// The content-addressed id of a block: `blk_` + the first 24 hex chars of
/// `sha256(kind_tag \0 content)`. Mirrors the context store's `nod_…` shape.
/// Byte-identical blocks of the same kind share an id — the property that makes
/// dedup/supersession identities and lets residency track a block across steps.
///
/// The preimage is hashed as three streamed parts rather than a `format!`-joined
/// String: the id is unchanged (concatenation is what the stream hashes) and the
/// per-step copy of every block's full content is gone.
fn block_id(kind: BlockKind, content: &str) -> String {
    format!(
        "blk_{}",
        &sha256_hex_parts(&[kind_tag(kind).as_bytes(), b"\0", content.as_bytes()])[..24]
    )
}

/// The `block_id` of a tool result, computed from the same canonical content
/// (`serde_json` of the output) that [`decompose`] uses — so a result compaction
/// stubs resolves to the exact block the manifest cited. Shared with
/// `crate::compaction` so its `Compaction` report can name evicted/deduped/
/// superseded/aged blocks by identity, not just count (spec §6.2).
pub(crate) fn tool_result_block_id(output: &ToolOutput) -> String {
    block_id(
        BlockKind::ToolResult,
        &serde_json::to_string(output).unwrap_or_default(),
    )
}

/// The stable snake_case tag for a block kind — kept in lockstep with the
/// protocol enum's `rename_all = "snake_case"` so a block's id and its stored
/// `kind` string agree.
fn kind_tag(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::SystemPrefix => "system_prefix",
        BlockKind::UserGoal => "user_goal",
        BlockKind::RecalledFrame => "recalled_frame",
        BlockKind::AssistantText => "assistant_text",
        BlockKind::ToolCall => "tool_call",
        BlockKind::ToolResult => "tool_result",
        BlockKind::Steered => "steered",
        BlockKind::Summary => "summary",
        BlockKind::Attachment => "attachment",
        BlockKind::Other => "other",
    }
}

/// The stable snake_case tag for a cache zone, in lockstep with the protocol
/// enum's `rename_all` for the same reason [`kind_tag`] is: the frame preimage
/// carries the string, and a divergence would move every stored frame hash
/// without moving anything a reader can see.
fn zone_tag(zone: CacheZone) -> &'static str {
    match zone {
        CacheZone::StablePrefix => "stable_prefix",
        CacheZone::Cacheable => "cacheable",
        CacheZone::Volatile => "volatile",
        CacheZone::Other => "other",
    }
}

/// The engine's raw per-block token estimate: one block's content over the
/// conversation estimator's [`CHARS_PER_TOKEN`] divisor, so a block's cost is
/// on the same scale as `StepUsage.estimated_input_tokens`.
///
/// The counting UNIT differs and is worth stating rather than assuming: this
/// counts chars, the conversation estimator counts bytes
/// (`crate::estimator::estimate_message_tokens` sums `str::len`). For ASCII
/// the two agree exactly; for multi-byte content a manifest's summed
/// `token_cost` reads lower than the same step's `estimated_input_tokens`.
/// Reconciling the units is a receipts-fidelity change (every stored
/// manifest's numbers move), not a comment fix, so the divergence is named
/// here rather than papered over.
fn estimate_tokens(content: &str) -> u32 {
    (content.chars().count() as f64 / CHARS_PER_TOKEN).ceil() as u32
}

/// Whether a block kind's preimage lives in the event journal (resolved at
/// reconstruction time) or must be captured locally at emission. Tool I/O and
/// assistant text ride the journal (`ToolStart`/`ToolResult`/`Text`); the
/// system prefix and the assembled user/recall/steer/summary messages do not,
/// so their bytes are carried as local-only block content (spec §5.3).
fn is_gap_kind(kind: BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::SystemPrefix
            | BlockKind::UserGoal
            | BlockKind::RecalledFrame
            | BlockKind::Steered
            | BlockKind::Summary
    )
}

/// Counts in-place rewrites of the transcript, as distinct from appends.
///
/// Two per-step memos — the driver's result identities and this module's block
/// digests — are keyed by a message's POSITION. A position is a stable name for a
/// message's bytes only as long as nothing rewrites what is already there.
/// Appends never invalidate one; compaction (which stubs and ages tool results in
/// place) and the overflow summarizer (which splices a span down to a single
/// message) both do.
///
/// It is bumped at the mutation sites themselves, and every consumer takes it as
/// an argument rather than reading it from shared state, so a future pass that
/// rewrites the transcript cannot quietly skip invalidation and leave a memo
/// serving digests for bytes that no longer exist.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TranscriptRevision(u64);

impl TranscriptRevision {
    /// Record that the transcript was rewritten in place.
    pub fn rewritten(&mut self) {
        self.0 = self.0.wrapping_add(1);
    }
}

/// The per-block work that is expensive and position-stable: two SHA-256 passes
/// over the block's content plus a full character count of it.
#[derive(Clone)]
struct BlockDigests {
    block_id: String,
    content_digest: String,
    token_cost: u32,
}

/// Position-keyed memo over [`BlockDigests`], valid for one transcript revision.
///
/// `block_id` and `content_digest` hash overlapping preimages (`kind_tag ‖ NUL ‖
/// content` and `content`) and cannot share one hash state, so a block costs two
/// full passes over its bytes plus a third to count characters — and `decompose`
/// recomputed all three for every block in the transcript on every step, plus a
/// `serde_json` re-serialization of every tool call and tool result to obtain the
/// preimage in the first place. The manifest has to *name* every block each step,
/// but the bytes behind an unchanged block do not change, so naming it again is
/// the only part that has to happen twice.
#[derive(Default)]
struct BlockDigestCache {
    entries: HashMap<(usize, usize), BlockDigests>,
    revision: TranscriptRevision,
}

impl BlockDigestCache {
    /// Drop everything if the transcript was rewritten since the last emission.
    fn sync_to(&mut self, revision: TranscriptRevision) {
        if self.revision != revision {
            self.entries.clear();
            self.revision = revision;
        }
    }

    /// The digests for the block at `key`, computing them only on a miss.
    ///
    /// `content` is a closure so a hit skips producing the preimage as well as
    /// hashing it — for tool calls and tool results that preimage is a fresh
    /// `serde_json::to_string` of the whole payload. It yields a `Cow` so a miss
    /// costs nothing extra either: the kinds whose preimage is already a field on
    /// the message borrow it, and only the serialized kinds allocate.
    fn digests<'c>(
        &mut self,
        key: (usize, usize),
        kind: BlockKind,
        content: impl FnOnce() -> Cow<'c, str>,
    ) -> BlockDigests {
        if let Some(hit) = self.entries.get(&key) {
            return hit.clone();
        }
        let content = content();
        let computed = BlockDigests {
            block_id: block_id(kind, &content),
            content_digest: format!("sha256:{}", sha256_hex(&content)),
            token_cost: estimate_tokens(&content),
        };
        self.entries.insert(key, computed.clone());
        computed
    }
}

/// One decomposed context block, before it becomes a manifest entry.
struct BlockDraft<'a> {
    block_id: String,
    kind: BlockKind,
    content_digest: String,
    token_cost: u32,
    call_id: Option<String>,
    cache_zone: CacheZone,
    /// Which sent `CompletionMessage` this block belonged to — its position in
    /// the message vector — so reconstruction regroups exactly (spec §5.1).
    message_index: usize,
    /// Local-only preimage for gap kinds; `None` for journal-resolvable kinds.
    ///
    /// **Borrowed**, and copied only at the one site that needs it: a block's
    /// bytes ride `BlockRegistered`, which fires once per block for the whole
    /// turn. Owning it here meant re-cloning the system prefix — the largest gap
    /// block there is — on every step for the rest of the turn, to hand it to a
    /// registration that had already happened.
    content: Option<&'a str>,
    /// The `nod_…` record this block was recalled from, for a `RecalledFrame`.
    /// The join hub of [`BlockOrigin`], filled by decomposition when the block
    /// is a recall frame and `None` for every structural kind.
    memory_id: Option<String>,
    /// The human label a recall frame is cited under, carried to
    /// `BlockRegistered::citation_label` so a receipt names frames rather than
    /// ids (L-C4).
    citation_label: Option<String>,
    /// Why this block is in the prompt, when the producer knows. `None` for
    /// structural blocks, whose reason is that they are structural.
    selection_reason: Option<String>,
}

impl<'a> BlockDraft<'a> {
    /// This block as it enters the frame preimage.
    fn frame_block(&self) -> FrameBlock<'_> {
        FrameBlock {
            block_id: &self.block_id,
            kind: kind_tag(self.kind),
            cache_zone: zone_tag(self.cache_zone),
            token_cost: self.token_cost,
            message_index: self.message_index,
            content_digest: &self.content_digest,
            call_id: self.call_id.as_deref(),
            memory_id: self.memory_id.as_deref(),
            selection_reason: self.selection_reason.as_deref(),
        }
    }
}

/// Decompose the live message vector into event-granular blocks, in wire order,
/// tagging each with its source message index (for reconstruction regrouping)
/// and a structural cache zone. The zone is a hint at emission — the system
/// prefix is the stable-cached head (L-E8) and the final message is the live
/// tail that is recomputed each step; cache attribution (spec §7, A3) refines
/// these against reported usage.
///
/// Two known gaps, named rather than papered over — closing either moves every
/// stored manifest's numbers or ids, so both are fidelity increments, not
/// comment fixes:
///
/// - **Every `User` message becomes [`BlockKind::UserGoal`].** The driver also
///   injects User-role messages that are not the user's goal: the overflow
///   summary (`driver::SUMMARY_MARKER_PREFIX`) and the stuck-loop steer
///   (`driver::LOOP_STEER_PREFIX`), plus real mid-turn steers from
///   `ports::TurnSteering`. [`BlockKind::Steered`] and [`BlockKind::Summary`]
///   exist for exactly those and are never emitted from here, so a receipt
///   attributes engine-generated text to the user. Reclassifying changes
///   `kind_tag`, and with it the block id.
/// - **Attachments are not decomposed.** `CompletionMessage::attachments`
///   yields no block, while `crate::estimator` counts its base64 weight — so on
///   a multimodal turn the manifest's summed `token_cost` reads materially
///   below the same event's `estimated_input_tokens`. [`BlockKind::Attachment`]
///   is the seat reserved for it.
fn decompose<'a>(
    messages: &'a [CompletionMessage],
    cache: &mut BlockDigestCache,
) -> Vec<BlockDraft<'a>> {
    let mut drafts = Vec::new();
    for (message_index, message) in messages.iter().enumerate() {
        // Ordinal within this message, counted over every block it yields
        // regardless of kind, so `(message_index, ordinal)` names one block
        // uniquely and identically on every step of a revision.
        let mut ordinal = 0usize;
        let mut push = |drafts: &mut Vec<BlockDraft<'a>>,
                        cache: &mut BlockDigestCache,
                        kind: BlockKind,
                        content: &dyn Fn() -> Cow<'a, str>,
                        borrowed: Option<&'a str>,
                        call_id: Option<String>| {
            let digests = cache.digests((message_index, ordinal), kind, content);
            ordinal += 1;
            drafts.push(BlockDraft {
                block_id: digests.block_id,
                kind,
                content_digest: digests.content_digest,
                token_cost: digests.token_cost,
                call_id,
                cache_zone: CacheZone::Cacheable,
                message_index,
                content: is_gap_kind(kind).then_some(borrowed).flatten(),
                // Decomposition fills these for recall frames; every
                // structural kind leaves them `None` (#713).
                memory_id: None,
                citation_label: None,
                selection_reason: None,
            });
        };
        match message.role {
            MessageRole::System => {
                push(
                    &mut drafts,
                    cache,
                    BlockKind::SystemPrefix,
                    &|| Cow::Borrowed(message.content.as_str()),
                    Some(message.content.as_str()),
                    None,
                );
            }
            MessageRole::User => {
                // The recalled frames live inside this message; splitting them
                // into per-frame blocks is the memory-join increment (§9).
                push(
                    &mut drafts,
                    cache,
                    BlockKind::UserGoal,
                    &|| Cow::Borrowed(message.content.as_str()),
                    Some(message.content.as_str()),
                    None,
                );
            }
            MessageRole::Assistant => {
                if !message.content.is_empty() {
                    push(
                        &mut drafts,
                        cache,
                        BlockKind::AssistantText,
                        &|| Cow::Borrowed(message.content.as_str()),
                        Some(message.content.as_str()),
                        None,
                    );
                }
                for call in &message.tool_calls {
                    push(
                        &mut drafts,
                        cache,
                        BlockKind::ToolCall,
                        &|| Cow::Owned(serde_json::to_string(call).unwrap_or_default()),
                        None,
                        Some(call.call_id.clone()),
                    );
                }
            }
            MessageRole::Tool => {
                for result in &message.tool_results {
                    push(
                        &mut drafts,
                        cache,
                        BlockKind::ToolResult,
                        &|| Cow::Owned(serde_json::to_string(&result.output).unwrap_or_default()),
                        None,
                        Some(result.call_id.clone()),
                    );
                }
            }
        }
    }
    // Structural zones: the head is the stable prefix, the tail is volatile.
    // Set the tail first so a single-block conversation ends StablePrefix.
    if let Some(last) = drafts.last_mut() {
        last.cache_zone = CacheZone::Volatile;
    }
    if let Some(first) = drafts.first_mut()
        && first.kind == BlockKind::SystemPrefix
    {
        first.cache_zone = CacheZone::StablePrefix;
    }
    drafts
}

// ── The compiled frame (Phase 2, #713 deliverable 4) ────────────────────────

/// Schema version of the frame preimage. Part of the hashed body on purpose: a
/// later change to which fields participate must produce different hashes for
/// the same prompt, or two incompatible preimages would silently share a digest
/// space and a replay could not tell which scheme minted a stored hash.
pub const FRAME_SCHEMA_VERSION: &str = "1.0-draft";

/// One block as it enters the frame preimage.
///
/// Deliberately NOT [`ManifestEntry`]: `resident_since_step` is on the manifest
/// and must not be on the frame. It records how long a block has been carried,
/// which is a fact about the *history* of the session rather than about what
/// this prompt contained — a turn replayed from step 0 and the same turn reached
/// after a compaction see identical bytes with different residencies. Hashing it
/// would make the frame hash a function of when you started looking.
#[derive(Debug, Serialize)]
struct FrameBlock<'a> {
    block_id: &'a str,
    /// The block's kind tag — the same string that is half its id preimage.
    /// Carried explicitly so a frame body is readable without a registry join.
    kind: &'a str,
    cache_zone: &'a str,
    token_cost: u32,
    message_index: usize,
    /// `sha256:<hex>` of the exact bytes. Present so the frame hash is a
    /// function of *content*, not only of the 96-bit id prefix that stands in
    /// for it, and so a stored frame can be verified against the block registry
    /// without recomputing every id.
    content_digest: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    call_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selection_reason: Option<&'a str>,
}

/// The canonical body a [`CompiledContextFrameBuilt::frame_hash`] is taken over.
///
/// Every field here is stable across two runs of identical work. The volatile
/// fields of the manifest event are absent by construction rather than by
/// filtering, so adding one back is a visible edit to this struct:
///
/// - `provider` / `model` — the served model can differ per run and per
///   fallback, and the same prompt is the same prompt whoever answers it;
/// - `call_seq` — an allocation order within an execution, not a property of
///   the prompt;
/// - `effective_budget_tokens` / `calibration_factor` /
///   `estimated_input_tokens` — accounting *about* the frame, and the first two
///   move with a per-model calibration that learns across a session;
/// - `resident_since_step` (per block) — see [`FrameBlock`].
#[derive(Debug, Serialize)]
struct FrameBody<'a> {
    schema_version: &'a str,
    turn_instance: u32,
    step: usize,
    /// Which role's prompt this is. Kept: a worker frame and a summarizer frame
    /// at the same step are different frames, and `call_seq` — the field that
    /// would otherwise distinguish them — is excluded as volatile.
    role: ModelCallRole,
    blocks: Vec<FrameBlock<'a>>,
}

/// The identity of the compiled frame a set of blocks constitutes: a
/// content-addressed id and the byte-stable hash it derives from.
///
/// The hash reuses the canonical scheme ADR 0004 ratified —
/// [`crate::context_record::hash::record_hash`]: strip nulls, RFC 8785 JCS,
/// sha256, `sha256:` prefix. A second hashing scheme in one codebase is two
/// answers to "are these the same bytes", so this calls the existing one rather
/// than growing a parallel canonicalizer next to [`block_id`]'s streamed
/// sha256 (which hashes raw prompt bytes, not JSON, and is a different job).
///
/// The id is `cf_` + the first 24 hex chars of the hash, mirroring `blk_…` and
/// the context store's `nod_…`. Derived from the hash rather than allocated so
/// two runs that produce the same frame also produce the same id — an id drawn
/// from a counter would make the determinism gate untestable across processes.
///
/// Returns `None` only when the body will not canonicalize, which for this
/// shape (owned scalars and strings) does not happen — but a receipt is
/// telemetry, and no frame identity is worth failing a turn over.
fn frame_identity(body: &FrameBody<'_>) -> Option<CompiledContextFrameBuilt> {
    let frame_hash = crate::context_record::hash::record_hash(body).ok()?;
    let hex = frame_hash.strip_prefix("sha256:")?;
    Some(CompiledContextFrameBuilt {
        compiled_frame_id: format!("cf_{}", &hex[..24]),
        frame_hash,
    })
}

/// Per-turn receipt state: which blocks have been registered (and at which
/// step they first appeared, for residency), plus the effective compaction
/// budget the driver computed for the step about to run. Lives on the stack in
/// `run_turn`, threaded into the model call by reference — the engine holds no
/// receipt state of its own.
///
/// Public so reconstruction tests and inspect tooling can drive receipt
/// emission directly against a message vector without standing up a full turn.
pub struct ReceiptLedger {
    turn_instance: u32,
    call_seq: u64,
    first_seen_step: HashMap<String, usize>,
    effective_budget_tokens: u64,
    calibration_factor: f64,
    /// Memo of the per-block hashing, invalidated whenever the transcript is
    /// rewritten rather than appended to.
    digests: BlockDigestCache,
    /// The revision the next receipt describes; see
    /// [`Self::set_transcript_revision`].
    revision: TranscriptRevision,
    /// Whether `context.lifecycle.enabled` is on for this session. Off by
    /// default and off for every caller that does not opt in, which is the
    /// whole point: with it off the receipt is byte-for-byte what it was
    /// before Phase 2, so the flag's default preserves behavior rather than
    /// merely gating a feature.
    lifecycle_enabled: bool,
}

impl ReceiptLedger {
    /// A ledger for the engine's own step loop — the worker call, `call_seq` 0.
    pub fn new(turn_instance: u32) -> Self {
        ReceiptLedger::with_call_seq(turn_instance, 0)
    }

    /// A ledger for one auxiliary call riding an engine step (the overflow
    /// summarizer, or a pipeline management role). `call_seq` must be non-zero
    /// and unique within the execution, or the receipt overwrites another
    /// call's row — see `AgentEvent::StepManifest::call_seq`.
    pub fn with_call_seq(turn_instance: u32, call_seq: u64) -> Self {
        ReceiptLedger {
            turn_instance,
            call_seq,
            first_seen_step: HashMap::new(),
            effective_budget_tokens: 0,
            calibration_factor: 1.0,
            digests: BlockDigestCache::default(),
            revision: TranscriptRevision::default(),
            lifecycle_enabled: false,
        }
    }

    /// Turn the adaptive-context lifecycle on for this ledger — the session's
    /// `context.lifecycle.enabled` setting, threaded from the CLI through
    /// [`crate::EngineConfig`]. While it is off, no manifest carries a compiled
    /// frame and nothing else about the receipt changes.
    #[must_use]
    pub fn with_lifecycle(mut self, enabled: bool) -> Self {
        self.lifecycle_enabled = enabled;
        self
    }

    /// Record the effective compaction budget and calibration factor the
    /// driver computed for the next step — the values the compaction pass
    /// actually compared against, carried onto the manifest so the receipt's
    /// numbers line up with the decision that was made (#364 item 1).
    pub fn set_effective_budget(&mut self, budget_tokens: u64, factor: f64) {
        self.effective_budget_tokens = budget_tokens;
        self.calibration_factor = factor;
    }

    /// Tell the ledger which revision of the transcript the next receipt
    /// describes, so its block-digest memo can drop keys that no longer name the
    /// bytes they were computed over.
    ///
    /// Pushed per step alongside [`Self::set_effective_budget`] rather than passed
    /// to [`Self::emit_step_receipt`]: that call is reached through
    /// `run_model_call`, which is already at the argument limit. A ledger that is
    /// never told keeps `TranscriptRevision::default()` and therefore memoizes
    /// across rewrites — which is why the driver sets it in the same breath as
    /// the compaction pass that can cause one, and why
    /// `a_rewritten_block_is_never_served_from_the_digest_memo` exists.
    pub fn set_transcript_revision(&mut self, revision: TranscriptRevision) {
        self.revision = revision;
    }

    /// Emit the receipt for one committed step: a `BlockRegistered` for every
    /// block first seen this step, then the ordered `StepManifest`. Called at
    /// the settled boundary where the served model/provider are known.
    /// `estimated_input_tokens` is the conversation estimate the driver already
    /// computed for this same slice and pairs with `StepUsage` (a drift sample).
    /// It is **passed in, not recomputed**: this used to run its own
    /// `estimate_conversation_tokens` over the identical unmutated messages, one
    /// call frame below the driver's, purely so the two events would agree.
    /// Threading the value makes them agree by construction and removes a full
    /// transcript walk from every step. [`Self::emit_step_receipt_estimating`]
    /// is for callers that have no estimate in hand.
    pub fn emit_step_receipt(
        &mut self,
        messages: &[CompletionMessage],
        estimated_input_tokens: u64,
        step: usize,
        served: ServedBy<'_>,
        events: &EventSender,
    ) {
        let ServedBy {
            role,
            provider,
            model,
        } = served;
        self.digests.sync_to(self.revision);
        let drafts = decompose(messages, &mut self.digests);
        let mut blocks = Vec::with_capacity(drafts.len());
        for draft in &drafts {
            let resident_since_step = match self.first_seen_step.get(&draft.block_id) {
                Some(first) => *first,
                None => {
                    self.first_seen_step.insert(draft.block_id.clone(), step);
                    // Durable-before-visible: register the block before the
                    // manifest cites it. Gap kinds carry their local-only bytes
                    // (spec §5.3); journal-resolvable kinds carry only a digest.
                    let _ = events.send(AgentEvent::BlockRegistered {
                        block_id: draft.block_id.clone(),
                        kind: draft.kind,
                        origin: BlockOrigin {
                            turn_instance: self.turn_instance,
                            step,
                            call_id: draft.call_id.clone(),
                            memory_id: draft.memory_id.clone(),
                        },
                        token_cost: draft.token_cost,
                        content_digest: draft.content_digest.clone(),
                        citation_label: draft.citation_label.clone(),
                        // The only place a gap block's bytes are copied, and
                        // only on the step it first appears.
                        content: draft.content.map(str::to_string),
                    });
                    step
                }
            };
            blocks.push(ManifestEntry {
                block_id: draft.block_id.clone(),
                cache_zone: draft.cache_zone,
                token_cost: draft.token_cost,
                resident_since_step,
                message_index: draft.message_index,
                // Per occurrence, not per block: `BlockRegistered` fires once
                // for a content-addressed id, so a repeat of byte-identical
                // tool output would otherwise keep only the first call's id.
                call_id: draft.call_id.clone(),
            });
        }
        // The compiled frame IS this manifest (ADR 0006 as amended), so its
        // identity is computed from the same drafts the entries came from —
        // never from a second walk that could disagree with them.
        let compiled_frame = self.lifecycle_enabled.then(|| {
            frame_identity(&FrameBody {
                schema_version: FRAME_SCHEMA_VERSION,
                turn_instance: self.turn_instance,
                step,
                role,
                blocks: drafts.iter().map(BlockDraft::frame_block).collect(),
            })
        });
        let _ = events.send(AgentEvent::StepManifest {
            turn_instance: self.turn_instance,
            step,
            call_seq: self.call_seq,
            role,
            provider: provider.to_string(),
            model: model.to_string(),
            blocks,
            effective_budget_tokens: self.effective_budget_tokens,
            calibration_factor: self.calibration_factor,
            estimated_input_tokens,
            compiled_frame: compiled_frame.flatten(),
        });
    }

    /// [`Self::emit_step_receipt`] for a caller with no estimate in hand — the
    /// replay/reconstruction paths, which rebuild a receipt from stored messages
    /// rather than from a live step. On the step path the driver always has the
    /// number already; use the other form there so the transcript is walked once.
    pub fn emit_step_receipt_estimating(
        &mut self,
        messages: &[CompletionMessage],
        step: usize,
        served: ServedBy<'_>,
        events: &EventSender,
    ) {
        let estimated_input_tokens = estimate_conversation_tokens(messages);
        // A fresh ledger per call, so its digest memo is empty and the default
        // revision is correct; the step loop sets its real one.
        self.emit_step_receipt(messages, estimated_input_tokens, step, served, events);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::{ToolCall, ToolOutput, ToolResult};
    use tokio::sync::mpsc::unbounded_channel;

    fn worker<'a>(provider: &'a str, model: &'a str) -> ServedBy<'a> {
        ServedBy {
            role: ModelCallRole::Worker,
            provider,
            model,
        }
    }

    fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    fn convo() -> Vec<CompletionMessage> {
        vec![
            CompletionMessage::system("you are a careful engineer"),
            CompletionMessage::user("fix the failing test"),
            CompletionMessage {
                role: MessageRole::Assistant,
                content: String::new(),
                tool_calls: vec![ToolCall {
                    call_id: "c1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "a.rs"}),
                }],
                tool_results: vec![],
                attachments: vec![],
            },
            CompletionMessage {
                role: MessageRole::Tool,
                content: String::new(),
                tool_calls: vec![],
                tool_results: vec![ToolResult {
                    call_id: "c1".into(),
                    output: ToolOutput::Ok {
                        content: "fn a() {}".into(),
                    },
                }],
                attachments: vec![],
            },
        ]
    }

    #[test]
    fn manifest_names_every_block_in_wire_order_with_structural_zones() {
        let (tx, mut rx) = unbounded_channel();
        let events = EventSender::new(tx);
        let mut ledger = ReceiptLedger::new(0);
        ledger.set_effective_budget(136_363, 1.1);
        let messages = convo();
        ledger.emit_step_receipt_estimating(&messages, 0, worker("anthropic", "opus"), &events);

        let evts = drain(&mut rx);
        let manifest = evts
            .iter()
            .find_map(|e| match e {
                AgentEvent::StepManifest { blocks, .. } => Some(blocks.clone()),
                _ => None,
            })
            .expect("a manifest was emitted");
        // system prefix, user goal, one tool_call, one tool_result = 4 blocks.
        assert_eq!(manifest.len(), 4);
        assert_eq!(manifest[0].cache_zone, CacheZone::StablePrefix);
        assert_eq!(manifest[3].cache_zone, CacheZone::Volatile);
        // A BlockRegistered was emitted for each, before the manifest.
        let registered = evts
            .iter()
            .filter(|e| matches!(e, AgentEvent::BlockRegistered { .. }))
            .count();
        assert_eq!(registered, 4);
    }

    /// Two distinct calls whose output is byte-identical collapse onto one
    /// content-addressed `block_id`, so `BlockRegistered` fires once and its
    /// `BlockOrigin` can only name the first call. If the manifest inherited
    /// that provenance, "which calls left context" would silently drop the
    /// duplicate — an eviction of this block would be attributed to `c1` alone.
    /// The manifest records the call per occurrence, so both survive.
    #[test]
    fn identical_output_from_two_calls_keeps_both_call_ids_on_the_manifest() {
        let (tx, mut rx) = unbounded_channel();
        let events = EventSender::new(tx);
        let mut ledger = ReceiptLedger::new(0);
        // Same bytes, different calls — e.g. the agent runs `git status` twice.
        let identical = |call_id: &str| CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![ToolResult {
                call_id: call_id.into(),
                output: ToolOutput::Ok {
                    content: "nothing to commit".into(),
                },
            }],
            attachments: vec![],
        };
        let messages = vec![
            CompletionMessage::system("s"),
            identical("c1"),
            identical("c2"),
        ];
        ledger.emit_step_receipt_estimating(&messages, 0, worker("p", "m"), &events);
        let evts = drain(&mut rx);

        // The content-addressed contract still holds: one registration, one id.
        let registered: Vec<_> = evts
            .iter()
            .filter_map(|e| match e {
                AgentEvent::BlockRegistered {
                    block_id, origin, ..
                } => Some((block_id.clone(), origin.call_id.clone())),
                _ => None,
            })
            .collect();
        let dupes: Vec<_> = registered
            .iter()
            .filter(|(_, call_id)| call_id.is_some())
            .collect();
        assert_eq!(dupes.len(), 1, "byte-identical results register once");
        assert_eq!(
            dupes[0].1.as_deref(),
            Some("c1"),
            "birth provenance is the first call to mint the block"
        );

        let manifest = evts
            .iter()
            .find_map(|e| match e {
                AgentEvent::StepManifest { blocks, .. } => Some(blocks.clone()),
                _ => None,
            })
            .expect("manifest");
        let tool_entries: Vec<_> = manifest.iter().filter(|b| b.call_id.is_some()).collect();
        assert_eq!(
            tool_entries.len(),
            2,
            "both occurrences are on the manifest"
        );
        assert_eq!(
            tool_entries[0].block_id, tool_entries[1].block_id,
            "and they do share the one content-addressed id"
        );
        assert_eq!(
            tool_entries
                .iter()
                .map(|b| b.call_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["c1", "c2"],
            "each occurrence keeps its own call — the join is not lossy"
        );
    }

    #[test]
    fn a_block_carried_across_steps_registers_once_and_ages_its_residency() {
        let (tx, mut rx) = unbounded_channel();
        let events = EventSender::new(tx);
        let mut ledger = ReceiptLedger::new(0);
        let messages = convo();

        ledger.emit_step_receipt_estimating(&messages, 0, worker("p", "m"), &events);
        let _ = drain(&mut rx);
        // Same conversation on the next step: no NEW registrations, and the
        // blocks report resident_since_step == 0 (they arrived on step 0).
        ledger.emit_step_receipt_estimating(&messages, 1, worker("p", "m"), &events);
        let evts = drain(&mut rx);

        let new_registrations = evts
            .iter()
            .filter(|e| matches!(e, AgentEvent::BlockRegistered { .. }))
            .count();
        assert_eq!(new_registrations, 0, "carried blocks re-register 0 times");
        let manifest = evts
            .iter()
            .find_map(|e| match e {
                AgentEvent::StepManifest { blocks, .. } => Some(blocks.clone()),
                _ => None,
            })
            .expect("manifest");
        assert!(
            manifest.iter().all(|b| b.resident_since_step == 0),
            "every block has been resident since step 0"
        );
    }

    // ── The compiled frame (#713 deliverable 4) ─────────────────────────────

    /// The compiled frame on a manifest, or `None` when the lifecycle is off.
    fn frame_of(events: &[AgentEvent]) -> Option<CompiledContextFrameBuilt> {
        events.iter().find_map(|e| match e {
            AgentEvent::StepManifest { compiled_frame, .. } => Some(compiled_frame.clone()),
            _ => None,
        })?
    }

    /// Emit one receipt and return the events, with the lifecycle switch and
    /// the volatile inputs (step, served-by, budget) under the caller's control.
    fn emit(
        lifecycle: bool,
        step: usize,
        served: ServedBy<'_>,
        budget: (u64, f64),
        messages: &[CompletionMessage],
    ) -> Vec<AgentEvent> {
        let (tx, mut rx) = unbounded_channel();
        let events = EventSender::new(tx);
        let mut ledger = ReceiptLedger::new(0).with_lifecycle(lifecycle);
        ledger.set_effective_budget(budget.0, budget.1);
        ledger.emit_step_receipt_estimating(messages, step, served, &events);
        drain(&mut rx)
    }

    #[test]
    fn the_lifecycle_switch_is_off_by_default_and_gates_the_whole_frame() {
        // The default constructor must produce a receipt byte-identical to the
        // pre-Phase-2 one. If this ever fails, "default off" is not true and
        // every existing consumer sees a field it was not promised.
        let off = emit(false, 0, worker("anthropic", "opus"), (100, 1.0), &convo());
        assert_eq!(frame_of(&off), None, "lifecycle off ⇒ no compiled frame");
        // And the manifest is otherwise unchanged: same block count, same order.
        let blocks = off
            .iter()
            .find_map(|e| match e {
                AgentEvent::StepManifest { blocks, .. } => Some(blocks.len()),
                _ => None,
            })
            .expect("a manifest was still emitted");
        assert_eq!(blocks, 4);

        let on = emit(true, 0, worker("anthropic", "opus"), (100, 1.0), &convo());
        assert!(frame_of(&on).is_some(), "lifecycle on ⇒ a compiled frame");
    }

    #[test]
    fn identical_inputs_produce_byte_identical_frames_and_hashes_across_runs() {
        // The gate criterion, stated directly: two independent ledgers, no
        // shared state, same messages.
        let a = frame_of(&emit(true, 2, worker("p", "m"), (99, 1.25), &convo()));
        let b = frame_of(&emit(true, 2, worker("p", "m"), (99, 1.25), &convo()));
        assert_eq!(a, b);
        let a = a.expect("a frame");
        assert!(a.frame_hash.starts_with("sha256:"));
        assert_eq!(a.frame_hash.len(), "sha256:".len() + 64);
        assert_eq!(a.compiled_frame_id, format!("cf_{}", &a.frame_hash[7..31]));
    }

    #[test]
    fn the_volatile_fields_are_excluded_from_the_frame_hash() {
        // Same prompt, different everything the manifest records ABOUT the
        // call: who served it, and what the budget arithmetic produced. A frame
        // hash that moved here would make the determinism gate unmeetable —
        // provider fallback and per-model calibration both change these
        // mid-session, on inputs the user never touched.
        let baseline = frame_of(&emit(true, 1, worker("p", "m"), (100, 1.0), &convo()));
        let served_elsewhere = frame_of(&emit(
            true,
            1,
            worker("other-provider", "other-model"),
            (100, 1.0),
            &convo(),
        ));
        let other_budget = frame_of(&emit(true, 1, worker("p", "m"), (777_777, 2.5), &convo()));
        assert_eq!(baseline, served_elsewhere, "provider/model are volatile");
        assert_eq!(baseline, other_budget, "budget/calibration are volatile");

        // …and `call_seq` likewise: the summarizer's seat is an allocation
        // order within an execution, not a property of the prompt it sent.
        let (tx, mut rx) = unbounded_channel();
        let events = EventSender::new(tx);
        ReceiptLedger::with_call_seq(0, RECEIPT_SEQ_SUMMARIZER)
            .with_lifecycle(true)
            .emit_step_receipt_estimating(&convo(), 1, worker("p", "m"), &events);
        assert_eq!(frame_of(&drain(&mut rx)), baseline, "call_seq is volatile");
    }

    #[test]
    fn residency_does_not_move_the_frame_hash() {
        // Two ledgers reach step 1 with the same messages by different routes:
        // one has carried them since step 0 (resident_since_step 0), the other
        // is seeing them for the first time (resident_since_step 1). The
        // manifests differ; the frames must not. Residency is a fact about the
        // session's history, not about what this prompt contained.
        let (tx, mut rx) = unbounded_channel();
        let events = EventSender::new(tx);
        let mut carried = ReceiptLedger::new(0).with_lifecycle(true);
        carried.emit_step_receipt_estimating(&convo(), 0, worker("p", "m"), &events);
        let _ = drain(&mut rx);
        carried.emit_step_receipt_estimating(&convo(), 1, worker("p", "m"), &events);
        let carried_events = drain(&mut rx);

        let fresh = emit(true, 1, worker("p", "m"), (0, 1.0), &convo());

        // The manifests genuinely disagree — otherwise this test proves nothing.
        let residency = |evts: &[AgentEvent]| {
            evts.iter().find_map(|e| match e {
                AgentEvent::StepManifest { blocks, .. } => Some(
                    blocks
                        .iter()
                        .map(|b| b.resident_since_step)
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
        };
        assert_ne!(residency(&carried_events), residency(&fresh));
        assert_eq!(frame_of(&carried_events), frame_of(&fresh));
    }

    #[test]
    fn a_changed_prompt_changes_the_frame_hash() {
        // The other half of determinism: the hash must actually be a function
        // of the prompt. A hash that never moves is trivially deterministic.
        let baseline = frame_of(&emit(true, 0, worker("p", "m"), (0, 1.0), &convo()));
        let mut edited = convo();
        edited[1].content = "fix the OTHER failing test".into();
        assert_ne!(
            baseline,
            frame_of(&emit(true, 0, worker("p", "m"), (0, 1.0), &edited))
        );
        // As must the step: the same messages at a different step are a
        // different frame, because a frame identifies one model call.
        assert_ne!(
            baseline,
            frame_of(&emit(true, 1, worker("p", "m"), (0, 1.0), &convo()))
        );
    }

    /// The RFC 8785 canonical bytes of a value — the same canonicalizer
    /// `context_record::hash` builds `record_hash` preimages with.
    fn jcs<T: serde::Serialize>(value: &T) -> String {
        serde_json_canonicalizer::to_string(value).expect("canonicalizes")
    }

    #[test]
    fn golden_frame_preimage_and_hash_are_pinned() {
        // The golden vector, in the shape `context_event.rs` set: the exact
        // canonical bytes first (hand-verifiable — keys sorted, absent options
        // gone, whitespace minimal), then the digest they hash to.
        //
        // These two lines are the wire contract for every stored frame hash.
        // Renaming a field, retyping one, adding one, or changing which fields
        // participate breaks exactly one of them — which is the point: a frame
        // hash that silently changes scheme makes every previously stored hash
        // unverifiable, and nothing else in the tree would notice.
        let messages = convo();
        let mut cache = BlockDigestCache::default();
        let drafts = decompose(&messages, &mut cache);
        let body = FrameBody {
            schema_version: FRAME_SCHEMA_VERSION,
            turn_instance: 0,
            step: 0,
            role: ModelCallRole::Worker,
            blocks: drafts.iter().map(BlockDraft::frame_block).collect(),
        };
        assert_eq!(jcs(&body), GOLDEN_FRAME_PREIMAGE);
        let identity = frame_identity(&body).expect("the body canonicalizes");
        assert_eq!(identity.frame_hash, GOLDEN_FRAME_HASH);
        assert_eq!(identity.compiled_frame_id, GOLDEN_FRAME_ID);

        // And the ledger reaches the same identity through the live path, so
        // the golden pins the emitted frame rather than a parallel computation.
        assert_eq!(
            frame_of(&emit(
                true,
                0,
                worker("anthropic", "opus"),
                (136_363, 1.1),
                &convo()
            )),
            Some(identity)
        );
    }

    const GOLDEN_FRAME_PREIMAGE: &str = r#"{"blocks":[{"block_id":"blk_1a2e1ee86e8551d1b069e12d","cache_zone":"stable_prefix","content_digest":"sha256:aadec3e1342a8b1cd71bc46a3796b4b2fc4ab35cb9822527f31f65d1f610ee90","kind":"system_prefix","message_index":0,"token_cost":8},{"block_id":"blk_901ab18c12179eb804a69d40","cache_zone":"cacheable","content_digest":"sha256:75804d696e4c6efcadc89848672f6a9304612fcf6180cedd429e25ac1cfa1bd1","kind":"user_goal","message_index":1,"token_cost":6},{"block_id":"blk_c49dacefa9f909aca42b2880","cache_zone":"cacheable","call_id":"c1","content_digest":"sha256:6c0ec0938d9f77b7ac74006a2ee21268972407d0c98395fc5b9cc6a0fb1a2602","kind":"tool_call","message_index":2,"token_cost":17},{"block_id":"blk_e7032f26bd4843608dfe31d9","cache_zone":"volatile","call_id":"c1","content_digest":"sha256:bfd1986b0e5cdf2925326cf75fa8d40320722e0a3971274a7844b2147ac927e7","kind":"tool_result","message_index":3,"token_cost":9}],"role":"worker","schema_version":"1.0-draft","step":0,"turn_instance":0}"#;
    const GOLDEN_FRAME_HASH: &str =
        "sha256:d022c54960f40c84ad82fae068a87e8b857e917605f0d3b549c6af7047e42ff9";
    const GOLDEN_FRAME_ID: &str = "cf_d022c54960f40c84ad82fae0";

    #[test]
    fn identical_tool_output_resolves_to_the_same_block_id() {
        // Two tool results with byte-identical output share a content-addressed
        // id — the property dedup/supersession identities rely on.
        let a = serde_json::to_string(&ToolOutput::Ok {
            content: "same".into(),
        })
        .unwrap();
        let id1 = block_id(BlockKind::ToolResult, &a);
        let id2 = block_id(BlockKind::ToolResult, &a);
        assert_eq!(id1, id2);
        assert!(id1.starts_with("blk_"));
        assert_eq!(id1.len(), 4 + 24);
    }

    #[test]
    fn streamed_preimage_hashes_exactly_like_the_joined_one() {
        // `block_id` streams `kind_tag`, a NUL, and the content as three
        // `update`s instead of allocating the joined String. Every stored
        // block_id depends on that being the same digest.
        for kind in [
            BlockKind::SystemPrefix,
            BlockKind::ToolResult,
            BlockKind::AssistantText,
        ] {
            for content in ["", "plain", "ünïcödé — 日本語 \u{0}embedded NUL"] {
                let joined = format!("{}\0{}", kind_tag(kind), content);
                assert_eq!(
                    block_id(kind, content),
                    format!("blk_{}", &sha256_hex(&joined)[..24]),
                    "streamed and joined preimages must agree for {kind:?}"
                );
            }
        }
    }
}
