//! The driver's side of loop detection: turning the live message vector into the
//! evidence `crate::loop_detect` reasons over.
//!
//! `loop_detect` is a pure module that compares `CallRecord`s. Producing those
//! records is the messy part, and it lives here: pairing each tool result back to
//! the call it answers, bounding the window to the current turn, and — because
//! compaction rewrites older results in place before the detector ever sees them
//! — remembering what each call REALLY produced while its bytes are still present
//! (#554).
//!
//! Both scans here run over the transcript on every step, which is why both are
//! memoized against `TranscriptRevision`: the pairing walk is cheap and stays,
//! the per-result serialization and SHA-256 behind an identity does not.

use std::borrow::Cow;
use std::collections::HashMap;

use stella_protocol::{CompletionMessage, MessageRole, ToolCall, ToolOutput};

use crate::loop_detect::CallRecord;
use crate::receipts::TranscriptRevision;

use super::{LOOP_STEER_PREFIX, SUMMARY_MARKER_PREFIX};

/// Pair the tool calls of the CURRENT turn — assistant messages after the
/// last user message — with the outputs they produced, in chronological
/// order, for `crate::loop_detect::detect_loop`. Windowing at the user
/// boundary matters: identical calls across turns are the user re-asking a
/// question, not a stuck loop (a REPL session asking the same thing three
/// times would otherwise trip the exact-repeat detector), and it keeps
/// this per-step scan O(turn) instead of O(entire history). The overflow
/// summary and the stuck-loop warning are also User-role but are not real
/// user turns — treating either as a boundary would truncate the loop
/// window (on every summarization pass, or right when re-detection needs
/// the evidence), so both are skipped when locating the boundary.
///
/// Results attach to the most recent still-unresolved call with a matching
/// `call_id` — providers only guarantee ids unique within one step, and a
/// scripted or misbehaving backend may reuse them across steps. A call
/// whose result is missing keeps `output: None`, which the detector treats
/// as unprovable progress, never loop evidence.
///
/// `identities` is the turn's snapshot from [`snapshot_result_identities`],
/// attached to each resolved record so the detector can compare what a call
/// really produced rather than what compaction left of it (#554). It is
/// keyed by [`CallIdentityKey`], so the lookup here must derive the key from
/// the record's own call. A key that is absent — or poisoned because that
/// same call produced two different outputs — leaves `identity: None` and
/// the detector falls back to the output bytes.
pub(super) fn recent_call_records<'a>(
    messages: &'a [CompletionMessage],
    identities: &ResultIdentities,
) -> Vec<CallRecord<'a>> {
    let turn_start = messages
        .iter()
        .rposition(|m| {
            m.role == MessageRole::User
                && !m.content.starts_with(SUMMARY_MARKER_PREFIX)
                && !m.content.starts_with(LOOP_STEER_PREFIX)
        })
        .map(|i| i + 1)
        .unwrap_or(0);
    let mut records: Vec<CallRecord> = Vec::new();
    for message in &messages[turn_start..] {
        match message.role {
            MessageRole::Assistant => {
                records.extend(message.tool_calls.iter().map(|call| CallRecord {
                    call: call.clone(),
                    output: None,
                    identity: None,
                }));
            }
            MessageRole::Tool => {
                for result in &message.tool_results {
                    if let Some(record) = records
                        .iter_mut()
                        .rev()
                        .find(|r| r.output.is_none() && r.call.call_id == result.call_id)
                    {
                        // Kept as the `Cow` it comes back as: with no volatile
                        // footer (the common path) this borrows, not copies.
                        record.output = Some(comparable_output(&result.output));
                        let key = call_identity_key(&record.call);
                        record.identity = identities.identity_of(&key);
                    }
                }
            }
            MessageRole::System | MessageRole::User => {}
        }
    }
    records
}

/// Identifies a tool call for the purpose of [`ResultIdentities`]: the
/// provider's `call_id` PLUS the call's name and input.
///
/// `call_id` alone is not enough. Providers only guarantee ids unique within
/// one response, and Gemini and Vertex mint them as `call_{ordinal}` where
/// the ordinal is local to a single assistant step (`stella-model/src/
/// gemini.rs`; `vertex.rs` reuses the same aggregation path). So `call_0`
/// restarts on EVERY step, and a bare-`call_id` key collides across steps by
/// construction on two real providers — poisoning the id on the first step
/// whose first call differs, and keeping it poisoned for the rest of the turn.
///
/// Name and input are exactly the fields [`loop_detect::same_record`] already
/// requires to match before two records are "the same", so widening the key
/// with them cannot merge two calls the detector would have distinguished. It
/// only stops two UNRELATED calls that happened to share an ordinal from being
/// treated as one.
///
/// Deliberately not positional: [`Engine::apply_overflow_summary`] splices a
/// span of messages down to a single summary, so any index-derived key would
/// silently re-point surviving results at another call's evidence after the
/// first overflow — a WRONG identity, which is far worse than none.
pub(super) type CallIdentityKey = (String, String, String);

/// Build the [`CallIdentityKey`] for one tool call. `input` is serialized
/// rather than hashed so the key stays debuggable; both producers derive it
/// from the same `ToolCall`, so they always agree.
pub(super) fn call_identity_key(call: &ToolCall) -> CallIdentityKey {
    (
        call.call_id.clone(),
        call.name.clone(),
        call.input.to_string(),
    )
}

/// A turn's snapshot of what each tool call really produced.
///
/// `by_call` is the evidence the loop detector reads, keyed by
/// [`CallIdentityKey`] ([`snapshot_result_identities`]). `None` marks a POISONED
/// key: that same call was observed carrying two different uncompacted outputs,
/// so nothing about it can be trusted. It **accumulates for the whole turn** and
/// is never cleared — that accumulation is the point of #554.
///
/// `by_position` is a memo, not evidence. The sweep re-derives every result's
/// identity on every step, and deriving one costs a `serde_json` serialization of
/// the whole output plus a SHA-256 over it. A result at a given
/// `(message_index, result_index)` keeps producing the same identity for as long
/// as nothing rewrites the transcript underneath it, so within one
/// [`TranscriptRevision`] the second and later derivations are pure waste. Unlike
/// `by_call` this IS cleared, on every rewrite.
///
/// # Why clearing it is belt-and-braces rather than load-bearing
///
/// Worth stating precisely, because disabling the clearing breaks no test and that
/// could be mistaken for missing coverage. Every rewrite the compaction passes
/// perform leaves content `crate::compaction::is_compacted_output` recognizes —
/// the three stubs, or the aging elision marker — and the sweep skips a recognized
/// result before deriving anything. So the only positions ever served from this
/// memo are ones compaction did not touch, which makes a stale entry unobservable
/// even with the clearing removed.
///
/// That is a cross-module invariant, so it is pinned where it can break rather
/// than assumed: `every_rewrite_compaction_performs_is_recognized_as_compacted`.
/// A future pass that rewrote a result into something unrecognized would make this
/// memo start serving pre-rewrite identities, and the clearing is what keeps that
/// from being a silent correctness bug instead of a caught one.
#[derive(Debug, Default)]
pub(super) struct ResultIdentities {
    pub(super) by_call: HashMap<CallIdentityKey, Option<String>>,
    by_position: HashMap<(usize, usize), String>,
    revision: TranscriptRevision,
}

impl ResultIdentities {
    /// Drop the positional memo if the transcript was rewritten since the last
    /// sweep. `by_call` survives — it is the turn's accumulated evidence.
    fn sync_to(&mut self, revision: TranscriptRevision) {
        if self.revision != revision {
            self.by_position.clear();
            self.revision = revision;
        }
    }

    /// What the loop detector asks: the identity recorded for this call, or
    /// `None` if absent or poisoned.
    pub(super) fn identity_of(&self, key: &CallIdentityKey) -> Option<String> {
        self.by_call.get(key).cloned().flatten()
    }
}

/// Record every tool result's identity into `identities`, keyed by
/// [`CallIdentityKey`] — the driver's answer to #554. (Keyed by the call's
/// id AND its name and input, never the id alone: see that type's docs for
/// the providers that recycle ids across steps.)
///
/// Compaction rewrites tool results IN PLACE (dedup/supersession stubs,
/// middle-out aging, the eviction stub) and runs immediately before loop
/// detection in the same step, so a three-call identical streak reaches the
/// detector as `[stub, stub, real]` and the byte-identical-output
/// requirement can never be met. Snapshotting each result's identity while
/// its real content is still present preserves exactly the evidence the
/// detector needs, without teaching either pure module about the other.
/// Called once per step BEFORE the compaction pass; the map accumulates
/// across the turn, because a result's real content is only on hand for the
/// one step between producing it and the pass that stubs it.
///
/// Two rules make accumulation safe:
///
/// - **A compacted output is never recorded.** Its identity is the stub's,
///   not the call's; recording it would overwrite the real one and lose the
///   evidence permanently, and every evicted result shares one stub.
/// - **A call seen with two different real outputs is poisoned** (`None`),
///   never overwritten. Keeping either identity would attach one call's
///   evidence to a different call, and a WRONG identity is far worse than
///   none: it can make two genuinely different outputs compare equal and
///   abort a healthy turn. Poisoned keys fall back to comparing the live
///   outputs, i.e. the behavior before this fix. With a
///   [`CallIdentityKey`] a conflict now means what it says — the same tool,
///   same arguments, different result, which IS progress — rather than
///   firing on two unrelated calls that shared a recycled ordinal.
///
/// The identity is computed over [`comparable_output`], the same
/// normalization the detector compares, so `read_file`'s volatile
/// session-tally footer does not make every reread a distinct identity.
///
/// # The invariant that keeps a poisoned key from manufacturing a loop
///
/// A poisoned key degrades to comparing the live outputs, and compaction
/// rewrites older results to ONE shared stub — so if every record in the
/// detector's window could be a stub, three unrelated calls would compare
/// byte-equal and abort a healthy turn. They cannot, and the reason is a
/// cross-module invariant worth naming: `crate::compaction::compact` never
/// touches the LAST `MessageRole::Tool` message (its `last_tool_idx` guard),
/// and `detect_loop` only ever reports a verdict anchored on the trailing
/// record. The anchor therefore always carries its real, freshly-produced
/// output, and a stub can never match it. Relaxing that guard in
/// `compaction.rs` would silently turn every over-budget turn into a
/// false-positive loop abort here.
pub(super) fn snapshot_result_identities(
    messages: &[CompletionMessage],
    identities: &mut ResultIdentities,
    revision: TranscriptRevision,
) {
    identities.sync_to(revision);
    // Calls seen so far that nothing has answered yet, oldest first. The map
    // is keyed by the CALL, so a result has to be paired back to the call it
    // answers before its identity can be filed — using the same attachment
    // rule as [`recent_call_records`], or the two would key differently and
    // the lookup would silently miss.
    let mut unanswered: Vec<&ToolCall> = Vec::new();
    for (message_index, message) in messages.iter().enumerate() {
        match message.role {
            MessageRole::Assistant => unanswered.extend(message.tool_calls.iter()),
            MessageRole::Tool => {
                for (result_index, result) in message.tool_results.iter().enumerate() {
                    let Some(position) = unanswered
                        .iter()
                        .rposition(|call| call.call_id == result.call_id)
                    else {
                        continue;
                    };
                    let call = unanswered.remove(position);
                    if crate::compaction::is_compacted_output(&result.output) {
                        continue;
                    }
                    let key = call_identity_key(call);
                    // A poisoned key can never be un-poisoned: any later
                    // observation conflicts with `None`, so the verdict is
                    // already final. Hashing this result's content only to
                    // re-derive it is pure waste, and this scan re-walks the
                    // WHOLE transcript on every step (#554).
                    if matches!(identities.by_call.get(&key), Some(None)) {
                        continue;
                    }
                    // The walk itself has to keep going — pairing a result back
                    // to its call is what makes the key meaningful, and the
                    // `unanswered` bookkeeping only works read in order. What is
                    // skippable is DERIVING the identity, which is the
                    // serialization and the hash. Same position, same revision,
                    // same bytes, same answer.
                    let position_key = (message_index, result_index);
                    let identity = match identities.by_position.get(&position_key) {
                        Some(known) => known.clone(),
                        None => {
                            let derived = crate::receipts::tool_result_block_id(
                                &comparable_output(&result.output),
                            );
                            identities.by_position.insert(position_key, derived.clone());
                            derived
                        }
                    };
                    let conflicts = matches!(
                        identities.by_call.get(&key),
                        Some(seen) if seen.as_deref() != Some(identity.as_str())
                    );
                    identities
                        .by_call
                        .insert(key, if conflicts { None } else { Some(identity) });
                }
            }
            MessageRole::System | MessageRole::User => {}
        }
    }
}

/// Normalize one tool output for loop comparison: strip `read_file`'s
/// volatile session-tally footer (`\n\n(N/M lines shown · read K× this
/// session)`). The footer changes on EVERY read by design — it is the
/// model-facing "you already read this" nudge — but the loop detector
/// requires byte-identical outputs, so the footer made every reread unique
/// and blinded detection to the exact thrash it exists to catch (the
/// read → failing-edit → read cycle the `loop_detect` module doc names).
/// Comparison-only: the transcript and history keep the footer untouched.
///
/// Borrowed on the overwhelmingly common path (no footer): both callers run
/// over the WHOLE transcript on every step, so returning an owned copy meant a
/// full heap copy of every tool result — a step's worth of garbage proportional
/// to the entire history, for a normalization that usually changes nothing.
pub(super) fn comparable_output(output: &ToolOutput) -> Cow<'_, ToolOutput> {
    if let ToolOutput::Ok { content } = output
        && content.ends_with("\u{d7} this session)")
        && let Some(idx) = content.rfind("\n\n(")
        && content[idx..].contains(" lines shown \u{b7} read ")
    {
        return Cow::Owned(ToolOutput::Ok {
            content: content[..idx].to_string(),
        });
    }
    Cow::Borrowed(output)
}
