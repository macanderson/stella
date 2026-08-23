//! Opening and reading the workspace code graph
//! (`.stella/private/codegraph.db`) for the CLI's query surfaces.
//!
//! `stella search` ([`super::engine`]) ranks over the graph and `stella
//! storage` reads its storage map. This module owns what those doors share:
//! the index location, the open-with-catch-up discipline, and the non-fatal
//! index-warning plumbing.

use std::path::{Path, PathBuf};

use stella_protocol::tool::ToolOutput;

/// Where an existing index lives, for every reader that queries one.
/// `None` when this workspace has no code graph.
///
/// Delegates to the store rather than joining the path itself, because
/// `.stella/private/` is not always literally under the workspace root and a
/// join cannot know that. `STELLA_WORKSPACE_STATE_ROOT`
/// (`stella_home::WORKSPACE_STATE_ROOT_ENV`) redirects it so a throwaway
/// worktree keeps its private state outside the tree that is about to be
/// deleted, and the same resolver migrates a legacy `.stella/codegraph.db`
/// into `.stella/private/` on the way past.
///
/// A bare join here saw neither. Under a redirect the session built its index
/// through the store and mounted its watcher, its CGP host and its deck on an
/// empty database at the literal path, so a `stella self-driving` turn's own
/// edits were indexed nowhere the queries looked (#4394).
///
/// The **read-only** resolver, not the writable one [`open_or_build`] takes:
/// every caller here is answering a query, and a lookup in a workspace with
/// no `.stella/` must create none — a Command Deck entity search in a
/// directory nobody has run `stella init` in leaves the tree exactly as it
/// found it.
pub fn graph_db_path(root: &Path) -> Result<Option<PathBuf>, IndexError> {
    stella_store::existing_workspace_private_sqlite_path(root, "codegraph.db")
        .map_err(|error| IndexError::PrivateState(error.to_string()))
}

/// Why a code-graph door did not open (invariant #5).
///
/// The three variants are the three different things a caller may want to do
/// about it, which is the test for whether a variant earns its place: a
/// workspace whose private state cannot be resolved is a *setup* problem the
/// operator must fix (an unsafe legacy layout, bad permissions) and is worth
/// surfacing loudly; a store that cannot be prepared or a graph that cannot
/// be opened are index problems a query may degrade past — which is exactly
/// what `super::engine::dispatch` does, dropping to a lexical rung rather
/// than failing the search. Before this was typed, telling those apart meant
/// matching on prose.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// The workspace's private state directory could not be resolved or
    /// migrated — a setup problem, not an index problem.
    #[error("cannot resolve private code graph state: {0}")]
    PrivateState(String),
    /// The index's backing store could not be prepared for writing.
    #[error("cannot prepare the code graph store: {0}")]
    Prepare(String),
    /// The index exists but could not be opened.
    #[error("could not open the code graph: {0}")]
    Open(String),
}

/// Fallible storage-map assembly for every governance caller. The graph
/// crate's lower loader remains format-focused and best-effort; this boundary
/// performs private-state migration and rejects unsafe legacy layouts before
/// delegating.
pub fn load_storage_snapshot(root: &Path) -> Result<stella_graph::StorageSnapshot, IndexError> {
    stella_store::existing_workspace_private_sqlite_path(root, "codegraph.db")
        .map_err(|error| IndexError::PrivateState(error.to_string()))?;
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
pub fn open_or_build(root: &Path) -> Result<OpenedGraph, IndexError> {
    // The WRITABLE path (creates `.stella/private/`), not the read-only
    // `existing_...` probe — this is the one place a query is allowed to
    // create the index it needs. `graph_db_path` is the read-only half of the
    // same rule and answers `None` here instead, which is why a writer cannot
    // route through it (#4394). Reported as `Prepare` rather than
    // `PrivateState` because a search degrades past this to a lexical rung
    // (`super::engine::dispatch`) instead of failing, which is the distinction
    // those variants carry.
    let db_path = stella_store::workspace_private_sqlite_path(root, "codegraph.db")
        .map_err(|error| IndexError::Prepare(error.to_string()))?;
    let graph = stella_graph::CodeGraph::open(root, &db_path)
        .map_err(|error| IndexError::Open(error.to_string()))?;
    // A build/refresh failure is not fatal to a query: an existing index
    // still answers from its last good state, and a brand-new one answers
    // empty rather than aborting the search. It is still reported —
    // returned to the caller, never printed over the frame.
    //
    // Single-flight (#3650). This pass fires on EVERY graph-tool open, so in a
    // live session it is routinely racing the mount catch-up and any second
    // `stella` process over the same tree — each walking and hashing every
    // file to produce byte-identical rows, and the loser previously able to
    // exhaust `busy_timeout` and surface here as a warning. Yielding to a walk
    // already in progress costs this query nothing: the other pass is writing
    // the very rows it would have written.
    let index_warning = graph
        .index_all_single_flight()
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
