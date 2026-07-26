//! Node access — the typed vocabulary, the write shape, the read shape, and
//! the statements that put nodes in and take them out.
//!
//! Split out of the store module unchanged (#712 deliverable 1). The
//! ranking-time readers are not here: they live one level up in
//! [`crate::candidates`], because they answer to a different constraint.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use contextgraph_types::FrameKind;

use crate::error::ContextError;

use super::{sha256_hex, to_hex};

/// Typed node vocabulary. Stored as its `as_str` form; retrieval maps it onto
/// a `contextgraph_types::FrameKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// A workspace file; surfaces as a `Snippet` frame.
    File,
    /// A code symbol — function, type, module; surfaces as a `Symbol` frame.
    Symbol,
    /// A named idea or entity. The general-purpose kind, and the one a stored
    /// `node.kind` this binary does not recognize reads back as.
    Concept,
    /// A fact reified as its own retrievable node. Assertions written through
    /// `FactAssertion` become `edge` rows, not nodes; this is for callers that
    /// want the fact itself to be a citable frame.
    Fact,
    /// The mirror node of an `episode` record — what makes a past turn
    /// recallable.
    Episode,
    /// A person (author, reviewer, teammate).
    Person,
    /// A produced artifact — a doc, report, or generated file; surfaces as a
    /// `Doc` frame.
    Artifact,
    /// A unit of work (issue, ticket, TODO).
    Task,
    /// A memory (e.g. a post-turn reflection). Mirrors a `memory` record so it
    /// is retrievable and domain-taggable through the normal node pipeline.
    Memory,
}

impl NodeKind {
    /// The canonical string stored in `node.kind`.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::File => "file",
            NodeKind::Symbol => "symbol",
            NodeKind::Concept => "concept",
            NodeKind::Fact => "fact",
            NodeKind::Episode => "episode",
            NodeKind::Person => "person",
            NodeKind::Artifact => "artifact",
            NodeKind::Task => "task",
            NodeKind::Memory => "memory",
        }
    }

    /// Parse a stored `node.kind` back into the enum.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "file" => NodeKind::File,
            "symbol" => NodeKind::Symbol,
            "concept" => NodeKind::Concept,
            "fact" => NodeKind::Fact,
            "episode" => NodeKind::Episode,
            "person" => NodeKind::Person,
            "artifact" => NodeKind::Artifact,
            "task" => NodeKind::Task,
            "memory" => NodeKind::Memory,
            _ => return None,
        })
    }

    /// Map onto the CGP frame kind a retrieved node surfaces as.
    pub fn to_frame_kind(self) -> FrameKind {
        match self {
            NodeKind::File => FrameKind::Snippet,
            NodeKind::Symbol => FrameKind::Symbol,
            NodeKind::Episode => FrameKind::Episode,
            NodeKind::Artifact => FrameKind::Doc,
            NodeKind::Memory => FrameKind::Memory,
            // Concept/Fact/Person/Task all read as facts to a consuming host.
            NodeKind::Concept | NodeKind::Fact | NodeKind::Person | NodeKind::Task => {
                FrameKind::Fact
            }
        }
    }
}

/// A node to write. `display_name` is mandatory and non-empty — it is the
/// human citation label (`L-C4`), enforced at write time so retrieval can
/// never later fail to cite.
#[derive(Debug, Clone)]
pub struct NodeInput {
    /// The typed vocabulary entry this node belongs to; also decides the
    /// `FrameKind` retrieval surfaces it as.
    pub kind: NodeKind,
    /// The human citation label (`L-C4`). Mandatory and non-empty —
    /// `upsert_node` rejects a blank one. It is also the identity key when no
    /// `uri` is set.
    pub display_name: String,
    /// The retrievable body: what gets embedded, hashed for the byte-compat
    /// skip (`L-C2`), and served as a frame's content. Leave it empty for a
    /// node that exists only as a graph endpoint — empty content is never
    /// embedded.
    pub content: String,
    /// Source uri (`file://…`, `memory://…`, `episode://…`). When present it is
    /// the node's identity key, so two writes with the same uri update one row,
    /// and it is what a query's `anchors` are matched against.
    pub uri: Option<String>,
    /// Free-form JSON stored on the node row for consumers reading the graph
    /// directly. Nothing in the retrieval path reads it back today.
    pub properties: serde_json::Value,
    /// Workspace domain tags (e.g. `["auth", "billing"]`). One or more; stored
    /// via the `node_domains` junction (indexable), never a JSON blob.
    pub domains: Vec<String>,
}

impl NodeInput {
    /// A node with the given kind and label, empty content, no uri, no domains.
    pub fn new(kind: NodeKind, display_name: impl Into<String>) -> Self {
        Self {
            kind,
            display_name: display_name.into(),
            content: String::new(),
            uri: None,
            properties: serde_json::json!({}),
            domains: Vec::new(),
        }
    }

    /// Attach retrievable content.
    // `#[must_use]` on every builder below: they consume and return `self`, so
    // a dropped result is silent data loss — content or domain tags that never
    // reach the store, with no error to notice.
    #[must_use]
    pub fn with_content(mut self, content: impl Into<String>) -> Self {
        self.content = content.into();
        self
    }

    /// Attach a source uri (used for anchor matching and provenance).
    #[must_use]
    pub fn with_uri(mut self, uri: impl Into<String>) -> Self {
        self.uri = Some(uri.into());
        self
    }

    /// Tag with one or more workspace domains.
    #[must_use]
    pub fn with_domains<I, S>(mut self, domains: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.domains = domains.into_iter().map(Into::into).collect();
        self
    }

    /// The identity key: the uri when present, else the display name. Two
    /// writes with the same identity update the same node (content-on-touch).
    fn natural_key(&self) -> &str {
        match &self.uri {
            Some(u) if !u.is_empty() => u.as_str(),
            _ => self.display_name.as_str(),
        }
    }

    fn public_id(&self) -> String {
        node_public_id(self.kind, self.natural_key())
    }
}

/// A node read back from the store.
#[derive(Debug, Clone)]
pub struct NodeRow {
    /// The SQLite rowid. Store-internal: edges, the domain junctions, and the
    /// retrieval pipeline join on it, but it is never surfaced to a host —
    /// `public_id` is the identity that travels.
    pub id: i64,
    /// The stable `nod_…` identity, derived from kind + natural key. It is the
    /// frame id a host holds, reuses, and presents back to `context/verify`.
    pub public_id: String,
    /// The typed vocabulary entry, mapped onto a `FrameKind` at retrieval. An
    /// unrecognized stored kind reads back as [`NodeKind::Concept`].
    pub kind: NodeKind,
    /// The human citation label (`L-C4`) — it becomes the frame's title, and a
    /// blank one is a `MissingCitation` error rather than an unlabeled frame.
    pub display_name: String,
    /// The retrievable body, served as the frame's content and the basis of its
    /// declared `token_cost`.
    pub content: String,
    /// sha256 of `content`. Keys the embedding index together with the
    /// fingerprint (`L-C2`), and is what a frame declares as
    /// `sha256:<content_hash>` so a host can revalidate it without moving the
    /// body.
    pub content_hash: String,
    /// Source uri when the node has one — carried onto the frame and into its
    /// provenance chain.
    pub uri: Option<String>,
    /// Valid time: when the fact became true in the world — may precede
    /// `recorded_at` (observation), never follows it. `None` = unknown,
    /// treated as valid-since-observation.
    ///
    /// **Always `None` today.** Only EDGES are versioned (see `MIGRATION_V1`);
    /// `upsert_node` never writes `node.valid_from`, so this reads back the
    /// column's NULL for every row. It is kept because the column exists and a
    /// point-in-time node reader would populate it — not because anything
    /// currently sets it. Read it as "not yet wired", never as "unknown".
    pub valid_from: Option<String>,
    /// Transaction time the row was **first** written (RFC-3339). `upsert_node`
    /// updates content in place without touching it, so it is a creation time,
    /// not a modification time — which is exactly what recall's recency
    /// ranking sorts on.
    pub recorded_at: String,
}

fn node_public_id(kind: NodeKind, natural_key: &str) -> String {
    let mut h = Sha256::new();
    h.update(kind.as_str().as_bytes());
    h.update([0u8]);
    h.update(natural_key.as_bytes());
    let hex = to_hex(&h.finalize());
    format!("nod_{}", &hex[..24])
}

/// Upsert a node by identity, updating content-on-touch. Returns its rowid.
/// Rejects an empty display name (`L-C4` — a node must be citable).
pub(crate) fn upsert_node(
    conn: &Connection,
    node: &NodeInput,
    now: &str,
) -> Result<i64, ContextError> {
    if node.display_name.trim().is_empty() {
        return Err(ContextError::InvalidInput(
            "node display_name must be non-empty (every node must be humanly citable, L-C4)".into(),
        ));
    }
    let content_hash = sha256_hex(&node.content);
    let props = serde_json::to_string(&node.properties)?;
    let id: i64 = conn.query_row(
        "INSERT INTO node (public_id, kind, display_name, content, content_hash, uri, properties, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(public_id) DO UPDATE SET
             display_name = excluded.display_name,
             content      = excluded.content,
             content_hash = excluded.content_hash,
             uri          = excluded.uri,
             properties   = excluded.properties
         RETURNING id",
        params![
            node.public_id(),
            node.kind.as_str(),
            node.display_name,
            node.content,
            content_hash,
            node.uri,
            props,
            now
        ],
        |r| r.get(0),
    )?;
    Ok(id)
}

pub(crate) fn map_node_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeRow> {
    let kind_str: String = row.get("kind")?;
    Ok(NodeRow {
        id: row.get("id")?,
        public_id: row.get("public_id")?,
        // An unrecognized kind reads as `Concept` rather than failing the
        // whole query: one unknown row must not blind recall to every other
        // row. `migrate` rejects a store stamped by a newer stella, so a newer
        // binary's node kinds no longer reach here — what is left is a
        // hand-edited or partially restored db. The node stays retrievable and
        // citable; only its `FrameKind` is coarsened.
        kind: NodeKind::parse(&kind_str).unwrap_or(NodeKind::Concept),
        display_name: row.get("display_name")?,
        content: row.get("content")?,
        content_hash: row.get("content_hash")?,
        uri: row.get("uri")?,
        valid_from: row.get("valid_from")?,
        recorded_at: row.get("recorded_at")?,
    })
}

/// Live node ids whose uri matches one of `uris` (anchor resolution).
pub(crate) fn node_ids_for_uris(
    conn: &Connection,
    uris: &[String],
) -> Result<Vec<i64>, ContextError> {
    let mut out = Vec::new();
    let mut stmt = conn.prepare("SELECT id FROM node WHERE uri = ?1 AND superseded_at IS NULL")?;
    for uri in uris {
        let ids = stmt.query_map(params![uri], |r| r.get::<_, i64>(0))?;
        for id in ids {
            out.push(id?);
        }
    }
    Ok(out)
}

/// Fetch a single node by rowid.
pub(crate) fn node_by_id(conn: &Connection, id: i64) -> Result<Option<NodeRow>, ContextError> {
    let row = conn
        .query_row(
            "SELECT id, public_id, kind, display_name, content, content_hash, uri, valid_from, recorded_at
             FROM node WHERE id = ?1",
            params![id],
            map_node_row,
        )
        .optional()?;
    Ok(row)
}

/// Mark a node superseded at `now` — the context plane's tombstone. Returns
/// whether a live row actually changed, so an idempotent caller can tell
/// "suppressed it" from "it was already suppressed".
///
/// **Never deletes** (`L-C3`). The row survives with its content and its
/// history intact, which is what makes [`restore_node`] an exact inverse and
/// what lets a point-in-time query still see what was believed before the
/// suppression. Every candidate reader filters on this column, so the effect is
/// immediate and applies before any budget is spent.
///
/// The `superseded_at IS NULL` guard makes a second call a no-op rather than a
/// rewrite: re-forgetting must not move the tombstone's timestamp forward, or
/// a point-in-time query would start seeing a row it had already stopped
/// seeing.
pub(crate) fn supersede_node(
    conn: &Connection,
    public_id: &str,
    now: &str,
) -> Result<bool, ContextError> {
    let changed = conn.execute(
        "UPDATE node SET superseded_at = ?2 WHERE public_id = ?1 AND superseded_at IS NULL",
        params![public_id, now],
    )?;
    Ok(changed > 0)
}

/// Lift a node's supersession, making it a candidate again. The exact inverse
/// of [`supersede_node`]; returns whether anything was lifted.
///
/// Deliberately resolves by `public_id` without a liveness filter — a
/// superseded node is invisible to every other reader, so a restore that went
/// through one of them could never find the row it exists to bring back.
pub(crate) fn restore_node(conn: &Connection, public_id: &str) -> Result<bool, ContextError> {
    let changed = conn.execute(
        "UPDATE node SET superseded_at = NULL WHERE public_id = ?1 AND superseded_at IS NOT NULL",
        params![public_id],
    )?;
    Ok(changed > 0)
}

/// Whether a node exists at all under this id, superseded or not — the lookup
/// a restore needs, since every other reader hides exactly the rows it targets.
pub(crate) fn node_exists_any_state(
    conn: &Connection,
    public_id: &str,
) -> Result<bool, ContextError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM node WHERE public_id = ?1",
        params![public_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}
