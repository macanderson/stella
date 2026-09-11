//! The binary's side of `stella-core`'s time ports. The engine exports only
//! the `Sleeper` and `Clock` traits; the real sources behind them are
//! `stella-time`'s, re-exported here under the names the rest of this crate
//! wires at construction. One-shot runs also arm their task deadline here.

pub use stella_time::{MonotonicClock, TokioSleeper, WallClock};

/// The budget guard for a one-shot invocation, with its wall-clock task
/// deadline armed (#1503).
///
/// A one-shot run is one task, so the `--turn-timeout` allowance the caller
/// resolved — for the bench adapters, Harbor's own per-task timeout minus a
/// teardown reserve (`bench/harbor_adapter`'s `turn_timeout.py`) — IS the
/// task's ceiling, and this is the one place the shipping binary computes
/// `Instant::now() + allowance`, the absolute point
/// `BudgetGuard::set_task_deadline` documents. #1481 built and proved the
/// stop-at-a-safe-boundary mechanism; until this call nothing armed it, so a
/// bench task ran into the harness's kill (`AgentTimeoutError`, work on disk
/// discarded) instead of stopping itself with a scorable partial.
///
/// Goal rounds re-enter the one-shot path per round, so each round gets the
/// same allowance — the flag's documented per-turn contract. The interactive
/// surfaces (chat, the deck) deliberately keep an unarmed deadline: their
/// sessions span many turns and no single wall-clock ceiling describes them.
///
/// **Repointed to the bare loop (#3868).** Its original call site was the
/// staged pipeline's one-shot driver (`run_pipeline_one_shot`), deleted with
/// the crate that drove it (#3865); the raw one-shot door
/// (`run_raw_one_shot`, `agent/goal.rs`) — the *only* one-shot door since
/// #3381 made it the default — called plain [`crate::agent::build_budget_guard`]
/// instead, so this deadline was unarmed on every one-shot run for as long as
/// raw had been the default, not only after that deletion. #2957 measured the
/// cost across 20 bench trials: 1,087 `budget_tick` events, zero carrying
/// `deadline_remaining_ms`, so every over-run died on the harness's SIGKILL
/// with its work discarded rather than stopping itself with a scorable
/// partial. The raw door now calls this, which is what makes that field
/// appear.
pub(crate) fn one_shot_budget_guard(
    budget_limit: Option<f64>,
    task_allowance: Option<std::time::Duration>,
) -> stella_core::budget::BudgetGuard {
    let mut guard = crate::agent::build_budget_guard(budget_limit);
    if let Some(allowance) = task_allowance {
        guard.set_task_deadline(Some(std::time::Instant::now() + allowance));
    }
    guard
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The #1503 witness: #1481's deadline mechanism was proven by a driver
    /// test but nothing in the shipping binary armed it — this is the arming,
    /// so a one-shot task stops itself at a safe boundary instead of running
    /// into the harness's kill.
    #[test]
    fn a_one_shot_guard_arms_the_task_deadline_from_its_allowance() {
        let allowance = std::time::Duration::from_secs(840);
        let before = std::time::Instant::now();
        let guard = one_shot_budget_guard(None, Some(allowance));
        let deadline = guard
            .task_deadline()
            .expect("an allowance arms the deadline");
        assert!(
            deadline >= before + allowance,
            "the deadline is the allowance measured from arming time"
        );
        // No allowance leaves the deadline unarmed — the advisory-only shape
        // every caller had before #1503, and the one chat/deck keep.
        assert!(one_shot_budget_guard(None, None).task_deadline().is_none());
        // The dollar axis is orthogonal: arming time must not change mode.
        let enforced = one_shot_budget_guard(Some(2.5), Some(allowance));
        assert_eq!(enforced.mode(), stella_protocol::BudgetMode::Enforced);
        assert!(enforced.task_deadline().is_some());
    }

    /// **The #3868 witness.** The test above proves the guard arms a deadline
    /// when someone calls it; this proves someone does. Between #3381 (raw
    /// became the default one-shot door) and this change, nothing did:
    /// `run_raw_one_shot` built a plain `build_budget_guard`, so
    /// `--turn-timeout` bounded only the engine's advisory `turn_budget`
    /// forecast and no enforced wall clock existed. #2957 measured the
    /// consequence — 1,087 `budget_tick` events over 20 bench trials, not one
    /// carrying `deadline_remaining_ms`.
    ///
    /// Asserted against the driver's source for the reason
    /// `every_recalling_driver_arms_the_control_first` (`memory/tests/ab_control.rs`)
    /// uses the same shape: reaching the arming any other way means standing up
    /// a provider, an MCP connect and a store to observe one constructor call.
    /// `one_shot_budget_guard` is a real `pub(crate)` item, so a rename that
    /// breaks this test breaks the compile too.
    #[test]
    fn the_raw_one_shot_door_arms_the_deadline_rather_than_building_a_bare_guard() {
        const GOAL: &str = include_str!("agent/goal.rs");

        let door = GOAL
            .find("pub(crate) async fn run_raw_one_shot")
            .expect("the raw one-shot door must still be named this");
        // The next door down the file bounds the body we are asserting about,
        // so a call belonging to the goal loop cannot satisfy this test.
        let next_door = GOAL[door..]
            .find("pub async fn run_goal_cmd")
            .map_or(GOAL.len(), |offset| door + offset);
        let body = &GOAL[door..next_door];

        assert!(
            body.contains("one_shot_budget_guard(budget_limit, cfg.turn_timeout)"),
            "run_raw_one_shot must arm the task deadline from --turn-timeout; \
             a bare build_budget_guard leaves every one-shot run to die on the \
             harness's kill with its work discarded (#3868)"
        );
    }
}
