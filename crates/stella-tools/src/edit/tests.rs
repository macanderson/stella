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
            &serde_json::json!({"path": "f.rs", "old_string": "nope", "new_string": "x"}),
            &cx(dir.path()),
        )
        .await;
    match result {
        ToolOutput::Error { message, class } => {
            assert_eq!(
                message,
                "old_string not found in `f.rs` — no read of this file is recorded \
                 this session; read it first and copy old_string byte-exact"
            );
            assert_eq!(class, Some(stella_protocol::ErrorClass::NotFound));
        }
        other => panic!("expected an error, got {other:?}"),
    }
}

#[tokio::test]
async fn replaces_unique_substring() {
    let dir = std::env::temp_dir();
    let path = format!("stella_edit_{}.rs", std::process::id());
    let full = dir.join(&path);
    tokio::fs::write(&full, "fn main() { old }").await.unwrap();

    let result = EditFile::default()
        .execute(
            &serde_json::json!({"path": path, "old_string": "old", "new_string": "new"}),
            &cx(&dir),
        )
        .await;
    match result {
        ToolOutput::Ok { content, .. } => assert!(content.contains("replaced 1")),
        ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
    }
    let after = tokio::fs::read_to_string(&full).await.unwrap();
    assert_eq!(after, "fn main() { new }");
    let _ = tokio::fs::remove_file(&full).await;
}

/// The #3176 witness: the stagnation detector keys on byte-identical
/// tool output, and the old constant success string made every edit to
/// one file render the same bytes — seven distinct, correct edits were
/// killed as a stuck loop mid-solve. Two DIFFERENT edits back-to-back
/// must produce two different success outputs.
#[tokio::test]
async fn distinct_edits_produce_distinct_success_outputs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("input.tex"), "very big and very large\n").unwrap();
    let edit = EditFile::default();

    let first = edit
        .execute(
            &serde_json::json!({"path": "input.tex", "old_string": "big", "new_string": "huge"}),
            &cx(dir.path()),
        )
        .await;
    let ToolOutput::Ok { content: first, .. } = first else {
        panic!("expected ok, got: {first:?}");
    };
    let second = edit
        .execute(
            &serde_json::json!({"path": "input.tex", "old_string": "large", "new_string": "vast"}),
            &cx(dir.path()),
        )
        .await;
    let ToolOutput::Ok {
        content: second, ..
    } = second
    else {
        panic!("expected ok, got: {second:?}");
    };

    assert_ne!(
        first, second,
        "two different edits must not render byte-identical output — \
         the stagnation detector keys on repeated identical tool output"
    );
    // The identity is stamped, not incidental: offset and digest are
    // both present, so the guarantee survives edits that happen to
    // share one of the two.
    for output in [&first, &second] {
        assert!(output.contains("at byte "), "offset missing: {output}");
        assert!(
            output.contains("file sha256/8 "),
            "digest missing: {output}"
        );
    }
}

#[tokio::test]
async fn errors_on_multiple_without_replace_all() {
    let dir = std::env::temp_dir();
    let path = format!("stella_edit_multi_{}.rs", std::process::id());
    let full = dir.join(&path);
    tokio::fs::write(&full, "a a a").await.unwrap();

    let result = EditFile::default()
        .execute(
            &serde_json::json!({"path": path, "old_string": "a", "new_string": "b"}),
            &cx(&dir),
        )
        .await;
    assert!(result.is_error());
    let _ = tokio::fs::remove_file(&full).await;
}

#[tokio::test]
async fn replace_all_works() {
    let dir = std::env::temp_dir();
    let path = format!("stella_edit_all_{}.rs", std::process::id());
    let full = dir.join(&path);
    tokio::fs::write(&full, "a a a").await.unwrap();

    let result = EditFile::default()
        .execute(
            &serde_json::json!({"path": path, "old_string": "a", "new_string": "b", "replace_all": true}),
            &cx(&dir),
        )
        .await;
    match result {
        ToolOutput::Ok { content, .. } => assert!(content.contains("replaced 3")),
        ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
    }
    let after = tokio::fs::read_to_string(&full).await.unwrap();
    assert_eq!(after, "b b b");
    let _ = tokio::fs::remove_file(&full).await;
}

#[tokio::test]
async fn not_found_without_a_read_names_the_missing_read() {
    let dir = std::env::temp_dir();
    let path = format!("stella_edit_nf_{}.rs", std::process::id());
    let full = dir.join(&path);
    tokio::fs::write(&full, "hello world").await.unwrap();

    let result = EditFile::default()
        .execute(
            &serde_json::json!({"path": path, "old_string": "xyz", "new_string": "abc"}),
            &cx(&dir),
        )
        .await;
    match result {
        ToolOutput::Error { message, .. } => {
            assert!(message.contains("old_string not found"), "got: {message}");
            assert!(
                message.contains("no read of this file is recorded"),
                "an unread file must be attributed as such: {message}"
            );
        }
        ToolOutput::Ok { content, .. } => panic!("expected error, got: {content}"),
    }
    let _ = tokio::fs::remove_file(&full).await;
}

/// The #331 witness: read a file, mutate it out-of-band, then edit with
/// an `old_string` that no longer matches — the error must name the
/// concurrent change (not the generic not-found) and carry the fresh
/// content so the model can re-issue the edit without a round-trip.
#[tokio::test]
async fn drift_is_attributed_and_fresh_content_echoed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "original contents\n").unwrap();
    let ledger = Arc::new(ReadLedger::default());
    let read = ReadFile::with_ledger(ledger.clone());
    let edit = EditFile::with_ledger(ledger.clone());

    let seen = read
        .execute(&serde_json::json!({"path": "a.rs"}), &cx(dir.path()))
        .await;
    assert!(!seen.is_error(), "{seen:?}");

    // Out-of-band change (another process, the user, a subagent).
    std::fs::write(dir.path().join("a.rs"), "rewritten elsewhere\n").unwrap();

    let result = edit
        .execute(
            &serde_json::json!({"path": "a.rs", "old_string": "original", "new_string": "x"}),
            &cx(dir.path()),
        )
        .await;
    match result {
        ToolOutput::Error { message, .. } => {
            assert!(
                message.contains("CHANGED after you last read it"),
                "drift must be attributed: {message}"
            );
            assert!(
                message.contains("rewritten elsewhere"),
                "fresh content must be echoed: {message}"
            );
        }
        ToolOutput::Ok { content, .. } => panic!("expected drift error, got: {content}"),
    }

    // The echo counts as seen: a repeat failure against the SAME bytes is
    // reported as unchanged, not re-attributed as drift forever.
    let repeat = edit
        .execute(
            &serde_json::json!({"path": "a.rs", "old_string": "original", "new_string": "x"}),
            &cx(dir.path()),
        )
        .await;
    match repeat {
        ToolOutput::Error { message, .. } => {
            assert!(
                message.contains("unchanged since you last saw it"),
                "got: {message}"
            );
        }
        ToolOutput::Ok { content, .. } => panic!("expected error, got: {content}"),
    }

    // And the recovery works: an edit against current bytes succeeds.
    let recovered = edit
        .execute(
            &serde_json::json!({"path": "a.rs", "old_string": "rewritten", "new_string": "fixed"}),
            &cx(dir.path()),
        )
        .await;
    assert!(!recovered.is_error(), "{recovered:?}");
}

#[tokio::test]
async fn unchanged_file_failure_is_not_drift_attributed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "hello world\n").unwrap();
    let ledger = Arc::new(ReadLedger::default());
    let read = ReadFile::with_ledger(ledger.clone());
    let edit = EditFile::with_ledger(ledger.clone());

    let seen = read
        .execute(&serde_json::json!({"path": "a.rs"}), &cx(dir.path()))
        .await;
    assert!(!seen.is_error());

    let result = edit
        .execute(
            &serde_json::json!({"path": "a.rs", "old_string": "helo world", "new_string": "x"}),
            &cx(dir.path()),
        )
        .await;
    match result {
        ToolOutput::Error { message, .. } => {
            assert!(
                message.contains("unchanged since you last saw it"),
                "an unchanged file must not be blamed on drift: {message}"
            );
            assert!(
                message.contains("whitespace/newline"),
                "the classic hint stays: {message}"
            );
        }
        ToolOutput::Ok { content, .. } => panic!("expected error, got: {content}"),
    }
}

#[tokio::test]
async fn own_successful_edit_is_not_later_misattributed_as_drift() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "one two three\n").unwrap();
    let ledger = Arc::new(ReadLedger::default());
    let read = ReadFile::with_ledger(ledger.clone());
    let edit = EditFile::with_ledger(ledger.clone());

    let seen = read
        .execute(&serde_json::json!({"path": "a.rs"}), &cx(dir.path()))
        .await;
    assert!(!seen.is_error());

    // The model's own edit changes the file relative to the read…
    let first = edit
        .execute(
            &serde_json::json!({"path": "a.rs", "old_string": "two", "new_string": "2"}),
            &cx(dir.path()),
        )
        .await;
    assert!(!first.is_error(), "{first:?}");

    // …but a subsequent bad old_string is the model's mistake, not drift.
    let second = edit
        .execute(
            &serde_json::json!({"path": "a.rs", "old_string": "bogus", "new_string": "x"}),
            &cx(dir.path()),
        )
        .await;
    match second {
        ToolOutput::Error { message, .. } => {
            assert!(
                message.contains("unchanged since you last saw it"),
                "own edits must update the seen hash: {message}"
            );
        }
        ToolOutput::Ok { content, .. } => panic!("expected error, got: {content}"),
    }
}

/// The CRLF round trip. `read_file` renders a Windows-line-ending file
/// through `str::lines()`, which strips the `\r`; a multi-line
/// `old_string` copied out of that render matched nothing on disk, so
/// EVERY multi-line edit of a CRLF file was impossible — and the tool
/// blamed the model's whitespace for it. The edit must land, and the file
/// must still be CRLF afterwards (an LF island would show up as a
/// whole-file diff in the user's next `git status`).
#[tokio::test]
async fn a_multi_line_edit_of_a_crlf_file_lands_and_keeps_crlf() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("win.rs"), "fn a() {\r\n    old();\r\n}\r\n").unwrap();
    let ledger = Arc::new(ReadLedger::default());
    let read = ReadFile::with_ledger(ledger.clone());
    let edit = EditFile::with_ledger(ledger.clone());

    let seen = read
        .execute(&serde_json::json!({"path": "win.rs"}), &cx(dir.path()))
        .await;
    let ToolOutput::Ok { content, .. } = seen else {
        panic!("expected ok, got: {seen:?}");
    };
    assert!(
        !content.contains('\r'),
        "the render the model copies from has no CR: {content:?}"
    );

    // Exactly what a model copies back out of that render.
    let out = edit
        .execute(
            &serde_json::json!({
                "path": "win.rs",
                "old_string": "fn a() {\n    old();",
                "new_string": "fn a() {\n    new();",
            }),
            &cx(dir.path()),
        )
        .await;
    assert!(!out.is_error(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("win.rs")).unwrap(),
        "fn a() {\r\n    new();\r\n}\r\n",
        "the file keeps its CRLF convention"
    );
}

#[test]
fn crlf_promotion_fires_only_where_it_is_needed() {
    // An LF file needs nothing.
    assert_eq!(crlf_promoted("a\nb\n", "a\nb", "x"), None);
    // A single-line needle matches inside a CRLF line already.
    assert_eq!(crlf_promoted("a\r\nb\r\n", "b", "x"), None);
    // A needle that already carries CR is the model's own bytes — leave it.
    assert_eq!(crlf_promoted("a\r\nb\r\n", "a\r\nb", "x"), None);
    // A needle that is simply absent stays absent (a real not-found).
    assert_eq!(crlf_promoted("a\r\nb\r\n", "zz\nqq", "x"), None);
    // The one case that fires — and the replacement is promoted with it.
    assert_eq!(
        crlf_promoted("a\r\nb\r\n", "a\nb", "p\nq"),
        Some(("a\r\nb".to_string(), "p\r\nq".to_string()))
    );
}

/// The line cap alone never saw this file: 4 MB on ONE line is one line,
/// so it sailed under `DRIFT_ECHO_MAX_LINES` and the whole bundle went to
/// the model inside an *error message* — the one payload nobody budgets
/// for. The width cap must clip it, loudly.
#[test]
fn drift_echo_clips_a_pathologically_long_line() {
    let echo = drift_echo(&"x".repeat(4 * 1024 * 1024));
    assert!(
        echo.len() < 8 * 1024,
        "a 4 MB one-liner must not be echoed whole (got {} bytes)",
        echo.len()
    );
    assert!(echo.contains("bytes elided"), "elision is loud: {echo}");
}

/// Many individually-clipped long lines still add up — the render stops
/// at the total cap and says which cap it hit.
#[test]
fn drift_echo_stops_at_the_total_byte_cap() {
    let body: String = std::iter::repeat_n("y".repeat(4096), 300)
        .collect::<Vec<_>>()
        .join("\n");
    let echo = drift_echo(&body);
    assert!(
        echo.len() < DRIFT_ECHO_MAX_BYTES + 4096,
        "echo stays under the ceiling (got {} bytes)",
        echo.len()
    );
    assert!(
        echo.contains("echo cap"),
        "the footer names the cap: {echo}"
    );
}

#[tokio::test]
async fn drift_echo_is_capped_for_huge_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("big.txt"), "seed\n").unwrap();
    let ledger = Arc::new(ReadLedger::default());
    let read = ReadFile::with_ledger(ledger.clone());
    let edit = EditFile::with_ledger(ledger.clone());

    let seen = read
        .execute(&serde_json::json!({"path": "big.txt"}), &cx(dir.path()))
        .await;
    assert!(!seen.is_error());

    let big: String = (1..=1000).map(|i| format!("line {i}\n")).collect();
    std::fs::write(dir.path().join("big.txt"), &big).unwrap();

    let result = edit
        .execute(
            &serde_json::json!({"path": "big.txt", "old_string": "seed", "new_string": "x"}),
            &cx(dir.path()),
        )
        .await;
    match result {
        ToolOutput::Error { message, .. } => {
            assert!(message.contains("CHANGED after you last read it"));
            assert!(message.contains("line 400"), "echo shows the cap window");
            assert!(
                !message.contains("line 401"),
                "echo must stop at the cap: {}",
                &message[message.len().saturating_sub(200)..]
            );
            assert!(
                message.contains("first 400 of 1000 lines"),
                "truncation must be named: {message}"
            );
        }
        ToolOutput::Ok { content, .. } => panic!("expected drift error, got: {content}"),
    }
}

// ── batching (#4151) ──────────────────────────────────────────────────

/// One call, several edits, across more than one file.
#[tokio::test]
async fn one_call_applies_several_edits_across_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "let a = 1;\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "let b = 2;\n").unwrap();

    let out = EditFile::default()
        .execute(
            &serde_json::json!({"edits": [
                {"path": "a.rs", "old_string": "1", "new_string": "10"},
                {"path": "b.rs", "old_string": "2", "new_string": "20"}
            ]}),
            &cx(dir.path()),
        )
        .await;
    assert!(!out.is_error(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
        "let a = 10;\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("b.rs")).unwrap(),
        "let b = 20;\n"
    );
}

/// **The guarantee `sed -i` cannot offer at any length.**
///
/// A shell chain applies edit 1, fails edit 2, and leaves a tree neither
/// the model nor the turn's diff can describe — the model has to work out
/// which half happened before it can retry. Here a miss anywhere writes
/// nothing, so a failed batch costs a retry rather than a repair.
#[tokio::test]
async fn a_batch_that_fails_midway_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "let a = 1;\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "let b = 2;\n").unwrap();

    let out = EditFile::default()
        .execute(
            &serde_json::json!({"edits": [
                {"path": "a.rs", "old_string": "1", "new_string": "10"},
                {"path": "b.rs", "old_string": "NOT PRESENT", "new_string": "x"}
            ]}),
            &cx(dir.path()),
        )
        .await;
    let ToolOutput::Error { message, .. } = out else {
        panic!("a batch with an unmatchable edit must fail: {out:?}");
    };
    assert!(message.contains("[1]"), "names the failing edit: {message}");
    assert!(message.contains("Nothing was written"), "{message}");
    // The first edit validated cleanly and must STILL not be on disk.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
        "let a = 1;\n",
        "the edit that would have succeeded must not have landed"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("b.rs")).unwrap(),
        "let b = 2;\n"
    );
}

/// Two edits to one file compose in order, so the second sees the first.
/// That is what lets a single call rename a symbol and then edit the line
/// that now mentions it.
#[tokio::test]
async fn two_edits_to_one_file_compose_in_order() {
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
