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

/// The [`stella_core::ports::SteeringRequery`] implementation (#3243 Phase
/// 3): [`super::SessionMemory`]'s plane, asked again mid-turn when the
/// engine's signal says the work has moved.
///
/// The port contract puts hysteresis and dedup here, and both are cheap and
/// deterministic:
///
/// - **Hysteresis** — a re-query is bought by a *changed* signal, never by a
///   counter alone: the fingerprint is the set of touched paths and error
///   classes (tool names deliberately excluded — they churn every step
///   without meaning drift), and it starts at the empty set, so a turn that
///   never drifts never queries. `MIN_STEPS_BETWEEN` spaces answers so two
///   changes in adjacent steps cannot double-bill.
/// - **Dedup** — the produced-set is seeded from every `RECALL_MARKER`
///   message already in history (the turn-opening block included), mirroring
///   `inject_recall_block`'s any-prior-marker rule: whatever this returns
///   WILL be injected verbatim by the engine, so a byte-identical block must
///   die here.
/// **Telemetry** — a re-query is a full recall fan-out, provider spend
/// included, so it reports the same `ContextRecall` event the pre-turn block
/// does (#3366). The pre-turn recall runs before the turn's channel exists and
/// is carried forward by the caller; this one runs *inside* the step loop, so
/// the adapter holds the sender itself and emits at the moment it spends.
pub(crate) struct SessionRequery<'m> {
    memory: &'m super::SessionMemory,
    state: std::sync::Mutex<RequeryState>,
    /// The turn's event stream, when the driver has one — absent only in
    /// tests, which construct the adapter without a channel.
    events: Option<stella_core::EventSender>,
}

struct RequeryState {
    /// Blocks already in (or headed for) history, by exact bytes.
    produced: std::collections::HashSet<String>,
    /// The signal fingerprint the plane last answered — or the empty
    /// fingerprint at turn open, so an unchanged signal never queries.
    answered_fingerprint: u64,
}

/// Steps a fresh answer must wait after the previous one — spacing, not the
/// trigger (the fingerprint is the trigger).
const MIN_STEPS_BETWEEN: u32 = 2;

impl<'m> SessionRequery<'m> {
    /// A per-turn adapter over this session's memory, seeded with the recall
    /// blocks `messages` already carries so none is ever re-injected.
    pub(crate) fn new(
        memory: &'m super::SessionMemory,
        messages: &[stella_protocol::CompletionMessage],
    ) -> Self {
        let produced = messages
            .iter()
            .filter(|m| {
                m.role == stella_protocol::MessageRole::User
                    && m.content.starts_with(stella_core::receipts::RECALL_MARKER)
            })
            .map(|m| m.content.clone())
            .collect();
        Self {
            memory,
            state: std::sync::Mutex::new(RequeryState {
                produced,
                answered_fingerprint: fingerprint(&[], &[]),
            }),
            events: None,
        }
    }

    /// Report every answered re-query's recall into this turn's event stream
    /// (#3366). Separate from [`Self::new`] because the sender does not exist
    /// until the driver has opened the channel, which happens after the
    /// adapter's borrow of session memory is taken.
    pub(crate) fn with_events(mut self, events: stella_core::EventSender) -> Self {
        self.events = Some(events);
        self
    }
}

/// Order-free digest of the drift markers. `BTreeSet` so two signals that
/// saw the same facts in a different order agree.
fn fingerprint(paths: &[String], errors: &[&str]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    let paths: std::collections::BTreeSet<&str> = paths.iter().map(String::as_str).collect();
    let errors: std::collections::BTreeSet<&str> = errors.iter().copied().collect();
    (paths, errors).hash(&mut hasher);
    hasher.finish()
}

#[async_trait::async_trait]
impl stella_core::ports::SteeringRequery for SessionRequery<'_> {
    async fn requery(&self, signal: &stella_core::steering::TurnSignal<'_>) -> Option<String> {
        if signal.since_last_query < MIN_STEPS_BETWEEN {
            return None;
        }
        let current = fingerprint(signal.touched_paths, signal.errors_seen);
        if self
            .state
            .lock()
            .expect("requery state")
            .answered_fingerprint
            == current
        {
            return None;
        }
        let recalled = self.memory.signal_recall_block(signal).await;
        // The spend happened here, so it is reported here — before the dedup
        // and the empty-block gate below, both of which can discard the text
        // while the provider round trip is already paid for. That discard is
        // exactly the unmeterable cost the event exists to surface (#3366).
        if let (Some(events), Some(event)) = (&self.events, recalled.telemetry_event()) {
            let _ = events.send(event);
        }
        let mut state = self.state.lock().expect("requery state");
        // The signal is answered either way: a drift that surfaced nothing
        // new must not be re-asked every step until it drifts again.
        state.answered_fingerprint = current;
        let block = recalled.text?;
        state.produced.insert(block.clone()).then_some(block)
    }
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
