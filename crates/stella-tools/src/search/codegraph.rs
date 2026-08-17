//! Opening and reading the workspace code graph
//! (`.stella/private/codegraph.db`) for the CLI's query surfaces.
//!
//! `stella search` ([`super::engine`]) ranks over the graph and `stella
//! storage` reads its storage map. This module owns what those doors share:
//! the index location, the open-with-catch-up discipline, and the non-fatal
//! index-warning plumbing.

use std::path::{Path, PathBuf};

use stella_protocol::tool::ToolOutput;

/// The index location `stella init` writes and every graph reader resolves.
pub fn graph_db_path(root: &Path) -> PathBuf {
    root.join(".stella").join("private").join("codegraph.db")
}

/// Fallible storage-map assembly for every governance caller. The graph
/// crate's lower loader remains format-focused and best-effort; this boundary
/// performs private-state migration and rejects unsafe legacy layouts before
/// delegating.
pub fn load_storage_snapshot(root: &Path) -> Result<stella_graph::StorageSnapshot, String> {
    stella_store::existing_workspace_private_sqlite_path(root, "codegraph.db")
        .map_err(|error| format!("cannot resolve private code graph state: {error}"))?;
    Ok(stella_graph::load_storage_snapshot(root))
}

/// The single phrasing of the non-fatal index-pass diagnostic, shared by
/// every surface that reports it so the operator always reads the same words
/// (and a test can assert on it without pinning the store's error text).
pub const INDEX_PASS_WARNING: &str = "warning: the code graph index pass \
     failed — answering from what the index already holds, which may be stale";

/// An open graph handle **plus** whatever non-fatal diagnostic its opening
/// catch-up pass produced.
///
/// A library must not own the process's stderr: Stella's primary surface is a
/// TUI, where a stray print to stderr paints raw text over the rendered frame
/// (issue #643). So the warning travels back to the caller as data instead,
/// and every caller has somewhere to put it. It is never dropped on the
/// floor.
pub struct OpenedGraph {
    pub graph: stella_graph::CodeGraph,
    /// `Some(message)` when the `index_all` catch-up pass failed and the
    /// answer therefore comes from whatever the index already held.
    pub index_warning: Option<String>,
}

/// Attach a non-fatal index warning to a rendered answer.
///
/// The warning goes **above** a successful answer — the reader meets the
/// caveat before the possibly-stale frames it qualifies — and below a
/// failure, where the named error is the headline and the failed index pass
/// is context for it.
pub fn with_index_warning(output: ToolOutput, warning: Option<String>) -> ToolOutput {
    let Some(warning) = warning else {
        return output;
    };
    match output {
        ToolOutput::Ok { content, .. } => ToolOutput::Ok {
            content: format!("({warning})\n{content}"),
            data: None,
        },
        ToolOutput::Error { message, .. } => ToolOutput::error(format!("{message}\n({warning})")),
    }
}

/// Open the code graph for a read, **building it on first use** when no
/// index exists yet.
///
/// The index is a session-start background build (`spawn_session_graph`), so
/// a search can race ahead of it. Bootstrapping here means the first query
/// that needs an index builds one instead of erroring. `index_all` is the
/// same pass `stella init` runs — a full build on a fresh db, a hash-diff
/// catch-up on an existing one — so it doubles as the freshness pass that
/// lets the graph see files written moments ago.
///
/// The `stale answers are worse than none` rule still holds: the pass runs
/// on every open, only a hard failure to prepare the store surfaces as an
/// error to the caller, and a failed pass is reported as
/// [`OpenedGraph::index_warning`] rather than silently tolerated.
///
/// **Synchronous — an async caller must wrap it in `spawn_blocking`**, the
/// same contract `stella_graph::CodeGraph::index_all` states.
pub fn open_or_build(root: &Path) -> Result<OpenedGraph, String> {
    // The WRITABLE path (creates `.stella/private/`), not the read-only
    // `existing_...` probe — this is the one place a query is allowed to
    // create the index it needs.
    let db_path = stella_store::workspace_private_sqlite_path(root, "codegraph.db")
        .map_err(|error| format!("cannot prepare the code graph store: {error}"))?;
    let graph = stella_graph::CodeGraph::open(root, &db_path)
        .map_err(|error| format!("could not open the code graph: {error}"))?;
    // A build/refresh failure is not fatal to a query: an existing index
    // still answers from its last good state, and a brand-new one answers
    // empty rather than aborting the search. It is still reported —
    // returned to the caller, never printed over the frame.
    let index_warning = graph
        .index_all()
        .err()
        .map(|error| format!("{INDEX_PASS_WARNING}: {error}"));
    Ok(OpenedGraph {
        graph,
        index_warning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A failing answer that ALSO had a failing index pass must report both —
    /// the named error leads, the stale-index caveat follows it.
    #[test]
    fn a_failed_answer_keeps_both_its_error_and_the_index_warning() {
        let warned = with_index_warning(
            ToolOutput::error("code-graph query failed: boom"),
            Some(format!("{INDEX_PASS_WARNING}: disk on fire")),
        );
        match warned {
            ToolOutput::Error { message, .. } => {
                assert!(
                    message.starts_with("code-graph query failed: boom"),
                    "{message}"
                );
                assert!(message.contains(INDEX_PASS_WARNING), "{message}");
            }
            ToolOutput::Ok { content, .. } => panic!("an error must stay an error: {content}"),
        }
        // No warning, no noise: the common path is byte-identical.
        let clean = with_index_warning(
            ToolOutput::Ok {
                content: "frames".into(),
                data: None,
            },
            None,
        );
        assert!(matches!(clean, ToolOutput::Ok { content , .. } if content == "frames"));
    }
}
