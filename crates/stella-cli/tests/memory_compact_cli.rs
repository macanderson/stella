//! End-to-end witness for `stella memory compact`: drive the real binary
//! against a real workspace `context.db`.
//!
//! Modelled on `stats_prune_cli.rs`. The predicates and the one-transaction
//! guarantee are covered by `stella-context`'s own tests; what only the binary
//! can show is the part that lives at the CLI seam — that an unindexed workspace
//! is not given a database as a side effect of asking about it, that `--dry-run`
//! is a preview rather than a confirm prompt, and that a memory forgotten
//! through `stella memory forget` is still restorable after a compaction.
//!
//! The workspace's `context.db` is seeded through `stella_context` directly
//! rather than by running a turn: the agent path needs a model provider, and
//! what is under test is the compaction verb, not reflection.

use std::path::Path;
use std::process::{Command, Output};

use stella_context::{ContextDelta, ContextStore, MemoryInput};

fn context_db(workspace: &Path) -> std::path::PathBuf {
    workspace.join(".stella/private/context.db")
}

fn count(workspace: &Path, table: &str) -> i64 {
    let conn = rusqlite::Connection::open(context_db(workspace)).expect("open context.db");
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .expect("count")
}

/// A workspace whose context plane holds one memory that has been revised
/// `edits` times — so `edits` vectors are stranded and exactly `edits` are
/// reclaimable.
fn seeded(edits: usize) -> tempfile::TempDir {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::create_dir_all(workspace.path().join(".stella/private")).expect("private dir");
    let store = ContextStore::open(context_db(workspace.path())).expect("context store");
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        store
            .upsert(
                ContextDelta::new()
                    .with_memory(MemoryInput::reflection("revision 0", Vec::<String>::new())),
            )
            .await
            .expect("seed memory");
        let node_id = store.memory_nodes().expect("nodes")[0].public_id.clone();
        let lineage = store
            .memory_lineage(&node_id)
            .expect("lineage")
            .expect("id");
        for n in 1..=edits {
            store
                .upsert(
                    ContextDelta::new().with_memory(
                        MemoryInput::reflection(format!("revision {n}"), Vec::<String>::new())
                            .revises(&lineage),
                    ),
                )
                .await
                .expect("revise memory");
        }
    });
    workspace
}

fn run(workspace: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stella"))
        .args(args)
        .current_dir(workspace)
        .env("STELLA_DATA_DIR", workspace.join("data"))
        .output()
        .expect("run stella")
}

fn ok(workspace: &Path, args: &[&str]) -> String {
    let output = run(workspace, args);
    assert!(
        output.status.success(),
        "stella {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Asking a workspace what it could reclaim must not create the thing being
/// asked about. `stella stats prune` keeps the same courtesy for `store.db`, and
/// for the context plane it matters more: opening it runs migrations and writes
/// an embedder-fingerprint row, so a stray `compact` in an un-indexed repo would
/// leave a `.stella/private/` behind.
#[test]
fn compacting_a_workspace_with_no_context_store_creates_nothing() {
    let workspace = tempfile::tempdir().expect("workspace");
    let out = ok(workspace.path(), &["memory", "compact"]);
    assert!(
        out.contains("no context store in this workspace"),
        "an unindexed workspace is a state, not an error: {out}"
    );
    assert!(
        !context_db(workspace.path()).exists(),
        "and asking must not create the database"
    );
}

/// The reclaim, end to end: three revisions leave two unreachable vectors, and
/// the verb takes exactly those.
#[test]
fn compact_reclaims_the_vectors_that_memory_edits_stranded() {
    let workspace = seeded(2);
    assert_eq!(count(workspace.path(), "embedding"), 3, "one per revision");

    let out = ok(workspace.path(), &["memory", "compact", "--vacuum"]);
    assert!(
        out.contains("reclaimed 2 orphaned embedding(s)"),
        "the reclaim is reported per class: {out}"
    );
    assert!(out.contains("vacuumed"), "{out}");
    assert!(
        out.contains("compaction watermark:"),
        "and the durable record of it is shown: {out}"
    );
    assert_eq!(count(workspace.path(), "embedding"), 1);
    assert_eq!(
        count(workspace.path(), "memory"),
        3,
        "every revision survives — compaction reclaims the index, not the record (L-C3)"
    );
}

/// `--dry-run` is the preview, and it is the ONLY preview: there is no
/// interactive confirm to hang a script on.
#[test]
fn a_dry_run_previews_the_compaction_without_deleting_anything() {
    let workspace = seeded(2);
    let out = ok(workspace.path(), &["memory", "compact", "--dry-run"]);
    assert!(
        out.contains("would reclaim 2 orphaned embedding(s)"),
        "a dry run speaks in the conditional: {out}"
    );
    assert!(
        !out.contains("compaction watermark:"),
        "and stamps nothing: {out}"
    );
    assert_eq!(count(workspace.path(), "embedding"), 3, "nothing deleted");
}

/// A second pass has nothing left, and says so in the vocabulary of the store
/// rather than of the invocation — "nothing to compact" is a fact about the
/// data, not a usage error.
#[test]
fn a_second_compaction_reports_an_already_clean_store() {
    let workspace = seeded(1);
    ok(workspace.path(), &["memory", "compact"]);
    let out = ok(workspace.path(), &["memory", "compact"]);
    assert!(
        out.contains("nothing to compact"),
        "an already-clean store is a state, not an error: {out}"
    );
}

/// The exclusion that matters most at this seam: `forget` writes a tombstone in
/// `store.db` and projects it onto `node.superseded_at`, and `restore` resolves
/// back through that surviving row. A compaction between the two must leave the
/// undo intact — vector included, so the restored memory does not need a
/// re-embed to be recallable again.
#[test]
fn a_memory_forgotten_before_a_compaction_is_still_restorable_after_it() {
    let workspace = seeded(1);
    let listed = ok(workspace.path(), &["memory", "list"]);
    let id = listed
        .split_whitespace()
        .find(|token| token.starts_with("nod_"))
        .unwrap_or_else(|| panic!("no memory id in listing: {listed}"))
        .to_string();

    ok(workspace.path(), &["memory", "forget", &id]);
    let out = ok(workspace.path(), &["memory", "compact"]);
    assert!(
        out.contains("reclaimed 1 orphaned embedding(s)"),
        "the superseded revision's vector still goes: {out}"
    );

    let restored = ok(workspace.path(), &["memory", "restore", &id]);
    assert!(restored.contains("restored"), "{restored}");
    let listed = ok(workspace.path(), &["memory", "list"]);
    assert!(
        listed.contains(&id),
        "the memory is back after forget -> compact -> restore: {listed}"
    );
    assert_eq!(
        count(workspace.path(), "embedding"),
        1,
        "with its vector intact — a restore must never need a re-embed"
    );
}
