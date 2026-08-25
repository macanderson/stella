//! Reconciling the workspace's current-branch PR through `gh` — the read
//! half of the deck's PR monitor.
//!
//! Split out of `command_deck.rs` because that file is a grandfathered god
//! file closed to growth (AGENTS.md § "God files"): every function here is a
//! pure fold or a single `gh` invocation. The spawning half (`spawn_pr_monitor`,
//! which owns the deck's channels) lives beside the driver loop's other
//! channel-owning tasks in `driver_support`.

use std::time::Duration;

use serde_json::Value;
use stella_protocol::{CiStatus, PrStatus};

use super::driver_support::PrObservation;

/// Stable store tokens for PR/CI states (schema strings, not display).
pub(super) fn pr_status_token(status: PrStatus) -> &'static str {
    match status {
        PrStatus::Draft => "draft",
        PrStatus::Open => "open",
        PrStatus::Merged => "merged",
        PrStatus::Closed => "closed",
    }
}

pub(super) fn ci_status_token(ci: CiStatus) -> &'static str {
    match ci {
        CiStatus::Pending => "pending",
        CiStatus::Running => "running",
        CiStatus::Passing => "passing",
        CiStatus::Failing => "failing",
    }
}

/// Wall-clock ceiling on one `gh` invocation. `gh` talks to the network and
/// can wedge (captive portal, hung TLS handshake, a proxy that never answers);
/// without a deadline the deck's PR reconciliation waits forever on it.
const GH_CALL_TIMEOUT: Duration = Duration::from_secs(20);

async fn gh_json(root: &std::path::Path, args: &[&str]) -> Option<Value> {
    let mut command = tokio::process::Command::new("gh");
    command.args(args).current_dir(root).kill_on_drop(true);
    scrub_gh_command(&mut command);
    // Timing out is indistinguishable from every other failure here by design:
    // the signature already folds "no gh", "not authenticated" and "no PR" into
    // `None`, and `kill_on_drop` reaps the child when the timeout drops it.
    let output = tokio::time::timeout(GH_CALL_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    serde_json::from_slice(&output.stdout).ok()
}

pub(super) fn scrub_gh_command(command: &mut tokio::process::Command) {
    stella_tools::subprocess_env::scrub_sensitive_env_except(
        command,
        stella_tools::subprocess_env::GITHUB_CLI_AUTH_ENV_VARS,
    );
}

/// Reconcile the workspace's current-branch PR: `gh pr view` for identity
/// and state, `gh pr checks` for the aggregate CI verdict. `None` when no
/// PR exists for the branch (or `gh` is absent/unauthenticated).
pub(super) async fn observe_pr(root: &std::path::Path) -> Option<PrObservation> {
    let view = gh_json(root, &["pr", "view", "--json", "url,number,state,isDraft"]).await?;
    let url = view.get("url")?.as_str()?.to_string();
    let number = view.get("number").and_then(Value::as_u64);
    let is_draft = view
        .get("isDraft")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let status = match view.get("state").and_then(Value::as_str).unwrap_or("") {
        "MERGED" => PrStatus::Merged,
        "CLOSED" => PrStatus::Closed,
        _ if is_draft => PrStatus::Draft,
        _ => PrStatus::Open,
    };
    let ci = match gh_json(root, &["pr", "checks", "--json", "bucket"]).await {
        Some(Value::Array(rows)) => aggregate_ci(
            &rows
                .iter()
                .filter_map(|r| r.get("bucket").and_then(Value::as_str))
                .collect::<Vec<_>>(),
        ),
        _ => None,
    };
    Some(PrObservation {
        url,
        number,
        status,
        ci,
    })
}

/// Fold `gh pr checks` buckets into one verdict. Any failure wins; then
/// anything still moving; a fully-settled green set is passing. An empty
/// set is `None` — absence of checks must never render as passing.
fn aggregate_ci(buckets: &[&str]) -> Option<CiStatus> {
    if buckets.is_empty() {
        return None;
    }
    if buckets.iter().any(|b| matches!(*b, "fail" | "cancel")) {
        return Some(CiStatus::Failing);
    }
    if buckets.contains(&"pending") {
        return Some(CiStatus::Running);
    }
    Some(CiStatus::Passing)
}
