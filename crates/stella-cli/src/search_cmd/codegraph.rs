//! Opening and reading the workspace code graph
//! (`.stella/private/codegraph.db`) for the CLI's query surfaces.
//!
//! `stella search` ([`super::engine`]) ranks over the graph, `stella storage`
//! reads its storage map, and `stella stats graph` classifies the answers
//! recorded in past telemetry. This module owns what those doors share: the
//! index location, the open-with-catch-up discipline, the non-fatal
//! index-warning plumbing, and the answer-classification vocabulary the
//! health metric counts with.

use std::path::{Path, PathBuf};

use stella_protocol::tool::ToolOutput;

// ---------------------------------------------------------------------------
// Answer classification (#896)
//
// The health metric (`stella stats graph`) reports what share of recorded
// graph queries actually resolved. That number can only exist if a miss is
// machine-recognizable, and the only thing a telemetry reader has is the
// rendered answer text — so the exact phrases every miss branch emitted live
// here as constants and [`classify_answer`] is the single reader.
// ---------------------------------------------------------------------------

/// Opens every "the graph holds nothing for this" answer.
pub(crate) const UNRESOLVED_MARKER: &str = "the code graph has no";
/// Marks a target that lies outside the one indexed root.
pub(crate) const OUT_OF_ROOT_MARKER: &str = "outside the indexed root";
/// Marks a target the indexer structurally does not cover.
pub(crate) const NOT_INDEXED_MARKER: &str = "is not a file type the code graph indexes";
/// Marks a path target that matches several indexed files.
pub(crate) const AMBIGUOUS_MARKER: &str = "matches several indexed files";

/// What a rendered graph answer actually was — the vocabulary of the
/// resolved-query-rate health metric (`stella stats graph`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphAnswer {
    /// Frames came back (possibly under a labelled fallback spelling).
    Resolved,
    /// The target is not under the indexed root — re-rooting is the fix, and
    /// no amount of re-indexing helps.
    OutOfRoot,
    /// The target exists but the indexer does not cover that file type.
    NotIndexed,
    /// The graph should plausibly have answered and did not — including an
    /// ambiguous path and the importer-resolution capability gap.
    Unresolved,
}

/// Classify one rendered graph answer.
///
/// Tolerant of the `(warning: …)` prefix a failed index pass adds above an
/// answer, because a stale-index warning qualifies an answer rather than
/// replacing it.
#[must_use]
pub(crate) fn classify_answer(content: &str) -> GraphAnswer {
    if content.contains(OUT_OF_ROOT_MARKER) {
        return GraphAnswer::OutOfRoot;
    }
    if content.contains(NOT_INDEXED_MARKER) {
        return GraphAnswer::NotIndexed;
    }
    if content.contains(AMBIGUOUS_MARKER) || content.contains(UNRESOLVED_MARKER) {
        return GraphAnswer::Unresolved;
    }
    GraphAnswer::Resolved
}

/// The index location `stella init` writes and every graph reader resolves.
pub(crate) fn graph_db_path(root: &Path) -> PathBuf {
    root.join(".stella").join("private").join("codegraph.db")
}

/// Fallible storage-map assembly for every governance caller. The graph
/// crate's lower loader remains format-focused and best-effort; this boundary
/// performs private-state migration and rejects unsafe legacy layouts before
/// delegating.
pub(crate) fn load_storage_snapshot(root: &Path) -> Result<stella_graph::StorageSnapshot, String> {
    stella_store::existing_workspace_private_sqlite_path(root, "codegraph.db")
        .map_err(|error| format!("cannot resolve private code graph state: {error}"))?;
    Ok(stella_graph::load_storage_snapshot(root))
}

/// The single phrasing of the non-fatal index-pass diagnostic, shared by
/// every surface that reports it so the operator always reads the same words
/// (and a test can assert on it without pinning the store's error text).
pub(crate) const INDEX_PASS_WARNING: &str = "warning: the code graph index pass \
     failed — answering from what the index already holds, which may be stale";

/// An open graph handle **plus** whatever non-fatal diagnostic its opening
/// catch-up pass produced.
///
/// A library must not own the process's stderr: Stella's primary surface is a
/// TUI, where a stray print to stderr paints raw text over the rendered frame
/// (issue #643). So the warning travels back to the caller as data instead,
/// and every caller has somewhere to put it. It is never dropped on the
/// floor.
pub(crate) struct OpenedGraph {
    pub(crate) graph: stella_graph::CodeGraph,
    /// `Some(message)` when the `index_all` catch-up pass failed and the
    /// answer therefore comes from whatever the index already held.
    pub(crate) index_warning: Option<String>,
}

/// Attach a non-fatal index warning to a rendered answer.
///
/// The warning goes **above** a successful answer — the reader meets the
/// caveat before the possibly-stale frames it qualifies — and below a
/// failure, where the named error is the headline and the failed index pass
/// is context for it.
pub(crate) fn with_index_warning(output: ToolOutput, warning: Option<String>) -> ToolOutput {
    let Some(warning) = warning else {
        return output;
    };
    match output {
        ToolOutput::Ok { content } => ToolOutput::Ok {
            content: format!("({warning})\n{content}"),
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
pub(crate) fn open_or_build(root: &Path) -> Result<OpenedGraph, String> {
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

    /// The warning prefix qualifies an answer; it must not reclassify it.
    #[test]
    fn an_index_warning_prefix_does_not_change_the_classification() {
        let warned = format!("({INDEX_PASS_WARNING}: boom)\n- lib.rs::greet\n  pub fn greet()");
        assert_eq!(classify_answer(&warned), GraphAnswer::Resolved);
        let warned_miss =
            format!("({INDEX_PASS_WARNING}: boom)\n{UNRESOLVED_MARKER} definitions for `x`");
        assert_eq!(classify_answer(&warned_miss), GraphAnswer::Unresolved);
    }

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
            ToolOutput::Ok { content } => panic!("an error must stay an error: {content}"),
        }
        // No warning, no noise: the common path is byte-identical.
        let clean = with_index_warning(
            ToolOutput::Ok {
                content: "frames".into(),
            },
            None,
        );
        assert!(matches!(clean, ToolOutput::Ok { content } if content == "frames"));
    }
}
