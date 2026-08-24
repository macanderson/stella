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
use stella_protocol::RecalledFrame;

use super::recall::frame_recall_line;

/// Recalled frames as candidates — the CLI's own adapter over
/// [`RecalledFrame`] (a `stella-protocol` boundary type, not
/// `stella-core::steering::adapt`'s own vocabulary), because the recall
/// budget/citation shaping here (`est_tokens`, the `frame_recall_line` wire
/// format) is CLI-surface presentation, not engine decision logic (invariant
/// 2: no I/O, no presentation formatting inside `stella-core`).
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

/// What one turn's recall blocks have already put in front of the model, by
/// steering handle.
///
/// The dedup key for the mid-turn re-query (#4236). The re-query used to
/// compare rendered blocks byte for byte, which answers a question drift never
/// asks: drift is incremental, so the block that follows `{A, B, C}` is
/// `{A, B, C, D}` — different bytes, same three frames, injected again whole.
/// Frames and skills carry stable handles (a `nod_…` id or citation label, a
/// skill slug), so the set of handles is the honest granularity.
///
/// Records joined the set in #4498: their channel renders as one budgeted
/// block, so suppressing one means re-rendering the channel without it —
/// [`stella_core::records::Registry::render_volatile_for_turn_excluding`] is
/// that door, and these handles are what it is handed.
#[derive(Debug, Default, Clone)]
pub(crate) struct ProducedSteering {
    frames: std::collections::HashSet<String>,
    skills: std::collections::HashSet<String>,
    records: std::collections::HashSet<String>,
}

impl ProducedSteering {
    /// The handles a rendered block contributed — the frames, skills and
    /// records that reached its bytes, not the wider set recall returned.
    ///
    /// `skills` is the plane's selection, and only its `section_fit` prefix
    /// actually renders: [`stella_core::skills::render_skills_section`] stops at
    /// its own token budget and marks the rest omitted. Counting the omitted
    /// tail as produced would suppress a skill the model was never shown, which
    /// is the one direction this set must not be wrong in. `records` carries no
    /// such asymmetry: [`stella_core::records::RenderedChannel::rendered`] is
    /// already the post-budget answer.
    pub(super) fn of(
        frames: &[RecalledFrame],
        skills: &[stella_core::skills::SelectedSkill],
        records: &[String],
    ) -> Self {
        let rendered = &skills[..stella_core::skills::section_fit(skills)];
        Self {
            frames: frames.iter().map(frame_handle).collect(),
            skills: rendered.iter().map(|s| s.skill.name.clone()).collect(),
            records: records.iter().cloned().collect(),
        }
    }

    /// Has this frame handle already been rendered this turn?
    pub(super) fn has_frame(&self, handle: &str) -> bool {
        self.frames.contains(handle)
    }

    /// Has this skill slug already been rendered this turn?
    pub(super) fn has_skill(&self, slug: &str) -> bool {
        self.skills.contains(slug)
    }

    /// The record handles already rendered this turn, for the channel's
    /// exclusion door.
    pub(super) fn records(&self) -> &std::collections::HashSet<String> {
        &self.records
    }

    /// Fold another block's handles in.
    fn absorb(&mut self, other: &Self) {
        self.frames.extend(other.frames.iter().cloned());
        self.skills.extend(other.skills.iter().cloned());
        self.records.extend(other.records.iter().cloned());
    }
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
    /// Drops the sources' own budgets already decided, from all three sources
    /// since #3358: the record channel's named evictions, the recall host's
    /// merge report, and the skills selector's top-k and section cuts.
    ///
    /// One asymmetry is deliberate and worth stating, because `by_source`
    /// counts it: a skill omitted by `render_skills_section`'s own token
    /// budget appears in **both** `selected` and `dropped`. The plane
    /// authorized it and the section renderer then left it out — those are two
    /// true facts about one skill, and the plane suppressing it instead would
    /// change the rendered bytes, which is the Phase 4 budget merge (#3243),
    /// not this ledger slice.
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
/// - **Dedup** — two granularities, because the two failures are different.
///   Per *handle* ([`ProducedSteering`]): a frame or skill this turn has
///   already rendered is left out of the next block, so an incremental drift
///   re-injects only what it added rather than the whole set again (#4236).
///   Per *block*: the produced-set is seeded from every `RECALL_MARKER`
///   message already in history (the turn-opening block included), mirroring
///   `inject_recall_block`'s any-prior-marker rule: whatever this returns
///   WILL be injected verbatim by the engine, so a byte-identical block must
///   die here.
///
///   The handle set is seeded from the turn-opening block's own
///   [`ProducedSteering`] (#4498), carried in by the driver rather than
///   recovered from history — a rendered block is not invertible to handles
///   (a code-graph frame renders under its label while its handle is its node
///   id), which is why the opening block's `produced` travels with its
///   telemetry event instead of being re-derived here.
/// - **Telemetry** — a re-query is a full recall fan-out with provider spend
///   behind it, so it reports the same `ContextRecall` event the pre-turn
///   block does (#3366). The pre-turn recall runs before the turn's channel
///   exists and is carried forward by the caller; this one runs *inside* the
///   step loop, so the adapter holds the sender and emits as it spends.
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
    /// Frames and skills this turn's re-queries have already rendered, so the
    /// next block carries only what is new (#4236).
    produced_steering: ProducedSteering,
    /// The signal fingerprint the plane last answered — or the empty
    /// fingerprint at turn open, so an unchanged signal never queries.
    answered_fingerprint: u64,
}

/// Steps a fresh answer must wait after the previous one — spacing, not the
/// trigger (the fingerprint is the trigger).
const MIN_STEPS_BETWEEN: u32 = 2;

impl<'m> SessionRequery<'m> {
    /// A per-turn adapter over this session's memory, seeded with the recall
    /// blocks `messages` already carries so none is ever re-injected, and with
    /// the turn-opening block's own handles so the first answered re-query
    /// does not repeat its frames, skills or records (#4498).
    pub(crate) fn new(
        memory: &'m super::SessionMemory,
        messages: &[stella_protocol::CompletionMessage],
        opening: ProducedSteering,
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
                produced_steering: opening,
                answered_fingerprint: fingerprint(&[], &[]),
            }),
            events: None,
        }
    }

    /// Report every answered re-query's recall into this turn's event stream
    /// (#3366). Separate from [`Self::new`] because the sender does not exist
    /// until the driver has opened the channel.
    #[must_use]
    pub(crate) fn with_events(mut self, events: stella_core::EventSender) -> Self {
        self.events = Some(events);
        self
    }
}

/// The turn's re-query adapter, wired to report into the turn's own stream —
/// the whole seam a driver needs, in one call.
///
/// A free function rather than two call sites assembling the same two steps,
/// because the second step is the one that is silently optional: a driver
/// that constructs a [`SessionRequery`] and forgets [`SessionRequery::with_events`]
/// still compiles and still re-queries, and the only symptom is the missing
/// telemetry #3366 was filed for. Both drivers (`agent::run_turn` and the
/// deck's `run_lead_turn`) take it from here, so there is one place to forget.
pub(crate) fn requery_for_turn<'m>(
    memory: Option<&'m super::SessionMemory>,
    messages: &[stella_protocol::CompletionMessage],
    events: stella_core::EventSender,
    opening: ProducedSteering,
) -> Option<SessionRequery<'m>> {
    memory.map(|memory| SessionRequery::new(memory, messages, opening).with_events(events))
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
        // The handle set is cloned out rather than held across the await: the
        // guard is a `std::sync::Mutex`, and holding one over a suspension
        // point is what makes a future non-`Send`.
        let already = self
            .state
            .lock()
            .expect("requery state")
            .produced_steering
            .clone();
        let recalled = self.memory.signal_recall_block(signal, &already).await;
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
        // Recorded before the block-level dedup can discard the text, because
        // these handles are in front of the model either way: a block dropped
        // here is a block byte-identical to one already in history.
        state.produced_steering.absorb(&recalled.produced);
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

#[cfg(test)]
mod tests {
    use super::*;
    use stella_core::skills::{SelectedSkill, Skill, SkillOrigin};

    fn selected(name: &str, body_bytes: usize) -> SelectedSkill {
        SelectedSkill {
            skill: Skill {
                name: name.to_string(),
                description: "a skill".to_string(),
                domains: Vec::new(),
                body: "x".repeat(body_bytes),
                source_path: format!(".stella/skills/{name}/SKILL.md"),
                origin: SkillOrigin::Workspace,
            },
            score: 1.0,
            matched_terms: Vec::new(),
            matched_domains: Vec::new(),
        }
    }

    /// A skill the section renderer's own token budget omitted was never shown
    /// to the model, so it must not be recorded as produced — recording it
    /// would suppress it on every later re-query of the turn, which is the one
    /// direction this set must not be wrong in.
    ///
    /// Six full-length skills, because a single one cannot blow the section
    /// budget: each body is truncated to its own `SKILL_BODY_TOKEN_BUDGET`
    /// first, so the cut is only reachable by accumulation. The arithmetic
    /// itself is `section_fit`'s to own — this asserts against whatever it
    /// answers rather than pinning a number that would drift with either
    /// budget.
    #[test]
    fn a_skill_the_section_budget_omitted_is_not_recorded_as_produced() {
        let skills: Vec<_> = (0..6).map(|i| selected(&format!("s{i}"), 4_000)).collect();
        let fit = stella_core::skills::section_fit(&skills);
        assert!(
            fit > 0 && fit < skills.len(),
            "the control: the section renderer renders some and cuts the rest, got {fit}"
        );

        let produced = ProducedSteering::of(&[], &skills, &[]);
        for sel in &skills[..fit] {
            assert!(
                produced.has_skill(&sel.skill.name),
                "a rendered skill is produced: {}",
                sel.skill.name
            );
        }
        for sel in &skills[fit..] {
            assert!(
                !produced.has_skill(&sel.skill.name),
                "a skill the model was never shown stays offerable: {}",
                sel.skill.name
            );
        }
    }

    /// Frames carry no such cut — the plane's pack already decided them, and
    /// every one that reaches [`ProducedSteering::of`] reaches the bytes.
    #[test]
    fn an_empty_selection_produces_nothing_and_does_not_panic() {
        let produced = ProducedSteering::of(&[], &[], &[]);
        assert!(!produced.has_skill("anything"));
        assert!(!produced.has_frame("nod_anything"));
        assert!(produced.records().is_empty());
    }
}
