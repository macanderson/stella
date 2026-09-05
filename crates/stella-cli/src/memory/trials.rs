//! Trials for the two things recall puts in front of the model: a memory
//! record, and a mined rule.
//!
//! A trial says what a turn could have used, and what it did use.
//! [`appraisals::sweep`] needs both. With only the second it has no control
//! arm, so it can never say whether the thing helped.
//!
//! Skills got this seam first. `SessionMemory::note_turn_skills` notes the
//! join at turn start, and `record_skill_trials` writes it at turn end. This
//! is the same pair of moves for the other two kinds, whose half of the
//! ledger had no producer and so stayed empty.
//!
//! # One pick per turn
//!
//! A turn can render more than once. The opening block runs one query, and a
//! mid-turn re-query runs another. Each pass scores its own shortlist, so the
//! two see different things. A holdout picked per pass could drop one record
//! from the first block and a different one from the second, and the ledger
//! would then describe neither turn. The skill arm hit that bug, and fixed
//! it by sharing one pick across its passes.
//!
//! So the pick is made once, by the first pass that has something to pick
//! from, and every later pass in the turn reads it back. The turn's offered
//! set is the union of what the passes offered, and its selected set is the
//! union of what they showed.
//!
//! # One item per turn
//!
//! [`stella_learn::holdout`] holds one item back per turn. That rule does
//! not bend for a third kind. Hold a skill and a memory back on one turn,
//! and every memory control turn is a skill control turn too. Then neither
//! arm can say which one the outcome belongs to.
//!
//! So the three kinds take the schedule in turn. [`HOLDOUT_ARMS`] is the
//! order, and `stella_learn::holdout::arm` says whose turn it is.

use std::collections::HashSet;

use stella_learn::ledger::ArtifactKind;
use stella_learn::self_tuning::TaskOutcome;
use stella_learn::skills::appraisal::SkillTrial;
use stella_protocol::RecalledFrame;
use stella_records::records::{Registry, RenderedChannel, TurnFacts};

use super::recall::{RECORD_CHANNEL_BUDGET, turn_path_tokens};
use super::steering::frame_handle;
use super::{SessionMemory, appraisals};

/// The kinds that take the holdout schedule in turn.
///
/// A kind's place in this list is the arm number
/// `stella_learn::holdout::arm` returns. Skill is first because it was here
/// first. It held the schedule alone. Move it now and every live workspace
/// holds its next skill back on a different turn.
pub(super) const HOLDOUT_ARMS: [ArtifactKind; 3] = [
    ArtifactKind::Skill,
    ArtifactKind::Memory,
    ArtifactKind::Rule,
];

/// Which kind the `ordinal`-th holdout acts on.
pub(super) fn holdout_kind(ordinal: u64) -> Option<ArtifactKind> {
    stella_learn::holdout::arm(ordinal, HOLDOUT_ARMS.len())
        .and_then(|arm| HOLDOUT_ARMS.get(arm).copied())
}

/// One turn of the live window, before `appraisals::record_turn` sets
/// `selected` per artifact.
///
/// One shape for all three kinds. A row is the same row whatever the id
/// names. A second copy of this literal is a second place for the window's
/// task key to drift.
pub(super) fn live_trial(succeeded: bool) -> SkillTrial {
    SkillTrial {
        task: appraisals::LIVE_WINDOW_TASK.to_string(),
        // Set per artifact by `appraisals::record_turn`. This value is never
        // read.
        selected: false,
        outcome: TaskOutcome {
            succeeded,
            cost_usd: 0.0,
            tokens: 0,
            retries: 0,
        },
        turns: 1,
    }
}

/// What one kind's turn offered, and what it showed.
#[derive(Debug, Default)]
struct KindJoin {
    offered: Vec<String>,
    selected: Vec<String>,
}

impl KindJoin {
    /// Fold one render pass in. Order is kept and repeats are dropped. The
    /// ledger rows then land in the order the turn met them.
    fn note(&mut self, offered: &[String], selected: &[String]) {
        for id in offered {
            if !self.offered.contains(id) {
                self.offered.push(id.clone());
            }
        }
        for id in selected {
            if !self.selected.contains(id) {
                self.selected.push(id.clone());
            }
        }
    }
}

/// What this turn has to say about its memories and its rules.
#[derive(Debug, Default)]
pub(crate) struct TurnContextTrials {
    memories: KindJoin,
    rules: KindJoin,
    /// What the holdout settled on. One pair, because one item goes per
    /// turn: the kind whose turn it is, and the id it picked.
    held: Option<(ArtifactKind, String)>,
}

impl TurnContextTrials {
    /// This kind's join, to fold a pass into.
    fn join_mut(&mut self, kind: ArtifactKind) -> Option<&mut KindJoin> {
        match kind {
            ArtifactKind::Memory => Some(&mut self.memories),
            ArtifactKind::Rule => Some(&mut self.rules),
            // Skills keep their own join on the session. It is armed at turn
            // start by the selection pass, not by a render.
            ArtifactKind::Skill => None,
        }
    }
}

impl SessionMemory {
    /// Drop the turn's join and its holdout pick.
    ///
    /// Called where the turn arms its controls. A turn that never reaches an
    /// episode then leaves no population for the next one.
    pub(super) fn reset_context_trials(&self) {
        if let Ok(mut guard) = self.context_trials.lock() {
            *guard = TurnContextTrials::default();
        }
    }

    /// The id this turn holds back for `kind`, settled from `population` the
    /// first time a pass asks and read back after that.
    ///
    /// `None` on three counts: this turn is not a holdout turn, the schedule
    /// is on another kind, or there was nothing to pick from.
    fn held_for(&self, kind: ArtifactKind, population: &[String]) -> Option<String> {
        let ordinal = self.holdout_ordinal?;
        if holdout_kind(ordinal) != Some(kind) {
            return None;
        }
        let mut guard = self.context_trials.lock().ok()?;
        if guard.held.is_none() {
            let ids: Vec<&str> = population.iter().map(String::as_str).collect();
            guard.held =
                stella_learn::holdout::pick(ordinal, &ids).map(|id| (kind, id.to_string()));
        }
        match &guard.held {
            Some((held, id)) if *held == kind => Some(id.clone()),
            _ => None,
        }
    }

    /// Fold one render pass into this turn's join for `kind`.
    fn note_context_trial(&self, kind: ArtifactKind, offered: &[String], selected: &[String]) {
        let Ok(mut guard) = self.context_trials.lock() else {
            return;
        };
        if let Some(join) = guard.join_mut(kind) {
            join.note(offered, selected);
        }
    }

    /// Take this turn's held-back memory out of a block about to render, and
    /// note what the block offered and showed.
    ///
    /// `offered` is what the recall query answered. Both arms come from that
    /// set. It takes in the frames the plane's budget then cut, because a
    /// frame that lost its seat is still one this turn could have used.
    /// `kept` is what the model is about to read.
    ///
    /// No per-turn notice here, unlike the skill arm. The deck says at boot
    /// what the schedule costs. A memory handle is not a name a reader can
    /// act on.
    pub(super) fn withhold_held_memory(
        &self,
        offered: &[RecalledFrame],
        kept: Vec<RecalledFrame>,
    ) -> Vec<RecalledFrame> {
        let population: Vec<String> = offered.iter().map(frame_handle).collect();
        let shown = match self.held_for(ArtifactKind::Memory, &population) {
            None => kept,
            Some(held) => kept
                .into_iter()
                .filter(|frame| frame_handle(frame) != held)
                .collect(),
        };
        let selected: Vec<String> = shown.iter().map(frame_handle).collect();
        self.note_context_trial(ArtifactKind::Memory, &population, &selected);
        shown
    }

    /// This turn's volatile record channel, with the holdout's pick left out
    /// and the turn's join noted.
    ///
    /// The registry rides back with the render. The steering adapters
    /// (`stella_records::adapt`) resolve the rendered handles through it for
    /// their token estimates. The render and the ledger must come from one
    /// selection pass, not two.
    ///
    /// A holdout turn renders twice. The channel is one budgeted block. A
    /// record left out through the exclusion door lands in neither
    /// `rendered` nor `dropped`. Read the population after that and it is
    /// missing the one record the trial is about. So the first render leaves
    /// nothing out and gives the population. The second is what the model
    /// reads.
    pub(super) fn turn_records_held(
        &self,
        facts: &TurnFacts<'_>,
        already_rendered: &HashSet<String>,
    ) -> Option<(Registry, RenderedChannel)> {
        // Cloned out of the lock rather than borrowed through it. The caller
        // threads the registry across the rest of its selection pass. A guard
        // held that long would block a mid-session freshness swap.
        let registry = self.record_registry.read().expect("records lock").clone()?;
        let offered = registry.render_volatile_for_turn_excluding(
            facts,
            Some(RECORD_CHANNEL_BUDGET),
            already_rendered,
        );
        let population: Vec<String> = offered
            .rendered
            .iter()
            .chain(offered.dropped.iter())
            .cloned()
            .collect();
        let rendered = match self.held_for(ArtifactKind::Rule, &population) {
            None => offered,
            Some(held) => {
                let mut excluded = already_rendered.clone();
                excluded.insert(held);
                registry.render_volatile_for_turn_excluding(
                    facts,
                    Some(RECORD_CHANNEL_BUDGET),
                    &excluded,
                )
            }
        };
        self.note_context_trial(ArtifactKind::Rule, &population, &rendered.rendered);
        Some((registry, rendered))
    }

    /// [`Self::turn_records_held`] for a turn described by its prompt alone —
    /// the facts the turn-opening block selects on.
    pub(super) fn turn_records_for_prompt(
        &self,
        prompt: &str,
    ) -> Option<(Registry, RenderedChannel)> {
        let paths = turn_path_tokens(prompt);
        let facts = TurnFacts {
            text: prompt,
            paths: &paths,
        };
        self.turn_records_held(&facts, &HashSet::new())
    }

    /// Append this turn's memory and rule trials to the shared ledger.
    ///
    /// Beside `record_skill_trials` on the episode seam. It takes the join
    /// rather than reading it, so one turn writes its rows once.
    ///
    /// A turn that offered nothing of a kind writes nothing for that kind. It
    /// is not evidence about any memory or any rule. Best effort, like every
    /// ledger write here: a failed write must never fail its own turn.
    pub(super) fn record_context_trials(&self, succeeded: bool) {
        let Ok(mut guard) = self.context_trials.lock() else {
            return;
        };
        let joins = std::mem::take(&mut *guard);
        drop(guard);
        let trial = live_trial(succeeded);
        for (kind, join) in [
            (ArtifactKind::Memory, &joins.memories),
            (ArtifactKind::Rule, &joins.rules),
        ] {
            if join.offered.is_empty() {
                continue;
            }
            appraisals::record_turn(
                &self.workspace_root,
                kind,
                &join.offered,
                &join.selected,
                &trial,
            );
        }
    }
}

#[cfg(test)]
mod tests;
