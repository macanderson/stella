//! The closures the loop did not make, and how they reach the dedup set.
//!
//! A digest drops a repeat. It stops once the issue it was filed as closes on
//! a fix (`stella_autonomy::seen`). One signal for that is the loop's own
//! receipt. It is written as `stella self-driving close` runs, and nothing
//! else writes one.
//!
//! Most of this loop's issues close some other way. A person closes them, or
//! a merge that says `Closes #N` does. That leaves no receipt. So the digest
//! keeps dropping repeats for ever, and ending that is what the decay rule is
//! for.
//!
//! The tracker knows. [`reconcile`] asks it once a cycle. Which issues have
//! closed since the last look, and how did each one close? Only `completed`
//! ages a digest out. A declined issue is not a fix. Nor is a copy of another
//! issue.
//!
//! # What it costs
//!
//! One tracker read a cycle. The list of filings grows for the life of a
//! loop. Asking about each key would cost a call per filing per pass. One
//! read over a window answers for all of them. It asks for every issue closed
//! since the last read. Rows for keys this loop did not file are dropped.
//!
//! The answer is kept in `closures.json`, beside the loop's other state. The
//! cycle it was taken on is kept with it. A second call in that cycle reads
//! the file and asks nothing.
//!
//! # What it does when it cannot ask
//!
//! Nothing. A tracker that refuses is an unknown. So is a read this provider
//! cannot answer, and so is a payload that will not parse. An unknown age
//! reads as live. The mark of how far it has read stays where it was, so the
//! next good read covers this window too. Offline ages nothing out.
//!
//! The window is one page. More than [`WINDOW`] issues can close inside it.
//! Then the read covers part of the window, and says so in the audit trail. A
//! row missed that way costs one digest that keeps dropping repeats, which is
//! what this module found. A read that keeps up on a busy day is its own work.
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use stella_autonomy::seen::Filing;
use stella_protocol::issue::{IssueClosure, IssueProvider, RESOLUTION_COMPLETED};

use super::audit::{self, Action as Audit};
use super::state::LoopState as Durable;

/// How many closed issues one pass reads.
///
/// A page over the newest closures. It is not a promise to see them all. Wide
/// enough for a repository that closes a few dozen issues a day. Small enough
/// that the read stays one cheap call.
pub(super) const WINDOW: usize = 200;

/// What the tracker has said about the issues this loop filed.
///
/// One `closures.json` in the loop's state directory. Every field has a
/// default. So a file an older build wrote is read here, not refused.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Ledger {
    /// The cycle this was last asked on. `None` on a loop that has never
    /// asked. That is what lets the first call happen at cycle zero.
    #[serde(default)]
    pub asked_on_cycle: Option<u64>,
    /// The window already read, RFC3339. Empty until a read works. Only a
    /// read that works moves it.
    #[serde(default)]
    pub through: String,
    /// The keys the tracker says closed on a fix. Only keys this loop filed
    /// are kept. Somebody else's issue ages nothing out here.
    #[serde(default)]
    pub completed: Vec<String>,
}

/// Ask which of this loop's filings have closed. Once a cycle, at most.
///
/// A cycle begins in one place and a sweep draws in another. Both call this.
/// The second call in one cycle costs a file read.
pub(super) fn reconcile(durable: &Durable, provider: &dyn IssueProvider) {
    let cycle = durable.cycle_counter();
    let mut ledger = durable.tracker_closures();
    if ledger.asked_on_cycle == Some(cycle) {
        return;
    }

    let filings = durable.filings();
    if filings.is_empty() {
        // Nothing filed, so no closure could age anything out. This is not
        // recorded as asked. The first filing is then read in the cycle after
        // it, rather than the one after that.
        return;
    }
    let filed: BTreeSet<String> = filings.iter().map(|filing| key(&filing.key)).collect();
    let since = window_start(&ledger, &filings);

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            audit::record(
                durable,
                Audit::Transient,
                None,
                &format!("could not start a runtime to read closed issues: {error}"),
            );
            return;
        }
    };

    // Asked, whatever the answer. A tracker that is down costs one read a
    // cycle, not one per pass.
    ledger.asked_on_cycle = Some(cycle);
    match runtime.block_on(provider.closed_since(&since, WINDOW)) {
        Ok(rows) => {
            let learned = absorb(&mut ledger, &rows, &filed);
            ledger.through = crate::timefmt::rfc3339_utc_now();
            let saturated = if rows.len() >= WINDOW {
                format!(" — the window was full at {WINDOW}, so older closures in it were not read")
            } else {
                String::new()
            };
            audit::record(
                durable,
                Audit::Swept,
                None,
                &format!(
                    "closures: {} closed issue(s) read since `{}`, {learned} of them this loop's \
                     own fix(es){saturated}",
                    rows.len(),
                    if since.is_empty() {
                        "the start"
                    } else {
                        since.as_str()
                    },
                ),
            );
        }
        Err(error) => audit::record(
            durable,
            Audit::Transient,
            None,
            &format!("could not ask the tracker which issues closed: {error}; nothing decays"),
        ),
    }

    if let Err(error) = durable.write_tracker_closures(&ledger) {
        audit::record(
            durable,
            Audit::Transient,
            None,
            &format!("could not write down what the tracker said about closures: {error}"),
        );
    }
}

/// Fold one page of answers into the ledger. Says how many keys it learned.
///
/// A row is kept when this loop filed that key and the tracker calls it a
/// fix. A row the provider could not place is an unknown, and is skipped. So
/// an issue closed with no word on why ages nothing out.
fn absorb(ledger: &mut Ledger, rows: &[IssueClosure], filed: &BTreeSet<String>) -> usize {
    let mut learned = 0;
    for row in rows {
        if row.resolution.as_deref() != Some(RESOLUTION_COMPLETED) {
            continue;
        }
        let key = key(row.key.as_str());
        if !filed.contains(&key) || ledger.completed.contains(&key) {
            continue;
        }
        ledger.completed.push(key);
        learned += 1;
    }
    learned
}

/// Where the next window starts.
///
/// The last read that worked, else the oldest filing this loop can date. A
/// filing written before `Filing::at` has no stamp. Its issue could have
/// closed at any time. One of those opens the first window to the whole
/// range, which is read once.
fn window_start(ledger: &Ledger, filings: &[Filing]) -> String {
    let through = ledger.through.trim();
    if !through.is_empty() {
        return through.to_owned();
    }
    if filings.iter().any(|filing| filing.at.trim().is_empty()) {
        return String::new();
    }
    // A UTC RFC3339 stamp sorts right as text. That is the rule
    // `stella_protocol::issue::Issue` reads its own stamps under.
    filings
        .iter()
        .map(|filing| filing.at.trim())
        .min()
        .unwrap_or_default()
        .to_owned()
}

/// An issue key with its decoration removed, so `#412` and ` 412 ` match.
fn key(raw: &str) -> String {
    raw.trim().trim_start_matches('#').trim().to_owned()
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use stella_protocol::issue::{Issue, IssueDraft, IssueError, IssueKey, RESOLUTION_NOT_PLANNED};

    use super::*;

    /// A tracker somebody else closes issues on.
    ///
    /// It counts the reads. This module claims one per cycle, and that claim
    /// is about this number.
    #[derive(Default)]
    struct Tracker {
        closed: Vec<IssueClosure>,
        reads: Mutex<usize>,
        dead: bool,
    }

    impl Tracker {
        fn closing(key: &str, resolution: Option<&str>) -> Self {
            Self {
                closed: vec![IssueClosure {
                    key: IssueKey::from(key),
                    resolution: resolution.map(str::to_owned),
                    closed_at: "2026-09-05T10:00:00Z".to_owned(),
                }],
                ..Self::default()
            }
        }

        fn unreachable() -> Self {
            Self {
                dead: true,
                ..Self::default()
            }
        }

        fn reads(&self) -> usize {
            *self.reads.lock().expect("fixture lock")
        }
    }

    #[async_trait]
    impl IssueProvider for Tracker {
        fn id(&self) -> &str {
            "fixture"
        }

        async fn list_open(&self, _limit: usize) -> Result<Vec<Issue>, IssueError> {
            Ok(Vec::new())
        }

        async fn file(&self, _draft: &IssueDraft) -> Result<IssueKey, IssueError> {
            Ok(IssueKey::from("1"))
        }

        async fn close(
            &self,
            _key: &IssueKey,
            _receipt: &str,
            _state: &str,
        ) -> Result<(), IssueError> {
            Ok(())
        }

        async fn comment(&self, _key: &IssueKey, _body: &str) -> Result<(), IssueError> {
            Ok(())
        }

        async fn relabel(
            &self,
            _key: &IssueKey,
            _add: &[String],
            _remove: &[String],
        ) -> Result<(), IssueError> {
            Ok(())
        }

        async fn edit(
            &self,
            _key: &IssueKey,
            _title: Option<&str>,
            _body: Option<&str>,
        ) -> Result<(), IssueError> {
            Ok(())
        }

        async fn closed_since(
            &self,
            _since: &str,
            _limit: usize,
        ) -> Result<Vec<IssueClosure>, IssueError> {
            *self.reads.lock().expect("fixture lock") += 1;
            if self.dead {
                return Err(IssueError::Unavailable {
                    provider: "fixture".into(),
                    reason: "no tracker here".into(),
                });
            }
            Ok(self.closed.clone())
        }
    }

    const DIGEST: &str = "d0d0d0d0d0d0d0d0";
    const KEY: &str = "4100";

    /// A loop that filed one finding as one issue, and closed nothing itself.
    fn one_filing(dir: &Path) {
        std::fs::create_dir_all(dir).expect("state dir");
        std::fs::write(dir.join("seen.txt"), format!("{DIGEST}\n")).expect("seen");
        std::fs::write(dir.join("cycle"), "7\n").expect("cycle");
        std::fs::write(
            dir.join("filings.jsonl"),
            format!(
                "{{\"digest\":\"{DIGEST}\",\"key\":\"{KEY}\",\"at\":\"2026-09-01T00:00:00Z\"}}\n"
            ),
        )
        .expect("filings");
    }

    fn state(tmp: &tempfile::TempDir) -> Durable {
        let dir = tmp.path().join("state");
        one_filing(&dir);
        Durable {
            dir,
            repo_root: tmp.path().to_path_buf(),
        }
    }

    /// **The witness.** A person closed the issue the loop filed. The digest
    /// stops dropping repeats, so the defect can be filed again.
    ///
    /// The one signal before this module was `receipts.jsonl`, and only
    /// `stella self-driving close` writes that. A filing with no receipt and
    /// a closed issue left the digest in force for ever. The first assert
    /// below pins that.
    #[test]
    fn an_issue_a_person_closed_ages_its_digest_out() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let st = state(&tmp);
        let tracker = Tracker::closing(KEY, Some(RESOLUTION_COMPLETED));

        assert_eq!(
            st.live_seen(),
            st.seen(),
            "with no receipt the digest still suppresses"
        );

        reconcile(&st, &tracker);

        assert_eq!(st.seen(), vec![DIGEST.to_owned()], "the file is unchanged");
        assert!(
            st.live_seen().is_empty(),
            "a closure the loop did not make must still decay, got {:?}",
            st.live_seen()
        );
    }

    /// A tracker nobody can reach ages nothing out. It leaves the window
    /// where it was, so the next read that works still covers it.
    #[test]
    fn an_unreachable_tracker_decays_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let st = state(&tmp);

        reconcile(&st, &Tracker::unreachable());

        assert_eq!(st.live_seen(), st.seen(), "an unknown closure is not one");
        assert_eq!(
            st.tracker_closures().through,
            "",
            "a failed read must not advance the window"
        );
    }

    /// Declined work is not a fix, so its digest keeps dropping repeats.
    #[test]
    fn a_closure_that_is_not_a_fix_decays_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let st = state(&tmp);

        reconcile(&st, &Tracker::closing(KEY, Some(RESOLUTION_NOT_PLANNED)));

        assert_eq!(st.live_seen(), st.seen());
        assert!(st.tracker_closures().completed.is_empty());
    }

    /// An issue closed with no word on why is an unknown. An unknown is not
    /// a fix.
    #[test]
    fn a_closure_with_no_resolution_decays_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let st = state(&tmp);

        reconcile(&st, &Tracker::closing(KEY, None));

        assert_eq!(st.live_seen(), st.seen());
    }

    /// The tracker is asked once a cycle, not once a pass. Both doors call
    /// this, and the second one must cost a file read.
    #[test]
    fn the_tracker_is_asked_once_a_cycle() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let st = state(&tmp);
        let tracker = Tracker::closing(KEY, Some(RESOLUTION_COMPLETED));

        reconcile(&st, &tracker);
        reconcile(&st, &tracker);
        assert_eq!(tracker.reads(), 1, "the cached answer must be reused");

        st.set_cycle_counter(8).expect("counter");
        reconcile(&st, &tracker);
        assert_eq!(tracker.reads(), 2, "a new cycle asks again");
    }

    /// Somebody else's closed issue is not this loop's business. It is not
    /// kept, so the file stays as small as the list of filings.
    #[test]
    fn a_closure_of_an_unfiled_issue_is_not_kept() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let st = state(&tmp);

        reconcile(&st, &Tracker::closing("9999", Some(RESOLUTION_COMPLETED)));

        assert!(st.tracker_closures().completed.is_empty());
        assert_eq!(st.live_seen(), st.seen());
    }

    /// The window starts where the last read stopped. With no such mark, it
    /// starts at the oldest filing the loop can date.
    #[test]
    fn the_window_starts_at_the_watermark_then_at_the_oldest_filing() {
        let dated = Filing::new(DIGEST, KEY, "2026-09-01T00:00:00Z");
        let older = Filing::new("aaaa", "4000", "2026-08-01T00:00:00Z");
        let undated = Filing::new("bbbb", "3900", "");

        let fresh = Ledger::default();
        assert_eq!(
            window_start(&fresh, &[dated.clone(), older]),
            "2026-08-01T00:00:00Z"
        );
        assert_eq!(
            window_start(&fresh, &[dated.clone(), undated]),
            "",
            "a filing with no stamp makes the first window unbounded"
        );

        let read = Ledger {
            through: "2026-09-04T12:00:00Z".to_owned(),
            ..Ledger::default()
        };
        assert_eq!(window_start(&read, &[dated]), "2026-09-04T12:00:00Z");
    }
}
