//! Witnesses for the single `FileChange` emission point.
//!
//! `record_touch` is the one place that knows what a file tool did — it holds
//! the pre- and post-images and writes the session's file-touch ledger from
//! them — so it is also the only place that announces the change. These tests
//! pin the properties that made that necessary: bulk `apply_edits` announcing
//! every file in its batch, the CRUD-shaped deltas (a create adds its whole
//! length, a delete removes it, an edit is a real line diff), silence on
//! failure and on a dry run, and — the point of the whole arrangement — that
//! the announced delta is byte-for-byte the ledger's.
//!
//! Before this, the deck synthesized its own events from tool *inputs* in a
//! wrapper that knew four tool names and sat on one of three tool stacks. In a
//! real session that lost 57 of 65 mutating calls, and a file the model had
//! read first still showed a row — reporting `+0 -0` over a rewrite.

use super::*;

/// Fresh registry over a fresh tempdir, no optional backends.
fn telemetry_fixture() -> (tempfile::TempDir, ToolRegistry) {
    let dir = tempfile::tempdir().unwrap();
    let reg = ToolRegistry::with_issue_backend(dir.path().to_path_buf(), None);
    (dir, reg)
}

async fn exec_ok(reg: &ToolRegistry, name: &str, input: serde_json::Value) {
    let out = reg.execute(name, &input).await;
    assert!(!out.is_error(), "{name} {input} failed: {out:?}");
}

/// A fixture whose file changes are announced, plus the receiver.
fn announcing_fixture() -> (
    tempfile::TempDir,
    ToolRegistry,
    tokio::sync::mpsc::UnboundedReceiver<stella_protocol::AgentEvent>,
) {
    let (dir, reg) = telemetry_fixture();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    reg.attach_events(stella_core::EventSender::new(tx));
    (dir, reg, rx)
}

/// Every `FileChange` drained so far, as `(path, kind, added, removed,
/// has_diff)` — the tuple the Files tab actually renders.
fn drain_changes(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<stella_protocol::AgentEvent>,
) -> Vec<(String, stella_protocol::FileChangeKind, u32, u32, bool)> {
    let mut out = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let stella_protocol::AgentEvent::FileChange {
            path,
            kind,
            added,
            removed,
            diff,
        } = event
        {
            out.push((path, kind, added, removed, diff.is_some()));
        }
    }
    out
}

/// The bug behind "the Files tab shows +0/-0 on files I edited": the deck
/// synthesized its own events from tool inputs, and its four-name table
/// never listed `apply_edits` — the tree's BULK edit path, as heavily used
/// as `edit_file`. Its input also has no top-level `path` (the paths live
/// inside `edits[]`), so the call was discarded twice over. Emitting from
/// the recorder fixes both by construction: it already classifies every
/// file in the batch to write the ledger.
#[tokio::test]
async fn apply_edits_announces_every_file_with_its_measured_delta() {
    let (dir, reg, mut rx) = announcing_fixture();
    std::fs::write(dir.path().join("a.rs"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "keep\n").unwrap();

    exec_ok(
        &reg,
        "apply_edits",
        serde_json::json!({
            "edits": [
                { "path": "a.rs", "old_string": "two", "new_string": "TWO\nEXTRA" },
                { "path": "b.rs", "old_string": "keep", "new_string": "kept" },
            ]
        }),
    )
    .await;

    let changes = drain_changes(&mut rx);
    assert_eq!(changes.len(), 2, "one per file in the batch: {changes:?}");
    assert_eq!(changes[0].0, "a.rs");
    assert_eq!(changes[0].1, stella_protocol::FileChangeKind::Modified);
    assert_eq!(
        (changes[0].2, changes[0].3),
        (2, 1),
        "a.rs: `two` became two lines — a real delta, never 0/0"
    );
    assert!(changes[0].4, "and it carries a diff to show");
    assert_eq!(
        (changes[1].0.as_str(), changes[1].2, changes[1].3),
        ("b.rs", 1, 1)
    );
}

/// A validated-but-unwritten batch touched nothing, so it must claim
/// nothing — a `+N` for an edit that never reached disk is worse than a
/// missing row.
#[tokio::test]
async fn a_dry_run_batch_announces_nothing() {
    let (dir, reg, mut rx) = announcing_fixture();
    std::fs::write(dir.path().join("a.rs"), "one\n").unwrap();
    exec_ok(
        &reg,
        "apply_edits",
        serde_json::json!({
            "edits": [{ "path": "a.rs", "old_string": "one", "new_string": "uno" }],
            "dry_run": true,
        }),
    )
    .await;
    assert!(drain_changes(&mut rx).is_empty());
}

/// Create is "N added, 0 removed"; delete is the reverse; an edit is the
/// real line diff. One recorder decides all three, so the CRUD shapes
/// cannot drift apart per surface.
#[tokio::test]
async fn create_edit_and_delete_carry_the_crud_shaped_delta() {
    let (dir, reg, mut rx) = announcing_fixture();

    exec_ok(
        &reg,
        "write_file",
        serde_json::json!({ "path": "new.rs", "content": "a\nb\nc\n" }),
    )
    .await;
    exec_ok(
        &reg,
        "edit_file",
        serde_json::json!({ "path": "new.rs", "old_string": "b", "new_string": "B" }),
    )
    .await;
    exec_ok(&reg, "delete_file", serde_json::json!({ "path": "new.rs" })).await;

    let changes = drain_changes(&mut rx);
    let shapes: Vec<(stella_protocol::FileChangeKind, u32, u32)> =
        changes.iter().map(|c| (c.1, c.2, c.3)).collect();
    assert_eq!(
        shapes,
        vec![
            (stella_protocol::FileChangeKind::Created, 3, 0),
            (stella_protocol::FileChangeKind::Modified, 1, 1),
            (stella_protocol::FileChangeKind::Deleted, 0, 3),
        ],
        "create adds its whole length, delete removes it, an edit diffs"
    );
    assert!(changes.iter().all(|c| c.4), "each carries a diff to show");
    let _ = dir;
}

/// A read is a touch with no delta — it still earns a row (the Files tab
/// counts reads) but must never look like a change.
#[tokio::test]
async fn a_read_announces_itself_with_no_delta_and_no_diff() {
    let (dir, reg, mut rx) = announcing_fixture();
    std::fs::write(dir.path().join("r.rs"), "one\n").unwrap();
    exec_ok(&reg, "read_file", serde_json::json!({ "path": "r.rs" })).await;
    assert_eq!(
        drain_changes(&mut rx),
        vec![(
            "r.rs".to_string(),
            stella_protocol::FileChangeKind::Read,
            0,
            0,
            false
        )]
    );
}

/// A failed tool leaves no trace: the recorder runs on success only, so a
/// refused or impossible write cannot leave a phantom row behind.
#[tokio::test]
async fn a_failed_write_announces_nothing() {
    let (_dir, reg, mut rx) = announcing_fixture();
    let out = reg
        .execute(
            "edit_file",
            &serde_json::json!({
                "path": "missing.rs",
                "old_string": "x",
                "new_string": "y",
            }),
        )
        .await;
    assert!(out.is_error(), "editing a nonexistent file fails");
    assert!(drain_changes(&mut rx).is_empty());
}

/// The announced delta and the ledger's are the same measurement, because
/// they are computed together from one pre/post pair. This is the property
/// the whole change exists for: the TUI, the audit log and the exported
/// telemetry can no longer report different numbers for one edit.
#[tokio::test]
async fn the_announced_delta_equals_the_ledger_and_telemetry_delta() {
    let (dir, reg, mut rx) = announcing_fixture();
    std::fs::write(dir.path().join("a.rs"), "one\ntwo\nthree\n").unwrap();
    exec_ok(
        &reg,
        "edit_file",
        serde_json::json!({
            "path": "a.rs",
            "old_string": "two",
            "new_string": "TWO\nAND\nMORE",
        }),
    )
    .await;

    let changes = drain_changes(&mut rx);
    let (_, _, added, removed, _) = changes[0];
    let payload = reg.file_touch_telemetry();
    let record = payload
        .files_touched
        .iter()
        .find(|r| r.path == "a.rs")
        .expect("the ledger recorded the same touch");
    assert_eq!(
        (added as u64, removed as u64),
        (record.lines_added, record.lines_removed),
        "one measurement, reported identically on both planes"
    );
}

/// A registry nobody attached is silent — the shape a Best-of-N or witness
/// candidate runs in, whose edits live in a shadow worktree and must not be
/// claimed as the user's until adoption.
#[tokio::test]
async fn an_unattached_registry_records_but_announces_nothing() {
    let (dir, reg) = telemetry_fixture();
    exec_ok(
        &reg,
        "write_file",
        serde_json::json!({ "path": "c.rs", "content": "x\n" }),
    )
    .await;
    assert_eq!(
        reg.file_touch_telemetry().files_touched.len(),
        1,
        "the ledger still records it"
    );
    let _ = dir;
}
