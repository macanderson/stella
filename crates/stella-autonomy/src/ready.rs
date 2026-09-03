//! Readiness over a backlog: which issues the loop may take next.
//!
//! An issue is ready when it carries [`READY_LABEL`], or when every
//! `Blocked by: #N` line in its body names a closed issue.
//! One line per blocker, leading the line: `Blocked by: #4`.
//! Several blockers may share one line: `Blocked by: #4, #7`.
//! A reference outside the open set does not block. The issue it
//! names is closed, or it never existed. Neither holds work back.
//!
//! A tracking issue never enters the queue at all, whatever its
//! blockers say. [`ready_queue`] drops any item carrying a label in
//! [`DEFAULT_CONTAINER_LABELS`] (or the caller's own set), the same
//! way it drops an escalated one — see that function's docs.
//!
//! Pure over owned data, like the rest of this crate. The caller
//! reads the tracker once and hands in the open set. Nothing here
//! performs I/O.

use std::collections::BTreeSet;

use crate::QueueIssue;
use crate::escalation::EscalationPolicy;
use crate::priority::{PriorityLadder, by_age, rank_of};

/// The label a human applies to say: work this now, whatever its
/// `Blocked by:` lines say. That call outranks the parsed lines.
pub const READY_LABEL: &str = "status:ready";

/// Labels marking a tracking issue — a checklist of other issues, kept
/// open as bookkeeping rather than as work. [`ready_queue`] excludes any
/// item carrying one of these when the caller configures nothing else.
///
/// GitHub's common word for this shape is `epic`, so that is the one
/// default entry. An operator whose tracker spells it differently
/// (`tracking`, say) declares their own set instead of this one.
pub const DEFAULT_CONTAINER_LABELS: &[&str] = &["epic"];

/// One backlog issue with the blockers parsed out of its body.
#[derive(Debug, Clone, PartialEq)]
pub struct BacklogItem {
    /// The issue, in the ranker's shape.
    pub issue: QueueIssue,
    /// Issue numbers this one declares it is blocked by.
    pub blocked_by: Vec<u64>,
}

/// Whether one issue may be taken now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// No open blocker stands in the way, or a human marked it ready.
    Ready,
    /// These blockers are still open.
    WaitingOn(Vec<u64>),
}

/// Parse the `Blocked by: #N` lines out of an issue body.
///
/// A line counts when it starts with `blocked by`, in any case, once
/// markdown chrome (`-`, `*`, `>`, whitespace) is stripped. Every `#N`
/// on the rest of that line is a blocker. A body that says the same
/// blocker twice declares one blocker.
#[must_use]
pub fn blocker_refs(body: &str) -> Vec<u64> {
    let mut refs = Vec::new();
    for line in body.lines() {
        let stripped = line
            .trim_start_matches(|c: char| c == '-' || c == '>' || c.is_whitespace())
            .trim_start_matches('*');
        let lower = stripped.to_ascii_lowercase();
        if !lower.starts_with("blocked by") {
            continue;
        }
        let rest = &stripped["blocked by".len()..];
        let bytes = rest.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'#' {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end > start
                    && let Ok(number) = rest[start..end].parse::<u64>()
                {
                    refs.push(number);
                }
                i = end;
            } else {
                i += 1;
            }
        }
    }
    refs.sort_unstable();
    refs.dedup();
    refs
}

/// Whether this issue may be taken, given which issues are still open.
///
/// [`READY_LABEL`] wins over the parsed lines. The human who applied
/// it read the blockers and judged them stale. The loop must not
/// overrule that by re-parsing prose. A blocker naming the issue
/// itself is ignored — an issue cannot hold itself out of the queue.
#[must_use]
pub fn readiness(item: &BacklogItem, open: &BTreeSet<u64>) -> Readiness {
    if item.issue.has_label(READY_LABEL) {
        return Readiness::Ready;
    }
    let waiting: Vec<u64> = item
        .blocked_by
        .iter()
        .copied()
        .filter(|number| *number != item.issue.number && open.contains(number))
        .collect();
    if waiting.is_empty() {
        Readiness::Ready
    } else {
        Readiness::WaitingOn(waiting)
    }
}

/// The ready backlog, in the order the loop should take it.
///
/// An escalated issue is held back while its cooldown runs, and comes back
/// on its own once that is over — see [`crate::escalation`] for how long
/// each kind waits and when waiting ends. Nobody has to remove the label:
/// it stays as the marker a person reads. An escalated issue with no
/// record stays out, because nothing says what went wrong or when.
///
/// A tracking/container issue is dropped unconditionally —
/// `container_labels` names which labels mean that, and
/// [`DEFAULT_CONTAINER_LABELS`] is the answer when a caller has
/// configured nothing. That drop cannot be overridden by [`READY_LABEL`]:
/// it answers "is this actually work", not "may this be taken".
///
/// The rest is filtered to the ready and ordered. Issues with a rung come
/// first: most urgent rung, then oldest. The unranked follow, oldest
/// first. The defect queue holds unranked issues for triage; this one does
/// not. It drains a whole backlog, so work nobody ranked still ships in
/// the end.
#[must_use]
pub fn ready_queue(
    items: Vec<BacklogItem>,
    open: &BTreeSet<u64>,
    ladder: &PriorityLadder,
    container_labels: &[String],
    escalation: &EscalationPolicy,
    now_unix: i64,
) -> Vec<QueueIssue> {
    let mut ranked = Vec::new();
    let mut unranked = Vec::new();
    for item in items {
        if item.issue.escalation_holds(escalation, now_unix) {
            continue;
        }
        if container_labels
            .iter()
            .any(|label| item.issue.has_label(label))
        {
            continue;
        }
        if readiness(&item, open) != Readiness::Ready {
            continue;
        }
        if rank_of(&item.issue, ladder).is_some() {
            ranked.push(item.issue);
        } else {
            unranked.push(item.issue);
        }
    }
    ranked.sort_by(|a, b| {
        rank_of(a, ladder)
            .cmp(&rank_of(b, ladder))
            .then_with(|| by_age(&a.created_at, &b.created_at))
    });
    unranked.sort_by(|a, b| by_age(&a.created_at, &b.created_at));
    ranked.extend(unranked);
    ranked
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
            escalation: None,
        }
    }

    fn item(number: u64, labels: &[&str], blocked_by: &[u64]) -> BacklogItem {
        BacklogItem {
            issue: issue(number, "2026-08-01T00:00:00Z", labels),
            blocked_by: blocked_by.to_vec(),
        }
    }

    fn bare(issue: QueueIssue) -> BacklogItem {
        BacklogItem {
            issue,
            blocked_by: Vec::new(),
        }
    }

    fn escalated(number: u64, record: crate::escalation::EscalationRecord) -> BacklogItem {
        let mut issue = issue(
            number,
            "2026-08-01T00:00:00Z",
            &["P1", crate::ESCALATION_LABEL],
        );
        issue.escalation = Some(record);
        bare(issue)
    }

    fn queue(items: Vec<BacklogItem>, open: &BTreeSet<u64>, now_unix: i64) -> Vec<QueueIssue> {
        ready_queue(
            items,
            open,
            &PriorityLadder::default(),
            &[],
            &EscalationPolicy::default(),
            now_unix,
        )
    }

    fn open(numbers: &[u64]) -> BTreeSet<u64> {
        numbers.iter().copied().collect()
    }

    /// The selection witness. An issue whose `Blocked by:` line names
    /// an open issue waits. It is ready the moment that blocker closes.
    /// Nobody edits the body. Nobody applies a label.
    #[test]
    fn an_issue_blocked_by_an_open_issue_waits_and_a_closed_blocker_frees_it() {
        let blocked = item(41, &["feature", "P1"], &[40]);

        assert_eq!(
            readiness(&blocked, &open(&[40, 41])),
            Readiness::WaitingOn(vec![40]),
            "an open blocker must hold the issue out of the queue"
        );
        assert_eq!(
            readiness(&blocked, &open(&[41])),
            Readiness::Ready,
            "closing the blocker is the whole gesture; nothing else may be required"
        );
    }

    /// A human's `status:ready` label outranks the parsed lines. They
    /// read the blockers and judged them stale. Re-parsing prose must
    /// not overrule that.
    #[test]
    fn the_ready_label_marks_an_issue_ready_even_while_a_blocker_is_open() {
        let overridden = item(41, &["feature", READY_LABEL], &[40]);
        assert_eq!(readiness(&overridden, &open(&[40, 41])), Readiness::Ready);
    }

    /// The line format is greppable, and this is the grep. Plain,
    /// lower-case, bold, bulleted, and shared-line spellings all parse.
    /// Prose that merely mentions blocking does not.
    #[test]
    fn blocker_lines_follow_the_machine_greppable_convention() {
        assert_eq!(blocker_refs("Blocked by: #1"), vec![1]);
        assert_eq!(blocker_refs("blocked by #3, #4"), vec![3, 4]);
        assert_eq!(blocker_refs("**Blocked by:** #9"), vec![9]);
        assert_eq!(blocker_refs("- Blocked by: #7\n- Blocked by: #7"), vec![7]);
        assert_eq!(
            blocker_refs("This work is blocked by the outage in #5."),
            Vec::<u64>::new(),
            "a mid-sentence mention is prose, not a declaration"
        );
        assert_eq!(blocker_refs("Blocked by: nothing"), Vec::<u64>::new());
    }

    /// An issue cannot block itself by citing its own number.
    #[test]
    fn a_self_reference_does_not_block() {
        let looped = item(41, &["bug", "P1"], &[41]);
        assert_eq!(readiness(&looped, &open(&[41])), Readiness::Ready);
    }

    /// The queue order: rungs first, most urgent then oldest. The
    /// unranked follow, oldest first. This queue drains a whole
    /// backlog; it does not hold unranked issues for triage.
    #[test]
    fn the_ready_queue_ranks_by_rung_then_age_and_appends_the_unranked() {
        let items = vec![
            bare(issue(1, "2026-08-10T00:00:00Z", &["P1"])),
            bare(issue(2, "2026-08-01T00:00:00Z", &["P0"])),
            bare(issue(3, "2026-07-01T00:00:00Z", &[])),
            // Blocked by an open issue: not in the queue at all.
            BacklogItem {
                issue: issue(4, "2026-06-01T00:00:00Z", &["P0"]),
                blocked_by: vec![1],
            },
        ];

        let order: Vec<u64> = queue(items, &open(&[1, 2, 3, 4]), 0)
            .iter()
            .map(|i| i.number)
            .collect();
        assert_eq!(order, vec![2, 1, 3]);
    }

    /// An escalated issue with no record stays out. The label alone says
    /// nothing about what went wrong or when, and a person who applied it
    /// by hand meant it.
    #[test]
    fn an_escalated_issue_with_no_record_never_enters_the_ready_queue() {
        let items = vec![bare(issue(
            5,
            "2026-08-01T00:00:00Z",
            &["P0", crate::ESCALATION_LABEL],
        ))];
        assert!(queue(items, &open(&[5]), i64::MAX).is_empty());
    }

    /// **The cooldown witness.** An issue escalated because the machine
    /// broke is out of the queue while its cooldown runs and back in once
    /// it is over — with the label still on it. Nobody removed anything.
    #[test]
    fn an_environmental_escalation_leaves_the_queue_and_returns_when_its_cooldown_is_over() {
        let policy = EscalationPolicy::default();
        let escalated_at = 10_000_i64;
        let record = crate::escalation::next(
            None,
            crate::escalation::EscalationReason::Environmental(
                crate::escalation::EnvCause::StuckLoop,
            ),
            "2026-09-02T00:00:00Z",
            escalated_at,
        );
        let item = || escalated(17, record.clone());

        assert!(
            queue(vec![item()], &open(&[17]), escalated_at).is_empty(),
            "the cooldown has not run out, so the loop must not take it again yet"
        );

        let over = escalated_at + i64::try_from(policy.environmental_cooldown_secs).expect("fits");
        let back: Vec<u64> = queue(vec![item()], &open(&[17]), over)
            .iter()
            .map(|i| i.number)
            .collect();
        assert_eq!(
            back,
            vec![17],
            "an environmental abort must requeue on its own, with no label surgery"
        );
    }

    /// An issue escalated to its ceiling stays parked however long anyone
    /// waits. The cooldown has an end.
    #[test]
    fn an_issue_escalated_to_the_ceiling_stays_out_of_the_ready_queue() {
        let policy = EscalationPolicy::default();
        let spent = crate::escalation::EscalationRecord {
            attempts: policy.park_after,
            last_reason: crate::escalation::EscalationReason::Environmental(
                crate::escalation::EnvCause::ProviderError,
            ),
            last_at: "2026-09-02T00:00:00Z".to_owned(),
            last_at_unix: 0,
        };
        assert!(queue(vec![escalated(17, spent)], &open(&[17]), i64::MAX).is_empty());
    }

    /// The witness for this issue's fix: a tracking issue is absent from
    /// the ready queue even with no open blocker, because the loop
    /// re-checks the label after `readiness` says yes.
    ///
    /// `rainforest#2` reproduced the defect this guards: the loop claimed
    /// an epic the instant its last child closed, found nothing left to
    /// do under it, and re-built files a child issue had already merged.
    #[test]
    fn an_epic_with_no_open_blocker_is_absent_from_the_ready_queue() {
        let items = vec![bare(issue(2, "2026-08-01T00:00:00Z", &["P0", "epic"]))];
        let container_labels: Vec<String> = DEFAULT_CONTAINER_LABELS
            .iter()
            .map(|label| (*label).to_owned())
            .collect();

        assert!(
            ready_queue(
                items,
                &open(&[2]),
                &PriorityLadder::default(),
                &container_labels,
                &EscalationPolicy::default(),
                0,
            )
            .is_empty(),
            "an epic with every blocker closed is still not workable"
        );
    }

    /// A caller who configures no container labels at all gets none of
    /// this behaviour — the label set is a policy the caller states, not
    /// something the crate silently applies.
    #[test]
    fn an_empty_container_label_set_excludes_nothing() {
        let items = vec![bare(issue(2, "2026-08-01T00:00:00Z", &["P0", "epic"]))];

        let order: Vec<u64> = queue(items, &open(&[2]), 0)
            .iter()
            .map(|i| i.number)
            .collect();
        assert_eq!(order, vec![2]);
    }
}
