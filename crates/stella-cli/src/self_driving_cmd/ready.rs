//! The backlog work generator behind `drive --backlog`, and the dry-run
//! report.
//!
//! The defect queue takes only what triage marked as a defect. This
//! generator drains a whole backlog, in readiness order. See
//! `stella_autonomy::ready` for what ready means and how `Blocked by:`
//! lines are read. A sibling of `self_driving_cmd.rs`: new logic lands
//! beside that file, never in it.

use std::collections::BTreeSet;

use stella_autonomy::CycleRecord;
use stella_protocol::issue::IssueProvider;

use crate::timefmt::{now_unix, rfc3339_utc_now};

use super::config::LoopConfig;
use super::state::LoopState;

/// One read of the tracker, folded into the ready backlog.
///
/// The open set and the items come from one read, so an issue and its
/// blocker are judged at one moment. Two reads could see a blocker
/// close between them and disagree.
///
/// The readiness rule trusts the open set to be complete: a blocker
/// not in it reads as closed. A read that fills `QUEUE_READ_LIMIT`
/// cannot make that promise. The tracker may hold more open issues
/// than the page carried, and one of them could be the blocker a
/// dependent issue names. That issue would then misread as ready. So
/// a hit on the ceiling fails loud instead of guessing.
fn ready_issues(
    provider: &dyn IssueProvider,
    cfg: &LoopConfig,
) -> Result<Vec<stella_autonomy::QueueIssue>, String> {
    let issues = open_page(provider)?;
    Ok(fold_ready(&issues, cfg))
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
    cfg: &LoopConfig,
) -> Result<Vec<stella_protocol::issue::Issue>, String> {
    let issues = open_page(provider)?;
    Ok(join_records(&issues, &fold_ready(&issues, cfg)))
}

/// [`ready_full`], for a caller that already holds a runtime.
///
/// The driver channel's `backlog_next` is one. It is served inside the
/// runtime the driver session runs on. A nested `block_on` panics there.
///
/// Same read, same page cap, same fold. This one builds no runtime.
/// [`ready_full`] wraps it and blocks.
pub(crate) async fn ready_full_async(
    provider: &dyn IssueProvider,
    cfg: &LoopConfig,
) -> Result<Vec<stella_protocol::issue::Issue>, String> {
    let issues = read_open_page(provider).await?;
    Ok(join_records(&issues, &fold_ready(&issues, cfg)))
}

/// The ready numbers, joined back to the records of the read that judged
/// them. So one read's order can never meet another read's body.
fn join_records(
    issues: &[stella_protocol::issue::Issue],
    ready: &[stella_autonomy::QueueIssue],
) -> Vec<stella_protocol::issue::Issue> {
    ready
        .iter()
        .filter_map(|queued| {
            issues
                .iter()
                .find(|issue| issue.key.as_str() == queued.number.to_string())
                .cloned()
        })
        .collect()
}

/// One bounded read of the open set, with the cap above.
fn open_page(provider: &dyn IssueProvider) -> Result<Vec<stella_protocol::issue::Issue>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime for the issue provider: {error}"))?;
    runtime.block_on(read_open_page(provider))
}

/// The read itself, and the ceiling that governs it.
async fn read_open_page(
    provider: &dyn IssueProvider,
) -> Result<Vec<stella_protocol::issue::Issue>, String> {
    let issues = provider
        .list_open(super::backlog::QUEUE_READ_LIMIT)
        .await
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
///
/// The clock is read here: `stella-autonomy` does no I/O and takes the time
/// as an argument. The escalation record rides on the issue itself, put
/// there by `to_queue_issue` out of the body this read already carried.
fn fold_ready(
    issues: &[stella_protocol::issue::Issue],
    cfg: &LoopConfig,
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
    stella_autonomy::ready::ready_queue(
        items,
        &open,
        &cfg.triage.ladder,
        &cfg.container_labels,
        &cfg.escalation,
        now_unix(),
    )
}

/// The ready backlog as bare keys, in the order the loop takes them.
///
/// The backlog twin of `backlog::ranked_keys`: same read ceiling, same
/// shape, a different meaning of "next" — ready, not defect rank.
pub(super) fn ready_keys(
    provider: &dyn IssueProvider,
    cfg: &LoopConfig,
) -> Result<Vec<String>, String> {
    Ok(ready_issues(provider, cfg)?
        .into_iter()
        .map(|issue| issue.number.to_string())
        .collect())
}

/// Report what `drive` would take next, and take nothing.
///
/// One tracker read and a printed answer. No claim, no branch, no
/// label, no session record. Checking the loop's aim must not move
/// its hand.
pub(super) fn dry_run(
    provider: &dyn IssueProvider,
    cfg: &LoopConfig,
    backlog: bool,
    bound: u32,
) -> Result<(), String> {
    let queue = if backlog {
        ready_issues(provider, cfg)?
    } else {
        super::backlog::ranked(provider, cfg)?.0.ranked
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
/// The same `ledger.jsonl` the audit cycles write, so `state`,
/// `metrics` and the dashboard fold delivered work with no second
/// reader. The aperture is `backlog`. A delivered cycle is not a lens
/// going dry, and scoping the dry-streak math by aperture keeps the
/// two apart.
///
/// The row is appended before the durable counter advances. If the
/// append fails, the counter is untouched, and the next attempt
/// claims the same cycle number. The other order is wrong: a failed
/// append would leave the counter ahead of the ledger, and the next
/// delivery would skip a number the ledger never saw.
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

    /// The delivery witness. The generator picks the ready issue: an
    /// open blocker holds its dependent back, and a closed one holds
    /// nothing back. A delivered issue lands in the cycle ledger as a
    /// row the existing folds can read.
    #[test]
    fn the_backlog_generator_picks_the_ready_issue_and_records_the_delivered_cycle() {
        let provider = FixtureProvider::with(vec![
            issue("4", &["feature", "P2"], ""),
            issue("5", &["feature", "P1"], "Blocked by: #4"),
            // Its blocker is not in the open set: closed, so this is ready.
            issue("6", &["feature", "P0"], "Blocked by: #9"),
        ]);

        let keys = ready_keys(&provider, &LoopConfig::default()).expect("fixture read");
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

    /// The ordering witness. A failed append must not leave the
    /// counter ahead of the ledger it counts.
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

    /// The truncation witness. A read that fills the ceiling is not
    /// the whole open set. A blocker could sit just off the page and
    /// misread as closed.
    #[test]
    fn a_read_at_the_ceiling_refuses_to_judge_readiness() {
        let issues: Vec<Issue> = (0..super::super::backlog::QUEUE_READ_LIMIT)
            .map(|n| issue(&n.to_string(), &["feature"], ""))
            .collect();
        let provider = FixtureProvider::with(issues);

        let result = ready_keys(&provider, &LoopConfig::default());
        assert!(
            result.is_err(),
            "a page-bounded read must refuse rather than guess at readiness"
        );
    }

    /// **The end-to-end cooldown witness.** An issue escalated because the
    /// box broke keeps its record in its body. The generator reads that
    /// body, sees the wait is over, and offers the issue again. The
    /// `agent-escalated` label is still on it. Nothing was written.
    ///
    /// An issue that used up its tries is offered by nothing.
    #[test]
    fn the_backlog_generator_offers_an_escalated_issue_again_once_its_cooldown_is_over() {
        use stella_autonomy::escalation::{
            EnvCause, EscalationPolicy, EscalationReason, EscalationRecord, stamp,
        };

        let policy = EscalationPolicy::default();
        let long_ago =
            now_unix() - i64::try_from(policy.environmental_cooldown_secs).expect("fits") - 60;
        let cooled = EscalationRecord {
            attempts: 1,
            last_reason: EscalationReason::Environmental(EnvCause::StuckLoop),
            last_at: "2026-09-02T00:00:00Z".to_owned(),
            last_at_unix: long_ago,
        };
        let spent = EscalationRecord {
            attempts: policy.park_after,
            ..cooled.clone()
        };

        let body = "## What happens\nThe environment was stale.\n";
        let provider = FixtureProvider::with(vec![
            issue(
                "17",
                &["feature", "P1", stella_autonomy::ESCALATION_LABEL],
                &stamp(body, &cooled),
            ),
            issue(
                "18",
                &["feature", "P1", stella_autonomy::ESCALATION_LABEL],
                &stamp(body, &spent),
            ),
        ]);

        let keys = ready_keys(&provider, &LoopConfig::default()).expect("fixture read");
        assert_eq!(
            keys,
            vec!["17".to_owned()],
            "a cooled environmental escalation returns; one out of attempts stays parked"
        );
        assert_eq!(
            provider.writes(),
            0,
            "requeueing must cost no label surgery and no tracker write"
        );
    }

    /// `LoopConfig::default()`'s container labels reach `ready_keys`.
    /// An epic drops out, even with no open blocker.
    #[test]
    fn the_default_config_container_labels_drop_an_epic_from_ready_keys() {
        let provider = FixtureProvider::with(vec![
            issue("4", &["feature", "P2"], ""),
            issue("7", &["P0", "epic"], ""),
        ]);

        let keys = ready_keys(&provider, &LoopConfig::default()).expect("fixture read");
        assert_eq!(
            keys,
            vec!["4".to_owned()],
            "the epic must not reach the backlog reader's output"
        );
    }

    /// The dry-run witness. It reads the tracker and writes nothing —
    /// no filing, no label, no comment, no closure — in either mode.
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
