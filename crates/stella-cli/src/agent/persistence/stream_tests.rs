//! `spawn_renderer`'s stream-ordering tests.
//!
//! Split out of `persistence.rs` when the transcript-frame wiring took that
//! file over the 1500-line limit. The guard refuses to grandfather a new god
//! file, and it is right to: the remedy is a submodule, not a bigger ceiling.

use super::*;

#[test]
fn run_complete_is_final_even_when_later_events_arrive() {
    // Engine `TurnComplete` events pass through inline as ordinary
    // narration; only the run's `RunComplete` terminator is held back and
    // emitted last, even when accounting/reflection events follow it in the
    // channel (#3379).
    let events = vec![
        AgentEvent::TurnComplete {
            model: "worker".into(),
            cost_usd: 1.0,
        },
        AgentEvent::RunComplete {
            model: "final".into(),
            cost_usd: 1.25,
        },
        AgentEvent::Stage {
            name: stella_protocol::StageKind::Reflect.into(),
            scope: stella_protocol::StageScope::Run,
        },
    ];
    let mut terminal = None;
    let mut ordered: Vec<_> = events
        .into_iter()
        .filter_map(|event| defer_stream_terminal(&mut terminal, event))
        .collect();
    ordered.extend(terminal);

    // The engine turn completion survives inline.
    assert!(
        ordered
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnComplete { .. }))
    );
    // Exactly one terminator, and it is the last event.
    assert_eq!(
        ordered
            .iter()
            .filter(|event| matches!(event, AgentEvent::RunComplete { .. }))
            .count(),
        1
    );
    assert!(matches!(
        ordered.last(),
        Some(AgentEvent::RunComplete { model, cost_usd })
            if model == "final" && (*cost_usd - 1.25).abs() < f64::EPSILON
    ));
}

#[tokio::test]
async fn stream_renderer_persists_reflection_before_the_run_terminator() {
    let store = std::sync::Arc::new(stella_store::Store::in_memory().expect("store"));
    let execution_id = store
        .begin_execution("pipeline", "prompt", "anthropic", "claude")
        .expect("begin");
    store
        .set_execution_session(execution_id, "stream-order")
        .expect("session");
    let (tx, rx) = mpsc::unbounded_channel();
    let renderer = spawn_renderer(
        rx,
        OutputFormat::StreamJson,
        Some((store.clone(), execution_id)),
        "anthropic".into(),
        false,
        None,
    );
    // The engine turn completes inline, then the run owner emits the
    // terminator — but a reflection event still comes down the channel
    // after it. The renderer must hold the terminator until that later
    // event has drained, so `RunComplete` is the final durable line.
    tx.send(AgentEvent::TurnComplete {
        model: "worker".into(),
        cost_usd: 1.0,
    })
    .unwrap();
    tx.send(AgentEvent::RunComplete {
        model: "worker+reflection".into(),
        cost_usd: 1.25,
    })
    .unwrap();
    tx.send(AgentEvent::Stage {
        name: stella_protocol::StageKind::Reflect.into(),
        scope: stella_protocol::StageScope::Run,
    })
    .unwrap();
    drop(tx);

    let outcome = renderer.await.expect("renderer");
    assert!(outcome.persistence_complete);
    let journal = store.session_events("stream-order").expect("journal");
    assert_eq!(journal.events.len(), 3);
    // The engine turn completion passes through inline as narration.
    assert!(matches!(
        journal.events.first().map(|record| &record.event),
        Some(AgentEvent::TurnComplete { model, .. }) if model == "worker"
    ));
    // The reflection event arrived after the terminator on the wire but is
    // persisted before it.
    assert!(matches!(
        journal.events.get(1).map(|record| &record.event),
        Some(AgentEvent::Stage {
            name,
            scope: stella_protocol::StageScope::Run
        }) if name.kind() == Some(stella_protocol::StageKind::Reflect)
    ));
    // The run terminator is the final durable line.
    assert!(matches!(
        journal.events.last().map(|record| &record.event),
        Some(AgentEvent::RunComplete { model, cost_usd })
            if model == "worker+reflection"
                && (*cost_usd - 1.25).abs() < f64::EPSILON
    ));
}

#[test]
fn receipt_events_persist_into_queryable_block_and_manifest_rows() {
    // The increment-1 promise, end to end: a BlockRegistered + StepManifest
    // pair flowing through persist_event lands as queryable receipt rows,
    // and the manifest reconstructs the step's block order with token_cost
    // joined back from the block registry.
    use stella_protocol::{BlockKind, BlockOrigin, CacheZone, ManifestEntry, ModelCallRole};
    let store = Store::in_memory().expect("store");
    let id = store
        .begin_execution("run", "p", "anthropic", "opus")
        .expect("exec");

    let registered = AgentEvent::BlockRegistered {
        block_id: "blk_tool1".into(),
        kind: BlockKind::ToolResult,
        origin: BlockOrigin {
            turn_instance: 0,
            step: 0,
            call_id: Some("c1".into()),
            memory_id: None,
        },
        token_cost: 40,
        content_digest: "sha256:abc".into(),
        citation_label: None,
        content: None,
    };
    assert!(persist_event(&store, id, 0, &registered, "anthropic"));

    let manifest = AgentEvent::StepManifest {
        turn_instance: 0,
        step: 0,
        call_seq: 0,
        role: ModelCallRole::Worker,
        provider: "anthropic".into(),
        model: "opus".into(),
        blocks: vec![ManifestEntry {
            block_id: "blk_tool1".into(),
            cache_zone: CacheZone::Volatile,
            token_cost: 40,
            resident_since_step: 0,
            message_index: 0,
            call_id: Some("call_tool1".into()),
        }],
        effective_budget_tokens: 136_363,
        calibration_factor: 1.1,
        estimated_input_tokens: 40,
        compiled_frame: None,
    };
    assert!(persist_event(&store, id, 1, &manifest, "anthropic"));

    // The block registry row, with its call_id join key and snake_case kind.
    let blocks = store.context_blocks(id).expect("blocks");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].block_id, "blk_tool1");
    assert_eq!(blocks[0].call_id.as_deref(), Some("c1"));
    assert_eq!(blocks[0].kind, "tool_result");

    // The manifest reconstructs the step's ordered blocks, token_cost joined
    // back from context_blocks.
    let entries = store.step_manifest(id, 0, 0, 0).expect("manifest");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].block_id, "blk_tool1");
    assert_eq!(entries[0].cache_zone, "volatile");
    assert_eq!(entries[0].token_cost, Some(40));
}

#[test]
fn end_to_end_receipt_reconstructs_the_step_byte_exact_from_the_persisted_store() {
    // The increment-2 gate: the REAL emitter produces a receipt that, once
    // persisted, reconstructs byte-exact what the model saw — resolved from
    // the fold (tool I/O, assistant text) + local gaps (system/user), never
    // from the emitter's in-memory state. Exercises a full tool round-trip.
    use stella_core::event_sender::EventSender;
    use stella_core::receipts::ReceiptLedger;
    use stella_protocol::{CompletionMessage, MessageRole, ToolCall, ToolOutput, ToolResult};

    let call = ToolCall {
        call_id: "c1".into(),
        name: "read_file".into(),
        input: serde_json::json!({ "path": "a.rs" }),
    };
    let output = ToolOutput::Ok {
        content: "fn a() {}".into(),
        data: None,
    };
    // The step-1 input: system, user, assistant (text + tool call), result.
    let original = vec![
        CompletionMessage::system("you are a careful engineer"),
        CompletionMessage::user("fix the failing test"),
        CompletionMessage {
            role: MessageRole::Assistant,
            content: "let me read the file".into(),
            tool_calls: vec![call.clone()],
            tool_results: vec![],
            attachments: vec![],
        },
        CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![ToolResult {
                call_id: "c1".into(),
                output: output.clone(),
            }],
            attachments: vec![],
        },
    ];

    // Drive the REAL emitter + the journal events the driver would emit.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let events = EventSender::new(tx);
    let _ = events.send(AgentEvent::Text {
        text: "let me read the file".into(),
    });
    let _ = events.send(AgentEvent::ToolStart { call: call.clone() });
    let _ = events.send(AgentEvent::ToolResult {
        call_id: "c1".into(),
        output: output.clone(),
        duration_ms: 5,
        speculated: false,
    });
    let mut ledger = ReceiptLedger::new(0);
    ledger.set_effective_budget(136_363, 1.1);
    ledger.emit_step_receipt_estimating(
        &original,
        1,
        stella_core::receipts::ServedBy {
            role: stella_protocol::ModelCallRole::Worker,
            provider: "anthropic",
            model: "opus",
        },
        &events,
    );
    drop(events);

    // Persist the whole stream exactly as the renderer would.
    let store = Store::in_memory().expect("store");
    let id = store
        .begin_execution("run", "fix the failing test", "anthropic", "opus")
        .expect("exec");
    let mut seq = 0u64;
    while let Ok(event) = rx.try_recv() {
        persist_event(&store, id, seq, &event, "anthropic");
        seq += 1;
    }

    // Reconstruct purely from the persisted store, and prove it byte-exact.
    let recon = store
        .reconstruct_worker_step(id, 0, 1)
        .expect("reconstruct");
    assert!(
        recon.is_verified(),
        "unresolved={:?} mismatches={:?}",
        recon.unresolved,
        recon.digest_mismatches
    );
    assert_eq!(recon.messages, original);
}

#[test]
fn a_decomposed_recall_turn_still_reconstructs_byte_exact() {
    // The Phase 2 gate (#713): "byte-exact turn reconstruction still
    // passes, NOW INCLUDING DECOMPOSED RECALL." The recall block splits
    // into one block per recalled item, the summary and the steer stop
    // being attributed to the user, and an attachment gets a block of its
    // own — and after all of that the persisted receipt must still rebuild
    // the exact messages the model saw.
    //
    // This is the single most important assertion in the phase: every other
    // benefit of decomposition is worthless if the receipt stops being a
    // faithful record.
    use stella_core::event_sender::EventSender;
    use stella_core::receipts::{RECALL_MARKER, ReceiptLedger};
    use stella_protocol::{Attachment, CompletionMessage, MessageRole};

    let recall = format!(
        "{RECALL_MARKER}\n\nRelevant context:\n\
         - [nod_abc123] auth module — always validate the token before use\n\
         - [nod_def456] deploy runbook — staging first, then production\n\
         - engine step-driver (driver.rs) — the step loop lives here"
    );
    let summary = "[earlier history summarized to fit context — full detail was compacted \
                   away; re-read files or re-run tools for specifics]\n\nwe read three files";
    let steer = "[stuck-loop warning] you appear to be looping: same call twice.";

    let original = vec![
        CompletionMessage::system("you are a careful engineer"),
        CompletionMessage::user(summary),
        CompletionMessage::user(recall.clone()),
        CompletionMessage {
            role: MessageRole::User,
            content: "fix the failing test".into(),
            tool_calls: vec![],
            tool_results: vec![],
            attachments: vec![Attachment::from_path(
                "screenshot.png",
                "image/png",
                2048,
                "/tmp/screenshot.png",
            )],
        },
        CompletionMessage::user(steer),
    ];

    let (tx, mut rx) = mpsc::unbounded_channel();
    let events = EventSender::new(tx);
    let mut ledger = ReceiptLedger::new(0).with_lifecycle(true);
    ledger.emit_step_receipt_estimating(
        &original,
        0,
        stella_core::receipts::ServedBy {
            role: stella_protocol::ModelCallRole::Worker,
            provider: "anthropic",
            model: "opus",
        },
        &events,
    );
    drop(events);

    let store = Store::in_memory().expect("store");
    let id = store
        .begin_execution("run", "fix the failing test", "anthropic", "opus")
        .expect("exec");
    let mut seq = 0u64;
    let mut kinds: Vec<String> = Vec::new();
    let mut memory_ids: Vec<String> = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let AgentEvent::BlockRegistered { kind, origin, .. } = &event {
            kinds.push(enum_tag(kind));
            if let Some(memory_id) = &origin.memory_id {
                memory_ids.push(memory_id.clone());
            }
        }
        persist_event(&store, id, seq, &event, "anthropic");
        seq += 1;
    }

    // Decomposition actually happened — otherwise the round-trip below
    // would pass trivially by never having split anything.
    assert!(
        kinds.contains(&"summary".to_string()),
        "the overflow summary is no longer the user's goal: {kinds:?}"
    );
    assert!(
        kinds.contains(&"steered".to_string()),
        "the stuck-loop steer is no longer the user's goal: {kinds:?}"
    );
    assert!(
        kinds.contains(&"attachment".to_string()),
        "the attachment has a block: {kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|k| *k == "recalled_frame").count(),
        4,
        "one leading segment + three recalled items: {kinds:?}"
    );
    // Per-item provenance: the two memory frames resolve to their records.
    assert_eq!(memory_ids, vec!["nod_abc123", "nod_def456"]);

    // And the receipt is still a faithful record of what the model saw.
    let recon = store
        .reconstruct_worker_step(id, 0, 0)
        .expect("reconstruct");
    assert!(
        recon.is_verified(),
        "unresolved={:?} mismatches={:?}",
        recon.unresolved,
        recon.digest_mismatches
    );
    assert_eq!(recon.messages, original);
}
