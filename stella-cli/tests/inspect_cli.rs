//! End-to-end witness for `stella inspect`: drive the real binary against a
//! real store and check that the context a past call was sent comes back out.
//!
//! The receipts plane could reconstruct a step long before this command
//! existed; what was missing was any way to ask it. These tests exercise the
//! asking — argument parsing, the SQL, and the rendering — not the
//! reconstruction algorithm (covered by `stella-store`'s own tests).

use std::process::Command;

use stella_store::{ContextBlockRow, ManifestBlockRow, StepManifestRow, Store};

/// Digest helper matching `stella-core`'s block identity: the store verifies
/// journal-resolved blocks against this, and skips the check for gap kinds
/// (which carry their bytes locally), so a placeholder is fine here.
fn block(block_id: &str, kind: &str, content: &str) -> ContextBlockRow {
    ContextBlockRow {
        block_id: block_id.into(),
        kind: kind.into(),
        origin_turn: 0,
        origin_step: 0,
        call_id: None,
        memory_id: None,
        token_cost: 10,
        content_digest: format!("sha256:{block_id}"),
        citation_label: None,
        // Gap kinds carry their preimage locally — this is the field that makes
        // a system prompt readable after the fact.
        content: Some(content.into()),
    }
}

fn entry(block_id: &str, message_index: u64) -> ManifestBlockRow {
    ManifestBlockRow {
        block_id: block_id.into(),
        cache_zone: "cacheable".into(),
        token_cost: 10,
        resident_since_step: 0,
        message_index,
    }
}

/// A workspace whose store holds one execution with two model calls at the
/// SAME step: the engine's worker (seq 0) and an overflow summarizer (seq 1).
fn seeded_workspace() -> (tempfile::TempDir, i64) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");
    let id = store
        .begin_execution("run", "fix the failing test", "anthropic", "opus")
        .expect("execution");

    store
        .record_context_block(id, &block("blk_sys", "system_prefix", "You are Stella."))
        .expect("system block");
    store
        .record_context_block(id, &block("blk_goal", "user_goal", "fix the failing test"))
        .expect("goal block");
    store
        .record_context_block(
            id,
            &block("blk_sum", "system_prefix", "Condense this span faithfully."),
        )
        .expect("summarizer block");

    store
        .record_step_manifest(
            id,
            &StepManifestRow {
                turn_instance: 0,
                step: 0,
                call_seq: 0,
                provider: "anthropic".into(),
                model: "opus".into(),
                call_role: "worker".into(),
                effective_budget_tokens: 136_363,
                calibration_factor: 1.1,
                estimated_input_tokens: 20,
                blocks: vec![entry("blk_sys", 0), entry("blk_goal", 1)],
            },
        )
        .expect("worker manifest");
    store
        .record_step_manifest(
            id,
            &StepManifestRow {
                turn_instance: 0,
                step: 0,
                call_seq: 1,
                provider: "anthropic".into(),
                model: "haiku".into(),
                call_role: "summarization".into(),
                effective_budget_tokens: 136_363,
                calibration_factor: 1.1,
                estimated_input_tokens: 10,
                blocks: vec![entry("blk_sum", 0)],
            },
        )
        .expect("summarizer manifest");
    (dir, id)
}

fn inspect(dir: &tempfile::TempDir, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_stella"))
        .arg("inspect")
        .args(args)
        .current_dir(dir.path())
        .output()
        .expect("run stella inspect");
    assert!(
        output.status.success(),
        "stella inspect {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn inspect_lists_executions_then_calls_then_the_context_itself() {
    let (dir, id) = seeded_workspace();

    // Level 1: the execution index.
    let index = inspect(&dir, &[]);
    assert!(
        index.contains(&id.to_string()),
        "index lists the execution: {index}"
    );
    assert!(
        index.contains("fix the failing test"),
        "index previews the prompt: {index}"
    );

    // Level 2: both calls of the step are listed — the collision this fixes.
    let calls = inspect(&dir, &[&id.to_string()]);
    assert!(calls.contains("worker"), "worker call listed: {calls}");
    assert!(
        calls.contains("summarization"),
        "the summarizer call is no longer overwritten by the worker: {calls}"
    );

    // Level 3: the worker's actual context.
    let worker = inspect(&dir, &[&id.to_string(), "--step", "0"]);
    assert!(
        worker.contains("You are Stella."),
        "the system prompt is readable: {worker}"
    );
    assert!(
        worker.contains("fix the failing test"),
        "the user goal is readable: {worker}"
    );
    assert!(
        !worker.contains("Condense this span"),
        "the worker call must not show the summarizer's prompt: {worker}"
    );

    // The summarizer's own context — previously unrecoverable entirely.
    let summarizer = inspect(&dir, &[&id.to_string(), "--step", "0", "--call-seq", "1"]);
    assert!(
        summarizer.contains("Condense this span faithfully."),
        "the summarizer's prompt is recorded and readable: {summarizer}"
    );
    assert!(
        !summarizer.contains("You are Stella."),
        "each call shows its own context: {summarizer}"
    );
}

#[test]
fn inspect_json_is_machine_readable_and_reports_verification() {
    let (dir, id) = seeded_workspace();
    let out = inspect(&dir, &[&id.to_string(), "--step", "0", "--format", "json"]);
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
    assert_eq!(parsed["verified"], true, "clean path verifies: {out}");
    let messages = parsed["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 2, "system + user: {out}");
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "You are Stella.");
    assert_eq!(messages[1]["role"], "user");
}

#[test]
fn inspect_names_a_missing_receipt_instead_of_printing_an_empty_transcript() {
    let (dir, id) = seeded_workspace();
    let output = Command::new(env!("CARGO_BIN_EXE_stella"))
        .args(["inspect", &id.to_string(), "--step", "99"])
        .current_dir(dir.path())
        .output()
        .expect("run");
    assert!(
        !output.status.success(),
        "an absent step is an error, not silence"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no receipt"),
        "says what was missing: {stderr}"
    );
}
