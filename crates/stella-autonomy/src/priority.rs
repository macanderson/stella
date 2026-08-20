//! The operator's priority vocabulary, and what the loop does with an issue
//! that has not been given one.
//!
//! `doc:backlog-self-driving` §3.2 (#3599). Two decisions live here, and both
//! used to be hardcoded in `rank_defects`.
//!
//! # The rungs are declared, not built in
//!
//! `["P0", "P1", "P2"]` was written into the ranking function. Every tracker
//! spells urgency differently — `Sev1`, `critical`, `now/next/later`, a numeric
//! field — and a loop that only understands one spelling can only be pointed at
//! one repository. [`PriorityLadder`] is the operator's list, most urgent
//! first, and the position in that list *is* the rank.
//!
//! # An issue with no rung is not the lowest rung
//!
//! This is the half that was wrong rather than merely inflexible. The old
//! `priority_rank` mapped "carries no priority label" to `3` — below `P2` and
//! indistinguishable from an issue somebody had deliberately ranked lowest. So
//! a P0 filed thirty seconds ago with no labels yet sorted beneath a P2 from
//! March, and the loop would work its way through the entire ranked backlog
//! before ever looking at it.
//!
//! [`rank_of`] returns `None` for that issue instead, and [`partition`]
//! separates it out. **Unranked is a question, not a position** — it means
//! nobody has judged this yet, and the answer is to judge it, which is what
//! [`Unassessed`] hands to the caller.

use crate::QueueIssue;

/// The operator's priority labels, most urgent first.
///
/// Position is rank: `rungs[0]` outranks `rungs[1]`. An empty ladder ranks
/// nothing, which makes every issue [`Unassessed`] rather than making them all
/// equal — a ladder nobody configured is a question nobody has answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorityLadder {
    /// The label names, most urgent first.
    pub rungs: Vec<String>,
}

impl Default for PriorityLadder {
    /// GitHub's common convention, which is also this repository's.
    ///
    /// A default exists so an operator who configures nothing still gets a
    /// working loop — the same reason the issue vocabulary ships GitHub's
    /// defaults rather than refusing to start.
    fn default() -> Self {
        Self {
            rungs: ["P0", "P1", "P2", "P3"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        }
    }
}

impl PriorityLadder {
    /// Build a ladder from declared rungs, most urgent first.
    #[must_use]
    pub fn new(rungs: Vec<String>) -> Self {
        Self { rungs }
    }

    /// Whether this ladder can rank anything at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rungs.is_empty()
    }

    /// The most urgent rung, for a caller that has to name one.
    #[must_use]
    pub fn most_urgent(&self) -> Option<&str> {
        self.rungs.first().map(String::as_str)
    }
}

/// Where an issue sits on the ladder, or `None` if nobody has said.
///
/// The first rung the issue carries wins, so an issue mislabelled with two
/// rungs is treated as the more urgent of them. That is the safe direction: a
/// contradiction should not be resolved by ignoring the alarm.
#[must_use]
pub fn rank_of(issue: &QueueIssue, ladder: &PriorityLadder) -> Option<u8> {
    ladder
        .rungs
        .iter()
        .position(|rung| issue.has_label(rung))
        .and_then(|index| u8::try_from(index).ok())
}

/// An issue nobody has placed on the ladder.
///
/// Carried as its own type rather than as a `QueueIssue` with a `None` rank so
/// a caller cannot accidentally sort it in among the ranked ones — which is
/// precisely the bug this module exists to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unassessed {
    /// The issue's key, as the tracker spells it.
    pub key: String,
    /// Its title, so a triage prompt has something to work with.
    pub title: String,
    /// When it was filed, so the oldest unassessed issue is assessed first.
    pub created_at: String,
}

/// The queue, split into what can be ranked and what must be judged first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Queue {
    /// Issues carrying a rung, most urgent first, oldest first within a rung.
    pub ranked: Vec<QueueIssue>,
    /// Issues carrying no rung, oldest first.
    ///
    /// **Not a tail of `ranked`.** These are unjudged, and the loop's response
    /// is to judge them — see the module docs.
    pub unassessed: Vec<Unassessed>,
}

impl Queue {
    /// Whether there is anything at all to do.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ranked.is_empty() && self.unassessed.is_empty()
    }
}

/// Split a queue into ranked work and issues awaiting a judgement.
///
/// Sorting is by rung, then by age within a rung, so a P0 filed today is taken
/// before a P0 filed last month and both are taken before any P1. Age is
/// compared as the tracker's own timestamp string, which is ISO-8601 and
/// therefore sorts correctly as text.
#[must_use]
pub fn partition(issues: Vec<QueueIssue>, ladder: &PriorityLadder) -> Queue {
    let mut ranked = Vec::new();
    let mut unassessed = Vec::new();

    for issue in issues {
        if rank_of(&issue, ladder).is_some() {
            ranked.push(issue);
        } else {
            unassessed.push(Unassessed {
                key: issue.number.to_string(),
                title: issue.title.clone(),
                created_at: issue.created_at.clone(),
            });
        }
    }

    ranked.sort_by(|a, b| {
        rank_of(a, ladder)
            .cmp(&rank_of(b, ladder))
            .then_with(|| a.created_at.cmp(&b.created_at))
    });
    unassessed.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    Queue { ranked, unassessed }
}

/// Everything the loop needs in order to place an issue, all of it declared.
///
/// Two axes, because an issue arrives needing a judgement on both: *is this
/// mine to work* and *how urgent is it*. A loop that only reads urgency will
/// claim a documentation request; one that only reads kind will take a
/// three-month-old `P3` ahead of this morning's `P0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriagePolicy {
    /// The urgency rungs, most urgent first.
    pub ladder: PriorityLadder,
    /// Labels meaning "this loop works this".
    pub defect_kinds: Vec<String>,
    /// Labels meaning "this loop does not work this".
    ///
    /// Named explicitly rather than inferred from "not a defect kind", because
    /// the two are different claims: an issue labelled `enhancement` has been
    /// judged and excluded, while an issue labelled only `P0` has been judged
    /// on one axis and not the other. Only the first may be dropped silently.
    pub excluded_kinds: Vec<String>,
}

impl Default for TriagePolicy {
    fn default() -> Self {
        Self {
            ladder: PriorityLadder::default(),
            defect_kinds: ["bug", "triage"].into_iter().map(str::to_owned).collect(),
            excluded_kinds: ["enhancement", "feature", "documentation", "question"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        }
    }
}

/// Split the open queue into work, questions, and things that are not ours.
///
/// The order of the checks is the whole content of this function:
///
/// 1. **Escalated** issues are dropped. The loop already tried and could not
///    resolve them; re-claiming spends the same money on the same wall. The
///    label is how that survives a restart, which process-local state cannot.
/// 2. **Excluded kinds** are dropped. Somebody judged this and said it is not
///    a defect.
/// 3. **A defect kind *and* a rung** is rankable work.
/// 4. **Everything else is a question**, not a low-priority answer — the
///    unlabelled issue filed a minute ago, and the issue carrying a rung but
///    no kind. See the module docs for what that mistake costs.
#[must_use]
pub fn triage(issues: Vec<QueueIssue>, policy: &TriagePolicy) -> Queue {
    let mut rankable = Vec::new();
    let mut queue = Queue::default();

    for issue in issues {
        if issue.has_label(crate::ESCALATION_LABEL) {
            continue;
        }
        if policy
            .excluded_kinds
            .iter()
            .any(|kind| issue.has_label(kind))
        {
            continue;
        }

        let is_defect = policy.defect_kinds.iter().any(|kind| issue.has_label(kind));
        if is_defect && rank_of(&issue, &policy.ladder).is_some() {
            rankable.push(issue);
        } else {
            queue.unassessed.push(Unassessed {
                key: issue.number.to_string(),
                title: issue.title.clone(),
                created_at: issue.created_at.clone(),
            });
        }
    }

    let ranked = partition(rankable, &policy.ladder);
    queue.ranked = ranked.ranked;
    queue
        .unassessed
        .sort_by(|a, b| a.created_at.cmp(&b.created_at));
    queue
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IssueLabel;

    fn issue(number: u64, created_at: &str, labels: &[&str]) -> QueueIssue {
        QueueIssue {
            number,
            title: format!("issue {number}"),
            created_at: created_at.to_owned(),
            labels: labels
                .iter()
                .map(|name| IssueLabel {
                    name: (*name).to_owned(),
                })
                .collect(),
            url: String::new(),
        }
    }

    /// An unlabelled issue is a question, not the bottom of the queue.
    ///
    /// The witness for this module's reason to exist. `priority_rank` mapped
    /// "no priority label" to `3`, one below `P2`, so a freshly filed issue
    /// nobody had triaged yet sorted underneath everything anyone had ever
    /// ranked — including issues deliberately marked as least urgent. A P0
    /// filed a minute ago would wait for the entire backlog.
    #[test]
    fn an_unlabelled_issue_is_unassessed_not_lowest() {
        let ladder = PriorityLadder::default();
        let queue = partition(
            vec![
                issue(1, "2026-03-01T00:00:00Z", &["P2"]),
                issue(2, "2026-08-19T00:00:00Z", &[]),
            ],
            &ladder,
        );

        assert_eq!(queue.ranked.len(), 1, "only the P2 can be ranked");
        assert_eq!(queue.ranked[0].number, 1);
        assert_eq!(
            queue.unassessed.len(),
            1,
            "the unlabelled one is a question"
        );
        assert_eq!(queue.unassessed[0].key, "2");
    }

    /// The rungs come from the operator, not from this crate.
    ///
    /// A tracker spelling urgency as `Sev1`/`Sev2` ranks correctly, and the
    /// built-in `P0` means nothing to it — proving the ladder is consulted
    /// rather than a hardcoded list being consulted first.
    #[test]
    fn the_ladder_is_the_operators_vocabulary() {
        let ladder = PriorityLadder::new(vec!["Sev1".into(), "Sev2".into()]);
        let queue = partition(
            vec![
                issue(1, "2026-01-01T00:00:00Z", &["Sev2"]),
                issue(2, "2026-01-01T00:00:00Z", &["Sev1"]),
                issue(3, "2026-01-01T00:00:00Z", &["P0"]),
            ],
            &ladder,
        );

        assert_eq!(
            queue.ranked.iter().map(|i| i.number).collect::<Vec<_>>(),
            vec![2, 1],
            "Sev1 outranks Sev2"
        );
        assert_eq!(
            queue.unassessed[0].key, "3",
            "`P0` is not in this operator's vocabulary, so it is unjudged"
        );
    }

    /// Within a rung, the older issue goes first.
    #[test]
    fn age_breaks_a_tie_within_a_rung() {
        let ladder = PriorityLadder::default();
        let queue = partition(
            vec![
                issue(1, "2026-08-19T00:00:00Z", &["P0"]),
                issue(2, "2026-01-01T00:00:00Z", &["P0"]),
            ],
            &ladder,
        );
        assert_eq!(
            queue.ranked.iter().map(|i| i.number).collect::<Vec<_>>(),
            vec![2, 1]
        );
    }

    /// Two rungs on one issue resolve to the more urgent.
    ///
    /// A contradiction is not resolved by ignoring the alarm.
    #[test]
    fn the_more_urgent_of_two_rungs_wins() {
        let ladder = PriorityLadder::default();
        assert_eq!(
            rank_of(&issue(1, "2026-01-01T00:00:00Z", &["P2", "P0"]), &ladder),
            Some(0)
        );
    }

    /// A brand-new unlabelled issue is a question the loop must answer.
    ///
    /// The behaviour this module was asked for: an issue arriving with no
    /// labels at all must reach the loop's attention as something to judge.
    /// Under `rank_defects` it was invisible — the kind filter dropped it
    /// before ranking ever ran, so a P0 nobody had labelled yet was not
    /// low-priority, it was *absent*.
    #[test]
    fn an_unlabelled_new_issue_is_a_question_not_an_omission() {
        let policy = TriagePolicy::default();
        let queue = triage(
            vec![
                issue(1, "2026-01-01T00:00:00Z", &["bug", "P2"]),
                issue(2, "2026-08-19T00:00:00Z", &[]),
            ],
            &policy,
        );

        assert_eq!(queue.ranked.len(), 1);
        assert_eq!(
            queue
                .unassessed
                .iter()
                .map(|u| u.key.as_str())
                .collect::<Vec<_>>(),
            vec!["2"],
            "the unlabelled issue must surface for judgement, not vanish"
        );
    }

    /// A rung without a kind is still only half-judged.
    #[test]
    fn a_rung_with_no_kind_is_unassessed() {
        let policy = TriagePolicy::default();
        let queue = triage(vec![issue(1, "2026-01-01T00:00:00Z", &["P0"])], &policy);
        assert!(queue.ranked.is_empty());
        assert_eq!(queue.unassessed[0].key, "1");
    }

    /// An issue somebody judged as not-a-defect is dropped, not queued.
    ///
    /// The distinction the `excluded_kinds` field exists for: this has been
    /// judged, so it is not a question — unlike the rung-without-a-kind above.
    #[test]
    fn a_judged_non_defect_is_dropped_rather_than_asked_about() {
        let policy = TriagePolicy::default();
        let queue = triage(
            vec![issue(1, "2026-01-01T00:00:00Z", &["enhancement", "P0"])],
            &policy,
        );
        assert!(queue.is_empty(), "somebody already answered this one");
    }

    /// An escalated issue leaves the queue entirely, on either axis.
    #[test]
    fn an_escalated_issue_is_not_re_asked() {
        let policy = TriagePolicy::default();
        let queue = triage(
            vec![
                issue(
                    1,
                    "2026-01-01T00:00:00Z",
                    &["bug", "P0", crate::ESCALATION_LABEL],
                ),
                issue(2, "2026-01-01T00:00:00Z", &[crate::ESCALATION_LABEL]),
            ],
            &policy,
        );
        assert!(
            queue.is_empty(),
            "an escalated issue must not come back as ranked work OR as a question"
        );
    }

    /// A ladder nobody configured judges nothing, rather than judging
    /// everything equal.
    #[test]
    fn an_empty_ladder_leaves_everything_unassessed() {
        let ladder = PriorityLadder::new(Vec::new());
        let queue = partition(vec![issue(1, "2026-01-01T00:00:00Z", &["P0"])], &ladder);
        assert!(queue.ranked.is_empty());
        assert_eq!(queue.unassessed.len(), 1);
    }
}
