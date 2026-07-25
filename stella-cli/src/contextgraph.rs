//! The session's CGP host: the in-tree context sources served through the
//! real `contextgraph-host` runtime instead of ad-hoc in-process calls.
//!
//! Until now the protocol and its conformance suite shipped, but the
//! shipping CLI's own retrieval bypassed them — the workspace memory store
//! was called directly and the code graph was not consulted at all. This
//! module closes that gap: recall builds one [`contextgraph_host::Host`], registers
//! two in-process providers, and fans every query out through
//! [`Host::query_all`] — the same consent gate, per-provider timeout,
//! crash isolation, and budget-honesty audit any external CGP provider
//! gets. "Code is a graph, not text" is now the runtime path, not just a
//! wire spec.
//!
//! - **`workspace-memory`** — the context plane: a
//!   [`stella_context::ProviderRegistry`] fan-out with the bi-temporal store
//!   registered domain-scoped (issue #103's wire decision — the store is
//!   queried through the plane's own provider seam, never directly).
//!   Reflections, episodes, facts, fused by the store's recall pipeline.
//! - **`code-graph`** — the tree-sitter index (`stella-graph`), opened
//!   read-only per query (the schema-gate discipline) on the blocking pool.
//!
//! Both are local, `egress: false` sources — the consent store passes them
//! without a prompt; only an egress provider would gate.
//!
//! ## Context reuse (`docs/context-reuse.md`)
//!
//! Recall also honors the protocol's reuse guarantees, which is what makes a
//! multi-turn session cheap rather than merely correct:
//!
//! - **§1 deterministic composition.** [`recall_via_host`] *selects* by score
//!   and *renders* in canonical `FrameId` order, so a turn whose underlying
//!   frames did not change emits byte-identical prompt text and rides the
//!   provider's prompt cache instead of busting it every turn.
//! - **§4 `context/verify`.** `workspace-memory` advertises and answers
//!   verify by comparing digests against the live store, so a host can
//!   revalidate held frames for bytes instead of re-querying them for tokens.
//!   `code-graph` does not advertise it and is re-queried — the conformant
//!   fallback (V3).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use contextgraph_conformance::{ProviderTarget, run_conformance};
use contextgraph_host::{
    ConsentDecision, ConsentRecord, ContextProvider, Host, HostError, ProviderResult,
};
use contextgraph_types::{
    Capabilities, ConsentReceipt, ContextFrame, ContextQuery, ContextQueryResult, DataFlow,
    EgressScope, FrameId, Grantor, ProviderInfo, UsageReport, VerifyRequest, VerifyResponse,
    canonical_order,
};
use stella_context::{
    ContextError, ContextProvider as PlaneProvider, ContextStore, ProviderRegistry,
};
use stella_protocol::{ContextProviderUsage, ContextUsage};

use crate::settings::{ContextProviderSettings, ExternalContextProvider, ProviderEndpoint};

/// Per-provider recall timeout. Recall runs before every turn, so a wedged
/// source must cost bounded latency — the host isolates it and the other
/// providers' frames still arrive.
const RECALL_TIMEOUT_MS: u64 = 2_000;

fn local_info(name: &str) -> ProviderInfo {
    ProviderInfo {
        name: name.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        data_flow: DataFlow {
            reads: true,
            writes: false,
            egress: false,
            egress_scopes: vec![],
        },
    }
}

/// The built-in store registered at the context-plane seam, with the
/// session's domain scope applied. Domain scoping is provider-internal:
/// CGP's `ContextQuery` is workspace-agnostic, and which taxonomy applies is
/// exactly the kind of local knowledge a provider owns. Identity and
/// capabilities are the store's own provider declarations.
struct ScopedStore {
    store: Arc<ContextStore>,
    domains: Vec<String>,
}

#[async_trait]
impl PlaneProvider for ScopedStore {
    fn info(&self) -> ProviderInfo {
        PlaneProvider::info(self.store.as_ref())
    }
    fn capabilities(&self) -> Capabilities {
        PlaneProvider::capabilities(self.store.as_ref())
    }
    async fn query(&self, query: &ContextQuery) -> Result<ContextQueryResult, ContextError> {
        Ok(self.store.recall_scoped(query, &self.domains).await?.into())
    }
    /// Domain scoping narrows *retrieval*, never *identity*: a frame this
    /// wrapper served is the store's frame, so revalidation is the store's
    /// answer, unmodified (`docs/context-reuse.md` §4).
    async fn verify(&self, request: &VerifyRequest) -> Result<VerifyResponse, ContextError> {
        PlaneProvider::verify(self.store.as_ref(), request).await
    }
}

/// The context-plane registry behind `workspace-memory`: the seam every
/// in-plane source registers through (issue #103's wire decision). Today
/// that is the built-in store, domain-scoped; a further plane source (a
/// git-history provider, an adapted external CGP provider) lands by
/// registering here, not by editing the host adapter.
fn memory_plane(store: Arc<ContextStore>, domains: Vec<String>) -> ProviderRegistry {
    let mut plane = ProviderRegistry::new();
    plane.register(Arc::new(ScopedStore { store, domains }));
    plane
}

/// The workspace context plane behind the CGP provider trait: recall fans
/// through the plane's [`ProviderRegistry`] instead of hitting the store
/// directly, so the registry's capability routing, id-dedup, and aggregated
/// truncation report are the production path.
struct MemoryProvider {
    plane: ProviderRegistry,
    info: ProviderInfo,
    caps: Capabilities,
}

#[async_trait]
impl ContextProvider for MemoryProvider {
    fn id(&self) -> &str {
        "workspace-memory"
    }
    fn info(&self) -> &ProviderInfo {
        &self.info
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    async fn query(&self, query: &ContextQuery) -> Result<ContextQueryResult, HostError> {
        let result = self
            .plane
            .query_all(query)
            .await
            .map_err(|e| HostError::Transport {
                id: "workspace-memory".to_string(),
                message: e.to_string(),
            })?;
        // The registry routes `query.kinds` at provider granularity, but the
        // store's recall does not honor it per-frame, so a kind-filtered
        // query (routed here because we advertise those kinds) must still be
        // filtered before returning — otherwise a `kinds: [Symbol]` request
        // could surface memory/fact frames. This filtering is NOT truncation:
        // `ContextQueryResult.truncated`/`dropped_estimate` describe candidates
        // that matched the request but were cut for budget, so they reflect
        // only the plane's own drops — a non-matching kind was never a
        // candidate for this query in the first place.
        let mut frames = result.frames;
        if !query.kinds.is_empty() {
            frames.retain(|f| query.kinds.contains(&f.kind));
        }
        Ok(ContextQueryResult {
            truncated: result.truncated,
            dropped_estimate: result.dropped_estimate,
            frames,
        })
    }

    /// Revalidate held identities through the same plane registry that served
    /// them (`docs/context-reuse.md` §4). Every store-minted frame declares
    /// `sha256:<content_hash>`, so the plane compares digests against the live
    /// rows and the host reuses only what verifies `valid` — no frame body
    /// travels in either direction.
    async fn verify(&self, request: &VerifyRequest) -> Result<VerifyResponse, HostError> {
        self.plane
            .verify_all(request)
            .await
            .map_err(|e| HostError::Transport {
                id: "workspace-memory".to_string(),
                message: e.to_string(),
            })
    }
}

/// The code graph behind the CGP provider trait: open → query → shutdown
/// per call, on the blocking pool (SQLite reads are synchronous I/O, #64).
/// An absent index is an empty answer, not an error — a workspace that
/// never ran `stella init` still recalls memories normally.
struct GraphProvider {
    workspace_root: PathBuf,
    info: ProviderInfo,
    caps: Capabilities,
}

#[async_trait]
impl ContextProvider for GraphProvider {
    fn id(&self) -> &str {
        "code-graph"
    }
    fn info(&self) -> &ProviderInfo {
        &self.info
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    async fn query(&self, query: &ContextQuery) -> Result<ContextQueryResult, HostError> {
        let db_path = stella_tools::graph::graph_db_path(&self.workspace_root);
        if !db_path.exists() {
            return Ok(ContextQueryResult {
                frames: Vec::new(),
                truncated: false,
                dropped_estimate: None,
            });
        }
        let root = self.workspace_root.clone();
        let query = query.clone();
        let frames = tokio::task::spawn_blocking(move || {
            let graph = stella_graph::CodeGraph::open(&root, &db_path)?;
            let frames = graph.query(&query);
            graph.shutdown();
            frames
        })
        .await
        .map_err(|e| HostError::Transport {
            id: "code-graph".to_string(),
            message: format!("blocking task failed: {e}"),
        })?
        .map_err(|e| HostError::Transport {
            id: "code-graph".to_string(),
            message: e.to_string(),
        })?;
        Ok(ContextQueryResult {
            frames,
            truncated: false,
            dropped_estimate: None,
        })
    }
}

/// The session host: both in-tree providers registered, ready for
/// [`recall_via_host`]. Built once per session by `SessionMemory`.
pub fn session_host(
    store: Arc<ContextStore>,
    domains: Vec<String>,
    workspace_root: PathBuf,
) -> Host {
    let mut host = Host::with_timeout(std::time::Duration::from_millis(RECALL_TIMEOUT_MS));
    // Both providers advertise the frame kinds they serve. Empty `kinds`
    // passes only kind-UNfiltered queries through `capability_matches` — a
    // caller that ever sets `ContextQuery.kinds` would silently route to
    // zero providers if these stayed empty.
    // The wire strings mirror each provider's `to_frame_kind` mapping (the
    // memory store serves every kind it mints; the graph serves symbols,
    // snippets, and graph frames).
    host.register(Box::new(MemoryProvider {
        plane: memory_plane(store, domains),
        info: local_info("workspace-memory"),
        caps: memory_capabilities(),
    }));
    host.register(Box::new(GraphProvider {
        workspace_root,
        info: local_info("code-graph"),
        caps: graph_capabilities(),
    }));
    host
}

/// What happened when the host was asked to admit an external provider.
///
/// A value rather than an error because refusing a provider is a *normal*,
/// reportable outcome: the session continues on its remaining sources, and the
/// operator is told exactly which contract the refused one failed.
#[derive(Debug, Clone, PartialEq)]
pub enum Admission {
    /// Conformance passed, consent (if any was needed) is on file, and the
    /// provider is registered on the host.
    Registered { id: String },
    /// The entry is present in config but not turned on.
    Disabled { id: String },
    /// The config entry cannot be turned into a transport target.
    Misconfigured { id: String, reason: String },
    /// The conformance suite failed. The provider is **not** registered.
    NonConformant { id: String, failures: String },
    /// The transport could not be established (spawn failed, handshake
    /// refused, endpoint unreachable).
    Unreachable { id: String, error: String },
    /// The provider declares off-machine egress scopes with no recorded
    /// consent.
    ///
    /// The transport is up (scopes are only knowable from the handshake), but
    /// the host's own consent gate refuses every query to it and **the query
    /// payload is never transmitted** (`docs/context-reuse.md` §3.5) — so it
    /// contributes nothing to a turn and no workspace content reaches it. The
    /// admission is reported as a refusal because that is what it is
    /// operationally: a source that will serve no frames until consent exists.
    NeedsEgressConsent { id: String, scopes: Vec<String> },
}

impl Admission {
    /// The provider id this outcome concerns.
    pub fn id(&self) -> &str {
        match self {
            Self::Registered { id }
            | Self::Disabled { id }
            | Self::Misconfigured { id, .. }
            | Self::NonConformant { id, .. }
            | Self::Unreachable { id, .. }
            | Self::NeedsEgressConsent { id, .. } => id,
        }
    }

    /// Whether the provider was admitted to the host.
    pub fn registered(&self) -> bool {
        matches!(self, Self::Registered { .. })
    }

    /// A one-line operator-facing explanation, or `None` when the outcome
    /// needs no explaining (registered, or simply not enabled).
    pub fn refusal(&self) -> Option<String> {
        match self {
            Self::Registered { .. } | Self::Disabled { .. } => None,
            Self::Misconfigured { id, reason } => Some(format!(
                "context provider `{id}` is misconfigured: {reason}"
            )),
            Self::NonConformant { id, failures } => Some(format!(
                "context provider `{id}` refused: it is not CGP-conformant ({failures})"
            )),
            Self::Unreachable { id, error } => {
                Some(format!("context provider `{id}` is unreachable: {error}"))
            }
            Self::NeedsEgressConsent { id, scopes } => Some(format!(
                "context provider `{id}` refused: it sends workspace content off this machine \
                 under scope(s) {} — add them to `context_providers.{id}.egress_consent` to allow it",
                scopes.join(", ")
            )),
        }
    }
}

/// Register every **enabled** external CGP provider from user config onto
/// `host`, gating each on the conformance suite and on egress consent.
///
/// Returns one [`Admission`] per configured entry, in config order, so a
/// caller can report refusals without inspecting the host. One provider's
/// refusal never affects another's — the same isolation the query fan-out
/// gives, applied at admission time.
pub async fn register_external_providers(
    host: &mut Host,
    configured: &ContextProviderSettings,
) -> Vec<Admission> {
    let mut admissions = Vec::with_capacity(configured.len());
    for (id, config) in configured {
        admissions.push(admit_external_provider(host, id, config).await);
    }
    admissions
}

/// Admit one configured provider: validate → conformance-gate → connect →
/// consent-gate.
///
/// The order matters. Conformance runs on its **own** connection, before the
/// session host holds one, so a provider that fails is never registered even
/// transiently — "refuse before it serves a turn" is only true if the refusal
/// happens before registration, not after.
async fn admit_external_provider(
    host: &mut Host,
    id: &str,
    config: &ExternalContextProvider,
) -> Admission {
    if !config.enabled {
        return Admission::Disabled { id: id.to_string() };
    }
    let endpoint = match config.target() {
        Ok(endpoint) => endpoint,
        Err(reason) => {
            return Admission::Misconfigured {
                id: id.to_string(),
                reason,
            };
        }
    };
    if let Some(refusal) = conformance_refusal(id, conformance_target(&endpoint)).await {
        return refusal;
    }
    match connect(host, id, &endpoint).await {
        Ok(()) => {}
        Err(error) => {
            return Admission::Unreachable {
                id: id.to_string(),
                error,
            };
        }
    }
    grant_declared_egress_consent(host, id, config)
}

/// The conformance suite's view of a validated endpoint.
fn conformance_target(endpoint: &ProviderEndpoint) -> ProviderTarget {
    match endpoint {
        ProviderEndpoint::Stdio { program, args } => ProviderTarget::Stdio {
            program: program.clone(),
            args: args.clone(),
        },
        ProviderEndpoint::Http { url } => ProviderTarget::Http { url: url.clone() },
    }
}

/// Run the conformance suite against `target`, returning the refusal to
/// report when it fails and `None` when the provider is clean.
///
/// Split out from [`admit_external_provider`] so the gate is exercisable
/// against an in-process target — a scripted misbehaving provider proves the
/// gate bites without needing a fixture binary on disk.
async fn conformance_refusal(id: &str, target: ProviderTarget) -> Option<Admission> {
    let report = run_conformance(target).await;
    if report.passed() {
        return None;
    }
    Some(Admission::NonConformant {
        id: id.to_string(),
        failures: report
            .failures()
            .map(|check| format!("{}: {}", check.name, check.evidence))
            .collect::<Vec<_>>()
            .join("; "),
    })
}

/// Establish the transport and register the provider on the session host.
async fn connect(host: &mut Host, id: &str, endpoint: &ProviderEndpoint) -> Result<(), String> {
    let result = match endpoint {
        ProviderEndpoint::Stdio { program, args } => host.add_stdio(id, program, args).await,
        ProviderEndpoint::Http { url } => host.add_http(id, url.clone()).await,
    };
    result.map_err(|error| error.to_string())
}

/// Record the operator's declared consent, then re-evaluate the host's own
/// gate — and refuse the provider if anything it declares is still uncovered.
///
/// Consent is matched against the scopes the provider declared **at handshake
/// time**, not the ones config guessed at: a provider that quietly widened its
/// egress since consent was granted is refused rather than grandfathered.
fn grant_declared_egress_consent(
    host: &mut Host,
    id: &str,
    config: &ExternalContextProvider,
) -> Admission {
    let Some(info) = host.provider(id).map(|p| p.info().clone()) else {
        return Admission::Unreachable {
            id: id.to_string(),
            error: "provider vanished immediately after registration".to_string(),
        };
    };
    let grantor = match &config.consent_grantor {
        Some(who) => Grantor::Human(who.clone()),
        None => Grantor::Human("local-operator".to_string()),
    };
    let now = now_rfc3339();
    for scope in &config.egress_consent {
        let scope = EgressScope::from_wire(scope.as_str());
        host.record_receipt(ConsentReceipt::new(
            id,
            &info,
            scope,
            grantor.clone(),
            now.clone(),
        ));
    }
    // The legacy boolean contract: a provider declaring `egress` with NO
    // scopes is gated on a plain consent record, which any declared consent
    // satisfies. With no consent declared at all it stays gated below.
    if info.data_flow.egress
        && info.data_flow.egress_scopes.is_empty()
        && !config.egress_consent.is_empty()
    {
        host.record_consent(ConsentRecord::new(
            id,
            info.data_flow.clone(),
            config.egress_consent.join(", "),
        ));
    }

    match host.consent().evaluate(id, &info) {
        ConsentDecision::Permitted => Admission::Registered { id: id.to_string() },
        ConsentDecision::NeedsConsent => Admission::NeedsEgressConsent {
            id: id.to_string(),
            scopes: vec!["egress".to_string()],
        },
        ConsentDecision::NeedsReceipts(scopes) => Admission::NeedsEgressConsent {
            id: id.to_string(),
            scopes: scopes.iter().map(|s| s.as_str().to_string()).collect(),
        },
    }
}

/// The `workspace-memory` capability declaration, shared by `session_host` and
/// the conformance tests so the suite audits what actually ships.
fn memory_capabilities() -> Capabilities {
    Capabilities {
        query: contextgraph_types::capability::QueryCapability {
            kinds: ["memory", "episode", "fact", "snippet", "symbol", "doc"]
                .map(String::from)
                .to_vec(),
        },
        // `docs/context-reuse.md` §4: the plane compares each presented digest
        // against the live node's `content_hash`, so held memory frames are
        // revalidated instead of re-queried — and an unchanged set keeps
        // rendering byte-identically (§1).
        verify: true,
        ..Capabilities::default()
    }
}

/// The `code-graph` capability declaration.
///
/// Deliberately **without** `verify`. A graph frame's digest covers the
/// *rendered* frame body (a quoted snippet, an import neighborhood), not a file
/// the index already hashes, so answering `valid`/`stale` honestly would mean
/// re-deriving the frame — which is the re-query the host performs anyway. Per
/// §4 V3 a provider that does not advertise `verify` has its frames re-queried,
/// which is the correct, conformant degradation. Advertising it without an
/// honest answer would be worse than not advertising it at all: the suite's
/// `verify-honesty` check fails a provider that can never vouch for anything.
fn graph_capabilities() -> Capabilities {
    Capabilities {
        graph: true,
        query: contextgraph_types::capability::QueryCapability {
            kinds: ["symbol", "snippet", "graph"].map(String::from).to_vec(),
        },
        ..Capabilities::default()
    }
}

/// A frame paired with the CGP provider leg that returned it. Provider
/// identity is host-owned metadata rather than frame content, so flattening a
/// fan-out to bare frames would irreversibly misattribute graph hits as
/// workspace memory at the pipeline/event boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributedContextFrame {
    pub provider: String,
    pub frame: ContextFrame,
}

/// The identity triple that names a frame's exact bytes
/// (`docs/context-reuse.md` §1) — the sort key for canonical composition and
/// the payload of a `context/verify` request.
impl AttributedContextFrame {
    /// This frame's stable `(provider id, frame id, content digest)` identity.
    pub fn identity(&self) -> FrameId {
        self.frame.identity(&self.provider)
    }
}

/// One recall through the host: the frames that won selection, plus the CGP
/// usage report for the request that produced them.
pub struct HostRecall {
    /// Selected frames, in canonical render order (`docs/context-reuse.md` §1).
    pub frames: Vec<AttributedContextFrame>,
    /// The per-request cost roll-up (`docs/context-reuse.md` §2).
    pub usage: ContextUsage,
}

/// Fan `query` out through the host, **select** by score, and **render** in
/// canonical order.
///
/// The split is the whole point of `docs/context-reuse.md` §1. Selection is
/// query-dependent: the highest-scoring frames win the frame and token budget,
/// deduped by identity and re-capped across providers (each provider already
/// respected the budget individually; the merge must too). Rendering is not:
/// the surviving set is returned in the protocol's canonical `FrameId` order —
/// `(provider id, frame id, content digest)` — so a turn whose *underlying
/// frames did not change* emits byte-identical prompt text even when retrieval
/// re-ranked them, and the provider's prompt-cache prefix survives (D2/D3).
/// Score is what got a frame in; it must never decide where it renders.
///
/// Every downstream prompt builder consumes this order — the recall block, the
/// planner prompt, the witness prompt — so ordering here is the single seam
/// that makes them all cache-stable.
///
/// Failed, timed-out, or budget-lying providers contribute nothing — their
/// isolation is the point of routing through the host.
///
/// It also returns the fan-out's **usage report** — the per-request roll-up of
/// which providers served how many frames, at what token cost, against which
/// budget (`docs/context-reuse.md` §2).
///
/// The report is taken from the fan-out **before** fusion, so it accounts for
/// what each provider actually served and what the host rejected — a budget
/// liar's dropped frames, a consent-gated or failed leg — not merely what
/// survived the merge into the prompt. That distinction is what makes it an
/// accounting record rather than a debug counter: a provider that is expensive
/// but always loses fusion is invisible in the frame mix and perfectly visible
/// here. `as_of` is the accounting-event time this host stamps, never the
/// query's bi-temporal retrieval pin — two different clocks (§2).
pub async fn recall_via_host(host: &Host, query: &ContextQuery) -> HostRecall {
    let fanout = host.query_all(query).await;
    let usage = to_context_usage(&fanout.usage_report(query, now_rfc3339()));
    let mut frames: Vec<AttributedContextFrame> = Vec::new();
    for outcome in fanout.outcomes {
        if let ProviderResult::Frames(result) = outcome.result {
            frames.extend(
                result
                    .frames
                    .into_iter()
                    .map(|frame| AttributedContextFrame {
                        provider: outcome.provider_id.clone(),
                        frame,
                    }),
            );
        }
    }
    frames.sort_by(|a, b| {
        b.frame
            .score
            .partial_cmp(&a.frame.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut seen = std::collections::HashSet::new();
    let mut kept: Vec<AttributedContextFrame> = Vec::new();
    let mut spent_tokens: u32 = 0;
    for frame in frames {
        if kept.len() >= query.max_frames as usize {
            break;
        }
        if spent_tokens.saturating_add(frame.frame.token_cost) > query.max_tokens {
            continue;
        }
        if !seen.insert((frame.provider.clone(), frame.frame.id.clone())) {
            continue;
        }
        spent_tokens += frame.frame.token_cost;
        kept.push(frame);
    }
    // Selection is finished; how the survivors *render* must now be free of
    // score and cost (§1 D2/D3). `canonical_order` puts the identities in the
    // protocol's total order, and the frames follow their identity — so an
    // unchanged frame set emits identical bytes however this turn ranked it.
    let mut ids: Vec<FrameId> = kept.iter().map(AttributedContextFrame::identity).collect();
    canonical_order(&mut ids);
    kept.sort_by_cached_key(|frame| ids.binary_search(&frame.identity()).unwrap_or(usize::MAX));
    HostRecall {
        frames: kept,
        usage,
    }
}

/// Project the CGP [`UsageReport`] onto the telemetry envelope
/// (`stella_protocol::ContextUsage`).
///
/// Deliberately **lossy in one direction only**: the spec's `served_frames`
/// drill-down is dropped here because the sibling `frames` field of the same
/// `ContextRecall` event already records frame-granular identities locally.
/// What survives is counts, costs, and a timestamp — no titles, no bodies, no
/// query text, so the event stays content-free (AGENTS.md invariant 3, #466).
fn to_context_usage(report: &UsageReport) -> ContextUsage {
    ContextUsage {
        budget_requested: report.budget_requested,
        budget_consumed: report.budget_consumed,
        as_of: report.as_of.clone(),
        providers: report
            .providers
            .iter()
            .map(|provider| ContextProviderUsage {
                provider_id: provider.provider_id.clone(),
                frames_served: provider.frames_served,
                frames_rejected: provider.frames_rejected,
                token_cost: provider.token_cost,
            })
            .collect(),
    }
}

/// The accounting-event timestamp stamped on a usage report (RFC 3339 UTC),
/// via the context plane's dependency-free formatter. A clock before the epoch
/// renders as the epoch rather than panicking — a report is an accounting
/// record, and no timestamp is worth aborting a turn over.
fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    stella_context::format_rfc3339(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(id: &str, score: f32, token_cost: u32) -> ContextFrame {
        ContextFrame {
            id: id.to_string(),
            kind: contextgraph_types::FrameKind::Memory,
            title: id.to_string(),
            content: Some(format!("content of {id}")),
            uri: None,
            score,
            token_cost,
            content_digest: None,
            representation: contextgraph_types::Representation::Full,
            content_fidelity: None,
            canonical_content_hash: None,
            content_ref: None,
            transform: None,
            minimum_content_fidelity: None,
            inline_content_requirement: None,
            canonical_token_cost: None,
            tokenizer_ref: None,

            valid_from: None,
            valid_to: None,
            recorded_at: None,
            provenance: vec![],
            citation_label: Some(format!("[{id}]")),
            embedding: None,
            relations: vec![],
        }
    }

    /// A scripted provider for merge tests.
    struct Scripted {
        id: &'static str,
        frames: Vec<ContextFrame>,
        info: ProviderInfo,
        caps: Capabilities,
    }

    #[async_trait]
    impl ContextProvider for Scripted {
        fn id(&self) -> &str {
            self.id
        }
        fn info(&self) -> &ProviderInfo {
            &self.info
        }
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        async fn query(&self, _q: &ContextQuery) -> Result<ContextQueryResult, HostError> {
            Ok(ContextQueryResult {
                frames: self.frames.clone(),
                truncated: false,
                dropped_estimate: None,
            })
        }
    }

    fn scripted(id: &'static str, frames: Vec<ContextFrame>) -> Box<Scripted> {
        Box::new(Scripted {
            id,
            frames,
            info: local_info(id),
            caps: Capabilities::default(),
        })
    }

    fn query(max_frames: u32, max_tokens: u32) -> ContextQuery {
        ContextQuery {
            goal: "the goal".to_string(),
            query_text: Some("the goal".to_string()),
            embedding: None,
            kinds: vec![],
            anchors: vec![],
            max_frames,
            max_tokens,
            as_of: None,
            representation_preferences: vec![],
        }
    }

    #[tokio::test]
    async fn merges_providers_and_dedupes_only_within_a_provider() {
        let mut host = Host::new();
        host.register(scripted(
            "a",
            vec![frame("low", 0.2, 10), frame("shared", 0.5, 10)],
        ));
        host.register(scripted(
            "b",
            vec![frame("high", 0.9, 10), frame("shared", 0.5, 10)],
        ));
        let kept = recall_via_host(&host, &query(10, 1_000)).await.frames;
        // Canonical order (§1 D2): by provider id, then frame id — NOT by
        // score, which is query-dependent and must not move rendered bytes.
        let rendered: Vec<(&str, &str)> = kept
            .iter()
            .map(|f| (f.provider.as_str(), f.frame.id.as_str()))
            .collect();
        assert_eq!(
            rendered,
            vec![
                ("a", "low"),
                ("a", "shared"),
                ("b", "high"),
                ("b", "shared")
            ],
            "frames render in canonical identity order"
        );
        let mut shared_providers: Vec<&str> = kept
            .iter()
            .filter(|f| f.frame.id == "shared")
            .map(|f| f.provider.as_str())
            .collect();
        shared_providers.sort_unstable();
        assert_eq!(
            shared_providers,
            vec!["a", "b"],
            "provider-local ids must not collide across host legs"
        );
        assert_eq!(
            kept.iter()
                .find(|f| f.frame.id == "high")
                .map(|f| f.provider.as_str()),
            Some("b"),
            "each frame keeps its own leg"
        );
    }

    /// WITNESS (#451, §1 D2/D3): selection stays score-driven, rendering does
    /// not. Two turns that surface the same frames with re-ranked scores — what
    /// vector search does on every re-query — must produce byte-identical
    /// prompt text, or the provider's cached prefix is forfeited every turn.
    #[tokio::test]
    async fn a_rerank_of_an_unchanged_frame_set_renders_byte_identically() {
        let render = |kept: &[AttributedContextFrame]| -> String {
            kept.iter()
                .map(|f| {
                    format!(
                        "{}|{}|{}\n",
                        f.provider,
                        f.frame.id,
                        f.frame.content.as_deref().unwrap_or("")
                    )
                })
                .collect()
        };

        let mut first = Host::new();
        first.register(scripted(
            "mem",
            vec![frame("alpha", 0.9, 10), frame("beta", 0.1, 10)],
        ));
        first.register(scripted("graph", vec![frame("gamma", 0.5, 10)]));
        let turn_one = render(&recall_via_host(&first, &query(10, 1_000)).await.frames);

        // Same three frames, same content, completely inverted relevance —
        // and the graph leg registered first this time, so arrival order
        // differs too.
        let mut second = Host::new();
        second.register(scripted("graph", vec![frame("gamma", 0.95, 10)]));
        second.register(scripted(
            "mem",
            vec![frame("beta", 0.99, 10), frame("alpha", 0.02, 10)],
        ));
        let turn_two = render(&recall_via_host(&second, &query(10, 1_000)).await.frames);

        assert_eq!(
            turn_one, turn_two,
            "an unchanged frame set must render byte-identically across turns (§1 D2/D3)"
        );
    }

    /// WITNESS (#452, §2): the fan-out's usage report is surfaced — per
    /// provider, how many frames it served, how many the host rejected, and
    /// what that cost — against the query's budget, and it re-sums.
    #[tokio::test]
    async fn recall_reports_per_provider_frame_counts_and_token_costs() {
        let mut host = Host::new();
        host.register(scripted(
            "mem",
            vec![frame("m1", 0.9, 4), frame("m2", 0.5, 4)],
        ));
        host.register(scripted("graph", vec![frame("g1", 0.7, 4)]));

        let recall = recall_via_host(&host, &query(10, 1_000)).await;
        let usage = &recall.usage;
        assert!(
            usage.is_consistent(),
            "the report must re-sum from the frames it itemizes: {usage:?}"
        );
        assert_eq!(usage.budget_requested, 1_000, "the query's max_tokens");
        assert_eq!(usage.total_frames_served(), 3);
        assert!(!usage.as_of.is_empty(), "an accounting snapshot is stamped");

        let mem = usage
            .providers
            .iter()
            .find(|p| p.provider_id == "mem")
            .expect("every provider the query reached is itemized");
        assert_eq!(mem.frames_served, 2);
        assert_eq!(mem.frames_rejected, 0);
        assert_eq!(mem.token_cost, 8);
        assert_eq!(usage.budget_consumed, 12, "8 from mem + 4 from graph");
    }

    /// WITNESS (#452, §2): the report is taken from the fan-out **before**
    /// fusion, so a provider whose frames the host threw out for lying about
    /// cost is accounted as `frames_rejected` rather than vanishing. A report
    /// built from the surviving prompt frames could never show this.
    #[tokio::test]
    async fn a_budget_lying_provider_is_reported_as_rejected_not_forgotten() {
        let mut host = Host::new();
        host.register(scripted("honest", vec![frame("h1", 0.5, 4)]));
        // Declares far more than the query's whole budget: the host drops the
        // leg wholesale, so it contributes nothing to the prompt.
        host.register(scripted("liar", vec![frame("l1", 0.9, 9_999)]));

        let recall = recall_via_host(&host, &query(10, 1_000)).await;
        assert!(
            recall.frames.iter().all(|f| f.provider != "liar"),
            "a budget liar's frames must never reach the prompt"
        );
        let liar = recall
            .usage
            .providers
            .iter()
            .find(|p| p.provider_id == "liar")
            .expect("a rejected provider is still itemized");
        assert_eq!(liar.frames_served, 0);
        assert_eq!(liar.frames_rejected, 1, "the drop is counted, not lost");
        assert_eq!(liar.token_cost, 0, "a rejected frame contributes no cost");
        assert!(recall.usage.is_consistent());
    }

    /// WITNESS (#451, §1): selection is still by score — the highest-scoring
    /// frames win a tight budget, even though they render canonically.
    /// Guards the fix against the trivial "sort canonically before selecting"
    /// mistake, which would silently make recall pick alphabetically.
    #[tokio::test]
    async fn selection_still_prefers_the_highest_scoring_frames() {
        let mut host = Host::new();
        // `zzz` scores highest but sorts LAST canonically; `aaa` sorts first
        // but is the least relevant. One leg each, so both are individually
        // budget-honest and only the merge has to choose.
        host.register(scripted("a", vec![frame("aaa", 0.1, 600)]));
        host.register(scripted("z", vec![frame("zzz", 0.9, 600)]));
        let kept = recall_via_host(&host, &query(10, 1_000)).await.frames;
        assert_eq!(kept.len(), 1, "only one frame fits the budget");
        assert_eq!(
            kept[0].frame.id, "zzz",
            "the budget must be spent on the most relevant frame, not the alphabetically first"
        );
    }

    #[tokio::test]
    async fn merge_respects_the_query_budget_across_providers() {
        let mut host = Host::new();
        host.register(scripted("a", vec![frame("a1", 0.9, 600)]));
        host.register(scripted("b", vec![frame("b1", 0.8, 600)]));
        // Each provider individually fits 1000 tokens; the merged set must
        // not exceed it either.
        let kept = recall_via_host(&host, &query(10, 1_000)).await.frames;
        assert_eq!(kept.len(), 1, "second frame would blow the merged budget");
        assert_eq!(kept[0].frame.id, "a1");
        assert_eq!(kept[0].provider, "a");
    }

    #[tokio::test]
    async fn an_absent_graph_index_yields_empty_frames_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = GraphProvider {
            workspace_root: dir.path().to_path_buf(),
            info: local_info("code-graph"),
            caps: Capabilities::default(),
        };
        let result = provider.query(&query(5, 500)).await.expect("empty ok");
        assert!(result.frames.is_empty());
    }

    #[tokio::test]
    async fn the_session_host_registers_both_in_tree_providers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ContextStore::open(dir.path().join("context.db")).expect("store");
        let host = session_host(Arc::new(store), vec![], dir.path().to_path_buf());
        let mut ids = host.provider_ids();
        ids.sort();
        assert_eq!(ids, vec!["code-graph", "workspace-memory"]);
    }

    use stella_context::{ContextDelta, NodeInput, NodeKind};

    /// A store with one strongly-matching node, for plane-routing tests.
    async fn seeded_store(dir: &tempfile::TempDir) -> Arc<ContextStore> {
        let store = Arc::new(ContextStore::open(dir.path().join("context.db")).expect("store"));
        store
            .upsert(
                ContextDelta::new().with_node(
                    NodeInput::new(NodeKind::File, "src/store.rs")
                        .with_uri("file:///repo/src/store.rs")
                        .with_content("open the sqlite connection in wal mode"),
                ),
            )
            .await
            .expect("seed");
        store
    }

    #[tokio::test]
    async fn recall_routes_through_the_plane_registry_to_the_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = seeded_store(&dir).await;
        let host = session_host(store, vec![], dir.path().to_path_buf());
        let mut q = query(5, 4_000);
        q.query_text = Some("open the sqlite connection in wal mode".to_string());
        // Host → workspace-memory → plane registry → store: the full
        // production path, end to end.
        let kept = recall_via_host(&host, &q).await.frames;
        assert!(
            kept.iter()
                .any(|f| f.frame.content.as_deref().unwrap_or("").contains("sqlite")),
            "the seeded node surfaces through the registry-routed path"
        );
    }

    /// A scripted context-plane provider (the `stella-context` seam, not the
    /// host trait) for plane fan-out tests.
    struct PlaneScripted {
        kinds: Vec<String>,
        frames: Vec<ContextFrame>,
        truncated: bool,
    }

    #[async_trait]
    impl PlaneProvider for PlaneScripted {
        fn info(&self) -> ProviderInfo {
            local_info("plane-scripted")
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                query: contextgraph_types::capability::QueryCapability {
                    kinds: self.kinds.clone(),
                },
                ..Capabilities::default()
            }
        }
        async fn query(&self, _q: &ContextQuery) -> Result<ContextQueryResult, ContextError> {
            Ok(ContextQueryResult {
                frames: self.frames.clone(),
                truncated: self.truncated,
                dropped_estimate: None,
            })
        }
    }

    #[tokio::test]
    async fn the_plane_fans_out_and_kind_routes_across_registered_providers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut plane = memory_plane(seeded_store(&dir).await, vec![]);
        let mut graph_frame = frame("plane-graph", 0.9, 10);
        graph_frame.kind = contextgraph_types::FrameKind::Graph;
        plane.register(Arc::new(PlaneScripted {
            kinds: vec!["graph".to_string()],
            frames: vec![graph_frame],
            truncated: true,
        }));
        let provider = MemoryProvider {
            plane,
            info: local_info("workspace-memory"),
            caps: Capabilities::default(),
        };

        // Unfiltered: both plane providers answer — the store's frame and the
        // second provider's graph frame merge, and the second provider's
        // truncation survives the fan-out instead of being erased (L-C5).
        let mut q = query(10, 4_000);
        q.query_text = Some("open the sqlite connection in wal mode".to_string());
        let result = provider.query(&q).await.expect("fan-out");
        assert!(result.frames.iter().any(|f| f.id == "plane-graph"));
        assert!(
            result
                .frames
                .iter()
                .any(|f| f.content.as_deref().unwrap_or("").contains("sqlite"))
        );
        assert!(result.truncated, "a plane provider's drop report survives");

        // Kind-filtered to `graph`: the registry routes the store away (it
        // never serves graph frames) and only the second provider answers.
        let mut q = query(10, 4_000);
        q.kinds = vec![contextgraph_types::FrameKind::Graph];
        let result = provider.query(&q).await.expect("kind routing");
        let ids: Vec<&str> = result.frames.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["plane-graph"]);
    }

    // CGP conformance
    //
    // The providers this host registers are green on the protocol's own
    // conformance suite (§3.6), verified in-tree — not merely asserted to be
    // by documentation. Both are exercised through `run_conformance` exactly
    // as `session_host` constructs them, so a regression that made a shipped
    // provider lie about cost, drop a citation label, or fail to shut down
    // cleanly turns this suite red.

    use contextgraph_conformance::{
        CHECK_FRAME_VALIDITY, CHECK_VERIFY_HONESTY, CheckStatus, ConformanceReport, ProviderTarget,
        run_conformance, sample_query,
    };

    /// Render a report's failures for a panic message, so a red run names the
    /// exact contract that broke rather than just "not conformant".
    fn conformance_failures(report: &ConformanceReport) -> String {
        report
            .failures()
            .map(|check| format!("{}: {}", check.name, check.evidence))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// The status of one named check, for non-vacuity assertions.
    fn check_status(report: &ConformanceReport, name: &str) -> CheckStatus {
        report
            .checks
            .iter()
            .find(|check| check.name == name)
            .unwrap_or_else(|| panic!("report has no `{name}` check"))
            .status
    }

    /// A store seeded so the **conformance suite's own probe query** retrieves
    /// something.
    ///
    /// This is what turns the gate from decorative into real. The suite probes
    /// with `sample_query()` ("conformance probe"), and zero frames is a
    /// *permitted* answer — so a store seeded only with unrelated content made
    /// `frame-validity` pass on an empty set and skipped `verify-honesty`
    /// entirely. Every check that inspects a frame was inspecting nothing.
    async fn probe_seeded_store(dir: &tempfile::TempDir) -> Arc<ContextStore> {
        let store = seeded_store(dir).await;
        let probe = sample_query();
        let text = probe.query_text.clone().unwrap_or(probe.goal);
        store
            .upsert(
                ContextDelta::new().with_node(
                    NodeInput::new(NodeKind::Memory, "cgp conformance probe fixture")
                        .with_uri("file:///repo/.stella/memories/cgp-probe.md")
                        // Derived from the suite's own query text, so a pin
                        // bump that reworded the probe cannot silently return
                        // this gate to a vacuous pass.
                        .with_content(format!(
                            "{text}: this memory exists so the CGP conformance suite has a real \
                             frame to validate and revalidate."
                        )),
                ),
            )
            .await
            .expect("seed probe fixture");
        store
    }

    #[tokio::test]
    async fn workspace_memory_provider_is_cgp_conformant() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = probe_seeded_store(&dir).await;
        // Constructed exactly as `session_host` builds the shipped
        // `workspace-memory` provider: the store behind the plane registry,
        // advertising the kinds and the `verify` capability it ships with.
        let provider = MemoryProvider {
            plane: memory_plane(store, vec![]),
            info: local_info("workspace-memory"),
            caps: memory_capabilities(),
        };
        let report = run_conformance(ProviderTarget::InProcess(Box::new(provider))).await;
        assert!(
            report.passed(),
            "workspace-memory is not CGP-conformant: {}",
            conformance_failures(&report)
        );

        // WITNESS (#451): the gate has teeth. Before the probe-matching seed,
        // the suite surfaced 0 frames and every frame-level check was a
        // free pass — `frame-validity` on an empty set, `verify-honesty`
        // skipped. Asserting the *evidence* is what pins that shut.
        let validity = report
            .checks
            .iter()
            .find(|check| check.name == CHECK_FRAME_VALIDITY)
            .expect("frame-validity check present");
        assert!(
            !validity.evidence.contains("0 frames"),
            "the conformance probe surfaced no frames — the gate is passing vacuously: {}",
            validity.evidence
        );
        assert_eq!(
            check_status(&report, CHECK_VERIFY_HONESTY),
            CheckStatus::Pass,
            "verify-honesty must actually run (not skip): the shipped provider advertises \
             `verify` and its frames must carry digests it can vouch for"
        );
    }

    /// WITNESS (#451, §4 V1/V2): `context/verify` is implemented, not just
    /// advertised. A frame the plane just served verifies `valid`; the same
    /// identity with a digest the store never minted must NOT be retained.
    #[tokio::test]
    async fn workspace_memory_verifies_its_own_frames_and_refuses_a_foreign_digest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = probe_seeded_store(&dir).await;
        let host = session_host(store, vec![], dir.path().to_path_buf());

        let mut q = query(5, 4_000);
        q.query_text = Some("conformance probe".to_string());
        let kept = recall_via_host(&host, &q).await.frames;
        let held: Vec<FrameId> = kept
            .iter()
            .filter(|f| f.provider == "workspace-memory")
            .map(AttributedContextFrame::identity)
            .collect();
        assert!(
            !held.is_empty(),
            "the probe fixture must surface through the shipped recall path"
        );
        assert!(
            held.iter().all(FrameId::is_verifiable),
            "every store-minted frame must declare a content_digest (§1 D4)"
        );

        let unchanged = host.verify_frames(&held).await;
        assert_eq!(
            unchanged.retained.len(),
            held.len(),
            "unchanged frames must verify `valid`, not be re-queried: {:?}",
            unchanged.dropped
        );

        // A digest the store never served: default-deny must evict it.
        let forged: Vec<FrameId> = held
            .iter()
            .map(|id| {
                FrameId::new(
                    &id.provider_id,
                    &id.frame_id,
                    Some(format!("sha256:{}", "0".repeat(64))),
                )
            })
            .collect();
        let changed = host.verify_frames(&forged).await;
        assert!(
            changed.retained.is_empty(),
            "a digest the provider never minted must never verify `valid` (§4 V1)"
        );
        assert_eq!(changed.dropped.len(), forged.len(), "every forgery evicted");
    }

    #[tokio::test]
    async fn code_graph_provider_is_cgp_conformant() {
        let dir = tempfile::tempdir().expect("tempdir");
        // The `code-graph` provider as `session_host` registers it. With no
        // index on disk it returns zero frames — permitted by §3.4 — so this
        // pins handshake, budget honesty, and clean shutdown for the graph leg.
        let provider = GraphProvider {
            workspace_root: dir.path().to_path_buf(),
            info: local_info("code-graph"),
            caps: Capabilities {
                graph: true,
                query: contextgraph_types::capability::QueryCapability {
                    kinds: ["symbol", "snippet", "graph"].map(String::from).to_vec(),
                },
                ..Capabilities::default()
            },
        };
        let report = run_conformance(ProviderTarget::InProcess(Box::new(provider))).await;
        assert!(
            report.passed(),
            "code-graph is not CGP-conformant: {}",
            conformance_failures(&report)
        );
    }

    #[tokio::test]
    async fn conformance_gate_catches_a_frame_without_a_citation_label() {
        // Teeth: a provider that returns a bare-id frame (§3.4 — "NEVER a
        // bare uuid") must FAIL frame-validity. This proves the two passing
        // tests above are a real gate that would catch a regression, not a
        // suite that waves everything through.
        let mut bare_id = frame("bare-id", 0.9, 8);
        bare_id.citation_label = None;
        let report =
            run_conformance(ProviderTarget::InProcess(scripted("bad", vec![bare_id]))).await;
        assert!(
            !report.passed(),
            "a frame with no citation label must not be judged conformant"
        );
        assert!(
            report.checks.iter().any(
                |check| check.name == CHECK_FRAME_VALIDITY && check.status == CheckStatus::Fail
            ),
            "the missing citation label must surface as a frame-validity failure"
        );
    }

    // External-provider seam (#453)
    //
    // The admission gate is exercised against IN-PROCESS targets: the gate's
    // contract — refuse a non-conformant provider before it can serve a turn —
    // is transport-independent, and an in-process target proves it without a
    // fixture binary on disk. The transport wiring itself is `Host::add_stdio`
    // / `add_http`, already covered by the protocol's own suite at the pin.

    use crate::settings::ExternalContextProvider;
    use crate::settings::context_providers::ContextTransport;

    fn stdio_config(command: &str) -> ExternalContextProvider {
        ExternalContextProvider {
            transport: ContextTransport::Stdio,
            command: Some(command.to_string()),
            enabled: true,
            ..Default::default()
        }
    }

    /// WITNESS (#453): the conformance suite is an ADMISSION gate. A provider
    /// that serves a frame with no citation label — a real §3.4 violation —
    /// must be refused, and must not end up registered on the session host.
    #[tokio::test]
    async fn a_non_conformant_provider_is_refused_before_it_can_serve_a_turn() {
        let mut bare_id = frame("bare-id", 0.9, 8);
        bare_id.citation_label = None;
        let target = ProviderTarget::InProcess(scripted("rogue", vec![bare_id]));

        let refusal = conformance_refusal("rogue", target)
            .await
            .expect("a frame with no citation label must not be admitted");
        match &refusal {
            Admission::NonConformant { id, failures } => {
                assert_eq!(id, "rogue");
                assert!(
                    failures.contains(CHECK_FRAME_VALIDITY),
                    "the refusal must name the contract that broke: {failures}"
                );
            }
            other => panic!("expected a conformance refusal, got {other:?}"),
        }
        assert!(!refusal.registered());
        assert!(refusal.refusal().is_some(), "a refusal is explained");
    }

    /// The gate passes a genuinely conformant provider — otherwise "refused"
    /// would be indistinguishable from "the gate rejects everything".
    #[tokio::test]
    async fn a_conformant_provider_clears_the_admission_gate() {
        let target = ProviderTarget::InProcess(scripted("good", vec![frame("f1", 0.5, 4)]));
        assert!(
            conformance_refusal("good", target).await.is_none(),
            "a conformant provider must not be refused"
        );
    }

    /// WITNESS (#453): a declared-but-disabled provider is never spawned, never
    /// conformance-probed, and never registered. `enabled` defaults false, so
    /// merely appearing in config — including a merged-in org scope — cannot
    /// put a provider in the recall path.
    #[tokio::test]
    async fn a_disabled_provider_is_never_reached() {
        let mut host = Host::new();
        let mut configured = ContextProviderSettings::new();
        configured.insert("off".to_string(), {
            let mut config = stdio_config("definitely-not-a-real-program-451");
            config.enabled = false;
            config
        });
        let admissions = register_external_providers(&mut host, &configured).await;
        assert_eq!(admissions, vec![Admission::Disabled { id: "off".into() }]);
        assert!(
            host.provider_ids().is_empty(),
            "a disabled entry must not register"
        );
    }

    /// A malformed entry is reported as configuration — naming the missing
    /// field — rather than surfacing later as a process that would not start.
    #[tokio::test]
    async fn a_transport_missing_its_required_field_is_a_config_refusal() {
        let mut host = Host::new();
        let mut configured = ContextProviderSettings::new();
        configured.insert(
            "broken".to_string(),
            ExternalContextProvider {
                transport: ContextTransport::Http,
                enabled: true,
                ..Default::default()
            },
        );
        let admissions = register_external_providers(&mut host, &configured).await;
        match &admissions[0] {
            Admission::Misconfigured { id, reason } => {
                assert_eq!(id, "broken");
                assert!(reason.contains("`url`"), "{reason}");
            }
            other => panic!("expected a config refusal, got {other:?}"),
        }
        assert!(host.provider_ids().is_empty());
    }

    /// An unspawnable program is a per-provider refusal, not a session
    /// failure: the remaining sources keep serving.
    #[tokio::test]
    async fn an_unreachable_provider_is_refused_without_taking_the_session_down() {
        let mut host = Host::new();
        let mut configured = ContextProviderSettings::new();
        configured.insert(
            "ghost".to_string(),
            stdio_config("stella-no-such-cgp-provider-451"),
        );
        let admissions = register_external_providers(&mut host, &configured).await;
        assert!(
            !admissions[0].registered(),
            "a program that cannot be reached must not be admitted: {:?}",
            admissions[0]
        );
        assert_eq!(admissions[0].id(), "ghost");
        assert!(admissions[0].refusal().is_some());
        assert!(host.provider_ids().is_empty());
    }

    /// An egress provider with declared consent for every off-machine scope
    /// it advertises is permitted; the same provider with no consent is not.
    /// This is the path that has never fired, because every built-in is
    /// `egress: false`.
    #[tokio::test]
    async fn egress_consent_gates_a_provider_that_sends_content_off_the_machine() {
        use contextgraph_host::ConsentDecision;

        let info = ProviderInfo {
            name: "acme".to_string(),
            version: "1".to_string(),
            data_flow: DataFlow {
                reads: true,
                writes: false,
                egress: true,
                egress_scopes: vec![EgressScope::ThirdPartyIndex],
            },
        };

        // No consent recorded: the host refuses to transmit, so the query
        // payload — which carries workspace content — never leaves.
        let bare = Host::new();
        assert_eq!(
            bare.consent().evaluate("acme", &info),
            ConsentDecision::NeedsReceipts(vec![EgressScope::ThirdPartyIndex]),
            "an unconsented egress scope must gate before any payload moves"
        );

        // The receipt the admission path writes from config unlocks exactly
        // that scope, and nothing wider.
        let mut consented = Host::new();
        consented.record_receipt(ConsentReceipt::new(
            "acme",
            &info,
            EgressScope::ThirdPartyIndex,
            Grantor::Human("ada@acme.test".into()),
            "2026-07-24T00:00:00Z",
        ));
        assert_eq!(
            consented.consent().evaluate("acme", &info),
            ConsentDecision::Permitted
        );

        // A receipt for a DIFFERENT scope does not unlock this one — consent
        // is per-scope, never a blanket egress switch.
        let mut wrong_scope = Host::new();
        wrong_scope.record_receipt(ConsentReceipt::new(
            "acme",
            &info,
            EgressScope::OrgTenant,
            Grantor::Human("ada@acme.test".into()),
            "2026-07-24T00:00:00Z",
        ));
        assert_eq!(
            wrong_scope.consent().evaluate("acme", &info),
            ConsentDecision::NeedsReceipts(vec![EgressScope::ThirdPartyIndex]),
            "consent granted for one scope must not silently cover another"
        );
    }

    /// Every built-in stays `egress: false`, which is why the consent prompt
    /// has never fired — and must keep not firing.
    #[tokio::test]
    async fn the_in_tree_providers_still_declare_no_egress() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ContextStore::open(dir.path().join("context.db")).expect("store");
        let host = session_host(Arc::new(store), vec![], dir.path().to_path_buf());
        for id in host.provider_ids() {
            let info = host.provider(id).expect("registered").info();
            assert!(
                !info.data_flow.egress,
                "built-in `{id}` must never egress without consent"
            );
        }
    }

    #[test]
    fn pinned_protocol_version_is_the_expected_draft() {
        // Tripwire: our conformance is verified against this exact wire
        // version. If a pin bump moves the protocol version, this fails loudly
        // so conformance is re-audited rather than silently assumed to hold.
        assert_eq!(
            contextgraph_types::PROTOCOL_VERSION,
            "contextgraph/1.0-draft",
            "CGP protocol version changed — re-verify the conformance suite before bumping the pin"
        );
    }
}
