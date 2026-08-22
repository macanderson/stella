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

use stella_protocol::{CompletionMessage, MessageRole, ToolCall};

use crate::loop_detect::CallRecord;
use crate::receipts::TranscriptRevision;

use super::{CONTINUATION_MARKER_PREFIX, LOOP_STEER_PREFIX, SUMMARY_MARKER_PREFIX};

/// Pair the tool calls of the CURRENT turn — assistant messages after the
/// last user message — with the outputs they produced, in chronological
/// order, for `crate::loop_detect::detect_loop`. Windowing at the user
/// boundary matters: identical calls across turns are the user re-asking a
/// question, not a stuck loop (a REPL session asking the same thing three
/// times would otherwise trip the exact-repeat detector), and it keeps
/// this per-step scan O(turn) instead of O(entire history). The overflow
/// summary, the stuck-loop warning and the output-limit continuation nudge
/// are also User-role but are not real user turns — treating any of them as a
/// boundary would truncate the loop window (on every summarization pass, right
/// when re-detection needs the evidence, or on the one path that exists
/// precisely because the turn is *not* over), so all three are skipped when
/// locating the boundary.
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
/// Where the current turn's messages start within `messages`: the index
/// right after the last genuine user turn boundary, skipping the synthetic
/// User-role markers the driver injects mid-turn (the overflow summary, the
/// stuck-loop warning, the length-continuation nudge, the working-set
/// restoration) — none of those are a real user turn, and treating one as
/// the boundary would truncate the window right when a consumer needs the
/// fuller history (on every summarization pass, or on the one path that
/// exists precisely because the turn is not over).
///
/// Shared by [`recent_call_records`] (loop-detection evidence) and
/// `driver::confident_zero` (turn-activity evidence for #1477): both need
/// exactly this same window, and a second copy of the boundary rule would be
/// a second place for it to drift out of sync with the first.
pub(super) fn turn_start_index(messages: &[CompletionMessage]) -> usize {
    messages
        .iter()
        .rposition(|m| {
            m.role == MessageRole::User
                && !m.content.starts_with(SUMMARY_MARKER_PREFIX)
                && !m.content.starts_with(LOOP_STEER_PREFIX)
                && !m.content.starts_with(CONTINUATION_MARKER_PREFIX)
                && !m.content.starts_with(crate::restore::RESTORE_MARKER_PREFIX)
                // A recalled-context block is host-injected context, not a
                // user turn. Pre-turn blocks land BEFORE the prompt and never
                // decided this boundary; a proactive re-query (#3243 Phase 3)
                // injects one mid-turn, where treating it as the boundary
                // would silently reset loop-detection and confident-zero
                // evidence on every re-query.
                && !m.content.starts_with(crate::receipts::RECALL_MARKER)
        })
        .map(|i| i + 1)
        .unwrap_or(0)
}

/// The transcript evidence a steering re-query reads (#3243 Phase 3): the
/// prompt that opened this turn, the tools it has called (most recent last),
/// and the error classes it has seen — assembled fresh each step from the
/// same window [`turn_start_index`] gives loop detection, so the two planes
/// cannot disagree about what "this turn" means.
pub struct TurnEvidence {
    /// Index of the real user message that bounds this turn, when one exists.
    pub prompt_index: Option<usize>,
    /// Tool names in call order, most recent last.
    pub tool_names: Vec<String>,
    /// Stable error-class tokens for every failed tool result, in order. An
    /// unclassified failure reads as `"error"` — still evidence of being
    /// stuck, just not audited into a class yet.
    pub error_classes: Vec<&'static str>,
    /// Path-shaped tokens the turn's tool-call INPUTS named, first mention
    /// first, deduplicated and capped. Signal, never evidence — the same
    /// caveat `AgentEvent::FileChange` carries: a `bash` heredoc mutates the
    /// tree and names nothing here. Unlike a file ledger this includes a
    /// path the turn is about to *create*, which is exactly the anchor the
    /// prompt could not contribute (`TurnSignal::touched_paths`).
    pub touched_paths: Vec<String>,
}

/// Cap on [`TurnEvidence::touched_paths`]: enough to move a domain scope,
/// small enough that a pathological turn cannot turn every re-query into a
/// full-graph walk — the same reasoning as recall's own anchor cap.
const MAX_TOUCHED_PATHS: usize = 16;

pub fn turn_evidence(messages: &[CompletionMessage]) -> TurnEvidence {
    let start = turn_start_index(messages);
    let mut evidence = TurnEvidence {
        prompt_index: start.checked_sub(1),
        tool_names: Vec::new(),
        error_classes: Vec::new(),
        touched_paths: Vec::new(),
    };
    let mut seen_paths = std::collections::HashSet::new();
    for message in &messages[start..] {
        match message.role {
            MessageRole::Assistant => {
                for call in &message.tool_calls {
                    evidence.tool_names.push(call.name.clone());
                    collect_path_tokens(&call.input, &mut seen_paths, &mut evidence.touched_paths);
                }
            }
            MessageRole::Tool => {
                for result in &message.tool_results {
                    if let stella_protocol::ToolOutput::Error { class, .. } = &result.output {
                        evidence.error_classes.push(
                            class
                                .map(stella_protocol::ErrorClass::as_str)
                                .unwrap_or("error"),
                        );
                    }
                }
            }
            _ => {}
        }
    }
    evidence
}

/// Walk one tool input for path-shaped string values: a token qualifies with
/// a `/` or an interior `.` (a file name), escapes (`..`) and absolute paths
/// are rejected, and a `./` prefix is normalized off — the same shape rule
/// the CLI's record selector applies to prompts. Only whole string values
/// are considered, never substrings of prose: a tool input's `path`-like
/// field IS the path, while free text inside a `command` string is noise
/// this deliberately under-collects. (`bash -c "cat a.rs"` contributes
/// nothing — signal, not evidence.)
fn collect_path_tokens(
    input: &serde_json::Value,
    seen: &mut std::collections::HashSet<String>,
    out: &mut Vec<String>,
) {
    if out.len() >= MAX_TOUCHED_PATHS {
        return;
    }
    match input {
        serde_json::Value::String(s) => {
            let token = s.strip_prefix("./").unwrap_or(s);
            let path_shaped = token.contains('/')
                || token
                    .find('.')
                    .is_some_and(|at| at > 0 && at + 1 < token.len());
            if path_shaped
                && !token.contains(char::is_whitespace)
                && token.len() <= 256
                && !token.starts_with('/')
                && !token.split('/').any(|seg| seg == "..")
                && seen.insert(token.to_string())
            {
                out.push(token.to_string());
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_path_tokens(item, seen, out);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values() {
                collect_path_tokens(value, seen, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Witness (#3243 Phase 3).** A mid-turn recalled-context block is not
    /// a turn boundary. Fails before the `RECALL_MARKER` exclusion: the
    /// marker message is User-role, so `rposition` picked it and the window
    /// silently lost everything before the re-query — loop detection and
    /// confident-zero evidence reset on every injection.
    #[test]
    fn a_mid_turn_recall_block_does_not_reset_the_turn_window() {
        let mut assistant = CompletionMessage::assistant("working");
        assistant.tool_calls = vec![];
        let messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("fix the flaky test"),
            assistant.clone(),
            CompletionMessage::user(format!(
                "{}\n\nRelevant context:\n- freshly recalled",
                crate::receipts::RECALL_MARKER
            )),
            assistant,
        ];

        assert_eq!(
            turn_start_index(&messages),
            2,
            "the prompt, not the injected recall block, bounds the turn window"
        );
    }

    /// The evidence walk reads whole path-shaped string values out of tool
    /// inputs — a `path` field is a path; prose (whitespace) and escapes are
    /// not — and includes a file the turn is about to create.
    #[test]
    fn touched_paths_come_from_tool_inputs_whole_values_only() {
        let mut call = CompletionMessage::assistant("");
        call.tool_calls = vec![stella_protocol::ToolCall {
            call_id: "c1".into(),
            name: "write_file".into(),
            input: serde_json::json!({
                "path": "crates/stella-model/src/mistral.rs",
                "content": "run cat ./a.rs first",
                "escape": "../etc/passwd",
                "absolute": "/etc/hosts",
            }),
        }];
        let messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("add a mistral adapter"),
            call,
        ];

        let evidence = turn_evidence(&messages);

        assert_eq!(
            evidence.touched_paths,
            vec!["crates/stella-model/src/mistral.rs".to_string()],
            "one whole path-shaped value; prose, escapes and absolutes drop"
        );
        assert_eq!(evidence.tool_names, vec!["write_file".to_string()]);
    }
}

pub(super) fn recent_call_records<'a>(
    messages: &'a [CompletionMessage],
    identities: &ResultIdentities,
) -> Vec<CallRecord<'a>> {
    let turn_start = turn_start_index(messages);
    let mut records: Vec<CallRecord> = Vec::new();
    for message in &messages[turn_start..] {
        match message.role {
            MessageRole::Assistant => {
                // Borrowed, not cloned: this window is rebuilt every step, and
                // an owned call deep-copied every `edit_file` input's file
                // chunks once per step — quadratic across a long turn (the
                // same cost the output's `Cow` exists to avoid).
                records.extend(message.tool_calls.iter().map(|call| CallRecord {
                    call: Cow::Borrowed(call),
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
                        // Borrowed, never copied: this walk runs over the
                        // whole transcript on every step.
                        record.output = Some(Cow::Borrowed(&result.output));
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
/// Name and input are exactly the fields `loop_detect::same_record` already
/// requires to match before two records are "the same", so widening the key
/// with them cannot merge two calls the detector would have distinguished. It
/// only stops two UNRELATED calls that happened to share an ordinal from being
/// treated as one.
///
/// Deliberately not positional: [`Engine::apply_overflow_summary`](super::Engine::apply_overflow_summary) splices a
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

/// Opens `read_file`'s trailing footer, on its own line after the payload's
/// last newline.
///
/// # One definition, two crates
///
/// The footer is written by `stella_tools::read` and read back by
/// [`comparable_output`] below, across a crate boundary — the producer
/// depends on `stella-core`, so this module is the only place both sides can
/// name. These five fragments are that shared definition, and
/// `the_footer_a_read_writes_is_the_one_loop_comparison_strips` in
/// `stella-tools` is what fails if either side stops using them: a reworded
/// footer no longer stripped here is not a cosmetic change, it is loop
/// detection going blind on `read_file` (see [`comparable_output`]).
pub const READ_FOOTER_OPEN: &str = "\n(";

/// Closes the footer, after every clause it carries.
pub const READ_FOOTER_CLOSE: &str = ")";

/// Joins the footer's clauses. The tally is only the first; a read that
/// clipped a long line or stopped at the payload cap appends more.
pub const READ_FOOTER_CLAUSE_SEP: &str = " \u{b7} ";

/// Sits between the shown/total line counts and the per-session read tally:
/// `{shown}/{total}` + this + `{reads}` + [`READ_FOOTER_TALLY_END`].
pub const READ_FOOTER_TALLY_MID: &str = " lines shown \u{b7} read ";

/// Ends the tally clause — the volatile part, and the reason the whole footer
/// has to come off before two reads can be compared.
pub const READ_FOOTER_TALLY_END: &str = "\u{d7} this session";

/// Normalize one tool output for loop comparison: strip `read_file`'s
/// volatile session-tally footer (`\n\n(N/M lines shown · read K× this
/// session[ · …][ · …])`). The tally changes on EVERY read by design — it is
/// the model-facing "you already read this" nudge — but the loop detector
/// requires byte-identical outputs, so the footer made every reread unique
/// and blinded detection to the exact thrash it exists to catch (the
/// read → failing-edit → read cycle the `loop_detect` module doc names).
/// Comparison-only: the transcript and history keep the footer untouched.
///
/// Anchored on the footer's opening paren rather than on the tally's own last
/// byte, because the tally is only the FIRST clause: a read that clipped a
/// long line or stopped at the payload cap appends further `·` clauses before
/// the close. Ending the match at the tally would leave the volatile count in
/// the compared bytes for precisely the large-file rereads a stuck agent
/// produces most.
///
/// Borrowed on the overwhelmingly common path (no footer): both callers run
/// over the WHOLE transcript on every step, so returning an owned copy meant a
/// full heap copy of every tool result — a step's worth of garbage proportional
/// to the entire history, for a normalization that usually changes nothing.
pub fn comparable_output(
    output: &stella_protocol::ToolOutput,
) -> Cow<'_, stella_protocol::ToolOutput> {
    if let stella_protocol::ToolOutput::Ok { content, data } = output
        && let Some(idx) = read_footer_start(content)
    {
        return Cow::Owned(stella_protocol::ToolOutput::Ok {
            content: content[..idx].to_string(),
            data: data.clone(),
        });
    }
    Cow::Borrowed(output)
}

/// Where `read_file`'s footer begins in `content`, or `None` when the output
/// does not carry one.
///
/// The footer's own bytes contain no [`READ_FOOTER_OPEN`], so the last one in
/// a payload that ends with [`READ_FOOTER_CLOSE`] and carries the tally is
/// the footer's — a file line that merely starts with `(` sits earlier and is
/// never selected.
fn read_footer_start(content: &str) -> Option<usize> {
    if !content.ends_with(READ_FOOTER_CLOSE) {
        return None;
    }
    let start = content.rfind(READ_FOOTER_OPEN)?;
    let footer = &content[start..];
    (footer.contains(READ_FOOTER_TALLY_MID) && footer.contains(READ_FOOTER_TALLY_END))
        .then_some(start)
}
