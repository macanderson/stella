//! The optional things a caller bolts onto a [`Pipeline`] after construction,
//! and the rule that every engine the pipeline builds inherits them.
//!
//! Separate from the ports in [`PipelinePorts`] because these are not
//! capabilities the pipeline needs in order to run — it runs without any of
//! them. They are host-owned state that outlives a turn (a supervisor's pause
//! flag, the session's accumulated token-drift model), which is why each is a
//! borrow with the pipeline's own lifetime rather than an owned field: the
//! caller keeps it across turns and lends it in, mirroring how `BudgetGuard`
//! and `CalibrationMap` are owned above the engine rather than by it.
//!
//! Split out of `pipeline.rs` for the reason everything is: it is a
//! grandfathered god file closed to growth, and this is a coherent seam to
//! take with it.

use super::*;

impl<'a> Pipeline<'a> {
    /// Attach a boundary pause gate. Every engine the pipeline builds — the
    /// worker's execute/revise turns and the witness author's — parks at its
    /// step boundaries while the gate holds, and every management call
    /// (triage, verifier, guidance) parks before dispatch: the same safe
    /// boundary as budget aborts, never mid-tool.
    ///
    /// This is the seam that lets a supervisor's pause reach a
    /// pipeline-driven worker at all. Without it only the raw step-loop path
    /// held a gate, so `Fleet::pause_task` on a pipeline worker silently did
    /// nothing — the named follow-up in `fleet_cmd`.
    #[must_use]
    pub fn with_turn_gate(mut self, gate: &'a dyn stella_core::ports::TurnGate) -> Self {
        self.turn_gate = Some(gate);
        self
    }

    /// Attach the caller's token-drift calibration, so the engines this
    /// pipeline builds size their compaction budget against what this model's
    /// tokenizer actually charged rather than against the raw estimate
    /// (#1595).
    ///
    /// Five of `stella-cli`'s seven assembly sites seeded a `CalibrationMap`
    /// and handed it to the engine; the two that did not were the **default
    /// `stella run`** path and fleet workers, because a pipeline had no way to
    /// accept one. So the most-used path in the product was the one estimating
    /// worst — and not only by losing the seed: with no map at all the
    /// correction is inert for the whole run, so the drift those turns
    /// measured was never applied to them either.
    ///
    /// Borrowed rather than owned because `CalibrationMap` is deliberately not
    /// `Clone` — the caller owns it across turns and the engine reads it
    /// through `&self` (see its type doc). An owned field here would also cost
    /// `PipelineConfig` its `Clone`, which many call sites rely on.
    #[must_use]
    pub fn with_calibration(mut self, calibration: &'a CalibrationMap) -> Self {
        self.calibration = Some(calibration);
        self
    }

    /// Bolt this pipeline's attachments onto one freshly-built engine.
    ///
    /// Every engine the pipeline constructs goes through here, which is the
    /// point: the worker's turns, the best-of-N children and the witness
    /// author are all engines, and attaching to some but not others is how
    /// #1595 happened one level up. A new attachment is added once, here,
    /// rather than at each construction site.
    pub(super) fn attach<'e>(&'e self, mut engine: Engine<'e>) -> Engine<'e> {
        if let Some(gate) = self.turn_gate {
            engine = engine.with_gate(gate);
        }
        if let Some(calibration) = self.calibration {
            engine = engine.with_calibration(calibration);
        }
        engine
    }
}
