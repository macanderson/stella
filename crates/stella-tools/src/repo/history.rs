//! Read-only history orientation: `repo_history` and `repo_recover`.
//!
//! The sibling module [`super`] answers "what is pending right now?"
//! (`repo_status`, `repo_diff`). Neither it nor anything else in the crate
//! could answer "where has this repository *been*?" — so an agent that
//! needed a branch list, a stash list, a commit log or the position log had
//! exactly one route to it: `bash`. In a read-only role (`verifier`,
//! `research`, `witness_author`) `bash` is withheld by
//! `stella_core::ports::ReadOnlyTools`, which left those roles with no route
//! at all. A `fix-git` bench trial recorded the consequence: nine
//! consecutive `tool_search` calls hunting for a git tool that did not
//! exist, then two `bash` calls refused, then the step cap — 45 seconds and
//! ~$0.11 spent to learn nothing, in a stage whose entire job was to read
//! git state.
//!
//! Both tools here are `read_only: true`, so they survive that filter and
//! the withheld shell stops being a dead end.
//!
//! **Vendor-neutral, like [`super`].** No tool name and no argument name
//! says "git". The concepts are repository concepts — commits, branches,
//! shelved changes, the log of where the working position has been — and
//! reach a backend through the [`HistoryBackend`] port. Where a git
//! spelling genuinely aids comprehension it rides in the *description*
//! (which the model reads) and never in an identifier (which a second
//! adapter would have to honour).
//!
//! [`HistoryBackend`] is a separate port rather than more methods on
//! [`super::RepoBackend`]: this is a distinct capability with a distinct
//! cost profile — `repo_recover` walks the whole object store — and
//! widening the existing trait would break every mock that implements it
//! for a test that does not care about history.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use super::{GitCli, RepoError};
use crate::registry::Tool;

/// Commits rendered by `repo_history` at most, and the ceiling its `limit`
/// argument is clamped to. Twenty is the default because that is roughly
/// where a log stops being orientation and starts being an archive.
const MAX_COMMITS: usize = 100;
const DEFAULT_COMMITS: usize = 20;

/// Branches rendered at most. Each one past the current branch costs a
/// `rev-list --count` subprocess to place it ahead//behind the default, so
/// this bounds *work*, not just output.
const MAX_BRANCHES: usize = 50;

/// Position-log ("reflog") entries rendered at most.
const MAX_POSITION_LOG: usize = 30;

/// Unreferenced commits rendered by `repo_recover` at most.
const MAX_UNREFERENCED: usize = 50;

/// Field separator inside a `--format` line. ASCII unit separator: git never
/// emits it itself, and `%s` (subject) cannot contain a newline, so one
/// record per line with `\x1f` between fields parses unambiguously without
/// the `-z` handling a NUL-terminated stream would need.
const FS: char = '\u{1f}';

/// One commit, as every history surface here renders it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommitRow {
    pub sha: String,
    pub author: String,
    /// Author date, ISO-8601 with offset (`%aI`) — sortable and unambiguous,
    /// unlike git's default human format.
    pub date: String,
    pub subject: String,
}

/// One branch and its position relative to the repository's default branch.
///
/// `ahead`/`behind` are `None` when the comparison could not be made (no
/// default branch resolved, or the two have no merge base) — deliberately
/// distinct from `Some(0)`, which asserts the branches are level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BranchRow {
    pub name: String,
    pub head: String,
    pub current: bool,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
}

/// One shelved (git: stashed) change set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShelvedRow {
    /// The selector that addresses it (`stash@{0}`).
    pub selector: String,
    pub subject: String,
}

/// One entry in the log of where the working position has been (git:
/// reflog) — the record that survives a checkout and is how work "lost" by
/// moving off a detached head is found again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PositionRow {
    pub sha: String,
    pub selector: String,
    pub description: String,
}

/// The `repo_history` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoHistory {
    /// Current branch, or `None` when the working position is detached —
    /// which is itself the answer to a large share of "where did my work
    /// go?", so it is a first-class field rather than an absence.
    pub branch: Option<String>,
    pub detached: bool,
    pub head: String,
    pub default_branch: Option<String>,
    pub commits: Vec<CommitRow>,
    pub branches: Vec<BranchRow>,
    pub shelved: Vec<ShelvedRow>,
    pub position_log: Vec<PositionRow>,
    /// Set when any list above was cut at its ceiling, naming which. A
    /// capped answer that does not say so reads as a complete one.
    pub truncated: Vec<String>,
}

/// One commit no branch points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnreferencedCommit {
    pub sha: String,
    pub author: String,
    pub date: String,
    pub subject: String,
    pub parents: Vec<String>,
    /// True when `parents[0]` is contained in the reference the search was
    /// scoped to — i.e. this commit sits directly on top of it and can be
    /// integrated by fast-forward rather than by a merge that could conflict.
    pub sits_on_target: bool,
}

/// The `repo_recover` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoRecovery {
    /// The reference "unreferenced" was judged against.
    pub target: String,
    pub commits: Vec<UnreferencedCommit>,
    pub truncated: bool,
    /// How many candidates were withheld as Stella's own bookkeeping.
    /// Reported rather than silently dropped: a caller comparing this
    /// against raw `fsck` output must be able to reconcile the counts.
    pub pipeline_commits_excluded: usize,
}

/// Options for [`HistoryBackend::history`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRequest {
    pub limit: usize,
    pub include_shelved: bool,
    pub include_position_log: bool,
}

impl Default for HistoryRequest {
    fn default() -> Self {
        Self {
            limit: DEFAULT_COMMITS,
            include_shelved: true,
            include_position_log: true,
        }
    }
}

/// The history port. See the module doc for why this is separate from
/// [`super::RepoBackend`].
#[async_trait]
pub trait HistoryBackend: Send + Sync {
    async fn history(
        &self,
        root: &Path,
        request: &HistoryRequest,
    ) -> Result<RepoHistory, RepoError>;

    /// Commits reachable from neither `target` nor any branch — the ones a
    /// checkout can strand. `target` is a reference name; the backend
    /// resolves it.
    async fn unreferenced(&self, root: &Path, target: &str) -> Result<RepoRecovery, RepoError>;
}

/// Split one `--format` record on [`FS`] into exactly `n` fields.
///
/// Returns `None` for a short record rather than padding it: a truncated
/// line means the format and the parser disagree, and inventing empty
/// fields would render that as data.
fn fields(line: &str, n: usize) -> Option<Vec<&str>> {
    let parts: Vec<&str> = line.splitn(n, FS).collect();
    (parts.len() == n).then_some(parts)
}

impl GitCli {
    /// `git` for history reads, tolerating a nonzero exit.
    ///
    /// Several of these commands answer correctly *and* exit nonzero:
    /// `symbolic-ref` exits 1 on a detached head (that is the answer),
    /// `fsck` exits 1 whenever it reports unreachable objects (also the
    /// answer), and `stash list` on a repository that never stashed exits 0
    /// with nothing. Treating a nonzero exit as failure here would turn
    /// every one of those into an error the agent has to work around.
    async fn history_probe(root: &Path, action: &'static str, args: &[&str]) -> (i32, String) {
        match Self::git(root, action, args).await {
            Ok((code, out)) => (code, out),
            Err(_) => (-1, String::new()),
        }
    }

    async fn head_sha(root: &Path) -> Result<String, RepoError> {
        let (code, out) = Self::history_probe(root, "repo_history", &["rev-parse", "HEAD"]).await;
        if code != 0 {
            // An empty repository has no HEAD. That is a legitimate state
            // to report, not a failure to read one.
            return Ok(String::new());
        }
        Ok(out.trim().to_string())
    }

    async fn ahead_behind(root: &Path, base: &str, tip: &str) -> (Option<u64>, Option<u64>) {
        let range = format!("{base}...{tip}");
        let (code, out) = Self::history_probe(
            root,
            "repo_history",
            &["rev-list", "--left-right", "--count", &range],
        )
        .await;
        if code != 0 {
            return (None, None);
        }
        let mut cols = out.split_whitespace();
        let behind = cols.next().and_then(|c| c.parse::<u64>().ok());
        let ahead = cols.next().and_then(|c| c.parse::<u64>().ok());
        match (behind, ahead) {
            (Some(b), Some(a)) => (Some(a), Some(b)),
            _ => (None, None),
        }
    }
}

#[async_trait]
impl HistoryBackend for GitCli {
    async fn history(
        &self,
        root: &Path,
        request: &HistoryRequest,
    ) -> Result<RepoHistory, RepoError> {
        let limit = request.limit.clamp(1, MAX_COMMITS);
        let mut truncated = Vec::new();

        let head = Self::head_sha(root).await?;
        let (branch_code, branch_out) = Self::history_probe(
            root,
            "repo_history",
            &["symbolic-ref", "-q", "--short", "HEAD"],
        )
        .await;
        let branch = (branch_code == 0)
            .then(|| branch_out.trim().to_string())
            .filter(|b| !b.is_empty());
        let detached = branch.is_none() && !head.is_empty();

        let default_branch = super::RepoBackend::default_branch(self, root).await?;

        let count = limit.to_string();
        let (_, log_out) = Self::history_probe(
            root,
            "repo_history",
            &[
                "log",
                "--no-color",
                "--max-count",
                &count,
                &format!("--format=%H{FS}%an{FS}%aI{FS}%s"),
            ],
        )
        .await;
        let commits: Vec<CommitRow> = log_out
            .lines()
            .filter_map(|line| fields(line, 4))
            .map(|f| CommitRow {
                sha: f[0].to_string(),
                author: f[1].to_string(),
                date: f[2].to_string(),
                subject: f[3].to_string(),
            })
            .collect();
        if commits.len() == limit {
            truncated.push("commits".to_string());
        }

        let (_, refs_out) = Self::history_probe(
            root,
            "repo_history",
            &[
                "for-each-ref",
                &format!("--format=%(refname:short){FS}%(objectname)"),
                "refs/heads",
            ],
        )
        .await;
        let rows: Vec<(String, String)> = refs_out
            .lines()
            .filter_map(|line| fields(line, 2))
            .map(|f| (f[0].to_string(), f[1].to_string()))
            .collect();
        if rows.len() > MAX_BRANCHES {
            truncated.push("branches".to_string());
        }
        let mut branches = Vec::new();
        for (name, sha) in rows.into_iter().take(MAX_BRANCHES) {
            let (ahead, behind) = match default_branch.as_deref() {
                Some(base) if base != name => Self::ahead_behind(root, base, &name).await,
                // A branch is never ahead of or behind itself, and there is
                // nothing to compare against when no default resolved.
                Some(_) => (Some(0), Some(0)),
                None => (None, None),
            };
            branches.push(BranchRow {
                current: Some(&name) == branch.as_ref(),
                name,
                head: sha,
                ahead,
                behind,
            });
        }

        let shelved = if request.include_shelved {
            let (_, out) = Self::history_probe(
                root,
                "repo_history",
                &["stash", "list", &format!("--format=%gd{FS}%s")],
            )
            .await;
            out.lines()
                .filter_map(|line| fields(line, 2))
                .map(|f| ShelvedRow {
                    selector: f[0].to_string(),
                    subject: f[1].to_string(),
                })
                .collect()
        } else {
            Vec::new()
        };

        let position_log = if request.include_position_log {
            let cap = MAX_POSITION_LOG.to_string();
            let (_, out) = Self::history_probe(
                root,
                "repo_history",
                &[
                    "reflog",
                    "show",
                    "--max-count",
                    &cap,
                    &format!("--format=%H{FS}%gd{FS}%gs"),
                ],
            )
            .await;
            let rows: Vec<PositionRow> = out
                .lines()
                .filter_map(|line| fields(line, 3))
                .map(|f| PositionRow {
                    sha: f[0].to_string(),
                    selector: f[1].to_string(),
                    description: f[2].to_string(),
                })
                .collect();
            if rows.len() == MAX_POSITION_LOG {
                truncated.push("position_log".to_string());
            }
            rows
        } else {
            Vec::new()
        };

        Ok(RepoHistory {
            branch,
            detached,
            head,
            default_branch,
            commits,
            branches,
            shelved,
            position_log,
            truncated,
        })
    }

    async fn unreferenced(&self, root: &Path, target: &str) -> Result<RepoRecovery, RepoError> {
        // `fsck` finds commits no ref points at; `rev-list --reflog` finds
        // commits a ref *did* point at and no longer does. Neither is a
        // superset of the other — a commit stranded by a checkout is in the
        // position log until it expires, and unreachable forever after — so
        // the union is what "work I cannot see any more" actually means.
        let mut candidates: BTreeSet<String> = BTreeSet::new();

        let (_, fsck_out) = Self::history_probe(
            root,
            "repo_recover",
            &["fsck", "--unreachable", "--no-progress"],
        )
        .await;
        for line in fsck_out.lines() {
            let mut cols = line.split_whitespace();
            if cols.next() == Some("unreachable")
                && cols.next() == Some("commit")
                && let Some(sha) = cols.next()
            {
                candidates.insert(sha.to_string());
            }
        }

        let not_target = format!("^{target}");
        let (_, reflog_out) = Self::history_probe(
            root,
            "repo_recover",
            &["rev-list", "--reflog", "--no-merges", &not_target],
        )
        .await;
        for line in reflog_out.lines() {
            let sha = line.trim();
            if !sha.is_empty() {
                candidates.insert(sha.to_string());
            }
        }

        let mut commits = Vec::new();
        let mut pipeline_commits_excluded = 0usize;
        let mut truncated = false;
        for sha in candidates {
            // Already integrated: not lost, just old.
            let (contained, _) = Self::history_probe(
                root,
                "repo_recover",
                &["merge-base", "--is-ancestor", &sha, target],
            )
            .await;
            if contained == 0 {
                continue;
            }

            let (code, out) = Self::history_probe(
                root,
                "repo_recover",
                &[
                    "show",
                    "-s",
                    &format!("--format=%an{FS}%ae{FS}%aI{FS}%P{FS}%s"),
                    &sha,
                ],
            )
            .await;
            let Some(f) = (code == 0)
                .then(|| out.lines().next())
                .flatten()
                .and_then(|l| fields(l, 5))
            else {
                continue;
            };

            // Stella's own candidate-workspace plumbing is unreachable by
            // construction — the pipeline seals a snapshot commit per
            // verified step and the previous one is orphaned as the
            // per-worktree baseline ref advances. Surfacing those as "lost
            // work" is how a `fix-git` trial spent its second half trying to
            // merge Stella's bookkeeping into the user's branch, an
            // obligation that grows by one commit every revise cycle and can
            // therefore never be discharged. They are excluded here, at the
            // one place that enumerates them, and counted so the omission is
            // auditable.
            if f[1] == crate::verify::CANDIDATE_SNAPSHOT_EMAIL {
                pipeline_commits_excluded += 1;
                continue;
            }

            if commits.len() >= MAX_UNREFERENCED {
                truncated = true;
                break;
            }

            let parents: Vec<String> = f[3].split_whitespace().map(str::to_string).collect();
            let sits_on_target = match parents.first() {
                Some(first) => {
                    let (code, _) = Self::history_probe(
                        root,
                        "repo_recover",
                        &["merge-base", "--is-ancestor", first, target],
                    )
                    .await;
                    code == 0
                }
                None => false,
            };

            commits.push(UnreferencedCommit {
                sha: sha.clone(),
                author: f[0].to_string(),
                date: f[2].to_string(),
                subject: f[4].to_string(),
                parents,
                sits_on_target,
            });
        }

        // Newest first: the work someone just lost is the work they want.
        commits.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.sha.cmp(&b.sha)));

        Ok(RepoRecovery {
            target: target.to_string(),
            commits,
            truncated,
            pipeline_commits_excluded,
        })
    }
}

fn render<T: Serialize>(value: &T, what: &str) -> ToolOutput {
    match serde_json::to_string_pretty(value) {
        Ok(json) => ToolOutput::Ok { content: json },
        Err(e) => ToolOutput::Error { class: None,
            message: format!("cannot render {what}: {e}"),
        },
    }
}

pub struct RepoHistoryTool(pub Arc<dyn HistoryBackend>);

#[async_trait]
impl Tool for RepoHistoryTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "repo_history".into(),
            // No longer claims to be the first stop for missing work (#2575):
            // `repo_status` costs one git call, takes no arguments, and
            // already reports the detached position, the shelved count and
            // the orphaned commits. What is only here is the HISTORY those
            // facts sit inside — when the position moved, and how far each
            // branch has diverged.
            description: "Where this repository has BEEN: recent commits, every branch with \
                          its ahead/behind vs the default branch, each shelved change with \
                          the selector that addresses it (`stash@{0}`), and the log of where \
                          the working position has moved (git reflog). One call, no shell — \
                          reach for it instead of `bash git log`/`git branch`/`git reflog`. \
                          `repo_status` is cheaper and already answers the current branch, a \
                          detached position, the shelved COUNT and the orphaned commits the \
                          reflog still reaches, so start there; come here when you need the \
                          history around those facts — which commit the position moved to and \
                          when, how far every branch has diverged, or a stash's selector so \
                          you can address it. If a commit appears in none of it, \
                          `repo_recover` searches the object store itself."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": format!(
                            "Commits to return, newest first (default {DEFAULT_COMMITS}, \
                             max {MAX_COMMITS})."
                        ),
                    },
                    "include_shelved": {
                        "type": "boolean",
                        "description": "Include shelved/stashed changes (default true).",
                    },
                    "include_position_log": {
                        "type": "boolean",
                        "description": "Include the working-position log (default true).",
                    }
                }
            }),
            read_only: true,
            speculation_safe: true,
        }
    }

    async fn execute(&self, input: &Value, root: &Path) -> ToolOutput {
        let request = HistoryRequest {
            limit: input
                .get("limit")
                .and_then(Value::as_u64)
                .map_or(DEFAULT_COMMITS, |n| n as usize),
            include_shelved: input
                .get("include_shelved")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            include_position_log: input
                .get("include_position_log")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        };
        match self.0.history(root, &request).await {
            Ok(history) => render(&history, "repository history"),
            Err(e) => ToolOutput::Error { class: None,
                message: e.to_string(),
            },
        }
    }

    async fn command_for_gate(&self, _input: &Value, _root: &Path) -> Option<String> {
        Some("git log --format=%H".into())
    }
}

pub struct RepoRecoverTool(pub Arc<dyn HistoryBackend>);

#[async_trait]
impl Tool for RepoRecoverTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "repo_recover".into(),
            // The escalation clause used to route through `repo_history` for
            // "no stash and no detached position" — written before
            // `repo_status` reported either (#2551), so it sent a caller to
            // the middle rung for facts the cheapest one already had (#2575).
            // It now names what is genuinely only here: a caller-chosen
            // `target`, fast-forwardability, and the reach past the reflog.
            description: "Find committed work that no branch points at, judged against a \
                          reference you name (`target`, default HEAD). Searches the whole \
                          object store (git fsck) UNIONed with the position log, so it is the \
                          only tool that reaches commits the reflog has already expired, and \
                          the only one that answers \"unreferenced relative to THIS ref\". \
                          Returns each commit's sha, subject, author, date, parents, and \
                          `sits_on_target` (true when it sits directly on the target and can \
                          be integrated by fast-forward rather than a merge that could \
                          conflict); commits already contained in the target are omitted as \
                          found, not lost. This is the slow last resort — scanning every \
                          object is why `repo_status` does not do it on every call — so reach \
                          for it only once `repo_status` has shown no shelved change, no \
                          detached position and nothing under `unreachable`. Stella's own \
                          candidate-workspace snapshot commits are excluded and counted \
                          separately — they are unreachable by construction and are never the \
                          user's lost work."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Reference the search is judged against — a commit \
                                        is reported only when it is NOT already contained \
                                        in this (default: HEAD).",
                    }
                }
            }),
            read_only: true,
            speculation_safe: true,
        }
    }

    async fn execute(&self, input: &Value, root: &Path) -> ToolOutput {
        let target = input
            .get("target")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or("HEAD");
        // A reference is a revision, and a revision is `git`'s own
        // mini-language: `--foo` would be read as an option and `a b` as two.
        // Reject anything that is not a plain revision token rather than
        // handing it to the backend.
        if target.starts_with('-') || target.split_whitespace().count() != 1 {
            return ToolOutput::Error { class: None,
                message: format!(
                    "`target` must be a single reference name (got {target:?}) — \
                     options and multi-word revisions are not accepted"
                ),
            };
        }
        match self.0.unreferenced(root, target).await {
            Ok(recovery) => render(&recovery, "recovery report"),
            Err(e) => ToolOutput::Error { class: None,
                message: e.to_string(),
            },
        }
    }

    async fn command_for_gate(&self, _input: &Value, _root: &Path) -> Option<String> {
        Some("git fsck --unreachable".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_splits_exactly_and_keeps_separators_in_the_last_field() {
        let line = format!("sha{FS}Ada{FS}2026-01-01{FS}subject: with{FS}separator");
        let f = fields(&line, 4).expect("four fields");
        assert_eq!(f[0], "sha");
        assert_eq!(f[1], "Ada");
        assert_eq!(f[2], "2026-01-01");
        // `splitn` leaves the remainder whole, so a subject that itself
        // contains the separator is not silently cut into extra columns.
        assert_eq!(f[3], format!("subject: with{FS}separator"));
    }

    #[test]
    fn fields_refuses_a_short_record_rather_than_padding_it() {
        assert!(fields(&format!("sha{FS}Ada"), 4).is_none());
        assert!(fields("", 2).is_none());
    }

    #[test]
    fn history_request_defaults_include_both_optional_sections() {
        let d = HistoryRequest::default();
        assert_eq!(d.limit, DEFAULT_COMMITS);
        assert!(d.include_shelved);
        assert!(d.include_position_log);
    }

    /// Invariant #4 (serde-first): every type crossing the tool boundary
    /// round-trips byte-for-byte.
    #[test]
    fn payloads_round_trip_through_serde_json() {
        let history = RepoHistory {
            branch: None,
            detached: true,
            head: "abc".into(),
            default_branch: Some("main".into()),
            commits: vec![CommitRow {
                sha: "abc".into(),
                author: "Ada".into(),
                date: "2026-01-01T00:00:00+00:00".into(),
                subject: "s".into(),
            }],
            branches: vec![BranchRow {
                name: "main".into(),
                head: "abc".into(),
                current: false,
                ahead: Some(0),
                behind: None,
            }],
            shelved: vec![ShelvedRow {
                selector: "stash@{0}".into(),
                subject: "wip".into(),
            }],
            position_log: vec![PositionRow {
                sha: "abc".into(),
                selector: "HEAD@{0}".into(),
                description: "checkout: moving from a to b".into(),
            }],
            truncated: vec!["commits".into()],
        };
        let json = serde_json::to_string(&history).expect("serialize");
        assert!(json.contains("\"detached\":true"));
        assert!(json.contains("\"position_log\""));

        let recovery = RepoRecovery {
            target: "master".into(),
            commits: vec![UnreferencedCommit {
                sha: "def".into(),
                author: "Ada".into(),
                date: "2026-01-01T00:00:00+00:00".into(),
                subject: "lost".into(),
                parents: vec!["abc".into()],
                sits_on_target: true,
            }],
            truncated: false,
            pipeline_commits_excluded: 2,
        };
        let json = serde_json::to_string(&recovery).expect("serialize");
        assert!(json.contains("\"sits_on_target\":true"));
        assert!(json.contains("\"pipeline_commits_excluded\":2"));
    }

    /// The three git readers are a LADDER, and each one's schema description
    /// is the only thing that tells a model which rung it is standing on.
    ///
    /// #2551 and #2538 diagnosed the same `fix-git` trial independently and
    /// landed minutes apart, so this surface is the residue of a merge rather
    /// than a decision (#2575). What the merge actually broke was not the
    /// overlap — `repo_recover` really does reach commits the other two
    /// cannot — it was the *disclosure*: `repo_status` grew `unreachable` and
    /// a shelved count while its description still named three of its ten
    /// payload fields, and `repo_recover`'s description went on sending
    /// callers to `repo_history` to rule out a stash that `repo_status` now
    /// answered in one git call. Three tools that overlap are a design; three
    /// whose descriptions do not say where the boundaries are is what makes
    /// selection guesswork, in exactly the situation these exist to serve.
    ///
    /// Asserted against the schema text because that is the only thing the
    /// model ever reads. A comment stating the same boundaries is what was
    /// already there, and it did not fail when they stopped being true.
    #[test]
    fn each_git_reader_names_its_boundary_and_the_next_rung() {
        let history_backend: Arc<dyn HistoryBackend> = Arc::new(GitCli);
        let repo_backend: Arc<dyn crate::repo::RepoBackend> = Arc::new(GitCli);
        let status = crate::repo::RepoStatusTool(repo_backend)
            .schema()
            .description;
        let history = RepoHistoryTool(history_backend.clone())
            .schema()
            .description;
        let recover = RepoRecoverTool(history_backend).schema().description;

        // Rung 1 must advertise the payload that makes it the first call —
        // these are the exact fields whose omission hid the fact that the
        // cheapest tool already answers "where did my commit go".
        for named in ["unreachable", "shelved", "ahead/behind", "FIRST"] {
            assert!(
                status.contains(named),
                "repo_status does not advertise `{named}`, so nothing tells a model the \
                 cheap tool answers it: {status}"
            );
        }
        for deeper in ["repo_history", "repo_recover"] {
            assert!(
                status.contains(deeper),
                "repo_status must name `{deeper}` as the next rung: {status}"
            );
        }

        // Rung 2 sends the cheap case back down and the object-store case up.
        assert!(
            history.contains("repo_status"),
            "repo_history must defer the facts repo_status answers more cheaply: {history}"
        );
        assert!(
            history.contains("repo_recover"),
            "repo_history must name the rung above it: {history}"
        );

        // Rung 3 defers to the CHEAPEST rung, not the middle one. The stale
        // sentence is named exactly rather than banning any mention of
        // `repo_history`, which would forbid a legitimate future reference.
        assert!(
            recover.contains("repo_status"),
            "repo_recover must name repo_status as its prerequisite: {recover}"
        );
        assert!(
            !recover.contains("`repo_history` showed"),
            "repo_recover routes the stash/detached check through repo_history again — \
             repo_status answers both in one git call (#2575): {recover}"
        );

        // …and it states what is only here, or it has no claim to be a tool
        // of its own rather than a flag on repo_status.
        for only_here in ["target", "sits_on_target", "fsck"] {
            assert!(
                recover.contains(only_here),
                "repo_recover does not state `{only_here}`, the capability that earns it a \
                 separate tool: {recover}"
            );
        }
    }

    /// The whole point of these two tools: they survive the read-only
    /// filter, so a role without `bash` can still read git state.
    #[test]
    fn both_tools_are_read_only() {
        let backend: Arc<dyn HistoryBackend> = Arc::new(GitCli);
        assert!(RepoHistoryTool(backend.clone()).schema().read_only);
        assert!(RepoRecoverTool(backend).schema().read_only);
    }

    #[tokio::test]
    async fn recover_refuses_a_target_that_is_an_option_or_multi_word() {
        let tool = RepoRecoverTool(Arc::new(GitCli));
        let root = Path::new(".");
        for bad in ["--all", "HEAD master", "-n 5"] {
            let out = tool
                .execute(&serde_json::json!({ "target": bad }), root)
                .await;
            assert!(
                matches!(out, ToolOutput::Error { .. }),
                "{bad:?} should be refused"
            );
        }
    }
}
