// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The standing deploy watch.
//!
//! The base watch guards `main`. This one guards the release path. A red
//! release run blocks every ship, and it is nobody's one change. So the
//! answer has the base watch's shape: file the issue if none is open, adopt
//! it, and let the work path fix it. `deploy_watch = "off"` in `stella.toml`
//! stands the watch down.

use stella_autonomy::IssueRef;

use super::{Audit, Durable, audit};

/// The workflow file the watch reads.
///
/// A repository without it has no release path to watch. The read below
/// answers `None` there, so the watch sits inert rather than wrong.
const RELEASE_WORKFLOW: &str = "release.yml";

/// The latest completed release run, as the forge tells it.
struct ReleaseRun {
    conclusion: String,
    url: String,
}

impl ReleaseRun {
    /// Whether this run's conclusion is an emergency.
    ///
    /// `cancelled`, `skipped` and `neutral` are not red. Each is a person's
    /// call about one run, not proof the release path broke. Filing on them
    /// would teach humans to ignore the filings.
    fn is_red(&self) -> bool {
        matches!(
            self.conclusion.as_str(),
            "failure" | "timed_out" | "startup_failure"
        )
    }
}

/// One pass of the watch: look, and file-and-adopt on red.
///
/// Anything short of "red and unfiled" is a quiet no-op. The watch runs on
/// the poll cadence, and one that talks every pass is noise. The filing
/// dedups through the tracker label. A restarted process finds the issue
/// already open and files nothing — the contract the base watch keeps.
pub(super) fn pass(
    durable: &Durable,
    provider: &crate::issue_provider::GhIssueProvider,
    state: &mut stella_autonomy::LoopState,
    attribution: &stella_autonomy::Attribution,
) {
    let Some(run) = latest_release_run() else {
        return;
    };
    if !run.is_red() {
        return;
    }

    match crate::self_driving_cmd::backlog::file_deploy_breakage(
        provider,
        RELEASE_WORKFLOW,
        &run.url,
        attribution,
    ) {
        Ok(Some(key)) => {
            audit::record(
                durable,
                Audit::FiledDeployBreakage,
                Some(&key),
                "the release workflow is red and nobody had filed it",
            );
            // Adopted the way a base breakage is. A broken release path
            // outranks queued work. The work path takes it from here.
            audit::record(
                durable,
                Audit::Claimed,
                Some(&key),
                "adopting the deploy breakage — nothing ships until it is fixed",
            );
            state.claimed.push(IssueRef(key));
        }
        // Already filed. The issue carries a defect kind and the most
        // urgent rung, so the ranked queue brings it to a worker.
        Ok(None) => {}
        Err(error) => audit::record(
            durable,
            Audit::Transient,
            None,
            &format!("the release workflow is red and this could not file it ({error})"),
        ),
    }
}

/// The latest completed release run, or `None` when nobody can say.
///
/// It degrades the way the base watch does. An unreadable forge — no `gh`,
/// no such workflow, a rate limit — makes the watch wait. It must never file
/// a report about nothing. Completed runs only: a run still going has not
/// answered yet, and the one before it already did.
fn latest_release_run() -> Option<ReleaseRun> {
    let out = std::process::Command::new("gh")
        .args([
            "api",
            &format!(
                "repos/{{owner}}/{{repo}}/actions/workflows/{RELEASE_WORKFLOW}/runs?status=completed&per_page=1"
            ),
            "--jq",
            ".workflow_runs[0] | {conclusion: (.conclusion // \"\"), url: (.html_url // \"\")}",
        ])
        .env("NO_COLOR", "1")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let row: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).ok()?;
    Some(ReleaseRun {
        conclusion: row.get("conclusion")?.as_str()?.to_owned(),
        url: row.get("url")?.as_str()?.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a conclusion that says the path broke counts as red. A cancelled
    /// or skipped run is somebody's call, not an outage.
    #[test]
    fn only_a_broken_conclusion_reads_as_red() {
        let run = |conclusion: &str| ReleaseRun {
            conclusion: conclusion.into(),
            url: String::new(),
        };
        assert!(run("failure").is_red());
        assert!(run("timed_out").is_red());
        assert!(run("startup_failure").is_red());
        assert!(!run("success").is_red());
        assert!(!run("cancelled").is_red());
        assert!(!run("skipped").is_red());
        assert!(!run("").is_red(), "an unanswerable run is not an emergency");
    }
}
