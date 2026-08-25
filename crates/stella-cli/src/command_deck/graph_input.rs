// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Answering the GRAPH tab's re-root requests.
//!
//! Both recv sites in `super` route here — the idle one inline, the mid-turn
//! one on the blocking pool — so the two cannot answer a re-root differently.
//! They already had to be kept in step by hand for the file picker; adding
//! the `q` box's query as a second verb is what made one home for the
//! answering worth having (#4335).

use stella_tui::envelope::{Inbound, WorkspaceInput};
use tokio::sync::mpsc::UnboundedSender;

use crate::agent;

/// Answer a re-root request by requerying the index and pushing a fresh
/// snapshot back, the same out-of-band refresh path `/init` uses.
///
/// Blocking: it opens SQLite and loads grammars. Silent on anything that is
/// not a re-root, and silent when the index cannot answer — the deck then
/// keeps the neighborhood it has, which is the honest thing to leave on
/// screen when nothing was learned.
pub(super) fn answer(
    input: WorkspaceInput,
    workspace_root: &std::path::Path,
    in_tx: &UnboundedSender<Inbound>,
) {
    let snapshot = match input {
        WorkspaceInput::FocusGraphFile { file } => {
            agent::graph_snapshot_focus(workspace_root, Some(&file))
        }
        WorkspaceInput::GraphQuery { text } => agent::graph_query_snapshot(workspace_root, &text),
        _ => None,
    };
    if let Some(snapshot) = snapshot {
        let _ = in_tx.send(Inbound::GraphSnapshot(snapshot));
    }
}
