// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A mutating row resolves the change its own call made (#4155).
//!
//! Which turn's edit claims which diff, and what a row shows when the answer
//! is "none". The block `model/tests.rs` already fenced under its own `#4155`
//! banner, with the two helpers only these tests use.
//!
//! Split out at the gate's 1500-line ceiling (#5225), along the seam the file
//! had drawn for itself.

use super::*;

/// The live cause of #4155: a successful edit rendered no diff and no
/// `+N −M`, because the seq the row stamped named the change *before* its own.
#[test]
fn an_edit_result_resolves_the_change_its_own_call_made() {
    let mut model = SessionModel::new();
    edit_call(&mut model, "c1", "src/a.rs");
    turn_boundary(
        &mut model,
        "src/a.rs",
        Some("@@ -1 +1,2 @@\n+first\n"),
        2,
        1,
    );

    let dref = inline_ref(&model, "c1").expect("a successful edit keeps an inline-diff ref");
    let file = model.files.iter().find(|f| f.path == "src/a.rs").unwrap();
    assert_eq!(
        file.diff_at(dref.seq),
        Some("@@ -1 +1,2 @@\n+first\n"),
        "the row resolves the diff of the change its own call produced"
    );
    assert_eq!(
        file.delta_at(dref.seq),
        Some((2, 1)),
        "and the measurement that rides with it"
    );
}

/// The half the off-by-one hid: it did not merely blank the row, it pointed
/// every turn after the first at the PREVIOUS turn's change to that path —
/// the misattribution `render::resolve_inline_diff` exists to prevent.
#[test]
fn a_later_turns_edit_never_renders_an_earlier_turns_diff() {
    let mut model = SessionModel::new();
    edit_call(&mut model, "c1", "src/a.rs");
    turn_boundary(&mut model, "src/a.rs", Some("@@ first turn @@\n"), 1, 0);
    edit_call(&mut model, "c2", "src/a.rs");
    turn_boundary(&mut model, "src/a.rs", Some("@@ second turn @@\n"), 3, 2);

    let file = model.files.iter().find(|f| f.path == "src/a.rs").unwrap();
    let first = inline_ref(&model, "c1").expect("turn one keeps its ref");
    let second = inline_ref(&model, "c2").expect("turn two keeps its ref");
    assert_ne!(first.seq, second.seq, "two turns, two distinct changes");
    assert_eq!(file.diff_at(first.seq), Some("@@ first turn @@\n"));
    assert_eq!(
        file.diff_at(second.seq),
        Some("@@ second turn @@\n"),
        "turn two's row shows turn two's change, not the one before it"
    );
}

/// One aggregate change per path per turn, so exactly one row may claim it.
/// Stamping every call that touched the path would render the turn's whole
/// change under each of them; the last keeps it, the earlier ones degrade to
/// naming their change.
#[test]
fn only_the_last_call_to_a_path_in_a_turn_claims_the_turns_change() {
    let mut model = SessionModel::new();
    edit_call(&mut model, "c1", "src/a.rs");
    edit_call(&mut model, "c2", "src/a.rs");
    turn_boundary(&mut model, "src/a.rs", Some("@@ both edits @@\n"), 4, 1);

    assert!(
        inline_ref(&model, "c1").is_none(),
        "the superseded row gives up its ref rather than restating the aggregate"
    );
    let last = inline_ref(&model, "c2").expect("the last call keeps it");
    let file = model.files.iter().find(|f| f.path == "src/a.rs").unwrap();
    assert_eq!(file.diff_at(last.seq), Some("@@ both edits @@\n"));
}

/// Supersession is per path: two files edited in one turn keep one row each.
#[test]
fn calls_to_different_paths_in_one_turn_each_keep_their_own_ref() {
    let mut model = SessionModel::new();
    edit_call(&mut model, "c1", "src/a.rs");
    edit_call(&mut model, "c2", "src/b.rs");
    turn_boundary(&mut model, "src/a.rs", Some("@@ a @@\n"), 1, 0);
    turn_boundary(&mut model, "src/b.rs", Some("@@ b @@\n"), 2, 0);

    for (call, path, text) in [
        ("c1", "src/a.rs", "@@ a @@\n"),
        ("c2", "src/b.rs", "@@ b @@\n"),
    ] {
        let dref = inline_ref(&model, call).expect("each path's row keeps its ref");
        let file = model.files.iter().find(|f| f.path == path).unwrap();
        assert_eq!(file.diff_at(dref.seq), Some(text));
    }
}

/// A turn that measured no net change to the path leaves the ref dangling,
/// and a dangling ref renders nothing. An edit reverted within the same turn
/// changed nothing on disk, so the row has nothing to show.
#[test]
fn a_turn_that_measured_no_change_leaves_the_row_silent() {
    let mut model = SessionModel::new();
    edit_call(&mut model, "c1", "src/a.rs");
    let dref = inline_ref(&model, "c1").expect("the ref is still stamped");
    assert_eq!(dref.path, "src/a.rs");
    assert!(
        model.files.iter().all(|f| f.path != "src/a.rs"),
        "nothing measured the path, so nothing resolves"
    );
}

/// A failed mutation still carries no reference: the change it would point at
/// is one it never made.
#[test]
fn a_failed_mutation_keeps_no_inline_diff_ref() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c1".into(),
            name: "edit_file".into(),
            input: serde_json::json!({ "path": "src/a.rs" }),
        },
        sub_agent_id: None,
        task_id: None,
    });
    model.apply(&AgentEvent::ToolResult {
        call_id: "c1".into(),
        output: ToolOutput::error("no such file"),
        duration_ms: 3,
        speculated: false,
        sub_agent_id: None,
        task_id: None,
    });
    turn_boundary(&mut model, "src/a.rs", Some("@@ someone else @@\n"), 1, 0);
    assert!(inline_ref(&model, "c1").is_none());
}

/// #4155's second named cause: counts and diff text arrive independently, and
/// a change measured without an attachable patch used to be dropped entirely —
/// so the row lost its `+N −M` as well as its diff.
#[test]
fn a_measured_change_with_no_patch_still_reports_its_delta() {
    let mut model = SessionModel::new();
    edit_call(&mut model, "c1", "src/a.rs");
    turn_boundary(&mut model, "src/a.rs", None, 3, 1);

    let dref = inline_ref(&model, "c1").expect("the row keeps its ref");
    let file = model.files.iter().find(|f| f.path == "src/a.rs").unwrap();
    assert_eq!(
        file.delta_at(dref.seq),
        Some((3, 1)),
        "the measurement survives the missing patch"
    );
    assert_eq!(
        file.diff_at(dref.seq),
        None,
        "and no patch is invented for it"
    );
}
