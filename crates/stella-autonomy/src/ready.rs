//! Readiness over a backlog: which issues the delivery loop may take next.
//!
//! An issue is ready when it carries [`READY_LABEL`]. It is also ready when
//! every `Blocked by: #N` line in its body names a closed issue. One
//! greppable line per blocker, leading the line: `Blocked by: #4`. Blockers
//! may share one line: `Blocked by: #4, #7`. A blocker the open set lacks
//! does not block. That issue is closed, or it never was. Neither holds
//! work back.
//!
//! Pure over owned data, like the rest of this crate. The caller reads the
//! tracker once and hands in the open set. Nothing here performs I/O.

use std::collections::BTreeSet;

use crate::QueueIssue;
use crate::priority::{PriorityLadder, by_age, rank_of};

/// The label a human puts on an issue to say: work this now. It wins over
/// the `Blocked by:` lines. A person's call outranks parsed prose.
pub const READY_LABEL: &str = "status:ready";

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
/// A line counts when it starts with `blocked by`, in any case, once the
/// markdown chrome (`-`, `*`, `>`, whitespace) is stripped. Every `#N` on
/// the rest of that line is a blocker. Repeats are dropped: a body that
/// names one blocker twice declares one blocker.
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
/// [`READY_LABEL`] wins over the parsed lines. The human who applied it
/// read the blockers and judged them stale. The loop must not overrule
/// that by re-parsing prose. A blocker naming the issue itself is ignored.
/// An issue cannot hold itself out of the queue.
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

/// The ready backlog, in the order the delivery loop should take it.
///
/// Escalated issues are dropped. The loop already tried them, and the
/// label is how that survives a restart. The rest is cut down to the ready
/// and then sorted. Issues with a rung go first: top rung, then oldest.
/// Unranked issues follow, oldest first. The defect queue holds unranked
/// work back for triage. This queue does not. It drains a whole backlog,
/// so an unranked ready issue still ships once the ranked work is done.
#[must_use]
pub fn ready_queue(
    items: Vec<BacklogItem>,
    open: &BTreeSet<u64>,
    ladder: &PriorityLadder,
) -> Vec<QueueIssue> {
    let mut ranked = Vec::new();
    let mut unranked = Vec::new();
    for item in items {
        if item.issue.has_label(crate::ESCALATION_LABEL) {
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
        }
    }

    fn item(number: u64, labels: &[&str], blocked_by: &[u64]) -> BacklogItem {
        BacklogItem {
            issue: issue(number, "2026-08-01T00:00:00Z", labels),
            blocked_by: blocked_by.to_vec(),
        }
    }

    fn open(numbers: &[u64]) -> BTreeSet<u64> {
        numbers.iter().copied().collect()
    }

    /// The selection witness. An issue with an open blocker waits. The same
    /// issue is ready the moment that blocker closes. Nobody edits the
    /// body. Nobody applies a label.
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

    /// A human's `status:ready` label outranks the parsed lines. They read
    /// the blockers and judged them stale. Parsed prose must not overrule
    /// them.
    #[test]
    fn the_ready_label_marks_an_issue_ready_even_while_a_blocker_is_open() {
        let overridden = item(41, &["feature", READY_LABEL], &[40]);
        assert_eq!(readiness(&overridden, &open(&[40, 41])), Readiness::Ready);
    }

    /// The convention is greppable, and this is the grep. Plain, lower-case,
    /// bold, and bulleted spellings parse. So does a line with two refs.
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

    /// An issue cannot hold itself out of the queue by citing its own number.
    #[test]
    fn a_self_reference_does_not_block() {
        let looped = item(41, &["bug", "P1"], &[41]);
        assert_eq!(readiness(&looped, &open(&[41])), Readiness::Ready);
    }

    /// The queue order: rungs first, then age. Unranked issues come last,
    /// oldest first. This queue drains a whole backlog. It does not hold
    /// unranked issues back for triage.
    #[test]
    fn the_ready_queue_ranks_by_rung_then_age_and_appends_the_unranked() {
        let ladder = PriorityLadder::default();
        let items = vec![
            BacklogItem {
                issue: issue(1, "2026-08-10T00:00:00Z", &["P1"]),
                blocked_by: Vec::new(),
            },
            BacklogItem {
                issue: issue(2, "2026-08-01T00:00:00Z", &["P0"]),
                blocked_by: Vec::new(),
            },
            BacklogItem {
                issue: issue(3, "2026-07-01T00:00:00Z", &[]),
                blocked_by: Vec::new(),
            },
            // Blocked by an open issue: not in the queue at all.
            BacklogItem {
                issue: issue(4, "2026-06-01T00:00:00Z", &["P0"]),
                blocked_by: vec![1],
            },
        ];

        let order: Vec<u64> = ready_queue(items, &open(&[1, 2, 3, 4]), &ladder)
            .iter()
            .map(|i| i.number)
            .collect();
        assert_eq!(order, vec![2, 1, 3]);
    }

    /// An escalated issue never re-enters through the ready door.
    #[test]
    fn an_escalated_issue_never_enters_the_ready_queue() {
        let ladder = PriorityLadder::default();
        let items = vec![BacklogItem {
            issue: issue(5, "2026-08-01T00:00:00Z", &["P0", crate::ESCALATION_LABEL]),
            blocked_by: Vec::new(),
        }];
        assert!(ready_queue(items, &open(&[5]), &ladder).is_empty());
    }
}
