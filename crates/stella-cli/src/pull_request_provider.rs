// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The GitHub adapter behind [`PullRequestProvider`].
//!
//! This is the one place a pull request `gh` command is spelled. It is the
//! sibling of [`crate::issue_provider`], which does the same for issues.
//! Before it, the loop ran `gh pr view` and `gh pr merge` in its own
//! `deliver` module. It read two REST endpoints there too, and both of
//! GitHub's check dialects. So "which forge" was not a choice. It was
//! spelled into the reader.
//!
//! The I/O and the dialect moved here. The port is
//! [`stella_protocol::pull_request`]. The policy stays above it, in
//! `self_driving_cmd::deliver`.
//!
//! # `gh`, not the REST API
//!
//! Shelling out keeps one property. **Stella never holds a GitHub
//! credential.** `gh` owns the auth. The token never enters this process, and
//! `stella auth` stores nothing new. `doc:agent-native-delivery` §4.4's
//! `kind = "exec"` is the general form.

use std::process::Command;

use stella_protocol::pull_request::{
    Check, CheckOutcome, MergeStatus, PullRequest, PullRequestDraft, PullRequestError,
    PullRequestKey, PullRequestProvider, PullRequestState, PullRequestSummary, ReviewDecision,
};

/// The provider id this adapter answers to, and the one an error names.
pub(crate) const GITHUB: &str = "github";

/// Reads and drives GitHub pull requests through the `gh` CLI.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct GhPullRequests;

impl GhPullRequests {
    /// The adapter. It holds nothing: `gh` carries the repository and the
    /// credential.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self
    }
}

/// What `gh pr view --json …` and `gh pr list --json …` write.
///
/// This and [`GhCheck`] are the **only** types here that carry GitHub's field
/// names. Both are private to this module. `issue_provider.rs` confines the
/// issue names the same way.
#[derive(Debug, Default, serde::Deserialize)]
struct GhPullRequest {
    #[serde(default)]
    number: u64,
    #[serde(default, rename = "isDraft")]
    is_draft: bool,
    #[serde(default)]
    mergeable: String,
    #[serde(default, rename = "reviewDecision")]
    review_decision: String,
    #[serde(default, rename = "statusCheckRollup")]
    status_check_rollup: Vec<GhCheck>,
    #[serde(default, rename = "baseRefName")]
    base_ref_name: String,
    #[serde(default, rename = "headRefName")]
    head_ref_name: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    labels: Vec<GhLabel>,
}

/// A label, which GitHub reports as an object rather than a string.
#[derive(Debug, Default, serde::Deserialize)]
struct GhLabel {
    #[serde(default)]
    name: String,
}

/// One check as GitHub reports it.
///
/// # Two kinds of check, two spellings
///
/// A rollup mixes **check runs** with **commit statuses**. A check run carries
/// `name`, `status` and `conclusion`. A commit status carries `context` and
/// `state`, and no `status` at all. Third-party services post those.
///
/// Reading only the first shape does not fail. Every field is
/// `#[serde(default)]`, so it yields a check with no name and no outcome.
///
/// Such a check reads as *pending forever*. Its `status` is never
/// `COMPLETED`. This repository carries a Vercel commit status that fails on
/// every pull request. The loop re-read `ci=Pending` for twenty-five minutes
/// after all three required checks went green.
///
/// The empty name is the same bug's other half. A base read joins by name, so
/// every commit status would join against every other.
#[derive(Debug, Default, Clone, serde::Deserialize)]
struct GhCheck {
    /// A check run's name.
    #[serde(default)]
    name: String,
    /// A commit status's name. GitHub calls the same thing `context` here.
    #[serde(default)]
    context: String,
    /// A check run's outcome: `SUCCESS`, `FAILURE`, `SKIPPED`, `""` while
    /// running.
    #[serde(default)]
    conclusion: String,
    /// A commit status's outcome: `SUCCESS`, `FAILURE`, `ERROR`, `PENDING`,
    /// `EXPECTED`.
    #[serde(default)]
    state: String,
    /// A check run's progress: `COMPLETED`, `IN_PROGRESS`, `QUEUED`. **Absent
    /// on a commit status**, which is how the two are told apart.
    #[serde(default)]
    status: String,
}

impl GhCheck {
    /// The join key, whichever shape reported it.
    fn name(&self) -> &str {
        if self.name.is_empty() {
            &self.context
        } else {
            &self.name
        }
    }

    /// The outcome, whichever shape reported it.
    fn raw_outcome(&self) -> String {
        let raw = if self.conclusion.is_empty() {
            &self.state
        } else {
            &self.conclusion
        };
        raw.trim().to_ascii_uppercase()
    }

    /// Where this check got to, in the port's vocabulary.
    ///
    /// Inert wins over failed, and failed wins over running. That is the order
    /// a rollup verdict reads. A skipped job is not a failure. A build that
    /// already failed stays failed while other jobs run.
    fn outcome(&self) -> CheckOutcome {
        let raw = self.raw_outcome();
        if matches!(raw.as_str(), "SKIPPED" | "NEUTRAL" | "CANCELLED") {
            return CheckOutcome::Inert;
        }
        if matches!(
            raw.as_str(),
            "FAILURE" | "ERROR" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED"
        ) {
            return CheckOutcome::Failed;
        }
        // A commit status has no progress field. Its outcome is the whole
        // story. A check run has one. A completed run with no conclusion is a
        // forge quirk. Read it as unknown, not as green.
        let running = if self.status.trim().is_empty() {
            matches!(raw.as_str(), "PENDING" | "EXPECTED" | "")
        } else {
            !self.status.eq_ignore_ascii_case("COMPLETED") || raw.is_empty()
        };
        if running {
            CheckOutcome::Running
        } else {
            CheckOutcome::Passed
        }
    }

    /// The port's shape.
    fn into_check(self) -> Check {
        Check {
            name: self.name().to_owned(),
            outcome: self.outcome(),
        }
    }
}

/// Map GitHub's mergeability string.
///
/// `UNKNOWN` and anything else read as [`MergeStatus::Unknown`], never
/// `Clean`. GitHub reports the field before it has worked it out. Reading
/// that as clean is how a merge is tried into a conflict.
fn merge_status_from(raw: &str) -> MergeStatus {
    match raw.to_ascii_uppercase().as_str() {
        "MERGEABLE" => MergeStatus::Clean,
        "CONFLICTING" => MergeStatus::Conflicted,
        _ => MergeStatus::Unknown,
    }
}

/// Map GitHub's review decision.
///
/// An empty string means nobody has reviewed. GitHub returns it when a
/// repository asks for no review.
fn review_from(raw: &str) -> ReviewDecision {
    match raw.to_ascii_uppercase().as_str() {
        "APPROVED" => ReviewDecision::Approved,
        "CHANGES_REQUESTED" => ReviewDecision::ChangesRequested,
        _ => ReviewDecision::None,
    }
}

/// Map GitHub's pull request state.
///
/// Anything else reads as [`PullRequestState::Open`], which keeps a caller
/// asking. Guessing `Merged` from an unknown word would strand a live pull
/// request.
fn state_from(raw: &str) -> PullRequestState {
    match raw.to_ascii_uppercase().as_str() {
        "MERGED" => PullRequestState::Merged,
        "CLOSED" => PullRequestState::Closed,
        _ => PullRequestState::Open,
    }
}

impl From<GhPullRequest> for PullRequest {
    fn from(row: GhPullRequest) -> Self {
        Self {
            key: PullRequestKey(row.number.to_string()),
            state: state_from(&row.state),
            draft: row.is_draft,
            base_ref: row.base_ref_name,
            head_ref: row.head_ref_name,
            title: row.title,
            body: row.body,
            merge_status: merge_status_from(&row.mergeable),
            review: review_from(&row.review_decision),
            checks: row
                .status_check_rollup
                .into_iter()
                .map(GhCheck::into_check)
                .collect(),
            labels: row.labels.into_iter().map(|label| label.name).collect(),
        }
    }
}

/// The workflow-run id inside a check's link, if it has one.
///
/// A check link looks like `…/actions/runs/<run>/job/<job>`. Anything else
/// yields `None` rather than a guess. A third-party check points at its own
/// dashboard. Re-running the wrong id is worse than re-running nothing.
#[must_use]
fn run_id_from_link(link: &str) -> Option<String> {
    let after = link.split("/actions/runs/").nth(1)?;
    let id = after.split('/').next()?;
    if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
        Some(id.to_owned())
    } else {
        None
    }
}

/// Run `gh` and return stdout, with colour forced off.
fn gh(args: &[&str]) -> Result<String, PullRequestError> {
    let out = Command::new("gh")
        .args(args)
        .env("NO_COLOR", "1")
        .env("CLICOLOR_FORCE", "0")
        .output()
        .map_err(|error| PullRequestError::Unavailable {
            provider: GITHUB.into(),
            reason: format!("could not run `gh`: {error} — is the GitHub CLI installed?"),
        })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(PullRequestError::Failed {
            provider: GITHUB.into(),
            reason: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        })
    }
}

/// Run `gh` and return stdout even when the command exits non-zero.
///
/// Some `gh` subcommands report through the exit code and still write the
/// payload. `gh pr checks` exits 1 on a failing check and 8 on a pending one.
/// It writes its `--json` output either way. There a non-zero exit is a
/// verdict about the checks, not a failed command, so the payload has to
/// survive it. stderr is surfaced only when stdout came back empty. That is
/// the one case where the command itself could not run.
fn gh_stdout(args: &[&str]) -> Result<String, PullRequestError> {
    let out = Command::new("gh")
        .args(args)
        .env("NO_COLOR", "1")
        .env("CLICOLOR_FORCE", "0")
        .output()
        .map_err(|error| PullRequestError::Unavailable {
            provider: GITHUB.into(),
            reason: format!("could not run `gh`: {error} — is the GitHub CLI installed?"),
        })?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if stdout.is_empty() && !out.status.success() {
        return Err(PullRequestError::Failed {
            provider: GITHUB.into(),
            reason: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    Ok(stdout)
}

/// What a payload this build cannot read is called.
fn malformed(what: &str, error: &impl std::fmt::Display) -> PullRequestError {
    PullRequestError::Malformed {
        provider: GITHUB.into(),
        reason: format!("`{what}`: {error}"),
    }
}

/// Run one `gh api` read and decode a [`GhCheck`] per line.
///
/// **The exit status is the read's verdict.** An empty stdout is not. A 404,
/// an expired token and a rate limit all run `gh`, exit non-zero, and print
/// little or nothing. `--paginate` widens it. A run whose third page fails
/// still has the first two on stdout, so a part of the list would come back
/// as the whole of it.
///
/// It matters because a base read that came back empty reads as "the base
/// does not excuse this failure". That answer costs a fix turn on somebody
/// else's breakage.
fn read_checks(endpoint: &str, jq: &str) -> Result<Vec<GhCheck>, PullRequestError> {
    let out = Command::new("gh")
        .args(["api", endpoint, "--paginate", "--jq", jq])
        .env("NO_COLOR", "1")
        .output()
        .map_err(|error| PullRequestError::Unavailable {
            provider: GITHUB.into(),
            reason: format!("could not run `gh`: {error} — is the GitHub CLI installed?"),
        })?;
    if !out.status.success() {
        return Err(PullRequestError::Failed {
            provider: GITHUB.into(),
            reason: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<GhCheck>(line).ok())
        .collect())
}

/// The endpoint carrying a ref's check runs.
///
/// Split out so a test can see the one thing that can be wrong here: *which
/// commit gets asked about*.
#[must_use]
fn check_runs_endpoint(git_ref: &str) -> String {
    format!("repos/{{owner}}/{{repo}}/commits/{git_ref}/check-runs")
}

/// The endpoint carrying a ref's commit statuses.
///
/// A second endpoint, not a second field. GitHub never merged the two APIs,
/// and `check-runs` holds no commit status.
#[must_use]
fn statuses_endpoint(git_ref: &str) -> String {
    format!("repos/{{owner}}/{{repo}}/commits/{git_ref}/status")
}

impl PullRequestProvider for GhPullRequests {
    fn id(&self) -> &str {
        GITHUB
    }

    fn open(&self, draft: &PullRequestDraft) -> Result<PullRequestKey, PullRequestError> {
        let mut args = vec![
            "pr",
            "create",
            "--head",
            &draft.head_ref,
            "--title",
            &draft.title,
            "--body",
            &draft.body,
        ];
        if draft.draft {
            args.push("--draft");
        }
        let url = gh(&args)?;
        url.rsplit('/')
            .next()
            .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
            .map(|s| PullRequestKey(s.to_owned()))
            .ok_or_else(|| PullRequestError::Malformed {
                provider: GITHUB.into(),
                reason: format!("`gh pr create` printed no pull request number: {url:?}"),
            })
    }

    fn get(&self, key: &PullRequestKey) -> Result<PullRequest, PullRequestError> {
        let raw = gh(&[
            "pr",
            "view",
            key.as_str(),
            "--json",
            "number,isDraft,mergeable,reviewDecision,statusCheckRollup,baseRefName,headRefName,\
             title,body,state,labels",
        ])?;
        let row: GhPullRequest =
            serde_json::from_str(&raw).map_err(|error| malformed("gh pr view", &error))?;
        // `gh pr view` echoes the number it was asked about. A payload that
        // dropped the field would decode as pull request 0, and every later
        // call would address the wrong one.
        let mut pr = PullRequest::from(row);
        pr.key = key.clone();
        Ok(pr)
    }

    fn list_open(&self, search: Option<&str>) -> Result<Vec<PullRequestSummary>, PullRequestError> {
        let mut args = vec![
            "pr",
            "list",
            "--state",
            "open",
            "--json",
            "number,title,body,headRefName",
        ];
        if let Some(search) = search {
            args.extend_from_slice(&["--search", search]);
        }
        let raw = gh(&args)?;
        let rows: Vec<GhPullRequest> =
            serde_json::from_str(&raw).map_err(|error| malformed("gh pr list", &error))?;
        // A row with no `number` is skipped rather than failing the whole
        // read. It decodes as 0, which is no pull request, and passing that on
        // would hand a caller a key that addresses nothing.
        Ok(rows
            .into_iter()
            .filter(|row| row.number != 0)
            .map(|row| PullRequestSummary {
                key: PullRequestKey(row.number.to_string()),
                title: row.title,
                body: row.body,
                head_ref: row.head_ref_name,
            })
            .collect())
    }

    fn checks_for_ref(&self, git_ref: &str) -> Result<Vec<Check>, PullRequestError> {
        let mut checks = read_checks(
            &check_runs_endpoint(git_ref),
            ".check_runs[] | {name: .name, conclusion: (.conclusion // \"\"), status: .status}",
        )?;

        // Commit statuses sit behind a second endpoint and speak a second
        // dialect. They are read apart and appended, because a rollup holds
        // both. A base showing only half of it would fail to excuse the very
        // checks most likely to be broken repository-wide: a third-party
        // service is what posts a commit status. A failed second read leaves
        // that half, so its failure fails the pair.
        checks.extend(read_checks(
            &statuses_endpoint(git_ref),
            ".statuses[] | {context: .context, state: .state}",
        )?);
        Ok(checks.into_iter().map(GhCheck::into_check).collect())
    }

    fn required_checks(&self, branch: &str) -> Result<Vec<String>, PullRequestError> {
        let raw = gh(&[
            "api",
            &format!("repos/{{owner}}/{{repo}}/branches/{branch}/protection"),
            "--jq",
            ".required_status_checks.contexts[]?",
        ])?;
        Ok(raw
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn recent_commits(&self, branch: &str, depth: usize) -> Result<Vec<String>, PullRequestError> {
        let raw = gh(&[
            "api",
            &format!("repos/{{owner}}/{{repo}}/commits?sha={branch}&per_page={depth}"),
            "--jq",
            ".[].sha",
        ])?;
        Ok(raw
            .lines()
            .map(str::trim)
            .filter(|sha| !sha.is_empty())
            .take(depth)
            .map(str::to_owned)
            .collect())
    }

    fn mark_ready(&self, key: &PullRequestKey) -> Result<(), PullRequestError> {
        gh(&["pr", "ready", key.as_str()]).map(|_| ())
    }

    fn add_label(&self, key: &PullRequestKey, label: &str) -> Result<(), PullRequestError> {
        gh(&["pr", "edit", key.as_str(), "--add-label", label]).map(|_| ())
    }

    fn merge(&self, key: &PullRequestKey) -> Result<(), PullRequestError> {
        // No `--delete-branch`. It deletes the *local* branch too, and this
        // loop works inside worktrees that hold those branches. So the delete
        // fails, `gh` exits non-zero, and a merge that already worked is
        // reported as a failure. That is what it did. A pull request merged,
        // and the loop went on re-reading it, because the branch cleanup after
        // the merge had failed.
        //
        // Cleanup is not delivery. The forge's own "automatically delete head
        // branches" setting does it with no local side effect. Losing a branch
        // can be undone. Losing the record of a merge strands the pull request
        // for good.
        let outcome = gh(&["pr", "merge", key.as_str(), "--squash"]).map(|_| ());

        // A pull request that is already merged is the state this asked for.
        // So it is a success. Racing a human who merged it by hand must not
        // read as an error. Nor must a re-read after a part-done merge.
        match outcome {
            Err(error)
                if error
                    .to_string()
                    .to_ascii_lowercase()
                    .contains("already merged") =>
            {
                Ok(())
            }
            other => other,
        }
    }

    fn sync_with_base(&self, key: &PullRequestKey) -> Result<(), PullRequestError> {
        gh(&["pr", "update-branch", key.as_str()]).map(|_| ())
    }

    fn rerun_failed_checks(&self, key: &PullRequestKey) -> Result<(), PullRequestError> {
        // `gh pr checks --json` names each check's run only in its URL, so
        // the run id is read from there.
        //
        // `gh pr checks` exits non-zero whenever the checks are not all green.
        // It exits 1 on a failure and 8 while any are still pending, and it
        // writes the JSON either way. The plain helper reads that as an error
        // and throws the payload away. So it cannot be used here. This only
        // ever runs on a red pull request, which is exactly when the exit code
        // is non-zero.
        let raw = gh_stdout(&["pr", "checks", key.as_str(), "--json", "state,link"])?;

        #[derive(serde::Deserialize)]
        struct Row {
            #[serde(default)]
            state: String,
            #[serde(default)]
            link: String,
        }

        let rows: Vec<Row> =
            serde_json::from_str(&raw).map_err(|error| malformed("gh pr checks", &error))?;

        let mut runs: Vec<String> = rows
            .iter()
            .filter(|row| row.state.eq_ignore_ascii_case("FAILURE"))
            .filter_map(|row| run_id_from_link(&row.link))
            .collect();
        runs.sort();
        runs.dedup();

        if runs.is_empty() {
            return Err(PullRequestError::Failed {
                provider: GITHUB.into(),
                reason: "no failed check named a workflow run to re-run".to_owned(),
            });
        }

        for run in &runs {
            gh(&["run", "rerun", run, "--failed"])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_run(name: &str, conclusion: &str) -> GhCheck {
        GhCheck {
            name: name.into(),
            conclusion: conclusion.into(),
            status: "COMPLETED".into(),
            ..GhCheck::default()
        }
    }

    /// A commit status is a concluded check, not a pending one.
    ///
    /// **The witness for the bug that stalled the loop.** A rollup mixes two
    /// dialects. A check run carries `name`, `status` and `conclusion`. A
    /// commit status carries `context` and `state`, and no `status`. Reading
    /// only the first shape does not fail. Every field defaults, so it yields
    /// a nameless check with no outcome. That check reads as unfinished on
    /// every poll, for ever.
    ///
    /// That is what happened. This repository's Vercel commit status had
    /// already concluded `FAILURE`. The loop re-read `ci=Pending` for
    /// twenty-five minutes after all three required checks went green.
    #[test]
    fn a_commit_status_is_read_as_concluded_not_as_pending() {
        let vercel = GhCheck {
            context: "Vercel".into(),
            state: "FAILURE".into(),
            ..GhCheck::default()
        };
        assert_eq!(vercel.name(), "Vercel", "it must join by its context");
        assert_eq!(vercel.outcome(), CheckOutcome::Failed);
    }

    /// A commit status that really is pending still reads as pending.
    ///
    /// The other way round, so the fix above cannot be "call every commit
    /// status finished".
    #[test]
    fn a_pending_commit_status_still_reads_as_pending() {
        let waiting = GhCheck {
            context: "Vercel".into(),
            state: "PENDING".into(),
            ..GhCheck::default()
        };
        assert_eq!(waiting.outcome(), CheckOutcome::Running);
    }

    /// A completed check run with no conclusion is unknown, not green.
    #[test]
    fn a_completed_check_with_no_conclusion_is_still_running() {
        assert_eq!(check_run("ci", "").outcome(), CheckOutcome::Running);
        assert_eq!(check_run("ci", "SUCCESS").outcome(), CheckOutcome::Passed);
    }

    /// A skipped job did not run, so it is neither a pass nor a failure.
    ///
    /// Counting it as pending would stall the loop on a job that is skipped
    /// by design. This repository skips several on a docs-only diff.
    #[test]
    fn a_skipped_check_is_inert() {
        assert_eq!(check_run("docs", "SKIPPED").outcome(), CheckOutcome::Inert);
        assert_eq!(
            check_run("docs", "CANCELLED").outcome(),
            CheckOutcome::Inert
        );
    }

    /// A service that broke while running its own check is a failure.
    #[test]
    fn a_check_that_errored_counts_as_failed() {
        for raw in ["FAILURE", "ERROR", "TIMED_OUT", "STARTUP_FAILURE"] {
            assert_eq!(
                check_run("ci", raw).outcome(),
                CheckOutcome::Failed,
                "{raw} is a failure"
            );
        }
    }

    /// The base is asked about by branch name, so no local ref can go stale.
    ///
    /// A remote-tracking name resolves against the last fetch, and this loop
    /// never fetches. A base that broke while the process was up would still
    /// resolve to the older commit. Then a failure the base handed down gets
    /// scored as the pull request's own. One pull request was escalated for a
    /// `cargo fmt` diff in a file it never touched.
    ///
    /// So the check is on the *absence* of `origin/`. That name is what brings
    /// the staleness back.
    #[test]
    fn the_base_is_named_to_the_forge_not_a_remote_tracking_ref() {
        let endpoint = check_runs_endpoint("main");
        assert_eq!(endpoint, "repos/{owner}/{repo}/commits/main/check-runs");
        assert!(
            !endpoint.contains("origin/"),
            "a remote-tracking ref resolves against the last fetch, not the forge: {endpoint}"
        );
    }

    /// A check link naming a workflow run yields its id, and anything else
    /// yields nothing.
    #[test]
    fn only_an_actions_link_names_a_run_to_rerun() {
        assert_eq!(
            run_id_from_link("https://github.com/o/r/actions/runs/12345/job/9"),
            Some("12345".to_owned())
        );
        assert_eq!(run_id_from_link("https://vercel.com/dashboard"), None);
        assert_eq!(run_id_from_link(""), None);
    }

    /// The whole payload decodes into the port's shape.
    #[test]
    fn a_gh_pr_view_payload_maps_onto_a_pull_request() {
        let raw = r#"{
            "number": 4022,
            "isDraft": true,
            "mergeable": "MERGEABLE",
            "reviewDecision": "",
            "baseRefName": "main",
            "headRefName": "fix/4022-x",
            "title": "a title",
            "body": "Closes #4022",
            "state": "OPEN",
            "labels": [{"name": "stella-verified-locally"}],
            "statusCheckRollup": [
                {"name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"context": "Vercel", "state": "FAILURE"}
            ]
        }"#;
        let row: GhPullRequest = serde_json::from_str(raw).expect("decode");
        let pr = PullRequest::from(row);

        assert_eq!(pr.key, PullRequestKey::from("4022"));
        assert_eq!(pr.state, PullRequestState::Open);
        assert!(pr.draft);
        assert_eq!(pr.base_ref, "main");
        assert_eq!(pr.head_ref, "fix/4022-x");
        assert_eq!(pr.merge_status, MergeStatus::Clean);
        assert_eq!(pr.review, ReviewDecision::None);
        assert_eq!(pr.labels, vec!["stella-verified-locally".to_owned()]);
        assert_eq!(
            pr.checks,
            vec![
                Check {
                    name: "ci".to_owned(),
                    outcome: CheckOutcome::Passed
                },
                Check {
                    name: "Vercel".to_owned(),
                    outcome: CheckOutcome::Failed
                },
            ]
        );
    }

    /// A listing row with no number is skipped, not passed on as zero.
    ///
    /// `number` defaults, so a payload that dropped it decodes as pull request
    /// 0. That key addresses nothing. Handing it to a caller would turn a
    /// dropped field into a pull request the loop then tries to read.
    #[test]
    fn a_listing_row_with_no_number_is_skipped() {
        let raw = r#"[
            {"title": "no number here", "body": "Closes #43"},
            {"number": 9, "title": "t", "body": "Closes #43", "headRefName": "fix/43"}
        ]"#;
        let rows: Vec<GhPullRequest> = serde_json::from_str(raw).expect("decode");
        let kept: Vec<String> = rows
            .into_iter()
            .filter(|row| row.number != 0)
            .map(|row| row.number.to_string())
            .collect();
        assert_eq!(kept, vec!["9".to_owned()]);
    }

    /// An unknown state keeps the caller asking. It never calls a pull
    /// request finished.
    #[test]
    fn an_unknown_state_reads_as_open() {
        assert_eq!(state_from("MERGED"), PullRequestState::Merged);
        assert_eq!(state_from("CLOSED"), PullRequestState::Closed);
        assert_eq!(state_from("DRAFTING"), PullRequestState::Open);
    }

    /// Mergeability the forge has not computed is unknown, never clean.
    #[test]
    fn unknown_mergeability_is_not_clean() {
        assert_eq!(merge_status_from("MERGEABLE"), MergeStatus::Clean);
        assert_eq!(merge_status_from("CONFLICTING"), MergeStatus::Conflicted);
        assert_eq!(merge_status_from("UNKNOWN"), MergeStatus::Unknown);
        assert_eq!(merge_status_from(""), MergeStatus::Unknown);
    }
}
