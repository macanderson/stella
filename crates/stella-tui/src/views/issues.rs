// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The ISSUES tab's heat sort and its linked-work text — SPEC 9.4.
//!
//! Heat is the code graph's coupling for the files an issue's linked work
//! touched, times how long the issue has sat. Both terms are read off data the
//! deck was handed: the coupling is an edge count in the [`GraphSnapshot`] the
//! driver queried out of `codegraph.db`, and the age is the tracker's own
//! stamp. Nothing here estimates, which is what earns [`HEAT_CAPTION`] — the
//! same caption over a local heuristic would be a claim the code cannot keep.
//!
//! Split from [`super::issues_tab`] the way [`super::graph`] is split from
//! [`super::graph_tab`]: a pure fold with no buffer and no terminal, of which
//! the tab draws a projection.

use crate::envelope::{IssueRow, LinkedWork};
use crate::graph::{GraphNode, GraphSnapshot};

/// What the tab prints under a heat-sorted backlog (SPEC 9.4).
pub const HEAT_CAPTION: &str = "from the graph, not vibes";

/// Milliseconds in a day.
const DAY_MS: u64 = 86_400_000;

/// The heat of one issue: the graph's coupling for every file its linked work
/// touched, times the issue's age in days.
///
/// `None` when the question cannot be asked — the issue has no linked work, no
/// graph has been loaded, or the loaded neighborhood holds none of the files
/// the work touched. An unanswerable question is not a zero, and the sort
/// below keeps those rows in the tracker's own order rather than ranking them
/// against a number nobody computed.
#[must_use]
pub fn heat(row: &IssueRow, graph: Option<&GraphSnapshot>, now_ms: u64) -> Option<u64> {
    let linked = row.linked.as_ref()?;
    let graph = graph?;
    let mut coupling: u64 = 0;
    let mut answered = false;
    for path in &linked.touched_files {
        if let Some(edges) = file_coupling(graph, path) {
            coupling += edges as u64;
            answered = true;
        }
    }
    if !answered {
        return None;
    }
    Some(coupling.saturating_mul(age_days(row.updated_at.as_deref(), now_ms)))
}

/// The graph's coupling for one root-relative file: how many edges touch its
/// node, in either direction.
///
/// Undirected for the reason [`super::graph::coupling`] states — blast radius
/// counts an edge that points *at* the file as surely as one it points out
/// with.
///
/// `None` when the loaded neighborhood holds no such file, which is a
/// different answer from zero: an unqueried file has no coupling to report,
/// and ranking it at the bottom would be a guess dressed as a measurement.
#[must_use]
pub fn file_coupling(graph: &GraphSnapshot, path: &str) -> Option<usize> {
    graph
        .nodes
        .iter()
        .position(|node| is_file(node, path))
        .map(|index| graph.degree(index))
}

/// Whether `node` is the file at `path`.
///
/// A file node carries its root-relative path in
/// [`location`](GraphNode::location) — `stella-cli`'s `agent::graph_view`
/// builds every one of them that way — while
/// [`label`](GraphNode::label) is a display name a producer may shorten to a
/// basename. Matching on the location is what keeps `src/a/mod.rs` and
/// `src/b/mod.rs` two files.
fn is_file(node: &GraphNode, path: &str) -> bool {
    node.kind == "file" && node.location.as_deref().unwrap_or(&node.label) == path
}

/// Days between the tracker's `updated` stamp and `now_ms`, floored at one.
///
/// One day rather than zero for an issue the tracker moved today: age is a
/// multiplier, and a zero would rank the most coupled file in the workspace
/// below an untouched one that happens to be a day older. An absent stamp gets
/// the same floor, so an issue whose tracker reports no timestamp is ordered
/// on its coupling alone instead of on a date nobody supplied.
#[must_use]
pub fn age_days(updated: Option<&str>, now_ms: u64) -> u64 {
    let today = now_ms / DAY_MS;
    updated
        .and_then(civil_days)
        .map(|then| today.saturating_sub(then))
        .unwrap_or(0)
        .max(1)
}

/// Days from 1970-01-01 to the `YYYY-MM-DD` prefix of `stamp`, or `None` when
/// it does not begin with a well-formed one.
///
/// Howard Hinnant's `days_from_civil` closed form
/// (<https://howardhinnant.github.io/date_algorithms.html>), so the one piece
/// of calendar arithmetic this crate does costs it no date dependency.
fn civil_days(stamp: &str) -> Option<u64> {
    let mut parts = stamp.get(..10)?.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // The era arithmetic shifts the year so that a leap day lands at the end.
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    u64::try_from(era * 146_097 + day_of_era - 719_468).ok()
}

/// Order `rows` by heat, heaviest first, and report whether any heat existed.
///
/// Rows the graph cannot speak for keep the tracker's order and sit below
/// every row that has heat: the tracker's order is a real answer, and
/// reshuffling it against a number that does not exist is exactly the vibes
/// [`HEAT_CAPTION`] denies. The sort is stable, so rows sharing a heat keep
/// that order too.
///
/// `false` means nothing moved — no row had a linked file the graph knows —
/// and the tab then draws no caption, because there is no heat sort to caption.
pub fn sort_by_heat(rows: &mut Vec<IssueRow>, graph: Option<&GraphSnapshot>, now_ms: u64) -> bool {
    let mut ranked: Vec<(Option<u64>, IssueRow)> = std::mem::take(rows)
        .into_iter()
        .map(|row| (heat(&row, graph, now_ms), row))
        .collect();
    let sorted = ranked.iter().any(|(heat, _)| heat.is_some());
    if sorted {
        // `None` orders below `Some` for `Option`, so reversing the key puts
        // the heaviest first and every unranked row last. `sort_by_key` is
        // stable, which is what keeps the tracker's order inside a tie.
        ranked.sort_by_key(|(heat, _)| std::cmp::Reverse(*heat));
    }
    rows.extend(ranked.into_iter().map(|(_, row)| row));
    sorted
}

/// The inline plan tag an in-progress row carries: `plan r3 · task 3 live`.
///
/// Empty when the link names no plan — a claim that opened a branch before the
/// first plan existed draws nothing here rather than a bare keyword.
#[must_use]
pub fn plan_tag(linked: &LinkedWork) -> String {
    if linked.plan.is_empty() {
        return String::new();
    }
    match linked.live_task {
        Some(task) => format!("plan {} · task {task} live", linked.plan),
        None => format!("plan {}", linked.plan),
    }
}

/// The detail pane's `linked` line without its keyword:
/// `plan r3 · 2/6 · branch fix/token-race · evidence 2 files · 4/4 tests`.
///
/// A clause whose fact is missing is dropped rather than printed empty: a link
/// that opened a branch before its first plan reads `branch …` alone, and one
/// whose evidence ledger is still empty says nothing about files or tests
/// instead of claiming zero of them.
#[must_use]
pub fn linked_summary(linked: &LinkedWork) -> String {
    let mut clauses: Vec<String> = Vec::new();
    if !linked.plan.is_empty() {
        clauses.push(format!("plan {}", linked.plan));
        if linked.tasks_total > 0 {
            clauses.push(format!("{}/{}", linked.tasks_done, linked.tasks_total));
        }
    }
    if !linked.branch.is_empty() {
        clauses.push(format!("branch {}", linked.branch));
    }
    let files = linked.touched_files.len();
    if files > 0 {
        clauses.push(format!(
            "evidence {files} file{}",
            if files == 1 { "" } else { "s" }
        ));
    }
    if linked.tests_total > 0 {
        clauses.push(format!(
            "{}/{} tests",
            linked.tests_passed, linked.tests_total
        ));
    }
    clauses.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::GraphEdge;

    /// A neighborhood holding two file nodes: `hot.rs` with three edges,
    /// `cold.rs` with one.
    fn snapshot() -> GraphSnapshot {
        let file = |path: &str| GraphNode {
            label: path.to_string(),
            kind: "file".into(),
            location: Some(path.to_string()),
        };
        GraphSnapshot {
            focus: "src/hot.rs".into(),
            nodes: vec![
                file("src/hot.rs"),
                file("src/cold.rs"),
                GraphNode {
                    label: "run_turn".into(),
                    kind: "function".into(),
                    location: Some("src/hot.rs:12".into()),
                },
                GraphNode {
                    label: "Engine".into(),
                    kind: "struct".into(),
                    location: Some("src/hot.rs:40".into()),
                },
            ],
            edges: vec![
                GraphEdge {
                    from: 0,
                    to: 2,
                    kind: "defines".into(),
                },
                GraphEdge {
                    from: 0,
                    to: 3,
                    kind: "defines".into(),
                },
                GraphEdge {
                    from: 1,
                    to: 0,
                    kind: "imports".into(),
                },
            ],
            files: vec!["src/hot.rs".into(), "src/cold.rs".into()],
            query_ms: None,
            query: None,
        }
    }

    fn linked(files: &[&str]) -> LinkedWork {
        LinkedWork {
            plan: "r3".into(),
            tasks_done: 2,
            tasks_total: 6,
            live_task: Some(3),
            branch: "fix/token-race".into(),
            touched_files: files.iter().map(|f| (*f).to_string()).collect(),
            tests_passed: 4,
            tests_total: 4,
        }
    }

    fn row(key: &str, updated: Option<&str>, files: Option<&[&str]>) -> IssueRow {
        IssueRow {
            key: key.into(),
            title: format!("title of {key}"),
            state: "open".into(),
            labels: Vec::new(),
            assignee: None,
            url: String::new(),
            updated_at: updated.map(str::to_string),
            linked: files.map(linked),
        }
    }

    /// 2026-02-13T00:00:00Z — thirty days after the stamps below.
    const NOW_MS: u64 = 1_770_940_800_000;

    #[test]
    fn coupling_counts_every_edge_touching_a_file_in_either_direction() {
        let snap = snapshot();
        // `src/hot.rs` defines two symbols and is imported once.
        assert_eq!(file_coupling(&snap, "src/hot.rs"), Some(3));
        assert_eq!(file_coupling(&snap, "src/cold.rs"), Some(1));
        // A file the loaded neighborhood never mentions has no coupling to
        // report, which is not the same statement as "no coupling".
        assert_eq!(file_coupling(&snap, "src/absent.rs"), None);
        // A symbol node is not its file, even though its location names one.
        assert_eq!(file_coupling(&snap, "src/hot.rs:12"), None);
    }

    #[test]
    fn heat_is_coupling_times_age_in_days() {
        let snap = snapshot();
        let hot = row("#1", Some("2026-01-14T09:30:00Z"), Some(&["src/hot.rs"]));
        assert_eq!(heat(&hot, Some(&snap), NOW_MS), Some(3 * 30));

        // Two touched files add their coupling before the age multiplies it.
        let both = row(
            "#2",
            Some("2026-01-14T09:30:00Z"),
            Some(&["src/hot.rs", "src/cold.rs"]),
        );
        assert_eq!(heat(&both, Some(&snap), NOW_MS), Some((3 + 1) * 30));
    }

    #[test]
    fn heat_is_unanswerable_without_a_link_a_graph_or_a_known_file() {
        let snap = snapshot();
        let unlinked = row("#1", Some("2026-01-14T09:30:00Z"), None);
        assert_eq!(heat(&unlinked, Some(&snap), NOW_MS), None);

        let linked = row("#2", Some("2026-01-14T09:30:00Z"), Some(&["src/hot.rs"]));
        assert_eq!(heat(&linked, None, NOW_MS), None, "no graph loaded");

        let elsewhere = row("#3", Some("2026-01-14T09:30:00Z"), Some(&["src/absent.rs"]));
        assert_eq!(heat(&elsewhere, Some(&snap), NOW_MS), None);
    }

    #[test]
    fn age_floors_at_a_day_so_a_fresh_issue_still_ranks_on_coupling() {
        // Moved today: zero days elapsed, and a zero multiplier would erase
        // the coupling term entirely.
        assert_eq!(age_days(Some("2026-02-13T00:00:00Z"), NOW_MS), 1);
        // Never stamped: the same floor, so the ordering is coupling alone.
        assert_eq!(age_days(None, NOW_MS), 1);
        // A stamp in the future is not a negative age.
        assert_eq!(age_days(Some("2030-01-01"), NOW_MS), 1);
        // Unparseable text is treated as no stamp at all.
        assert_eq!(age_days(Some("last tuesday"), NOW_MS), 1);
        assert_eq!(age_days(Some("2026-13-01T00:00:00Z"), NOW_MS), 1);

        assert_eq!(age_days(Some("2026-01-14T09:30:00Z"), NOW_MS), 30);
        assert_eq!(age_days(Some("2025-02-13T00:00:00Z"), NOW_MS), 365);
    }

    #[test]
    fn civil_days_matches_known_epoch_days() {
        assert_eq!(civil_days("1970-01-01"), Some(0));
        // 2000-03-01 crosses the century leap rule the era arithmetic exists
        // for: 2000 is a leap year, 1900 was not.
        assert_eq!(civil_days("2000-03-01"), Some(11017));
        assert_eq!(civil_days("2026-01-01"), Some(20454));
        // Before the epoch there is no non-negative day count to return.
        assert_eq!(civil_days("1969-12-31"), None);
        assert_eq!(civil_days("2026-01"), None);
    }

    #[test]
    fn the_sort_ranks_by_heat_and_leaves_unranked_rows_in_tracker_order() {
        let snap = snapshot();
        let mut rows = vec![
            row(
                "#cold",
                Some("2026-01-14T09:30:00Z"),
                Some(&["src/cold.rs"]),
            ),
            row("#none-a", Some("2020-01-01T00:00:00Z"), None),
            row("#hot", Some("2026-01-14T09:30:00Z"), Some(&["src/hot.rs"])),
            row("#none-b", Some("2019-01-01T00:00:00Z"), None),
        ];
        assert!(sort_by_heat(&mut rows, Some(&snap), NOW_MS));
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, vec!["#hot", "#cold", "#none-a", "#none-b"]);
    }

    #[test]
    fn a_backlog_the_graph_cannot_speak_for_keeps_its_order_and_reports_no_sort() {
        let snap = snapshot();
        let mut rows = vec![
            row("#3", Some("2020-01-01T00:00:00Z"), None),
            row("#1", Some("2026-01-14T09:30:00Z"), None),
            row("#2", None, Some(&["src/absent.rs"])),
        ];
        assert!(!sort_by_heat(&mut rows, Some(&snap), NOW_MS));
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, vec!["#3", "#1", "#2"]);
    }

    #[test]
    fn the_plan_tag_names_the_round_and_the_live_task() {
        let mut work = linked(&["src/hot.rs"]);
        assert_eq!(plan_tag(&work), "plan r3 · task 3 live");

        // Nothing running: the round alone, with no dangling separator.
        work.live_task = None;
        assert_eq!(plan_tag(&work), "plan r3");

        // A branch claimed before any plan draws no tag at all.
        work.plan = String::new();
        assert!(plan_tag(&work).is_empty());
    }

    #[test]
    fn the_linked_line_reads_plan_progress_branch_and_evidence() {
        let work = linked(&["src/hot.rs", "src/cold.rs"]);
        assert_eq!(
            linked_summary(&work),
            "plan r3 · 2/6 · branch fix/token-race · evidence 2 files · 4/4 tests"
        );
    }

    #[test]
    fn an_empty_evidence_ledger_says_nothing_rather_than_claiming_zero() {
        let work = LinkedWork {
            plan: String::new(),
            tasks_done: 0,
            tasks_total: 0,
            live_task: None,
            branch: "fix/token-race".into(),
            touched_files: Vec::new(),
            tests_passed: 0,
            tests_total: 0,
        };
        assert_eq!(linked_summary(&work), "branch fix/token-race");

        // One file is one file, not "1 files".
        let one = LinkedWork {
            touched_files: vec!["src/hot.rs".into()],
            ..work
        };
        assert_eq!(
            linked_summary(&one),
            "branch fix/token-race · evidence 1 file"
        );
    }
}
