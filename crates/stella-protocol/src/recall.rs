// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Context recall at turn start (L-E8) — the port every door's memory
//! injection shares, and the frame/telemetry shapes it produces.
//!
//! # Why this lives here and not in a pipeline-shaped crate
//!
//! [`ContextRecallPort`] and [`RecalledFrame`] were first defined in
//! `stella-pipeline::ports`, which was the only caller at the time. That
//! crate's own module doc already described this as "a deliberately minimal
//! local shape" adapted at the seam so the pipeline "takes **no** dependency
//! on `stella-context`" — i.e. it was a boundary type from the start, not
//! pipeline-internal orchestration logic, and every door's recall injection
//! (`stella run`, `stella goal`, `stella fleet`, the Command Deck) depended on
//! it whether or not the staged pipeline itself ran. The removal census that
//! later deleted `stella-pipeline` outright (#3865) named this the single
//! highest-fan-out RETARGET in the whole crate for exactly that reason, and
//! moved it here ahead of the deletion — beside [`crate::provider::Provider`]
//! and [`crate::event::ScopeProposal`], the crate's other async-trait
//! crossing points — rather than into `stella-cli`, so a future host that is
//! not `stella-cli` (an embedded `stella-engine` consumer, `stella-serve`)
//! can still speak this port without a dependency on the CLI binary.
//!
//! While `stella-pipeline` still existed, it re-exported every name below
//! from `crate::ports` unchanged, so its own call sites needed no edits; the
//! crate is gone now (#3865), and this module is these types' only home.

use async_trait::async_trait;

use crate::event::{AgentEvent, ContextFrameRef, ContextUsage, ProviderShare};

/// One frame recalled from the context plane at turn start (the "context
/// recall" stage). A deliberately minimal local shape: the real
/// `stella-context` crate owns the rich `ContextFrame`/retrieval types; a
/// host adapts its frames down to this at the seam so nothing above the
/// context plane takes a dependency on it (dependency direction discipline).
/// `citation_label` is mandatory and human-readable (L-C4); a
/// not-yet-materialized frame carries `id: None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecalledFrame {
    /// Human-readable citation, never a raw id (L-C4).
    pub citation_label: String,
    /// The CGP provider leg that returned this frame.
    pub provider: String,
    /// The record's original source from its provenance chain. This can
    /// differ from `provider` when an adapter fronts another context store.
    pub source: String,
    /// Protocol frame kind (`symbol`, `memory`, `graph`, ...).
    pub kind: String,
    /// Canonical source URI, when declared.
    pub uri: Option<String>,
    /// Most-derived provenance method, when declared.
    pub method: Option<String>,
    /// The actual text injected into the volatile recall message.
    pub content: String,
    /// Token cost of this frame, for the `ContextRecall` event's budget
    /// report (L-C5).
    pub token_cost: u32,
    /// Present only when the frame is materialized in the store; a candidate
    /// frame carries `None` (L-C4).
    pub id: Option<String>,
    /// `"sha256:<hex>"` over the exact bytes of [`Self::content`], as the
    /// source frame declared it — Phase 2 (#713).
    ///
    /// The digest was already minted at the store boundary and was then thrown
    /// away one layer up, at the CGP→pipeline projection, so every
    /// `ContextFrameRef` on every recall event carried `content_digest: null`.
    /// It is what makes a frame reference *resolve*: with it, a receipt names
    /// a record and the exact revision of that record's content, which is the
    /// identity ADR 0004 defines. Without it, a reference names a row whose
    /// text may since have been superseded, and a past turn's context is
    /// reconstructed rather than verified.
    ///
    /// `None` when the serving provider declared none — per
    /// `docs/spec/adaptive-context/context-reuse.md` §1 such a frame is *not verifiable* and a host
    /// must re-query rather than reuse it, so the absence is meaningful and is
    /// carried rather than papered over with a locally recomputed hash.
    pub content_digest: Option<String>,
}

impl RecalledFrame {
    /// The citation label, when it says something [`Self::content`] does not.
    ///
    /// Memory and episode nodes mint their label FROM their content
    /// (`stella-context`'s `writeback.rs::truncate_label`: the content
    /// verbatim at ≤80 chars, its first 79 chars plus `…` above that), so for
    /// them the label is the content — or its head — repeated. Every surface
    /// that rendered `label` beside `content` therefore shipped the same
    /// sentence twice into a rationed recall budget, on every turn that
    /// recalled anything, and the budget packer never saw it: frames are
    /// packed by `content` alone, so the block silently exceeded the budget
    /// it was packed to (#2476).
    ///
    /// `None` for exactly the two shapes `truncate_label` produces and
    /// nothing else — a label equal to the content, or a `…`-terminated
    /// prefix of it. An author-chosen label that merely resembles the
    /// content's opening words stays distinct: this predicate models the
    /// mint, not a similarity heuristic, so it can never swallow a label a
    /// human picked (L-C4 keeps its guarantee that every frame is citable —
    /// the collapsed cases are those where the content IS the citation text).
    #[must_use]
    pub fn distinct_label(&self) -> Option<&str> {
        let label = self.citation_label.trim();
        if label.is_empty() {
            return None;
        }
        let content = self.content.trim();
        if label == content {
            return None;
        }
        if let Some(stem) = label.strip_suffix('…')
            && !stem.is_empty()
            && content.starts_with(stem)
        {
            return None;
        }
        Some(label)
    }
}

/// Context recall at turn start (L-E8): a *live provider query*, never a
/// cached prompt block. The recalled frames ride as a volatile message
/// **after** the byte-stable system prefix so prompt-cache hits on that
/// prefix are preserved. A caller with no context plane wired yet supplies
/// [`NoContextRecall`].
#[async_trait]
pub trait ContextRecallPort: Send + Sync {
    /// Recall material relevant to `goal`. Returns an empty frame list when
    /// nothing is relevant or no plane is configured — never an error the
    /// caller has to special-case (weak/absent context degrades to "no
    /// frames", L-C6).
    async fn recall(&self, goal: &str) -> Recall;
}

/// The outcome of one context recall: the frames that reached the prompt, and
/// what producing them cost.
///
/// The two travel together because they are answers to different questions
/// about the same request — "what will the model see?" and "what did this turn
/// spend on context, and which providers drove it?" — and separating them
/// would mean either re-running recall to bill it or losing the cost entirely,
/// which is how context cost stayed unmeterable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recall {
    /// The frames selected for this turn, in the host's canonical render
    /// order.
    pub frames: Vec<RecalledFrame>,
    /// The CGP usage report for the request (`docs/spec/adaptive-context/context-reuse.md` §2).
    /// `None` when the port has no CGP host behind it to report one.
    pub usage: Option<ContextUsage>,
    /// Wall-clock milliseconds this recall took, measured by the port that
    /// performed it (#875). Recall is on the first-token path of every turn,
    /// so this is the one number that says whether context retrieval is the
    /// reason a turn felt slow. `0` means "not measured" — the ports with
    /// nothing to recall do not time an empty answer.
    pub latency_ms: u32,
    /// Whether the IVF approximate-nearest-neighbour accelerator fired, or
    /// `None` when this recall path does not report it. Paired with
    /// `latency_ms` because a slow recall with a cold index is a different
    /// problem, with a different fix, from a slow recall that used the index
    /// and was still slow. Tri-state because the CGP host fan-out does not
    /// carry the flag across the provider-result boundary — see the protocol
    /// event for why `false` would be a worse answer than `None`.
    pub used_ann_index: Option<bool>,
}

impl Recall {
    /// A recall of `frames` with no usage report — the shape a port without a
    /// CGP host behind it produces.
    pub fn frames(frames: Vec<RecalledFrame>) -> Self {
        Self {
            frames,
            usage: None,
            latency_ms: 0,
            used_ann_index: None,
        }
    }

    /// Whether nothing was recalled.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// This recall as its telemetry event, or `None` when nothing was recalled.
    ///
    /// Phase 2 (#713) deliverable 3. This lives on [`Recall`] rather than
    /// beside any one caller because it was, until it did, the failure mode
    /// of copying the projection into each of several call sites: the
    /// one-shot run, the interactive REPL, `/goal`, and the Command Deck all
    /// recalled and reported nothing, so most real usage was invisible to
    /// `stella inspect`. Copying the projection five times would have made
    /// the event's shape a convention rather than a definition — and the
    /// first divergence would be a provider mix that only some surfaces
    /// counted.
    ///
    /// `block_id` is deliberately absent here and present on the receipt side
    /// instead: a recalled frame becomes a context block only once it is
    /// *rendered* into a message, which happens after this event is emitted and
    /// does not happen at all on the planner's structured path. The join to a
    /// block runs through the record — `BlockOrigin::memory_id` — not through
    /// an id fabricated before the block exists.
    #[must_use]
    pub fn telemetry_event(&self) -> Option<AgentEvent> {
        if self.frames.is_empty() {
            return None;
        }
        let mut provider_mix: Vec<ProviderShare> = Vec::new();
        for frame in &self.frames {
            match provider_mix
                .iter_mut()
                .find(|share| share.provider == frame.provider)
            {
                Some(share) => share.frames += 1,
                None => provider_mix.push(ProviderShare {
                    provider: frame.provider.clone(),
                    frames: 1,
                }),
            }
        }
        Some(AgentEvent::ContextRecall {
            tokens: self.frames.iter().map(|f| f.token_cost).sum(),
            frames: self
                .frames
                .iter()
                .map(|f| ContextFrameRef {
                    id: f.id.clone(),
                    citation_label: f.citation_label.clone(),
                    provider: f.provider.clone(),
                    source: f.source.clone(),
                    kind: f.kind.clone(),
                    uri: f.uri.clone(),
                    method: f.method.clone(),
                    token_cost: f.token_cost,
                    block_id: None,
                    content_digest: f.content_digest.clone(),
                })
                .collect(),
            provider_mix,
            usage: self.usage.clone(),
            latency_ms: self.latency_ms,
            used_ann_index: self.used_ann_index,
        })
    }
}

/// A [`ContextRecallPort`] that recalls nothing — the correct default before
/// a context plane is wired, and for tasks where context grounding is off.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoContextRecall;

#[async_trait]
impl ContextRecallPort for NoContextRecall {
    async fn recall(&self, _goal: &str) -> Recall {
        Recall::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives a future to completion on the current thread with no runtime.
    ///
    /// This crate is a zero-logic, zero-I/O leaf (`AGENTS.md` invariant 4) and
    /// pulls in no async executor — [`ContextRecallPort::recall`] is `async`
    /// only because the trait must accommodate an implementer that really
    /// does await a query, and [`NoContextRecall`]'s own body never does.
    /// `Waker::noop` (stable since 1.90) is enough to poll a future that
    /// completes on its first poll; a future that genuinely parks would hang
    /// here, which is the correct failure mode for a "no-op port" test.
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        use std::task::{Context, Poll};

        let mut fut = std::pin::pin!(fut);
        let waker = std::task::Waker::noop();
        match fut.as_mut().poll(&mut Context::from_waker(waker)) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("test future did not complete on its first poll"),
        }
    }

    #[test]
    fn no_context_recall_is_inert() {
        assert!(block_on(NoContextRecall.recall("anything")).is_empty());
        assert!(
            block_on(NoContextRecall.recall("anything")).usage.is_none(),
            "a port with no CGP host behind it reports no usage"
        );
    }

    fn recalled(provider: &str, id: &str, digest: Option<&str>) -> RecalledFrame {
        RecalledFrame {
            citation_label: format!("label for {id}"),
            provider: provider.into(),
            source: "stella-context".into(),
            kind: "memory".into(),
            uri: None,
            method: None,
            content: "body".into(),
            token_cost: 7,
            id: Some(id.into()),
            content_digest: digest.map(str::to_owned),
        }
    }

    /// #2476's predicate: a label is dropped for exactly the two shapes the
    /// memory mint (`truncate_label`) produces, and nothing else.
    #[test]
    fn a_label_minted_from_the_content_is_not_distinct() {
        let frame = |label: &str, content: &str| RecalledFrame {
            citation_label: label.into(),
            content: content.into(),
            ..recalled("workspace-memory", "nod_x", None)
        };
        // ≤80 chars: the mint copies the content verbatim.
        let short = frame("prefer rg over grep", "prefer rg over grep");
        assert_eq!(short.distinct_label(), None);
        // >80 chars: the mint keeps the first 79 chars plus `…`.
        let long_content = "a".repeat(120);
        let minted: String = format!("{}…", "a".repeat(79));
        let long = frame(&minted, &long_content);
        assert_eq!(long.distinct_label(), None);
        // Whitespace differences do not defeat the match — both sides render
        // trimmed, so they compare trimmed.
        let padded = frame("  prefer rg over grep  ", "prefer rg over grep\n");
        assert_eq!(padded.distinct_label(), None);
        // An empty label was never citable text at all.
        assert_eq!(frame("", "some content").distinct_label(), None);
    }

    #[test]
    fn an_author_chosen_label_stays_distinct() {
        let frame = |label: &str, content: &str| RecalledFrame {
            citation_label: label.into(),
            content: content.into(),
            ..recalled("code-graph", "nod_y", None)
        };
        // A path label over a code excerpt — the ordinary code-graph shape.
        let hit = frame("src/lib.rs", "fn main() {}");
        assert_eq!(hit.distinct_label(), Some("src/lib.rs"));
        // A hand-picked label that happens to open the content is NOT the
        // mint's shape (no `…`), so it is the author's choice and it stays.
        let prefix = frame("prefer rg", "prefer rg over grep here");
        assert_eq!(prefix.distinct_label(), Some("prefer rg"));
        // `…`-terminated, but over different text: distinct.
        let elsewhere = frame("retry policy…", "the backoff doubles per attempt");
        assert_eq!(elsewhere.distinct_label(), Some("retry policy…"));
        // A frame with no content cannot cover any label.
        let bare = frame("an episode title", "");
        assert_eq!(bare.distinct_label(), Some("an episode title"));
    }

    #[test]
    fn a_recall_projects_to_one_telemetry_event_with_provenance_intact() {
        // Phase 2 (#713) deliverable 3. The projection lives here, once, so
        // every surface that recalls cannot disagree about the shape of the
        // event it reports — the failure mode of copying it into each.
        let recall = Recall {
            frames: vec![
                recalled("workspace-memory", "nod_a", Some("sha256:aa")),
                recalled("workspace-memory", "nod_b", None),
                recalled("code-graph", "sym_c", Some("sha256:cc")),
            ],
            usage: None,
            latency_ms: 42,
            used_ann_index: Some(true),
        };
        let Some(AgentEvent::ContextRecall {
            frames,
            provider_mix,
            tokens,
            ..
        }) = recall.telemetry_event()
        else {
            panic!("a non-empty recall reports an event");
        };
        assert_eq!(tokens, 21, "the summed per-frame cost");
        // The mix counts frames that reached the prompt, per provider, in
        // first-seen order.
        assert_eq!(provider_mix.len(), 2);
        assert_eq!(provider_mix[0].provider, "workspace-memory");
        assert_eq!(provider_mix[0].frames, 2);
        assert_eq!(provider_mix[1].frames, 1);
        // The digest survives the projection — the whole point of deliverable
        // 2. Before this it was hard-coded `None` at the one emission site.
        assert_eq!(frames[0].content_digest.as_deref(), Some("sha256:aa"));
        assert_eq!(
            frames[1].content_digest, None,
            "a provider that declared none keeps none — that absence is the \
             signal that the frame is not verifiable and must be re-queried"
        );
        assert_eq!(frames[2].content_digest.as_deref(), Some("sha256:cc"));
    }

    #[test]
    fn an_empty_recall_reports_nothing_rather_than_an_empty_event() {
        // A turn that recalled nothing did not have a recall stage worth a
        // receipt; an empty event would be a row that means "we looked" and
        // reads as "we found nothing", which are different claims.
        assert!(Recall::default().telemetry_event().is_none());
    }
}
