// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Answering the GRAPH tab's re-root requests.
//!
//! Both recv sites in `super` route here — the idle one inline, the mid-turn
//! one on the blocking pool — so the two cannot answer a re-root differently.
//! They already had to be kept in step by hand for the file picker; adding
//! the `q` box's query as a second verb is what made one home for the
//! answering worth having (#4335).

use contextgraph_host::Host;
use stella_tui::envelope::{Inbound, WorkspaceInput};
use tokio::sync::mpsc::UnboundedSender;

use crate::agent;

/// Answer a re-root request by requerying and pushing a fresh snapshot back,
/// the same out-of-band refresh path `/init` uses.
///
/// Silent on anything that is not a re-root, and silent when nothing can
/// answer — the deck then keeps the neighborhood it has, which is what a
/// reader should still be looking at when nothing was learned.
///
/// The two verbs take different routes and so are offloaded differently. The
/// picker's file re-root is a synchronous SQLite read plus a grammar load, so
/// it goes to the blocking pool. The `q` box's query goes through the CGP
/// host, which is already async and already puts its provider's SQLite work on
/// the blocking pool itself ([`crate::contextgraph`]), so it is awaited here
/// rather than wrapped again.
pub(super) async fn answer(
    input: WorkspaceInput,
    host: &Host,
    workspace_root: &std::path::Path,
    in_tx: &UnboundedSender<Inbound>,
) {
    let snapshot = match input {
        WorkspaceInput::FocusGraphFile { file } => {
            let root = workspace_root.to_path_buf();
            tokio::task::spawn_blocking(move || agent::graph_snapshot_focus(&root, Some(&file)))
                .await
                .ok()
                .flatten()
        }
        WorkspaceInput::GraphQuery { text } => {
            agent::graph_query_snapshot(host, workspace_root, &text).await
        }
        _ => None,
    };
    if let Some(snapshot) = snapshot {
        let _ = in_tx.send(Inbound::GraphSnapshot(snapshot));
    }
}
