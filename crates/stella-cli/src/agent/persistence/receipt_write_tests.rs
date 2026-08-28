//! The receipt plane's write path, covered end to end.
//!
//! `record_step_manifest` had tests; the arm of `persist_event_detailed` that
//! calls it had none, so nothing anywhere asserted that a `StepManifest` event
//! turns into the two rows a prompt reconstruction is rebuilt from. That gap is
//! where #5255 was hunted, and it is worth closing whatever that turns out to
//! have been: this is the only test that fails if the arm is deleted, renamed
//! or given a row it cannot write.

use super::*;

fn manifest_event() -> AgentEvent {
    AgentEvent::StepManifest {
        turn_instance: 0,
        step: 1,
        call_seq: 0,
        role: stella_protocol::ModelCallRole::Worker,
        provider: "openrouter".into(),
        upstream_provider: None,
        model: "moonshotai/kimi-k3".into(),
        blocks: vec![stella_protocol::ManifestEntry {
            block_id: "blk_1dfbdb3ce2c541220dfa27b7".into(),
            cache_zone: stella_protocol::CacheZone::StablePrefix,
            token_cost: 2033,
            resident_since_step: 1,
            message_index: 0,
            call_id: None,
        }],
        effective_budget_tokens: 153_159,
        calibration_factor: 1.0,
        estimated_input_tokens: 2_194,
        stall_seconds_requested: None,
        compiled_frame: None,
    }
}

/// One manifest event, both rows.
///
/// The values are the ones a real turn recorded (execution 321 of this
/// repository's own store), so the test exercises the shape the engine emits
/// rather than a shape invented to pass.
#[test]
fn a_manifest_event_lands_its_receipt_and_entries() {
    let store = stella_store::Store::in_memory().expect("store");
    let execution_id = store
        .begin_execution("deck", "prompt", "openrouter", "moonshotai/kimi-k3")
        .expect("begin");

    let outcome = persist_event_detailed(&store, execution_id, 0, &manifest_event(), "openrouter");
    assert!(
        outcome.is_complete(),
        "persisting a manifest reported {outcome:?}"
    );

    let entries = store
        .step_manifest(execution_id, 0, 1, 0)
        .expect("read the manifest back");
    assert_eq!(
        entries.len(),
        1,
        "the event's one block should be one manifest row"
    );
    assert_eq!(entries[0].block_id, "blk_1dfbdb3ce2c541220dfa27b7");
    // The header carries the call's identity, which is what the reconstruction
    // addresses a receipt by.
    let calls = store
        .recorded_calls(execution_id)
        .expect("read the receipt header back");
    assert_eq!(calls.len(), 1, "one call, one receipt header");
}
