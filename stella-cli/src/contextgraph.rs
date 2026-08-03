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
use clap::ValueEnum;
use contextgraph_conformance::{ProviderTarget, run_conformance};
use contextgraph_host::{
    ConsentDecision, ConsentRecord, ContextProvider, Host, HostError, ProviderResult,
    refuse_insecure_transport,
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

mod suppression;

pub use suppression::SuppressionReader;
#[cfg(test)]
use suppression::no_suppression;

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
    suppression: SuppressionReader,
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
        // Read suppression here, inside the provider, so it lands **before**
        // the budget pass rather than on frames the budget already paid for
        // (#712 deliverable 4). A read failure fails this leg closed: the
        // session continues on its other providers with no workspace memory,
        // which is the same posture the CLI takes when it cannot read the
        // state at all. Surfacing everything is the one outcome that is
        // definitely wrong.
        let excluded = (self.suppression)().map_err(ContextError::InvalidInput)?;
        Ok(self
            .store
            .recall_scoped_excluding(query, &self.domains, &excluded)
            .await?
            .into())
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
fn memory_plane(
    store: Arc<ContextStore>,
    domains: Vec<String>,
    suppression: SuppressionReader,
) -> ProviderRegistry {
    let mut plane = ProviderRegistry::new();
    plane.register(Arc::new(ScopedStore {
        store,
        domains,
        suppression,
    }));
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
    suppression: SuppressionReader,
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
        plane: memory_plane(store, domains, suppression),
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
    /// The provider is configured at a plaintext `http://` URL whose host is not
    /// loopback, so workspace content would cross the network in cleartext. The
    /// host refuses the transport before any bytes leave the machine (**C7**).
    ///
    /// Its own outcome rather than an [`Self::Unreachable`] or a
    /// [`Self::NonConformant`], because the cause is neither: the endpoint is
    /// very likely answering fine, and the provider has done nothing wrong. It is
    /// an operator typo, one scheme character wide, and both other labels would
    /// send someone to debug the wrong thing — a network, or somebody else's
    /// provider.
    ///
    /// Carries the peer host, never a credential: the host keeps credentials out
    /// of this error by contract (**C8**), and Stella sends none anyway.
    InsecureTransport { id: String, host: String },
    /// The provider declares off-machine egress scopes with no recorded
    /// consent.
    ///
    /// The transport is up (scopes are only knowable from the handshake), but
    /// the host's own consent gate refuses every query to it and **the query
    /// payload is never transmitted** (`docs/context-reuse.md` §4) — so it
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
            | Self::InsecureTransport { id, .. }
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
            Self::InsecureTransport { id, host } => Some(format!(
                "context provider `{id}` refused: `{host}` is not on this machine and its URL is \
                 plaintext `http://`, so workspace content would cross the network unencrypted — \
                 use `https://` (CGP C7)"
            )),
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

/// Admit one configured provider: validate → transport-security → conformance-gate
/// → connect → consent-gate.
///
/// The order matters twice over.
///
/// Conformance runs on its **own** connection, before the session host holds one,
/// so a provider that fails is never registered even transiently — "refuse before
/// it serves a turn" is only true if the refusal happens before registration.
///
/// And the transport-security check runs before *conformance*, because the
/// conformance suite is not a passive inspection: it connects and sends sample
/// queries. Probing a plaintext non-loopback endpoint would put those on the wire
/// in cleartext. `contextgraph-host` refuses that connection either way — the
/// suite calls the same `add_http`, so C7 is never actually breached — but then
/// the failure arrives as a failed `handshake` *check*, and the operator is told
/// their provider is non-conformant when what is wrong is one character of their
/// own URL. Checking first buys the honest label.
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
    if let Some(refusal) = transport_security_refusal(id, &endpoint) {
        return refusal;
    }
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

/// Classify an endpoint against the protocol's transport-security rule (**C7**)
/// before anything connects to it, returning the refusal to report or `None` when
/// the endpoint is safe to probe.
///
/// The rule itself is [`refuse_insecure_transport`] — CGP's, called here, not
/// reimplemented here. That is the whole point of asking upstream to export it:
/// "which hosts count as loopback" is a normative decision with edge cases
/// (`[::1]`, all of `127.0.0.0/8` rather than just `127.0.0.1`, the casing of
/// `LOCALHOST`, and the `127.0.0.1.example.com` prefix trap), and a second
/// implementation living in Stella would be a second answer, free to drift from
/// the one the host enforces. A host may consult a protocol rule; it should not
/// keep its own copy.
///
/// A stdio endpoint has no transport to secure — the bytes never touch a network
/// — so it passes untouched.
fn transport_security_refusal(id: &str, endpoint: &ProviderEndpoint) -> Option<Admission> {
    let ProviderEndpoint::Http { url } = endpoint else {
        return None;
    };
    match refuse_insecure_transport(id, url) {
        Ok(()) => None,
        Err(HostError::InsecureTransport { host, .. }) => Some(Admission::InsecureTransport {
            id: id.to_string(),
            host,
        }),
        // The rule also rejects a URL it cannot parse. That is a config error,
        // not a security verdict: telling an operator to add TLS to a string
        // that is not a URL would point them at the wrong line. Previously such
        // a URL passed `target()` (which only checks non-emptiness) and failed
        // later as `Unreachable`, which was just as misleading.
        Err(error) => Some(Admission::Misconfigured {
            id: id.to_string(),
            reason: error.to_string(),
        }),
    }
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
///
/// The HTTP leg passes **no credential** (C8). Stella has never had a way to
/// declare one in `context_providers`, so there is no bearer token for the host
/// to attach — and therefore none it could leak into a log or an error. If
/// credentialed providers are ever configurable, the token belongs in
/// `contextgraph_host::http::Credential`, which the host is required to keep out
/// of its own diagnostics, not in a URL Stella prints in an [`Admission`].
async fn connect(host: &mut Host, id: &str, endpoint: &ProviderEndpoint) -> Result<(), String> {
    let result = match endpoint {
        ProviderEndpoint::Stdio { program, args } => host.add_stdio(id, program, args).await,
        ProviderEndpoint::Http { url } => host.add_http(id, url.clone(), None).await,
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
/// V3 a provider that does not advertise `verify` has its frames re-queried,
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
    /// What the host's own merge dropped, and why — Phase 2 (#713).
    ///
    /// This re-pack is a **second** budget pass, downstream of the context
    /// plane's: each provider already respected the budget individually, so
    /// the merge across providers has to as well. It had no drop report at
    /// all, which made it a silent truncation (`L-C5` bans exactly this) and
    /// meant a required-item precedence that only landed in `pack_to_budget`
    /// would be undone here without a trace.
    pub dropped: Vec<HostDroppedFrame>,
}

/// A frame the host's cross-provider merge did not admit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDroppedFrame {
    /// The serving provider's routing key.
    pub provider: String,
    /// The provider's frame id.
    pub id: String,
    /// The human citation, so the report reads as names not ids (`L-C4`).
    pub citation_label: String,
    /// What it would have cost.
    pub token_cost: u32,
    /// Which limit dropped it, in the same vocabulary the context plane uses.
    pub reason: stella_context::DropReason,
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

    // Phase 2 (#713): required-item precedence and an honest drop report, both
    // of which this second pack lacked entirely. A frame the context plane
    // marked required (a file the goal named verbatim) survived
    // `pack_to_budget` and was then silently dropped here the moment a
    // higher-scoring frame from any provider filled the count budget first —
    // so the guarantee held in one packer and was undone in the other. The
    // required pass runs first for the same reason it does there.
    let mut seen = std::collections::HashSet::new();
    let mut kept: Vec<AttributedContextFrame> = Vec::new();
    let mut dropped: Vec<HostDroppedFrame> = Vec::new();
    let mut spent_tokens: u32 = 0;
    //
    // The ranked band splits again by `RecallTier` for the same reason: the
    // context plane's packer admits deferred candidates only out of what the
    // normal ones left, and a precedence honored in one packer and ignored in
    // the other is not a precedence. `partition` is stable, so within each of
    // the three bands the score order established above is untouched.
    let (required, ranked): (Vec<_>, Vec<_>) = frames.into_iter().partition(is_required_frame);
    let (ranked, deferred): (Vec<_>, Vec<_>) =
        ranked.into_iter().partition(|f| !is_deferred_frame(f));
    for (frames, count_limited) in [(required, false), (ranked, true), (deferred, true)] {
        for frame in frames {
            if !seen.insert((frame.provider.clone(), frame.frame.id.clone())) {
                continue;
            }
            // `break` became `continue` with a report: the old loop stopped
            // walking the moment the count filled, so everything after it
            // vanished uncounted as well as unkept.
            if count_limited && kept.len() >= query.max_frames as usize {
                dropped.push(host_dropped(&frame, stella_context::DropReason::FrameCount));
                continue;
            }
            if spent_tokens.saturating_add(frame.frame.token_cost) > query.max_tokens {
                dropped.push(host_dropped(
                    &frame,
                    if count_limited {
                        stella_context::DropReason::TokenBudget
                    } else {
                        stella_context::DropReason::RequiredOverBudget
                    },
                ));
                continue;
            }
            spent_tokens += frame.frame.token_cost;
            kept.push(frame);
        }
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
        dropped,
    }
}

/// Whether the context plane marked this frame required, read back from the
/// provenance chain — the only channel that survives the CGP seam, which is
/// why the reason is written there (`stella_context::SELECTION_PROVENANCE_KIND`).
/// A frame from a provider that declares no selection reason is not required,
/// which is the safe default: an external provider cannot claim exemption from
/// the host's budget merely by omitting a field.
fn is_required_frame(frame: &AttributedContextFrame) -> bool {
    frame.frame.provenance.iter().any(|entry| {
        entry.kind == stella_context::SELECTION_PROVENANCE_KIND
            && entry.method.as_deref() == Some(stella_context::SelectionReason::Anchored.as_str())
    })
}

/// Whether the context plane marked this frame as yielding first when the
/// budget binds, read back from the provenance chain
/// (`stella_context::RECALL_TIER_PROVENANCE_KIND`).
///
/// A frame that declares no tier is `Normal`, which is the safe default in the
/// opposite direction from [`is_required_frame`]: there, silence must not let a
/// provider claim exemption from the budget; here, silence must not let the
/// host demote a provider's frame it knows nothing about. Both defaults resolve
/// to "compete on rank like everything else".
fn is_deferred_frame(frame: &AttributedContextFrame) -> bool {
    frame.frame.provenance.iter().any(|entry| {
        entry.kind == stella_context::RECALL_TIER_PROVENANCE_KIND
            && entry.method.as_deref() == Some(stella_context::RecallTier::Deferred.as_str())
    })
}

fn host_dropped(
    frame: &AttributedContextFrame,
    reason: stella_context::DropReason,
) -> HostDroppedFrame {
    HostDroppedFrame {
        provider: frame.provider.clone(),
        id: frame.frame.id.clone(),
        citation_label: frame
            .frame
            .citation_label
            .clone()
            .unwrap_or_else(|| frame.frame.title.clone()),
        token_cost: frame.frame.token_cost,
        reason,
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
///
/// **UR1 still holds through the sibling field**, which is worth stating because
/// dropping a field the spec names looks like a violation and is not one. UR1
/// requires a billed total to be walkable back to the exact `(provider id, frame
/// id, content_digest)` triples behind it. Each `ContextFrameRef` on the same
/// event carries all three: `provider`, `id`, and `content_digest` — the last of
/// which used to be thrown away here and was restored precisely so a reference
/// resolves to a revision rather than a row (#713). `id` and `content_digest`
/// being `Option` is not a hole in the walk: a candidate frame that was never
/// materialized has no id, and a frame whose provider declared no digest is
/// *not verifiable* by §1 and must be re-queried rather than reused — so the
/// absence is the meaningful answer, and CGP's own `FrameId.content_digest` is
/// equally optional for the same reason.
///
/// What is genuinely **not** emitted anywhere is a §14 `AttributionReport`: what
/// each served frame went on to do. That is not a projection loss — there is no
/// source for it at this point in the turn, since `cited` is only observable
/// after the model has answered. See
/// `the_ledger_names_the_same_three_attribution_observations_as_the_protocol` for
/// the vocabulary alignment that is in place and the store migration that
/// frame-keyed attribution is waiting on.
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

/// The seven code-graph queries, mirroring the `graph_query` agent tool's ops
/// one-for-one so a human at the CLI and the model inside a turn see the
/// same frames for the same question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GraphOp {
    /// Where a symbol is defined
    Definitions,
    /// Best-effort textual references to a symbol
    References,
    /// What a definition calls (recorded call sites inside its span)
    Callees,
    /// Call sites naming a symbol (best-effort — matched by name)
    Callers,
    /// What a file imports
    Imports,
    /// Which files import a file
    Importers,
    /// A file's immediate graph neighborhood (symbols + edges)
    Neighbors,
}

impl GraphOp {
    fn as_str(self) -> &'static str {
        match self {
            GraphOp::Definitions => "definitions",
            GraphOp::References => "references",
            GraphOp::Callees => "callees",
            GraphOp::Callers => "callers",
            GraphOp::Imports => "imports",
            GraphOp::Importers => "importers",
            GraphOp::Neighbors => "neighbors",
        }
    }
}

/// `stella graph <op> <target>` — the human door to the same query surface
/// the `graph_query` tool gives the agent. Frames print exactly as the model
/// would receive them.
pub fn run_graph(op: GraphOp, target: &str) -> Result<(), String> {
    let root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    match stella_tools::graph::run_query(&root, op.as_str(), target) {
        stella_protocol::tool::ToolOutput::Ok { content } => {
            println!("{content}");
            Ok(())
        }
        stella_protocol::tool::ToolOutput::Error { message } => Err(message),
    }
}

#[cfg(test)]
mod tests;
