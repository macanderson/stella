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
        extent: Extent::default(),
        patch: None,
    };
    let from_fragment = FileDiff::build(&fragment);
    assert_eq!(from_fragment.hunks[0].header, "@@ -1,1 +1,1 @@");

    let measured = FileChange {
        extent: Extent::delta(1, 1),
        patch: Some(Patch {
            text: "--- a/main.tex\n+++ b/main.tex\n@@ -212,1 +212,1 @@\n-{15pt}\n+{12pt}\n"
                .to_string(),
            minimal: true,
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
    assert_eq!(diff.extent, Extent::delta(1, 1));
    assert!(diff.minimal, "the producer reported an exact diff");
    // The pairing that gives a modified line its word spans survives the patch
    // path — it is the same row builder, reached with parsed hunks.
    assert!(diff.hunks[0].rows[0].spans.iter().any(|s| s.changed));
}

/// **The witness for #4696.** A producer's patch that tripped
/// `LCS_AREA_CAP` carries `minimal: false` on the wire; the build must read
/// that flag rather than assume every patch is exact.
#[test]
fn a_producers_blunt_fallback_patch_stays_marked_non_minimal() {
    let change = FileChange {
        path: "big.txt".to_string(),
        before: String::new(),
        after: String::new(),
        status: FileStatus::Modified,
        extent: Extent::delta(1, 1),
        patch: Some(Patch {
            text: "--- a/big.txt\n+++ b/big.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n".to_string(),
            minimal: false,
        }),
    };
    assert!(
        !FileDiff::build(&change).minimal,
        "the producer's own cap trip must survive onto the rendered diff"
    );
}

#[test]
fn a_new_file_renders_as_an_all_green_diff() {
    let diff = FileDiff::build(&FileChange {
        path: ".latexmkrc".to_string(),
        before: String::new(),
        after: "$pdf_mode = 1;\n$clean_ext = 'aux log';\n".to_string(),
        status: FileStatus::New,
        extent: Extent::default(),
        patch: None,
    });
    assert_eq!(diff.extent, Extent::delta(2, 0));
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
        extent: Extent::default(),
        patch: None,
    });
    assert_eq!(diff.extent, Extent::delta(0, 2));
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
            extent: Extent::default(),
            patch: None,
        }],
        status: Status::Ok,
        duration_ms: 4,
        speculated: false,
        sub_agent_id: None,
    };
    let dig = digest::step_digest(&step(delete, 0), 40);
    assert_eq!(dig.delta.unwrap().label().as_deref(), Some("deleted · −2"));

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
            extent: Extent::default(),
            patch: None,
        }],
        status: Status::Ok,
        duration_ms: 4,
        speculated: false,
        sub_agent_id: None,
    };
    let dig = digest::step_digest(&step(create, 0), 40);
    assert_eq!(dig.delta.unwrap().label().as_deref(), Some("new file · +3"));

    let dig = digest::step_digest(&step(edit("main.tex", "a\n", "b\n"), 0), 40);
    assert_eq!(dig.delta.unwrap().label().as_deref(), Some("+1 −1"));
}

/// **Witness (#4289 piece 1).** A row can exist before anything measured it,
/// and the model has to be able to say so.
///
/// A journal replay of a `delete_file` is that row: the call's arguments name
/// the path and carry neither side of the file, so there is nothing to compare
/// and no producer count. Rendering it used to run `unified_diff("", "")`,
/// which measures nothing and returns `0`, and the header then read `−0` —
/// a claim that the deletion removed no lines, over a file whose size nobody
/// knew. The size column is absent instead, which is what
/// [`crate::model::Extent`] is for.
#[test]
fn a_change_nothing_measured_renders_no_size_at_all() {
    let unmeasured = FileChange {
        path: "main.aux".to_string(),
        before: String::new(),
        after: String::new(),
        status: FileStatus::Deleted,
        extent: Extent::default(),
        patch: None,
    };
    let diff = FileDiff::build(&unmeasured);
    assert_eq!(
        diff.extent,
        Extent::default(),
        "comparing nothing to nothing measures nothing"
    );

    let call = Call {
        tool: ToolKind::DeleteFile,
        header_object: "main.aux".to_string(),
        args: Vec::new(),
        output: Output::default(),
        files: vec![unmeasured],
        status: Status::Ok,
        duration_ms: 4,
        speculated: false,
        sub_agent_id: None,
    };
    assert_eq!(call.extent(), Extent::default());

    // The word survives — the tool named the act — and the count does not
    // appear at all. Asserting the absence, not a zero.
    let label = digest::step_digest(&step(call.clone(), 0), 40)
        .delta
        .expect("a deletion still says it deleted")
        .label();
    assert_eq!(label.as_deref(), Some("deleted"));

    let run = run_with(vec![step(call, 0)]);
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Everything);
    let text = grid::to_plain(&grid::render(&run, &state, 120));
    assert!(
        !text.contains("−0") && !text.contains("+0"),
        "a zero was drawn for a size nobody measured:\n{text}"
    );
    assert!(
        text.contains("deleted"),
        "the act itself must still be stated:\n{text}"
    );

    // Both renderers, because the rule is the model's rather than either
    // painter's: the web surface is where a replayed deletion is actually read.
    let html = html::render_run(&run, &state);
    assert!(
        !html.contains("−0") && !html.contains("+0"),
        "a zero was drawn for a size nobody measured:\n{html}"
    );
    assert!(
        html.contains("deleted"),
        "the act itself must still be stated"
    );
}

/// A modification nobody measured has no word of its own, so it renders no
/// size column at all rather than an empty one.
#[test]
fn an_unmeasured_modification_has_no_size_column() {
    let call = Call {
        tool: ToolKind::EditFile,
        header_object: "main.tex".to_string(),
        args: Vec::new(),
        output: Output::default(),
        files: vec![FileChange {
            path: "main.tex".to_string(),
            before: String::new(),
            after: String::new(),
            status: FileStatus::Modified,
            extent: Extent::default(),
            patch: None,
        }],
        status: Status::Ok,
        duration_ms: 4,
        speculated: false,
        sub_agent_id: None,
    };
    assert!(digest::step_digest(&step(call, 0), 40).delta.is_none());
}
