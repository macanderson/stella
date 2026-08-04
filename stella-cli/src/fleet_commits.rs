//! Which commits are *this* fleet task's — the ledger's attribution seam.
//!
//! `stella-fleet`'s ledger calls its commit table "the authoritative record"
//! of a subagent's work: the run report prints from it, `--watch` takes the
//! branch to poll from it, and nothing reconstructs it once written. So it
//! has to be right under the DEFAULT configuration, which is
//! [`Isolation::SharedTree`] — every worker of the wave in ONE repository
//! root, on ONE `HEAD`.
//!
//! Deriving a worker's commits after the fact from `git log <start>..HEAD` in
//! that root is not right: `start` is snapshotted when the attempt opens, and
//! every commit a *sibling* lands before this attempt finishes falls inside
//! the range. Two shared-tree tasks committing concurrently — the common case
//! for a wide fan-out — each sweep up the other's commits and stamp them with
//! their own task id (#1216).
//!
//! [`CommitObserver`] answers the question at the seam instead of re-deriving
//! it from a shared log. It wraps the worker's tool stack *underneath*
//! [`crate::claims::ClaimTap`], so a commit-creating tool call runs while the
//! tap holds the workspace-wide [`COMMIT_CLAIM`] lane: no other writer in the
//! tree — another fleet's worker, a deck lane, another process — can move
//! `HEAD` inside that window. The advance observed across the call is
//! therefore exactly this worker's, and it is recorded per call rather than
//! summed at the end, so an interleaved sibling commit lands between two
//! windows and is never seen.
//!
//! ## What it does not see
//!
//! Only the tools [`crate::claims::transient_lane`] routes to the commit lane
//! are observed — `repo_commit` today. A commit made some other way in a
//! shared tree (a `git commit` through the shell, which the default
//! configuration does not register at all, or one made by a sub-agent running
//! against the raw registry) is left OUT of the ledger rather than stamped
//! onto whichever task happened to be open. That is the honest direction of
//! the two: an under-reported commit costs a `--watch` target, while a
//! misattributed one makes the authoritative record state something false.
//!
//! An [`Isolation::Isolated`] task has no such problem — its worktree has one
//! writer — so `stella fleet` keeps deriving that case from the log via
//! [`commits_between`], which sees shell commits too.
//!
//! [`Isolation::SharedTree`]: stella_fleet::Isolation::SharedTree
//! [`Isolation::Isolated`]: stella_fleet::Isolation::Isolated
//! [`COMMIT_CLAIM`]: crate::claims::COMMIT_CLAIM

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;
use stella_core::ports::ToolExecutor;
use stella_fleet::{CommitRecord, GitCli, Isolation, SystemGitCli, Task};
use stella_protocol::{ToolOutput, ToolSchema};

/// The record separator between a commit's fields. `%x1f` (ASCII US) cannot
/// appear in a sha or a unix timestamp and is vanishingly unlikely in a
/// subject line — unlike every printable delimiter.
const FIELD_SEP: char = '\u{1f}';

/// `git log`'s pretty format: sha, subject, unix commit time, separated by
/// [`FIELD_SEP`] (`%x1f`). The two must stay in step — [`parse_log`] splits
/// on what this asks git to write.
const LOG_FORMAT: &str = "--format=%H%x1f%s%x1f%ct";

/// A tool-stack decorator that records the commits its worker actually makes.
///
/// Placed **under** [`crate::claims::ClaimTap`] so each observation window is
/// the commit lane's — see the module doc for why that is the whole point.
pub(crate) struct CommitObserver<'a> {
    inner: &'a dyn ToolExecutor,
    git: Box<dyn GitCli>,
    root: PathBuf,
    /// The task id every observed commit is stamped with. Fixed for the
    /// attempt: one observer, one worker, one task.
    task_id: String,
    observed: Mutex<Vec<CommitRecord>>,
}

impl<'a> CommitObserver<'a> {
    pub(crate) fn new(
        inner: &'a dyn ToolExecutor,
        git: impl GitCli + 'static,
        root: impl Into<PathBuf>,
        task_id: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            git: Box::new(git),
            root: root.into(),
            task_id: task_id.into(),
            observed: Mutex::new(Vec::new()),
        }
    }

    /// Every commit observed so far, oldest first, leaving the observer
    /// empty. Called once, when the attempt settles.
    pub(crate) fn take(&self) -> Vec<CommitRecord> {
        std::mem::take(&mut self.observed.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

#[async_trait]
impl ToolExecutor for CommitObserver<'_> {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.inner.schemas()
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        if crate::claims::transient_lane(name) != Some(crate::claims::COMMIT_CLAIM) {
            return self.inner.execute(name, input).await;
        }
        // `HEAD` as of NOW, inside the lane the tap above is holding — the
        // fence a sibling's commit cannot cross until this call returns.
        let before = head(&*self.git, &self.root).await;
        let output = self.inner.execute(name, input).await;
        // A refused or failed call leaves `HEAD` where it was, so the range
        // below is empty and nothing is recorded: success is not asserted
        // here, it is observed.
        if let Some(before) = before {
            let mut made = commits_between(&*self.git, &self.root, &before, &self.task_id).await;
            self.observed
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .append(&mut made);
        }
        output
    }

    /// Forwarded: a decorator that let the default `0.0` stand would drop
    /// sub-agent spend out of the parent's budget (see the port's contract).
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        self.inner.drain_sub_agent_spend_usd()
    }
}

/// One attempt's commits, oldest first — and the two ways of knowing which
/// commits those are.
///
/// An **isolated** task owns its worktree outright, so the whole advance from
/// `start_sha` to `HEAD` is its own; deriving it from the log is both correct
/// and complete there, catching even a commit made outside Stella's repo
/// tools.
///
/// The **shared tree** — the default — has no such luxury, which is what this
/// module exists for: the answer is the set of advances this worker was
/// *observed* making, each read inside a window no sibling can commit
/// through.
pub(crate) async fn for_attempt(
    observed: &CommitObserver<'_>,
    root: &Path,
    start_sha: &str,
    task: &Task,
) -> Vec<CommitRecord> {
    match task.isolation {
        Isolation::Isolated => commits_between(&SystemGitCli, root, start_sha, &task.id).await,
        Isolation::SharedTree => observed.take(),
    }
}

/// The commits `root` gained between `from` and its current `HEAD`, oldest
/// first, as ledger-ready records.
///
/// Correct for a whole attempt only where `root` has ONE writer (an isolated
/// worktree). Under a shared tree call it across a window a sibling cannot
/// commit inside — which is what [`CommitObserver`] does.
pub(crate) async fn commits_between(
    git: &dyn GitCli,
    root: &Path,
    from: &str,
    task_id: &str,
) -> Vec<CommitRecord> {
    let Some(branch) = stdout(git, root, &["rev-parse", "--abbrev-ref", "HEAD"]).await else {
        return Vec::new();
    };
    let range = format!("{from}..HEAD");
    let Some(log) = stdout(git, root, &["log", "--reverse", LOG_FORMAT, &range]).await else {
        return Vec::new();
    };
    parse_log(&log, &branch, task_id)
}

/// The current `HEAD` sha, or `None` when git could not say (an empty
/// repository with no commits yet, a broken checkout, no `git` at all). A
/// worker whose fence cannot be read records nothing rather than guessing.
async fn head(git: &dyn GitCli, root: &Path) -> Option<String> {
    stdout(git, root, &["rev-parse", "--verify", "HEAD"]).await
}

/// Trimmed stdout of a successful `git -C root <args>`; `None` for a
/// non-zero exit or a failure to spawn git at all. Attribution is
/// observability: a git that will not answer degrades the record, never the
/// work.
async fn stdout(git: &dyn GitCli, root: &Path, args: &[&str]) -> Option<String> {
    let output = git.run(root, args).await.ok()?;
    output.success.then(|| output.stdout.trim().to_string())
}

/// Parse `git log --format=%H%x1f%s%x1f%ct` output into ledger records. A
/// line that does not carry all three fields is skipped, never guessed at.
fn parse_log(log: &str, branch: &str, task_id: &str) -> Vec<CommitRecord> {
    log.lines()
        .filter_map(|line| {
            let mut parts = line.split(FIELD_SEP);
            let sha = parts.next()?.to_string();
            let message = parts.next()?.to_string();
            // The ledger stores milliseconds; `%ct` is unix seconds.
            let timestamp_ms = parts.next()?.parse::<u64>().ok()?.saturating_mul(1000);
            Some(CommitRecord {
                sha,
                branch: branch.to_string(),
                task_id: task_id.to_string(),
                message,
                timestamp_ms,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use stella_fleet::{GitError, GitOutput};

    use super::*;

    /// One shared tree, scripted: a `HEAD` every worker's git sees, and a
    /// log of what each sha's commit was. Commits land through
    /// [`Self::commit`], which is what a scripted `repo_commit` calls.
    #[derive(Default)]
    struct SharedTree {
        head: Mutex<Vec<(String, String)>>,
    }

    impl SharedTree {
        fn commit(&self, sha: &str, subject: &str) {
            self.head
                .lock()
                .unwrap()
                .push((sha.to_string(), subject.to_string()));
        }

        fn head_sha(&self) -> String {
            self.head
                .lock()
                .unwrap()
                .last()
                .map(|(sha, _)| sha.clone())
                .unwrap_or_else(|| "base".to_string())
        }

        /// The scripted `git log --reverse <from>..HEAD`: every commit
        /// recorded after `from`, oldest first.
        fn log_after(&self, from: &str) -> String {
            let history = self.head.lock().unwrap();
            let start = history
                .iter()
                .position(|(sha, _)| sha == from)
                .map(|i| i + 1)
                .unwrap_or(0);
            history[start..]
                .iter()
                .enumerate()
                .map(|(i, (sha, subject))| format!("{sha}{FIELD_SEP}{subject}{FIELD_SEP}{}", 1 + i))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    /// A `GitCli` reading the scripted tree above.
    struct ScriptedGit(Arc<SharedTree>);

    #[async_trait]
    impl GitCli for ScriptedGit {
        async fn run(&self, _repo: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
            Ok(match args {
                ["rev-parse", "--verify", "HEAD"] => GitOutput::ok(self.0.head_sha()),
                ["rev-parse", "--abbrev-ref", "HEAD"] => GitOutput::ok("main"),
                ["log", "--reverse", _, range] => {
                    let from = range.trim_end_matches("..HEAD");
                    GitOutput::ok(self.0.log_after(from))
                }
                other => GitOutput::failed(1, format!("unscripted: {other:?}")),
            })
        }
    }

    /// A `repo_commit` that lands the scripted commits it was primed with,
    /// and a passthrough for everything else.
    struct CommittingTools {
        tree: Arc<SharedTree>,
        /// One entry per call: the shas that call commits.
        script: Mutex<Vec<Vec<(String, String)>>>,
    }

    impl CommittingTools {
        fn new(tree: Arc<SharedTree>, script: Vec<Vec<(&str, &str)>>) -> Self {
            Self {
                tree,
                script: Mutex::new(
                    script
                        .into_iter()
                        .map(|call| {
                            call.into_iter()
                                .map(|(sha, subject)| (sha.to_string(), subject.to_string()))
                                .collect()
                        })
                        .collect(),
                ),
            }
        }
    }

    #[async_trait]
    impl ToolExecutor for CommittingTools {
        fn schemas(&self) -> Vec<ToolSchema> {
            Vec::new()
        }

        async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
            if name == "repo_commit" {
                let mut script = self.script.lock().unwrap();
                if !script.is_empty() {
                    for (sha, subject) in script.remove(0) {
                        self.tree.commit(&sha, &subject);
                    }
                }
            }
            ToolOutput::Ok {
                content: "ok".into(),
            }
        }
    }

    fn observer<'a>(
        inner: &'a CommittingTools,
        tree: &Arc<SharedTree>,
        task_id: &str,
    ) -> CommitObserver<'a> {
        CommitObserver::new(
            inner,
            ScriptedGit(tree.clone()),
            Path::new("/repo"),
            task_id,
        )
    }

    fn shas(commits: &[CommitRecord]) -> Vec<&str> {
        commits.iter().map(|c| c.sha.as_str()).collect()
    }

    /// The witness for #1216's first half. Two shared-tree workers commit
    /// into ONE tree, interleaved — `t1`, then `t2`, then `t1` again. Every
    /// commit must appear under the task that made it and under no other.
    ///
    /// The old derivation (`git log <start_sha>..HEAD` at the end of the
    /// attempt) gives `t1` all three commits and `t2` the last two: the
    /// range is the shared tree's whole advance, not this worker's.
    #[tokio::test]
    async fn interleaved_shared_tree_commits_stay_with_the_task_that_made_them() {
        let tree = Arc::new(SharedTree::default());
        let tools_1 = CommittingTools::new(
            tree.clone(),
            vec![vec![("c1", "t1 first")], vec![("c3", "t1 second")]],
        );
        let tools_2 = CommittingTools::new(tree.clone(), vec![vec![("c2", "t2 only")]]);
        let t1 = observer(&tools_1, &tree, "t1");
        let t2 = observer(&tools_2, &tree, "t2");
        let input = serde_json::json!({ "message": "m", "paths": ["a.rs"] });

        // Serialized by the commit lane in the real stack; awaiting them in
        // this order is that serialization.
        t1.execute("repo_commit", &input).await;
        t2.execute("repo_commit", &input).await;
        t1.execute("repo_commit", &input).await;

        let first = t1.take();
        let second = t2.take();
        assert_eq!(shas(&first), vec!["c1", "c3"]);
        assert_eq!(shas(&second), vec!["c2"]);
        assert!(
            first.iter().all(|c| c.task_id == "t1"),
            "every commit carries the id of the task that made it"
        );
        assert!(second.iter().all(|c| c.task_id == "t2"));
        // And the sibling's commit is in exactly one worker's record.
        assert!(!shas(&first).contains(&"c2"));

        // The derivation this replaced, run against the same tree: `t1`'s
        // whole-attempt range is the SHARED advance, sibling commit and all.
        // Correct only where one writer owns the tree — see `commits_between`.
        let whole_advance =
            commits_between(&ScriptedGit(tree.clone()), Path::new("/repo"), "base", "t1").await;
        assert_eq!(shas(&whole_advance), vec!["c1", "c2", "c3"]);
    }

    /// A commit-free attempt records nothing — the observer reports what it
    /// saw, not what the tree did.
    #[tokio::test]
    async fn a_call_that_commits_nothing_records_nothing() {
        let tree = Arc::new(SharedTree::default());
        let tools = CommittingTools::new(tree.clone(), vec![vec![]]);
        let observer = observer(&tools, &tree, "t1");

        observer
            .execute("repo_commit", &serde_json::json!({}))
            .await;

        assert!(observer.take().is_empty());
    }

    /// A sibling that commits between two of this worker's windows is never
    /// swept in, even though it moved the shared `HEAD`.
    #[tokio::test]
    async fn a_sibling_commit_outside_the_window_is_not_attributed() {
        let tree = Arc::new(SharedTree::default());
        let tools = CommittingTools::new(tree.clone(), vec![vec![("mine", "mine")]]);
        let observer = observer(&tools, &tree, "t1");

        // The sibling's commit lands with no window of ours open.
        tree.commit("theirs", "a sibling's work");
        observer
            .execute("repo_commit", &serde_json::json!({}))
            .await;

        assert_eq!(shas(&observer.take()), vec!["mine"]);
    }

    /// Tools that do not create commits pass straight through and open no
    /// window — the observer must not spawn git on every tool call.
    #[tokio::test]
    async fn non_committing_tools_are_not_observed() {
        let tree = Arc::new(SharedTree::default());
        let tools = CommittingTools::new(tree.clone(), vec![]);
        let observer = observer(&tools, &tree, "t1");

        // `write_file` moves nothing; `repo_status` is read-only.
        observer
            .execute("write_file", &serde_json::json!({ "path": "a.rs" }))
            .await;
        observer
            .execute("repo_status", &serde_json::json!({}))
            .await;

        assert!(observer.take().is_empty());
    }

    /// The structural join with `crate::claims`: the observer watches
    /// exactly the tools the tap serializes under the commit lane. If they
    /// ever disagree, a commit is observed outside its exclusion window —
    /// which is the race this whole module exists to close.
    #[test]
    fn the_observed_tools_are_the_commit_lane_tools() {
        assert_eq!(
            crate::claims::transient_lane("repo_commit"),
            Some(crate::claims::COMMIT_CLAIM)
        );
        for name in ["run_tests", "write_file", "repo_status", "bash"] {
            assert_ne!(
                crate::claims::transient_lane(name),
                Some(crate::claims::COMMIT_CLAIM),
                "{name} would be observed outside the commit lane"
            );
        }
    }

    #[test]
    fn a_log_line_missing_fields_is_skipped_not_guessed() {
        let log = format!("abc{FIELD_SEP}subject{FIELD_SEP}1700000000\nmalformed");
        let parsed = parse_log(&log, "main", "t1");
        assert_eq!(shas(&parsed), vec!["abc"]);
        assert_eq!(parsed[0].timestamp_ms, 1_700_000_000_000);
        assert_eq!(parsed[0].branch, "main");
    }
}
