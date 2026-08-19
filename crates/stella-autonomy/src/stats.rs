//! What a session did, counted as it happens.
//!
//! A perpetual loop is judged on a record, not on a moment, and the record has
//! to be readable **while the loop is still running** — an operator watching a
//! dashboard wants to know whether the last hour was productive, not to wait
//! for a run that never ends to produce a summary.
//!
//! So the counters live here, in the leaf crate, and both readers share them:
//! `stella-cli` increments and writes, the Observatory reads. That is the
//! #1613 lesson applied before it can bite — the dashboard and the terminal
//! carried two `fold_runs` once, drifted, and disagreed about whether the loop
//! was `NOISY` for every odd cycle count.
//!
//! # Counts are facts; the interesting numbers are ratios
//!
//! A raw count answers "how busy was it", which is the question that flatters.
//! [`SessionStats`] therefore also derives the three ratios that answer
//! "was it *worth* it", and each is deliberately capable of reporting badly:
//!
//! - [`SessionStats::inflation_ratio`] — issues created per issue closed. A
//!   loop that sustains more than 1.0 is **losing ground**: it is filing faster
//!   than it is finishing, which `doc:backlog-self-driving` §4.4 names as the
//!   failure mode none of its mitigations is proven to prevent.
//! - [`SessionStats::attempt_yield`] — how many attempts produced a change.
//! - [`SessionStats::merge_rate`] — how many opened pull requests landed.
//!
//! A dashboard that showed only the counts would make a loop generating
//! twenty issues and closing three look like a productive week.

use serde::{Deserialize, Serialize};

/// Everything one driving session has done so far.
///
/// Every field is a monotonic count, so a reader that samples twice can
/// subtract to get a rate without the writer having to keep a window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionStats {
    // -- the backlog side ---------------------------------------------------
    /// Issues taken off the ranked queue.
    pub issues_claimed: u32,
    /// Issues a turn was actually spent on.
    ///
    /// Lower than `issues_claimed` whenever something was claimed and then
    /// could not be started — a peer already on it, a branch left by a dead
    /// run. The gap between the two is the contention story.
    pub issues_attempted: u32,
    /// Attempts that left a change on a branch.
    pub issues_changed: u32,
    /// Attempts whose turn ran and changed nothing.
    ///
    /// Not a failure: an issue that needed no change is a real answer.
    pub issues_no_change: u32,
    /// Attempts whose turn did not complete.
    pub issues_failed: u32,
    /// Issues marked `agent-escalated` — tried, unresolved, handed back.
    pub issues_escalated: u32,
    /// Issues skipped because something else appeared to be working them.
    pub issues_deferred: u32,

    // -- what the loop added to the backlog ---------------------------------
    /// Issues the loop filed. New work it discovered and wrote down.
    pub issues_created: u32,
    /// Issues the loop placed on the ladder itself, having found them
    /// unjudged. Counted separately from `issues_attempted` because triaging
    /// an issue is not working it — a dashboard that merged the two would
    /// report a labelling pass as delivery.
    pub issues_triaged: u32,
    /// Filings refused for not matching the workspace's convention.
    pub filings_refused: u32,
    /// Filings skipped because the finding was already in the seen set.
    pub filings_duplicate: u32,

    // -- what the loop removed from it --------------------------------------
    /// Issues closed, however they were resolved.
    pub closed_total: u32,
    /// Closed as completed — the work was done.
    pub closed_completed: u32,
    /// Closed as not planned — declined, stale, superseded.
    pub closed_not_planned: u32,
    /// Closed as a duplicate of another issue.
    pub closed_duplicate: u32,

    // -- delivery -----------------------------------------------------------
    /// Pull requests opened.
    pub prs_opened: u32,
    /// Pull requests merged.
    pub prs_merged: u32,
    /// Pull requests handed to a human rather than merged.
    pub prs_escalated: u32,
    /// Fix pushes made in response to a red build.
    pub fixes_pushed: u32,
    /// Rebases made in response to a conflict.
    pub rebases: u32,
    /// Times a red build was found to reproduce on the base branch.
    ///
    /// Worth counting separately: it is time the loop spent waiting on
    /// somebody else's breakage, and a high number is a fact about the
    /// repository rather than about the loop.
    pub base_broken_waits: u32,

    // -- what the loop learned ----------------------------------------------
    /// Reflections written to the reflection log.
    pub reflections_logged: u32,
    /// Memories created.
    pub memories_created: u32,
    /// Proposals emitted for a human to accept or decline.
    pub proposals_made: u32,

    // -- cost ---------------------------------------------------------------
    /// Turns run through the model.
    pub turns_run: u32,
    /// Turns that stopped because they hit their spend ceiling.
    pub turns_over_budget: u32,
}

impl SessionStats {
    /// Issues created per issue closed.
    ///
    /// `None` when nothing has been closed yet — a ratio against zero is not a
    /// large number, it is an undefined one, and rendering it as `∞` or `0`
    /// would both mislead a dashboard on a session's first minutes.
    ///
    /// **Above 1.0 sustained means the loop is losing ground**: filing faster
    /// than finishing. `doc:backlog-self-driving` §4.4 names this as the
    /// failure mode none of its mitigations is proven to prevent, which is
    /// exactly why it should be on the dashboard rather than in a report
    /// somebody reads later.
    #[must_use]
    pub fn inflation_ratio(&self) -> Option<f64> {
        (self.closed_total > 0)
            .then(|| f64::from(self.issues_created) / f64::from(self.closed_total))
    }

    /// The share of attempts that produced a change.
    ///
    /// `None` before anything has been attempted.
    #[must_use]
    pub fn attempt_yield(&self) -> Option<f64> {
        (self.issues_attempted > 0)
            .then(|| f64::from(self.issues_changed) / f64::from(self.issues_attempted))
    }

    /// The share of opened pull requests that merged.
    ///
    /// `None` before anything has been opened. Note this can only fall as a
    /// session runs — a pull request is opened before it can merge — so a low
    /// number early is not yet a signal.
    #[must_use]
    pub fn merge_rate(&self) -> Option<f64> {
        (self.prs_opened > 0).then(|| f64::from(self.prs_merged) / f64::from(self.prs_opened))
    }

    /// Whether the closure counts add up.
    ///
    /// A dashboard showing `closed_total` beside three kinds that sum to
    /// something else is worse than one showing neither, because a reader will
    /// believe whichever they looked at first. Checked rather than assumed,
    /// because the counts are incremented at different call sites.
    #[must_use]
    pub fn closures_balance(&self) -> bool {
        self.closed_completed + self.closed_not_planned + self.closed_duplicate == self.closed_total
    }

    /// Record a closure by its canonical resolution.
    ///
    /// One method rather than four fields a caller increments by hand: that is
    /// what keeps [`SessionStats::closures_balance`] true by construction
    /// instead of by discipline. An unrecognised resolution still counts
    /// toward the total, so the balance check catches it rather than the
    /// number silently going missing.
    pub fn record_closure(&mut self, canonical: &str) {
        self.closed_total += 1;
        match canonical {
            "completed" => self.closed_completed += 1,
            "not_planned" => self.closed_not_planned += 1,
            "duplicate" => self.closed_duplicate += 1,
            // Deliberately lands nowhere but the total, so `closures_balance`
            // reports false and a reader learns the vocabulary grew.
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The witness.** A loop filing faster than it finishes is losing ground,
    /// and the number that says so must be on the dashboard — the raw counts
    /// would make twenty created and three closed look like a productive week.
    #[test]
    fn inflation_above_one_means_the_backlog_is_growing() {
        let losing = SessionStats {
            issues_created: 20,
            closed_total: 3,
            closed_completed: 3,
            ..SessionStats::default()
        };
        assert!(losing.inflation_ratio().expect("closed something") > 1.0);

        let gaining = SessionStats {
            issues_created: 1,
            closed_total: 4,
            closed_completed: 4,
            ..SessionStats::default()
        };
        assert!(gaining.inflation_ratio().expect("closed something") < 1.0);
    }

    /// A ratio against zero is undefined, not large. Rendering it as `∞` or `0`
    /// would both mislead a dashboard in a session's first minutes.
    #[test]
    fn a_ratio_with_no_denominator_is_none_not_a_number() {
        let fresh = SessionStats::default();
        assert_eq!(fresh.inflation_ratio(), None);
        assert_eq!(fresh.attempt_yield(), None);
        assert_eq!(fresh.merge_rate(), None);
    }

    /// The three kinds must sum to the total, and `record_closure` is what
    /// makes that true by construction rather than by a caller remembering.
    #[test]
    fn recording_closures_keeps_the_counts_balanced() {
        let mut stats = SessionStats::default();
        stats.record_closure("completed");
        stats.record_closure("completed");
        stats.record_closure("not_planned");
        stats.record_closure("duplicate");

        assert_eq!(stats.closed_total, 4);
        assert_eq!(stats.closed_completed, 2);
        assert_eq!(stats.closed_not_planned, 1);
        assert_eq!(stats.closed_duplicate, 1);
        assert!(stats.closures_balance());
    }

    /// A resolution this build does not know still counts toward the total, so
    /// the balance check reports the gap rather than the number vanishing.
    #[test]
    fn an_unknown_resolution_is_visible_rather_than_lost() {
        let mut stats = SessionStats::default();
        stats.record_closure("cannot_reproduce");

        assert_eq!(stats.closed_total, 1);
        assert!(
            !stats.closures_balance(),
            "an unrecognised resolution must show up as an imbalance, not disappear"
        );
    }

    /// Claimed and attempted are different numbers, and the gap is the
    /// contention story — an issue taken off the queue that could not be
    /// started because somebody else was on it.
    #[test]
    fn the_gap_between_claimed_and_attempted_is_visible() {
        let stats = SessionStats {
            issues_claimed: 5,
            issues_attempted: 3,
            issues_deferred: 2,
            ..SessionStats::default()
        };
        assert_eq!(stats.issues_claimed - stats.issues_attempted, 2);
    }

    #[test]
    fn yields_are_shares_of_their_own_denominator() {
        let stats = SessionStats {
            issues_attempted: 4,
            issues_changed: 1,
            prs_opened: 2,
            prs_merged: 1,
            ..SessionStats::default()
        };
        assert!((stats.attempt_yield().expect("attempted") - 0.25).abs() < f64::EPSILON);
        assert!((stats.merge_rate().expect("opened") - 0.5).abs() < f64::EPSILON);
    }

    /// Serialized with every field defaulted, so a dashboard written against
    /// an older build reads a newer session's file without failing — and a
    /// newer dashboard reads an older file with the new counters at zero.
    #[test]
    fn stats_round_trip_and_tolerate_a_missing_field() {
        let stats = SessionStats {
            issues_created: 3,
            prs_merged: 1,
            ..SessionStats::default()
        };
        let json = serde_json::to_string(&stats).expect("serialize");
        let back: SessionStats = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(stats, back);

        let sparse: SessionStats =
            serde_json::from_str(r#"{"prs_merged":2}"#).expect("a partial document still reads");
        assert_eq!(sparse.prs_merged, 2);
        assert_eq!(sparse.issues_created, 0);
    }
}
