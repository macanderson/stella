//! The backlog half of the self-driving verbs, read through the issue port.
//!
//! `doc:backlog-self-driving` B1. Split out of `self_driving_cmd.rs` rather
//! than added to it: that file is 1005 lines and #3599 records it as closed to
//! growth, with new logic landing in siblings here (AGENTS.md § *God files* —
//! plan around them, never into them).
//!
//! The parent keeps the two verb entry points, which name the concrete
//! provider. Everything below takes a `&dyn IssueProvider` and has never heard
//! of GitHub — which is what makes "GitHub is an adapter, not the answer" a
//! property a test can falsify rather than a claim about the code's shape.
//!
//! # Why these two verbs are one module
//!
//! `queue` and the governor's `demand` are the same read. Before B1 they were
//! three separate `gh` invocations carrying **two** definitions of the word
//! "defect" — `rank_defects`'s label filter, and a `--label bug` flag written
//! into `demand`'s argv — which could disagree with each other about the very
//! backlog one cycle drew its batch from. One read, one definition, folded two
//! ways.

use stella_autonomy::Demand;
use stella_protocol::issue::IssueProvider;

use crate::query_format::{QueryFormat, Rows};

use super::state::LoopState;

/// How many open issues cross the port to produce one cycle's batch.
///
/// A ceiling rather than "all of them": ranking is deterministic and a batch is
/// single digits, so reading a ten-thousand-issue backlog to pick five is spend
/// with no effect on the answer. 200 is what the shell driver asked `gh` for,
/// kept unchanged so the picked batch cannot move in the change that relocates
/// the read.
pub(super) const QUEUE_READ_LIMIT: usize = 200;

/// Read the tracker once and rank it, or explain why it could not be read.
///
/// The port is async because a tracker is a network service. These callers are
/// short-lived CLI verbs with no runtime of their own, so each blocks on one
/// current-thread runtime rather than making every self-driving verb async for
/// a single awaited call.
fn ranked(
    provider: &dyn IssueProvider,
) -> Result<(Vec<stella_autonomy::QueueIssue>, usize), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime for the issue provider: {error}"))?;
    let issues = runtime
        .block_on(provider.list_open(QUEUE_READ_LIMIT))
        .map_err(|error| error.to_string())?;
    let total = issues.len();
    let defects = stella_autonomy::rank_defects(
        issues
            .iter()
            .map(crate::issue_provider::to_queue_issue)
            .collect(),
    );
    Ok((defects, total))
}

/// The ranked defect batch this cycle draws from.
pub(super) fn render_queue(
    _st: &LoopState,
    provider: &dyn IssueProvider,
    limit: usize,
    format: QueryFormat,
) -> Result<(), String> {
    let (defects, total_issues) = ranked(provider)?;
    let picked = &defects[..limit.min(defects.len())];

    if format == QueryFormat::Json {
        println!(
            "{}",
            serde_json::to_string_pretty(&Rows::new(picked)).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    for i in picked {
        let prio = ["P0", "P1", "P2"]
            .into_iter()
            .find(|p| i.labels.iter().any(|l| l.name == *p))
            .unwrap_or("--");
        let area = i
            .labels
            .iter()
            .find(|l| l.name.starts_with("area:"))
            .map(|l| l.name.as_str())
            .unwrap_or("");
        println!("{prio:>2}  #{:<6} {area:<18} {}", i.number, i.title);
    }
    eprintln!(
        "\n{} of {} open defects ({} open issues total)",
        picked.len(),
        defects.len(),
        total_issues
    );
    Ok(())
}

/// Size the demand half of the governor from the same read.
///
/// A tracker that cannot be reached yields [`Demand::default`] rather than an
/// error: the governor's job is to size a cycle, and a cycle sized as though
/// the backlog were empty is a survivable answer where a refusal to plan is
/// not. That degradation is inherited from the `gh_available()` check this
/// replaces, not introduced here.
pub(super) fn demand_from(provider: &dyn IssueProvider) -> Demand {
    let Ok((defects, _)) = ranked(provider) else {
        return Demand::default();
    };
    let p0 = defects
        .iter()
        .filter(|issue| issue.labels.iter().any(|label| label.name == "P0"))
        .count();
    Demand {
        open_defects: u32::try_from(defects.len()).unwrap_or(u32::MAX),
        p0: u32::try_from(p0).unwrap_or(u32::MAX),
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use stella_protocol::issue::{Issue, IssueClass, IssueError, IssueKey, IssueLabel, IssueState};

    use super::*;

    /// A tracker that is not GitHub and is not a process.
    struct FixtureProvider(Vec<Issue>);

    #[async_trait]
    impl IssueProvider for FixtureProvider {
        fn id(&self) -> &str {
            "fixture"
        }

        async fn list_open(&self, limit: usize) -> Result<Vec<Issue>, IssueError> {
            Ok(self.0.iter().take(limit).cloned().collect())
        }
    }

    /// A tracker that is not reachable — the degradation path.
    struct DeadProvider;

    #[async_trait]
    impl IssueProvider for DeadProvider {
        fn id(&self) -> &str {
            "dead"
        }

        async fn list_open(&self, _limit: usize) -> Result<Vec<Issue>, IssueError> {
            Err(IssueError::Unavailable {
                provider: "dead".into(),
                reason: "no tracker here".into(),
            })
        }
    }

    fn issue(key: &str, labels: &[&str], created: &str) -> Issue {
        Issue {
            key: IssueKey::from(key),
            title: format!("issue {key}"),
            body: String::new(),
            state: IssueState::Open,
            class: IssueClass::Bug,
            labels: labels.iter().copied().map(IssueLabel::from).collect(),
            created_at: created.into(),
            url: String::new(),
            parent: None,
        }
    }

    fn backlog() -> FixtureProvider {
        FixtureProvider(vec![
            issue("7", &["bug", "P1"], "2026-08-19T00:00:00Z"),
            issue("9", &["triage"], "2026-08-01T00:00:00Z"),
            issue("3", &["bug", "P0"], "2026-08-18T00:00:00Z"),
            issue("5", &["bug", "P1"], "2026-08-02T00:00:00Z"),
            // Feature work is deliberately not a defect: this loop closes
            // defects, and a mixed batch is unreviewable.
            issue("11", &["feature", "P0"], "2026-07-01T00:00:00Z"),
        ])
    }

    /// **The B1 witness.** A ranked defect queue is produced end to end — read,
    /// mapped, ranked — from a provider that is not GitHub, holds no
    /// credential, and runs no subprocess.
    ///
    /// It cannot compile on `main`: there is no `IssueProvider` to implement
    /// there, because the queue read *is* the `gh` call. That is the property
    /// under test — not that ranking works, which it already did, but that
    /// ranking no longer requires GitHub to exist.
    #[test]
    fn a_ranked_queue_needs_no_github_at_all() {
        let (defects, total) = ranked(&backlog()).expect("fixture read");
        let order: Vec<u64> = defects.iter().map(|issue| issue.number).collect();
        assert_eq!(
            order,
            vec![3, 5, 7, 9],
            "P0 first, then P1 aged-before-fresh, then triage — and no feature"
        );
        assert_eq!(total, 5, "the total counts every open issue, defect or not");
    }

    /// The governor's two numbers are folds of that one ranking, so they cannot
    /// disagree with the batch the same cycle draws.
    #[test]
    fn demand_is_a_fold_of_the_same_ranking() {
        let demand = demand_from(&backlog());
        assert_eq!(demand.open_defects, 4, "the feature is not a defect");
        assert_eq!(demand.p0, 1, "and its P0 label does not make it one");
    }

    /// An unreachable tracker sizes a cycle as though the backlog were empty,
    /// rather than refusing to plan.
    #[test]
    fn an_unreachable_tracker_degrades_rather_than_failing() {
        assert_eq!(demand_from(&DeadProvider), Demand::default());
    }

    /// The limit bounds what crosses the port, not what the ranker discards
    /// afterwards.
    #[test]
    fn the_read_limit_bounds_the_crossing() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let got = runtime.block_on(backlog().list_open(2)).expect("read");
        assert_eq!(got.len(), 2);
    }
}
