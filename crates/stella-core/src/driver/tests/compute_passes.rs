//! Guards on how much redundant work one step does.
//!
//! These assert *pass counts*, not wall clock. Each whole-transcript walk and
//! each transcript-wide hash is Θ(history), so on a long turn the difference
//! between two of them per step and four is the difference between two and four
//! full re-reads of everything the model has seen — a cost that grows with the
//! turn while the per-pass constant stays flat. Shaving the constant inside a
//! pass cannot fix that; removing the pass can. A counter is the only way to
//! keep it removed.

use super::*;
use std::collections::HashMap;

/// How many times a single step re-reads the whole transcript to estimate it.
///
/// Four walks per step was the state of things: `compact`'s `before_tokens`, the
/// `else`-arm re-estimate that recovered the number `compact` had just discarded,
/// `run_model_call`'s pre-call estimate, and `emit_step_receipt` recomputing that
/// same pre-call estimate from the same unmutated slice. Two of the four were
/// pure recomputation of a value already in hand.
///
/// Each walk is Θ(transcript), so this is not a micro-cost: on a 200-step turn it
/// is the difference between two and four full re-reads of the entire history
/// before every model call. The remaining two are the honest ones — compaction
/// must measure before deciding, and the step must know its input size — and this
/// asserts the count so a future refactor cannot quietly reintroduce a third.
#[tokio::test]
async fn a_step_walks_the_transcript_to_estimate_it_at_most_twice() {
    const STEPS: usize = 3;
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(tool_call_result("call_1", "bash")),
            Ok(tool_call_result("call_2", "bash")),
            Ok(text_result("done")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let _ = crate::estimator::take_conversation_walks();
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    let walks = crate::estimator::take_conversation_walks();
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));

    // The fixture stays far under budget, so no compaction pass fires and no
    // overflow summary runs — every walk counted here is on the plain step path.
    let compactions = drain_events(&mut rx)
        .iter()
        .filter(|e| matches!(e, AgentEvent::Compaction { .. }))
        .count();
    assert_eq!(compactions, 0, "fixture must not compact");
    assert!(
        walks <= 2 * STEPS,
        "{STEPS} steps performed {walks} whole-transcript estimate walks; at most \
         two per step are load-bearing (compaction's measurement and the step's \
         own input size). The other two were recomputations of a number the \
         caller already had."
    );
}

/// The receipt's estimate and the usage record's estimate must be the same
/// number for the same step.
///
/// They used to be produced by two independent `estimate_conversation_tokens`
/// walks over the same unmutated slice, one in the driver and one inside
/// `ReceiptLedger::emit_step_receipt` — agreeing only because both happened to
/// call the same function on the same input. The driver now passes its value in,
/// so agreement is structural; this pins it, because a future refactor that
/// re-derives the manifest's estimate from something else (post-compaction
/// messages, a calibrated figure) would silently desynchronize the pair that
/// `StepUsage`'s drift sampling compares.
#[tokio::test]
async fn the_receipt_and_the_usage_record_report_one_estimate_per_step() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(tool_call_result("call_1", "bash")),
            Ok(text_result("done")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));

    // Pair them by step so a missing or extra event fails rather than lines up
    // by accident.
    let mut manifest: HashMap<usize, u64> = HashMap::new();
    let mut usage: HashMap<usize, u64> = HashMap::new();
    for event in drain_events(&mut rx) {
        match event {
            AgentEvent::StepManifest {
                step,
                estimated_input_tokens,
                ..
            } => {
                manifest.insert(step, estimated_input_tokens);
            }
            AgentEvent::StepUsage {
                step,
                estimated_input_tokens,
                ..
            } => {
                usage.insert(step, estimated_input_tokens);
            }
            _ => {}
        }
    }
    assert_eq!(manifest.len(), 2, "one manifest per committed step");
    assert_eq!(
        manifest, usage,
        "the manifest's estimate and the usage record's estimate are the same \
         measurement of the same step and must not drift apart"
    );
    assert!(manifest.values().all(|&e| e > 0));
}

/// How the per-step hashing scales with the length of the turn.
///
/// `decompose` re-derived every block's `block_id` AND `content_digest` on every
/// step — two SHA-256 passes over the block's bytes, plus a `serde_json`
/// re-serialization of every tool call and tool result just to obtain the
/// preimage. Since the manifest names every block each step, that made the total
/// hashing quadratic in the turn's length: step k re-hashed all of step k-1's
/// blocks. `snapshot_result_identities` added a third pass over every tool result
/// in the transcript, every step, for the same reason.
///
/// A block's bytes do not change unless something rewrites them, so the memo
/// makes the total linear. This asserts the SHAPE rather than a magic number:
/// doubling the steps must not come close to quadrupling the hashing.
#[tokio::test]
async fn per_step_hashing_grows_with_the_turn_not_with_its_square() {
    async fn hashes_for(steps: usize) -> usize {
        let mut script: Vec<_> = (0..steps)
            .map(|i| {
                // Distinct inputs: three byte-identical calls are what
                // `loop_detect` aborts on, and this test is about hashing.
                let mut r = tool_call_result(&format!("call_{i}"), "bash");
                if let Some(call) = r.tool_calls.first_mut() {
                    call.input = serde_json::json!({ "cmd": format!("echo {i}") });
                }
                Ok(r)
            })
            .collect();
        script.push(Ok(text_result("done")));
        let provider = ScriptedProvider {
            id: "scripted".into(),
            script: TokioMutex::new(script),
            calls: Arc::new(AtomicU32::new(0)),
        };
        // Answers each command differently. `CountingTools` replies "ok" to
        // everything, which — with the distinct inputs above — is the
        // stagnation signature `loop_detect` aborts on: the arguments moved
        // and the answer never did. This test is about hashing, so the
        // fixture has to make progress the way real work does.
        struct EchoingTools;
        #[async_trait]
        impl ToolExecutor for EchoingTools {
            fn schemas(&self) -> Vec<ToolSchema> {
                vec![ToolSchema {
                    name: "bash".into(),
                    description: "run a command".into(),
                    input_schema: serde_json::json!({"type": "object"}),
                    read_only: false,
                    speculation_safe: false,
                }]
            }
            async fn execute(&self, _name: &str, input: &Value) -> ToolOutput {
                ToolOutput::Ok {
                    content: format!("ran {input}"),
                }
            }
        }
        let tools = EchoingTools;
        let sleeper = NoopSleeper;
        let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
        let mut messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("hi"),
        ];
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        let (tx, _rx) = mpsc::unbounded_channel();

        let _ = crate::receipts::take_hash_passes();
        let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
        assert!(matches!(outcome, TurnOutcome::Completed { .. }));
        crate::receipts::take_hash_passes()
    }

    let short = hashes_for(4).await;
    let long = hashes_for(8).await;
    eprintln!(
        "DBG short={short} long={long} ratio={:.2}",
        long as f64 / short as f64
    );

    // Quadratic would put `long` at roughly 4x `short`. Linear puts it near 2x;
    // allow real slack for the fixed system/user blocks, which are hashed once in
    // both runs and so make the short run proportionally more expensive.
    assert!(
        long < short * 5 / 2,
        "doubling a turn's steps took hashing from {short} to {long} passes. \
         Linear growth lands near 2x; anything approaching 4x means every step is \
         re-hashing the blocks the earlier steps already hashed."
    );
}

/// The memo must never outlive the bytes it describes.
///
/// Compaction rewrites tool results IN PLACE, so a position keeps naming a block
/// while its content — and therefore its `block_id` and `content_digest` — changes
/// underneath. A position-keyed memo that missed that would make the receipt cite
/// a block that was never sent, which is strictly worse than the cost it saves:
/// the entire point of a receipt is that it is what the model actually saw.
///
/// Driven through the ledger's public API so the assertion is on the contract:
/// rewrite one result, declare the new revision, emit again, and the manifest must
/// name the new bytes. Disabling the invalidation makes this fail — the second
/// manifest keeps citing the pre-rewrite id.
#[tokio::test]
async fn a_rewritten_block_is_never_served_from_the_digest_memo() {
    use crate::receipts::{ReceiptLedger, ServedBy, TranscriptRevision};
    use stella_protocol::{ToolOutput, ToolResult};

    let served = ServedBy {
        role: stella_protocol::ModelCallRole::Worker,
        provider: "p",
        model: "m",
    };
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
        CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::Ok {
                    content: "the original, uncompacted output".into(),
                },
            }],
            attachments: vec![],
        },
    ];

    let ids_at = |events: Vec<AgentEvent>| -> Vec<String> {
        events
            .into_iter()
            .filter_map(|e| match e {
                AgentEvent::StepManifest { blocks, .. } => Some(blocks),
                _ => None,
            })
            .flatten()
            .map(|b| b.block_id)
            .collect()
    };

    let mut ledger = ReceiptLedger::new(0);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let events = crate::event_sender::EventSender::new(tx);

    ledger.emit_step_receipt(&messages, 100, 0, served, &events);
    let before = ids_at(drain_events(&mut rx));

    // Exactly what compaction's eviction pass does: rewrite the result's bytes
    // in place, leaving its position untouched.
    messages[2].tool_results[0].output = ToolOutput::Ok {
        content: "[evicted to fit context]".into(),
    };
    let mut revision = TranscriptRevision::default();
    revision.rewritten();
    ledger.set_transcript_revision(revision);

    ledger.emit_step_receipt(&messages, 100, 1, served, &events);
    let after = ids_at(drain_events(&mut rx));

    assert_eq!(before.len(), after.len(), "same block count either side");
    // The system prefix and user goal did not move and must keep their ids.
    assert_eq!(
        before[0], after[0],
        "unrewritten blocks keep their identity"
    );
    assert_eq!(
        before[1], after[1],
        "unrewritten blocks keep their identity"
    );
    // The rewritten result must not.
    assert_ne!(
        before[2], after[2],
        "the tool result's bytes were rewritten, so the manifest must cite a new \
         block id — citing {} again means the digest memo outlived the content it \
         described, and the receipt now claims the model saw an output that had \
         already been evicted",
        before[2]
    );
}

/// A tool double whose outputs are large enough for the pure compaction passes to
/// have something worth evicting, and **distinct per call**.
///
/// Distinctness matters more than it looks: block ids are content-addressed, so
/// byte-identical outputs share one id across every position holding them. A test
/// asserting "this rewritten block's id never reappears" would then fail on
/// correct code, because the one tool message compaction is forbidden to touch
/// still legitimately carries that id.
struct BigOutputTools {
    filler: usize,
}

#[async_trait]
impl ToolExecutor for BigOutputTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "bash".into(),
            description: "run a command".into(),
            input_schema: serde_json::json!({"type": "object"}),
            read_only: false,
            speculation_safe: false,
        }]
    }
    async fn execute(&self, _name: &str, input: &Value) -> ToolOutput {
        // Seeded from the call's own input so no two results share a block id.
        let seed = input.get("cmd").and_then(Value::as_str).unwrap_or("?");
        ToolOutput::Ok {
            content: format!("{seed}: {}", "x".repeat(self.filler)),
        }
    }
}

/// The driver must actually TELL the ledger when it rewrites the transcript.
///
/// `a_rewritten_block_is_never_served_from_the_digest_memo` pins the ledger's own
/// contract, but nothing pinned the wiring: deleting the `revision.rewritten()`
/// call in `run_turn` left the entire suite green, which means the memo could have
/// shipped serving stale digests on every compacting turn while every unit test
/// passed. This drives a real turn over a budget tight enough to force eviction
/// and asserts the receipts follow the rewrite.
///
/// A `Compaction` event names the blocks it stubbed (`evicted_blocks`, captured
/// BEFORE mutation). Once those bytes are the eviction stub, no later manifest can
/// still be citing their original ids — if one does, the memo outlived them.
#[tokio::test]
async fn compaction_mid_turn_invalidates_the_receipt_ledgers_digest_memo() {
    let mut script: Vec<_> = (0..5)
        .map(|i| {
            let mut r = tool_call_result(&format!("call_{i}"), "bash");
            if let Some(call) = r.tool_calls.first_mut() {
                call.input = serde_json::json!({ "cmd": format!("echo {i}") });
            }
            Ok(r)
        })
        .collect();
    script.push(Ok(text_result("done")));
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(script),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = BigOutputTools { filler: 4_000 };
    let sleeper = NoopSleeper;
    let config = EngineConfig {
        // Small enough that a couple of 4 KB results blow it, and no summarizer so
        // the rewrite comes purely from eviction/aging.
        compaction_budget_tokens: 800,
        summarize_overflow: false,
        ..EngineConfig::default()
    };
    let engine = Engine::with_sleeper(&provider, &tools, config, &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));

    let events = drain_events(&mut rx);
    // Checked point-in-time, walking the stream in emission order: a block is
    // only stale for the manifests that come AFTER the compaction that rewrote it.
    // Comparing every manifest against the turn's whole eviction set instead would
    // flag a block for being cited on the step that produced it and evicted on the
    // next, which is exactly correct behaviour.
    let mut rewritten_so_far: std::collections::HashSet<String> = Default::default();
    let mut rewrites = 0usize;
    let mut checked = 0usize;
    for event in &events {
        match event {
            AgentEvent::Compaction {
                evicted_blocks,
                aged_blocks,
                deduped_blocks,
                superseded_blocks,
                ..
            } => {
                for id in evicted_blocks
                    .iter()
                    .chain(aged_blocks)
                    .chain(deduped_blocks)
                    .chain(superseded_blocks)
                {
                    rewrites += 1;
                    rewritten_so_far.insert(id.clone());
                }
            }
            AgentEvent::StepManifest { blocks, step, .. } => {
                for entry in blocks {
                    assert!(
                        !rewritten_so_far.contains(&entry.block_id),
                        "step {step}'s manifest cites block {}, which an earlier \
                         compaction had already rewritten. The driver did not tell \
                         the ledger the transcript changed, so its digest memo is \
                         describing bytes the model never saw.",
                        entry.block_id
                    );
                    checked += 1;
                }
            }
            _ => {}
        }
    }

    assert!(
        rewrites > 0,
        "fixture must actually rewrite tool results, or this proves nothing"
    );
    assert!(checked > 0, "manifests must cite blocks");
}

/// The invariant the identity memo's soundness actually rests on.
///
/// `snapshot_result_identities` memoizes a result's identity by position, and
/// clears that memo on a transcript rewrite — but disabling the clearing breaks no
/// test, and it is worth being precise about why rather than assuming the memo is
/// merely under-tested. Every rewrite the compaction passes perform leaves content
/// that `is_compacted_output` recognizes (the three stubs, or the aging elision
/// marker), and a recognized result is skipped before any identity is derived. So
/// the only positions that can be served from the memo are ones compaction did not
/// touch, which is what makes a stale entry unobservable.
///
/// That is a cross-module invariant, not a local one. A new pass that rewrote a
/// result into something `is_compacted_output` did not recognize would silently
/// make the memo serve a pre-rewrite identity, so the invariant is pinned here
/// where the memo's justification lives.
#[test]
fn every_rewrite_compaction_performs_is_recognized_as_compacted() {
    use crate::compaction::{compact, is_compacted_output};

    // An over-budget transcript with enough large, distinct results to drive the
    // dedup, supersession, aging and eviction passes.
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("goal"),
    ];
    for i in 0..8 {
        messages.push(CompletionMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: vec![stella_protocol::ToolCall {
                call_id: format!("c{i}"),
                name: "bash".into(),
                // Half the calls repeat an input, so supersession/dedup engage.
                input: serde_json::json!({ "cmd": format!("echo {}", i % 3) }),
            }],
            tool_results: vec![],
            attachments: vec![],
        });
        messages.push(CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![stella_protocol::ToolResult {
                call_id: format!("c{i}"),
                output: ToolOutput::Ok {
                    // Far above AGE_THRESHOLD_CHARS so aging alone can satisfy the
                    // budget below and the eviction pass never runs — otherwise
                    // every aged result would be evicted afterwards and its
                    // elision marker would never survive to be checked.
                    content: format!("result {i}: {}", "y".repeat(20_000)),
                },
            }],
            attachments: vec![],
        });
    }

    let before: Vec<Vec<ToolOutput>> = messages
        .iter()
        .map(|m| m.tool_results.iter().map(|r| r.output.clone()).collect())
        .collect();

    let report = compact(&mut messages, 12_000).expect("the fixture must compact");
    assert!(
        report.aged > 0 && report.evicted == 0,
        "the fixture must exercise AGING specifically, with no eviction to mask it: \
         aged={} evicted={}",
        report.aged,
        report.evicted
    );
    let rewrites = report.evicted + report.deduped + report.superseded + report.aged;
    assert!(rewrites > 0, "the fixture must rewrite something");

    let mut changed = 0usize;
    for (message_index, message) in messages.iter().enumerate() {
        for (result_index, result) in message.tool_results.iter().enumerate() {
            let was = &before[message_index][result_index];
            if &result.output == was {
                continue;
            }
            changed += 1;
            assert!(
                is_compacted_output(&result.output),
                "compaction rewrote the result at ({message_index}, {result_index}) \
                 into content `is_compacted_output` does not recognize. That breaks \
                 the guard `snapshot_result_identities` relies on to never serve a \
                 memoized identity for bytes that changed."
            );
        }
    }
    // `<=`, not `==`: one result can be counted by two passes — aged to head+tail
    // and then evicted outright in the same call — so the report's totals exceed
    // the number of positions that changed.
    assert!(
        changed > 0 && changed <= rewrites,
        "{changed} positions changed against {rewrites} counted rewrites"
    );
}
