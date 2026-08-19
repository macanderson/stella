// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Witnesses for the already-read digest (#3806).
//!
//! The two the issue names — a compacted read IS digested, and a path written
//! after its read is NOT — plus the byte-stability and idempotence properties
//! that make the digest safe to run before every model call.

use serde_json::json;
use stella_protocol::{CompletionMessage, MessageRole, ToolCall, ToolOutput, ToolResult};

use super::*;
use crate::compaction::{RetentionPolicy, compact_and_digest};

/// An assistant turn making one call.
fn calls(name: &str, call_id: &str, path: &str) -> CompletionMessage {
    CompletionMessage {
        role: MessageRole::Assistant,
        content: String::new(),
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: name.to_string(),
            input: json!({ "path": path }),
        }],
        tool_results: Vec::new(),
        attachments: Vec::new(),
    }
}

/// The matching tool result.
fn result(call_id: &str, content: &str) -> CompletionMessage {
    CompletionMessage {
        role: MessageRole::Tool,
        content: String::new(),
        tool_calls: Vec::new(),
        tool_results: vec![ToolResult {
            call_id: call_id.to_string(),
            output: ToolOutput::Ok {
                content: content.to_string(),
                data: None,
            },
        }],
        attachments: Vec::new(),
    }
}

/// A transcript big enough that the budget passes must evict, built from
/// `reads` as (call_id, path) pairs each answered with a large payload.
fn transcript(reads: &[(&str, &str)]) -> Vec<CompletionMessage> {
    let mut messages = vec![
        CompletionMessage::system("stable prefix"),
        CompletionMessage::user("do the thing"),
    ];
    for (call_id, path) in reads {
        messages.push(calls(READ_TOOL, call_id, path));
        messages.push(result(call_id, &"x".repeat(40_000)));
    }
    messages
}

/// **The issue's first witness.** Drive a transcript that reads a file and
/// compacts, and assert the digest names that path.
///
/// Fails on the old code for the reason #3806 was filed: compaction stubbed
/// the result and left `EVICTION_STUB`'s "re-run the tool" as the only trace,
/// so nothing in the transcript stated the path as current knowledge.
#[test]
fn a_read_whose_result_compaction_evicted_is_named_by_the_digest() {
    let mut messages = transcript(&[("c1", "src/alpha.rs"), ("c2", "src/beta.rs")]);

    let (_after, report) = compact_and_digest(&mut messages, 1_000, None);
    assert!(
        report.is_some(),
        "the budget must be low enough that a pass actually stubbed something, \
         or this test proves nothing about what compaction destroys"
    );

    let digest = messages
        .iter()
        .find(|message| is_digest(message))
        .expect("compaction evicted a read result, so a digest must state what was lost");
    assert!(
        digest.content.contains("src/alpha.rs"),
        "the evicted read's path must be named; got: {}",
        digest.content
    );
    assert_eq!(
        digest.role,
        MessageRole::User,
        "the digest rides as an engine-authored User turn, like every other engine marker"
    );
}

/// **The issue's second witness.** A path written after its read must not be
/// digested — the staleness trap, where a wrong answer is strictly worse than
/// the redundant read the digest exists to save.
#[test]
fn a_path_written_after_its_read_is_absent_from_the_digest() {
    let mut messages = transcript(&[("c1", "src/edited.rs"), ("c2", "src/untouched.rs")]);
    // The write lands after both reads, so only `src/edited.rs` is invalidated.
    messages.push(calls("edit_file", "c3", "src/edited.rs"));
    messages.push(result("c3", "edited"));

    compact_and_digest(&mut messages, 1_000, None);

    let digest = messages
        .iter()
        .find(|message| is_digest(message))
        .expect("the untouched read is still digestible, so a digest must exist");
    assert!(
        !digest.content.contains("src/edited.rs"),
        "a path edited after it was read must not be stated as already known; got: {}",
        digest.content
    );
    assert!(
        digest.content.contains("src/untouched.rs"),
        "invalidating one path must not silently drop the others; got: {}",
        digest.content
    );
}

/// Every mutating file tool invalidates, not just the one the test above used.
#[test]
fn every_mutating_file_tool_invalidates_a_prior_read() {
    for tool in MUTATING_TOOLS {
        // A second read that nothing writes, so a digest is definitely
        // produced: without it the assertion below would hold vacuously on a
        // build where the digest never fires at all.
        let mut messages = transcript(&[("c1", "src/target.rs"), ("c2", "src/witness.rs")]);
        messages.push(calls(tool, "c3", "src/target.rs"));
        messages.push(result("c3", "done"));

        compact_and_digest(&mut messages, 1_000, None);

        let digest = messages
            .iter()
            .find(|message| is_digest(message))
            .unwrap_or_else(|| panic!("{tool}: the untouched read must still produce a digest"));
        assert!(
            digest.content.contains("src/witness.rs"),
            "{tool}: anti-vacuity — the digest must actually be naming paths"
        );
        assert!(
            !digest.content.contains("src/target.rs"),
            "{tool} must invalidate the digest entry for the path it wrote"
        );
    }
}

/// A read whose result is still in context is not a loss, so restating it
/// would spend the very bytes compaction just reclaimed.
#[test]
fn a_read_still_holding_its_result_is_not_digested() {
    let mut messages = vec![
        CompletionMessage::system("stable prefix"),
        CompletionMessage::user("do the thing"),
        calls(READ_TOOL, "c1", "src/live.rs"),
        result("c1", "fn main() {}"),
    ];
    // A budget far above the transcript: no pass fires, nothing is lost.
    let (_after, report) = compact_and_digest(&mut messages, 1_000_000, None);
    assert!(report.is_none(), "nothing should have been compacted here");
    assert!(
        !messages.iter().any(is_digest),
        "with every read result still in context there is nothing to digest"
    );
}

/// Re-reading a path after an early eviction puts the answer back in context,
/// so the digest must drop it: the latest read decides, not the earliest.
#[test]
fn a_live_re_read_supersedes_an_earlier_evicted_read_of_the_same_path() {
    let mut messages = transcript(&[("c1", "src/alpha.rs"), ("c2", "src/beta.rs")]);
    compact_and_digest(&mut messages, 1_000, None);
    assert!(
        messages
            .iter()
            .any(|m| is_digest(m) && m.content.contains("src/alpha.rs")),
        "precondition: alpha was evicted and digested"
    );

    // Now read it again, and let the result stand.
    messages.push(calls(READ_TOOL, "c9", "src/alpha.rs"));
    messages.push(result("c9", "fn main() {}"));
    assert!(
        !collect_digest(&messages)
            .iter()
            .any(|read| read.path == "src/alpha.rs"),
        "the fresh result is in context, so naming the path again is pure cost"
    );
}

/// **Invariant 7.** The stable prefix must be byte-identical across a
/// compaction pass that emits a digest — the digest rides the volatile tail,
/// never the cached prefix. (`docs`: `AGENTS.md` § Architecture, invariant 7.)
#[test]
fn the_digest_never_disturbs_the_byte_stable_prefix() {
    let mut messages = transcript(&[("c1", "src/alpha.rs"), ("c2", "src/beta.rs")]);
    let system_before = messages[0].content.clone();
    let user_before = messages[1].content.clone();

    compact_and_digest(&mut messages, 1_000, None);

    assert_eq!(
        messages[0].content, system_before,
        "the system message is the prompt-cache prefix and must not move a byte"
    );
    assert_eq!(
        messages[0].role,
        MessageRole::System,
        "the digest must never be inserted ahead of the system message"
    );
    assert_eq!(
        messages[1].content, user_before,
        "the user's goal must keep its position and bytes"
    );
}

/// The hysteresis that makes this safe to call before every model call: a
/// second pass concluding the same thing writes nothing, so it costs no
/// prompt-cache hit and the digest cannot accumulate copies of itself.
#[test]
fn re_applying_an_unchanged_digest_is_a_no_op() {
    // Two reads: compaction never evicts a result whose call is still the most
    // recent one, so a single-read transcript loses nothing to digest.
    let mut messages = transcript(&[("c1", "src/alpha.rs"), ("c2", "src/beta.rs")]);
    compact_and_digest(&mut messages, 1_000, None);
    let after_first = messages.clone();

    assert!(
        !apply_digest(&mut messages),
        "an unchanged digest must report that it moved nothing"
    );
    assert_eq!(
        messages, after_first,
        "and must in fact move nothing — a second copy would restate the same \
         paths every round and grow without bound"
    );
    assert_eq!(
        messages.iter().filter(|m| is_digest(m)).count(),
        1,
        "exactly one digest lives in the transcript"
    );
}

/// A later pass rewrites the existing digest **in place** rather than
/// appending a second one.
#[test]
fn a_changed_digest_is_rewritten_in_place() {
    let mut messages = transcript(&[("c1", "src/alpha.rs"), ("c2", "src/beta.rs")]);
    compact_and_digest(&mut messages, 1_000, None);
    let position = messages
        .iter()
        .position(is_digest)
        .expect("first pass digested");

    // A third read, evicted by a second pass.
    messages.push(calls(READ_TOOL, "c3", "src/gamma.rs"));
    messages.push(result("c3", &"y".repeat(40_000)));
    compact_and_digest(&mut messages, 1_000, None);

    assert_eq!(
        messages.iter().filter(|m| is_digest(m)).count(),
        1,
        "a second digest message must never be appended beside the first"
    );
    assert_eq!(
        messages.iter().position(is_digest),
        Some(position),
        "the digest keeps its position, so only its own bytes change"
    );
    assert!(
        messages[position].content.contains("src/beta.rs"),
        // `gamma` is now the most recent read and so is never evicted; `beta`
        // is the one the second pass newly cost the model.
        "and its content is refreshed with the read the second pass evicted"
    );
}

/// Ordering is deterministic — a `HashMap` drain is not reproducible across
/// runs, and a digest whose line order shuffled would churn the tail bytes
/// (and the cache) on every pass for no reason.
#[test]
fn digest_ordering_is_deterministic_and_oldest_first() {
    let mut messages = transcript(&[
        ("c1", "src/alpha.rs"),
        ("c2", "src/beta.rs"),
        ("c3", "src/gamma.rs"),
    ]);
    compact_and_digest(&mut messages, 1_000, None);
    let rendered = messages
        .iter()
        .find(|m| is_digest(m))
        .expect("digest exists")
        .content
        .clone();

    let steps: Vec<usize> = collect_digest(&messages)
        .iter()
        .map(|read| read.step)
        .collect();
    let mut sorted = steps.clone();
    sorted.sort_unstable();
    assert_eq!(steps, sorted, "reads are stated oldest first");

    for _ in 0..8 {
        let mut again = messages.clone();
        assert!(!apply_digest(&mut again), "stable across repeated renders");
        assert_eq!(
            again.iter().find(|m| is_digest(m)).unwrap().content,
            rendered,
            "the same transcript must render the same bytes every time"
        );
    }
}

/// Overflow is named, never silently elided — the same discipline
/// `restore.rs` applies to the files it cannot fit.
#[test]
fn an_overflowing_digest_names_what_it_dropped() {
    let reads: Vec<DigestedRead> = (0..DIGEST_MAX_PATHS + 3)
        .map(|n| DigestedRead {
            path: format!("src/file{n}.rs"),
            step: n + 1,
        })
        .collect();
    let rendered = render_digest(&reads).expect("a non-empty set renders");

    assert_eq!(
        rendered.lines().filter(|l| l.starts_with("- ")).count(),
        DIGEST_MAX_PATHS,
        "the cap bounds what the digest spends"
    );
    assert!(
        rendered.contains("3 older reads not listed"),
        "the drop is stated, so a reader is never misled into thinking the \
         list is exhaustive; got: {rendered}"
    );
    assert!(
        rendered.contains(&format!("src/file{}.rs", DIGEST_MAX_PATHS + 2)),
        "the newest reads survive the cap — they are the ones still being reasoned over"
    );
}

/// Nothing read means nothing said: an empty digest is no message at all,
/// not an empty one taking up context.
#[test]
fn an_empty_digest_renders_nothing() {
    assert!(render_digest(&[]).is_none());
    let mut messages = vec![CompletionMessage::system("prefix")];
    assert!(!apply_digest(&mut messages));
    assert_eq!(messages.len(), 1);
}

/// Runtime data is never unwrapped: a call whose `path` is missing, empty, or
/// not a string is skipped rather than panicking (invariant 5).
#[test]
fn a_malformed_call_input_is_skipped_not_unwrapped() {
    for input in [json!({}), json!({ "path": 42 }), json!({ "path": "" }), json!(null)] {
        let messages = vec![
            CompletionMessage::system("prefix"),
            CompletionMessage {
                role: MessageRole::Assistant,
                content: String::new(),
                tool_calls: vec![ToolCall {
                    call_id: "c1".into(),
                    name: READ_TOOL.to_string(),
                    input,
                }],
                tool_results: Vec::new(),
                attachments: Vec::new(),
            },
            result("c1", "whatever"),
        ];
        assert!(
            collect_digest(&messages).is_empty(),
            "a call with no usable path contributes nothing"
        );
    }
}

/// The digest survives the retention pass path too — pass 0 ages results
/// regardless of the budget, and an aged result is just as lost to the model.
#[test]
fn a_result_aged_by_the_retention_pass_is_digested() {
    let mut messages = transcript(&[
        ("c1", "src/old.rs"),
        ("c2", "src/mid.rs"),
        ("c3", "src/new.rs"),
    ]);
    // A budget nothing breaches, so only the age-based pass can fire.
    compact_and_digest(
        &mut messages,
        u64::MAX,
        Some(RetentionPolicy { keep_recent_steps: 1 }),
    );
    assert!(
        messages
            .iter()
            .any(|m| is_digest(m) && m.content.contains("src/old.rs")),
        "aging past the horizon loses the output just as eviction does"
    );
}

/// The digest is classified as engine text, never as the person speaking.
#[test]
fn the_digest_is_registered_as_an_engine_marker() {
    assert!(
        crate::engine_markers::ENGINE_MARKERS.contains(&READ_DIGEST_MARKER_PREFIX),
        "a marker missing from the table is one the prompt never teaches and \
         the receipt attributes to the user"
    );
}

/// The tool ids this module keys on are the catalog's, and a rename that
/// silently stopped the staleness rule from firing is not cosmetic.
#[test]
fn the_tool_ids_match_the_catalog_spelling() {
    assert_eq!(READ_TOOL, "read_file");
    assert_eq!(MUTATING_TOOLS, &["write_file", "edit_file", "delete_file"]);
}
