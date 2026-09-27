use super::*;

/// A bare execution context rooted at `root` — every file-tool test
/// drives the tool through one, since `Tool::execute` takes the
/// context rather than the bare root path it used to (#3284).
fn cx(root: impl AsRef<std::path::Path>) -> crate::ctx::ToolCtx {
    crate::ctx::ToolCtx::bare(root.as_ref().to_path_buf())
}
use crate::read::ReadFile;

/// The observed failure this diagnoses: a needle that is byte-correct
/// except for a missing list indent. The generic "check for exact
/// whitespace" message is true but costs a ranged re-read to act on.
#[test]
fn an_indentation_only_miss_hands_back_the_files_own_bytes() {
    let content = "prose\n   - one\n   - two\nmore\n";
    let hit = indentation_only_match(content, "- one\n- two").expect("the span");
    assert_eq!(
        hit, "   - one\n   - two",
        "the file's indentation, verbatim"
    );
}

/// The hint must never claim a match the real edit would not have made.
#[test]
fn the_hint_declines_when_it_would_be_a_guess() {
    // Genuinely absent text is not an indentation problem.
    assert_eq!(indentation_only_match("a\nb\n", "- nope"), None);
    // Two candidate spans: pointing at either one could be wrong.
    assert_eq!(indentation_only_match("  x\nsep\n    x\n", "x"), None);
    // Trailing whitespace is a real difference the model must reproduce,
    // and the generic message already covers it.
    assert_eq!(indentation_only_match("  keep  \n", "keep"), None);
    // Nothing to have mis-indented.
    assert_eq!(indentation_only_match("\n\n", "   "), None);
}

/// The #3167 witness: `edit_file`'s "no read recorded" refusal renders
/// the exact prose it always has — the loop detector and the prompt
/// cache compare those bytes — but now carries the honest
/// [`stella_protocol::ErrorClass::NotFound`] instead of the `class: None`
/// every refusal shared before this sweep classified them.
#[tokio::test]
async fn a_needle_missing_with_no_prior_read_keeps_its_prose_and_gains_a_class() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.rs"), "fn main() {}\n").unwrap();

    let result = EditFile::default()
        .execute(
            &serde_json::json!({"edits": [{"path": "f.rs", "old_string": "NOT", "new_string": "x"}]}),
            &cx(dir.path()),
        )
        .await;
    let ToolOutput::Error { message, class } = result else {
        panic!("expected error");
    };
    assert_eq!(class, Some(stella_protocol::ErrorClass::NotFound));
    assert!(
        message.contains("No record of a prior read"),
        "the prose must be exact, for the loop detector: {message}"
    );
}

/// An edit with a read ledger compares against it; one without records the
/// canonical state as it rolls forward (the composed file after each edit).
#[tokio::test]
async fn an_edit_with_no_ledger_composses_in_place() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
    let out = EditFile::default()
        .execute(
            &serde_json::json!({"edits": [{
                "path": "a.rs",
                "old_string": "fn a() {}",
                "new_string": "fn a() { // noop"
            }]}),
            &cx(dir.path()),
        )
        .await;
    assert!(!out.is_error(), "edit must apply: {out:?}");

    // The next edit must see the new content (composition) and must not
    // require a prior read (ledger is bare).
    let out = EditFile::default()
        .execute(
            &serde_json::json!({"edits": [{
                "path": "a.rs",
                "old_string": "// noop",
                "new_string": "// yep"
            }]}),
            &cx(dir.path()),
        )
        .await;
    assert!(
        !out.is_error(),
        "the composed content must be reachable: {out:?}"
    );
}

/// #3502: `edit_file` must flag any mismatches it cannot resolve, even if
/// it previously succeeded. A hash mismatch means the file has changed
/// behind the tool's back, and once **one** edit has moved the baseline,
/// any later edit **cannot** be sure of what a missing needle would have
/// matched. Continuing is unsafe.
#[tokio::test]
async fn a_file_that_drifts_between_edits_stops_the_batch() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
    let ledger = Arc::new(ReadLedger::default());
    ledger.record_known(dir.path(), "a.rs", "fn a() {}\n");

    let out = EditFile::with_ledger(ledger.clone())
        .execute(
            &serde_json::json!({"edits": [
                {"path": "a.rs", "old_string": "fn a()", "new_string": "fn b()"}
            ]}),
            &cx(dir.path()),
        )
        .await;
    assert!(!out.is_error(), "first edit ok: {out:?}");

    // Edit the file out of band, breaking the ledger's expectation.
    std::fs::write(dir.path().join("a.rs"), "fn b() { // modified\n").unwrap();

    let out = EditFile::with_ledger(ledger)
        .execute(
            &serde_json::json!({"edits": [
                {"path": "a.rs", "old_string": "modified", "new_string": "clean"}
            ]}),
            &cx(dir.path()),
        )
        .await;
    let ToolOutput::Error { message, .. } = out else {
        panic!("expected error for drifted file: {out:?}");
    };
    assert!(
        message.contains("out-of-band"),
        "the cause must be named clearly: {message}"
    );
}

/// An edit that touches a file but the ledger knows nothing about it can
/// proceed, because `edit_one` will record it once it compiles successfully.
#[tokio::test]
async fn an_edit_can_bootstrap_an_unrecorded_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
    let ledger = Arc::new(ReadLedger::default());
    // Ledger is empty; no prior read of a.rs.

    let out = EditFile::with_ledger(ledger)
        .execute(
            &serde_json::json!({"edits": [{"path": "a.rs", "old_string": "main", "new_string": "a"}]}),
            &cx(dir.path()),
        )
        .await;
    assert!(!out.is_error(), "bootstrap must work: {out:?}");
}

/// The first edit of a batch compiles fine. The second tries to reference a
/// function the first edit *changed*, by its old name. The batch must fail
/// with "not found" — the first edit removed the old name. The batch rule
/// lets a later edit assume the first edit already landed.
#[tokio::test]
async fn a_later_edit_in_a_batch_sees_the_composed_result_of_an_earlier_edit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn old_name() {}\n").unwrap();

    let out = EditFile::default()
        .execute(
            &serde_json::json!({"edits": [
                {"path": "a.rs", "old_string": "old_name", "new_string": "new_name"},
                {"path": "a.rs", "old_string": "fn new_name() {}", "new_string": "pub fn new_name() {}"}
            ]}),
            &cx(dir.path()),
        )
        .await;
    assert!(
        !out.is_error(),
        "the second edit must see the first: {out:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
        "pub fn new_name() {}\n"
    );
}

/// A batch must not cry drift at its own work: once an earlier edit has
/// changed the file, the composed content no longer matches the ledger, and
/// calling that an out-of-band modification sends the model hunting for a
/// second writer that is itself.
#[tokio::test]
async fn a_batch_does_not_report_its_own_earlier_edit_as_drift() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "let a = 1;\n").unwrap();
    let ledger = Arc::new(ReadLedger::default());
    ledger.record_known(dir.path(), "a.rs", "let a = 1;\n");

    let out = EditFile::with_ledger(ledger)
        .execute(
            &serde_json::json!({"edits": [
                {"path": "a.rs", "old_string": "1", "new_string": "10"},
                {"path": "a.rs", "old_string": "NOT PRESENT", "new_string": "x"}
            ]}),
            &cx(dir.path()),
        )
        .await;
    let ToolOutput::Error { message, .. } = out else {
        panic!("expected the second edit to miss: {out:?}");
    };
    assert!(
        !message.contains("out-of-band"),
        "the batch's own edit must not be reported as drift: {message}"
    );
    assert!(
        message.contains("earlier edit in this same batch"),
        "the real cause has to be named: {message}"
    );
}
