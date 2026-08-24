// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The one thing #4154 asked of the *fold* rather than of the renderer: a head
//! row that has already settled must pick up its measurement when that
//! measurement arrives.
//!
//! [`super::SessionFold`] caches a settled prefix and never re-folds an entry
//! it has already rendered, so a head that settled before the turn boundary
//! measured the tree would keep the silence it settled with — the exact
//! draw-once shape #4154 was filed against, moved one layer up. What saves it
//! is the `file_gen` term in the cache key (the sum of every path's `changes`),
//! which moves when a `FileChange` lands and drops the whole prefix.
//!
//! That is an argument, and this file is the evidence for it. It lives beside
//! `views/session.rs` rather than inside it because that file stays under the
//! 1500-line ratchet by growing sideways; being a *child* module is what
//! lets it reach `SessionFold::refresh`, which is private to the parent.

use std::collections::HashSet;

use stella_protocol::{AgentEvent, FileChangeKind, ToolCall, ToolOutput};

use super::{FoldPlan, SessionFold};
use crate::model::SessionModel;

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

/// The witness that the backfill survives the cache.
///
/// The head settles the moment its `ToolResult` lands, which is one frame; the
/// measurement arrives at the turn boundary, which is a later one. Between them
/// the row is honestly silent — asserted here, not assumed, because a fold that
/// showed `+1 -0` in the first frame would be reading something other than the
/// emitter. After the boundary the *same* fold must state the change.
///
/// Reusing one `SessionFold` across both frames is the whole point: a fresh one
/// would rebuild from zero and prove nothing about the cache that would
/// otherwise hold the stale row.
#[test]
fn a_settled_head_states_its_size_once_the_turn_boundary_measures_it() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c1".into(),
            name: "edit_file".into(),
            input: serde_json::json!({ "path": "src/x.rs" }),
        },
        sub_agent_id: None,
    });
    model.apply(&AgentEvent::ToolResult {
        call_id: "c1".into(),
        output: ToolOutput::Ok {
            content: "replaced 1 occurrence(s) in src/x.rs".into(),
            data: None,
        },
        duration_ms: 3,
        speculated: false,
        sub_agent_id: None,
    });

    let mut fold = SessionFold::default();
    refresh(&mut fold, &model);
    let before = text(&fold);
    assert!(
        before.contains("edit src/x.rs"),
        "the head must be on screen the moment the call dispatches: {before}"
    );
    assert!(
        !before.contains("+1") && !before.contains("+0"),
        "nothing has measured this change yet, so the row must state no size: {before}"
    );

    // The real producer: one aggregate `--numstat` change per path, at the turn
    // boundary, after every result of the turn has folded (#4155/#4176).
    model.apply(&AgentEvent::FileChange {
        path: "src/x.rs".into(),
        kind: FileChangeKind::Modified,
        added: 1,
        removed: 0,
        diff: Some("@@ -1,1 +1,1 @@\n+measured".into()),
    });
    refresh(&mut fold, &model);
    let after = text(&fold);
    assert!(
        after.contains("edit src/x.rs +1 -0"),
        "the settled head must pick up the measurement the boundary produced: {after}"
    );
}
