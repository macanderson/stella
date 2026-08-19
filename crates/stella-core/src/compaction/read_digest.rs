// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The already-read digest (#3806): what compaction destroys, stated once as
//! current knowledge instead of scattered across the transcript.
//!
//! When a budget pass stubs a `read_file` result, the only statement of what
//! that call returned leaves context, and `super`'s `EVICTION_STUB` tells the
//! model to *re-run the tool*. Post-compaction that is the only option it has:
//! the path is still visible in the assistant message's `tool_calls` — those
//! are never rewritten — but as N separate call blocks scattered through the
//! history, never as one sentence saying what this session already looked at.
//! Observed cost in a real trace: six whole files re-read in full after two
//! compaction passes, in a documentation-only session, and paid again after
//! every subsequent pass.
//!
//! This module is the pure decision half. It walks the transcript the engine
//! already owns and answers one question — *which reads have lost their
//! result, and are still worth naming* — then renders the single tail message
//! that carries the answer. It stats nothing and opens nothing (invariant 2);
//! every fact it states is read out of `messages`.
//!
//! # The sibling that solves the other half
//!
//! [`crate::restore`] (#2685) is the same problem on the *summarizer* path:
//! when an overflow summary destroys a span, restoration re-reads those files
//! through the [`crate::ports::ToolExecutor`] port and re-sends their
//! **contents**. This module deliberately does not do that. Compaction runs
//! before every model call and fires precisely because context is scarce, so
//! re-sending content is the one thing it must not spend bytes on. A digest
//! names paths; restoration carries bytes. The two are complementary and
//! neither subsumes the other.
//!
//! # Staleness is the correctness trap
//!
//! A digest that reads as "contents known" for a file edited after it was read
//! causes a *wrong* answer, which is strictly worse than the redundant read it
//! was meant to save. Two defenses, and the wording is the second one:
//!
//! - **Path-keyed invalidation.** A `write_file` / `edit_file` / `delete_file`
//!   naming the same path at or after the read drops that path
//!   ([`MUTATING_TOOLS`]). So does a later read of the same path whose result
//!   is still in context — nothing was lost there, so nothing needs saying.
//! - **The message claims a pointer, not a cache.** It says the read happened
//!   and its output has left context; it never says the current bytes are
//!   known. That claim stays true under an edit this module cannot see — a
//!   `bash` heredoc, `sed -i`, an external editor — which is the residual gap
//!   ([`MUTATING_TOOLS`] cannot name a path a shell command mutated) and the
//!   reason the wording is deliberately weaker than the invalidation rule.
//!
//! # Byte stability (invariant 7)
//!
//! The digest is a `User`-role message in the **volatile tail**, never part of
//! the stable prefix, and it carries [`READ_DIGEST_MARKER_PREFIX`] so both
//! [`crate::engine_markers::ENGINE_MARKERS`] consumers classify it as engine
//! text rather than the person speaking.
//!
//! [`apply_digest`] holds exactly one digest message in the transcript and
//! rewrites it **in place** when its content changes, rather than appending a
//! second one — an appended-per-pass chain would restate the same paths every
//! round and grow without bound. When the rendered digest is byte-identical to
//! the one already there, it writes nothing at all: a pass that changes no
//! conclusion must not mutate a single byte, the same hysteresis the retention
//! pass applies for the same reason (`super`'s pass 0).

use std::collections::HashMap;

use stella_protocol::{CompletionMessage, MessageRole};

use crate::restore::{READ_PATH_PARAM, READ_TOOL};

/// Prefix of the engine-authored digest message. Registered in
/// [`crate::engine_markers::ENGINE_MARKERS`] like every other engine-injected
/// `User`-role marker, and taught to the model by both static prompts — a
/// marker the injection-defense contract does not name is one the model is
/// entitled to treat as text impersonating the operator.
pub const READ_DIGEST_MARKER_PREFIX: &str = "[files already read";

/// The tools that make a prior read's conclusions unsafe to state. Named by
/// the same catalog ids `stella-tools` declares (`catalog.rs`); the pin test
/// below fails if this list stops matching the catalog's mutating file tools,
/// because a renamed write tool that the digest no longer recognizes is not
/// cosmetic — it is the staleness rule silently ceasing to fire.
pub const MUTATING_TOOLS: &[&str] = &["write_file", "edit_file", "delete_file"];

/// Ceiling on digested paths. Matched to `RESTORE_MAX_FILES`'s reasoning one
/// tier up: a digest is one line per path rather than a file body, so it can
/// afford far more entries, but an unbounded list would let a thousand-read
/// session spend real context restating itself. Overflow is *named* in the
/// message, never silently elided.
pub const DIGEST_MAX_PATHS: usize = 32;

/// One read whose result has left context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestedRead {
    /// The path exactly as the recorded call spelled it.
    pub path: String,
    /// Which tool-bearing step the read happened at, 1-based.
    ///
    /// Derived from the transcript rather than threaded in from the driver:
    /// the digest covers reads from many steps, and the driver only knows the
    /// current one. Counting assistant messages that carry `tool_calls` is the
    /// same notion of "step" the retention pass counts against its horizon.
    pub step: usize,
}

/// Every read whose result has left context and is still safe to name,
/// oldest first, deduplicated by path.
///
/// A path qualifies when its **latest** read has a compacted result and no
/// [`MUTATING_TOOLS`] call named it at or after that read. Keying on the
/// latest read is what makes an early evicted read plus a later live re-read
/// a non-entry: the answer is already in context, so restating it is pure
/// cost.
#[must_use]
pub fn collect_digest(messages: &[CompletionMessage]) -> Vec<DigestedRead> {
    // Pass 1: correlate every call to the step it was made at, so a result
    // (which names only its `call_id`) can be resolved back to a path.
    let mut call_paths: HashMap<&str, (&str, String, usize)> = HashMap::new();
    let mut step = 0usize;
    // Latest read per path, and the step of the latest mutation per path.
    let mut latest_read: HashMap<String, (usize, Option<bool>)> = HashMap::new();
    let mut latest_mutation: HashMap<String, usize> = HashMap::new();

    for message in messages {
        if message.role == MessageRole::Assistant && !message.tool_calls.is_empty() {
            step += 1;
            for call in &message.tool_calls {
                let Some(path) = call_path(&call.input) else {
                    continue;
                };
                if call.name == READ_TOOL {
                    call_paths.insert(call.call_id.as_str(), (READ_TOOL, path.clone(), step));
                    // `None` until the matching result is seen: a read still
                    // awaiting its result is not a loss to report.
                    latest_read.insert(path, (step, None));
                } else if MUTATING_TOOLS.contains(&call.name.as_str()) {
                    let slot = latest_mutation.entry(path).or_insert(step);
                    *slot = step.max(*slot);
                }
            }
        }
        for result in &message.tool_results {
            let Some((_, path, read_step)) = call_paths.get(result.call_id.as_str()) else {
                continue;
            };
            // Only the latest read of this path decides; an older read's
            // result answering late cannot overwrite a newer read's verdict.
            if let Some((step_of_latest, verdict)) = latest_read.get_mut(path.as_str())
                && *step_of_latest == *read_step
            {
                *verdict = Some(super::is_compacted_output(&result.output));
            }
        }
    }

    let mut digested: Vec<DigestedRead> = latest_read
        .into_iter()
        .filter(|(path, (read_step, verdict))| {
            // Its result must have been seen AND been compacted away.
            verdict == &Some(true)
                // ...and nothing may have written the path since.
                && latest_mutation
                    .get(path.as_str())
                    .is_none_or(|mutated_at| mutated_at < read_step)
        })
        .map(|(path, (step, _))| DigestedRead { path, step })
        .collect();
    // Deterministic and oldest-first: byte-stability is a property of this
    // ordering, since a `HashMap` drain is not reproducible across runs.
    digested.sort_by(|a, b| a.step.cmp(&b.step).then_with(|| a.path.cmp(&b.path)));
    digested
}

/// The path argument of a recorded call, when it has one. Runtime data:
/// a call whose `path` is absent or not a string is skipped, never unwrapped.
fn call_path(input: &serde_json::Value) -> Option<String> {
    input
        .get(READ_PATH_PARAM)
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
}

/// Render the digest message, or `None` when there is nothing to say.
///
/// The wording is load-bearing in two directions: it must be strong enough
/// that the model prefers it to a re-read, and weak enough to stay true if the
/// file changed underneath by a route this module cannot see (see the module
/// docs' staleness section).
#[must_use]
pub fn render_digest(reads: &[DigestedRead]) -> Option<String> {
    if reads.is_empty() {
        return None;
    }
    let shown = reads.len().min(DIGEST_MAX_PATHS);
    // Newest reads are the ones still being reasoned over, so an overflowing
    // digest keeps the tail rather than the head.
    let kept = &reads[reads.len() - shown..];
    let mut out = String::from(READ_DIGEST_MARKER_PREFIX);
    out.push_str(
        "] you already read these files earlier in this session. Their output was \
                  dropped to fit context, but the read happened — do not re-read one to \
                  rediscover what you already concluded from it. Re-read only if you need the \
                  exact current bytes, or if something may have changed it since.\n",
    );
    for read in kept {
        out.push_str(&format!("- {} (read at step {})\n", read.path, read.step));
    }
    if reads.len() > shown {
        out.push_str(&format!(
            "[… {} older read{} not listed — the digest is capped at {DIGEST_MAX_PATHS} paths …]\n",
            reads.len() - shown,
            if reads.len() - shown == 1 { "" } else { "s" },
        ));
    }
    Some(out)
}

/// Put the current digest into `messages`, and report whether any byte moved.
///
/// Exactly one digest lives in the transcript: an existing one is rewritten in
/// place, and a fresh one is appended at the tail. A digest whose content is
/// unchanged is left completely alone — the caller relies on the returned
/// `bool` to know whether the prompt-cache prefix was disturbed.
///
/// Separate from [`super::compact_measured`] because that function takes a
/// `&mut [CompletionMessage]` slice and so cannot grow the transcript; the
/// driver owns the `Vec` and calls this immediately after the pass.
pub fn apply_digest(messages: &mut Vec<CompletionMessage>) -> bool {
    let Some(rendered) = render_digest(&collect_digest(messages)) else {
        return false;
    };
    if let Some(existing) = messages.iter_mut().find(|message| is_digest(message)) {
        if existing.content == rendered {
            return false;
        }
        existing.content = rendered;
        return true;
    }
    let message = CompletionMessage {
        role: MessageRole::User,
        content: rendered,
        tool_calls: Vec::new(),
        tool_results: Vec::new(),
        attachments: Vec::new(),
    };
    // Context precedes the question, exactly as `inject_recall_block` places
    // the recall block (`stella-cli`'s `memory/recall.rs`): landing after a
    // trailing user turn would put engine bookkeeping between the person's
    // words and the model's answer.
    let at = match messages.last() {
        Some(last)
            if last.role == MessageRole::User
                && !is_digest(last)
                && last.tool_results.is_empty() =>
        {
            messages.len() - 1
        }
        _ => messages.len(),
    };
    messages.insert(at, message);
    true
}

/// Whether this message is the engine's digest rather than the person
/// speaking. Prefix-matched, like every other [`crate::engine_markers`]
/// consumer.
#[must_use]
pub fn is_digest(message: &CompletionMessage) -> bool {
    message.role == MessageRole::User && message.content.starts_with(READ_DIGEST_MARKER_PREFIX)
}

#[cfg(test)]
mod tests;
