//! The provider registry seam (
//! §2). One interface — [`ContextProvider`] — behind which context sources
//! register: the built-in [`ContextStore`] implements it (so the store is both
//! the primary backend and a first-class provider), and the shipping CLI's
//! session recall flows through this registry (`stella-cli/src/contextgraph.rs` wraps
//! it as the `workspace-memory` CGP provider, registering the store domain-
//! scoped).
//!
//! **External providers register on the CGP host, not here.** Third-party
//! stdio/HTTP sources are admitted by `stella-cli/src/contextgraph.rs`
//! (`register_external_providers`, #453) straight onto the
//! `contextgraph_host::Host`, gated on the protocol's conformance suite and on
//! egress consent. This registry stays the seam for *in-plane* sources that
//! share the workspace store's lifetime — a git-history provider, a future
//! reflection source — which is why this crate still takes no dependency on
//! `contextgraph-host`.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use contextgraph_types::capability::QueryCapability;
use contextgraph_types::{
    Capabilities, ContextQuery, ContextQueryResult, DataFlow, FrameVerdict, ProviderInfo, Verdict,
    VerifyRequest, VerifyResponse,
};

use crate::error::ContextError;
use crate::store::{ContextStore, NodeKind};

/// A source of context frames. Async because external providers are child
/// processes / HTTP endpoints; the built-in store resolves in-process.
#[async_trait]
pub trait ContextProvider: Send + Sync {
    /// Identity and data-flow declaration surfaced at install/consent time.
    /// A host gates `egress` providers
    /// on explicit consent using this.
    fn info(&self) -> ProviderInfo;

    /// What this provider can answer, for query routing.
    fn capabilities(&self) -> Capabilities;

    /// Return frames relevant to the query, as the CGP wire result so a
    /// provider's budget drops stay visible across the seam (`L-C5` — a bare
    /// frame list would erase the truncation report). Budgeting/fusion across
    /// providers is the host's job.
    async fn query(&self, q: &ContextQuery) -> Result<ContextQueryResult, ContextError>;

    /// Revalidate frame identities a host already holds, without any frame
    /// body travelling in either direction (`docs/context-reuse.md` §4,
    /// `context/verify`).
    ///
    /// The digest is the ground truth: a provider compares the digest the host
    /// presents against the digest its source carries *now* — equal is
    /// [`Verdict::Valid`], different is [`Verdict::Stale`], a vanished source
    /// is [`Verdict::Gone`], and anything it cannot judge is
    /// [`Verdict::Unknown`].
    ///
    /// Defaults to `Unknown` for every identity, mirroring
    /// `contextgraph_host::ContextProvider::verify`: a plane provider that
    /// implements nothing can never accidentally bless a stale frame, and the
    /// host degrades to re-querying (V3). A provider that overrides this
    /// **MUST** also advertise [`Capabilities::verify`], since the host only
    /// asks providers that declare support.
    async fn verify(&self, request: &VerifyRequest) -> Result<VerifyResponse, ContextError> {
        Ok(VerifyResponse::uniform(request, Verdict::Unknown))
    }
}

/// Every frame kind the built-in store can serve.
fn store_kinds() -> Vec<String> {
    [
        NodeKind::File,
        NodeKind::Symbol,
        NodeKind::Concept,
        NodeKind::Fact,
        NodeKind::Episode,
        NodeKind::Person,
        NodeKind::Artifact,
        NodeKind::Task,
        // The kind the store exists for — omitting it routed kind-filtered
        // memory queries away from the one provider that stores memories.
        NodeKind::Memory,
    ]
    .iter()
    .map(|k| k.to_frame_kind())
    // Serialize the FrameKind to its wire string via serde for one source of
    // truth on the spelling.
    .filter_map(|fk| {
        serde_json::to_value(fk)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
    })
    .collect::<HashSet<_>>()
    .into_iter()
    .collect()
}

#[async_trait]
impl ContextProvider for ContextStore {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "stella-context".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            // The local plane reads workspace content and persists upserts, but
            // nothing ever leaves the machine — no egress consent required.
            data_flow: DataFlow {
                reads: true,
                writes: true,
                egress: false,
                // Local plane: nothing leaves the machine, so no egress scopes.
                egress_scopes: vec![],
            },
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // `filters` and the `upsert`/`subscribe` flags were removed in the
            // CGP normative sweep (#33): durability is now declared on
            // `DataFlow.writes` (ADR 0004), and query filters are no longer a
            // separately advertised capability.
            query: QueryCapability {
                kinds: store_kinds(),
            },
            graph: true,
            embeddings_fingerprint: Some(self.fingerprint().id()),
            // The store can answer `context/verify` honestly: every frame it
            // mints declares `sha256:<node.content_hash>`, and the same hash is
            // re-read from the live row (`docs/context-reuse.md` §4).
            verify: true,
            ..Default::default()
        }
    }

    async fn query(&self, q: &ContextQuery) -> Result<ContextQueryResult, ContextError> {
        // The CGP-shaped adapter over the rich `recall` pipeline.
        Ok(self.recall(q).await?.into())
    }

    /// Compare each presented digest against the live node's `content_hash`
    /// (`docs/context-reuse.md` §4, requirement V1).
    ///
    /// The store mints `content_digest` as `sha256:<content_hash>` over exactly
    /// the bytes it serves as `content`, so re-reading the row is a faithful
    /// "what am I serving now?" — no frame body is read, sent, or compared.
    /// A superseded or deleted node is `Gone`; an identity presented without a
    /// digest is `Unknown` (nothing to compare), never `Valid`.
    async fn verify(&self, request: &VerifyRequest) -> Result<VerifyResponse, ContextError> {
        let mut verdicts = Vec::with_capacity(request.frames.len());
        for frame in &request.frames {
            let verdict = match self.node_by_public_id(&frame.frame_id)? {
                None => Verdict::Gone,
                Some(node) => {
                    let current = format!("sha256:{}", node.content_hash);
                    match frame.content_digest.as_deref() {
                        Some(presented) if presented == current => Verdict::Valid,
                        Some(_) => Verdict::Stale {
                            replacement_digest: Some(current),
                        },
                        // Silence is not validity: an identity with no digest
                        // is unverifiable, so the host re-queries it (§1 D4).
                        None => Verdict::Unknown,
                    }
                }
            };
            verdicts.push(FrameVerdict::new(frame.clone(), verdict));
        }
        Ok(VerifyResponse::new(verdicts))
    }
}

/// A set of registered providers. Fans a query out to those whose capabilities
/// match, then concatenates (deduping by frame id). Cross-provider reciprocal-
/// rank fusion is the store's internal pipeline today; multi-provider fusion is
/// the tracked follow-up once external CGP providers register here.
#[derive(Default)]
pub struct ProviderRegistry {
    providers: Vec<Arc<dyn ContextProvider>>,
}

impl ProviderRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider.
    pub fn register(&mut self, provider: Arc<dyn ContextProvider>) {
        self.providers.push(provider);
    }

    /// Number of registered providers.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Whether no providers are registered.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// The registered providers (for status/inspection surfaces).
    pub fn providers(&self) -> &[Arc<dyn ContextProvider>] {
        &self.providers
    }

    /// Whether a provider's declared kinds satisfy the query's requested kinds.
    /// An empty `q.kinds` matches every provider (no kind filter).
    fn matches(caps: &Capabilities, q: &ContextQuery) -> bool {
        if q.kinds.is_empty() {
            return true;
        }
        let wanted: HashSet<String> = q
            .kinds
            .iter()
            .filter_map(|k| {
                serde_json::to_value(k)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
            })
            .collect();
        caps.query.kinds.iter().any(|k| wanted.contains(k))
    }

    /// Fan the query to capability-matching providers and concatenate their
    /// frames, deduping by frame id (first writer wins). The truncation report
    /// aggregates across providers — `truncated` if any provider truncated,
    /// `dropped_estimate` summed over the providers that reported one — so the
    /// fan-out never erases a provider's honest drop report (`L-C5`).
    pub async fn query_all(&self, q: &ContextQuery) -> Result<ContextQueryResult, ContextError> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut frames = Vec::new();
        let mut truncated = false;
        let mut dropped_estimate: Option<u32> = None;
        for provider in &self.providers {
            if !Self::matches(&provider.capabilities(), q) {
                continue;
            }
            let result = provider.query(q).await?;
            truncated |= result.truncated;
            if let Some(d) = result.dropped_estimate {
                dropped_estimate = Some(dropped_estimate.unwrap_or(0).saturating_add(d));
            }
            for frame in result.frames {
                if seen.insert(frame.id.clone()) {
                    frames.push(frame);
                }
            }
        }
        Ok(ContextQueryResult {
            frames,
            truncated,
            dropped_estimate,
        })
    }

    /// Fan a `context/verify` request out to the plane providers that advertise
    /// the capability and merge their verdicts (`docs/context-reuse.md` §4).
    ///
    /// Plane provider ids are not part of a `FrameId` — the whole plane is one
    /// CGP provider (`workspace-memory`) to the host — so an identity is
    /// offered to every verify-capable provider and the **first definite**
    /// answer wins. `Unknown` is not definite: a provider that cannot judge an
    /// identity must not shadow one that can. An identity no provider can
    /// judge stays `Unknown`, which the host treats as "do not reuse" (V2).
    pub async fn verify_all(
        &self,
        request: &VerifyRequest,
    ) -> Result<VerifyResponse, ContextError> {
        let mut resolved: Vec<Option<Verdict>> = vec![None; request.frames.len()];
        for provider in &self.providers {
            if !provider.capabilities().verify {
                continue;
            }
            // Only ask about identities still unresolved, so a provider never
            // re-judges a frame another already vouched for.
            let pending: Vec<_> = request
                .frames
                .iter()
                .zip(resolved.iter())
                .filter(|(_, verdict)| verdict.is_none())
                .map(|(frame, _)| frame.clone())
                .collect();
            if pending.is_empty() {
                break;
            }
            let response = provider.verify(&VerifyRequest::new(pending)).await?;
            for (frame, slot) in request.frames.iter().zip(resolved.iter_mut()) {
                if slot.is_some() {
                    continue;
                }
                match response.verdict_for(frame) {
                    Some(Verdict::Unknown) | None => {}
                    Some(verdict) => *slot = Some(verdict.clone()),
                }
            }
        }
        let verdicts = request
            .frames
            .iter()
            .zip(resolved)
            .map(|(frame, verdict)| {
                FrameVerdict::new(frame.clone(), verdict.unwrap_or(Verdict::Unknown))
            })
            .collect();
        Ok(VerifyResponse::new(verdicts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{ContextStore, NodeInput};
    use crate::writeback::ContextDelta;
    use contextgraph_types::{ContextFrame, FrameKind};
    use tempfile::TempDir;

    async fn seeded_store() -> (TempDir, Arc<ContextStore>) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context.db");
        let store = Arc::new(ContextStore::open(&path).unwrap());
        store
            .upsert(
                ContextDelta::new().with_node(
                    NodeInput::new(NodeKind::File, "src/main.rs")
                        .with_uri("file:///repo/src/main.rs")
                        .with_content("fn main() { open the sqlite connection in wal mode }"),
                ),
            )
            .await
            .unwrap();
        (dir, store)
    }

    fn query(goal: &str) -> ContextQuery {
        ContextQuery {
            goal: goal.into(),
            query_text: None,
            embedding: None,
            kinds: vec![],
            anchors: vec![],
            max_frames: 10,
            max_tokens: 4000,
            as_of: None,
            representation_preferences: vec![],
        }
    }

    #[tokio::test]
    async fn store_advertises_local_no_egress_data_flow() {
        let (_dir, store) = seeded_store().await;
        let info = store.info();
        assert_eq!(info.name, "stella-context");
        assert!(info.data_flow.reads);
        assert!(info.data_flow.writes);
        assert!(!info.data_flow.egress, "the local plane never egresses");
    }

    #[tokio::test]
    async fn store_capabilities_report_upsert_graph_and_fingerprint() {
        let (_dir, store) = seeded_store().await;
        let caps = store.capabilities();
        // Durability moved from `capabilities.upsert` to `data_flow.writes`
        // in CGP #33 (ADR 0004).
        assert!(store.info().data_flow.writes);
        assert!(caps.graph);
        assert_eq!(
            caps.embeddings_fingerprint.as_deref(),
            Some("hash-ngram@1/256/l2")
        );
        assert!(caps.query.kinds.contains(&"snippet".to_string()));
    }

    #[tokio::test]
    async fn registry_fans_out_and_dedups_by_frame_id() {
        let (_dir, store) = seeded_store().await;
        let mut registry = ProviderRegistry::new();
        // Register the SAME store twice: the dedup must collapse duplicate ids.
        registry.register(store.clone());
        registry.register(store.clone());
        assert_eq!(registry.len(), 2);
        let result = registry
            .query_all(&query("open the sqlite connection"))
            .await
            .unwrap();
        let ids: HashSet<_> = result.frames.iter().map(|f| f.id.clone()).collect();
        assert_eq!(
            ids.len(),
            result.frames.len(),
            "no duplicate frame ids across providers"
        );
    }

    #[tokio::test]
    async fn kind_filter_routes_away_non_matching_providers() {
        let (_dir, store) = seeded_store().await;
        let mut registry = ProviderRegistry::new();
        registry.register(store);
        // The store serves File→snippet frames; asking only for `graph` kind
        // (which the store's node kinds never map to) skips it.
        let mut q = query("anything");
        q.kinds = vec![FrameKind::Graph];
        let result = registry.query_all(&q).await.unwrap();
        assert!(
            result.frames.is_empty(),
            "provider is routed away when kinds don't intersect"
        );
        assert!(!result.truncated, "a routed-away provider reports no drops");
    }

    /// A scripted provider for aggregation tests.
    struct Scripted {
        frames: Vec<ContextFrame>,
        truncated: bool,
        dropped_estimate: Option<u32>,
    }

    #[async_trait]
    impl ContextProvider for Scripted {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                name: "scripted".to_string(),
                version: "0".to_string(),
                data_flow: DataFlow {
                    reads: true,
                    writes: false,
                    egress: false,
                    egress_scopes: vec![],
                },
            }
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                query: QueryCapability {
                    kinds: store_kinds(),
                },
                ..Capabilities::default()
            }
        }

        async fn query(&self, _q: &ContextQuery) -> Result<ContextQueryResult, ContextError> {
            Ok(ContextQueryResult {
                frames: self.frames.clone(),
                truncated: self.truncated,
                dropped_estimate: self.dropped_estimate,
            })
        }
    }

    #[tokio::test]
    async fn fan_out_aggregates_the_truncation_report_instead_of_erasing_it() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(Scripted {
            frames: vec![],
            truncated: false,
            dropped_estimate: None,
        }));
        registry.register(Arc::new(Scripted {
            frames: vec![],
            truncated: true,
            dropped_estimate: Some(2),
        }));
        registry.register(Arc::new(Scripted {
            frames: vec![],
            truncated: true,
            dropped_estimate: Some(3),
        }));
        let result = registry.query_all(&query("anything")).await.unwrap();
        assert!(result.truncated, "one truncating provider taints the merge");
        assert_eq!(
            result.dropped_estimate,
            Some(5),
            "estimates sum over the providers that reported one (L-C5)"
        );
    }

    /// WITNESS (#451, `docs/context-reuse.md` §1): every store-minted frame
    /// declares a `content_digest` naming exactly the bytes it serves. A frame
    /// without one is unverifiable and a host must re-query rather than reuse
    /// it (D4), which is precisely the cache-busting this issue removes.
    #[tokio::test]
    async fn store_frames_declare_a_content_digest_over_their_own_content() {
        let (_dir, store) = seeded_store().await;
        let result = store
            .query(&query("open the sqlite connection"))
            .await
            .unwrap();
        assert!(!result.frames.is_empty(), "seeded node must surface");
        for frame in &result.frames {
            let digest = frame
                .content_digest
                .as_deref()
                .unwrap_or_else(|| panic!("frame `{}` declares no content_digest", frame.id));
            let expected = format!(
                "sha256:{}",
                crate::store::sha256_hex(frame.content.as_deref().unwrap_or_default())
            );
            assert_eq!(
                digest, expected,
                "the declared digest must name the bytes actually served (§1 D1)"
            );
        }
    }

    /// WITNESS (#451, §4 V1): the store answers `context/verify` by comparing
    /// digests — `valid` for what it still serves, `stale` for a digest that
    /// differs, `gone` for a node that no longer exists. A provider that
    /// rubber-stamped everything `valid` would let a host cite stale evidence.
    #[tokio::test]
    async fn store_verifies_by_digest_and_never_rubber_stamps() {
        use contextgraph_types::FrameId;

        let (_dir, store) = seeded_store().await;
        assert!(
            store.capabilities().verify,
            "the store must advertise `verify`, or the host will never ask (V3)"
        );

        let served = store
            .query(&query("open the sqlite connection"))
            .await
            .unwrap();
        let frame = served.frames.first().expect("a served frame");
        let real = FrameId::new("workspace-memory", &frame.id, frame.content_digest.clone());
        let mutated = FrameId::new(
            "workspace-memory",
            &frame.id,
            Some(format!("sha256:{}", "0".repeat(64))),
        );
        let absent = FrameId::new(
            "workspace-memory",
            "nod_does_not_exist",
            Some("sha256:whatever".to_string()),
        );
        let undigested = FrameId::new("workspace-memory", &frame.id, None);

        let response = store
            .verify(&VerifyRequest::new(vec![
                real.clone(),
                mutated.clone(),
                absent.clone(),
                undigested.clone(),
            ]))
            .await
            .unwrap();

        assert_eq!(response.verdict_for(&real), Some(&Verdict::Valid));
        assert!(
            matches!(
                response.verdict_for(&mutated),
                Some(Verdict::Stale { replacement_digest: Some(d) })
                    if Some(d.as_str()) == frame.content_digest.as_deref()
            ),
            "a differing digest must be `stale`, carrying the current digest and no body (V4)"
        );
        assert_eq!(response.verdict_for(&absent), Some(&Verdict::Gone));
        assert_eq!(
            response.verdict_for(&undigested),
            Some(&Verdict::Unknown),
            "an identity with no digest is unverifiable, never `valid` (§1 D4)"
        );
    }

    /// A plane provider that cannot judge must not shadow one that can: the
    /// registry fan-out takes the first *definite* verdict, and leaves an
    /// identity nobody can judge as `Unknown` (default-deny, V2).
    #[tokio::test]
    async fn verify_fan_out_prefers_a_definite_verdict_over_unknown() {
        use contextgraph_types::FrameId;

        let (_dir, store) = seeded_store().await;
        let mut registry = ProviderRegistry::new();
        // The abstaining provider is registered FIRST, so a naive
        // first-response-wins merge would report `Unknown` for everything.
        registry.register(Arc::new(Abstaining));
        registry.register(store.clone());

        let served = store
            .query(&query("open the sqlite connection"))
            .await
            .unwrap();
        let frame = served.frames.first().expect("a served frame");
        let real = FrameId::new("workspace-memory", &frame.id, frame.content_digest.clone());

        let response = registry
            .verify_all(&VerifyRequest::new(vec![real.clone()]))
            .await
            .unwrap();
        assert_eq!(
            response.verdict_for(&real),
            Some(&Verdict::Valid),
            "the abstaining provider must not shadow the one that can vouch"
        );

        // And with nobody able to judge, the answer stays `Unknown` — which
        // the host treats as "do not reuse" (V2), never as silent assent.
        let mut abstainers = ProviderRegistry::new();
        abstainers.register(Arc::new(Abstaining));
        let response = abstainers
            .verify_all(&VerifyRequest::new(vec![real.clone()]))
            .await
            .unwrap();
        assert_eq!(response.verdict_for(&real), Some(&Verdict::Unknown));
    }

    /// A verify-capable provider that abstains on everything.
    struct Abstaining;

    #[async_trait]
    impl ContextProvider for Abstaining {
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                name: "abstaining".to_string(),
                version: "0".to_string(),
                data_flow: DataFlow {
                    reads: true,
                    writes: false,
                    egress: false,
                    egress_scopes: vec![],
                },
            }
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                verify: true,
                ..Capabilities::default()
            }
        }
        async fn query(&self, _q: &ContextQuery) -> Result<ContextQueryResult, ContextError> {
            Ok(ContextQueryResult {
                frames: vec![],
                truncated: false,
                dropped_estimate: None,
            })
        }
    }
}
