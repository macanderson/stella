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
//! Granularity is what the engine can see in the message vec: the system
//! prefix, each assistant text and tool-call, each tool result by `call_id`,
//! each attachment, and — since Phase 2 (#713) — the assembled recall block
//! split into one `RecalledFrame` block per recalled item, with the `nod_…`
//! record each resolves to. The driver's own User-role injections (the overflow
//! summary, the stuck-loop steer) are classified as `Summary` and `Steered`
//! rather than attributed to the person who did not write them.
//!
//! Per step the receipt also carries the **compiled frame**'s identity — a
//! content-addressed id and a byte-stable hash over what entered the prompt,
//! excluding the accounting around it. ADR 0006 as amended: the compiled frame
//! is this manifest extended, not a parallel aggregate. Gated on
//! `context.lifecycle.enabled`, which ships on.

use std::borrow::Cow;
use std::collections::HashMap;

use serde::Serialize;
use sha2::{Digest, Sha256};
use stella_protocol::{
    AgentEvent, BlockKind, BlockOrigin, CacheZone, CompiledContextFrameBuilt, CompletionMessage,
    ManifestEntry, MessageRole, ModelCallRole, ToolOutput,
};

use crate::estimator::estimate_conversation_tokens;
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

/// Whether a block kind's preimage lives in the event journal (resolved at
/// reconstruction time) or must be captured locally at emission. Tool I/O and
/// assistant text ride the journal (`ToolStart`/`ToolResult`/`Text`); the
/// system prefix and the assembled user/recall/steer/summary messages do not,
/// so their bytes are carried as local-only block content (spec §5.3).
///
/// [`BlockKind::Attachment`] joined this list in Phase 2 (#713), and what it
/// carries is worth being precise about: an attachment at rest is
/// `AttachmentSource::Path` — a name, a MIME type, a byte count, and a
/// workspace path — and that metadata is the preimage. The base64 payload
/// exists only after the model layer hydrates a request, which happens below
/// this point and never on the messages a receipt sees. A hydrated attachment
/// that somehow reached here is stripped by
/// [`BlockDraft::without_local_content`] rather than journaled.
fn is_gap_kind(kind: BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::SystemPrefix
            | BlockKind::UserGoal
            | BlockKind::RecalledFrame
            | BlockKind::Steered
            | BlockKind::Summary
            | BlockKind::Attachment
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
            // The shared rule, called — this module used to carry its own copy
            // that counted `chars()` where the conversation estimator counts
            // bytes, so one receipt held two numbers for the same content that
            // reconciled only for ASCII (#925). The fix is not a corrected
            // copy; it is that there is no copy, so `token_cost` and
            // `estimated_input_tokens` cannot drift again without moving the
            // one function they both reach through.
            token_cost: stella_protocol::estimate_token_cost(&content),
        };
        self.entries.insert(key, computed.clone());
        computed
    }
}

/// Why a block is in the prompt, as recorded on the frame preimage.
///
/// A deliberately small, closed vocabulary of `&'static str` rather than an
/// enum: it participates in the frame hash, so its wire spelling is the
/// contract and a Rust-level rename that did not change the string would be
/// invisible where it matters. The retrieval plane has its own, finer
/// `SelectionReason` for why a *candidate* won ranking
/// (`stella_context::SelectionReason`); this one answers the coarser question a
/// receipt asks — which mechanism put these bytes in front of the model.
///
/// Only recall carries a reason today. Structural blocks — the system prefix,
/// the goal, tool I/O — have none, because "it is structural" is not a
/// selection decision anybody made and recording it would be noise in every
/// frame body.
pub const SELECTION_RECALLED: &str = "recalled";

/// Marker prefixing the volatile recalled-context message the CLI assembles
/// and lands at the conversation tail. Lives here, next to the decomposition
/// that reads it, because the engine cannot recognize a recall block without
/// it — and `stella-cli` re-exports this rather than keeping a second copy,
/// since two spellings of one marker is a decomposition that silently stops
/// firing the day one of them is edited.
pub const RECALL_MARKER: &str = "[auto-recalled context]";

/// Which block kind a `User`-role message actually is.
///
/// Not every `User` message is the user's goal. The driver injects three of its
/// own — the overflow summary, the stuck-loop steer and the output-limit
/// continuation nudge, all `CompletionMessage::user` — and the CLI injects the
/// recalled-context block. Before Phase 2 they all collapsed onto
/// [`BlockKind::UserGoal`], so a receipt attributed engine-generated text to the
/// person. The four markers are prefixes on content the engine and CLI both
/// write, which is why this is a prefix match and not a heuristic.
///
/// The continuation nudge files as [`BlockKind::Steered`] rather than earning a
/// kind of its own: it is a mid-turn injected instruction that redirects the
/// model, which is exactly what that kind means, and reusing it keeps the wire
/// enum (and every reader of it) unchanged.
fn user_block_kind(content: &str) -> BlockKind {
    if content.starts_with(crate::driver::SUMMARY_MARKER_PREFIX) {
        BlockKind::Summary
    } else if content.starts_with(crate::driver::LOOP_STEER_PREFIX)
        || content.starts_with(crate::driver::CONTINUATION_MARKER_PREFIX)
    {
        BlockKind::Steered
    } else if content.starts_with(RECALL_MARKER) {
        BlockKind::RecalledFrame
    } else {
        BlockKind::UserGoal
    }
}

/// One piece of a decomposed recall block: a contiguous slice of the assembled
/// message, plus whatever provenance its first line declares.
struct RecallSegment<'a> {
    text: &'a str,
    memory_id: Option<String>,
    citation_label: Option<String>,
}

/// Split the assembled recall message into per-item segments.
///
/// **The segments concatenate back to the input byte for byte**, and that is
/// the load-bearing property, not the split itself: reconstruction rebuilds the
/// message by appending each block's preimage in manifest order, so a split
/// that lost or duplicated a separator would break byte-exact reconstruction —
/// the one signal this whole plane exists to provide.
///
/// The cut is the start of every line beginning with `"- "`, which is how the
/// CLI renders one recalled frame. Everything before the first such line (the
/// marker and the section header) is its own leading segment. A frame whose
/// *body* happens to contain a line starting with `"- "` splits into two
/// segments: the concatenation still holds, and the second segment simply
/// parses no label. That is the right failure — a mis-split is a coarser
/// receipt, never a wrong reconstruction.
fn split_recall(content: &str) -> Vec<RecallSegment<'_>> {
    let mut cuts: Vec<usize> = vec![0];
    let mut at = 0usize;
    for line in content.split_inclusive('\n') {
        if at > 0 && line.starts_with("- ") {
            cuts.push(at);
        }
        at += line.len();
    }
    cuts.push(content.len());
    cuts.dedup();
    cuts.windows(2)
        .filter(|w| w[0] < w[1])
        .map(|w| {
            let text = &content[w[0]..w[1]];
            let (memory_id, citation_label) = parse_recall_item(text);
            RecallSegment {
                text,
                memory_id,
                citation_label,
            }
        })
        .collect()
}

/// The `nod_…` id and human label a rendered recall line declares.
///
/// The CLI renders a memory as `- [nod_…] <label> — <body>` and every other
/// frame kind as `- <label> — <body>`. Both are parsed; anything else yields
/// `(None, None)` rather than a guess, because a wrong `memory_id` is worse
/// than an absent one — it is the join key the write→citation loop reads, so a
/// mis-parse would attribute a turn's use to the wrong record.
fn parse_recall_item(segment: &str) -> (Option<String>, Option<String>) {
    let Some(first) = segment.lines().next() else {
        return (None, None);
    };
    let Some(rest) = first.strip_prefix("- ") else {
        return (None, None);
    };
    let (memory_id, rest) = match rest.strip_prefix('[').and_then(|r| r.split_once(']')) {
        Some((id, after)) if id.starts_with("nod_") => {
            (Some(id.to_string()), after.trim_start().to_string())
        }
        _ => (None, rest.to_string()),
    };
    // The em-dash separator the renderer writes between label and body. A line
    // without one is all label — the shape a frame with empty content takes.
    let label = rest
        .split_once(" — ")
        .map_or(rest.as_str(), |(l, _)| l)
        .trim();
    (memory_id, (!label.is_empty()).then(|| label.to_string()))
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
    content: Option<Cow<'a, str>>,
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
/// Both of the gaps this function used to name are closed as of Phase 2
/// (#713), and closing them moved block ids — a block's id is
/// `sha256(kind_tag \0 content)`, so a message that used to hash as
/// `user_goal` and now hashes as `summary` is a different id. That is a
/// one-way fidelity change, accepted deliberately: the ids that moved were
/// wrong, and a receipt that attributes the engine's own compaction notice to
/// the person is not a receipt anyone should be asked to keep for stability's
/// sake. Stored ids from before the change remain valid for the receipts that
/// cite them; nothing rewrites history.
///
/// - **`User` messages are classified, not assumed** ([`user_block_kind`]).
///   The overflow summary and the stuck-loop steer are the driver's own text
///   and now land on [`BlockKind::Summary`] and [`BlockKind::Steered`]; the
///   assembled recall block lands on [`BlockKind::RecalledFrame`], **split per
///   item** ([`split_recall`]) so a receipt can say *what* was recalled rather
///   than only that something was.
/// - **Attachments are decomposed.** Each yields an [`BlockKind::Attachment`]
///   block over its metadata, so a multimodal turn's summed manifest
///   `token_cost` stops reading materially below the same event's
///   `estimated_input_tokens`.
///
/// What did NOT move is cache zones (spec §5.1): the head is still the system
/// prefix and the tail is still the last block, so the prefix a provider caches
/// is byte-identical before and after this change. Splitting only ever adds
/// blocks *within* the volatile region.
///
/// Nor did the hashing budget move: every block a message yields — including
/// each recall segment and each attachment — is minted through the one `push`
/// below and is therefore memoized in `cache` under `(message_index, ordinal)`.
/// Splitting a message into more blocks makes the memo finer, never absent, so
/// a decomposed recall block is still hashed once per turn rather than once per
/// step.
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
                        local: Option<Cow<'a, str>>,
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
                content: is_gap_kind(kind).then_some(local).flatten(),
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
                    Some(Cow::Borrowed(message.content.as_str())),
                    None,
                );
            }
            MessageRole::User => match user_block_kind(&message.content) {
                // The recalled frames live inside this message, one per
                // segment. Each is pushed like any other block — same memo,
                // same `(message_index, ordinal)` key — and only then gains the
                // provenance that is the segment's alone.
                BlockKind::RecalledFrame => {
                    for segment in split_recall(&message.content) {
                        push(
                            &mut drafts,
                            cache,
                            BlockKind::RecalledFrame,
                            &|| Cow::Borrowed(segment.text),
                            Some(Cow::Borrowed(segment.text)),
                            None,
                        );
                        if let Some(draft) = drafts.last_mut() {
                            draft.memory_id = segment.memory_id;
                            draft.citation_label = segment.citation_label;
                            draft.selection_reason = Some(SELECTION_RECALLED.to_string());
                        }
                    }
                }
                kind => {
                    push(
                        &mut drafts,
                        cache,
                        kind,
                        &|| Cow::Borrowed(message.content.as_str()),
                        Some(Cow::Borrowed(message.content.as_str())),
                        None,
                    );
                }
            },
            MessageRole::Assistant => {
                if !message.content.is_empty() {
                    push(
                        &mut drafts,
                        cache,
                        BlockKind::AssistantText,
                        &|| Cow::Borrowed(message.content.as_str()),
                        Some(Cow::Borrowed(message.content.as_str())),
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
        // Attachments ride any role, so they are decomposed after the role
        // match rather than inside it. The preimage is the attachment's own
        // JSON — a name, a MIME type, a byte count, and (at rest) a path — so
        // the block is both content-addressed and reconstructable. A hydrated
        // attachment carries inline base64 and must not be journaled: its
        // identity still hashes the real value, but the local preimage is
        // dropped so the bytes never reach the event stream.
        for attachment in &message.attachments {
            let content = serde_json::to_string(attachment).unwrap_or_default();
            // A hydrated attachment's preimage is dropped rather than
            // journaled, so its inline base64 never reaches the event stream.
            // The identity still hashes the real value either way.
            let local = match attachment.source {
                stella_protocol::AttachmentSource::Data { .. } => None,
                stella_protocol::AttachmentSource::Path { .. } => Some(Cow::Owned(content.clone())),
            };
            push(
                &mut drafts,
                cache,
                BlockKind::Attachment,
                &|| Cow::Owned(content.clone()),
                local,
                None,
            );
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
///
/// `1.1` (#925) is the first bump, and it exercises the constant slightly
/// beyond its original wording: the participating *fields* did not change, the
/// **unit of one of them** did. A frame block's `token_cost` used to be a
/// character count and is now the shared byte-based rule
/// ([`stella_protocol::tokens`]), so the same prompt hashes differently before
/// and after — which is exactly the condition the version string exists to keep
/// out of one digest space. Stored `1.0-draft` hashes remain valid artifacts of
/// the scheme that minted them; no reader has to branch on the version to know
/// which rule a hash came from, because the version is inside the preimage.
pub const FRAME_SCHEMA_VERSION: &str = "1.1";

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
    /// A ledger for the engine's own step loop — the worker call,
    /// [`RECEIPT_SEQ_WORKER`].
    pub fn new(turn_instance: u32) -> Self {
        ReceiptLedger::with_call_seq(turn_instance, RECEIPT_SEQ_WORKER)
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
                        content: draft.content.as_deref().map(str::to_string),
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
        // Replay callers construct a fresh ledger per call, so the digest memo
        // starts empty and the default revision is correct for them; the step
        // loop instead sets its real revision via `set_transcript_revision`.
        self.emit_step_receipt(messages, estimated_input_tokens, step, served, events);
    }
}

#[cfg(test)]
mod tests;
