//! The compaction test suite — every pass, the retention gate, and the
//! elision helpers. A child module of `compaction` so the pass internals
//! stay reachable, split out to keep `compaction.rs` under the size gate.

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

/// A transcript long enough that `count` tool-bearing steps sit behind
/// the newest one, each carrying one `size`-byte output.
fn long_turn(count: usize, size: usize) -> Vec<CompletionMessage> {
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("do things"),
    ];
    for i in 0..count {
        let id = format!("c{i}");
        messages.push(assistant_with_call(&id));
        messages.push(tool_msg(
            &id,
            format!("STEP{i}-HEAD\n{}\nSTEP{i}-TAIL", "x".repeat(size)),
        ));
    }
    messages
}

#[test]
fn retention_ages_results_past_the_horizon_with_no_budget_pressure() {
    // The #1285 witness: below budget the old passes did NOTHING, so a
    // long turn re-sent every old tool output verbatim on every step
    // until the transcript crossed ~100k tokens. Pass 0 must age results
    // older than the horizon even under an effectively infinite budget.
    let mut messages = long_turn(12, 5_000);
    let (_, report) = compact_measured(
        &mut messages,
        u64::MAX,
        Some(RetentionPolicy {
            keep_recent_steps: 4,
        }),
    );
    let report = report.expect("retention must report the blocks it aged");
    // 12 tool steps, horizon 4 → 8 stale results, all above the age
    // threshold.
    assert_eq!(report.aged, 8, "{report:?}");
    assert_eq!(report.aged_blocks.len(), 8);
    assert!(report.after_tokens < report.before_tokens);
    assert_eq!(report.evicted, 0, "retention never drops whole outputs");
    // The oldest result is aged to head+tail…
    match &messages[3].tool_results[0].output {
        ToolOutput::Ok { content } => {
            assert!(content.starts_with("STEP0-HEAD"), "head lost");
            assert!(content.ends_with("STEP0-TAIL"), "tail lost");
            assert!(content.contains("middle elided"));
        }
        _ => panic!("expected aged content"),
    }
    // …and every result inside the horizon is untouched.
    for i in 8..12 {
        match &messages[3 + 2 * i].tool_results[0].output {
            ToolOutput::Ok { content } => assert!(
                content.len() > 5_000,
                "recent step {i} must keep its verbatim output"
            ),
            _ => panic!("recent result must be intact"),
        }
    }
}

#[test]
fn retention_reports_the_block_identity_the_manifest_cited() {
    // §6.2 for pass 0: the aged block is named by its PRE-mutation id.
    let mut messages = long_turn(6, 5_000);
    let expected = tool_result_block_id(&messages[3].tool_results[0].output);
    let (_, report) = compact_measured(
        &mut messages,
        u64::MAX,
        Some(RetentionPolicy {
            keep_recent_steps: 1,
        }),
    );
    let report = report.expect("should age");
    assert!(
        report.aged_blocks.contains(&expected),
        "aged_blocks {:?} must cite the original id {expected}",
        report.aged_blocks
    );
}

#[test]
fn retention_waits_for_a_batch_before_touching_the_prefix() {
    // Cache-prefix discipline (invariant 7): aging one result the moment
    // it crosses the horizon would rewrite the prefix on every step.
    // Below RETENTION_MIN_RECLAIM_CHARS of reclaimable bytes, nothing
    // moves.
    let mut messages = long_turn(5, 5_000);
    // Horizon leaves 3 stale results reclaiming ~10 KB: below the floor.
    let (_, report) = compact_measured(
        &mut messages,
        u64::MAX,
        Some(RetentionPolicy {
            keep_recent_steps: 2,
        }),
    );
    assert!(
        report.is_none(),
        "below the reclaim floor the transcript must stay byte-stable: {report:?}"
    );
}

#[test]
fn retention_fires_on_reclaimable_bytes_before_any_count_floor() {
    // The gate measures bytes, not results: two 100 KB outputs past the
    // horizon are ~2.1M re-sent input tokens over a 25-step turn if the
    // pass waits for more of them, so they must age as soon as the
    // reclaim pays for the prefix rewrite — a count gate (the original
    // four-result floor) held them verbatim for the rest of the turn.
    let mut messages = long_turn(4, 100_000);
    let (_, report) = compact_measured(
        &mut messages,
        u64::MAX,
        Some(RetentionPolicy {
            keep_recent_steps: 2,
        }),
    );
    let report = report.expect("two 100 KB stale results must age");
    assert_eq!(report.aged, 2, "{report:?}");
}

#[test]
fn retention_skips_a_trickle_of_barely_ageable_results() {
    // The mirror image: many results only just over AGE_THRESHOLD_CHARS
    // reclaim almost nothing — the original count gate fired a
    // cache-invalidating prefix rewrite for under 4 KB back. The bytes
    // gate must leave the transcript byte-stable instead.
    let mut messages = long_turn(12, 2_100);
    let (_, report) = compact_measured(
        &mut messages,
        u64::MAX,
        Some(RetentionPolicy {
            keep_recent_steps: 4,
        }),
    );
    assert!(
        report.is_none(),
        "a reclaim under the floor must not buy a prefix rewrite: {report:?}"
    );
}

#[test]
fn retention_is_idempotent_between_batches() {
    // After a batch fires, aged results are below the threshold, so an
    // immediately following pass finds no candidates and mutates nothing
    // — the prefix stays byte-stable until a NEW batch accumulates.
    let mut messages = long_turn(12, 5_000);
    let policy = Some(RetentionPolicy {
        keep_recent_steps: 4,
    });
    let (_, first) = compact_measured(&mut messages, u64::MAX, policy);
    assert!(first.is_some());
    let snapshot: Vec<String> = messages.iter().map(|m| format!("{m:?}")).collect();
    let (_, second) = compact_measured(&mut messages, u64::MAX, policy);
    assert!(second.is_none(), "second pass must be a no-op: {second:?}");
    let after: Vec<String> = messages.iter().map(|m| format!("{m:?}")).collect();
    assert_eq!(snapshot, after, "no bytes may move between batches");
}

#[test]
fn retention_never_touches_the_newest_tool_message_even_at_horizon_zero() {
    let mut messages = long_turn(6, 5_000);
    let (_, _) = compact_measured(
        &mut messages,
        u64::MAX,
        Some(RetentionPolicy {
            keep_recent_steps: 0,
        }),
    );
    match &messages[13].tool_results[0].output {
        ToolOutput::Ok { content } => assert!(
            content.len() > 5_000,
            "the result answering the latest call survives every pass"
        ),
        _ => panic!("latest result must be intact"),
    }
}

#[test]
fn small_recent_and_already_aged_results_are_not_retention_candidates() {
    // Below AGE_THRESHOLD_CHARS there is nothing worth reclaiming, so a
    // long turn of small outputs never triggers the batch — and never
    // churns the cache prefix.
    let mut messages = long_turn(20, 100);
    let (_, report) = compact_measured(
        &mut messages,
        u64::MAX,
        Some(RetentionPolicy {
            keep_recent_steps: 2,
        }),
    );
    assert!(report.is_none(), "{report:?}");
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
    // (`crates/stella-model/src/gemini.rs`), so `call_0` comes back on every
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
    let (after, report) = compact_measured(&mut messages, budget, None);
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
    let (_, report) = compact_measured(&mut messages, budget, None);
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
