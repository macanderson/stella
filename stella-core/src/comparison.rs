//! A/B comparison — the one report shape every stella measurement of "is this
//! variant better?" emits, folded from paired per-task trials.
//!
//! Two halves of the project need to answer that question and must not answer
//! it differently. The **offline** half is `loop-bench --compare`: run a fixed
//! task set under two model configurations and report the delta (#876). The
//! **live** half is shadow-first self-tuning: score a candidate overlay against
//! the incumbent from real receipts (#1065). A promotion gate reads one of them
//! — an eval-gated skill (#1067), an adapter (#836), a knob (#831) — and a gate
//! that reads two different report shapes is a gate with two different
//! standards. So the shape, the arithmetic, and the verdict vocabulary live
//! here, once, and both halves render the same struct.
//!
//! Like [`crate::self_tuning`] and [`crate::scoreboard`], this module does no
//! I/O. The caller runs the trials — a benchmark harness, a shadow execution, a
//! store query — and hands over owned [`ArmTrials`]. Everything below is a
//! deterministic function of that input: no clock, no randomness, every
//! ordering a stable key, so the same trials produce a byte-identical report on
//! every run. That is what makes "reproducible: same seed + same tasks + same
//! configs → same result" a property of the code rather than a hope.
//!
//! # Pairing is the honest part
//!
//! The most common way an A/B lies is comparing aggregates computed over
//! different task sets. If the candidate crashed on the two hardest tasks and
//! those trials simply vanish, its pass rate goes *up*. So [`compare`] first
//! intersects: a task counts only when **every** arm ran it. Everything else is
//! named in [`ComparisonReport::unpaired_tasks`] and counted in
//! [`ArmSummary::trials_excluded`], never silently dropped — a comparison that
//! discards evidence has to say how much.
//!
//! # The verdict is two independent bars
//!
//! A winner must clear both:
//!
//! 1. **A confident lift on the primary metric.** Per-trial scores are handed
//!    to [`crate::self_tuning::select_winner`], which applies the sample floor
//!    and the Welch-style significance test. There is exactly one such test in
//!    the workspace and this is it — the primary metric only decides *what a
//!    trial scores* ([`Metric::trial_score`]), never how much evidence is
//!    enough.
//! 2. **No regression past tolerance on any guard metric.** A candidate can win
//!    a pass-rate A/B by spending four times as much, or by taking twice as
//!    many turns. [`Guard`]s are how the operator says which of those are not
//!    acceptable prices, and a candidate that clears bar 1 but fails bar 2
//!    resolves to [`ComparisonVerdict::GuardBlocked`] — a named refusal
//!    carrying its evidence, never a silent non-promotion.
//!
//! # Reading the report
//!
//! ```
//! use stella_core::comparison::{ArmTrials, ComparisonConfig, Metric, TrialRecord, compare};
//! use stella_core::self_tuning::TaskOutcome;
//!
//! fn trial(task: &str, succeeded: bool) -> TrialRecord {
//!     TrialRecord {
//!         task: task.to_string(),
//!         outcome: TaskOutcome { succeeded, cost_usd: 0.1, tokens: 1_000, retries: 0 },
//!         turns: 4,
//!     }
//! }
//!
//! let arms = vec![
//!     ArmTrials {
//!         id: "base".into(),
//!         value: "glm-4.7-flash".into(),
//!         trials: (0..6).map(|_| trial("fix-git", false)).collect(),
//!     },
//!     ArmTrials {
//!         id: "candidate".into(),
//!         value: "glm-5.2".into(),
//!         trials: (0..6).map(|_| trial("fix-git", true)).collect(),
//!     },
//! ];
//! let report = compare(&arms, &ComparisonConfig::new("base", Metric::PassRate));
//! assert_eq!(report.verdict.winner(), Some("candidate"));
//! ```

mod arm;
mod guard;
mod metric;
mod report;
mod task;
mod trial;

pub use arm::ArmSummary;
pub use guard::{Guard, GuardFinding};
pub use metric::Metric;
pub use report::{
    COMPARISON_SCHEMA_VERSION, ComparisonConfig, ComparisonReport, ComparisonVerdict, LiftEvidence,
    compare,
};
pub use task::{Leader, TaskArmResult, TaskRow};
pub use trial::{ArmTrials, TrialRecord};

#[cfg(test)]
mod props;
#[cfg(test)]
mod tests;
