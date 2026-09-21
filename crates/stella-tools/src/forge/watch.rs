//! `watch_ci`: what CI says about one branch.
//!
//! The eleventh verb, and the only one that reads. It answers the question
//! every other verb here waits on: is it green yet?
//!
//! # One reading per call
//!
//! `gh run watch` blocks a terminal until the run ends. This tool blocks
//! nothing. It reads the checks once and says where they got to. The agent
//! picks when to look again.
//!
//! That is what splits a tool from a terminal command. A watch that blocks
//! inside a tool call spends the turn's wall clock on waiting. It makes no
//! tokens while it waits, and the thing that steers the turn cannot break in.
//! A reading costs one round trip and leaves the choice where choices belong.
//!
//! # Checks belong to a branch
//!
//! Checks run on a ref. A pull request puts commits on a ref, and so does a
//! push to `main`. Take the branch and one tool answers for both, which is
//! what the driver asked for. Most of the time a pull request is in play.
//! When the target is `main`, none is.

use async_trait::async_trait;
use serde_json::{Value, json};
use stella_protocol::pull_request::{Check, CheckOutcome};
use stella_protocol::tool::{ErrorClass, ToolOutput, ToolSchema};

use super::{ForgeSlots, required};
use crate::registry::Tool;

/// `watch_ci`: one branch's checks, and the verdict they add up to.
pub struct WatchCi {
    slots: ForgeSlots,
}

impl WatchCi {
    /// Build the tool over the host's slots.
    #[must_use]
    pub fn new(slots: ForgeSlots) -> Self {
        Self { slots }
    }
}

#[async_trait]
impl Tool for WatchCi {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "watch_ci".into(),
            description: "Report the CI checks on a branch: each check's name and where it got \
                 to, and whether the branch is green, red, or still running. Use this instead of \
                 `gh pr checks` or `gh run watch` in bash. It reads once and returns — it does \
                 not block waiting for a run to finish, so call it again to see movement."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "branch": {
                        "type": "string",
                        "description": "The branch to read checks for, such as the head branch \
                                        of your pull request, or `main`."
                    }
                },
                "required": ["branch"]
            }),
            read_only: true,
            // It reads no workspace state and changes nothing. But each call
            // is a forge request against a rate limit the user shares with
            // their own work. A guessed call the step then throws away spends
            // that budget for nothing.
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, _ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        let branch = match required(input, "branch") {
            Ok(branch) => branch,
            Err(refusal) => return refusal,
        };
        let forge = match self.slots.forge() {
            Ok(forge) => forge,
            Err(refusal) => return refusal,
        };
        match forge.checks_for_ref(branch) {
            Ok(checks) => render(branch, &checks),
            Err(error) => ToolOutput::classified_error(ErrorClass::Environment, error.to_string()),
        }
    }
}

/// The verdict a set of checks adds up to.
///
/// The order of these tests matters, and it is not the order of the names. A
/// failure is the verdict even while a check is still running. Nothing that
/// ends later can make a failed check pass. Say "running" there and the agent
/// goes back to wait for news that has already come in.
fn verdict(checks: &[Check]) -> &'static str {
    if checks.is_empty() {
        return "no checks";
    }
    if checks
        .iter()
        .any(|check| check.outcome == CheckOutcome::Failed)
    {
        return "red";
    }
    if checks
        .iter()
        .any(|check| check.outcome == CheckOutcome::Running)
    {
        return "running";
    }
    // Everything left has passed or is inert. All inert is not green, because
    // no check ran at all. Call it green and a branch whose workflows all
    // skipped gets merged as verified.
    if checks
        .iter()
        .any(|check| check.outcome == CheckOutcome::Passed)
    {
        "green"
    } else {
        "nothing ran"
    }
}

/// How one outcome reads in the report.
fn outcome(outcome: CheckOutcome) -> &'static str {
    match outcome {
        CheckOutcome::Passed => "passed",
        CheckOutcome::Failed => "FAILED",
        CheckOutcome::Running => "running",
        CheckOutcome::Inert => "skipped",
    }
}

/// The report the model reads, plus the same facts as structured data.
fn render(branch: &str, checks: &[Check]) -> ToolOutput {
    let verdict = verdict(checks);
    let mut content = format!("{branch}: {verdict}");
    // Failures first. Most calls to this tool are asking what broke. A red
    // check eleven rows down a list of passes is easy to miss, and the reader
    // should not have to hunt for it.
    let mut sorted: Vec<&Check> = checks.iter().collect();
    sorted.sort_by_key(|check| match check.outcome {
        CheckOutcome::Failed => 0,
        CheckOutcome::Running => 1,
        CheckOutcome::Passed => 2,
        CheckOutcome::Inert => 3,
    });
    for check in &sorted {
        content.push_str(&format!("\n  {} — {}", outcome(check.outcome), check.name));
    }
    if checks.is_empty() {
        content.push_str(
            "\n  Nothing has reported on this ref. It may not be pushed, or this repository may \
             run no checks.",
        );
    }
    ToolOutput::ok_with_data(
        content,
        json!({
            "branch": branch,
            "verdict": verdict,
            "checks": checks,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, outcome: CheckOutcome) -> Check {
        Check {
            name: name.into(),
            outcome,
        }
    }

    /// A failure outranks anything still running.
    ///
    /// This stops a wait that never pays off. An agent told "running" goes
    /// back to sleep, and the news it sleeps for has already come in.
    #[test]
    fn a_failure_is_the_verdict_even_while_other_checks_run() {
        let checks = [
            check("build", CheckOutcome::Running),
            check("test", CheckOutcome::Failed),
            check("lint", CheckOutcome::Passed),
        ];
        assert_eq!(verdict(&checks), "red");
    }

    /// Checks that all skipped are not a green branch.
    ///
    /// A workflow whose path filter matched nothing reports every job inert.
    /// Call that green and a branch nothing verified merges as verified.
    #[test]
    fn all_inert_is_not_green() {
        let checks = [
            check("build", CheckOutcome::Inert),
            check("test", CheckOutcome::Inert),
        ];
        assert_eq!(verdict(&checks), "nothing ran");
        assert_eq!(verdict(&[]), "no checks");
    }

    /// A pass beside a skip is green. Something ran, and nothing failed.
    #[test]
    fn a_pass_beside_a_skip_is_green() {
        let checks = [
            check("test", CheckOutcome::Passed),
            check("deploy", CheckOutcome::Inert),
        ];
        assert_eq!(verdict(&checks), "green");
    }

    /// The failed check is the first line of the list, not the last.
    #[test]
    fn failures_are_reported_first() {
        let checks = [
            check("a-pass", CheckOutcome::Passed),
            check("b-skip", CheckOutcome::Inert),
            check("c-fail", CheckOutcome::Failed),
        ];
        let ToolOutput::Ok { content, .. } = render("feat/x", &checks) else {
            panic!("a readable set of checks is not an error");
        };
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines[0], "feat/x: red");
        assert_eq!(lines[1], "  FAILED — c-fail");
    }
}
