//! The backlog-seeded work generator behind `drive --backlog`, plus the
//! dry-run report.
//!
//! The defect queue takes only what triage classified as a defect. This
//! generator drains a whole backlog in readiness order. See
//! `stella_autonomy::ready` for what ready means and how `Blocked by:`
//! lines are read. New logic lands here, beside `self_driving_cmd.rs`,
//! never inside it.

use std::collections::BTreeSet;

use stella_autonomy::CycleRecord;
use stella_autonomy::priority::PriorityLadder;
use stella_protocol::issue::IssueProvider;

use crate::timefmt::{now_unix, rfc3339_utc_now};

use super::config::LoopConfig;
use super::state::LoopState;

/// One read of the tracker, folded into the ready backlog.
///
/// The open set and the items come from the same read. An issue and its
/// blocker are judged against one moment. Two separate reads could see a
/// blocker close between them and disagree with each other.
///
/// `stella_autonomy::ready`'s readiness rule trusts that the open set it
/// gets is complete. A blocker missing from that set reads as closed. A
/// read that lands exactly on `QUEUE_READ_LIMIT` cannot promise that. The
/// tracker may hold more open issues than the page carried, and one of
/// them could be the blocker a dependent issue names. Folding a cut-off
/// page into `open` would let that dependent issue read as ready when its
/// blocker is only off the page. So a read at the ceiling fails with an
/// error instead of guessing.
fn ready_issues(
    provider: &dyn IssueProvider,
    ladder: &PriorityLadder,
) -> Result<Vec<stella_autonomy::QueueIssue>, String> {
    let issues = open_page(provider)?;
    Ok(fold_ready(&issues, ladder))
}

/// The ready backlog with the tracker's full records attached, in the order
/// the loop should take them.
///
/// The parallel wave needs whole issues, because a worker's prompt quotes
/// the body. The serial loop's queue wants bare keys instead, and looks
/// each one up again later. Same single read here, same readiness fold.
/// The ready numbers are then joined back to the records of the read that
/// judged them. So a wave can never pair one moment's readiness with
/// another moment's body.
pub(super) fn ready_full(
    provider: &dyn IssueProvider,
    ladder: &PriorityLadder,
) -> Result<Vec<stella_protocol::issue::Issue>, String> {
    let issues = open_page(provider)?;
    let ready = fold_ready(&issues, ladder);
    Ok(ready
        .iter()
        .filter_map(|queued| {
            issues
                .iter()
                .find(|issue| issue.key.as_str() == queued.number.to_string())
                .cloned()
        })
        .collect())
}

/// One bounded read of the open set — the truncation refusal above.
fn open_page(provider: &dyn IssueProvider) -> Result<Vec<stella_protocol::issue::Issue>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime for the issue provider: {error}"))?;
    let issues = runtime
        .block_on(provider.list_open(super::backlog::QUEUE_READ_LIMIT))
        .map_err(|error| error.to_string())?;
    if issues.len() >= super::backlog::QUEUE_READ_LIMIT {
        return Err(format!(
            "the open backlog has at least {} issues, at the {} the tracker read is bounded to \
             — readiness cannot be judged safely against a possibly-truncated open set, because \
             a blocker just off the page would misread as closed",
            issues.len(),
            super::backlog::QUEUE_READ_LIMIT,
        ));
    }
    Ok(issues)
}

/// The readiness fold over one read's records.
fn fold_ready(
    issues: &[stella_protocol::issue::Issue],
    ladder: &PriorityLadder,
) -> Vec<stella_autonomy::QueueIssue> {
    let open: BTreeSet<u64> = issues
        .iter()
        .filter_map(|issue| issue.key.as_str().parse().ok())
        .collect();
    let items = issues
        .iter()
        .map(|issue| stella_autonomy::ready::BacklogItem {
            issue: crate::issue_provider::to_queue_issue(issue),
            blocked_by: stella_autonomy::ready::blocker_refs(&issue.body),
        })
        .collect();
    stella_autonomy::ready::ready_queue(items, &open, ladder)
}

/// The ready backlog as bare keys, in the order the loop should take them.
///
/// The backlog twin of `backlog::ranked_keys`. Same read ceiling, same
/// shape, but a different rule for "next": readiness instead of defect
/// rank.
pub(super) fn ready_keys(
    provider: &dyn IssueProvider,
    ladder: &PriorityLadder,
) -> Result<Vec<String>, String> {
    Ok(ready_issues(provider, ladder)?
        .into_iter()
        .map(|issue| issue.number.to_string())
        .collect())
}

/// Report what `drive` would take next, and take nothing.
///
/// One tracker read, and one printed answer. No claim, no branch, no
/// label, no session record. A caller checking the loop's aim must not
/// move its hand.
pub(super) fn dry_run(
    provider: &dyn IssueProvider,
    cfg: &LoopConfig,
    backlog: bool,
    bound: u32,
) -> Result<(), String> {
    let queue = if backlog {
        ready_issues(provider, &cfg.triage.ladder)?
    } else {
        super::backlog::ranked(provider, &cfg.triage)?.0.ranked
    };

    println!("dry run — nothing was claimed, branched, filed, or labelled");
    println!(
        "  generator  {}",
        if backlog {
            "backlog (ready issues: `status:ready`, or every `Blocked by:` reference closed)"
        } else {
            "defect queue (ranked by triage)"
        }
    );
    println!("  queue      {} issue(s) available", queue.len());
    match queue.first() {
        Some(next) => {
            println!("  next       #{} — {}", next.number, next.title);
            println!(
                "  branch     would start with `{}`",
                cfg.attribution.branch_prefix()
            );
        }
        None => println!("  next       nothing — the queue offers no issue to take"),
    }
    println!("  bound      up to {bound} issue(s) this invocation");
    Ok(())
}

/// Append one delivered issue to the cycle ledger.
///
/// This writes the same `ledger.jsonl` the audit cycles write. `state`,
/// `metrics`, and the dashboard fold delivered work with no second
/// reader. The aperture is `backlog`. A delivered cycle is not a lens
/// going dry, and scoping the dry-streak math by aperture keeps the two
/// apart.
///
/// The ledger row is appended before the durable counter moves. If the
/// append fails, the counter stays put, and the next attempt still
/// claims this same cycle number. Moving the counter first would let a
/// failed append leave it ahead of the ledger. Then the next successful
/// delivery would skip a cycle number the ledger never recorded.
pub(super) fn record_delivery_cycle(st: &LoopState, issue: &str, pr: &str) -> Result<(), String> {
    let cycle = st.cycle_counter() + 1;

    let mut extra = serde_json::Map::new();
    extra.insert("issue".to_string(), issue.into());
    extra.insert("mode".to_string(), "backlog".into());

    let rec = CycleRecord {
        cycle,
        run_id: st.current_run_id().unwrap_or_else(|| "-".to_string()),
        ended_at: rfc3339_utc_now(),
        ended_at_unix: now_unix(),
        fixed: 1,
        filed: 0,
        new_findings: 0,
        bench: "skipped".to_string(),
        gate: "green".to_string(),
        prs: vec![pr.to_string()],
        tier: "delivery".to_string(),
        aperture: "backlog".to_string(),
        lens_tool: None,
        outcome: "ok".to_string(),
        minutes: 0,
        dry: false,
        extra,
    };
    st.append_cycle(&rec)?;
    st.set_cycle_counter(cycle)
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use stella_protocol::issue::{
        Issue, IssueClass, IssueDraft, IssueError, IssueKey, IssueLabel, IssueState,
    };

    use super::*;

    /// A tracker fixture that counts every write. A test can then assert
    /// what it read, and also that it wrote nothing.
    #[derive(Default)]
    struct FixtureProvider {
        open: Vec<Issue>,
        writes: std::sync::Mutex<u32>,
    }

    impl FixtureProvider {
        fn with(open: Vec<Issue>) -> Self {
            Self {
                open,
                writes: std::sync::Mutex::new(0),
            }
        }

        fn wrote(&self) {
            *self.writes.lock().expect("fixture lock") += 1;
        }

        fn writes(&self) -> u32 {
            *self.writes.lock().expect("fixture lock")
        }
    }

    #[async_trait]
    impl IssueProvider for FixtureProvider {
        fn id(&self) -> &str {
            "fixture"
        }

        async fn list_open(&self, limit: usize) -> Result<Vec<Issue>, IssueError> {
            Ok(self.open.iter().take(limit).cloned().collect())
        }

        async fn file(&self, _draft: &IssueDraft) -> Result<IssueKey, IssueError> {
            self.wrote();
            Ok(IssueKey::from("1000"))
        }

        async fn close(
            &self,
            _key: &IssueKey,
            _receipt: &str,
            _state: &str,
        ) -> Result<(), IssueError> {
            self.wrote();
            Ok(())
        }

        async fn comment(&self, _key: &IssueKey, _body: &str) -> Result<(), IssueError> {
            self.wrote();
            Ok(())
        }

        async fn relabel(
            &self,
            _key: &IssueKey,
            _add: &[String],
            _remove: &[String],
        ) -> Result<(), IssueError> {
            self.wrote();
            Ok(())
        }

        async fn edit(
            &self,
            _key: &IssueKey,
            _title: Option<&str>,
            _body: Option<&str>,
        ) -> Result<(), IssueError> {
            self.wrote();
            Ok(())
        }
    }

    fn issue(key: &str, labels: &[&str], body: &str) -> Issue {
        Issue {
            key: IssueKey::from(key),
            title: format!("issue {key}"),
            body: body.to_owned(),
            state: IssueState::Open,
            class: IssueClass::Feature,
            labels: labels.iter().copied().map(IssueLabel::from).collect(),
            created_at: "2026-08-01T00:00:00Z".into(),
            updated_at: "2026-08-01T00:00:00Z".into(),
            url: String::new(),
            parent: None,
        }
    }

    fn state(dir: &std::path::Path) -> LoopState {
        LoopState {
            dir: dir.to_path_buf(),
            repo_root: dir.to_path_buf(),
        }
    }

    /// The delivery witness. The generator picks the ready issue. The open
    /// blocker holds one back. The closed blocker holds nothing back. A
    /// delivered issue lands in the cycle ledger as a row the existing
    /// folds can read.
    #[test]
    fn the_backlog_generator_picks_the_ready_issue_and_records_the_delivered_cycle() {
        let provider = FixtureProvider::with(vec![
            issue("4", &["feature", "P2"], ""),
            issue("5", &["feature", "P1"], "Blocked by: #4"),
            // Its blocker is not in the open set: closed, so this is ready.
            issue("6", &["feature", "P0"], "Blocked by: #9"),
        ]);

        let keys = ready_keys(&provider, &PriorityLadder::default()).expect("fixture read");
        assert_eq!(
            keys,
            vec!["6".to_owned(), "4".to_owned()],
            "the open blocker must hold its dependent back; the closed one must hold nothing"
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let st = state(dir.path());
        record_delivery_cycle(&st, "6", "4321").expect("ledger write");

        let rows = st.cycles().rows;
        assert_eq!(rows.len(), 1, "one delivered issue is one ledger row");
        let row = &rows[0];
        assert_eq!(row.prs, vec!["4321".to_owned()]);
        assert_eq!(row.aperture, "backlog");
        assert_eq!(row.tier, "delivery");
        assert_eq!(row.fixed, 1);
        assert!(!row.dry, "a delivered cycle discovered work, it is not dry");
        assert_eq!(
            row.extra.get("issue").and_then(|v| v.as_str()),
            Some("6"),
            "the row must say which issue it delivered"
        );
        assert_eq!(st.cycle_counter(), 1, "the counter moved with the row");
    }

    /// The counter/ledger ordering witness. A failed append must not
    /// leave the durable counter ahead of the ledger it counts.
    #[test]
    fn a_failed_ledger_append_leaves_the_cycle_counter_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let st = state(dir.path());
        // A directory at the ledger path makes the append fail without
        // touching the counter file.
        std::fs::create_dir(st.ledger_path()).expect("seed a directory at the ledger path");

        let result = record_delivery_cycle(&st, "6", "4321");
        assert!(result.is_err(), "the append must fail");
        assert_eq!(
            st.cycle_counter(),
            0,
            "the counter must not advance past a cycle the ledger never recorded"
        );
    }

    /// The truncation witness. A read that lands on the read ceiling must
    /// not be trusted as the complete open set. A blocker could sit just
    /// off the page and read as closed by mistake.
    #[test]
    fn a_read_at_the_ceiling_refuses_to_judge_readiness() {
        let issues: Vec<Issue> = (0..super::super::backlog::QUEUE_READ_LIMIT)
            .map(|n| issue(&n.to_string(), &["feature"], ""))
            .collect();
        let provider = FixtureProvider::with(issues);

        let result = ready_keys(&provider, &PriorityLadder::default());
        assert!(
            result.is_err(),
            "a page-bounded read must refuse rather than guess at readiness"
        );
    }

    /// The dry-run witness. It reads the tracker and writes nothing: no
    /// filing, no label, no comment, no closure, in either generator
    /// mode.
    #[test]
    fn a_dry_run_reads_the_tracker_and_writes_nothing() {
        let provider = FixtureProvider::with(vec![
            issue("4", &["feature", "P2"], ""),
            issue("5", &["bug", "P1"], "Blocked by: #4"),
        ]);
        let cfg = LoopConfig::default();

        dry_run(&provider, &cfg, true, 3).expect("backlog dry run");
        dry_run(&provider, &cfg, false, 1).expect("defect dry run");

        assert_eq!(
            provider.writes(),
            0,
            "a dry run that wrote anything is not a dry run"
        );
    }
}
