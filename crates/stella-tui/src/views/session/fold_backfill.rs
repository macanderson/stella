// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The one thing #4154 asked of the *fold* rather than of the renderer: a
//! settled head row must not be served stale when what it renders from moves.
//!
//! [`super::SessionFold`] caches a settled prefix and never re-folds an entry
//! it has already rendered. What saves it is the `file_gen` term in the cache
//! key (the sum of every path's `changes`), which moves when a `FileChange`
//! lands and drops the whole prefix. That is an argument, and this file is the
//! evidence for it. It lives beside `views/session.rs` rather than inside it
//! because that file is a grandfathered god file and closed to growth; being a
//! *child* module is what lets it reach `SessionFold::refresh`, which is
//! private to the parent.
//!
//! # What this used to witness, and why the case is gone
//!
//! It witnessed a head *filling in*: settling silent and picking up its size
//! one frame later, when the turn-boundary sweep measured the tree. That
//! sequence cannot happen any more, in either direction:
//!
//! - A solo mutating call is measured the moment it returns, on the channel
//!   its `ToolResult` then rides (`stella_tools::call_measure`, #4175). The
//!   change folds *before* the row, so the row is right the first time it is
//!   drawn — there is nothing to fill in.
//! - A change the sweep reports afterwards belongs to no call, and no row
//!   claims it (#4203, `SessionModel::unclaimed_changes`). A row that filled
//!   in from one would be stating a `delegate`'s writes, or a human's edit in
//!   another window, as its own work.
//!
//! Both directions are witnessed below. The `file_gen` term outlives the case
//! it was added for, because a settled row's rendering still changes when
//! `files` moves — a diff aged out of
//! [`DIFF_HISTORY`](crate::model::DIFF_HISTORY) stops resolving, and the row
//! must stop showing it.

use std::collections::HashSet;

use stella_protocol::{AgentEvent, FileChangeKind, ToolCall, ToolOutput};

use super::{FoldPlan, SessionFold};
use crate::model::{DIFF_HISTORY, SessionModel};

/// Fold `model` into `fold`, the way a deck frame does.
fn refresh(fold: &mut SessionFold, model: &SessionModel) {
    fold.refresh(
        "lead",
        &model.transcript,
        &model.files,
        "",
        false,
        &HashSet::new(),
        false,
        0,
        120,
        &FoldPlan::default(),
        0,
    );
}

/// Every row the fold holds, as plain text.
fn text(fold: &SessionFold) -> String {
    fold.window_lines(0..fold.total())
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One mutating `FileChange`.
fn change(model: &mut SessionModel, diff: &str, added: u32) {
    model.apply(&AgentEvent::FileChange {
        path: "src/x.rs".into(),
        kind: FileChangeKind::Modified,
        added,
        removed: 0,
        diff: Some(diff.into()),
    });
}

/// Dispatch one `edit_file` against `src/x.rs`.
fn dispatch(model: &mut SessionModel) {
    model.apply(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c1".into(),
            name: "edit_file".into(),
            input: serde_json::json!({ "path": "src/x.rs" }),
        },
    });
}

/// Settle it.
fn settle(model: &mut SessionModel) {
    model.apply(&AgentEvent::ToolResult {
        call_id: "c1".into(),
        output: ToolOutput::Ok {
            content: "replaced 1 occurrence(s) in src/x.rs".into(),
            data: None,
        },
        duration_ms: 3,
        speculated: false,
    });
}

/// The head is right in the frame it settles in: the per-call producer has
/// already measured, so there is no silent frame to recover from.
///
/// The in-flight frame is asserted too, because it is the case #4150 fixed and
/// this must not undo: the head is on screen from dispatch, and until its
/// result folds it has claimed nothing and must state no size. `+0 -0` over a
/// real edit is a claim that it changed nothing.
#[test]
fn a_head_states_its_size_in_the_frame_it_settles() {
    let mut model = SessionModel::new();
    dispatch(&mut model);
    change(&mut model, "@@ -1,1 +1,1 @@\n+measured", 1);

    let mut fold = SessionFold::default();
    refresh(&mut fold, &model);
    let before = text(&fold);
    assert!(
        before.contains("edit src/x.rs"),
        "the head must be on screen the moment the call dispatches: {before}"
    );
    assert!(
        !before.contains("+1") && !before.contains("+0"),
        "the call has not returned, so the row has claimed nothing and must \
         state no size: {before}"
    );

    settle(&mut model);
    refresh(&mut fold, &model);
    let after = text(&fold);
    assert!(
        after.contains("edit src/x.rs +1 -0"),
        "the settled head states the change it claimed: {after}"
    );
}

/// **Witness (#4203).** A change measured after the head settled does not
/// reach it — and the cache is not what withholds it.
///
/// Reusing one [`SessionFold`] across both frames is the whole point. The
/// second `FileChange` moves `file_gen` and so genuinely drops the cached
/// prefix and re-folds the row; the row still states nothing, because it
/// claimed nothing. A fresh fold would prove only that a rebuild agrees, not
/// that the refusal is the fold's own.
#[test]
fn a_settled_head_does_not_adopt_a_change_measured_after_it() {
    let mut model = SessionModel::new();
    dispatch(&mut model);
    // No per-call measurement: this call's own reading found nothing.
    settle(&mut model);

    let mut fold = SessionFold::default();
    refresh(&mut fold, &model);
    let before = text(&fold);
    assert!(before.contains("edit src/x.rs"), "{before}");

    // The turn boundary sweeps up what no call claimed — a `delegate`'s
    // writes, a human editing in another window.
    change(&mut model, "@@ -1,1 +1,1 @@\n+the sweep", 9);
    refresh(&mut fold, &model);
    let after = text(&fold);
    assert!(
        !after.contains("+9"),
        "the head adopted a change its call did not make: {after}"
    );
    assert!(
        after.contains("edit src/x.rs"),
        "the row itself is still there, simply without a size: {after}"
    );
}

/// The `file_gen` term outlives the case it was added for: a settled row whose
/// diff ages out of [`DIFF_HISTORY`] must stop showing it.
///
/// This is the one remaining way a settled entry's rendering changes with no
/// transcript append — the reason `views/session.rs` keys the fold cache on the
/// total mutation count in the first place. Without it the cached row would go
/// on rendering a diff `render::resolve_inline_diff` has already stopped
/// resolving.
#[test]
fn a_settled_head_drops_its_diff_once_the_history_ages_it_out() {
    let mut model = SessionModel::new();
    dispatch(&mut model);
    change(&mut model, "@@ -1,1 +1,1 @@\n+measured", 1);
    settle(&mut model);

    let mut fold = SessionFold::default();
    refresh(&mut fold, &model);
    assert!(
        text(&fold).contains("+1 -0"),
        "the row starts out stating its own change"
    );

    // Push this path's history past its depth, one mutation at a time.
    for i in 0..DIFF_HISTORY {
        change(&mut model, &format!("@@ later {i} @@"), 2);
    }
    refresh(&mut fold, &model);
    let after = text(&fold);
    assert!(
        !after.contains("+1 -0"),
        "the row still states a measurement the path no longer remembers: \
         {after}"
    );
    assert!(
        !after.contains("+measured"),
        "and it still renders the aged-out diff: {after}"
    );
}
