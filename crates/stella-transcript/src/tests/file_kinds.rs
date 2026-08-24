//! What each file mutation renders as, and where its line numbers come from.
//!
//! In its own file because `tests.rs` is one line under the 1500-line ratchet
//! and these read as a set: a new file, a deletion, a mutation digest, and the
//! patch that decides whether a modification's gutter is the file's or the
//! fragment's.

use super::*;

/// **The witness for #3577.** A caller holding only the replaced fragment
/// produces a diff numbered from 1, and on a 400-line file those numbers read
/// as the file's. The producer's own patch, when there is one, is drawn as
/// given — same rows, same word spans, the file's line numbers.
#[test]
fn a_producers_patch_is_drawn_at_the_files_own_line_numbers() {
    let fragment = FileChange {
        path: "main.tex".to_string(),
        before: "{15pt}\n".to_string(),
        after: "{12pt}\n".to_string(),
        status: FileStatus::Modified,
        patch: None,
    };
    let from_fragment = FileDiff::build(&fragment);
    assert_eq!(from_fragment.hunks[0].header, "@@ -1,1 +1,1 @@");

    let measured = FileChange {
        patch: Some(Patch {
            text: "--- a/main.tex\n+++ b/main.tex\n@@ -212,1 +212,1 @@\n-{15pt}\n+{12pt}\n"
                .to_string(),
            added: 1,
            removed: 1,
        }),
        ..fragment
    };
    let diff = FileDiff::build(&measured);
    assert_eq!(diff.hunks[0].header, "@@ -212,1 +212,1 @@");
    assert_eq!(
        diff.hunks[0]
            .rows
            .iter()
            .map(|r| (r.old_no, r.new_no))
            .collect::<Vec<_>>(),
        vec![(Some(212), None), (None, Some(212))]
    );
    assert_eq!((diff.added, diff.removed), (1, 1));
    assert!(
        diff.minimal,
        "nothing was compared, so nothing can have fallen back"
    );
    // The pairing that gives a modified line its word spans survives the patch
    // path — it is the same row builder, reached with parsed hunks.
    assert!(diff.hunks[0].rows[0].spans.iter().any(|s| s.changed));
}

#[test]
fn a_new_file_renders_as_an_all_green_diff() {
    let diff = FileDiff::build(&FileChange {
        path: ".latexmkrc".to_string(),
        before: String::new(),
        after: "$pdf_mode = 1;\n$clean_ext = 'aux log';\n".to_string(),
        status: FileStatus::New,
        patch: None,
    });
    assert_eq!(diff.added, 2);
    assert_eq!(diff.removed, 0);
    assert!(
        diff.hunks
            .iter()
            .flat_map(|h| h.rows.iter())
            .all(|r| r.kind == RowKind::Added)
    );
}

#[test]
fn a_deleted_file_renders_as_an_all_red_diff() {
    let diff = FileDiff::build(&FileChange {
        path: "main.aux".to_string(),
        before: "\\relax\n\\gdef\n".to_string(),
        after: String::new(),
        status: FileStatus::Deleted,
        patch: None,
    });
    assert_eq!(diff.removed, 2);
    assert_eq!(diff.added, 0);
    assert_eq!(diff.status.token(), "gone");
}

#[test]
fn a_mutation_digest_names_what_happened_rather_than_making_the_reader_do_arithmetic() {
    let delete = Call {
        tool: ToolKind::DeleteFile,
        header_object: "main.aux".to_string(),
        args: Vec::new(),
        output: Output::default(),
        files: vec![FileChange {
            path: "main.aux".to_string(),
            before: "a\nb\n".to_string(),
            after: String::new(),
            status: FileStatus::Deleted,
            patch: None,
        }],
        status: Status::Ok,
        duration_ms: 4,
        speculated: false,
    };
    let dig = digest::step_digest(&step(delete, 0), 40);
    assert_eq!(dig.delta.unwrap().label(), "deleted · −2");

    let create = Call {
        tool: ToolKind::WriteFile,
        header_object: ".latexmkrc".to_string(),
        args: Vec::new(),
        output: Output::default(),
        files: vec![FileChange {
            path: ".latexmkrc".to_string(),
            before: String::new(),
            after: "a\nb\nc\n".to_string(),
            status: FileStatus::New,
            patch: None,
        }],
        status: Status::Ok,
        duration_ms: 4,
        speculated: false,
    };
    let dig = digest::step_digest(&step(create, 0), 40);
    assert_eq!(dig.delta.unwrap().label(), "new file · +3");

    let dig = digest::step_digest(&step(edit("main.tex", "a\n", "b\n"), 0), 40);
    assert_eq!(dig.delta.unwrap().label(), "+1 −1");
}
