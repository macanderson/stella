//! Context-plane host tests — moved verbatim out of the module's inline
//! `mod tests` to make room for the suppression seam (#712). The
//! assertions are unchanged.

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

/// The same frame, marked by the context plane as yielding first when the
/// budget binds — built through the provenance channel the host actually reads,
/// not by setting a field the seam would drop.
fn deferred_frame(id: &str, score: f32, token_cost: u32) -> ContextFrame {
    let mut f = frame(id, score, token_cost);
    f.provenance.push(contextgraph_types::Provenance {
        kind: stella_context::RECALL_TIER_PROVENANCE_KIND.into(),
        uri: None,
        range: None,
        digest: None,
        method: Some(stella_context::RecallTier::Deferred.as_str().to_string()),
        by: Some("stella-context/test".into()),
    });
    f
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
    assert!(usage.as_of_is_wellformed(), "as_of must be RFC 3339");

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

/// A frame the context plane deferred yields at the *host* budget too.
///
/// The same failure the required-item pass had before #713: a precedence
/// honored by `pack_to_budget` and ignored by the cross-provider merge is a
/// precedence that survives one packer and is undone by the next. The deferred
/// frame scores HIGHER here, so it wins the merge sort outright and only the
/// tier can dislodge it.
#[tokio::test]
async fn a_deferred_frame_yields_the_merge_budget_to_a_normal_one() {
    let mut host = Host::new();
    host.register(scripted("p", vec![deferred_frame("process", 0.9, 600)]));
    host.register(scripted("d", vec![frame("domain", 0.1, 600)]));
    let kept = recall_via_host(&host, &query(10, 1_000)).await.frames;
    assert_eq!(kept.len(), 1, "only one frame fits the budget");
    assert_eq!(
        kept[0].frame.id, "domain",
        "a deferred frame must not take the last slot from a normal one"
    );
}

/// The negative: with room for both, the deferred frame is still served. The
/// tier costs recall nothing until the budget actually binds.
#[tokio::test]
async fn a_deferred_frame_is_served_when_the_merge_budget_has_room() {
    let mut host = Host::new();
    host.register(scripted("p", vec![deferred_frame("process", 0.9, 100)]));
    host.register(scripted("d", vec![frame("domain", 0.1, 100)]));
    let kept = recall_via_host(&host, &query(10, 1_000)).await.frames;
    let ids: Vec<&str> = kept.iter().map(|k| k.frame.id.as_str()).collect();
    assert_eq!(ids.len(), 2, "both frames fit: {ids:?}");
    assert!(ids.contains(&"process"), "{ids:?}");
    assert!(ids.contains(&"domain"), "{ids:?}");
}

/// A provider that declares no tier competes normally — the host must not
/// demote a frame it knows nothing about.
#[tokio::test]
async fn a_frame_declaring_no_tier_competes_normally() {
    let mut host = Host::new();
    host.register(scripted("q", vec![frame("quiet", 0.9, 600)]));
    host.register(scripted("d", vec![deferred_frame("process", 0.8, 600)]));
    let kept = recall_via_host(&host, &query(10, 1_000)).await.frames;
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].frame.id, "quiet");
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
    let host = session_host(
        Arc::new(store),
        vec![],
        dir.path().to_path_buf(),
        no_suppression(),
    );
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
    let host = session_host(store, vec![], dir.path().to_path_buf(), no_suppression());
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
    let mut plane = memory_plane(seeded_store(&dir).await, vec![], no_suppression());
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
        plane: memory_plane(store, vec![], no_suppression()),
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
    let host = session_host(store, vec![], dir.path().to_path_buf(), no_suppression());

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
    let report = run_conformance(ProviderTarget::InProcess(scripted("bad", vec![bare_id]))).await;
    assert!(
        !report.passed(),
        "a frame with no citation label must not be judged conformant"
    );
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == CHECK_FRAME_VALIDITY && check.status == CheckStatus::Fail),
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
    let host = session_host(
        Arc::new(store),
        vec![],
        dir.path().to_path_buf(),
        no_suppression(),
    );
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
