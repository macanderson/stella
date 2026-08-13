// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella search <query>` — the human door to the agent's `search` tool.
//!
//! One engine, two surfaces (the `stella graph` / `graph_query` precedent):
//! this prints exactly what the model would receive for the same question, so
//! an operator can see — and tune, via `STELLA_SEARCH_DEPTH` /
//! `STELLA_SEARCH_BUDGET` — the answer the agent gets. Keyless: the graph
//! opens (or builds) `.stella/private/codegraph.db` locally, and the only
//! network is an explicitly configured embedder (`STELLA_EMBED_URL` /
//! `STELLA_EMBED_MODEL`); without one the name and scan strategies answer.

use stella_protocol::tool::ToolOutput;
use stella_tools::search::{SearchConfig, run_search};

/// Run one search against the current directory and print the answer.
pub(crate) async fn run(query: &str) -> Result<(), String> {
    let root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    match run_search(&root, query, SearchConfig::from_env()).await {
        ToolOutput::Ok { content } => {
            println!("{content}");
            Ok(())
        }
        ToolOutput::Error { message } => Err(message),
    }
}
