//! Write-back: memory flowing the other way.
//! [`ContextStore::upsert`] persists episode summaries, indexed content nodes,
//! and fact assertions with **bi-temporal supersession** — a correction closes
//! the prior belief's intervals and links the new edge with `SUPERSEDES`, so
//! "what did we believe at T1" still answers after a T2 correction (`L-C3`).
//! The whole delta is one transaction (`L-L1` crash consistency), and
//! byte-identical content under the active fingerprint is never re-embedded
//! (`L-C2`).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::ContextError;
use crate::retrieval::RecallTier;
use crate::store::{
    ContextStore, NodeInput, NodeKind, close_edge, edges_as_of, embedding_exists,
    end_world_validity, insert_edge, insert_episode, insert_memory, node_by_id, open_anchors,
    sha256_hex, store_embedding, tag_edge_domains, tag_node_domains, to_hex, upsert_domain,
    upsert_node,
};
use crate::warm;

/// `edge.rel` for a memory-to-file anchor: *this memory is about that file*.
///
/// One string, exported, because three planes have to agree on it — the write
/// side in [`ContextStore::upsert`], the staleness scan that ends its world
/// validity, and any future recall that traverses it. A literal repeated in
/// three crates is a rename away from anchors that silently stop matching.
pub const ANCHOR_REL: &str = "observed_in";

/// How an episode turned out. Stored as its `as_str` form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeOutcome {
    /// The episode achieved what it set out to do.
    Success,
    /// The episode ran to completion but did not achieve its goal (e.g.
    /// verification failed).
    Failure,
    /// Some of the goal landed and some did not.
    Partial,
    /// Stopped before finishing — cancelled, budget-exhausted, or interrupted.
    Aborted,
}

impl EpisodeOutcome {
    /// The canonical string stored in `episode.outcome`.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            EpisodeOutcome::Success => "success",
            EpisodeOutcome::Failure => "failure",
            EpisodeOutcome::Partial => "partial",
            EpisodeOutcome::Aborted => "aborted",
        }
    }
}

/// An episodic-memory write: a one-turn/one-session summary with the files it
/// touched and how it ended. It becomes both an `episode` row and a retrievable
/// `Episode` node (so recall can surface prior turns).
#[derive(Debug, Clone)]
pub struct EpisodeInput {
    /// What happened, in prose. Doubles as the mirror node's retrievable
    /// content and (truncated) its citation label, so this is the text recall
    /// actually embeds and surfaces.
    pub summary: String,
    /// Workspace-relative paths the episode touched, stored as a JSON array on
    /// the `episode` row.
    pub files_touched: Vec<String>,
    /// How the episode ended.
    pub outcome: EpisodeOutcome,
    /// RFC-3339 start of the episode's window.
    pub started_at: String,
    /// RFC-3339 end of the episode's window. With `summary` and `started_at`
    /// it forms the episode's stable `epi_…` identity, so re-writing the same
    /// summary over the same window updates one row instead of appending.
    pub ended_at: String,
    /// Caller-assigned importance, stored on the `episode` row. Recall does not
    /// rank by it today (scoring is similarity + recency + graph adjacency);
    /// it is a hint for consumers and for a future salience-aware ranker.
    pub salience: f32,
    /// Workspace domain tags carried onto the episode's mirror node.
    pub domains: Vec<String>,
}

impl EpisodeInput {
    /// A minimal successful episode with just a summary.
    pub fn new(
        summary: impl Into<String>,
        started_at: impl Into<String>,
        ended_at: impl Into<String>,
    ) -> Self {
        Self {
            summary: summary.into(),
            files_touched: Vec::new(),
            outcome: EpisodeOutcome::Success,
            started_at: started_at.into(),
            ended_at: ended_at.into(),
            salience: 0.0,
            domains: Vec::new(),
        }
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

    /// Stable identity: the summary plus its time window.
    fn public_id(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.summary.as_bytes());
        h.update([0u8]);
        h.update(self.started_at.as_bytes());
        h.update([0u8]);
        h.update(self.ended_at.as_bytes());
        format!("epi_{}", &to_hex(&h.finalize())[..24])
    }

    /// The retrievable Episode node mirroring this episode (carries its domains).
    fn as_node(&self) -> NodeInput {
        let label = truncate_label(&self.summary);
        NodeInput::new(NodeKind::Episode, label)
            .with_uri(format!("episode://{}", self.public_id()))
            .with_content(self.summary.clone())
            .with_domains(self.domains.clone())
    }
}

/// A fact assertion: `subject —predicate→ object`. Single-valued by default:
/// asserting a new object for the same `(subject, predicate)` supersedes the
/// prior belief. Set `multivalued` for facts that legitimately have several
/// concurrent objects.
#[derive(Debug, Clone)]
pub struct FactAssertion {
    /// The node the assertion is about. Upserted with the fact, so an
    /// assertion can introduce its own endpoints.
    pub subject: NodeInput,
    /// The relation, stored verbatim as `edge.rel`. With `subject` it is the
    /// key single-valued supersession matches on.
    pub predicate: String,
    /// The node the predicate points at. Also upserted with the fact.
    pub object: NodeInput,
    /// World time the belief became true (`edge.valid_from`) — independent of
    /// when it was recorded, and it may precede it. `None` = valid since it
    /// was observed. It also dates the close of the belief this one supersedes.
    pub valid_from: Option<String>,
    /// World time the belief stops holding (`edge.valid_to`); `None` leaves the
    /// interval open-ended.
    pub valid_to: Option<String>,
    /// Edge strength. It is the per-hop contribution in retrieval's
    /// graph-adjacency signal, so a heavier fact pulls its neighbors further up
    /// the fused ranking. Defaults to `1.0`.
    pub weight: f64,
    /// Free-form JSON stored on the edge row, for consumers reading the graph
    /// directly. Nothing in the retrieval path reads it back today.
    pub properties: serde_json::Value,
    /// When true the fact coexists with other objects for the same
    /// `(subject, predicate)` instead of superseding them. Re-asserting the
    /// exact same triple is a no-op either way.
    pub multivalued: bool,
    /// Domain tags applied to the resulting fact edge.
    pub domains: Vec<String>,
}

impl FactAssertion {
    /// A single-valued fact with default weight and no explicit validity.
    pub fn new(subject: NodeInput, predicate: impl Into<String>, object: NodeInput) -> Self {
        Self {
            subject,
            predicate: predicate.into(),
            object,
            valid_from: None,
            valid_to: None,
            weight: 1.0,
            properties: serde_json::json!({}),
            multivalued: false,
            domains: Vec::new(),
        }
    }

    /// Tag the fact edge with one or more workspace domains.
    #[must_use]
    pub fn with_domains<I, S>(mut self, domains: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.domains = domains.into_iter().map(Into::into).collect();
        self
    }
}

/// The kind of a memory record. Reflections are the post-turn self-improvement
/// lessons the CLI/pipeline writes after every chat turn (generation is
/// stella-pipeline/CLI scope; storage + recall are this crate's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// A post-turn self-improvement lesson.
    Reflection,
    /// A durable note the agent chose to remember.
    Note,
    /// An extracted insight/preference.
    Insight,
}

impl MemoryKind {
    /// The canonical string stored in `memory.kind`.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryKind::Reflection => "reflection",
            MemoryKind::Note => "note",
            MemoryKind::Insight => "insight",
        }
    }

    /// Parse a stored `memory.kind` back into the enum.
    ///
    /// Needed by the revision path: rewriting a memory must not silently
    /// reclassify it, so an edit reads the lineage's current kind and carries
    /// it forward rather than assuming one.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "reflection" => MemoryKind::Reflection,
            "note" => MemoryKind::Note,
            "insight" => MemoryKind::Insight,
            _ => return None,
        })
    }
}

/// A memory to write — content, kind, domains, salience. It becomes a `memory`
/// record and a retrievable `Memory` node, so future turns recall it by
/// similarity + domain overlap + recency.
#[derive(Debug, Clone)]
pub struct MemoryInput {
    /// Which sort of memory this is; stored on the `memory` row.
    pub kind: MemoryKind,
    /// The lesson/note text. It is both the stored record's body and the
    /// mirror node's content, so it is what recall embeds and what a returned
    /// frame carries.
    pub content: String,
    /// Workspace domain tags carried onto the memory's mirror node; recall
    /// scores domain overlap against them.
    pub domains: Vec<String>,
    /// Caller-assigned importance, stored on the `memory` row. As with
    /// [`EpisodeInput::salience`], recall does not rank by it today.
    pub salience: f32,
    /// The lineage this memory belongs to, when it revises an existing one.
    ///
    /// `None` — the overwhelmingly common case — means "a memory in its own
    /// right", and its lineage is seeded from its content hash so writing the
    /// same lesson twice stays the idempotent upsert it has always been.
    ///
    /// `Some(lineage)` makes this a **revision**: it lands as a new row under
    /// that lineage, closes whatever revision was live, and overwrites the one
    /// mirror node the lineage owns. That is the whole of #712 deliverable 5 —
    /// without it, editing a memory minted a second one and left the first
    /// live, with its old text and its own vector, competing for slots in every
    /// future recall.
    pub lineage: Option<String>,
    /// Which precedence band this memory's mirror node competes in once a
    /// recall budget binds. Defaults to [`RecallTier::Normal`]; the reflection
    /// lifecycle writes process notes as [`RecallTier::Deferred`] so a durable
    /// fact about the codebase is not evicted by commentary about the turn.
    pub recall_tier: RecallTier,
    /// Workspace-relative paths of the files this memory is *about*.
    ///
    /// Each becomes an `observed_in` edge from the memory's mirror node to a
    /// [`NodeKind::File`] node, which is what turns *"what does the agent know
    /// about `registry.py`"* into an edge traversal rather than an embedding
    /// guess.
    ///
    /// The edges are written with an open `valid_from`/`valid_to`: we do not
    /// claim to know when the memory *started* being about the file, only that
    /// it is now. When the file is deleted, the staleness scan ends the world
    /// validity — the belief is never retracted, because it was not wrong (see
    /// `store::edge::end_world_validity`).
    ///
    /// Anchors are a claim about the *subject* of the memory, not a log of what
    /// the turn happened to touch. A turn edits a dozen files; the lesson is
    /// usually about one.
    pub anchors: Vec<String>,
}

impl MemoryInput {
    /// A reflection memory tagged with the given domains.
    pub fn reflection<I, S>(content: impl Into<String>, domains: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            kind: MemoryKind::Reflection,
            content: content.into(),
            domains: domains.into_iter().map(Into::into).collect(),
            salience: 0.0,
            lineage: None,
            recall_tier: RecallTier::Normal,
            anchors: Vec::new(),
        }
    }

    /// Anchor this memory to the files it is about. See [`MemoryInput::anchors`].
    #[must_use]
    pub fn with_anchors<I, S>(mut self, anchors: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.anchors = anchors.into_iter().map(Into::into).collect();
        self
    }

    /// Ask for a non-default precedence band at recall. See [`RecallTier`].
    #[must_use]
    pub fn with_recall_tier(mut self, tier: RecallTier) -> Self {
        self.recall_tier = tier;
        self
    }

    /// A memory of an explicit kind.
    pub fn new(kind: MemoryKind, content: impl Into<String>) -> Self {
        Self {
            kind,
            content: content.into(),
            domains: Vec::new(),
            salience: 0.0,
            lineage: None,
            recall_tier: RecallTier::Normal,
            anchors: Vec::new(),
        }
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

    /// Mark this memory a revision of an existing lineage.
    ///
    /// The lineage id is the `mem_…` id of any revision in it — the id
    /// `ContextStore::memory_lineage` resolves from a recalled frame's node id.
    #[must_use]
    pub fn revises(mut self, lineage: impl Into<String>) -> Self {
        self.lineage = Some(lineage.into());
        self
    }

    /// This revision's own identity: kind + content.
    ///
    /// Still content-derived, and deliberately so. It is now the identity of a
    /// *revision* rather than of the memory, which is exactly what a
    /// content hash is good for: re-writing identical text is the same revision
    /// and upserts, while changed text is a new one.
    fn public_id(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.kind.as_str().as_bytes());
        h.update([0u8]);
        h.update(self.content.as_bytes());
        format!("mem_{}", &to_hex(&h.finalize())[..24])
    }

    /// The durable identity: the lineage when this revises one, else this
    /// revision's own id.
    ///
    /// Seeding a fresh memory's lineage from its content hash is what keeps
    /// this migration invisible: the lineage id of every memory written before
    /// #712 equals the id it already had, so its mirror node's uri, its node
    /// id, and any tombstone naming it are all unchanged.
    fn lineage_id(&self) -> String {
        self.lineage.clone().unwrap_or_else(|| self.public_id())
    }

    /// The retrievable Memory node mirroring this memory (carries its domains).
    ///
    /// Keyed by **lineage**, not by revision, which is what makes an edit
    /// update one node in place instead of minting a second. The old revision's
    /// vector is orphaned rather than deleted — `vectors_for_fingerprint` joins
    /// through `node.content_hash`, so a vector no live node points at is never
    /// selected again.
    fn as_node(&self) -> NodeInput {
        let label = truncate_label(&self.content);
        NodeInput::new(NodeKind::Memory, label)
            .with_uri(format!("memory://{}", self.lineage_id()))
            .with_content(self.content.clone())
            .with_domains(self.domains.clone())
            .with_recall_tier(self.recall_tier)
    }
}

/// An explicit domain definition (name + optional description). Writing bare
/// domain names on nodes/edges auto-creates them; this is how a caller attaches
/// a description (e.g. the `stella init` taxonomy).
#[derive(Debug, Clone)]
pub struct DomainInput {
    /// The domain's unique name, exactly as it appears in the `domains` tag
    /// lists on nodes, edges, episodes and memories.
    pub name: String,
    /// Optional prose describing the domain. This is the whole reason to write
    /// a domain explicitly — a bare tag auto-creates the name with no
    /// description.
    pub description: Option<String>,
}

impl DomainInput {
    /// A domain with the given name and optional description.
    pub fn new(name: impl Into<String>, description: Option<String>) -> Self {
        Self {
            name: name.into(),
            description,
        }
    }
}

/// A batch of context writes applied atomically.
#[derive(Debug, Clone, Default)]
pub struct ContextDelta {
    /// Explicit domain definitions (names + descriptions). Bare domain names on
    /// other records auto-create domains, so this is only needed to attach a
    /// description.
    pub domains: Vec<DomainInput>,
    /// Bare content nodes to index (docs, snippets, symbols).
    pub nodes: Vec<NodeInput>,
    /// Episodic-memory summaries.
    pub episodes: Vec<EpisodeInput>,
    /// Reflection/other memories.
    pub memories: Vec<MemoryInput>,
    /// Fact assertions with bi-temporal supersession.
    pub facts: Vec<FactAssertion>,
}

impl ContextDelta {
    /// An empty delta to build up fluently.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a content node.
    #[must_use]
    pub fn with_node(mut self, node: NodeInput) -> Self {
        self.nodes.push(node);
        self
    }

    /// Add an episode.
    #[must_use]
    pub fn with_episode(mut self, ep: EpisodeInput) -> Self {
        self.episodes.push(ep);
        self
    }

    /// Add a fact assertion.
    #[must_use]
    pub fn with_fact(mut self, fact: FactAssertion) -> Self {
        self.facts.push(fact);
        self
    }

    /// Add a memory.
    #[must_use]
    pub fn with_memory(mut self, memory: MemoryInput) -> Self {
        self.memories.push(memory);
        self
    }

    /// Add an explicit domain definition.
    #[must_use]
    pub fn with_domain(mut self, domain: DomainInput) -> Self {
        self.domains.push(domain);
        self
    }
}

/// What a write-back did — a typed, inspectable receipt.
/// `embeddings_reused` is the byte-compat skip count that
/// makes re-indexing cheap (`L-C2`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpsertReceipt {
    /// Node upsert *operations*, not distinct nodes: an episode and a memory
    /// each mirror one node, and every fact counts its subject and its object.
    /// Re-asserting a fact about the same subject therefore increments this
    /// even though no new row appears — read it as work done, not as growth.
    pub nodes_upserted: usize,
    /// `episode` rows written (inserted or updated in place by identity).
    pub episodes_written: usize,
    /// `memory` rows written (inserted or updated in place by identity).
    pub memories_written: usize,
    /// Fact edges inserted. Re-asserting a `(subject, predicate, object)` that
    /// is already believed inserts nothing and is not counted.
    pub facts_asserted: usize,
    /// Prior beliefs closed by a single-valued correction — **all** of the
    /// live ones, so a correction over two coexisting beliefs counts 2
    /// (#617). They are closed and linked with `SUPERSEDES`, never deleted
    /// (`L-C3`).
    pub facts_superseded: usize,
    /// Vectors the embedder produced this batch — content with no vector yet
    /// under the active fingerprint.
    pub embeddings_computed: usize,
    /// Content that already had a vector for `(content_hash, fingerprint)` and
    /// so was not re-embedded — the byte-compat skip (`L-C2`).
    pub embeddings_reused: usize,
    /// New domain-tag associations written across all records this batch.
    pub domain_tags_added: usize,
    /// `observed_in` anchor edges inserted from a memory to a file it is about.
    /// Re-asserting an anchor that is already open on both time axes inserts
    /// nothing and is not counted; an anchor whose world validity was ended by
    /// the staleness scan *is* re-opened, because a file that came back is a
    /// new fact about the world rather than a duplicate of the old one.
    pub anchors_written: usize,
}

/// An open memory→file anchor, as the staleness scan sees it.
#[derive(Debug, Clone)]
pub struct AnchorView {
    /// Edge rowid — pass to [`ContextStore::end_anchor_validity`].
    pub edge_id: i64,
    /// The anchored node's full uri, e.g. `file://stella-cli/src/agent.rs`.
    pub uri: String,
    /// The uri with its `file://` scheme stripped: the workspace-relative path
    /// to test for existence. Anchors are written relative (matching
    /// `record_taxonomy`), so this joins onto the workspace root directly.
    pub path: String,
    /// The anchoring memory's text, so a report can name what goes stale.
    pub memory: String,
}

/// A fact resolved to human labels for point-in-time queries (`L-C4`: cite by
/// label, never a bare id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactView {
    /// The subject node's display name, or `<unknown>` if the row it points at
    /// is gone.
    pub subject: String,
    /// The relation, as stored in `edge.rel`.
    pub predicate: String,
    /// The object node's display name, or `<unknown>` if the row it points at
    /// is gone.
    pub object: String,
    /// Transaction time the belief was recorded (RFC-3339).
    pub recorded_at: String,
    /// Transaction time the belief was closed, or `None` while it is still
    /// believed. A `facts_as_of(Some(t))` view can show a belief that is
    /// superseded now but was live at `t`.
    pub superseded_at: Option<String>,
}

/// Truncate a summary into a node label without splitting a UTF-8 char.
fn truncate_label(s: &str) -> String {
    const MAX: usize = 80;
    let trimmed = s.trim();
    if trimmed.chars().count() <= MAX {
        return trimmed.to_string();
    }
    let truncated: String = trimmed.chars().take(MAX - 1).collect();
    format!("{truncated}…")
}

impl ContextStore {
    /// Apply a delta atomically, returning a receipt. Embedding decisions
    /// happen before the transaction (async), then all node/episode/fact/vector
    /// writes commit together (`L-L1`).
    pub async fn upsert(&self, delta: ContextDelta) -> Result<UpsertReceipt, ContextError> {
        let now = self.clock().now_rfc3339();
        let fingerprint = self.fingerprint().id();

        // Phase A: decide what to embed (async, no lock held).
        // Gather every distinct piece of embeddable content in this delta.
        let mut contents: Vec<(String, String)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let push =
            |content: &str, contents: &mut Vec<(String, String)>, seen: &mut HashSet<String>| {
                if content.is_empty() {
                    return;
                }
                let hash = sha256_hex(content);
                if seen.insert(hash.clone()) {
                    contents.push((hash, content.to_string()));
                }
            };
        for node in &delta.nodes {
            push(&node.content, &mut contents, &mut seen);
        }
        for ep in &delta.episodes {
            push(&ep.summary, &mut contents, &mut seen);
        }
        for memory in &delta.memories {
            push(&memory.content, &mut contents, &mut seen);
        }
        for fact in &delta.facts {
            push(&fact.subject.content, &mut contents, &mut seen);
            push(&fact.object.content, &mut contents, &mut seen);
        }

        // Partition into already-embedded (reused) vs missing (`L-C2`).
        let (missing, reused): (Vec<_>, Vec<_>) = {
            let conn = self.conn();
            let mut missing = Vec::new();
            let mut reused = Vec::new();
            for (hash, content) in contents {
                if embedding_exists(&conn, &hash, &fingerprint)? {
                    reused.push(hash);
                } else {
                    missing.push((hash, content));
                }
            }
            (missing, reused)
        };

        // Embed only the missing content. An empty batch would error, so guard.
        let mut new_vectors: Vec<(String, Vec<f32>)> = Vec::with_capacity(missing.len());
        if !missing.is_empty() {
            // Split rather than clone: the embedder wants `&[String]`, and
            // copying every body into a second vector doubled the peak memory
            // of a large delta (a full-workspace first index is exactly the
            // case where the bodies are biggest).
            let (hashes, texts): (Vec<String>, Vec<String>) = missing.into_iter().unzip();
            // One request per `warm::BATCH` bodies rather than one request for
            // the whole delta — a full-workspace first index handed a backend
            // with a request-size limit the entire corpus in a single call.
            // Same width as the warm indexer so there is one number to tune
            // (#616 item 8). `chunks` borrows the bodies, so the peak-memory
            // property the split above protects is preserved; only the short
            // hash strings are copied per chunk.
            for (hash_chunk, text_chunk) in
                hashes.chunks(warm::BATCH).zip(texts.chunks(warm::BATCH))
            {
                let embeddings = self.embedder().embed(text_chunk).await?;
                for (hash, emb) in hash_chunk.iter().zip(embeddings) {
                    new_vectors.push((hash.clone(), emb.vector));
                }
            }
        }

        // Phase B: one transaction for all writes (`L-L1`).
        let mut receipt = UpsertReceipt {
            embeddings_computed: new_vectors.len(),
            embeddings_reused: reused.len(),
            ..Default::default()
        };
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;

        // Explicit domain definitions first, so descriptions land even if the
        // same names are also referenced as bare tags below.
        for domain in &delta.domains {
            upsert_domain(&tx, &domain.name, domain.description.as_deref(), &now)?;
        }

        for node in &delta.nodes {
            let id = upsert_node(&tx, node, &now)?;
            receipt.domain_tags_added += tag_node_domains(&tx, id, &node.domains, &now)?;
            receipt.nodes_upserted += 1;
        }

        for ep in &delta.episodes {
            let files = serde_json::json!(ep.files_touched);
            insert_episode(
                &tx,
                &ep.public_id(),
                &ep.summary,
                &files,
                ep.outcome.as_str(),
                ep.salience as f64,
                &ep.started_at,
                &ep.ended_at,
                &now,
            )?;
            let node = ep.as_node();
            let id = upsert_node(&tx, &node, &now)?;
            receipt.domain_tags_added += tag_node_domains(&tx, id, &node.domains, &now)?;
            receipt.nodes_upserted += 1;
            receipt.episodes_written += 1;
        }

        for memory in &delta.memories {
            insert_memory(
                &tx,
                &memory.public_id(),
                &memory.lineage_id(),
                memory.kind.as_str(),
                &memory.content,
                memory.salience as f64,
                &now,
            )?;
            let node = memory.as_node();
            let id = upsert_node(&tx, &node, &now)?;
            receipt.domain_tags_added += tag_node_domains(&tx, id, &node.domains, &now)?;
            receipt.nodes_upserted += 1;
            receipt.memories_written += 1;

            // Anchor the memory to the files it is about. Written here rather
            // than left to the caller as a `FactAssertion` because the mirror
            // node's identity is minted in this loop — a caller would have to
            // reconstruct `memory://{lineage}` by hand and would silently
            // anchor to nothing the day that spelling changed.
            for path in &memory.anchors {
                let file = NodeInput::new(NodeKind::File, path.as_str())
                    .with_uri(format!("file://{path}"))
                    .with_domains(memory.domains.clone());
                let dst = upsert_node(&tx, &file, &now)?;
                receipt.domain_tags_added += tag_node_domains(&tx, dst, &file.domains, &now)?;
                receipt.nodes_upserted += 1;
                // Open on BOTH axes, not merely un-superseded: an anchor whose
                // file was deleted has `valid_to` set and `superseded_at` null,
                // and re-learning the same lesson after the file comes back
                // must open a new interval rather than be swallowed as a
                // duplicate. `live_triple_exists` checks belief only, which is
                // right for `apply_fact` and wrong here.
                if !open_anchor_exists(&tx, id, ANCHOR_REL, dst)? {
                    insert_edge(
                        &tx,
                        ANCHOR_REL,
                        id,
                        dst,
                        1.0,
                        &serde_json::json!({}),
                        // No `valid_from`: we know the memory is about this
                        // file now, not when that started being true. NULL is
                        // "unbounded in the past", which is the honest claim.
                        None,
                        None,
                        &now,
                        None,
                    )?;
                    receipt.anchors_written += 1;
                }
            }
        }

        for fact in &delta.facts {
            let src = upsert_node(&tx, &fact.subject, &now)?;
            let dst = upsert_node(&tx, &fact.object, &now)?;
            receipt.domain_tags_added += tag_node_domains(&tx, src, &fact.subject.domains, &now)?;
            receipt.domain_tags_added += tag_node_domains(&tx, dst, &fact.object.domains, &now)?;
            receipt.nodes_upserted += 2;
            apply_fact(&tx, fact, src, dst, &now, &mut receipt)?;
        }

        for (hash, vector) in &new_vectors {
            store_embedding(&tx, hash, &fingerprint, vector, &now)?;
        }

        tx.commit()?;
        Ok(receipt)
    }

    /// Every memory→file anchor still believed and still holding in the world.
    ///
    /// This is deliberately a *reader*, not a scan. The store knows which
    /// anchors are open; it does not know which files exist, and it must not
    /// learn — a workspace walk in here would put filesystem policy behind the
    /// database's back and make the store untestable without a real tree. The
    /// caller pairs this with [`ContextStore::end_anchor_validity`].
    pub fn open_anchors(&self) -> Result<Vec<AnchorView>, ContextError> {
        let conn = self.conn();
        Ok(open_anchors(&conn, ANCHOR_REL)?
            .into_iter()
            .map(|a| AnchorView {
                edge_id: a.edge_id,
                path: a.uri.strip_prefix("file://").unwrap_or(&a.uri).to_string(),
                uri: a.uri,
                memory: a.memory,
            })
            .collect())
    }

    /// Record that an anchor stopped holding in the world at `valid_to`.
    ///
    /// Ends world validity only: the memory is not retracted and `as_of`
    /// queries still report the anchor as believed, because it was never wrong
    /// — the file simply went away. Returns `false` if the anchor had already
    /// been ended, so a second scan cannot move the date the file disappeared.
    ///
    /// See `store::edge::end_world_validity` for why this is not `close_edge`.
    pub fn end_anchor_validity(&self, edge_id: i64, valid_to: &str) -> Result<bool, ContextError> {
        let conn = self.conn();
        end_world_validity(&conn, edge_id, valid_to)
    }

    /// Fact edges as believed at a transaction-time instant, resolved to human
    /// labels. `as_of = None` returns currently-believed facts; `Some(t)`
    /// reconstructs the belief set at `t` (`L-C3` audit query).
    pub fn facts_as_of(&self, as_of: Option<&str>) -> Result<Vec<FactView>, ContextError> {
        let conn = self.conn();
        let edges = edges_as_of(&conn, as_of)?;
        let mut out = Vec::with_capacity(edges.len());
        for edge in edges {
            let subject = node_by_id(&conn, edge.src_id)?
                .map(|n| n.display_name)
                .unwrap_or_else(|| "<unknown>".to_string());
            let object = node_by_id(&conn, edge.dst_id)?
                .map(|n| n.display_name)
                .unwrap_or_else(|| "<unknown>".to_string());
            out.push(FactView {
                subject,
                predicate: edge.rel,
                object,
                recorded_at: edge.recorded_at,
                superseded_at: edge.superseded_at,
            });
        }
        Ok(out)
    }
}

/// Every currently-believed edge with this subject and relation, newest
/// first, as `(edge_id, dst_id)`.
///
/// **All** of them, not just the newest (#617). This used to be
/// `currently_valid_edge`, an `ORDER BY id DESC LIMIT 1` in
/// [`crate::store`]: a single-valued assert closed the newest belief and left
/// every older live one open, so the store kept answering with two
/// simultaneous beliefs for a fact that is single-valued by definition, and
/// `facts_superseded` under-counted what the assert had actually replaced.
/// Two live edges arise the ordinary way — the same predicate asserted
/// multivalued (which is allowed to coexist) and later corrected as
/// single-valued.
fn live_edges(
    conn: &rusqlite::Connection,
    src: i64,
    rel: &str,
) -> Result<Vec<(i64, i64)>, ContextError> {
    let mut statement = conn.prepare(
        "SELECT id, dst_id FROM edge
         WHERE src_id = ?1 AND rel = ?2 AND superseded_at IS NULL
         ORDER BY id DESC",
    )?;
    let rows = statement.query_map(rusqlite::params![src, rel], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    })?;
    let mut edges = Vec::new();
    for row in rows {
        edges.push(row?);
    }
    Ok(edges)
}

/// Insert a fact edge, superseding **every** prior single-valued belief when
/// the object changed. Idempotent when the same `(subject, predicate,
/// object)` is re-asserted and it is the only live belief.
fn apply_fact(
    tx: &rusqlite::Connection,
    fact: &FactAssertion,
    src: i64,
    dst: i64,
    now: &str,
    receipt: &mut UpsertReceipt,
) -> Result<(), ContextError> {
    if fact.multivalued {
        // Multi-valued: coexist unless this exact triple is already live.
        if !live_triple_exists(tx, src, &fact.predicate, dst)? {
            let edge_id = insert_edge(
                tx,
                &fact.predicate,
                src,
                dst,
                fact.weight,
                &fact.properties,
                fact.valid_from.as_deref(),
                fact.valid_to.as_deref(),
                now,
                None,
            )?;
            receipt.domain_tags_added += tag_edge_domains(tx, edge_id, &fact.domains, now)?;
            receipt.facts_asserted += 1;
        }
        return Ok(());
    }

    // Single-valued: every currently-believed edge for (subject, predicate).
    let live = live_edges(tx, src, &fact.predicate)?;
    match live.as_slice() {
        // The belief already holds, and holds alone → idempotent no-op.
        [(_, existing_dst)] if *existing_dst == dst => {}
        [] => {
            let edge_id = insert_edge(
                tx,
                &fact.predicate,
                src,
                dst,
                fact.weight,
                &fact.properties,
                fact.valid_from.as_deref(),
                fact.valid_to.as_deref(),
                now,
                None,
            )?;
            receipt.domain_tags_added += tag_edge_domains(tx, edge_id, &fact.domains, now)?;
            receipt.facts_asserted += 1;
        }
        _ => {
            // A correction. Close EVERY live interval, not just the newest:
            // "single-valued" is the claim that at most one belief holds, and
            // leaving an older one open is the state that claim forbids
            // (#617). The newest becomes the new edge's `supersedes` link —
            // that column names one predecessor, and the newest is the belief
            // this assertion is actually correcting; the rest are closed with
            // the same transaction time, so a bi-temporal query at any earlier
            // instant still sees exactly what was believed then (`L-C3`).
            let valid_to = fact.valid_from.as_deref().unwrap_or(now);
            for (edge, _) in &live {
                close_edge(tx, *edge, now, valid_to)?;
            }
            let newest = live[0].0;
            let edge_id = insert_edge(
                tx,
                &fact.predicate,
                src,
                dst,
                fact.weight,
                &fact.properties,
                fact.valid_from.as_deref(),
                fact.valid_to.as_deref(),
                now,
                Some(newest),
            )?;
            receipt.domain_tags_added += tag_edge_domains(tx, edge_id, &fact.domains, now)?;
            // One per belief actually closed. The old code always added 1,
            // which under-reported a correction that replaced several.
            receipt.facts_superseded += live.len();
            receipt.facts_asserted += 1;
        }
    }
    Ok(())
}

/// Whether an exact live `(src, rel, dst)` edge already exists.
fn live_triple_exists(
    conn: &rusqlite::Connection,
    src: i64,
    rel: &str,
    dst: i64,
) -> Result<bool, ContextError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM edge
         WHERE src_id = ?1 AND rel = ?2 AND dst_id = ?3 AND superseded_at IS NULL",
        rusqlite::params![src, rel, dst],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Does an anchor exist that is open on **both** time axes?
///
/// The belief-only test ([`live_triple_exists`]) is the right idempotence check
/// for [`apply_fact`], where ending world validity is not a thing that happens.
/// It is the wrong one for anchors: the staleness scan sets `valid_to` and
/// deliberately leaves `superseded_at` null, so a deleted-then-restored file
/// would look "live" forever and never regain an anchor.
fn open_anchor_exists(
    conn: &rusqlite::Connection,
    src: i64,
    rel: &str,
    dst: i64,
) -> Result<bool, ContextError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM edge
         WHERE src_id = ?1 AND rel = ?2 AND dst_id = ?3
           AND superseded_at IS NULL AND valid_to IS NULL",
        rusqlite::params![src, rel, dst],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::embed::HashEmbedder;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn store_at(clock: Arc<FixedClock>) -> (TempDir, ContextStore) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        let store =
            ContextStore::open_with(&path, Arc::new(HashEmbedder::default()), clock).unwrap();
        (dir, store)
    }

    #[tokio::test]
    async fn supersession_preserves_point_in_time_belief() {
        // L-C3: correct a fact at T2; querying belief-time T1 still answers
        // with the pre-correction value; history is never destroyed.
        let clock = FixedClock::shared(1_000);
        let (_dir, store) = store_at(clock.clone());

        // T1: the build system is make.
        let fact = FactAssertion::new(
            NodeInput::new(NodeKind::Concept, "build system"),
            "IS",
            NodeInput::new(NodeKind::Concept, "make"),
        );
        let r1 = store
            .upsert(ContextDelta::new().with_fact(fact))
            .await
            .unwrap();
        assert_eq!(r1.facts_asserted, 1);
        assert_eq!(r1.facts_superseded, 0);
        let t1 = store.clock().now_rfc3339();

        // T2: we learn it's actually bazel — supersede.
        clock.advance(1_000);
        let fact2 = FactAssertion::new(
            NodeInput::new(NodeKind::Concept, "build system"),
            "IS",
            NodeInput::new(NodeKind::Concept, "bazel"),
        );
        let r2 = store
            .upsert(ContextDelta::new().with_fact(fact2))
            .await
            .unwrap();
        assert_eq!(r2.facts_superseded, 1, "the make belief was superseded");
        assert_eq!(r2.facts_asserted, 1);

        // Now (currently believed): bazel.
        let now_beliefs = store.facts_as_of(None).unwrap();
        assert_eq!(now_beliefs.len(), 1);
        assert_eq!(now_beliefs[0].object, "bazel");

        // As believed at T1: make. History survived the correction.
        let then = store.facts_as_of(Some(&t1)).unwrap();
        assert_eq!(then.len(), 1, "exactly one belief held at T1");
        assert_eq!(then[0].object, "make");
        assert_eq!(then[0].subject, "build system");
    }

    /// #617: a single-valued assert closes **every** live belief for its
    /// `(subject, predicate)`, not just the newest.
    ///
    /// Two live edges are reachable the ordinary way — the same predicate
    /// asserted multivalued, which is allowed to coexist, and later corrected
    /// as single-valued. Against the old `ORDER BY id DESC LIMIT 1` lookup
    /// this closed one of the two and left the store believing "the build
    /// system IS make" *and* "the build system IS buck2" simultaneously, with
    /// `facts_superseded` reporting 1 for a correction that replaced two.
    #[tokio::test]
    async fn a_single_valued_assert_closes_every_live_belief() {
        let clock = FixedClock::shared(1_000);
        let (_dir, store) = store_at(clock.clone());
        for object in ["make", "bazel"] {
            let mut fact = FactAssertion::new(
                NodeInput::new(NodeKind::Concept, "build system"),
                "IS",
                NodeInput::new(NodeKind::Concept, object),
            );
            fact.multivalued = true;
            store
                .upsert(ContextDelta::new().with_fact(fact))
                .await
                .unwrap();
        }
        assert_eq!(
            store.facts_as_of(None).unwrap().len(),
            2,
            "the fixture needs two coexisting live beliefs"
        );
        let before_correction = store.clock().now_rfc3339();

        clock.advance(1_000);
        let receipt = store
            .upsert(ContextDelta::new().with_fact(FactAssertion::new(
                NodeInput::new(NodeKind::Concept, "build system"),
                "IS",
                NodeInput::new(NodeKind::Concept, "buck2"),
            )))
            .await
            .unwrap();

        assert_eq!(receipt.facts_superseded, 2, "both beliefs were replaced");
        assert_eq!(receipt.facts_asserted, 1);
        let live = store.facts_as_of(None).unwrap();
        assert_eq!(
            live.len(),
            1,
            "a single-valued fact must leave exactly one live belief, got {live:?}"
        );
        assert_eq!(live[0].object, "buck2");

        // L-C3: closing them is not deleting them.
        let then = store.facts_as_of(Some(&before_correction)).unwrap();
        assert_eq!(then.len(), 2, "history survives the correction");
    }

    #[tokio::test]
    async fn reasserting_the_same_fact_is_idempotent() {
        let clock = FixedClock::shared(1_000);
        let (_dir, store) = store_at(clock);
        let make_fact = || {
            FactAssertion::new(
                NodeInput::new(NodeKind::Concept, "lang"),
                "IS",
                NodeInput::new(NodeKind::Concept, "rust"),
            )
        };
        store
            .upsert(ContextDelta::new().with_fact(make_fact()))
            .await
            .unwrap();
        let r2 = store
            .upsert(ContextDelta::new().with_fact(make_fact()))
            .await
            .unwrap();
        assert_eq!(r2.facts_asserted, 0, "same object → no new edge");
        assert_eq!(r2.facts_superseded, 0);
        assert_eq!(store.facts_as_of(None).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn byte_identical_content_is_never_re_embedded() {
        // L-C2: the byte-compat skip. The receipt distinguishes computed from
        // reused; the second upsert of identical content computes nothing.
        let clock = FixedClock::shared(1_000);
        let (_dir, store) = store_at(clock);
        let node = || {
            NodeInput::new(NodeKind::Concept, "doc")
                .with_content("a paragraph of stable indexed content")
        };
        let r1 = store
            .upsert(ContextDelta::new().with_node(node()))
            .await
            .unwrap();
        assert_eq!(r1.embeddings_computed, 1);
        assert_eq!(r1.embeddings_reused, 0);
        let r2 = store
            .upsert(ContextDelta::new().with_node(node()))
            .await
            .unwrap();
        assert_eq!(
            r2.embeddings_computed, 0,
            "identical content is reused, not re-embedded"
        );
        assert_eq!(r2.embeddings_reused, 1);
    }

    #[tokio::test]
    async fn episodes_are_written_and_become_retrievable_nodes() {
        let clock = FixedClock::shared(1_000);
        let (_dir, store) = store_at(clock);
        let ep = EpisodeInput::new(
            "fixed the failing budget test by clamping the token estimate",
            "2026-07-01T10:00:00Z",
            "2026-07-01T10:05:00Z",
        );
        let receipt = store
            .upsert(ContextDelta::new().with_episode(ep))
            .await
            .unwrap();
        assert_eq!(receipt.episodes_written, 1);
        assert_eq!(receipt.nodes_upserted, 1, "the episode also indexed a node");
        assert!(store.node_count().unwrap() >= 1);
    }

    #[tokio::test]
    async fn the_builder_can_write_memories_and_domains() {
        // Witness: before `with_memory`/`with_domain` existed the builder could
        // not express the crate's most important record type at all, so every
        // memory writer had to drop to struct-literal syntax — this body did
        // not compile.
        let clock = FixedClock::shared(1_000);
        let (_dir, store) = store_at(clock);
        let receipt = store
            .upsert(
                ContextDelta::new()
                    .with_domain(DomainInput::new("auth", Some("login and sessions".into())))
                    .with_memory(MemoryInput::reflection(
                        "prefer typed errors over stringly ones",
                        ["auth"],
                    )),
            )
            .await
            .unwrap();
        assert_eq!(receipt.memories_written, 1);
        assert_eq!(receipt.nodes_upserted, 1, "the memory also indexed a node");
        assert_eq!(
            receipt.domain_tags_added, 1,
            "the memory's mirror node carries the declared domain"
        );
    }

    #[tokio::test]
    async fn multivalued_facts_coexist() {
        let clock = FixedClock::shared(1_000);
        let (_dir, store) = store_at(clock);
        let mut a = FactAssertion::new(
            NodeInput::new(NodeKind::Concept, "service"),
            "DEPENDS_ON",
            NodeInput::new(NodeKind::Concept, "postgres"),
        );
        a.multivalued = true;
        let mut b = FactAssertion::new(
            NodeInput::new(NodeKind::Concept, "service"),
            "DEPENDS_ON",
            NodeInput::new(NodeKind::Concept, "redis"),
        );
        b.multivalued = true;
        store
            .upsert(ContextDelta::new().with_fact(a).with_fact(b))
            .await
            .unwrap();
        let beliefs = store.facts_as_of(None).unwrap();
        assert_eq!(
            beliefs.len(),
            2,
            "both dependencies are concurrently believed"
        );
    }

    /// A memory carrying anchors writes one `observed_in` edge per file, and
    /// the store reports them as open.
    #[tokio::test]
    async fn a_memory_anchors_to_the_files_it_is_about() {
        let clock = FixedClock::shared(1_000);
        let (_dir, store) = store_at(clock.clone());

        let receipt = store
            .upsert(
                ContextDelta::new().with_memory(
                    MemoryInput::reflection("handlers register in registry.rs", ["core"])
                        .with_anchors(["src/registry.rs", "src/main.rs"]),
                ),
            )
            .await
            .unwrap();

        assert_eq!(receipt.anchors_written, 2);
        let open = store.open_anchors().unwrap();
        let mut paths: Vec<String> = open.iter().map(|a| a.path.clone()).collect();
        paths.sort();
        assert_eq!(paths, vec!["src/main.rs", "src/registry.rs"]);
        assert!(
            open.iter().all(|a| a.memory.contains("registry.rs")),
            "each anchor names the memory it came from, so a scan can report it"
        );
    }

    /// Re-learning the same lesson must not grow the graph. The reflection loop
    /// re-mines paraphrases constantly, so an anchor that duplicated per turn
    /// would make one file accumulate hundreds of identical edges.
    #[tokio::test]
    async fn re_anchoring_the_same_file_writes_nothing_new() {
        let clock = FixedClock::shared(1_000);
        let (_dir, store) = store_at(clock.clone());

        let delta = || {
            ContextDelta::new().with_memory(
                MemoryInput::reflection("handlers register in registry.rs", ["core"])
                    .with_anchors(["src/registry.rs"]),
            )
        };
        let first = store.upsert(delta()).await.unwrap();
        clock.advance(1_000);
        let second = store.upsert(delta()).await.unwrap();

        assert_eq!(first.anchors_written, 1);
        assert_eq!(second.anchors_written, 0, "the anchor already held");
        assert_eq!(store.open_anchors().unwrap().len(), 1);
    }

    /// The distinction `live_triple_exists` cannot make.
    ///
    /// A deleted file ends the anchor's world validity but leaves it believed.
    /// A belief-only idempotence check therefore sees it as "already there"
    /// forever, and a file that comes back would never regain an anchor.
    #[tokio::test]
    async fn an_anchor_reopens_after_its_file_comes_back() {
        let clock = FixedClock::shared(1_000);
        let (_dir, store) = store_at(clock.clone());

        let delta = || {
            ContextDelta::new().with_memory(
                MemoryInput::reflection("handlers register in registry.rs", ["core"])
                    .with_anchors(["src/registry.rs"]),
            )
        };
        store.upsert(delta()).await.unwrap();
        let anchor = store.open_anchors().unwrap().remove(0);

        // The file is deleted: world validity ends, belief is untouched.
        clock.advance(1_000);
        let deleted_at = store.clock().now_rfc3339();
        assert!(
            store
                .end_anchor_validity(anchor.edge_id, &deleted_at)
                .unwrap()
        );
        assert!(
            store.open_anchors().unwrap().is_empty(),
            "an ended anchor is not open"
        );

        // The file comes back and the lesson is re-learned.
        clock.advance(1_000);
        let again = store.upsert(delta()).await.unwrap();
        assert_eq!(
            again.anchors_written, 1,
            "a file that returned is a new fact about the world, not a duplicate"
        );
        assert_eq!(store.open_anchors().unwrap().len(), 1);
    }

    /// Re-learning a forgotten memory must revive its mirror node.
    /// `insert_memory` already resurrects the record row (`superseded_at =
    /// NULL` on conflict); a mirror node left tombstoned would keep the
    /// lineage invisible to every candidate reader, so the re-learn would be
    /// a write with no recallable effect.
    #[tokio::test]
    async fn re_learning_a_forgotten_memory_revives_its_mirror_node() {
        let clock = FixedClock::shared(1_000);
        let (_dir, store) = store_at(clock.clone());
        let delta = || {
            ContextDelta::new().with_memory(MemoryInput::reflection(
                "the build system is bazel, not make",
                ["build"],
            ))
        };
        store.upsert(delta()).await.unwrap();
        let node = store.memory_nodes().unwrap().remove(0);

        clock.advance(1_000);
        assert!(store.supersede_node(&node.public_id).unwrap());
        assert!(
            store.node_by_public_id(&node.public_id).unwrap().is_none(),
            "the forgotten memory is hidden from every live reader"
        );

        clock.advance(1_000);
        store.upsert(delta()).await.unwrap();
        assert!(
            store.node_by_public_id(&node.public_id).unwrap().is_some(),
            "re-learning must lift the mirror node's tombstone"
        );

        // And recall actually serves it again — the whole point of the write.
        let q = contextgraph_types::ContextQuery {
            goal: "what is the build system".into(),
            query_text: Some("the build system is bazel, not make".into()),
            embedding: None,
            kinds: vec![],
            anchors: vec![],
            max_frames: 5,
            max_tokens: 2000,
            as_of: None,
            representation_preferences: vec![],
        };
        let result = store.recall(&q).await.unwrap();
        assert!(
            result.frames.iter().any(|f| f.id == node.public_id),
            "a re-learned memory must be recallable, got {:?}",
            result
                .frames
                .iter()
                .map(|f| f.id.as_str())
                .collect::<Vec<_>>()
        );
    }

    /// Ending an anchor twice must not move the date the file disappeared.
    #[tokio::test]
    async fn ending_an_anchor_twice_keeps_the_first_date() {
        let clock = FixedClock::shared(1_000);
        let (_dir, store) = store_at(clock.clone());
        store
            .upsert(
                ContextDelta::new().with_memory(
                    MemoryInput::reflection("about registry.rs", ["core"])
                        .with_anchors(["src/registry.rs"]),
                ),
            )
            .await
            .unwrap();
        let anchor = store.open_anchors().unwrap().remove(0);

        clock.advance(1_000);
        let first = store.clock().now_rfc3339();
        assert!(store.end_anchor_validity(anchor.edge_id, &first).unwrap());

        clock.advance(10_000);
        let later = store.clock().now_rfc3339();
        assert!(
            !store.end_anchor_validity(anchor.edge_id, &later).unwrap(),
            "a second scan reports no change rather than re-dating the deletion"
        );
    }
}
