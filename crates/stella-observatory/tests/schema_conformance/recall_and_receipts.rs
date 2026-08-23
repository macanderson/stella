//! Recall latency and sent-context reconstruction, against the real schema (#4486 split of `../schema_conformance.rs`).
use super::*;

/// Context-recall latency reaches the execution detail (#875).
///
/// Recall is on the first-token path of every turn, so a slow one delays
/// everything after it — and it was invisible in the dashboard, the receipt
/// and the log alike. This drives the real event through the real store and
/// reads it back through the real route.
#[test]
fn recall_latency_reaches_the_execution_detail() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");
    let id = store
        .begin_execution("run", "slow recall", "anthropic", "opus")
        .expect("begin");
    store
        .record_event(
            id,
            0,
            &AgentEvent::ContextRecall {
                frames: vec![ContextFrameRef {
                    id: Some("nod_1".into()),
                    citation_label: "auth module".into(),
                    provider: "workspace-memory".into(),
                    source: "stella-context".into(),
                    kind: "memory".into(),
                    uri: None,
                    method: None,
                    token_cost: 90,
                    block_id: None,
                    content_digest: None,
                }],
                provider_mix: vec![ProviderShare {
                    provider: "workspace-memory".into(),
                    frames: 1,
                }],
                tokens: 90,
                usage: None,
                latency_ms: 1_450,
                used_ann_index: Some(false),
            },
        )
        .expect("recall event");

    let detail: serde_json::Value =
        serde_json::from_slice(&respond(dir.path(), &format!("/api/execution?id={id}")).body)
            .expect("json");

    assert_eq!(
        detail["recall"][0]["latency_ms"], 1_450,
        "the operator can see recall was the slow part: {detail}"
    );
    assert_eq!(
        detail["recall"][0]["used_ann_index"], false,
        "and that the accelerator did not fire, which is a different problem"
    );
    assert_eq!(detail["recall"][0]["frames"], 1);
    assert_eq!(detail["recall"][0]["tokens"], 90);
}

/// A stream recorded before the field existed must read as "not measured",
/// never as "instant" — `0 ms` would be a claim the data does not support.
#[test]
fn a_recall_without_a_measurement_reads_as_null_not_zero() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");
    let id = store
        .begin_execution("run", "legacy stream", "anthropic", "opus")
        .expect("begin");
    // Written straight to the table: this is a payload shaped the way a
    // pre-#875 binary wrote it, which no current constructor can produce.
    {
        let raw =
            rusqlite::Connection::open(dir.path().join(".stella/private/store.db")).expect("open");
        raw.execute(
            "INSERT INTO events (execution_id, seq, event_type, payload) \
             VALUES (?1, 0, 'context_recall', ?2)",
            rusqlite::params![
                id,
                r#"{"type":"context_recall","frames":[],"provider_mix":[],"tokens":0}"#
            ],
        )
        .expect("legacy event");
    }

    let detail: serde_json::Value =
        serde_json::from_slice(&respond(dir.path(), &format!("/api/execution?id={id}")).body)
            .expect("json");

    assert!(
        detail["recall"][0]["latency_ms"].is_null(),
        "unmeasured must not render as 0 ms: {detail}"
    );
    assert!(detail["recall"][0]["used_ann_index"].is_null());
}

/// Sent-context reconstruction, end to end against a real store (#1475).
///
/// The strongest form of the drift gate this suite exists for, because it
/// checks a *byte-level* coupling rather than a column-level one. The
/// observatory links no protocol crate, so it rebuilds a `tool_call` block's
/// preimage by hand in `stella_protocol::ToolCall`'s declaration order; the
/// receipt here was digested over that crate's own serializer. Reorder those
/// fields and `verified` flips to false right here — the alternative being a
/// silent "the journal is torn" banner on every dashboard in the field.
#[test]
fn sent_context_reconstructs_a_real_stores_receipt_and_verifies_it() {
    let workspace = real_store_workspace();
    let body = respond(
        workspace.path(),
        "/api/execution-context?id=1&turn=0&step=1&call_seq=0",
    )
    .body;
    let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let context = &v["context"];
    assert_eq!(context["found"], true, "{v}");
    assert_eq!(
        context["messages"][0]["body"], SYSTEM_PROMPT,
        "the system prompt the model was actually sent: {v}"
    );
    assert_eq!(context["messages"][0]["role"], "system");
    assert_eq!(
        context["messages"][2]["body"], "← tool_result c1\nfn a() {}",
        "the tool result, recovered from the journal: {v}"
    );
    assert_eq!(
        context["verified"], true,
        "a journal-resolved block stopped re-hashing to the digest \
         stella_protocol's own serializer produced — a wire shape moved under \
         the observatory's hand-built preimage: {v}"
    );
    // The era stamp is read from the real column the real writer set (#1981).
    // This is the drift half of that signal: rename `executions.journal_era` in
    // stella-store and the dashboard would quietly fall back to the legacy era
    // for every execution, under-reporting a genuine integrity failure with no
    // test saying a word. `tests/journal_era.rs` pins what the two eras mean;
    // this pins that the column is still there to read.
    assert_eq!(
        context["journal_era"], "compaction_journaled",
        "an execution this build's Store began must be stamped as journaling \
         its compaction rewrites: {v}"
    );
    assert_eq!(context["digest_mismatch_severity"], "none", "{v}");
}
