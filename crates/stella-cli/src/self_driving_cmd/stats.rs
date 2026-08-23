//! `stats` — what a session has done so far: recorded from a work unit's
//! outcome, and rendered for a person.
//!
//! The counters themselves are [`stella_autonomy::SessionStats`], shared with
//! the Observatory so the dashboard and the terminal cannot disagree.
//!
//! Split out of `self_driving_cmd.rs` rather than added to it: that file is
//! within a couple of hundred lines of the 1500-line ceiling `make file-size`
//! enforces, and it carries no entry in `scripts/file-size-baseline.txt` — so
//! a crossing fails the gate outright rather than being grandfathered
//! (AGENTS.md § *God files* — plan around them, never into them).

use crate::query_format::QueryFormat;

use super::learning::LearningTally;
use super::state::LoopState;
use super::work::WorkOutcome;

/// Fold one completed work unit into the session's counters.
///
/// # Why both callers come through here
///
/// There are two ways to run a work unit — `stella self-driving drive`'s Work
/// arm and the one-shot `stella self-driving work` verb — and only the first
/// used to move a counter. The second took a [`LoopState`], threaded it
/// through, and discarded it, so a driver sweeping issues through `work` saw
/// `turns_run 0` and `issues_changed 0` however much work happened, while
/// `stella self-driving stats` and the Observatory dashboard both read that
/// file (#4306).
///
/// Writing the decision once, here, is what keeps the two from drifting
/// again — the #1613 lesson this module's other half already carries. It is
/// also one atomic write per unit rather than two, so a dashboard sampling
/// mid-unit can no longer read "attempted 1, changed 0" and believe it.
///
/// # What a work unit is *not* allowed to move
///
/// Only what the unit itself did. `issues_claimed`, `issues_deferred`,
/// `prs_opened`, `prs_merged`, `prs_escalated` and `verified_locally` belong
/// to the surrounding loop — the queue walk, the delivery arm, the local
/// verifier — and none of them happens here. The one-shot verb reaches none
/// of those stages at all, so counting them for it would report a queue walk
/// and a pull request that never existed. `issues_escalated` is likewise the
/// caller's: `drive` escalates a failed issue and the `work` verb hands the
/// failure straight back to whoever typed the command.
pub(super) fn record_work(st: &LoopState, learned: LearningTally, outcome: &WorkOutcome) {
    st.update_stats(|s| {
        // Both, for every outcome: the turn ran and the issue was attempted
        // whatever the turn then left behind. A one-shot invocation is a
        // session of one unit, not a unit outside every session — the `file`
        // verb already counts its filings the same way.
        s.issues_attempted += 1;
        s.turns_run += 1;
        learned.add_to(s);

        match outcome {
            WorkOutcome::Changed { .. } => s.issues_changed += 1,
            WorkOutcome::NoChange { .. } => s.issues_no_change += 1,
            WorkOutcome::Failed { reason } => {
                s.issues_failed += 1;
                // A turn stopped by its ceiling is a budget fact, not a sign
                // the issue is unworkable; conflating them would make a
                // too-small allowance look like a hard backlog.
                if reason.contains("budget exceeded") {
                    s.turns_over_budget += 1;
                }
            }
        }
    });
}

/// `stella self-driving stats` — the session's counters.
///
/// Renders the ratios beside the counts, deliberately. A raw count answers
/// "how busy was it", which is the question that flatters; the ratios answer
/// "was it worth it", and each can report badly. A dashboard showing only
/// counts would make a loop that created twenty issues and closed three look
/// like a productive week.
pub(super) fn session_stats(st: &LoopState, format: QueryFormat) -> Result<(), String> {
    let stats = st.stats();

    if format == QueryFormat::Json {
        println!(
            "{}",
            serde_json::to_string(&stats).map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    // `--` rather than `0.00` for a ratio with no denominator: a ratio against
    // zero is undefined, not small, and printing a number would tell a reader
    // something the session has not yet established.
    let ratio = |value: Option<f64>| value.map_or_else(|| "  --".to_owned(), |v| format!("{v:.2}"));

    println!("backlog");
    println!("  claimed         {:>5}", stats.issues_claimed);
    println!("  attempted       {:>5}", stats.issues_attempted);
    println!("  changed         {:>5}", stats.issues_changed);
    println!("  no change       {:>5}", stats.issues_no_change);
    println!("  failed          {:>5}", stats.issues_failed);
    println!("  escalated       {:>5}", stats.issues_escalated);
    println!("  deferred        {:>5}", stats.issues_deferred);

    println!("\nfiled");
    println!("  attempted       {:>5}", stats.filings_attempted);
    println!("  created         {:>5}", stats.issues_created);
    println!("  refused         {:>5}", stats.filings_refused);
    println!("  duplicate       {:>5}", stats.filings_duplicate);
    if !stats.filings_balance() {
        eprintln!(
            "  warning: the three outcomes do not sum to the attempts — a filing \
             outcome this build does not recognise was recorded"
        );
    }

    println!("\nclosed");
    println!("  total           {:>5}", stats.closed_total);
    println!("    completed     {:>5}", stats.closed_completed);
    println!("    not planned   {:>5}", stats.closed_not_planned);
    println!("    duplicate     {:>5}", stats.closed_duplicate);
    if !stats.closures_balance() {
        eprintln!(
            "  warning: the three kinds do not sum to the total — a resolution \
             this build does not recognise was recorded"
        );
    }

    println!("\ndelivery");
    println!("  prs opened      {:>5}", stats.prs_opened);
    println!("  prs merged      {:>5}", stats.prs_merged);
    println!("  prs escalated   {:>5}", stats.prs_escalated);
    println!("  fixes pushed    {:>5}", stats.fixes_pushed);
    println!("  rebases         {:>5}", stats.rebases);
    println!("  base-broken     {:>5}", stats.base_broken_waits);

    println!("\nlearning");
    println!("  reflections     {:>5}", stats.reflections_logged);
    println!("  memories        {:>5}", stats.memories_created);
    println!("  proposals       {:>5}", stats.proposals_made);

    println!("\ncost");
    println!("  turns run       {:>5}", stats.turns_run);
    println!("  over budget     {:>5}", stats.turns_over_budget);

    println!("\nyield");
    println!(
        "  inflation       {:>5}   created per closed; above 1.00 is losing ground",
        ratio(stats.inflation_ratio())
    );
    println!(
        "  attempt yield   {:>5}   attempts that changed something",
        ratio(stats.attempt_yield())
    );
    println!(
        "  merge rate      {:>5}   opened prs that landed",
        ratio(stats.merge_rate())
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A state directory on a tempdir. Both fields are `pub`, so a test can
    /// build one without `LoopState::open`'s home resolution and migration.
    fn loop_state(dir: &tempfile::TempDir) -> LoopState {
        LoopState {
            dir: dir.path().to_path_buf(),
            repo_root: dir.path().to_path_buf(),
        }
    }

    fn changed() -> WorkOutcome {
        WorkOutcome::Changed {
            branch: "stella/4306".to_owned(),
            stat: "1 file changed".to_owned(),
            path: std::path::PathBuf::from("/tmp/worktree-4306"),
        }
    }

    /// **Witness (#4306).** A completed work unit moves the session's
    /// counters.
    ///
    /// This is the recording path the one-shot `work` verb reaches. It used
    /// to be a literal `let _ = st;` — the `LoopState` was taken, threaded
    /// through and discarded — so every counter below stayed 0 however much
    /// work happened, and `stella self-driving stats` and the Observatory
    /// dashboard both read that file.
    #[test]
    fn a_completed_work_unit_moves_the_session_counters() {
        let dir = tempfile::tempdir().expect("tempdir");
        let st = loop_state(&dir);
        assert_eq!(st.stats(), stella_autonomy::SessionStats::default());

        record_work(
            &st,
            LearningTally {
                reflections: 3,
                memories: 1,
                proposals: 2,
            },
            &changed(),
        );

        let stats = st.stats();
        assert_eq!(stats.issues_attempted, 1);
        assert_eq!(stats.turns_run, 1);
        assert_eq!(stats.issues_changed, 1);
        assert_eq!(stats.reflections_logged, 3);
        assert_eq!(stats.memories_created, 1);
        assert_eq!(stats.proposals_made, 2);
    }

    /// The three outcomes are three different counters, and the two that are
    /// not `Changed` are still attempts.
    ///
    /// Asserted together because the failure this guards against is one arm
    /// being folded into another — a turn that changed nothing is a real
    /// answer, not a failure, and `attempt_yield` is only meaningful while
    /// the two stay apart.
    #[test]
    fn each_outcome_moves_its_own_counter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let st = loop_state(&dir);

        record_work(&st, LearningTally::default(), &changed());
        record_work(
            &st,
            LearningTally::default(),
            &WorkOutcome::NoChange {
                why: "nothing to do".to_owned(),
            },
        );
        record_work(
            &st,
            LearningTally::default(),
            &WorkOutcome::Failed {
                reason: "the turn exited 1".to_owned(),
            },
        );

        let stats = st.stats();
        assert_eq!(stats.issues_attempted, 3, "every outcome is an attempt");
        assert_eq!(stats.turns_run, 3);
        assert_eq!(stats.issues_changed, 1);
        assert_eq!(stats.issues_no_change, 1);
        assert_eq!(stats.issues_failed, 1);
        assert_eq!(
            stats.turns_over_budget, 0,
            "an ordinary failure is not a budget failure"
        );
    }

    /// A turn stopped by its ceiling is counted as both a failure and a
    /// budget fact, so a too-small allowance cannot read as a hard backlog.
    #[test]
    fn a_budget_stop_is_counted_as_a_budget_stop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let st = loop_state(&dir);

        record_work(
            &st,
            LearningTally::default(),
            &WorkOutcome::Failed {
                reason: "the turn exited 2 — budget exceeded".to_owned(),
            },
        );

        let stats = st.stats();
        assert_eq!(stats.issues_failed, 1);
        assert_eq!(stats.turns_over_budget, 1);
    }

    /// The counters that belong to the surrounding loop stay where they are.
    ///
    /// Without this the previous tests would pass just as well on a
    /// `record_work` that incremented everything it could reach, and the
    /// one-shot verb would report a queue walk and a pull request that never
    /// happened.
    #[test]
    fn a_work_unit_does_not_move_the_loops_own_counters() {
        let dir = tempfile::tempdir().expect("tempdir");
        let st = loop_state(&dir);

        record_work(&st, LearningTally::default(), &changed());

        let stats = st.stats();
        assert_eq!(stats.issues_claimed, 0);
        assert_eq!(stats.issues_deferred, 0);
        assert_eq!(stats.issues_escalated, 0);
        assert_eq!(stats.verified_locally, 0);
        assert_eq!(stats.prs_opened, 0);
    }
}
