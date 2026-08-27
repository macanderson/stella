//! The code-graph facts a file mutation states on its own transcript row
//! (`design/tui-v2/SPEC.md` §6.3): what a write registered, and how many
//! files still imported what a deletion removed.
//!
//! The deletion's count is measured **before** the unlink, because a count
//! taken afterwards answers a different question and arrives too late to
//! warn anybody. That ordering is why [`WorkspaceGraph`] is a trait: a test
//! double answers out of whatever the filesystem looks like at the moment it
//! is asked, so a check that had run after the unlink would find its own
//! subject already gone.
//!
//! Every fact elides when no index answered. A workspace nobody has run
//! `stella init` in has no code graph, and a deletion that claims a clean
//! graph check it never ran is worse than a deletion that says nothing.
//!
//! Facts ride [`ToolOutput::Ok`]'s structured `data` under [`DATA_KEY`],
//! keyed by the path the caller asked for, so a batch carries one per file
//! and the row rendering a path picks its own. The success prose is
//! untouched: `delete_file` and `write_file` both have byte-identical
//! success strings the stagnation detector keys on (#3176).

use std::path::Path;

use serde::{Deserialize, Serialize};
use stella_protocol::tool::ToolOutput;

/// The `Ok.data` key these facts ride under.
pub const DATA_KEY: &str = "graph_facts";

/// One code-graph fact about one path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "fact", rename_all = "snake_case")]
pub enum GraphFact {
    /// How many indexed files imported `path`, counted before it was
    /// removed. Absent rather than zero when the index had never seen the
    /// path: "nothing imports this file" and "this file was never in the
    /// graph" are the two answers the line exists to separate.
    InboundRefs {
        /// The path as the caller spelled it, so the row rendering that path
        /// finds its own fact.
        path: String,
        /// Files whose imports resolve to `path`.
        inbound: u32,
    },
    /// `path` is a node in the index, registered by the write that created
    /// it. A file the index takes no node for — plain text no grammar
    /// claims — produces no fact at all.
    Registered {
        /// The path as the caller spelled it.
        path: String,
    },
}

impl GraphFact {
    /// The path this fact is about.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            GraphFact::InboundRefs { path, .. } | GraphFact::Registered { path } => path,
        }
    }
}

/// The two questions a file mutation asks the workspace code graph.
///
/// Both answer `None` for every reason a fact must not be stated — no index
/// in this workspace, an index that would not open, a query that failed, a
/// path the index holds no node for. A caller renders nothing on `None`
/// rather than substituting a plausible zero.
pub trait WorkspaceGraph: Send + Sync {
    /// How many indexed files import `file`, an absolute path inside `root`.
    fn inbound_refs(&self, root: &Path, file: &Path) -> Option<u32>;

    /// Index `file` now and answer whether it is a node afterwards.
    fn register(&self, root: &Path, file: &Path) -> Option<bool>;
}

/// The workspace's own code graph (`.stella/private/codegraph.db`).
///
/// Every method opens the index, asks, and closes: a mutation is rare next
/// to a query, and a handle held for the session's lifetime would keep a
/// write connection open against a store `stella observe` and the session's
/// own watcher are also writing.
pub struct Codegraph;

impl Codegraph {
    /// The index, or `None` when this workspace has none.
    ///
    /// The read-only resolver, never a bare join: `.stella/private/` moves
    /// under `STELLA_WORKSPACE_STATE_ROOT` and a legacy layout is migrated on
    /// the way past (#4394). A mutation must create no index it did not find
    /// — a `write_file` in a directory nobody has indexed leaves the tree as
    /// it found it.
    fn open(root: &Path) -> Option<stella_graph::CodeGraph> {
        let db_path = crate::search::codegraph::graph_db_path(root)
            .ok()
            .flatten()?;
        stella_graph::CodeGraph::open(root, &db_path).ok()
    }
}

impl WorkspaceGraph for Codegraph {
    fn inbound_refs(&self, root: &Path, file: &Path) -> Option<u32> {
        let graph = Codegraph::open(root)?;
        // A path the index holds no node for has no measured count, and zero
        // would read as one. The index is not refreshed first on purpose:
        // this answers what the graph knows about a file that is about to
        // stop existing, and indexing it in order to delete it would be work
        // whose only product is the row it is about to invalidate.
        if !graph.indexes_file(file).ok()? {
            return None;
        }
        u32::try_from(graph.file_neighborhood(file).ok()?.importers.len()).ok()
    }

    fn register(&self, root: &Path, file: &Path) -> Option<bool> {
        let graph = Codegraph::open(root)?;
        // The watcher would index this too, seconds later and debounced.
        // Registering here is what makes the statement true at the moment
        // the row states it.
        graph
            .register_paths(std::slice::from_ref(&file.to_path_buf()))
            .ok()?;
        graph.indexes_file(file).ok()
    }
}

/// Attach `facts` to a successful output, keeping whatever `data` already
/// carried — a deletion publishes its own reading under `changes` as well.
///
/// An empty slice attaches nothing: a call that measured no graph fact must
/// leave the key absent, not present and empty.
#[must_use]
pub fn attach(output: ToolOutput, facts: &[GraphFact]) -> ToolOutput {
    if facts.is_empty() {
        return output;
    }
    let ToolOutput::Ok { content, data } = output else {
        return output;
    };
    let mut data = match data {
        Some(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    // Serializing a plain struct of strings and integers cannot fail; the
    // fallback keeps the signature infallible rather than hiding a panic.
    let value = serde_json::to_value(facts).unwrap_or(serde_json::Value::Array(Vec::new()));
    data.insert(DATA_KEY.to_string(), value);
    ToolOutput::Ok {
        content,
        data: Some(serde_json::Value::Object(data)),
    }
}

/// The facts a successful output carries under [`DATA_KEY`], or none.
///
/// Lenient like [`crate::own_change::from_output`]: a payload in a shape this
/// version does not parse is a call with nothing to report, never an error at
/// the seam that reads it.
#[must_use]
pub fn from_output(output: &ToolOutput) -> Vec<GraphFact> {
    let ToolOutput::Ok {
        data: Some(data), ..
    } = output
    else {
        return Vec::new();
    };
    data.get(DATA_KEY)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
