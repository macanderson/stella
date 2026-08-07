//! Context compaction — pure synchronous logic over owned data
//! Five mechanisms: an age-based retention pass that runs on every step, and
//! four budget passes applied least-lossy first once the conversation
//! outgrows its budget:
//!
//! 0. **Tool-result retention** (#1285): tool results older than a horizon of
//!    tool-bearing steps are middle-out aged *regardless of the budget*. The
//!    budget passes below fire only near the context ceiling — 96k–150k
//!    tokens — which on a long trial is the very end of exactly the runs they
//!    should have been shaping from the middle (the measured cost: 4× more
//!    input per step than a comparator that holds its standing context flat).
//!    An old tool output has usually been consumed within a few steps; past
//!    the horizon its head and tail carry the framing and the errors, and the
//!    stub says how to get the rest back. Aging fires only once at least
//!    `RETENTION_MIN_RECLAIM_CHARS` are reclaimable, so the prompt-cache
//!    prefix is mutated only when the rewrite buys real bytes, not once per
//!    step (the same discipline as the budget hysteresis below — invariant
//!    7, #372).
//!
//! The budget passes:
//!
//! 1. **Dedup of repeated identical tool outputs** (L-E3): a byte-identical
//!    tool output appearing more than once keeps only its earliest copy; the
//!    later ones are stubbed with a pointer. Keeping the EARLIEST copy is
//!    deliberate: byte-identical content is position-independent, so the
//!    stub lands in the newest part of the conversation and the provider
//!    prompt-cache prefix stays byte-identical (#372). Supersession below is
//!    the opposite — it is about staleness, so it keeps the latest.
//! 2. **Supersession**: when the SAME call (same tool name, byte-identical
//!    input) ran more than once, only the latest result reflects current
//!    state — the older ones are stale by construction (a re-read after an
//!    edit, a re-listed directory) and are stubbed even though their
//!    content differs.
//! 3. **Aging**: still over budget, old large outputs are middle-out
//!    truncated to head+tail before anything is dropped whole — error
//!    lines and file headers survive where full eviction would lose them.
//! 4. **Tool-output eviction**: oldest large tool outputs are replaced with
//!    a stub once the conversation still exceeds the budget. A tool result
//!    whose call is still the most recent one is never evicted (the test
//!    below: compaction never drops a still-referenced tool result).
//!
//! The system message and the latest user message are never touched.

use stella_protocol::{CompactionRewrite, CompletionMessage, MessageRole, ToolOutput};

use crate::estimator::{estimate_conversation_tokens, estimate_message_tokens};
use crate::receipts::{tool_result_block_id, tool_result_rewrite};

/// What a compaction pass did, for the `Compaction` event. Carries both the
/// counts (back-compat) and the **identities** — the `block_id`s each pass
/// stubbed — so the receipt records *which* blocks left context, not just how
/// many (spec §6.2). Each `*_blocks` vec's length equals its count.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompactionReport {
    pub before_tokens: u64,
    pub after_tokens: u64,
    pub evicted: usize,
    pub deduped: usize,
    /// Older results of a repeated identical call, stubbed as stale.
    pub superseded: usize,
    /// Large old outputs middle-out truncated instead of dropped whole.
    pub aged: usize,
    /// `block_id`s evicted (pass 4), deduped (pass 1), superseded (pass 2), and
    /// aged (pass 3) — the content-addressed identity of each stubbed result,
    /// matching the id the manifest cited for it.
    pub evicted_blocks: Vec<String>,
    pub deduped_blocks: Vec<String>,
    pub superseded_blocks: Vec<String>,
    pub aged_blocks: Vec<String>,
    /// The replacement bytes each in-place rewrite left behind (#1667) — the
    /// post-rewrite identity, digest, and preimage of every block this call
    /// stubbed or aged, deduplicated by digest. Journaling these beside the
    /// identities above is what lets reconstruction resolve a compacted block
    /// to the bytes the model actually received.
    pub rewrites: Vec<CompactionRewrite>,
}

const EVICTION_STUB: &str =
    "[tool output evicted to fit context — re-run the tool if you need it again]";

/// Aging only touches outputs big enough that head+tail plus the marker is
/// a real saving; below this it would churn bytes for nothing. Counted in
/// UTF-8 bytes (`str::len`), like every other size floor in this module —
/// [`age_content`] then walks the cut back to a char boundary.
const AGE_THRESHOLD_CHARS: usize = 2_000;
/// What aging keeps from each end, in UTF-8 bytes. Head carries the tool's
/// framing (the PASSED/FAILED line, file headers); tail carries the errors.
const AGE_KEEP_CHARS: usize = 800;

/// What pass 1 writes over a later byte-identical copy. Models can't see
/// message indices — point at the surviving copy in terms they can act on.
/// A `const` rather than a constructor because [`is_compacted_output`] compares
/// against it once per tool result per step, and the driver re-scans the whole
/// transcript every step (#554) — building the String to throw it away was
/// per-step garbage proportional to the history.
const DEDUP_STUB: &str =
    "[identical output repeated — the full content already appears in an earlier tool result]";

/// What pass 2 writes over a stale result of a re-run call. `const` for the
/// same reason as [`DEDUP_STUB`].
const SUPERSESSION_STUB: &str = "[stale result of a repeated call — the same tool ran again with identical input; the \
     current output appears in a more recent tool result]";

/// The marker [`age_content`] splices between the head and tail it keeps.
/// Named so [`is_compacted_output`] can recognize an aged payload without
/// duplicating the string.
const AGE_ELISION_MARKER: &str =
    "[… middle elided during compaction — re-run the tool for the full output …]";

/// Whether this output is something a compaction pass wrote over a real
/// tool result, rather than the result itself. The driver uses it to keep
/// a *pre-compaction* identity for every tool result (#554): compaction
/// rewrites results in place, so a snapshot taken after a pass would
/// record the stub's identity and permanently lose the evidence that loop
/// detection compares. Keeping the predicate here keeps the stub strings
/// in the one module that writes them.
///
/// Deliberately conservative in the one direction that can be wrong: real
/// tool output that happens to contain [`AGE_ELISION_MARKER`] (reading
/// this file, say) is treated as compacted, which only means the driver
/// declines to snapshot an identity for it and falls back to comparing
/// outputs — the behavior before #554, never a false loop.
pub(crate) fn is_compacted_output(output: &ToolOutput) -> bool {
    let payload = match output {
        ToolOutput::Ok { content } => content,
        ToolOutput::Error { message } => message,
    };
    payload == EVICTION_STUB
        || payload == DEDUP_STUB
        || payload == SUPERSESSION_STUB
        || payload.contains(AGE_ELISION_MARKER)
}

/// Middle-out truncate `content` on char boundaries, keeping `head` bytes from
/// the start and `tail` bytes from the end with `marker` spliced between.
/// Caller guarantees the two keep windows do not overlap.
///
/// The windows are separate parameters rather than one symmetric size because
/// the two callers want opposite shapes. Aged tool output keeps both ends
/// evenly — the head carries the runner's framing (the PASSED/FAILED line, file
/// headers) and the tail carries the errors. A truncated assistant partial has
/// no framing worth keeping and exactly one thing that matters: where the model
/// was when it was cut off, which is the very end.
fn elide_middle(content: &str, head: usize, tail: usize, marker: &str) -> String {
    let mut head_end = head.min(content.len());
    while !content.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = content.len() - tail.min(content.len());
    while !content.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!(
        "{}\n{marker}\n{}",
        &content[..head_end],
        &content[tail_start..]
    )
}

/// Middle-out truncate `content` on char boundaries, keeping
/// [`AGE_KEEP_CHARS`] from each end. Caller guarantees
/// `content.len() > AGE_THRESHOLD_CHARS`, which the keep windows never
/// overlap.
fn age_content(content: &str) -> String {
    elide_middle(content, AGE_KEEP_CHARS, AGE_KEEP_CHARS, AGE_ELISION_MARKER)
}

/// What [`elide_truncated_partial`] splices in place of the middle it drops.
/// Addressed to the model, because the model is who reads it: it says what is
/// missing and that the missing part is not worth asking for.
const PARTIAL_ELISION_MARKER: &str = "[… middle of this cut-off message elided — it was working-out you were already \
     told not to repeat; resume from where it stops below …]";

/// Bytes of a truncated assistant partial kept from the start — enough to
/// orient ("what was I doing"), no more. Deliberately small: unlike tool
/// output, the head of a cut-off message carries no framing a reader needs.
const PARTIAL_KEEP_HEAD: usize = 200;

/// Bytes of a truncated assistant partial kept from the end. This is the whole
/// point of retaining the partial at all — the cut-off sentence the
/// continuation resumes from — so it gets the large share of the budget.
const PARTIAL_KEEP_TAIL: usize = 1_200;

/// Shrink a length-truncated assistant partial to its orientation and its
/// resume point, or `None` when it is small enough to keep verbatim.
///
/// The driver retains such a partial so the model can continue from where it
/// stopped ([`crate::driver`]'s continuation path). Retained whole, it is the
/// most expensive content in the transcript: up to a full `max_output_tokens`
/// of it, and — unlike a tool result — assistant text is protected content that
/// no pass in this module may touch, so it rides every remaining step of the
/// turn.
///
/// Eliding the middle is cheaper *and* a better prompt, which is why it is
/// worth doing rather than merely affordable. The continuation nudge tells the
/// model to resume from exactly where it stopped and not to restate its
/// reasoning; whole, those two referents are the last handful of tokens of the
/// block and the ~16k before them, so the instruction points at something
/// buried under the noise it is telling the model to ignore. Elided, the resume
/// point sits immediately above the instruction that names it.
///
/// `None` below [`AGE_THRESHOLD_CHARS`] is the guard that keeps this honest: a
/// short answer genuinely cut mid-sentence is never touched, and a long one
/// keeps its end, which is where mid-sentence lives.
pub(crate) fn elide_truncated_partial(content: &str) -> Option<String> {
    (content.len() > AGE_THRESHOLD_CHARS).then(|| {
        elide_middle(
            content,
            PARTIAL_KEEP_HEAD,
            PARTIAL_KEEP_TAIL,
            PARTIAL_ELISION_MARKER,
        )
    })
}

/// How many reclaimable bytes must accumulate past the horizon before the
/// retention pass rewrites anything.
///
/// Every firing is a prompt-cache trade: it mutates the prefix at horizon
/// depth, so everything behind that point re-writes at the cache-write rate
/// on the next call. The original gate counted *results* (four ageable ones),
/// which measured the wrong side of the trade both ways — four
/// barely-over-threshold results fired a prefix rewrite to reclaim under
/// 2 KB, while three 100 KB outputs could never fire at all and were re-sent
/// verbatim on every remaining call of the turn. The gate now measures what
/// the rewrite actually buys: the bytes the batch would remove. 12 KB
/// (~3.4k estimated tokens) keeps the #1285 shape firing — a long turn of
/// ~5 KB reads ages after four accumulate, as its step-loop witness pins —
/// while a trickle of barely-over-threshold results reclaiming a few
/// hundred bytes each no longer buys a rewrite at all. Between firings the
/// whole prefix is byte-stable (the same discipline as the budget
/// hysteresis — invariant 7).
const RETENTION_MIN_RECLAIM_CHARS: usize = 12_000;

/// The bytes one aged payload retains: both kept ends plus the elision
/// marker. What aging reclaims from a payload is its length minus this.
const AGE_RETAINED_CHARS: usize = 2 * AGE_KEEP_CHARS + AGE_ELISION_MARKER.len();

/// Age-based tool-result retention (pass 0, #1285): how many of the most
/// recent tool-bearing steps keep their results verbatim.
///
/// Results in older Tool messages are middle-out aged (`age_content`) once
/// at least `RETENTION_MIN_RECLAIM_CHARS` of them are reclaimable —
/// independent of the conversation's total size, which is what distinguishes
/// this from the budget passes: they fire near the context ceiling, this
/// shapes the standing context from the middle of a long turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Tool messages within this distance of the newest one are never touched
    /// (the newest itself is always protected, whatever this says — the
    /// most-recent-result invariant belongs to every pass).
    pub keep_recent_steps: usize,
}

/// Pass 0: age every large tool result older than the policy's horizon,
/// gated on [`RETENTION_MIN_RECLAIM_CHARS`]. Returns `(aged, aged_blocks,
/// rewrites, tokens_saved)`; block ids are captured before mutation so the
/// report cites the identity the previous step's manifest recorded (§6.2),
/// and each rewrite's replacement record is captured right after it (#1667) —
/// this pass runs before `compact_measured` snapshots `original_ids`, so its
/// rewrites cannot be recovered by the post-pass identity walk. Token savings
/// are measured per mutated message ([`estimate_message_tokens`] diffs),
/// never by a whole-transcript walk — this runs on every step, including the
/// common under-budget one, and Θ(transcript) work there is the cost class
/// `compute_passes` pins against.
fn age_stale_tool_results(
    messages: &mut [CompletionMessage],
    policy: RetentionPolicy,
) -> (usize, Vec<String>, Vec<CompactionRewrite>, u64) {
    let tool_positions: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == MessageRole::Tool)
        .map(|(idx, _)| idx)
        .collect();
    // The horizon counts tool-bearing steps from the end; everything at or
    // beyond `keep_recent_steps` distance is stale. The newest Tool message
    // is excluded unconditionally (`max(1)`) so a zero horizon can never
    // touch the results answering the latest call.
    let keep = policy.keep_recent_steps.max(1);
    if tool_positions.len() <= keep {
        return (0, Vec::new(), Vec::new(), 0);
    }
    let stale = &tool_positions[..tool_positions.len() - keep];
    let reclaimable: usize = stale
        .iter()
        .map(|&idx| {
            messages[idx]
                .tool_results
                .iter()
                .map(|result| {
                    let payload = match &result.output {
                        ToolOutput::Ok { content } => content,
                        ToolOutput::Error { message } => message,
                    };
                    if payload.len() > AGE_THRESHOLD_CHARS {
                        payload.len().saturating_sub(AGE_RETAINED_CHARS)
                    } else {
                        0
                    }
                })
                .sum::<usize>()
        })
        .sum();
    if reclaimable < RETENTION_MIN_RECLAIM_CHARS {
        return (0, Vec::new(), Vec::new(), 0);
    }
    let mut aged = 0usize;
    let mut aged_blocks = Vec::new();
    let mut rewrites = Vec::new();
    let mut saved = 0u64;
    for &idx in stale {
        let message = &mut messages[idx];
        let before = estimate_message_tokens(message);
        let mut touched = false;
        for result in message.tool_results.iter_mut() {
            let (payload, is_error) = match &result.output {
                ToolOutput::Ok { content } => (content, false),
                ToolOutput::Error { message } => (message, true),
            };
            if payload.len() > AGE_THRESHOLD_CHARS {
                aged_blocks.push(tool_result_block_id(&result.output));
                let aged_payload = age_content(payload);
                result.output = if is_error {
                    ToolOutput::Error {
                        message: aged_payload,
                    }
                } else {
                    ToolOutput::Ok {
                        content: aged_payload,
                    }
                };
                rewrites.push(tool_result_rewrite(&result.output));
                aged += 1;
                touched = true;
            }
        }
        if touched {
            saved += before.saturating_sub(estimate_message_tokens(message));
        }
    }
    (aged, aged_blocks, rewrites, saved)
}

/// Evict + dedup until the conversation fits `budget_tokens` — reclaiming
/// down to a low watermark an eighth below it (see `compact_measured`'s
/// hysteresis note) — or until nothing more can be safely removed. Returns
/// `None` if no compaction was needed (already under budget) — or if the
/// pass changed nothing (all remaining content is protected), so a
/// permanently-over-budget conversation doesn't emit a no-op `Compaction`
/// event before every step.
///
/// No retention pass: this form serves callers with no step horizon.
/// Prefer [`compact_measured`] on the step path: this form throws away the
/// post-pass token count, which the caller then has to walk the whole
/// transcript again to recover.
pub fn compact(messages: &mut [CompletionMessage], budget_tokens: u64) -> Option<CompactionReport> {
    compact_measured(messages, budget_tokens, None).1
}

/// [`compact`], plus the conversation's token count **after** the pass.
///
/// Every `None` path here already knows that number — the under-budget return
/// compares against it, and the nothing-compactable return re-scanned for
/// eviction — so returning it costs nothing. The caller needs it for the
/// overflow-summarizer decision, and without it `run_compaction_pass` ran a
/// second full `estimate_conversation_tokens` over the whole transcript on
/// every step that did NOT compact, i.e. the common case. Worse, that walk was
/// eager: it also ran when `summarize_overflow` was off and the value was
/// never read.
///
/// The returned count is exact, not an estimate of an estimate: it is the same
/// `estimate_conversation_tokens` value the discarded walk would have produced
/// over the same (already-mutated) slice.
pub fn compact_measured(
    messages: &mut [CompletionMessage],
    budget_tokens: u64,
    retention: Option<RetentionPolicy>,
) -> (u64, Option<CompactionReport>) {
    let before_tokens = estimate_conversation_tokens(messages);
    // Pass 0: age-based retention, before — and independent of — the budget
    // comparison. Its savings feed the comparison, so a retention pass that
    // shrinks the transcript under budget also spares it the budget passes'
    // deeper rewrites this step.
    let (retention_aged, retention_aged_blocks, retention_rewrites, retention_saved) =
        match retention {
            Some(policy) => age_stale_tool_results(messages, policy),
            None => (0, Vec::new(), Vec::new(), 0),
        };
    let current_tokens = before_tokens.saturating_sub(retention_saved);
    if current_tokens <= budget_tokens {
        if retention_aged == 0 {
            return (current_tokens, None);
        }
        return (
            current_tokens,
            Some(CompactionReport {
                before_tokens,
                after_tokens: current_tokens,
                aged: retention_aged,
                aged_blocks: retention_aged_blocks,
                rewrites: dedup_rewrites(retention_rewrites),
                ..CompactionReport::default()
            }),
        );
    }
    // Hysteresis: the pass TRIGGERS at the budget but passes 3/4 reclaim down
    // to this low watermark. Stopping exactly at the budget meant a saturated
    // long turn re-crossed it on every step's few thousand new tokens, and
    // each re-triggered pass rewrote the next-oldest tool result — a fresh
    // prefix mutation deep in the transcript on EVERY step, invalidating the
    // provider prompt cache for everything after it (invariant 7: cache hits
    // are a feature; #372's whole point). An eighth of headroom absorbs
    // several steps of growth per mutation instead of one. A tiny budget
    // (under 8 tokens) has no headroom eighth and degrades gracefully to the
    // old stop-at-budget behavior.
    let target_tokens = budget_tokens - budget_tokens / 8;

    let mut deduped = 0usize;
    let mut superseded = 0usize;
    // Seeded with pass 0's work: retention aging and budget aging are one
    // mechanism with two triggers, and the report folds them the same way.
    let mut aged = retention_aged;
    let mut evicted = 0usize;
    // Identities alongside the counts — the block_id each pass stubbed (§6.2).
    let mut deduped_blocks: Vec<String> = Vec::new();
    let mut superseded_blocks: Vec<String> = Vec::new();
    let mut aged_blocks: Vec<String> = retention_aged_blocks;
    let mut evicted_blocks: Vec<String> = Vec::new();
    // Pass 0's replacement records, captured at its mutation sites; the budget
    // passes' records are recovered by the identity walk after pass 4 (#1667).
    let mut rewrites: Vec<CompactionRewrite> = retention_rewrites;
    // Each tool result's ORIGINAL block_id, captured before any pass mutates
    // it, indexed by POSITION: `original_ids[message_idx][result_idx]`. A
    // result aged then evicted in the same call must be recorded under the id
    // the manifest cited, not the id of its intermediate aged content, which is
    // what capturing up front preserves.
    //
    // Deliberately NOT keyed by `call_id`. A call_id is unique only within ONE
    // step — `driver/loop_evidence.rs::snapshot_result_identities` says so and poisons any id
    // it sees carrying two different outputs — and Gemini/Vertex mint theirs as
    // `call_{ordinal}` counted per response, so `call_0` recurs on every
    // assistant step. A `HashMap<call_id, block_id>` keeps only the LAST
    // occurrence, which collapsed every result sharing a recurring id onto one
    // identity: pass 1 then read three distinct outputs as duplicates of each
    // other and stubbed the middle ones, and the receipt cited the wrong block.
    let original_ids: Vec<Vec<String>> = messages
        .iter()
        .map(|m| {
            m.tool_results
                .iter()
                .map(|r| tool_result_block_id(&r.output))
                .collect()
        })
        .collect();
    let id_at = |message_idx: usize, result_idx: usize| -> String {
        original_ids
            .get(message_idx)
            .and_then(|ids| ids.get(result_idx))
            .cloned()
            .unwrap_or_default()
    };

    // Index of the last Tool message — its results answer the most recent
    // assistant tool calls and must never be evicted or deduped away.
    let last_tool_idx = messages.iter().rposition(|m| m.role == MessageRole::Tool);

    // Pass 1: dedup byte-identical Ok outputs (keep the EARLIEST copy).
    // Byte-identical content is position-independent, so keeping the first
    // occurrence and stubbing later ones frees the same tokens while leaving
    // the prompt prefix — and the provider prompt cache built over it —
    // untouched (#372). Walk forward, recording first occurrences.
    {
        // Keyed on the content-addressed block id `original_ids` already
        // computed above, not on a clone of the content: the id IS the
        // content identity (receipts.rs hashes the serialized output), so
        // byte-identical outputs still collide exactly — and only those do —
        // while the map stops paying a full heap copy of every >200-byte
        // output plus a second full hash pass over it on lookup. The id is
        // read by POSITION, never through the result's call_id: two results
        // that merely share a recurring call_id are different content and
        // must keep different identities here, or dedup destroys one of them.
        // Keyed to the (message, result) POSITION of the earliest copy, not
        // the message index alone: the driver records a whole step's results
        // in ONE Tool message, so an index-only key made two byte-identical
        // outputs from the same step (two reads of the same file in one
        // parallel batch) structurally un-dedupable — `kept_at < idx` was
        // false for same-message duplicates.
        let mut seen: std::collections::HashMap<String, (usize, usize)> =
            std::collections::HashMap::new();
        // First record positions of the earliest occurrence.
        for (idx, message) in messages.iter().enumerate() {
            if message.role != MessageRole::Tool {
                continue;
            }
            for (ridx, result) in message.tool_results.iter().enumerate() {
                if let ToolOutput::Ok { content } = &result.output
                    && content.len() > 200
                {
                    // `id_at` yields "" only for an out-of-range position; an
                    // unidentifiable result is skipped rather than made to
                    // collide with every other unidentifiable one.
                    let id = id_at(idx, ridx);
                    if !id.is_empty() {
                        seen.entry(id).or_insert((idx, ridx));
                    }
                }
            }
        }
        // Then stub every later duplicate. The `> 200` guard stays on the
        // content, matching the recording pass: the id is finer-grained
        // (it also covers the Ok/Error tag), never a stand-in for length.
        for (idx, message) in messages.iter_mut().enumerate() {
            if Some(idx) == last_tool_idx || message.role != MessageRole::Tool {
                continue;
            }
            for (ridx, result) in message.tool_results.iter_mut().enumerate() {
                if let ToolOutput::Ok { content } = &result.output
                    && content.len() > 200
                {
                    let id = id_at(idx, ridx);
                    if !id.is_empty() && seen.get(&id).is_some_and(|&kept_at| kept_at < (idx, ridx))
                    {
                        deduped_blocks.push(id);
                        result.output = ToolOutput::Ok {
                            content: DEDUP_STUB.to_string(),
                        };
                        deduped += 1;
                    }
                }
            }
        }
    }

    // Pass 2: supersession — when the SAME invocation (tool name +
    // byte-identical input) produced results more than once, older results
    // are stale by construction: the newer run reflects newer workspace
    // state. Unlike pass 1 this fires even when the CONTENT differs (a
    // re-read after an edit). Keyed through the assistant messages' tool
    // calls because results themselves only carry a call_id.
    {
        use std::collections::HashMap;
        // The invocation key (tool name + byte-identical input) behind each
        // tool result, indexed by POSITION like `original_ids`. Resolved by
        // walking the conversation FORWARD and reading the most recent call
        // bearing that call_id, so a result is matched to the call it actually
        // answered. A conversation-wide `call_id -> invocation` map would bind
        // an old result to a NEWER, unrelated call whenever a provider reuses a
        // call_id across steps (Gemini/Vertex do, by construction) — and then
        // stub a live, distinct output as "stale". Input serialization is
        // deterministic for a given call because it round-trips the same
        // serde_json Value.
        let invocation_of: Vec<Vec<Option<String>>> = {
            let mut pending: HashMap<&str, String> = HashMap::new();
            let mut per_message: Vec<Vec<Option<String>>> = Vec::with_capacity(messages.len());
            for message in messages.iter() {
                for call in &message.tool_calls {
                    pending.insert(
                        call.call_id.as_str(),
                        format!("{}\u{0}{}", call.name, call.input),
                    );
                }
                per_message.push(
                    message
                        .tool_results
                        .iter()
                        .map(|r| pending.get(r.call_id.as_str()).cloned())
                        .collect(),
                );
            }
            per_message
        };
        let invocation_at = |message_idx: usize, result_idx: usize| -> Option<&str> {
            invocation_of
                .get(message_idx)
                .and_then(|keys| keys.get(result_idx))
                .and_then(|key| key.as_deref())
        };
        // Latest tool result POSITION per invocation key. The position lets the
        // staleness check below compare ORIGINAL content identities even after
        // pass 1 stubbed a copy.
        let mut latest: HashMap<&str, (usize, usize)> = HashMap::new();
        for (idx, message) in messages.iter().enumerate() {
            if message.role != MessageRole::Tool {
                continue;
            }
            for ridx in 0..message.tool_results.len() {
                if let Some(key) = invocation_at(idx, ridx) {
                    latest.insert(key, (idx, ridx));
                }
            }
        }
        let mut stale: Vec<(usize, usize)> = Vec::new();
        for (idx, message) in messages.iter().enumerate() {
            if Some(idx) == last_tool_idx || message.role != MessageRole::Tool {
                continue;
            }
            for (ridx, result) in message.tool_results.iter().enumerate() {
                let Some(key) = invocation_at(idx, ridx) else {
                    continue;
                };
                // Supersession only restubs Ok results. A superseded error is
                // left to aging/eviction below, which reclaim it by size
                // rather than by staleness — a still-small diagnostic survives
                // whole, only a large one is truncated head+tail.
                let ToolOutput::Ok { content } = &result.output else {
                    continue;
                };
                let Some(&(latest_idx, latest_ridx)) = latest.get(key) else {
                    continue;
                };
                // A later run whose output was byte-identical is redundancy,
                // not staleness — pass 1 already stubbed the later copy, and
                // stubbing this one too would destroy BOTH copies. Compare
                // ORIGINAL content identities (captured before pass 1 could
                // replace the later copy with a stub).
                if content.len() > 200
                    && latest_idx > idx
                    && id_at(latest_idx, latest_ridx) != id_at(idx, ridx)
                {
                    stale.push((idx, ridx));
                }
            }
        }
        for (idx, ridx) in stale {
            if let Some(result) = messages[idx].tool_results.get_mut(ridx) {
                superseded_blocks.push(id_at(idx, ridx));
                result.output = ToolOutput::Ok {
                    content: SUPERSESSION_STUB.to_string(),
                };
                superseded += 1;
            }
        }
    }

    // Pass 3: aging — before dropping anything whole, shrink old large
    // outputs to head+tail. Oldest first, incremental accounting, stop as
    // soon as the low watermark fits; what aging saves, eviction never has
    // to destroy.
    let mut current_tokens = estimate_conversation_tokens(messages);
    if current_tokens > target_tokens {
        for (idx, message) in messages.iter_mut().enumerate() {
            if Some(idx) == last_tool_idx || message.role != MessageRole::Tool {
                continue;
            }
            let before = estimate_message_tokens(message);
            for (ridx, result) in message.tool_results.iter_mut().enumerate() {
                let (payload, is_error) = match &result.output {
                    ToolOutput::Ok { content } => (content, false),
                    ToolOutput::Error { message } => (message, true),
                };
                if payload.len() > AGE_THRESHOLD_CHARS {
                    let aged_payload = age_content(payload);
                    result.output = if is_error {
                        ToolOutput::Error {
                            message: aged_payload,
                        }
                    } else {
                        ToolOutput::Ok {
                            content: aged_payload,
                        }
                    };
                    aged_blocks.push(id_at(idx, ridx));
                    aged += 1;
                }
            }
            let after = estimate_message_tokens(message);
            current_tokens = current_tokens.saturating_sub(before.saturating_sub(after));
            if current_tokens <= target_tokens {
                break;
            }
        }
    }

    // Pass 4: evict oldest large tool outputs until under budget. The running
    // total is tracked incrementally (diffing one message's estimate before
    // and after mutation) rather than by re-scanning the whole conversation
    // on every eviction — the borrow checker won't allow an immutable
    // whole-slice re-scan while a mutable borrow of one message is live, and
    // an O(n) rescan per eviction would be wasteful besides. (Re-scanned
    // once here so aging's incremental drift can't leak into eviction.)
    current_tokens = estimate_conversation_tokens(messages);
    if current_tokens > target_tokens {
        for (idx, message) in messages.iter_mut().enumerate() {
            if Some(idx) == last_tool_idx || message.role != MessageRole::Tool {
                continue;
            }
            let before = estimate_message_tokens(message);
            for (ridx, result) in message.tool_results.iter_mut().enumerate() {
                let (payload_len, is_error) = match &result.output {
                    ToolOutput::Ok { content } => (content.len(), false),
                    ToolOutput::Error { message } => (message.len(), true),
                };
                if payload_len > 400 {
                    evicted_blocks.push(id_at(idx, ridx));
                    result.output = if is_error {
                        ToolOutput::Error {
                            message: EVICTION_STUB.to_string(),
                        }
                    } else {
                        ToolOutput::Ok {
                            content: EVICTION_STUB.to_string(),
                        }
                    };
                    evicted += 1;
                }
            }
            let after = estimate_message_tokens(message);
            current_tokens = current_tokens.saturating_sub(before.saturating_sub(after));
            if current_tokens <= target_tokens {
                break;
            }
        }
    }

    if evicted == 0 && deduped == 0 && superseded == 0 && aged == 0 {
        // Over budget but nothing compactable — don't report a no-op. Nothing
        // was mutated, so pass 4's re-scan is still the live count.
        return (current_tokens, None);
    }
    // Recover the budget passes' replacement records (#1667): a result whose
    // content identity no longer matches the `original_ids` snapshot was
    // rewritten by passes 1–4, and its current output is the replacement the
    // next step's manifest will cite. Only rewritten results pay a hash here —
    // the walk itself compares against ids already captured — and a form that
    // survives unchanged into a later call is never re-journaled, because its
    // id then MATCHES that call's snapshot.
    for (idx, message) in messages.iter().enumerate() {
        if message.role != MessageRole::Tool {
            continue;
        }
        for (ridx, result) in message.tool_results.iter().enumerate() {
            if tool_result_block_id(&result.output) != id_at(idx, ridx) {
                rewrites.push(tool_result_rewrite(&result.output));
            }
        }
    }
    let after_tokens = estimate_conversation_tokens(messages);
    (
        after_tokens,
        Some(CompactionReport {
            before_tokens,
            after_tokens,
            evicted,
            deduped,
            superseded,
            aged,
            evicted_blocks,
            deduped_blocks,
            superseded_blocks,
            aged_blocks,
            rewrites: dedup_rewrites(rewrites),
        }),
    )
}

/// Collapse `rewrites` to one entry per digest, preserving first-seen order.
/// The constant stubs make duplicates the common case — every evicted result
/// leaves the same [`EVICTION_STUB`] bytes — and a digest-keyed consumer gains
/// nothing from the repeats, so the event should not pay to carry them.
fn dedup_rewrites(rewrites: Vec<CompactionRewrite>) -> Vec<CompactionRewrite> {
    let mut seen = std::collections::HashSet::with_capacity(rewrites.len());
    rewrites
        .into_iter()
        .filter(|rewrite| seen.insert(rewrite.content_digest.clone()))
        .collect()
}

#[cfg(test)]
mod tests;
