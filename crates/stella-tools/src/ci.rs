//! `ci_status` — read CI state for a branch, PR, or commit via the `gh`
//! CLI, including failure log tails. Read-only, so verifiers can verify "CI
//! is green" claims themselves; combined with the goal loop it powers
//! `stella monitor`: keep fixing until the checks pass.

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::exec;
// The crate's single POSIX single-quote escaper — ref names reach `gh`
// through a `bash -c` line, so they must never re-tokenize.
use crate::exec::shell_quote;
use crate::registry::Tool;

const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// `ci_status`'s `timeout_secs`, through the crate-wide clamp
/// ([`crate::exec::timeout_from`]): an unclamped model-supplied u64 would
/// disable the hang backstop for every `gh` sub-call (u64::MAX ≈ never).
fn ci_timeout(input: &Value) -> u64 {
    crate::exec::timeout_from(input, DEFAULT_TIMEOUT_SECS)
}

pub struct CiStatus;

#[async_trait]
impl Tool for CiStatus {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "ci_status".into(),
            description: "CI state via GitHub (gh). Give branch, pr, or commit; returns runs, \
                          conclusions, and a failure log tail scoped to the target's current \
                          head commit. wait=true blocks until every run for the target has \
                          completed, or timeout_secs expires — not just the newest-created one."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "branch": { "type": "string" },
                    "pr": { "type": "integer" },
                    "commit": { "type": "string" },
                    "wait": { "type": "boolean" },
                    "timeout_secs": { "type": "integer" }
                }
            }),
            read_only: true,
            // Reads through `gh`/the forge API — someone else's rate
            // limit, not free to hit twice per step (#923).
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput {
        let timeout_secs = ci_timeout(input);
        let wait = input.get("wait").and_then(|v| v.as_bool()).unwrap_or(false);

        if exec::run_github("command -v gh", root, 10)
            .await
            .map(|(c, _)| c)
            != Ok(0)
        {
            return ToolOutput::Error {
                message: "the `gh` CLI is required for ci_status and was not found on PATH".into(),
            };
        }

        let list_cmd = primary_command(input);

        let (code, mut report) = match exec::run_github(&list_cmd, root, timeout_secs).await {
            Ok(pair) => pair,
            Err(e) => return ToolOutput::Error { message: e },
        };
        if code != 0 {
            return ToolOutput::Error {
                message: format!("gh failed (exit {code}): {report}"),
            };
        }

        // The `gh run list` scope shared by the wait-loop and the
        // failure-log attach below, so both report on the SAME target the
        // top-level query resolved — an unscoped sub-query used to watch or
        // attach logs from an UNRELATED branch's most-recent run.
        let scope = gh_run_scope(input);

        // Optionally block until EVERY run for THIS target has completed
        // (not just the newest-created one — #1466), then re-list.
        if wait {
            let _ = exec::run_github(&wait_command(&scope), root, timeout_secs).await;
            if let Ok((0, fresh)) = exec::run_github(&list_cmd, root, timeout_secs).await {
                report = fresh;
            }
        }

        // Attach failure logs for THIS target's most recent failed run at
        // its CURRENT head commit, if any (#1470).
        let (_, failed_logs) =
            exec::run_github(&failure_log_command(&scope, input), root, timeout_secs)
                .await
                .unwrap_or((0, String::new()));

        ToolOutput::Ok {
            content: if failed_logs.trim().is_empty() {
                report
            } else {
                format!("{report}\n{failed_logs}")
            },
        }
    }

    // A `bash -c` composer joins the `command.started` fence (#804). The
    // gate sees the primary query line; the wait-watch and failure-log
    // sub-queries are same-target derivatives of the same input
    // ([`gh_run_scope`]) riding the same approval, and the `command -v gh`
    // probe is a constant.
    async fn command_for_gate(&self, input: &Value, _root: &std::path::Path) -> Option<String> {
        Some(primary_command(input))
    }
}

/// The primary `gh` line one `ci_status` call runs — a PR ref gets
/// `gh pr checks` (the richest view); branch/commit get `gh run list`
/// filtered accordingly. Shared by execute and the `command.started` gate so
/// the fence sees exactly the line that runs.
fn primary_command(input: &Value) -> String {
    if let Some(pr) = input.get("pr").and_then(|v| v.as_u64()) {
        format!("gh pr checks {pr} 2>&1 || true")
    } else if let Some(commit) = input.get("commit").and_then(|v| v.as_str()) {
        format!(
            "gh run list --commit {} --limit 15 \
             --json databaseId,name,status,conclusion,headBranch",
            shell_quote(commit)
        )
    } else {
        let branch = input
            .get("branch")
            .and_then(|v| v.as_str())
            .unwrap_or("main");
        format!(
            "gh run list --branch {} --limit 15 \
             --json databaseId,name,status,conclusion,headBranch",
            shell_quote(branch)
        )
    }
}

/// The `gh run list` filter flag that scopes a sub-query to the same target
/// (`--branch` / `--commit`, or a PR's resolved head branch) the top-level
/// `ci_status` query used — so the wait-loop and failure-log sub-queries can
/// never report on an unrelated branch's runs.
fn gh_run_scope(input: &Value) -> String {
    if let Some(pr) = input.get("pr").and_then(|v| v.as_u64()) {
        // Resolve the PR's head branch at run time (gh has no run-list-by-PR).
        format!("--branch \"$(gh pr view {pr} --json headRefName --jq .headRefName 2>/dev/null)\"")
    } else if let Some(commit) = input.get("commit").and_then(|v| v.as_str()) {
        format!("--commit {}", shell_quote(commit))
    } else {
        let branch = input
            .get("branch")
            .and_then(|v| v.as_str())
            .unwrap_or("main");
        format!("--branch {}", shell_quote(branch))
    }
}

/// The wait-loop composed as one shell line: repeatedly re-lists THIS
/// target's runs and polls until none remain incomplete (`status !=
/// "completed"`), relying on the caller's own `timeout_secs` (the
/// process-group kill in `exec::drive`) as the hang backstop rather than a
/// bound of its own.
///
/// Pulled out of `execute` so it is unit-testable the same way
/// `primary_command`/`gh_run_scope` are (#1466). The pre-fix version watched
/// only the single newest-CREATED run (`gh run list --limit 1` then `gh run
/// watch` on it) — when one push triggers several workflows, the
/// newest-created one is often a short job that is already done, so the
/// watch returned instantly while a long sibling run from the SAME push was
/// still in flight. This polls the WHOLE incomplete set for the target
/// instead of a single arbitrarily-chosen run. A `gh run list`/`jq` hiccup
/// yields an empty `$n`, which compares unequal to `"0"` — so a transient
/// failure keeps the loop waiting rather than falsely reporting "done".
fn wait_command(scope: &str) -> String {
    format!(
        "while :; do \
           n=$(gh run list {scope} --limit 15 --json status \
               --jq '[.[] | select(.status != \"completed\")] | length' 2>/dev/null); \
           [ \"$n\" = \"0\" ] && break; \
           sleep 5; \
         done"
    )
}

/// The shell expression resolving the target's CURRENT head commit SHA, used
/// to scope the failure-log sub-query so it can never attach logs from a run
/// the target has since moved past (#1470). Returns `None` for a `commit`
/// target: `gh run list --commit` already filters server-side to exactly
/// that SHA, so no extra filter is needed.
///
/// A `pr` target's head can move independently of `gh run list`'s creation
/// ordering, so it is read directly off the PR (`headRefOid`, resolved once,
/// same as `gh_run_scope`'s `headRefName` lookup for that target). A branch
/// target has no single authoritative "current" SHA short of another `gh`
/// round trip, so it falls back to the newest run's own `headSha` field —
/// `gh run list` already returns it for free.
fn head_sha_expr(input: &Value, scope: &str) -> Option<String> {
    if input.get("commit").and_then(|v| v.as_str()).is_some() {
        None
    } else if let Some(pr) = input.get("pr").and_then(|v| v.as_u64()) {
        Some(format!(
            "$(gh pr view {pr} --json headRefOid --jq .headRefOid 2>/dev/null)"
        ))
    } else {
        Some(format!(
            "$(gh run list {scope} --limit 1 --json headSha --jq '.[0].headSha' 2>/dev/null)"
        ))
    }
}

/// The composed failure-log sub-query: find THIS target's most recent failed
/// run and tail its logs, scoped to the target's current head commit
/// (#1470) — so a stale run from a since-superseded commit is never
/// attached beneath a fresh (possibly still-pending) status.
fn failure_log_command(scope: &str, input: &Value) -> String {
    match head_sha_expr(input, scope) {
        Some(sha_expr) => format!(
            "sha={sha_expr}; \
             id=$(gh run list {scope} --limit 15 --json databaseId,conclusion,headSha \
             --jq --arg sha \"$sha\" \
             '[.[] | select(.conclusion==\"failure\" and .headSha==$sha)][0].databaseId'); \
             if [ -n \"$id\" ] && [ \"$id\" != \"null\" ]; then \
               echo \"--- failure logs (run $id) ---\"; gh run view \"$id\" --log-failed 2>&1 | tail -120; \
             fi"
        ),
        None => format!(
            "id=$(gh run list {scope} --limit 15 --json databaseId,conclusion \
             --jq '[.[] | select(.conclusion==\"failure\")][0].databaseId'); \
             if [ -n \"$id\" ] && [ \"$id\" != \"null\" ]; then \
               echo \"--- failure logs (run $id) ---\"; gh run view \"$id\" --log-failed 2>&1 | tail -120; \
             fi"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_read_only_for_verifier_use() {
        assert!(CiStatus.schema().read_only);
    }

    #[test]
    fn sub_queries_are_scoped_to_the_requested_target() {
        // A branch request scopes by that branch…
        assert_eq!(
            gh_run_scope(&serde_json::json!({ "branch": "feat/x" })),
            "--branch 'feat/x'"
        );
        // …a commit request by that commit…
        assert_eq!(
            gh_run_scope(&serde_json::json!({ "commit": "abc123" })),
            "--commit 'abc123'"
        );
        // …a PR by its resolved head branch (never an unscoped list)…
        assert!(
            gh_run_scope(&serde_json::json!({ "pr": 42 })).contains("gh pr view 42"),
            "PR scope must resolve the head branch"
        );
        // …and an empty request defaults to main, still scoped (not global).
        assert_eq!(gh_run_scope(&serde_json::json!({})), "--branch 'main'");
    }

    #[test]
    fn timeout_is_clamped_to_the_exec_backstop() {
        assert_eq!(ci_timeout(&serde_json::json!({})), DEFAULT_TIMEOUT_SECS);
        // 0 means default (custom.rs's convention), not an instant timeout.
        assert_eq!(
            ci_timeout(&serde_json::json!({ "timeout_secs": 0 })),
            DEFAULT_TIMEOUT_SECS
        );
        assert_eq!(ci_timeout(&serde_json::json!({ "timeout_secs": 30 })), 30);
        // A u64::MAX request must not disable the hang backstop.
        assert_eq!(
            ci_timeout(&serde_json::json!({ "timeout_secs": u64::MAX })),
            crate::exec::MAX_TIMEOUT_SECS
        );
    }

    #[test]
    fn shell_quote_neutralizes_quotes() {
        assert_eq!(shell_quote("main"), "'main'");
        assert_eq!(shell_quote("a'b"), r"'a'\''b'");
    }

    // --- #1466: wait=true must watch every run for the target, not just ---
    // --- the newest-created one. -------------------------------------------

    #[test]
    fn wait_command_polls_the_whole_incomplete_set_instead_of_one_run() {
        let cmd = wait_command("--branch 'main'");
        // The pre-fix bug: `gh run list --limit 1` picked a single
        // newest-created run to `gh run watch`, which returns instantly if
        // THAT run happens to be a short sibling job while a long one from
        // the same push is still going. The fixed command must not select a
        // single run at all.
        assert!(
            // Trailing space matters: "--limit 15" also contains the bare
            // substring "--limit 1".
            !cmd.contains("--limit 1 "),
            "must not narrow to a single most-recently-created run: {cmd}"
        );
        assert!(
            !cmd.contains("gh run watch"),
            "must not delegate to watching one arbitrarily-chosen run: {cmd}"
        );
        // It must loop, re-listing THIS target's scope, until nothing is
        // left incomplete.
        assert!(cmd.contains("while"), "must loop: {cmd}");
        assert!(cmd.contains("--branch 'main'"), "must stay scoped: {cmd}");
        assert!(
            cmd.contains("status != \"completed\""),
            "must terminate on 'no incomplete runs', not one run's exit: {cmd}"
        );
    }

    #[test]
    fn wait_command_treats_a_list_failure_as_still_incomplete() {
        // An empty `$n` (gh/jq hiccup) must compare unequal to "0" so the
        // loop keeps waiting rather than falsely declaring done — the outer
        // `timeout_secs` process-group kill is the only backstop.
        let cmd = wait_command("--branch 'main'");
        assert!(cmd.contains(r#"[ "$n" = "0" ]"#), "{cmd}");
    }

    // --- #1470: the failure-log tail must be scoped to the target's ---
    // --- CURRENT head commit, not any recent failure on the branch. -------

    #[test]
    fn failure_log_command_filters_by_head_sha_for_a_branch_target() {
        let input = serde_json::json!({ "branch": "feat/x" });
        let scope = gh_run_scope(&input);
        let cmd = failure_log_command(&scope, &input);
        // The pre-fix bug: the failure query had no SHA filter at all, so it
        // could attach logs from a since-superseded commit's run.
        assert!(cmd.contains("headSha"), "must select on headSha: {cmd}");
        assert!(
            cmd.contains(r#".conclusion=="failure" and .headSha==$sha"#),
            "failure selection must be conjoined with the head-SHA match: {cmd}"
        );
    }

    #[test]
    fn failure_log_command_resolves_pr_head_via_head_ref_oid() {
        let input = serde_json::json!({ "pr": 42 });
        let scope = gh_run_scope(&input);
        let cmd = failure_log_command(&scope, &input);
        assert!(
            cmd.contains("gh pr view 42 --json headRefOid"),
            "a PR target's head SHA must be read off the PR itself: {cmd}"
        );
    }

    #[test]
    fn failure_log_command_skips_the_sha_filter_for_an_exact_commit_target() {
        // `gh run list --commit SHA` already scopes server-side to exactly
        // that commit — no extra client-side filter is needed, since there
        // is nothing else for a "current head" to mean here.
        let input = serde_json::json!({ "commit": "abc123" });
        let scope = gh_run_scope(&input);
        let cmd = failure_log_command(&scope, &input);
        assert!(!cmd.contains("headSha"), "{cmd}");
        assert_eq!(head_sha_expr(&input, &scope), None);
    }
}
