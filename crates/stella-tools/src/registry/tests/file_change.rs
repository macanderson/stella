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

/// A candidate fixture: announcing, but in the read-only posture a shadow
/// worktree runs in.
fn candidate_fixture() -> (
    tempfile::TempDir,
    ToolRegistry,
    tokio::sync::mpsc::UnboundedReceiver<stella_protocol::AgentEvent>,
) {
    let (dir, reg) = telemetry_fixture();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    reg.attach_read_events(stella_core::EventSender::new(tx));
    (dir, reg, rx)
}

/// The regression: an isolated run showed a completely empty Files tab.
///
/// `create_worktrees` (default `ask`) moves a run into a candidate workspace,
/// and a candidate registry used to be left wholly unattached — the only guard
/// against announcing edits the user's checkout had not received. But that
/// silenced reads too, and reads are most of what a run does: one recorded
/// session made 40 `read_file` calls and 0 `FileChange`s, so the tab read "no
/// files touched yet" from the first tool call to the last.
///
/// Reads announce immediately (they mutate nothing, so they claim nothing);
/// mutations stay withheld for adoption to re-emit.
#[tokio::test]
async fn a_candidate_announces_its_reads_and_withholds_its_mutations() {
    let (dir, reg, mut rx) = candidate_fixture();
    std::fs::write(dir.path().join("r.rs"), "one\ntwo\n").unwrap();

    exec_ok(&reg, "read_file", serde_json::json!({ "path": "r.rs" })).await;
    exec_ok(
        &reg,
        "write_file",
        serde_json::json!({ "path": "new.rs", "content": "a\nb\n" }),
    )
    .await;
    exec_ok(
        &reg,
        "edit_file",
        serde_json::json!({ "path": "r.rs", "old_string": "one", "new_string": "ONE" }),
    )
    .await;

    assert_eq!(
        drain_changes(&mut rx),
        vec![(
            "r.rs".to_string(),
            stella_protocol::FileChangeKind::Read,
            0,
            0,
            false
        )],
        "the read is announced; the create and the edit wait for adoption"
    );
}

/// The #973 invariant, re-pinned across the read-only posture: withholding an
/// *event* must never withhold the *count*. The verification ladder reads
/// `mutations_recorded` precisely because it is immune to which channel — or
/// which posture — the events took.
#[tokio::test]
async fn a_candidate_still_counts_the_mutations_it_withheld() {
    let (dir, reg, mut rx) = candidate_fixture();
    let before = reg.mutations_recorded();
    std::fs::write(dir.path().join("e.rs"), "one\n").unwrap();

    exec_ok(
        &reg,
        "write_file",
        serde_json::json!({ "path": "c.rs", "content": "x\n" }),
    )
    .await;
    exec_ok(
        &reg,
        "edit_file",
        serde_json::json!({ "path": "e.rs", "old_string": "one", "new_string": "ONE" }),
    )
    .await;

    assert_eq!(
        reg.mutations_recorded() - before,
        2,
        "an unannounced mutation is still a mutation"
    );
    assert!(
        drain_changes(&mut rx).is_empty(),
        "and none of them were announced"
    );
}

/// The posture is per-attachment, not sticky: a candidate registry is a
/// candidate for as long as it is one. A registry handed a real turn's channel
/// announces mutations again — otherwise one isolated run would silently blind
/// every later turn that reused the registry.
#[tokio::test]
async fn re_attaching_a_real_turn_restores_mutation_announcements() {
    let (dir, reg, _candidate_rx) = candidate_fixture();
    reg.detach_event_stream();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    reg.attach_events(stella_core::EventSender::new(tx));
    exec_ok(
        &reg,
        "write_file",
        serde_json::json!({ "path": "after.rs", "content": "x\n" }),
    )
    .await;

    let changes = drain_changes(&mut rx);
    assert_eq!(
        changes.iter().map(|c| c.1).collect::<Vec<_>>(),
        vec![stella_protocol::FileChangeKind::Created],
        "the real turn's tree announces its own edits: {changes:?}"
    );
    let _ = dir;
}

/// The counterpart every turn owes `attach_events` — and the whole of #960.
///
/// A one-shot run drops its own sender and then awaits the renderer, which
/// ends when `recv()` returns `None`. But the registry outlives the turn (its
/// ledger is still read for the audit close-out) and it holds an
/// `EventSender` — an `Arc<dyn Fn>` over that very channel. So `recv()` never
/// returned, the renderer task never finished, and a `stella run` that had
/// already printed its terminal event hung until something killed it.
#[tokio::test]
async fn detaching_the_event_stream_closes_the_channel() {
    let (_dir, reg, mut rx) = announcing_fixture();

    // The caller's own sender is already gone — the fixture moved it in — and
    // the channel is *still* open. This is the deadlock, reproduced: anyone
    // waiting for close here waits forever.
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv())
            .await
            .is_err(),
        "while attached, the registry's clone holds the channel open"
    );

    reg.detach_event_stream();

    assert!(
        rx.recv().await.is_none(),
        "detaching must release the registry's sender and close the channel"
    );
}

/// Idempotent, and safe on a registry that never attached either sender — a
/// candidate workspace's, or a second call on a turn that already ended.
#[test]
fn detaching_an_unattached_event_stream_is_a_no_op() {
    let (_dir, reg) = telemetry_fixture();
    reg.detach_event_stream();
    reg.detach_event_stream();
}

/// The drift guard for #973: `mutations_recorded` and the mutating
/// `FileChange`s on the stream are two views of one event, and must never
/// disagree.
///
/// They travel different wires. The event goes to whatever channel a host
/// attached; the count sits on the recorder. That gap is the bug — a verifier
/// wrapping the *engine's* sender saw none of the six changes a real run made
/// and told its verifier `file_change_events=0`, which the verifier read as "the file
/// likely does not exist" while the file sat on disk. Reading the count instead
/// only helps if the two provably agree, so: run a mixed batch of creates,
/// edits, deletes and reads, and assert the delta equals the stream's mutating
/// tally exactly.
#[tokio::test]
async fn the_mutation_count_equals_the_mutating_events_on_the_stream() {
    let (dir, reg, mut rx) = announcing_fixture();
    let before = reg.mutations_recorded();

    std::fs::write(dir.path().join("edit_me.rs"), "one\ntwo\n").unwrap();
    std::fs::write(dir.path().join("delete_me.rs"), "gone\n").unwrap();

    // Create, read (NOT a mutation), edit, bulk-edit the same file again,
    // delete — the shapes a real turn mixes.
    exec_ok(
        &reg,
        "write_file",
        serde_json::json!({ "path": "fresh.rs", "content": "a\nb\nc\n" }),
    )
    .await;
    exec_ok(&reg, "read_file", serde_json::json!({ "path": "fresh.rs" })).await;
    exec_ok(
        &reg,
        "edit_file",
        serde_json::json!({ "path": "edit_me.rs", "old_string": "two", "new_string": "TWO" }),
    )
    .await;
    exec_ok(
        &reg,
        "apply_edits",
        serde_json::json!({
            "edits": [{ "path": "edit_me.rs", "old_string": "one", "new_string": "ONE" }]
        }),
    )
    .await;
    exec_ok(
        &reg,
        "delete_file",
        serde_json::json!({ "path": "delete_me.rs" }),
    )
    .await;

    let streamed = drain_changes(&mut rx);
    let mutating = streamed.iter().filter(|c| c.1.is_mutation()).count() as u64;
    assert!(
        mutating > 0,
        "the fixture must actually mutate something: {streamed:?}"
    );
    assert!(
        streamed.len() as u64 > mutating,
        "and must include a read, so the count is proven to exclude them: {streamed:?}"
    );
    assert_eq!(
        reg.mutations_recorded() - before,
        mutating,
        "the recorder's count must equal the stream's mutating events: {streamed:?}"
    );
}

/// The count is a count of *touches*, not of files: re-editing one file twice
/// is two changes. `files_touched` deduplicates by path and would report one —
/// which is why the verification ladder cannot use its length as a change
/// count.
#[tokio::test]
async fn re_touching_one_file_counts_twice() {
    let (dir, reg) = telemetry_fixture();
    std::fs::write(dir.path().join("f.rs"), "one\ntwo\n").unwrap();
    let before = reg.mutations_recorded();

    exec_ok(
        &reg,
        "edit_file",
        serde_json::json!({ "path": "f.rs", "old_string": "one", "new_string": "ONE" }),
    )
    .await;
    exec_ok(
        &reg,
        "edit_file",
        serde_json::json!({ "path": "f.rs", "old_string": "two", "new_string": "TWO" }),
    )
    .await;

    assert_eq!(reg.mutations_recorded() - before, 2);
    assert_eq!(reg.files_touched().len(), 1, "one file, two changes");
}

/// The count must not depend on a channel being attached. A best-of-N or
/// witness candidate runs against a registry left deliberately unattached (its
/// edits must not be announced as the user's) — and that is exactly the surface
/// the issue found structurally blind, so the count has to survive there.
#[tokio::test]
async fn an_unattached_registry_still_counts_its_mutations() {
    let (_dir, reg) = telemetry_fixture();
    let before = reg.mutations_recorded();

    exec_ok(
        &reg,
        "write_file",
        serde_json::json!({ "path": "quiet.rs", "content": "x\n" }),
    )
    .await;

    assert_eq!(
        reg.mutations_recorded() - before,
        1,
        "an unannounced change is still a change"
    );
}

/// The `.stella/` exclusion (#1537): on a single bench trial the code-graph
/// database and its WAL/SHM sidecars produced 1,518 `file_change` events
/// against 24 tool calls, burying the agent's real edits in binary page
/// churn. Stella's own state directory is not workspace content, so a touch
/// there must reach no ledger, no fact stream, no mutation count, and no
/// `FileChange` — while a task file recorded in the same session behaves
/// exactly as before.
#[tokio::test]
async fn stellas_own_state_directory_is_never_announced() {
    let (_dir, reg, mut rx) = announcing_fixture();
    let before = reg.mutations_recorded();

    exec_ok(
        &reg,
        "write_file",
        serde_json::json!({
            "path": ".stella/private/codegraph.db",
            "content": "binary pages\n",
            "reason": "graph index update",
        }),
    )
    .await;

    assert!(
        drain_changes(&mut rx).is_empty(),
        "state-directory churn must not be announced"
    );
    assert_eq!(
        reg.mutations_recorded(),
        before,
        "state-directory churn is not attempted work"
    );
    assert!(
        reg.files_touched().is_empty(),
        "state-directory churn must not reach the ledger"
    );

    exec_ok(
        &reg,
        "write_file",
        serde_json::json!({ "path": "src/main.rs", "content": "fn main() {}\n" }),
    )
    .await;

    let changes = drain_changes(&mut rx);
    assert_eq!(changes.len(), 1, "task files still announce: {changes:?}");
    assert_eq!(changes[0].0, "src/main.rs");
    assert_eq!(reg.mutations_recorded() - before, 1);
    assert_eq!(reg.files_touched().len(), 1);
}
