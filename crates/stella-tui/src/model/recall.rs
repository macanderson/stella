//! The read-model of one context recall, and the fold that builds it.
//!
//! Its own module because the parent was 51 lines under the 1500-line guard
//! when this landed, and the god-file rule is a planning input rather than a
//! review afterthought: new logic lands in a sibling submodule instead of
//! pushing a file over the line. The cut follows a concern, not a line count —
//! everything here is about a single event, `AgentEvent::ContextRecall`, and
//! nothing else in the fold reads these types.
//!
//! What the split preserves is the reason the types exist at all. The parent's
//! `TranscriptEntry::ContextRecall` used to be `{ frames: usize, tokens: u32,
//! labels: Vec<String> }` — a five-record table with three of its four columns
//! deleted — so every surface downstream could do nothing but comma-join the
//! labels into a paragraph that wrapped mid-word at the pane edge. A `symbol`
//! and an `episode` rendered identically, per-frame cost was unrepresentable,
//! and `ctrl+o` had nothing left to reveal. **No renderer can be better than
//! what the read-model kept.**

use stella_protocol::{ContextFrameRef, ContextUsage, ProviderShare};

/// One recalled context frame.
///
/// A near-lossless read-model of [`ContextFrameRef`]: every field the wire
/// carries survives except `block_id`, which recall never sets — a frame
/// becomes a block only once it is *rendered* into a message, after the event
/// is emitted (see [`stella_protocol::Recall::telemetry_event`], which #3865
/// retargeted here out of `stella-pipeline` ahead of deleting that crate).
///
/// The two fields worth naming are the two the old label-only shape lost.
/// [`Self::kind`] is what separates a 60-token graph symbol from an 800-token
/// episodic memory, and until it survived to the renderer the deck showed both
/// as undifferentiated prose. [`Self::tokens`] is what turns a recall total
/// into a finding: "1155 tok" is a number, but one frame holding 812 of them
/// is a reason to tune retrieval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecalledFrameRow {
    /// Protocol frame kind — `symbol`, `memory`, `episode`, `fact`,
    /// `snippet`, `doc`, `graph`. Empty on a stream recorded before the field
    /// existed, which the renderer shows as `frame` rather than a blank column.
    pub kind: String,
    /// The human citation (L-C4) — never a raw id.
    pub label: String,
    /// The frame's canonical source URI, when it declared one.
    pub uri: Option<String>,
    /// The CGP provider leg that served the frame (`code-graph`,
    /// `workspace-memory`).
    pub provider: String,
    /// The original source named by the provenance chain — deliberately
    /// distinct from [`Self::provider`], which is the adapter that fronted it.
    pub source: String,
    /// The most-derived provenance method, when declared.
    pub method: Option<String>,
    /// The provider's own frame id, a detail-view identifier only (L-C4).
    pub id: Option<String>,
    /// `"sha256:<hex>"` over the exact injected bytes. Its **absence** is
    /// meaningful — such a frame is not verifiable per the context-reuse spec
    /// — so the expanded view says "unverifiable" rather than showing nothing.
    pub digest: Option<String>,
    /// What injecting this frame cost the prompt.
    pub tokens: u32,
}

/// The CGP budget report for one recall, as the deck's expanded view shows it.
///
/// `served`/`rejected` is the pair that earns this its place: a provider that
/// served five frames and had two rejected has a cost-honesty problem the
/// frame list alone cannot show, because rejected frames never reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallBudget {
    /// Tokens the host asked for.
    pub requested: u64,
    /// Tokens the recall actually spent.
    pub consumed: u64,
    /// `(provider, served, rejected, tokens)`, one per contributing leg.
    pub providers: Vec<(String, u32, u32, u64)>,
}

/// Every wire field of one `ContextRecall`, projected for the transcript.
///
/// Returned as a tuple rather than a struct because it is destructured
/// straight into [`crate::model::TranscriptEntry::ContextRecall`]'s fields at
/// the single call site; a second shape between the wire and the entry would
/// be a third place for the projection to lose something.
pub(super) type Projected = (
    Vec<RecalledFrameRow>,
    Vec<(String, u32)>,
    Option<RecallBudget>,
);

/// Project the wire event's frames, provider mix and usage report.
///
/// Deliberately total: there is no field here the fold is allowed to drop.
/// The last narrowing at this seam is why `latency_ms` and `used_ann_index` —
/// both added to the wire precisely because recall sits on the first-token
/// path of every turn (#875) — never reached a surface at all.
pub(super) fn project(
    frames: &[ContextFrameRef],
    provider_mix: &[ProviderShare],
    usage: Option<&ContextUsage>,
) -> Projected {
    let rows = frames
        .iter()
        .map(|f| RecalledFrameRow {
            kind: f.kind.clone(),
            label: f.citation_label.clone(),
            uri: f.uri.clone(),
            provider: f.provider.clone(),
            source: f.source.clone(),
            method: f.method.clone(),
            id: f.id.clone(),
            digest: f.content_digest.clone(),
            tokens: f.token_cost,
        })
        .collect();
    let mix = provider_mix
        .iter()
        .map(|s| (s.provider.clone(), s.frames))
        .collect();
    let budget = usage.map(|u| RecallBudget {
        requested: u64::from(u.budget_requested),
        consumed: u.budget_consumed,
        providers: u
            .providers
            .iter()
            .map(|p| {
                (
                    p.provider_id.clone(),
                    p.frames_served,
                    p.frames_rejected,
                    p.token_cost,
                )
            })
            .collect(),
    });
    (rows, mix, budget)
}
