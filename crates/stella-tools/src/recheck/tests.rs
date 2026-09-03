//! The witnesses for the last look before a write.
//!
//! They sit beside the check, not in `edit.rs` and `write.rs`. One rule is
//! what they prove. Both tools are just where it is applied.

use std::sync::Arc;

use serde_json::json;
use stella_protocol::tool::ToolOutput;

use super::{Drift, confirm};
use crate::edit::EditFile;
use crate::read::{ReadFile, ReadLedger};
use crate::registry::Tool;
use crate::write::WriteFile;

/// A bare run context rooted at `root`, as every file-tool test builds.
fn cx(root: impl AsRef<std::path::Path>) -> crate::ctx::ToolCtx {
    crate::ctx::ToolCtx::bare(root.as_ref().to_path_buf())
}

/// A seam that writes `bytes` to `file` when the tool reaches its gap.
fn writes(file: std::path::PathBuf, bytes: &'static str) -> super::Seam {
    Arc::new(move || std::fs::write(&file, bytes).expect("the concurrent write"))
}

#[tokio::test]
async fn confirm_passes_the_bytes_it_was_given_and_fails_any_other() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "before\n").unwrap();
    let handle = Arc::new(crate::rootfd::RootHandle::open(dir.path()).expect("root"));
    let sha = crate::staleness::hex_sha256(b"before\n");

    assert!(confirm(&handle, "f.txt", &sha).await.is_ok());

    std::fs::write(dir.path().join("f.txt"), "after\n").unwrap();
    match confirm(&handle, "f.txt", &sha).await {
        Err(Drift::Rewritten { fresh }) => assert_eq!(fresh.as_deref(), Some("after\n")),
        other => panic!("a rewritten file must read as drift: {:?}", other.is_ok()),
    }

    std::fs::remove_file(dir.path().join("f.txt")).unwrap();
    match confirm(&handle, "f.txt", &sha).await {
        Err(Drift::Unreadable(_)) => {}
        other => panic!("a file that vanished must refuse: {:?}", other.is_ok()),
    }
}

/// The witness for `edit_file`.
///
/// `notes.md` holds three lines. The model has read it. Another writer adds a
/// fourth line in the gap between the read and the write. The seam puts it
/// there every run.
///
/// The edit's needle is on line 2. It still matches the stale copy, so the
/// miss path never runs. That path is the only one that ever asked the ledger.
///
/// Without the check the tool writes its three-line copy, `delta` goes, and
/// the call says "replaced 1 occurrence(s)". Take the `confirm` call out of
/// `edit_one` and the first assert below fails.
#[tokio::test]
async fn a_concurrent_append_survives_an_edit_that_did_not_touch_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.md");
    std::fs::write(&file, "alpha\nbeta\ngamma\n").unwrap();

    let ledger = Arc::new(ReadLedger::default());
    let seen = ReadFile::with_ledger(ledger.clone())
        .execute(&json!({"path": "notes.md"}), &cx(dir.path()))
        .await;
    assert!(!seen.is_error(), "the seeding read: {seen:?}");

    let out = EditFile::with_seam(ledger, writes(file.clone(), "alpha\nbeta\ngamma\ndelta\n"))
        .execute(
            &json!({"path": "notes.md", "old_string": "beta", "new_string": "BETA"}),
            &cx(dir.path()),
        )
        .await;

    let after = std::fs::read_to_string(&file).unwrap();
    assert!(
        after.contains("delta\n"),
        "the concurrent write must survive; disk holds {after:?} and the tool said {out:?}"
    );
    let ToolOutput::Error { message, class } = out else {
        panic!("an edit computed from bytes that are gone must not land: {out:?}");
    };
    assert!(message.contains("refusing to write"), "{message}");
    assert!(
        message.contains("delta"),
        "the fresh content is echoed so the model can re-issue: {message}"
    );
    assert_eq!(class, Some(stella_protocol::ErrorClass::RefusedByPolicy));
}

/// The batch form has the same gap. It has one more rule to hold: a batch
/// refused on its second file must not have written its first.
#[tokio::test]
async fn a_batch_refuses_whole_when_one_of_its_files_changed_under_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "let a = 1;\n").unwrap();
    let b = dir.path().join("b.rs");
    std::fs::write(&b, "let b = 2;\n").unwrap();

    let out = EditFile::with_seam(
        Arc::new(ReadLedger::default()),
        writes(b.clone(), "let b = 2;\nlet c = 3;\n"),
    )
    .execute(
        &json!({"edits": [
            {"path": "a.rs", "old_string": "1", "new_string": "10"},
            {"path": "b.rs", "old_string": "2", "new_string": "20"}
        ]}),
        &cx(dir.path()),
    )
    .await;

    let ToolOutput::Error { message, .. } = out else {
        panic!("the batch was composed from bytes that are gone: {out:?}");
    };
    assert!(message.contains("Nothing was written"), "{message}");
    assert_eq!(
        std::fs::read_to_string(&b).unwrap(),
        "let b = 2;\nlet c = 3;\n",
        "the concurrent write survives"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
        "let a = 1;\n",
        "the file that would have succeeded must not have landed"
    );
}

/// `write_file` has the widest gap. It does no read of its own. The content
/// it writes comes from a `read_file` that may be many turns old.
///
/// Coverage says the model saw the file. It never says the file still holds
/// what the model saw.
///
/// This one needs no seam. The change happens in the open, between two tool
/// calls. Paste it onto `main` and it fails there: the write lands and `beta`
/// is gone.
#[tokio::test]
async fn an_overwrite_of_a_file_that_changed_since_the_read_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.md");
    std::fs::write(&file, "alpha\n").unwrap();

    let ledger = Arc::new(ReadLedger::default());
    let seen = ReadFile::with_ledger(ledger.clone())
        .execute(&json!({"path": "notes.md"}), &cx(dir.path()))
        .await;
    assert!(!seen.is_error(), "the seeding read: {seen:?}");

    // Another writer, in between.
    std::fs::write(&file, "alpha\nbeta\n").unwrap();

    let out = WriteFile::with_ledger(ledger)
        .execute(
            &json!({"path": "notes.md", "content": "ALPHA\n"}),
            &cx(dir.path()),
        )
        .await;

    let ToolOutput::Error { message, class } = out else {
        panic!("the content was composed from bytes that are gone: {out:?}");
    };
    assert!(message.contains("refusing to overwrite"), "{message}");
    assert_eq!(class, Some(stella_protocol::ErrorClass::RefusedByPolicy));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "alpha\nbeta\n",
        "the concurrent write survives"
    );
}

/// The batch form of the same. It adds the all-or-nothing half: nothing is
/// written when one file of the batch changed.
#[tokio::test]
async fn a_write_batch_refuses_whole_when_one_of_its_files_changed_since_the_read() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("notes.md");
    std::fs::write(&existing, "alpha\n").unwrap();

    let ledger = Arc::new(ReadLedger::default());
    let seen = ReadFile::with_ledger(ledger.clone())
        .execute(&json!({"path": "notes.md"}), &cx(dir.path()))
        .await;
    assert!(!seen.is_error(), "the seeding read: {seen:?}");

    std::fs::write(&existing, "alpha\nbeta\n").unwrap();

    let out = WriteFile::with_ledger(ledger)
        .execute(
            &json!({"files": [
                {"path": "fresh.md", "content": "new\n"},
                {"path": "notes.md", "content": "ALPHA\n"}
            ]}),
            &cx(dir.path()),
        )
        .await;

    let ToolOutput::Error { message, .. } = out else {
        panic!("the batch was composed from bytes that are gone: {out:?}");
    };
    assert!(message.contains("Nothing was written"), "{message}");
    assert!(
        !dir.path().join("fresh.md").exists(),
        "the file that would have succeeded must not have landed"
    );
    assert_eq!(std::fs::read_to_string(&existing).unwrap(), "alpha\nbeta\n");
}

/// The gate has to open. An edit with nothing racing it still lands. That is
/// what keeps this a check and not a wall.
#[tokio::test]
async fn an_uncontested_edit_still_lands() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.md");
    std::fs::write(&file, "alpha\nbeta\n").unwrap();

    let out = EditFile::default()
        .execute(
            &json!({"path": "notes.md", "old_string": "beta", "new_string": "BETA"}),
            &cx(dir.path()),
        )
        .await;
    assert!(!out.is_error(), "{out:?}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "alpha\nBETA\n");
}
