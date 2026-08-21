// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Two levels of evidence for [`super`]'s attribution rule.
//!
//! [`ClaimWindow`] first, on its own — the rule is plain logic over owned
//! data, so it can be stated without a transcript or a file ledger. Then the
//! same rule through [`SessionModel::apply`], which is where it has to hold:
//! the window is only correct if the fold opens, feeds and closes it on the
//! right events.

use super::*;
use crate::model::{FileState, SessionModel, TranscriptEntry};
use stella_protocol::{AgentEvent, FileChangeKind, ToolCall, ToolOutput};

/// The ordinary shape: one call, one measurement, one row.
#[test]
fn a_call_claims_the_change_that_folded_under_it() {
    let mut w = ClaimWindow::default();
    w.call_started();
    w.record("src/a.rs", 1);
    w.call_settled();

    assert_eq!(
        w.claim("src/a.rs"),
        Some(InlineDiffRef {
            path: "src/a.rs".into(),
            seq: 1
        })
    );
}

/// Claiming consumes: the second asker gets nothing, so one change is
/// shown by one row without any pass over the transcript.
#[test]
fn a_claimed_change_cannot_be_claimed_again() {
    let mut w = ClaimWindow::default();
    w.call_started();
    w.record("src/a.rs", 1);

    assert!(w.claim("src/a.rs").is_some());
    assert!(w.claim("src/a.rs").is_none());
}

/// A mutation measured with nothing in flight is the boundary sweep's, and
/// no row may show it.
#[test]
fn a_change_measured_with_nothing_in_flight_is_not_recorded() {
    let mut w = ClaimWindow::default();
    w.record("src/a.rs", 1);

    assert!(w.claim("src/a.rs").is_none());
}

/// Two calls to one path each take their own change, oldest first — the
/// order they finished in, which is the order their results fold in.
#[test]
fn two_changes_to_one_path_are_claimed_oldest_first() {
    let mut w = ClaimWindow::default();
    w.call_started();
    w.call_started();
    w.record("src/a.rs", 1);
    w.record("src/a.rs", 2);

    assert_eq!(w.claim("src/a.rs").map(|d| d.seq), Some(1));
    assert_eq!(w.claim("src/a.rs").map(|d| d.seq), Some(2));
}

/// A path nothing measured claims nothing, even when a sibling call in the
/// same window did measure something.
#[test]
fn a_path_with_no_change_in_the_window_claims_nothing() {
    let mut w = ClaimWindow::default();
    w.call_started();
    w.record("src/a.rs", 1);

    assert!(w.claim("src/b.rs").is_none());
}

/// Opening a new dispatch group discards what the last one left behind.
#[test]
fn a_new_dispatch_group_closes_the_previous_window() {
    let mut w = ClaimWindow::default();
    w.call_started();
    w.record("src/a.rs", 1);
    w.call_settled();

    w.call_started();
    assert!(
        w.claim("src/a.rs").is_none(),
        "a change from the previous group is not this call's to claim"
    );
}

/// ...but a later start *within* one group does not, which is what makes a
/// group wider than the engine's concurrency limit safe: its later
/// `ToolStart`s are sent after earlier siblings have already run.
#[test]
fn a_sibling_starting_mid_group_does_not_close_the_window() {
    let mut w = ClaimWindow::default();
    w.call_started();
    w.record("src/a.rs", 1);
    w.call_started();

    assert_eq!(w.claim("src/a.rs").map(|d| d.seq), Some(1));
}

/// A settle with no matching start must not wrap the counter — the halted
/// arm sends exactly that (#2661), and a wrapped counter would make every
/// later boundary sweep look like a call's own work.
#[test]
fn a_settle_with_no_start_does_not_underflow() {
    let mut w = ClaimWindow::default();
    w.call_settled();
    w.record("src/a.rs", 1);

    assert!(
        w.claim("src/a.rs").is_none(),
        "nothing was in flight, so nothing was recorded"
    );
}
// ---- The change a mutating row renders (#4155, #4175, #4203) ----
//
// Two producers of `FileChange` exist and the difference between them is the
// whole subject of these tests:
//
// - **Per-call** (`stella_tools::call_measure`, #4175). The registry measures
//   the work tree the moment a solo mutating call returns and publishes on the
//   channel the engine then sends that call's `ToolResult` on, so the change
//   folds *between* the call's `ToolStart` and its result. This is the one a
//   row can attribute.
// - **Turn boundary** (`stella_cli::turn_files::emit_shared_tree_changes`).
//   One aggregate per path once the turn is over, reporting whatever the
//   per-call readings did not claim — a `delegate`'s writes, a human editing
//   in another window. It folds when no call is in flight, and belongs to no
//   row.
//
// The fold tells them apart by *when* they arrive, not by what they say, which
// is why the helpers below are written as event sequences rather than as one
// convenience that hides the ordering.

/// Open a tool call — the `ToolStart` that names the path a later result
/// correlates back to.
fn tool_start(model: &mut SessionModel, call_id: &str, name: &str, path: &str) {
    model.apply(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: call_id.into(),
            name: name.into(),
            input: serde_json::json!({ "path": path }),
        },
    });
}

/// One measured mutation. The only event that carries one (L-T5).
fn change(model: &mut SessionModel, path: &str, diff: Option<&str>, added: u32, removed: u32) {
    model.apply(&AgentEvent::FileChange {
        path: path.into(),
        kind: FileChangeKind::Modified,
        added,
        removed,
        diff: diff.map(Into::into),
    });
}

/// Settle a call, successfully or not.
fn settle(model: &mut SessionModel, call_id: &str, output: ToolOutput) {
    model.apply(&AgentEvent::ToolResult {
        call_id: call_id.into(),
        output,
        duration_ms: 7,
        speculated: false,
    });
}

/// One successful `edit_file`, measured the way the per-call producer measures
/// it: start, the change that call made, then the result.
fn edit_call(
    model: &mut SessionModel,
    call_id: &str,
    path: &str,
    diff: Option<&str>,
    added: u32,
    removed: u32,
) {
    tool_start(model, call_id, "edit_file", path);
    change(model, path, diff, added, removed);
    settle(model, call_id, ToolOutput::ok("replaced 1 occurrence(s)"));
}

/// The inline-diff reference that call's row claimed, if any. Panics if the
/// call folded no row at all — that would be a different defect.
fn inline_ref<'a>(model: &'a SessionModel, call_id: &str) -> Option<&'a InlineDiffRef> {
    model
        .transcript
        .iter()
        .find_map(|e| match e {
            TranscriptEntry::ToolResult {
                call_id: cid, diff, ..
            } if cid == call_id => Some(diff.as_ref()),
            _ => None,
        })
        .expect("the call folded a result row")
}

fn tracked<'a>(model: &'a SessionModel, path: &str) -> &'a FileState {
    model
        .files
        .iter()
        .find(|f| f.path == path)
        .expect("the path is tracked")
}

/// The live symptom of #4155: a successful `edit_file` rendered with no diff
/// and no `+N −M` at all.
#[test]
fn an_edit_result_resolves_the_change_its_own_call_made() {
    let mut model = SessionModel::new();
    edit_call(
        &mut model,
        "c1",
        "src/a.rs",
        Some("@@ -1 +1,2 @@\n+first\n"),
        2,
        1,
    );

    let dref = inline_ref(&model, "c1").expect("a successful edit claims its change");
    let file = tracked(&model, "src/a.rs");
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

/// **Witness (#4175, #4203).** Two `edit_file` calls to one path in one turn
/// each render *their own* change.
///
/// This is the ceiling the per-call producer was filed to lift, and it is the
/// assertion that fails on the design this replaced. With the turn boundary as
/// the only producer there is one aggregate change to hand out among however
/// many calls edited the file, so the best that design could do was let the
/// *last* row claim it — rendering both edits under the second call and
/// nothing at all under the first.
#[test]
fn two_calls_to_one_path_each_resolve_the_change_they_made() {
    let mut model = SessionModel::new();
    edit_call(&mut model, "c1", "src/lib.rs", Some("@@\n+first"), 1, 0);
    edit_call(&mut model, "c2", "src/lib.rs", Some("@@\n+second"), 4, 0);

    let first = inline_ref(&model, "c1").expect("the first row claims a change");
    let second = inline_ref(&model, "c2").expect("the second row claims a change");
    assert_ne!(
        first.seq, second.seq,
        "two calls to one path must point at two different changes, or one row \
         is rendering the other's work"
    );

    let file = tracked(&model, "src/lib.rs");
    assert_eq!(
        file.diff_at(first.seq),
        Some("@@\n+first"),
        "the first row resolves the first edit — under a turn-boundary-only \
         producer this row had no diff at all"
    );
    assert_eq!(
        file.diff_at(second.seq),
        Some("@@\n+second"),
        "and the second row resolves the second edit, not both of them"
    );
    assert_eq!(
        (file.delta_at(first.seq), file.delta_at(second.seq)),
        (Some((1, 0)), Some((4, 0))),
        "each row's `+N −M` is its own call's measurement, not the turn's total"
    );
    assert_eq!(
        (file.added, file.removed),
        (5, 0),
        "and the path's cumulative total is still the sum of the calls — the \
         per-call readings partition the turn rather than double-counting it"
    );
}

/// The misattribution `render::resolve_inline_diff` exists to prevent, in its
/// original across-turns form: an edit must never render the previous turn's
/// change to the same path.
#[test]
fn a_later_turns_edit_never_renders_an_earlier_turns_diff() {
    let mut model = SessionModel::new();
    edit_call(
        &mut model,
        "c1",
        "src/a.rs",
        Some("@@ first turn @@\n"),
        1,
        0,
    );
    edit_call(
        &mut model,
        "c2",
        "src/a.rs",
        Some("@@ second turn @@\n"),
        3,
        2,
    );

    let file = tracked(&model, "src/a.rs");
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

/// Claiming is per path: two files edited in one turn keep one row each, and
/// neither call can take the other's change out of the window.
#[test]
fn calls_to_different_paths_in_one_turn_each_keep_their_own_ref() {
    let mut model = SessionModel::new();
    edit_call(&mut model, "c1", "src/a.rs", Some("@@ a @@\n"), 1, 0);
    edit_call(&mut model, "c2", "src/b.rs", Some("@@ b @@\n"), 2, 0);

    for (call, path, text) in [
        ("c1", "src/a.rs", "@@ a @@\n"),
        ("c2", "src/b.rs", "@@ b @@\n"),
    ] {
        let dref = inline_ref(&model, call).expect("each path's row keeps its ref");
        assert_eq!(tracked(&model, path).diff_at(dref.seq), Some(text));
    }
}

/// #4155's second named cause: counts and diff text arrive independently, and
/// a change measured without an attachable patch used to be dropped entirely —
/// so the row lost its `+N −M` as well as its diff.
#[test]
fn a_measured_change_with_no_patch_still_reports_its_delta() {
    let mut model = SessionModel::new();
    edit_call(&mut model, "c1", "src/a.rs", None, 3, 1);

    let dref = inline_ref(&model, "c1").expect("the row claims the change");
    let file = tracked(&model, "src/a.rs");
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

/// A failed mutation claims nothing: the change it would point at is one it
/// never made. The registry does not measure a failed call, so there is
/// nothing in the window for it to take.
#[test]
fn a_failed_mutation_keeps_no_inline_diff_ref() {
    let mut model = SessionModel::new();
    tool_start(&mut model, "c1", "edit_file", "src/a.rs");
    settle(&mut model, "c1", ToolOutput::error("no such file"));
    change(&mut model, "src/a.rs", Some("@@ someone else @@\n"), 1, 0);

    assert!(inline_ref(&model, "c1").is_none());
}

/// The same, with a resolvable change actually sitting on the path — which is
/// what makes it reachable. A failure following a successful edit must not
/// inherit that edit's diff and render it under its own ✗.
#[test]
fn a_failed_call_after_a_successful_one_claims_no_diff() {
    let mut model = SessionModel::new();
    edit_call(&mut model, "c1", "src/lib.rs", Some("@@\n+first"), 1, 0);

    tool_start(&mut model, "c2", "edit_file", "src/lib.rs");
    // No `FileChange`: the registry measures only a successful call.
    settle(&mut model, "c2", ToolOutput::error("no such occurrence"));

    assert!(
        inline_ref(&model, "c1").is_some(),
        "the successful row still resolves its own change"
    );
    assert!(
        inline_ref(&model, "c2").is_none(),
        "the failed row claims nothing"
    );
}

/// **Witness (#4203).** A call that *succeeded* but changed nothing measurable
/// claims nothing either.
///
/// This is the case that rules out reading the counter directly. It is
/// reachable two ways — a `write_file` of bytes identical to what is already
/// on disk succeeds and moves nothing, and `snapshot_worktree` is best-effort
/// and infallible by signature, so an unreadable tree simply measures nothing
/// — and in both the path's `changes` still names the *previous* call's
/// change. A row that stamped it would render that earlier edit as its own
/// work, which is #4155's misattribution pointing the other way.
///
/// It rules out the other arithmetic too: `changes + 1` stamps a reference
/// that resolves to nothing, so the row carries a handle on a change that does
/// not exist rather than no handle at all.
#[test]
fn a_successful_call_that_changed_nothing_claims_no_diff() {
    let mut model = SessionModel::new();
    edit_call(&mut model, "c1", "src/a.rs", Some("@@\n+first"), 1, 0);

    tool_start(&mut model, "c2", "write_file", "src/a.rs");
    // The call succeeds and the tree is unmoved, so nothing is measured.
    settle(&mut model, "c2", ToolOutput::ok("wrote 42 bytes"));

    assert!(
        inline_ref(&model, "c2").is_none(),
        "no change folded under this call, so its row claims none — reading \
         `changes` here would hand it the previous call's edit"
    );
    let first = inline_ref(&model, "c1").expect("the first row is untouched");
    assert_eq!(
        tracked(&model, "src/a.rs").diff_at(first.seq),
        Some("@@\n+first"),
        "and the change it did claim is still its own"
    );
}

/// **Witness (#4203).** A change measured while no call was in flight — the
/// turn-boundary sweep, a `delegate`'s writes, a human editing in another
/// window — reaches the file ledger and no transcript row.
///
/// Crediting it to the last row that happened to touch the path is a
/// misattribution with a plausible face: the row names a tool call, and that
/// call did not make this change. The Files tab is where a change nobody can
/// attribute belongs.
#[test]
fn a_change_measured_between_calls_is_claimed_by_no_row() {
    let mut model = SessionModel::new();
    tool_start(&mut model, "c1", "edit_file", "src/a.rs");
    settle(&mut model, "c1", ToolOutput::ok("replaced 1 occurrence(s)"));
    // The turn is over; the sweep reports what no call claimed.
    change(&mut model, "src/a.rs", Some("@@ the sweep @@\n"), 9, 9);

    assert!(
        inline_ref(&model, "c1").is_none(),
        "the row does not adopt a change measured after it settled"
    );
    let file = tracked(&model, "src/a.rs");
    assert_eq!(
        (file.added, file.removed),
        (9, 9),
        "but the ledger still counts it — unattributable is not unrecorded"
    );
    assert_eq!(file.best_diff(), Some(("@@ the sweep @@\n", true)));
}

/// A later call must not reach back for a change the sweep left in the
/// window: opening a dispatch group closes the previous claim window.
#[test]
fn an_unclaimed_sweep_change_is_not_claimed_by_the_next_turns_call() {
    let mut model = SessionModel::new();
    change(&mut model, "src/a.rs", Some("@@ the sweep @@\n"), 9, 9);

    tool_start(&mut model, "c1", "edit_file", "src/a.rs");
    settle(&mut model, "c1", ToolOutput::ok("replaced 1 occurrence(s)"));

    assert!(
        inline_ref(&model, "c1").is_none(),
        "a change that folded before this call started is not this call's"
    );
}

/// The halted arm of `driver::dispatch::execute_tool_calls` (#2661) answers a
/// call it never started with a `ToolResult` and no `ToolStart`. The window
/// counter must survive that, or every later change in the session becomes
/// unclaimable.
#[test]
fn a_result_with_no_start_leaves_the_claim_window_usable() {
    let mut model = SessionModel::new();
    settle(&mut model, "never-started", ToolOutput::error("halted"));
    edit_call(&mut model, "c1", "src/a.rs", Some("@@\n+first"), 1, 0);

    let dref = inline_ref(&model, "c1").expect("the next real call still claims its change");
    assert_eq!(
        tracked(&model, "src/a.rs").diff_at(dref.seq),
        Some("@@\n+first")
    );
}

/// `/clear` is a conversation reset, and an open claim window is conversation
/// state: a change measured before the reset cannot be claimed after it. The
/// file ledger is what survives, and it does.
#[test]
fn a_conversation_reset_drops_the_open_claim_window() {
    let mut model = SessionModel::new();
    tool_start(&mut model, "c1", "edit_file", "src/a.rs");
    change(&mut model, "src/a.rs", Some("@@\n+first"), 1, 0);

    model.reset_conversation();

    tool_start(&mut model, "c2", "edit_file", "src/a.rs");
    settle(&mut model, "c2", ToolOutput::ok("replaced 1 occurrence(s)"));
    assert!(
        inline_ref(&model, "c2").is_none(),
        "the pre-clear change is not this call's to claim"
    );
    assert_eq!(
        tracked(&model, "src/a.rs").best_diff(),
        Some(("@@\n+first", true)),
        "the ledger keeps it — a clear does not un-write the bytes on disk"
    );
}
