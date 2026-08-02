//! Context compaction — pure synchronous logic over owned data
//! Four mechanisms, applied least-lossy first:
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

use stella_protocol::{CompletionMessage, MessageRole, ToolOutput};

use crate::estimator::{estimate_conversation_tokens, estimate_message_tokens};
use crate::receipts::tool_result_block_id;

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

/// Evict + dedup until the conversation fits `budget_tokens` — reclaiming
/// down to a low watermark an eighth below it (see `compact_measured`'s
/// hysteresis note) — or until nothing more can be safely removed. Returns
/// `None` if no compaction was needed (already under budget) — or if the
/// pass changed nothing (all remaining content is protected), so a
/// permanently-over-budget conversation doesn't emit a no-op `Compaction`
/// event before every step.
///
/// Prefer [`compact_measured`] on the step path: this form throws away the
/// post-pass token count, which the caller then has to walk the whole
/// transcript again to recover.
pub fn compact(messages: &mut [CompletionMessage], budget_tokens: u64) -> Option<CompactionReport> {
    compact_measured(messages, budget_tokens).1
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
) -> (u64, Option<CompactionReport>) {
    let before_tokens = estimate_conversation_tokens(messages);
    if before_tokens <= budget_tokens {
        return (before_tokens, None);
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
    let mut aged = 0usize;
    let mut evicted = 0usize;
    // Identities alongside the counts — the block_id each pass stubbed (§6.2).
    let mut deduped_blocks: Vec<String> = Vec::new();
    let mut superseded_blocks: Vec<String> = Vec::new();
    let mut aged_blocks: Vec<String> = Vec::new();
    let mut evicted_blocks: Vec<String> = Vec::new();
    // Each tool result's ORIGINAL block_id, captured before any pass mutates
    // it, indexed by POSITION: `original_ids[message_idx][result_idx]`. A
    // result aged then evicted in the same call must be recorded under the id
    // the manifest cited, not the id of its intermediate aged content, which is
    // what capturing up front preserves.
    //
    // Deliberately NOT keyed by `call_id`. A call_id is unique only within ONE
    // step — `driver.rs::snapshot_result_identities` says so and poisons any id
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
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::{ToolCall, ToolResult};

    fn tool_msg(call_id: &str, content: String) -> CompletionMessage {
        CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![ToolResult {
                call_id: call_id.into(),
                output: ToolOutput::Ok { content },
            }],
            attachments: Vec::new(),
        }
    }

    fn tool_error_msg(call_id: &str, message: String) -> CompletionMessage {
        CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![ToolResult {
                call_id: call_id.into(),
                output: ToolOutput::Error { message },
            }],
            attachments: Vec::new(),
        }
    }

    fn assistant_with_call_on(call_id: &str, path: &str) -> CompletionMessage {
        CompletionMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                call_id: call_id.into(),
                name: "read_file".into(),
                input: serde_json::json!({ "path": path }),
            }],
            tool_results: vec![],
            attachments: Vec::new(),
        }
    }

    /// Distinct target per call id, so tests exercising dedup/eviction in
    /// isolation don't also trip the supersession pass (which keys on
    /// identical name+input).
    fn assistant_with_call(call_id: &str) -> CompletionMessage {
        CompletionMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                call_id: call_id.into(),
                name: "read_file".into(),
                input: serde_json::json!({ "path": call_id }),
            }],
            tool_results: vec![],
            attachments: Vec::new(),
        }
    }

    #[test]
    fn no_compaction_when_under_budget() {
        let mut messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("hi"),
        ];
        assert!(compact(&mut messages, 1_000_000).is_none());
    }

    #[test]
    fn evicts_oldest_large_output_first_and_reports() {
        let mut messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("do things"),
            assistant_with_call("c1"),
            tool_msg("c1", "old ".repeat(2000)),
            assistant_with_call("c2"),
            tool_msg("c2", "new ".repeat(2000)),
        ];
        let report = compact(&mut messages, 2500).expect("compaction should run");
        assert!(report.evicted >= 1);
        assert!(report.after_tokens < report.before_tokens);
        // The OLD output (idx 3) was evicted…
        match &messages[3].tool_results[0].output {
            ToolOutput::Ok { content } => assert!(content.contains("evicted")),
            _ => panic!("expected stub"),
        }
    }

    #[test]
    fn eviction_reports_the_block_identity_the_manifest_cited() {
        // §6.2: the report names WHICH block was evicted, by the same
        // content-addressed id the receipt manifest recorded for it — so a
        // later pass can prove that block was dropped before it was ever used.
        let old_output = ToolOutput::Ok {
            content: "old ".repeat(2000),
        };
        let expected = crate::receipts::tool_result_block_id(&old_output);
        let mut messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("do things"),
            assistant_with_call("c1"),
            tool_msg("c1", "old ".repeat(2000)),
            assistant_with_call("c2"),
            tool_msg("c2", "new ".repeat(2000)),
        ];
        let report = compact(&mut messages, 2500).expect("compaction should run");
        assert_eq!(
            report.evicted_blocks.len(),
            report.evicted,
            "one identity per evicted block"
        );
        assert!(
            report.evicted_blocks.contains(&expected),
            "evicted_blocks {:?} must name the evicted output's block_id {expected}",
            report.evicted_blocks
        );
    }

    #[test]
    fn never_evicts_the_most_recent_tool_result() {
        // Property: compaction never drops a still-referenced tool result —
        // the result answering the latest assistant call survives even under
        // an impossible budget.
        let latest = "latest ".repeat(2000);
        let mut messages = vec![
            CompletionMessage::system("sys"),
            assistant_with_call("c1"),
            tool_msg("c1", "old ".repeat(2000)),
            assistant_with_call("c2"),
            tool_msg("c2", latest.clone()),
        ];
        compact(&mut messages, 1); // impossible budget
        match &messages[4].tool_results[0].output {
            ToolOutput::Ok { content } => assert_eq!(content, &latest),
            _ => panic!("latest tool result must survive"),
        }
    }

    #[test]
    fn dedups_identical_outputs_keeping_the_earliest() {
        let repeated = "same big output ".repeat(100);
        let mut messages = vec![
            CompletionMessage::system("sys"),
            assistant_with_call("c1"),
            tool_msg("c1", repeated.clone()),
            assistant_with_call("c2"),
            tool_msg("c2", repeated.clone()),
            assistant_with_call("c3"),
            tool_msg("c3", "different".into()),
        ];
        // Budget must be tight enough to force compaction (below the
        // ~1000-token pre-dedup total) but loose enough that the single
        // surviving copy left after dedup (~500 tokens) doesn't ALSO need
        // to be evicted — the low WATERMARK (budget minus an eighth), not
        // the budget itself, is what passes 3/4 reclaim toward, so the
        // budget sits an eighth higher than the pre-hysteresis tuning.
        let report = compact(&mut messages, 800).expect("should compact");
        assert!(report.deduped >= 1);
        // The EARLIEST copy (idx 2) survives byte-identical, so the prompt
        // prefix — and the provider cache built over it — is untouched (#372).
        match &messages[2].tool_results[0].output {
            ToolOutput::Ok { content } => assert_eq!(content, &repeated),
            _ => panic!("earliest copy must be intact"),
        }
        // The later copy (idx 4) is stubbed with a pointer to the earlier one.
        match &messages[4].tool_results[0].output {
            ToolOutput::Ok { content } => {
                assert!(content.contains("earlier tool result"), "got: {content}")
            }
            _ => panic!("expected dedup stub"),
        }
    }

    #[test]
    fn duplicates_within_one_tool_message_are_deduped() {
        // A whole step's results land in ONE Tool message, so a parallel
        // batch that read the same file twice puts two byte-identical
        // outputs at the same message index. The index-only dedup key could
        // never fire on them (`kept_at < idx` is false within a message);
        // the positional key must stub the later sibling and keep the first.
        let repeated = "same big output ".repeat(100);
        let mut messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage {
                role: MessageRole::Assistant,
                content: String::new(),
                tool_calls: vec![
                    ToolCall {
                        call_id: "c1".into(),
                        name: "read_file".into(),
                        input: serde_json::json!({ "path": "a.rs" }),
                    },
                    ToolCall {
                        call_id: "c2".into(),
                        name: "read_file".into(),
                        input: serde_json::json!({ "path": "b.rs" }),
                    },
                ],
                tool_results: vec![],
                attachments: Vec::new(),
            },
            CompletionMessage {
                role: MessageRole::Tool,
                content: String::new(),
                tool_calls: vec![],
                tool_results: vec![
                    ToolResult {
                        call_id: "c1".into(),
                        output: ToolOutput::Ok {
                            content: repeated.clone(),
                        },
                    },
                    ToolResult {
                        call_id: "c2".into(),
                        output: ToolOutput::Ok {
                            content: repeated.clone(),
                        },
                    },
                ],
                attachments: Vec::new(),
            },
            assistant_with_call("c3"),
            tool_msg("c3", "different".into()),
        ];
        // Tight enough to trigger (below the ~970-token raw total), loose
        // enough that the surviving copy needs no eviction (watermark above
        // the ~550-token post-dedup total).
        let report = compact(&mut messages, 700).expect("should compact");
        assert!(report.deduped >= 1, "{report:?}");
        match &messages[2].tool_results[0].output {
            ToolOutput::Ok { content } => assert_eq!(content, &repeated, "first copy survives"),
            _ => panic!("earliest copy must be intact"),
        }
        match &messages[2].tool_results[1].output {
            ToolOutput::Ok { content } => {
                assert!(
                    content.contains("earlier tool result"),
                    "the same-message sibling must be stubbed, got: {content}"
                )
            }
            _ => panic!("expected a dedup stub on the sibling"),
        }
    }

    #[test]
    fn identical_rerun_is_deduped_not_superseded() {
        // The SAME call (name + byte-identical input) run twice returning
        // byte-identical output is redundancy, not staleness: pass 1 stubs
        // the later copy and pass 2 must NOT then stub the surviving earlier
        // copy — that would destroy both (#372 interplay guard).
        let repeated = "identical contents ".repeat(100);
        let mut messages = vec![
            CompletionMessage::system("sys"),
            assistant_with_call_on("c1", "src/lib.rs"),
            tool_msg("c1", repeated.clone()),
            assistant_with_call_on("c2", "src/lib.rs"),
            tool_msg("c2", repeated.clone()),
            assistant_with_call("c3"),
            tool_msg("c3", "different".into()),
        ];
        let report = compact(&mut messages, 800).expect("should compact");
        assert_eq!(report.superseded, 0, "{report:?}");
        assert!(report.deduped >= 1, "{report:?}");
        match &messages[2].tool_results[0].output {
            ToolOutput::Ok { content } => assert_eq!(content, &repeated),
            _ => panic!("earliest copy must survive intact"),
        }
        match &messages[4].tool_results[0].output {
            ToolOutput::Ok { content } => {
                assert!(content.contains("earlier tool result"), "got: {content}")
            }
            _ => panic!("expected a dedup stub on the later copy"),
        }
    }

    #[test]
    fn recurring_call_id_never_dedups_distinct_outputs() {
        // Witness for the pass-1 regression in PR #595 (issue #560). Dedup was
        // keyed on an identity resolved through a `HashMap<call_id, block_id>`,
        // so a call_id that RECURS collapsed every result carrying it onto the
        // LAST occurrence's id and they all compared equal. Gemini and Vertex
        // mint ids as `call_{ordinal}` counted per RESPONSE
        // (`stella-model/src/gemini.rs`), so `call_0` comes back on every
        // assistant step: below, three reads of three different files return
        // three different outputs, all answering `call_0`. The middle one was
        // replaced by the dedup stub — text asserting the model has already
        // seen content that was never in the transcript.
        //
        // Sized so no other pass can confound the result: each payload is >200
        // chars (pass 1 considers it) but <=400 (eviction's floor) and far
        // under AGE_THRESHOLD_CHARS, so even an impossible budget reclaims
        // nothing and `compact` correctly reports a no-op.
        let out = |tag: char| tag.to_string().repeat(300);
        let mut messages = vec![
            CompletionMessage::system("sys"),
            assistant_with_call_on("call_0", "src/a.rs"),
            tool_msg("call_0", out('a')),
            assistant_with_call_on("call_0", "src/b.rs"),
            tool_msg("call_0", out('b')),
            assistant_with_call_on("call_0", "src/c.rs"),
            tool_msg("call_0", out('c')),
        ];
        let report = compact(&mut messages, 1);
        assert_eq!(
            report.as_ref().map_or(0, |r| r.deduped),
            0,
            "distinct outputs sharing a recurring call_id are not duplicates: {report:?}"
        );
        assert_eq!(
            report.as_ref().map_or(0, |r| r.superseded),
            0,
            "three different files are three invocations — none is stale: {report:?}"
        );
        for (idx, tag) in [(2usize, 'a'), (4, 'b'), (6, 'c')] {
            match &messages[idx].tool_results[0].output {
                ToolOutput::Ok { content } => {
                    assert_eq!(content, &out(tag), "message {idx} lost its output")
                }
                _ => panic!("message {idx}: expected its untouched Ok output"),
            }
        }
    }

    #[test]
    fn byte_identical_outputs_still_dedup_under_a_recurring_call_id() {
        // The other half of the contract, and a witness for the receipt half of
        // the same regression: keying identities by position must not turn
        // dedup off, and the id it reports must be the id of the block it
        // actually stubbed. Under the call_id-keyed map both copies resolved to
        // the LAST result's id, so `deduped_blocks` cited a block that was
        // never touched.
        let repeated = "same big output ".repeat(20);
        let mut messages = vec![
            CompletionMessage::system("sys"),
            assistant_with_call_on("call_0", "src/a.rs"),
            tool_msg("call_0", repeated.clone()),
            assistant_with_call_on("call_0", "src/b.rs"),
            tool_msg("call_0", repeated.clone()),
            assistant_with_call_on("call_0", "src/c.rs"),
            tool_msg("call_0", "tail".into()),
        ];
        let report = compact(&mut messages, 1).expect("should compact");
        assert_eq!(report.deduped, 1, "{report:?}");
        assert_eq!(
            report.deduped_blocks,
            vec![tool_result_block_id(&ToolOutput::Ok {
                content: repeated.clone()
            })],
            "the receipt must cite the identity of the block it stubbed"
        );
        // The EARLIEST copy survives byte-identical (#372)…
        match &messages[2].tool_results[0].output {
            ToolOutput::Ok { content } => assert_eq!(content, &repeated),
            _ => panic!("earliest copy must survive intact"),
        }
        // …and only the later duplicate is stubbed.
        match &messages[4].tool_results[0].output {
            ToolOutput::Ok { content } => {
                assert!(content.contains("earlier tool result"), "got: {content}")
            }
            _ => panic!("expected a dedup stub on the later copy"),
        }
    }

    #[test]
    fn a_compacted_conversation_absorbs_a_step_of_growth_without_recompacting() {
        // The hysteresis witness: stopping exactly at the budget meant a
        // saturated turn re-crossed it on every step's few thousand new
        // tokens, and every step's pass rewrote another old tool result —
        // a prompt-cache-destroying prefix mutation per step. Reclaiming to
        // the low watermark must leave enough headroom that the next step's
        // ordinary growth does NOT re-trigger a rewrite.
        let budget = 4_000u64;
        let mut messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("do things"),
        ];
        for i in 0..8 {
            let id = format!("c{i}");
            messages.push(assistant_with_call(&id));
            messages.push(tool_msg(&id, format!("{i} ").repeat(1500)));
        }
        let (after, report) = compact_measured(&mut messages, budget);
        assert!(report.is_some(), "the oversized transcript must compact");
        assert!(after <= budget, "must land under budget: {after}");
        assert!(
            after <= budget - budget / 8,
            "must reclaim to the low watermark, not stop at the budget: {after}"
        );

        // One step of ordinary growth — an assistant turn and a small tool
        // result — fits inside the reclaimed headroom…
        messages.push(assistant_with_call("next"));
        messages.push(tool_msg("next", "small new output".into()));
        let (_, report) = compact_measured(&mut messages, budget);
        // …so the pass must NOT mutate the transcript again this step.
        assert!(
            report.is_none(),
            "growth inside the watermark headroom must not re-trigger a rewrite: {report:?}"
        );
    }

    #[test]
    fn eviction_is_monotonic_under_shrinking_budgets() {
        // Property: budget eviction monotonic — a smaller budget never
        // yields MORE tokens than a bigger one on the same input.
        let build = || {
            vec![
                CompletionMessage::system("sys"),
                assistant_with_call("c1"),
                tool_msg("c1", "aaaa ".repeat(1000)),
                assistant_with_call("c2"),
                tool_msg("c2", "bbbb ".repeat(1000)),
                assistant_with_call("c3"),
                tool_msg("c3", "cccc ".repeat(1000)),
            ]
        };
        let mut generous = build();
        let mut tight = build();
        compact(&mut generous, 3000);
        compact(&mut tight, 500);
        assert!(
            estimate_conversation_tokens(&tight) <= estimate_conversation_tokens(&generous),
            "tighter budget must not leave more tokens"
        );
    }

    #[test]
    fn repeated_identical_call_supersedes_older_differing_results() {
        // Same tool, same input, run twice with DIFFERENT outputs (a
        // re-read after an edit): the older result is stale by
        // construction and must be stubbed even though byte-dedup can't
        // touch it. A third call on a DIFFERENT target must be untouched.
        let mut messages = vec![
            CompletionMessage::system("sys"),
            assistant_with_call_on("c1", "src/lib.rs"),
            tool_msg("c1", "pre-edit contents ".repeat(100)),
            assistant_with_call_on("c2", "src/other.rs"),
            tool_msg("c2", "unrelated file ".repeat(100)),
            assistant_with_call_on("c3", "src/lib.rs"),
            tool_msg("c3", "post-edit contents ".repeat(100)),
        ];
        // Below the raw total (~1300 tokens), with the low WATERMARK (budget
        // minus an eighth — what passes 3/4 actually reclaim toward) still
        // above what supersession alone leaves (~900), so eviction never has
        // to fire and the untouched-neighbors assertions below stay
        // meaningful.
        let report = compact(&mut messages, 1_250).expect("should compact");
        assert!(report.superseded >= 1, "{report:?}");
        match &messages[2].tool_results[0].output {
            ToolOutput::Ok { content } => {
                assert!(content.contains("stale result"), "got: {content}")
            }
            _ => panic!("expected supersession stub"),
        }
        // The different-target read keeps its full content…
        match &messages[4].tool_results[0].output {
            ToolOutput::Ok { content } => {
                assert!(content.starts_with("unrelated file"), "got: {content}")
            }
            _ => panic!("different invocation must not be superseded"),
        }
        // …and the superseding (latest) result is intact.
        match &messages[6].tool_results[0].output {
            ToolOutput::Ok { content } => {
                assert!(content.starts_with("post-edit"), "got: {content}")
            }
            _ => panic!("latest result must survive"),
        }
    }

    #[test]
    fn aging_shrinks_old_outputs_keeping_head_and_tail_before_eviction() {
        let body = format!("HEADLINE\n{}\nTAILLINE", "filler ".repeat(6000));
        let mut messages = vec![
            CompletionMessage::system("sys"),
            assistant_with_call("c1"),
            tool_msg("c1", body),
            assistant_with_call("c2"),
            tool_msg("c2", "recent ".repeat(50)),
        ];
        // Budget below the raw size but comfortably above the aged size:
        // aging alone must satisfy it, so nothing gets evicted whole.
        let report = compact(&mut messages, 2_000).expect("should compact");
        assert!(report.aged >= 1, "{report:?}");
        assert_eq!(report.evicted, 0, "aging must run before eviction");
        match &messages[2].tool_results[0].output {
            ToolOutput::Ok { content } => {
                assert!(content.starts_with("HEADLINE"), "head lost: {content:.40}");
                assert!(content.ends_with("TAILLINE"), "tail lost");
                assert!(content.contains("middle elided"));
                assert!(content.len() < 2_000, "aged output still huge");
            }
            _ => panic!("expected aged content"),
        }
    }

    #[test]
    fn small_error_output_is_left_intact() {
        // A small error is pure diagnostic and below every size floor: it
        // must survive compaction whole even as large neighbors are reclaimed.
        let mut messages = vec![
            CompletionMessage::system("sys"),
            assistant_with_call("c1"),
            tool_error_msg("c1", "diagnostic that matters".into()),
            assistant_with_call("c2"),
            tool_msg("c2", "filler ".repeat(2000)),
            assistant_with_call("c3"),
            tool_msg("c3", "recent ".repeat(10)),
        ];
        compact(&mut messages, 200);
        match &messages[2].tool_results[0].output {
            ToolOutput::Error { message } => {
                assert_eq!(message, "diagnostic that matters")
            }
            _ => panic!("small error diagnostics must survive compaction"),
        }
    }

    #[test]
    fn aging_shrinks_old_error_outputs_keeping_head_and_tail_before_eviction() {
        // A large error is truncated middle-out like a large Ok output: the
        // head (framing) and tail (the failure lines) survive where whole
        // eviction would lose them.
        let body = format!("HEADLINE\n{}\nTAILLINE", "filler ".repeat(6000));
        let mut messages = vec![
            CompletionMessage::system("sys"),
            assistant_with_call("c1"),
            tool_error_msg("c1", body),
            assistant_with_call("c2"),
            tool_msg("c2", "recent ".repeat(50)),
        ];
        let report = compact(&mut messages, 2_000).expect("should compact");
        assert!(report.aged >= 1, "{report:?}");
        assert_eq!(report.evicted, 0, "aging must run before eviction");
        match &messages[2].tool_results[0].output {
            ToolOutput::Error { message } => {
                assert!(message.starts_with("HEADLINE"), "head lost: {message:.40}");
                assert!(message.ends_with("TAILLINE"), "tail lost");
                assert!(message.contains("middle elided"));
                assert!(message.len() < 2_000, "aged error still huge");
            }
            _ => panic!("expected aged error content"),
        }
    }

    #[test]
    fn large_error_output_is_evicted_like_large_ok() {
        // Between the aging threshold and the eviction size floor, so aging
        // can't touch it and eviction is what reclaims it — mirroring Ok.
        let mut messages = vec![
            CompletionMessage::system("sys"),
            assistant_with_call("c1"),
            tool_error_msg("c1", "boom ".repeat(300)),
            assistant_with_call("c2"),
            tool_msg("c2", "recent ".repeat(50)),
        ];
        let report = compact(&mut messages, 200).expect("should compact");
        assert!(report.evicted >= 1, "{report:?}");
        match &messages[2].tool_results[0].output {
            ToolOutput::Error { message } => assert!(message.contains("evicted")),
            _ => panic!("expected an eviction stub that keeps the error variant"),
        }
    }

    #[test]
    fn red_loop_of_large_errors_is_reclaimable() {
        // The bug: a red loop of repeated ~100 KB failures accumulated context
        // no pure compaction pass could reclaim. Now every large error but the
        // most recent is reclaimable, so the conversation fits budget again.
        let big_err = |n: usize| format!("failure {n}\n{}", "E".repeat(100_000));
        let mut messages = vec![
            CompletionMessage::system("sys"),
            assistant_with_call("c1"),
            tool_error_msg("c1", big_err(1)),
            assistant_with_call("c2"),
            tool_error_msg("c2", big_err(2)),
            assistant_with_call("c3"),
            tool_error_msg("c3", big_err(3)),
            assistant_with_call("c4"),
            tool_error_msg("c4", big_err(4)),
        ];
        let before = estimate_conversation_tokens(&messages);
        let budget = 35_000;
        let report = compact(&mut messages, budget).expect("should compact");
        assert!(
            report.aged >= 3,
            "older failures must be reclaimed: {report:?}"
        );
        let after = estimate_conversation_tokens(&messages);
        assert!(after < before, "compaction must reclaim tokens");
        assert!(
            after <= budget,
            "still over budget after compaction: {after}"
        );
        // The most recent failure — the one the agent is acting on — survives.
        match &messages[8].tool_results[0].output {
            ToolOutput::Error { message } => {
                assert!(
                    message.starts_with("failure 4"),
                    "latest error must survive whole"
                );
                assert!(
                    message.len() > 100_000,
                    "latest error must not be truncated"
                );
            }
            _ => panic!("most recent error must survive intact"),
        }
    }
}
