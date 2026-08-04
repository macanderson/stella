//! Tests for [`super`] — the receipts plane.
//!
//! Split out of `receipts.rs` so that file stays under the 1500-line ratchet
//! (`scripts/check-file-size.sh`). A pure move: no assertion changed.

use super::*;
use stella_protocol::{ToolCall, ToolOutput, ToolResult};
use tokio::sync::mpsc::unbounded_channel;

fn worker<'a>(provider: &'a str, model: &'a str) -> ServedBy<'a> {
    ServedBy {
        role: ModelCallRole::Worker,
        provider,
        model,
    }
}

fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

fn convo() -> Vec<CompletionMessage> {
    vec![
        CompletionMessage::system("you are a careful engineer"),
        CompletionMessage::user("fix the failing test"),
        CompletionMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                call_id: "c1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "a.rs"}),
            }],
            tool_results: vec![],
            attachments: vec![],
        },
        CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: vec![],
            tool_results: vec![ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::Ok {
                    content: "fn a() {}".into(),
                },
            }],
            attachments: vec![],
        },
    ]
}

#[test]
fn manifest_names_every_block_in_wire_order_with_structural_zones() {
    let (tx, mut rx) = unbounded_channel();
    let events = EventSender::new(tx);
    let mut ledger = ReceiptLedger::new(0);
    ledger.set_effective_budget(136_363, 1.1);
    let messages = convo();
    ledger.emit_step_receipt_estimating(&messages, 0, worker("anthropic", "opus"), &events);

    let evts = drain(&mut rx);
    let manifest = evts
        .iter()
        .find_map(|e| match e {
            AgentEvent::StepManifest { blocks, .. } => Some(blocks.clone()),
            _ => None,
        })
        .expect("a manifest was emitted");
    // system prefix, user goal, one tool_call, one tool_result = 4 blocks.
    assert_eq!(manifest.len(), 4);
    assert_eq!(manifest[0].cache_zone, CacheZone::StablePrefix);
    assert_eq!(manifest[3].cache_zone, CacheZone::Volatile);
    // A BlockRegistered was emitted for each, before the manifest.
    let registered = evts
        .iter()
        .filter(|e| matches!(e, AgentEvent::BlockRegistered { .. }))
        .count();
    assert_eq!(registered, 4);
}

/// Two distinct calls whose output is byte-identical collapse onto one
/// content-addressed `block_id`, so `BlockRegistered` fires once and its
/// `BlockOrigin` can only name the first call. If the manifest inherited
/// that provenance, "which calls left context" would silently drop the
/// duplicate — an eviction of this block would be attributed to `c1` alone.
/// The manifest records the call per occurrence, so both survive.
#[test]
fn identical_output_from_two_calls_keeps_both_call_ids_on_the_manifest() {
    let (tx, mut rx) = unbounded_channel();
    let events = EventSender::new(tx);
    let mut ledger = ReceiptLedger::new(0);
    // Same bytes, different calls — e.g. the agent runs `git status` twice.
    let identical = |call_id: &str| CompletionMessage {
        role: MessageRole::Tool,
        content: String::new(),
        tool_calls: vec![],
        tool_results: vec![ToolResult {
            call_id: call_id.into(),
            output: ToolOutput::Ok {
                content: "nothing to commit".into(),
            },
        }],
        attachments: vec![],
    };
    let messages = vec![
        CompletionMessage::system("s"),
        identical("c1"),
        identical("c2"),
    ];
    ledger.emit_step_receipt_estimating(&messages, 0, worker("p", "m"), &events);
    let evts = drain(&mut rx);

    // The content-addressed contract still holds: one registration, one id.
    let registered: Vec<_> = evts
        .iter()
        .filter_map(|e| match e {
            AgentEvent::BlockRegistered {
                block_id, origin, ..
            } => Some((block_id.clone(), origin.call_id.clone())),
            _ => None,
        })
        .collect();
    let dupes: Vec<_> = registered
        .iter()
        .filter(|(_, call_id)| call_id.is_some())
        .collect();
    assert_eq!(dupes.len(), 1, "byte-identical results register once");
    assert_eq!(
        dupes[0].1.as_deref(),
        Some("c1"),
        "birth provenance is the first call to mint the block"
    );

    let manifest = evts
        .iter()
        .find_map(|e| match e {
            AgentEvent::StepManifest { blocks, .. } => Some(blocks.clone()),
            _ => None,
        })
        .expect("manifest");
    let tool_entries: Vec<_> = manifest.iter().filter(|b| b.call_id.is_some()).collect();
    assert_eq!(
        tool_entries.len(),
        2,
        "both occurrences are on the manifest"
    );
    assert_eq!(
        tool_entries[0].block_id, tool_entries[1].block_id,
        "and they do share the one content-addressed id"
    );
    assert_eq!(
        tool_entries
            .iter()
            .map(|b| b.call_id.as_deref().unwrap())
            .collect::<Vec<_>>(),
        vec!["c1", "c2"],
        "each occurrence keeps its own call — the join is not lossy"
    );
}

#[test]
fn a_block_carried_across_steps_registers_once_and_ages_its_residency() {
    let (tx, mut rx) = unbounded_channel();
    let events = EventSender::new(tx);
    let mut ledger = ReceiptLedger::new(0);
    let messages = convo();

    ledger.emit_step_receipt_estimating(&messages, 0, worker("p", "m"), &events);
    let _ = drain(&mut rx);
    // Same conversation on the next step: no NEW registrations, and the
    // blocks report resident_since_step == 0 (they arrived on step 0).
    ledger.emit_step_receipt_estimating(&messages, 1, worker("p", "m"), &events);
    let evts = drain(&mut rx);

    let new_registrations = evts
        .iter()
        .filter(|e| matches!(e, AgentEvent::BlockRegistered { .. }))
        .count();
    assert_eq!(new_registrations, 0, "carried blocks re-register 0 times");
    let manifest = evts
        .iter()
        .find_map(|e| match e {
            AgentEvent::StepManifest { blocks, .. } => Some(blocks.clone()),
            _ => None,
        })
        .expect("manifest");
    assert!(
        manifest.iter().all(|b| b.resident_since_step == 0),
        "every block has been resident since step 0"
    );
}

// ── Decomposed recall (#713 deliverable 1) ──────────────────────────────

fn recall_message(body: &str) -> String {
    format!("{RECALL_MARKER}\n\n{body}")
}

#[test]
fn recall_segments_always_concatenate_back_to_the_original_bytes() {
    // The property the whole split rests on. If this ever fails, byte-exact
    // reconstruction of a recall turn is broken — and it would fail
    // silently, as a transcript that looks plausible and is not what the
    // model saw. Cases chosen to be adversarial about separators: no items,
    // a trailing newline, blank lines between items, a body line that
    // itself starts with "- ", CRLF, and a lone item with no header.
    for content in [
        recall_message("Relevant context:\n- [nod_a] x — y\n- b — z"),
        recall_message("Relevant context:\n- a — b\n"),
        recall_message("Relevant context:\n\n- a — b\n\n- c — d\n\n"),
        recall_message("Relevant context:\n- a — steps:\n- one\n- two\n"),
        recall_message("Relevant context:\r\n- a — b\r\n"),
        "- only an item, no header".to_string(),
        recall_message(""),
        String::new(),
    ] {
        let joined: String = split_recall(&content)
            .iter()
            .map(|s| s.text)
            .collect::<Vec<_>>()
            .concat();
        assert_eq!(joined, content, "segments must partition the input exactly");
    }
}

#[test]
fn a_recall_block_splits_per_item_and_resolves_memory_frames_to_their_records() {
    let content = recall_message(
        "Relevant context:\n\
         - [nod_abc] auth module — validate the token\n\
         - engine step-driver (driver.rs) — the step loop\n",
    );
    let segments = split_recall(&content);
    // A leading segment (marker + section header) plus one per item.
    assert_eq!(segments.len(), 3);
    assert_eq!(segments[0].memory_id, None, "the header is not an item");
    assert_eq!(segments[1].memory_id.as_deref(), Some("nod_abc"));
    assert_eq!(segments[1].citation_label.as_deref(), Some("auth module"));
    // A non-memory frame keeps its label and has no record to point at —
    // code-graph hits are grounding, not memories, and inventing an id for
    // one would corrupt the join the citation loop reads.
    assert_eq!(segments[2].memory_id, None);
    assert_eq!(
        segments[2].citation_label.as_deref(),
        Some("engine step-driver (driver.rs)")
    );
}

#[test]
fn a_record_bullet_resolves_to_its_handle_sigil_included() {
    // The volatile record channel renders `- <statement> ^<handle>` with an
    // optional ` [enforced]` marker (records::render::bullet). The handle —
    // sigil included — is the id the block row carries, because it is the
    // exact string `cite_memory` accepts for a record: the join from a
    // rendered record to its citation works verbatim, which is what makes
    // ContextUseKind::Cited reachable for directives at all.
    let content = recall_message(
        "## Relevant context\n\
         - New dependencies must clear the deny.toml allowlist. ^license-allowlist\n\
         - Never force-push to main. ^no-force-push [enforced]\n",
    );
    let segments = split_recall(&content);
    assert_eq!(segments.len(), 3);
    assert_eq!(
        segments[1].memory_id.as_deref(),
        Some("^license-allowlist"),
        "the sigil rides the id so it can never be mistaken for a nod_… key"
    );
    assert_eq!(
        segments[2].memory_id.as_deref(),
        Some("^no-force-push"),
        "the [enforced] marker peels off before the handle is read"
    );
    // Prose that merely contains a caret mints no join key, and a tail token
    // outside the slug charset is prose, not a handle.
    let prose = recall_message(
        "## Relevant context\n\
         - Use the ^caret syntax mid-sentence for anchors\n\
         - The exponent operator is ^2\n",
    );
    let segments = split_recall(&prose);
    assert_eq!(segments[1].memory_id, None);
    assert_eq!(
        segments[2].memory_id, None,
        "a letterless tail (`^2`) is arithmetic, not a handle — the parser \
         requires at least one letter beyond the slug charset"
    );
}

#[test]
fn a_bracketed_prefix_that_is_not_a_record_id_is_not_read_as_one() {
    // A memory whose text happens to open with a bracket must not be
    // mistaken for a record id. A wrong `memory_id` is worse than none:
    // it is the key the write→citation loop joins on, so it would
    // attribute this turn's use to a record that had nothing to do with it.
    let content = recall_message("Relevant context:\n- [TODO] fix this — later\n");
    let segments = split_recall(&content);
    assert_eq!(segments[1].memory_id, None);
}

#[test]
fn the_drivers_own_messages_stop_being_attributed_to_the_user() {
    // The gap the old `decompose` named in a comment and never closed.
    assert_eq!(
        user_block_kind(crate::driver::SUMMARY_MARKER_PREFIX),
        BlockKind::Summary
    );
    assert_eq!(
        user_block_kind(crate::driver::LOOP_STEER_PREFIX),
        BlockKind::Steered
    );
    // The continuation nudge is engine-written too — it shares `Steered`
    // rather than widening the wire enum.
    assert_eq!(
        user_block_kind(crate::driver::CONTINUATION_MARKER_PREFIX),
        BlockKind::Steered
    );
    assert_eq!(user_block_kind(RECALL_MARKER), BlockKind::RecalledFrame);
    assert_eq!(user_block_kind("fix the failing test"), BlockKind::UserGoal);
}

#[test]
fn a_hydrated_attachment_never_puts_its_payload_on_the_event_stream() {
    // An at-rest attachment is metadata and rides as a gap kind. A hydrated
    // one carries inline base64, and journaling that would put the payload
    // on stdout under `--output-format stream-json`. Its identity still
    // hashes the real value; only the local preimage is withheld.
    use stella_protocol::{Attachment, AttachmentSource};
    let at_rest = CompletionMessage {
        role: MessageRole::User,
        content: "look at this".into(),
        tool_calls: vec![],
        tool_results: vec![],
        attachments: vec![Attachment::from_path("a.png", "image/png", 9, "/tmp/a.png")],
    };
    let hydrated = CompletionMessage {
        role: MessageRole::User,
        content: "look at this".into(),
        tool_calls: vec![],
        tool_results: vec![],
        attachments: vec![Attachment {
            name: "a.png".into(),
            media_type: "image/png".into(),
            byte_len: 9,
            source: AttachmentSource::Data {
                base64: "aGVsbG8=".into(),
            },
        }],
    };
    let at_rest_msgs = [at_rest];
    let hydrated_msgs = [hydrated];
    let at_rest = decompose(&at_rest_msgs, &mut BlockDigestCache::default());
    let hydrated = decompose(&hydrated_msgs, &mut BlockDigestCache::default());
    let attachment_of = |drafts: &[BlockDraft]| {
        drafts
            .iter()
            .find(|d| d.kind == BlockKind::Attachment)
            .map(|d| {
                (
                    d.content.as_deref().map(str::to_string),
                    d.content_digest.clone(),
                )
            })
            .expect("an attachment block")
    };
    let (at_rest_bytes, _) = attachment_of(&at_rest);
    let (hydrated_bytes, hydrated_digest) = attachment_of(&hydrated);
    assert!(
        at_rest_bytes.is_some(),
        "a path attachment is reconstructable"
    );
    assert_eq!(hydrated_bytes, None, "base64 never reaches the journal");
    // …and the withheld block is still content-addressed over the truth.
    assert!(hydrated_digest.starts_with("sha256:"));
}

#[test]
fn decomposition_does_not_move_the_cache_zones_the_prefix_depends_on() {
    // Spec §5.1: cache stability outranks everything. Splitting the recall
    // block adds blocks inside the volatile region and must leave the head
    // exactly where it was — a zone shift on the prefix is a full-rate
    // re-bill for the rest of the session.
    let messages = vec![
        CompletionMessage::system("you are a careful engineer"),
        CompletionMessage::user(recall_message("Relevant context:\n- a — b\n- c — d")),
        CompletionMessage::user("fix the failing test"),
    ];
    let drafts = decompose(&messages, &mut BlockDigestCache::default());
    assert_eq!(drafts[0].kind, BlockKind::SystemPrefix);
    assert_eq!(drafts[0].cache_zone, CacheZone::StablePrefix);
    assert_eq!(
        drafts.last().expect("a tail").cache_zone,
        CacheZone::Volatile
    );
    assert!(
        drafts[1..drafts.len() - 1]
            .iter()
            .all(|d| d.cache_zone == CacheZone::Cacheable),
        "everything between head and tail is ordinary cacheable"
    );
}

// ── The compiled frame (#713 deliverable 4) ─────────────────────────────

/// The compiled frame on a manifest, or `None` when the lifecycle is off.
fn frame_of(events: &[AgentEvent]) -> Option<CompiledContextFrameBuilt> {
    events.iter().find_map(|e| match e {
        AgentEvent::StepManifest { compiled_frame, .. } => Some(compiled_frame.clone()),
        _ => None,
    })?
}

/// Emit one receipt and return the events, with the lifecycle switch and
/// the volatile inputs (step, served-by, budget) under the caller's control.
fn emit(
    lifecycle: bool,
    step: usize,
    served: ServedBy<'_>,
    budget: (u64, f64),
    messages: &[CompletionMessage],
) -> Vec<AgentEvent> {
    let (tx, mut rx) = unbounded_channel();
    let events = EventSender::new(tx);
    let mut ledger = ReceiptLedger::new(0).with_lifecycle(lifecycle);
    ledger.set_effective_budget(budget.0, budget.1);
    ledger.emit_step_receipt_estimating(messages, step, served, &events);
    drain(&mut rx)
}

#[test]
fn the_lifecycle_switch_is_off_by_default_and_gates_the_whole_frame() {
    // The default constructor must produce a receipt byte-identical to the
    // pre-Phase-2 one. If this ever fails, "default off" is not true and
    // every existing consumer sees a field it was not promised.
    let off = emit(false, 0, worker("anthropic", "opus"), (100, 1.0), &convo());
    assert_eq!(frame_of(&off), None, "lifecycle off ⇒ no compiled frame");
    // And the manifest is otherwise unchanged: same block count, same order.
    let blocks = off
        .iter()
        .find_map(|e| match e {
            AgentEvent::StepManifest { blocks, .. } => Some(blocks.len()),
            _ => None,
        })
        .expect("a manifest was still emitted");
    assert_eq!(blocks, 4);

    let on = emit(true, 0, worker("anthropic", "opus"), (100, 1.0), &convo());
    assert!(frame_of(&on).is_some(), "lifecycle on ⇒ a compiled frame");
}

#[test]
fn identical_inputs_produce_byte_identical_frames_and_hashes_across_runs() {
    // The gate criterion, stated directly: two independent ledgers, no
    // shared state, same messages.
    let a = frame_of(&emit(true, 2, worker("p", "m"), (99, 1.25), &convo()));
    let b = frame_of(&emit(true, 2, worker("p", "m"), (99, 1.25), &convo()));
    assert_eq!(a, b);
    let a = a.expect("a frame");
    assert!(a.frame_hash.starts_with("sha256:"));
    assert_eq!(a.frame_hash.len(), "sha256:".len() + 64);
    assert_eq!(a.compiled_frame_id, format!("cf_{}", &a.frame_hash[7..31]));
}

#[test]
fn the_volatile_fields_are_excluded_from_the_frame_hash() {
    // Same prompt, different everything the manifest records ABOUT the
    // call: who served it, and what the budget arithmetic produced. A frame
    // hash that moved here would make the determinism gate unmeetable —
    // provider fallback and per-model calibration both change these
    // mid-session, on inputs the user never touched.
    let baseline = frame_of(&emit(true, 1, worker("p", "m"), (100, 1.0), &convo()));
    let served_elsewhere = frame_of(&emit(
        true,
        1,
        worker("other-provider", "other-model"),
        (100, 1.0),
        &convo(),
    ));
    let other_budget = frame_of(&emit(true, 1, worker("p", "m"), (777_777, 2.5), &convo()));
    assert_eq!(baseline, served_elsewhere, "provider/model are volatile");
    assert_eq!(baseline, other_budget, "budget/calibration are volatile");

    // …and `call_seq` likewise: the summarizer's seat is an allocation
    // order within an execution, not a property of the prompt it sent.
    let (tx, mut rx) = unbounded_channel();
    let events = EventSender::new(tx);
    ReceiptLedger::with_call_seq(0, RECEIPT_SEQ_SUMMARIZER)
        .with_lifecycle(true)
        .emit_step_receipt_estimating(&convo(), 1, worker("p", "m"), &events);
    assert_eq!(frame_of(&drain(&mut rx)), baseline, "call_seq is volatile");
}

#[test]
fn residency_does_not_move_the_frame_hash() {
    // Two ledgers reach step 1 with the same messages by different routes:
    // one has carried them since step 0 (resident_since_step 0), the other
    // is seeing them for the first time (resident_since_step 1). The
    // manifests differ; the frames must not. Residency is a fact about the
    // session's history, not about what this prompt contained.
    let (tx, mut rx) = unbounded_channel();
    let events = EventSender::new(tx);
    let mut carried = ReceiptLedger::new(0).with_lifecycle(true);
    carried.emit_step_receipt_estimating(&convo(), 0, worker("p", "m"), &events);
    let _ = drain(&mut rx);
    carried.emit_step_receipt_estimating(&convo(), 1, worker("p", "m"), &events);
    let carried_events = drain(&mut rx);

    let fresh = emit(true, 1, worker("p", "m"), (0, 1.0), &convo());

    // The manifests genuinely disagree — otherwise this test proves nothing.
    let residency = |evts: &[AgentEvent]| {
        evts.iter().find_map(|e| match e {
            AgentEvent::StepManifest { blocks, .. } => Some(
                blocks
                    .iter()
                    .map(|b| b.resident_since_step)
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
    };
    assert_ne!(residency(&carried_events), residency(&fresh));
    assert_eq!(frame_of(&carried_events), frame_of(&fresh));
}

#[test]
fn a_changed_prompt_changes_the_frame_hash() {
    // The other half of determinism: the hash must actually be a function
    // of the prompt. A hash that never moves is trivially deterministic.
    let baseline = frame_of(&emit(true, 0, worker("p", "m"), (0, 1.0), &convo()));
    let mut edited = convo();
    edited[1].content = "fix the OTHER failing test".into();
    assert_ne!(
        baseline,
        frame_of(&emit(true, 0, worker("p", "m"), (0, 1.0), &edited))
    );
    // As must the step: the same messages at a different step are a
    // different frame, because a frame identifies one model call.
    assert_ne!(
        baseline,
        frame_of(&emit(true, 1, worker("p", "m"), (0, 1.0), &convo()))
    );
}

/// The RFC 8785 canonical bytes of a value — the same canonicalizer
/// `context_record::hash` builds `record_hash` preimages with.
fn jcs<T: serde::Serialize>(value: &T) -> String {
    serde_json_canonicalizer::to_string(value).expect("canonicalizes")
}

#[test]
fn golden_frame_preimage_and_hash_are_pinned() {
    // The golden vector, in the shape `context_event.rs` set: the exact
    // canonical bytes first (hand-verifiable — keys sorted, absent options
    // gone, whitespace minimal), then the digest they hash to.
    //
    // These two lines are the wire contract for every stored frame hash.
    // Renaming a field, retyping one, adding one, or changing which fields
    // participate breaks exactly one of them — which is the point: a frame
    // hash that silently changes scheme makes every previously stored hash
    // unverifiable, and nothing else in the tree would notice.
    let messages = convo();
    let mut cache = BlockDigestCache::default();
    let drafts = decompose(&messages, &mut cache);
    let body = FrameBody {
        schema_version: FRAME_SCHEMA_VERSION,
        turn_instance: 0,
        step: 0,
        role: ModelCallRole::Worker,
        blocks: drafts.iter().map(BlockDraft::frame_block).collect(),
    };
    assert_eq!(jcs(&body), GOLDEN_FRAME_PREIMAGE);
    let identity = frame_identity(&body).expect("the body canonicalizes");
    assert_eq!(identity.frame_hash, GOLDEN_FRAME_HASH);
    assert_eq!(identity.compiled_frame_id, GOLDEN_FRAME_ID);

    // And the ledger reaches the same identity through the live path, so
    // the golden pins the emitted frame rather than a parallel computation.
    assert_eq!(
        frame_of(&emit(
            true,
            0,
            worker("anthropic", "opus"),
            (136_363, 1.1),
            &convo()
        )),
        Some(identity)
    );
}

const GOLDEN_FRAME_PREIMAGE: &str = r#"{"blocks":[{"block_id":"blk_1a2e1ee86e8551d1b069e12d","cache_zone":"stable_prefix","content_digest":"sha256:aadec3e1342a8b1cd71bc46a3796b4b2fc4ab35cb9822527f31f65d1f610ee90","kind":"system_prefix","message_index":0,"token_cost":8},{"block_id":"blk_901ab18c12179eb804a69d40","cache_zone":"cacheable","content_digest":"sha256:75804d696e4c6efcadc89848672f6a9304612fcf6180cedd429e25ac1cfa1bd1","kind":"user_goal","message_index":1,"token_cost":6},{"block_id":"blk_c49dacefa9f909aca42b2880","cache_zone":"cacheable","call_id":"c1","content_digest":"sha256:6c0ec0938d9f77b7ac74006a2ee21268972407d0c98395fc5b9cc6a0fb1a2602","kind":"tool_call","message_index":2,"token_cost":17},{"block_id":"blk_e7032f26bd4843608dfe31d9","cache_zone":"volatile","call_id":"c1","content_digest":"sha256:bfd1986b0e5cdf2925326cf75fa8d40320722e0a3971274a7844b2147ac927e7","kind":"tool_result","message_index":3,"token_cost":9}],"role":"worker","schema_version":"1.1","step":0,"turn_instance":0}"#;
const GOLDEN_FRAME_HASH: &str =
    "sha256:2d6226fa93fd3840a538ca68b2994f9cc8292dd66d6f6aa8f2b79667d8bcc3cf";
const GOLDEN_FRAME_ID: &str = "cf_2d6226fa93fd3840a538ca68";

#[test]
fn identical_tool_output_resolves_to_the_same_block_id() {
    // Two tool results with byte-identical output share a content-addressed
    // id — the property dedup/supersession identities rely on.
    let a = serde_json::to_string(&ToolOutput::Ok {
        content: "same".into(),
    })
    .unwrap();
    let id1 = block_id(BlockKind::ToolResult, &a);
    let id2 = block_id(BlockKind::ToolResult, &a);
    assert_eq!(id1, id2);
    assert!(id1.starts_with("blk_"));
    assert_eq!(id1.len(), 4 + 24);
}

#[test]
fn streamed_preimage_hashes_exactly_like_the_joined_one() {
    // `block_id` streams `kind_tag`, a NUL, and the content as three
    // `update`s instead of allocating the joined String. Every stored
    // block_id depends on that being the same digest.
    for kind in [
        BlockKind::SystemPrefix,
        BlockKind::ToolResult,
        BlockKind::AssistantText,
    ] {
        for content in ["", "plain", "ünïcödé — 日本語 \u{0}embedded NUL"] {
            let joined = format!("{}\0{}", kind_tag(kind), content);
            assert_eq!(
                block_id(kind, content),
                format!("blk_{}", &sha256_hex(&joined)[..24]),
                "streamed and joined preimages must agree for {kind:?}"
            );
        }
    }
}

/// #925's acceptance criterion, stated precisely.
///
/// The complaint was not that a receipt's two numbers differ — they always
/// will, because `estimated_input_tokens` carries per-message framing overhead
/// that no block accounts for, and because ceiling each block separately
/// rounds up more often than ceiling each message does. The complaint was that
/// **the size of the difference was a function of the user's language**: with
/// blocks counted in characters and the step counted in bytes, a CJK
/// transcript's summed `token_cost` fell to roughly a third of its
/// `estimated_input_tokens` while an English one reconciled exactly.
///
/// So this pins the property that actually matters: the same transcript
/// written in ASCII and in CJK must leave the same residual, and that residual
/// must be explainable by framing and rounding alone — never by content size.
#[test]
fn a_receipts_two_numbers_reconcile_the_same_way_in_any_language() {
    // Structurally identical transcripts. The CJK one is ~3x the bytes of the
    // ASCII one for the same character count, which is exactly the axis the
    // old character rule was blind to.
    let transcript = |sys: &str, goal: &str, out: &str| {
        vec![
            CompletionMessage::system(sys),
            CompletionMessage::user(goal),
            CompletionMessage {
                role: MessageRole::Tool,
                content: String::new(),
                tool_calls: vec![],
                tool_results: vec![ToolResult {
                    call_id: "c1".into(),
                    output: ToolOutput::Ok {
                        content: out.into(),
                    },
                }],
                attachments: vec![],
            },
        ]
    };

    let residual = |messages: &[CompletionMessage]| -> i64 {
        let (tx, mut rx) = unbounded_channel();
        let events = EventSender::new(tx);
        let mut ledger = ReceiptLedger::new(0);
        ledger.set_effective_budget(136_363, 1.0);
        ledger.emit_step_receipt_estimating(messages, 0, worker("anthropic", "opus"), &events);
        let (blocks, estimated) = drain(&mut rx)
            .iter()
            .find_map(|e| match e {
                AgentEvent::StepManifest {
                    blocks,
                    estimated_input_tokens,
                    ..
                } => Some((blocks.clone(), *estimated_input_tokens)),
                _ => None,
            })
            .expect("a manifest was emitted");
        let summed: u64 = blocks.iter().map(|b| u64::from(b.token_cost)).sum();
        estimated as i64 - summed as i64
    };

    let ascii = residual(&transcript(
        "you are a careful engineer",
        "fix the failing test",
        "no changes staged",
    ));
    let cjk = residual(&transcript(
        "あなたは慎重なエンジニアです",
        "失敗したテストを直して",
        "変更はありません",
    ));

    // Three messages, four framing tokens each, plus at most one token of
    // ceiling slack per block. Nothing here scales with how many bytes the
    // content has — which is the whole claim.
    assert!(
        (0..=20).contains(&ascii),
        "ASCII residual {ascii} is not framing + rounding"
    );
    assert_eq!(
        ascii, cjk,
        "the gap between a receipt's two numbers must not depend on the \
         user's language: ASCII left {ascii}, CJK left {cjk}"
    );

    // And the regression guard: under the character rule the CJK residual was
    // dominated by content size, so it grew when the content did. Doubling the
    // CJK content must not move the residual at all.
    let cjk_doubled = residual(&transcript(
        "あなたは慎重なエンジニアですあなたは慎重なエンジニアです",
        "失敗したテストを直して失敗したテストを直して",
        "変更はありません変更はありません",
    ));
    assert_eq!(
        cjk, cjk_doubled,
        "the residual must be a function of message structure, not content size"
    );
}
