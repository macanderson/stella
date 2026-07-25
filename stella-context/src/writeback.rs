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
use crate::store::{
    ContextStore, NodeInput, NodeKind, close_edge, currently_valid_edge, edges_as_of,
    embedding_exists, insert_edge, insert_episode, insert_memory, node_by_id, sha256_hex,
    store_embedding, tag_edge_domains, tag_node_domains, to_hex, upsert_domain, upsert_node,
};

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
        }
    }

    /// A memory of an explicit kind.
    pub fn new(kind: MemoryKind, content: impl Into<String>) -> Self {
        Self {
            kind,
            content: content.into(),
            domains: Vec::new(),
            salience: 0.0,
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

    /// Stable identity: kind + content.
    fn public_id(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.kind.as_str().as_bytes());
        h.update([0u8]);
        h.update(self.content.as_bytes());
        format!("mem_{}", &to_hex(&h.finalize())[..24])
    }

    /// The retrievable Memory node mirroring this memory (carries its domains).
    fn as_node(&self) -> NodeInput {
        let label = truncate_label(&self.content);
        NodeInput::new(NodeKind::Memory, label)
            .with_uri(format!("memory://{}", self.public_id()))
            .with_content(self.content.clone())
            .with_domains(self.domains.clone())
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
    /// Prior beliefs closed by a single-valued correction. They are closed and
    /// linked with `SUPERSEDES`, never deleted (`L-C3`).
    pub facts_superseded: usize,
    /// Vectors the embedder produced this batch — content with no vector yet
    /// under the active fingerprint.
    pub embeddings_computed: usize,
    /// Content that already had a vector for `(content_hash, fingerprint)` and
    /// so was not re-embedded — the byte-compat skip (`L-C2`).
    pub embeddings_reused: usize,
    /// New domain-tag associations written across all records this batch.
    pub domain_tags_added: usize,
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
            let embeddings = self.embedder().embed(&texts).await?;
            for (hash, emb) in hashes.into_iter().zip(embeddings) {
                new_vectors.push((hash, emb.vector));
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

/// Insert a fact edge, superseding a prior single-valued belief if the object
/// changed. Idempotent when the same `(subject, predicate, object)` is
/// re-asserted.
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

    // Single-valued: find the current belief for (subject, predicate).
    match currently_valid_edge(tx, src, &fact.predicate)? {
        Some((_, existing_dst)) if existing_dst == dst => {
            // Same object → idempotent no-op; the belief already holds.
        }
        Some((existing_edge, _)) => {
            // Object changed → close the old interval, link SUPERSEDES.
            let valid_to = fact.valid_from.as_deref().unwrap_or(now);
            close_edge(tx, existing_edge, now, valid_to)?;
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
                Some(existing_edge),
            )?;
            receipt.domain_tags_added += tag_edge_domains(tx, edge_id, &fact.domains, now)?;
            receipt.facts_superseded += 1;
            receipt.facts_asserted += 1;
        }
        None => {
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
}
