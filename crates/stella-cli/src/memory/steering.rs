//! The steering plane's production implementation (#3349) — the frame adapter
//! `stella-core` cannot hold, and the one packing pass every context source
//! now goes through.
//!
//! `stella-core::steering` landed the port, the types, and the budgeter
//! (#3348); this module is what stands behind the port. The shape is
//! gather-then-pack: the async I/O (frame recall, skill loading, the record
//! render) happens in `memory::recall` as it always did, the adapters map each
//! source's output into candidates, and [`GatheredSteering::query`] packs the
//! union once. Selection is unchanged by construction — the adapters map what
//! the selectors already chose — and the golden block test
//! (`memory::tests::golden_block`) is the byte-level proof.

use stella_core::steering::{
    DroppedCandidate, SteeringCandidate, SteeringPlane, SteeringSet, SteeringSource, TurnSignal,
    pack_to_budget,
};
use stella_pipeline::RecalledFrame;

use super::recall::frame_recall_line;

/// Recalled frames as candidates — the adapter that cannot live in
/// `stella-core::steering::adapt` because [`RecalledFrame`] is a
/// `stella-pipeline` type and the core sits below the pipeline.
///
/// `score` is the recall fusion's own rank, highest first: the RRF+MMR merge
/// returns frames in fused order and reports no per-frame number, so position
/// is the source's whole within-source answer. `est_tokens` is measured over
/// the exact recall line the section renderer emits — the #3334
/// single-producer wire format, via [`frame_recall_line`].
pub(super) fn frame_candidates(frames: &[RecalledFrame]) -> Vec<SteeringCandidate> {
    frames
        .iter()
        .enumerate()
        .map(|(rank, frame)| SteeringCandidate {
            source: SteeringSource::Memory,
            handle: frame_handle(frame),
            score: (frames.len() - rank) as f64,
            why: format!("recall fusion ranked it #{} for this goal", rank + 1),
            est_tokens: stella_protocol::estimate_tokens(&frame_recall_line(frame)),
        })
        .collect()
}

/// A frame's identity in the steering ledger: the stable `nod_…` id when the
/// frame is materialized, its citation label otherwise — the same precedence
/// a receipt join uses.
pub(super) fn frame_handle(frame: &RecalledFrame) -> String {
    frame
        .id
        .clone()
        .unwrap_or_else(|| frame.citation_label.clone())
}

/// The plane over one turn's gathered candidates.
///
/// This slice's budget is **the spend the sources already authorized**: each
/// source's own budget (the record channel's char cap, the skills section
/// budget, recall's `max_tokens`) has already decided membership, so the pack
/// runs at exactly the sum of the surviving estimates and can evict nothing —
/// which is the migration contract (#3349: same selection, same bytes). What
/// the pack contributes today is the union, the deterministic cross-source
/// order, and the one drop ledger. The budget starts *binding* when the tool
/// arm joins the plane and the per-source caps collapse into a shared one —
/// that is a behavior change, and it is sequenced with #3033/#1856 as Phase 4
/// of #3243, not smuggled into a refactor.
pub(super) struct GatheredSteering {
    pub candidates: Vec<SteeringCandidate>,
    /// Drops the sources' own budgets already decided — today the record
    /// channel's named evictions, the behavior `SteeringSet::dropped`
    /// generalizes.
    pub source_drops: Vec<DroppedCandidate>,
}

impl SteeringPlane for GatheredSteering {
    /// The signal's prompt has already shaped every candidate upstream (each
    /// selector still queries per prompt, as before the migration); the
    /// richer fields — recent tools, touched paths, errors seen — are what
    /// Phase 3's proactive re-query starts reading. Until then the plane
    /// deliberately takes no second look at it.
    fn query(&self, _signal: &TurnSignal<'_>) -> SteeringSet {
        let authorized: u64 = self.candidates.iter().map(|c| c.est_tokens).sum();
        let mut set = pack_to_budget(self.candidates.clone(), authorized);
        set.dropped.extend(self.source_drops.iter().cloned());
        set
    }
}
