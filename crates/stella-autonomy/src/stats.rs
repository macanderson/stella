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
    /// Changes proved by running the project's own checks on this machine.
    ///
    /// Counted separately from anything the forge reported, because the two
    /// are different grades of evidence and a dashboard that merged them
    /// would hide which merges rested on which.
    pub verified_locally: u32,
    /// Filings attempted, however they ended.
    ///
    /// The denominator `issues_created`, `filings_refused` and
    /// `filings_duplicate` share, which is what makes
    /// [`SessionStats::filings_balance`] a check rather than an assumption.
    pub filings_attempted: u32,
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
    //
    // A turn runs in a child process, so these three are deltas over the
    // durable state it leaves behind rather than events this process saw. The
    // writer is `stella-cli`'s `self_driving_cmd::learning`, which names the
    // artifact each one is read from.
    /// Lessons written to the reflection log, restatements included — a lesson
    /// the loop keeps re-learning is the recurrence the miners exist to count.
    pub reflections_logged: u32,
    /// Memories created: distinct memory lineages the context store gained.
    ///
    /// Lower than `reflections_logged` by design — a restated lesson reaches
    /// the log and claims no memory.
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

    /// Whether the filing counts add up.
    ///
    /// The companion to [`SessionStats::closures_balance`], for the same
    /// reason: three outcomes rendered beside a total that disagrees with them
    /// is worse than either alone, because a reader believes whichever they
    /// read first.
    #[must_use]
    pub fn filings_balance(&self) -> bool {
        self.issues_created + self.filings_refused + self.filings_duplicate
            == self.filings_attempted
    }

    /// Record a filing by its canonical outcome.
    ///
    /// A `&str` rather than the caller's own enum, exactly as
    /// [`SessionStats::record_closure`]: this is a leaf crate, and the
    /// vocabulary of *why a filing did not happen* belongs to the surface that
    /// tried. An unrecognised outcome still counts toward the attempts, so
    /// [`SessionStats::filings_balance`] reports it rather than the number
    /// silently going missing.
    pub fn record_filing(&mut self, canonical: &str) {
        self.filings_attempted += 1;
        match canonical {
            "new" => self.issues_created += 1,
            "refused" => self.filings_refused += 1,
            "duplicate" => self.filings_duplicate += 1,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every counter, and the code that writes it.
    ///
    /// A counter with a printer and no producer renders `0` forever, and a
    /// reader cannot tell that from "the session did none of that" — five of
    /// them shipped that way (#4118). The compiler cannot catch it: a `pub`
    /// field that nothing assigns is neither dead code nor a warning.
    ///
    /// This is the discipline of invariant #10's consumer ledger
    /// (`stella_protocol::event::consumers`) pointed at production instead of
    /// consumption, and it enforces the same half: a field cannot be added
    /// without a line naming where it is written, which a reviewer reads. It
    /// does not prove the named site still exists — that string is prose. The
    /// test below proves totality, which is what makes the question
    /// unavoidable at the moment a field is added.
    const PRODUCERS: &[(&str, &str)] = &[
        ("issues_claimed", "drive.rs — taken off the ranked queue"),
        ("issues_attempted", "drive.rs — a turn was spent on it"),
        ("issues_changed", "drive.rs — WorkOutcome::Changed"),
        ("issues_no_change", "drive.rs — WorkOutcome::NoChange"),
        ("issues_failed", "drive.rs — WorkOutcome::Failed"),
        ("issues_escalated", "drive.rs — handed back to a human"),
        (
            "issues_deferred",
            "drive.rs — a peer had it, or it would not start",
        ),
        (
            "issues_created",
            "SessionStats::record_filing, from self_driving_cmd.rs",
        ),
        ("issues_triaged", "drive.rs — placed on the ladder"),
        (
            "verified_locally",
            "drive.rs — the project's own checks passed here",
        ),
        ("filings_attempted", "SessionStats::record_filing"),
        ("filings_refused", "SessionStats::record_filing"),
        ("filings_duplicate", "SessionStats::record_filing"),
        (
            "closed_total",
            "SessionStats::record_closure, from lifecycle.rs",
        ),
        ("closed_completed", "SessionStats::record_closure"),
        ("closed_not_planned", "SessionStats::record_closure"),
        ("closed_duplicate", "SessionStats::record_closure"),
        ("prs_opened", "drive.rs — delivery opened one"),
        ("prs_merged", "drive.rs — the forge reported a merge"),
        ("prs_escalated", "drive.rs — handed to a human instead"),
        ("fixes_pushed", "drive.rs — a red build was answered"),
        ("rebases", "drive.rs — a conflict was answered"),
        (
            "base_broken_waits",
            "drive.rs — the red reproduced on the base",
        ),
        (
            "reflections_logged",
            "self_driving_cmd/learning.rs — reflection-log delta",
        ),
        (
            "memories_created",
            "self_driving_cmd/learning.rs — memory-lineage delta",
        ),
        (
            "proposals_made",
            "self_driving_cmd/learning.rs — proposal-record delta",
        ),
        ("turns_run", "drive.rs — a turn reached the model"),
        (
            "turns_over_budget",
            "drive.rs — a turn hit its spend ceiling",
        ),
    ];

    /// **The witness.** Every field the report renders names the code that
    /// writes it. A counter added with a printer and no producer fails here
    /// instead of shipping a zero somebody reads as a fact.
    #[test]
    fn every_counter_names_its_producer() {
        let rendered = serde_json::to_value(SessionStats::default()).expect("serialize");
        let fields: std::collections::BTreeSet<String> = rendered
            .as_object()
            .expect("a struct serializes to an object")
            .keys()
            .cloned()
            .collect();
        let declared: std::collections::BTreeSet<String> = PRODUCERS
            .iter()
            .map(|(field, _)| (*field).to_owned())
            .collect();

        let undeclared: Vec<&String> = fields.difference(&declared).collect();
        assert!(
            undeclared.is_empty(),
            "these counters have a printer and no declared producer: {undeclared:?} \
             — name the code that writes each one in PRODUCERS, or delete the field"
        );
        let stale: Vec<&String> = declared.difference(&fields).collect();
        assert!(
            stale.is_empty(),
            "PRODUCERS names counters that no longer exist: {stale:?}"
        );
    }

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

    /// The three outcomes must sum to the attempts, and `record_filing` is
    /// what makes that true by construction. A refusal and a duplicate are
    /// real outcomes of a filing, not silences.
    #[test]
    fn recording_filings_keeps_the_counts_balanced() {
        let mut stats = SessionStats::default();
        stats.record_filing("new");
        stats.record_filing("refused");
        stats.record_filing("duplicate");
        stats.record_filing("duplicate");

        assert_eq!(stats.filings_attempted, 4);
        assert_eq!(stats.issues_created, 1);
        assert_eq!(stats.filings_refused, 1);
        assert_eq!(stats.filings_duplicate, 2);
        assert!(stats.filings_balance());
    }

    /// A filing outcome this build does not know still counts toward the
    /// attempts, so the balance check reports the gap rather than the number
    /// vanishing — the same contract `record_closure` holds.
    #[test]
    fn an_unknown_filing_outcome_is_visible_rather_than_lost() {
        let mut stats = SessionStats::default();
        stats.record_filing("rate_limited");

        assert_eq!(stats.filings_attempted, 1);
        assert!(
            !stats.filings_balance(),
            "an unrecognised filing outcome must show up as an imbalance, not disappear"
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
