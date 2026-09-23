//! The A/B recall control. It picks the turns that run with no recalled
//! context, on a schedule that outlives the process, so their outcomes can be
//! compared with the turns that had it.

use crate::memory::SessionMemory;

/// The name the A/B recall control's durable turn counter is filed under in
/// the context store (#1221). A name rather than an implicit singleton row so
/// a second experiment can be scheduled later without renumbering this one —
/// and so a reader of `ab_control_counter` can tell what the row counts.
pub(in crate::memory) const AB_RECALL_EXPERIMENT: &str = "recall_suppression";

/// The name the per-artifact holdout's durable turn counter is filed under.
///
/// Its own row, not a share of [`AB_RECALL_EXPERIMENT`]'s: the two schedules
/// count different populations, since a plane-control turn advances the first
/// and is skipped by the second.
pub(in crate::memory) const ARTIFACT_HOLDOUT_EXPERIMENT: &str = "artifact_holdout";

impl SessionMemory {
    /// Arm this turn's A/B recall control at the workspace's configured rate
    /// (`context.retrieval.ab_recall_rate`), returning whether this turn is a
    /// control turn.
    ///
    /// **Every driver calls this once per turn, before it recalls anything.**
    /// The flag is turn-scoped but only *reset* here, so a driver that skips it
    /// inherits the previous turn's arm — and, more to the point, a driver that
    /// skips it produces no control turns at all, which is what made the
    /// measurement structurally impossible everywhere except the interactive
    /// REPL's plain prompts (#1221). It takes no rate argument for the same
    /// reason [`SessionMemory::open_for_session`] takes no "and also attach the
    /// records" flag: a per-driver copy of the rate is a per-driver way to get
    /// the schedule wrong.
    ///
    /// A "turn" here is one user-supplied prompt — not one internal round. The
    /// goal loop's rounds and the pipeline's stages all belong to the turn that
    /// armed them, which is also the unit the episode is recorded in, so the
    /// arm and its attribution describe the same thing.
    pub fn arm_recall_control(&mut self) -> bool {
        self.arm_controls(
            self.retrieval.ab_recall_rate,
            self.retrieval.artifact_holdout_rate,
        )
    }

    /// Arm both of this turn's controls, plane first, and report whether the
    /// plane one fired.
    ///
    /// Order is the whole point. The per-artifact holdout is skipped on a turn
    /// that injects nothing anyway, and whether this is one is not settled
    /// until the plane arm is.
    fn arm_controls(&mut self, recall_rate: u32, holdout_rate: u32) -> bool {
        // The join and the holdout pick belong to the turn that armed them.
        self.reset_context_trials();
        let suppressed = self.maybe_suppress_recall(recall_rate);
        self.arm_artifact_holdout(holdout_rate);
        suppressed
    }

    /// Arm this turn's per-artifact holdout
    /// (`context.retrieval.artifact_holdout_rate`).
    ///
    /// Its own durable counter, under its own experiment name, so the two
    /// schedules can be read apart afterwards.
    ///
    /// **A turn that injects nothing claims no number here.** Both counters
    /// advance once per turn, so two schedules counting every turn stay in
    /// lockstep: a plane rate of 10 and a holdout rate of 20 would land every
    /// holdout on a turn where the plane had already withheld everything, and
    /// the per-artifact arm would carry no measurement the plane arm did not
    /// already carry. Skipping instead means this counter advances only over
    /// turns the holdout could act on — which rules out a session with
    /// steering switched off for the same reason.
    ///
    /// Degrades exactly as [`Self::maybe_suppress_recall`] does: a store that
    /// cannot hand out a number falls back to the in-session tally, which on a
    /// one-turn process means no holdout. A lost holdout costs one sample;
    /// holding a skill back on a turn the schedule did not choose costs the
    /// user that skill for nothing.
    fn arm_artifact_holdout(&mut self, rate: u32) {
        self.holdout_ordinal = None;
        if rate <= 1 || self.injection_suppressed() {
            return;
        }
        self.holdout_turn = match self.store.next_ab_control_turn(ARTIFACT_HOLDOUT_EXPERIMENT) {
            Ok(turn) => turn,
            Err(_) => self.holdout_turn.wrapping_add(1),
        };
        self.holdout_ordinal = stella_learn::holdout::ordinal(self.holdout_turn, rate);
    }

    /// A/B recall control (Proposal 4): suppress recall for this turn on a
    /// deterministic `1/rate` schedule, returning whether recall was
    /// suppressed. A rate of 0 (or 1) never suppresses.
    ///
    /// The schedule is driven by a **turn counter**, not a wall clock. A
    /// previous implementation seeded off `SystemTime` nanoseconds and tested
    /// `ns % rate == 0`; on any host whose realtime clock is coarser than
    /// nanoseconds (macOS keeps it in microseconds, so `ns` is always a
    /// multiple of 1000) that predicate is true on *every* turn for any `rate`
    /// dividing 1000 — silently disabling recall entirely. A plain counter
    /// makes exactly every `rate`-th turn a control turn, on every OS.
    ///
    /// That counter is **durable** (#1221): it is claimed from the workspace's
    /// context store, so it survives the process that claimed it. A per-session
    /// counter cannot schedule anything on the surfaces that matter most —
    /// `stella run`, a fleet task and a `/goal` are one turn per process, so
    /// the session counter is 1 on every one of them and no control turn ever
    /// happens. Two processes against one workspace each claim a distinct
    /// number, so the arms interleave across surfaces instead of each surface
    /// running its own private schedule.
    ///
    /// A store that cannot hand out a number degrades to the in-session
    /// counter, which on a one-turn process means "not a control turn". That
    /// direction is deliberate: a lost control turn costs the experiment one
    /// sample, while suppressing recall on a turn the schedule did not choose
    /// costs the user their memory for no measurement at all.
    fn maybe_suppress_recall(&mut self, rate: u32) -> bool {
        if rate == 0 || rate == 1 {
            self.ab_suppressed = false;
            return false;
        }
        self.ab_turn = match self.store.next_ab_control_turn(AB_RECALL_EXPERIMENT) {
            Ok(turn) => turn,
            Err(_) => self.ab_turn.wrapping_add(1),
        };
        self.ab_suppressed = ab_control_turn(self.ab_turn, rate);
        self.ab_suppressed
    }

    /// Arm the control at an explicit rate, for tests that must not depend on
    /// a workspace's settings file. Production arms through
    /// [`Self::arm_recall_control`], which is the only door that reads the
    /// configured rate.
    #[cfg(test)]
    pub(crate) fn arm_recall_control_at(&mut self, rate: u32) -> bool {
        self.arm_controls(rate, 0)
    }

    /// Arm both controls at explicit rates, for the same reason
    /// [`Self::arm_recall_control_at`] exists. A test that wants only the
    /// per-artifact holdout passes `0` for the plane rate.
    #[cfg(test)]
    pub(crate) fn arm_controls_at(&mut self, recall_rate: u32, holdout_rate: u32) -> bool {
        self.arm_controls(recall_rate, holdout_rate)
    }

    /// Whether recall was suppressed this turn.
    ///
    /// Test-gated: outcome attribution used to read this from `agent.rs` and
    /// compose the `[ab-control]` tag itself, which is exactly the arrangement
    /// that left three of the four episode-writing surfaces untagged.
    /// [`SessionMemory::record_episode`] now reads the flag directly, so a
    /// production caller of this is a caller re-deriving attribution beside the
    /// one place that owns it. Drop the gate if a surface ever needs to *show*
    /// a control turn rather than record one.
    #[cfg(test)]
    pub(crate) fn recall_was_suppressed(&self) -> bool {
        self.ab_suppressed
    }
}

/// Is the `turn`-th turn (1-based) an A/B control turn at the given `rate`?
/// Every `rate`-th turn is a control turn; `rate` of 0 or 1 never controls.
/// Pure so the schedule is property-testable independent of the (heavy)
/// [`SessionMemory`] it lives on.
pub(in crate::memory) fn ab_control_turn(turn: u64, rate: u32) -> bool {
    stella_learn::holdout::is_scheduled(turn, rate)
}
