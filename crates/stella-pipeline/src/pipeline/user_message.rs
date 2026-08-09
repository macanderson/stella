//! The worker's opening user message: the volatile half of the pipeline's
//! prompt to the model that does the work.
//!
//! Split out of `pipeline.rs`, which is closed to growth
//! (`scripts/check-file-size.sh`), on the same grounds as `plan_steps` — this
//! is pure text assembly over owned data, so it reads and tests better beside
//! its own doc than buried in `run`.
//!
//! Everything here rides **after** the byte-stable system prefix (L-E8, the
//! prompt-cache stability invariant): recalled frames, research findings, the
//! goal and the verification contract all change per turn, and putting any of
//! them in the prefix would cost the session its cache hits.
//!
//! The one property every caller depends on: with no frames, no findings and
//! no contract, the output is the bare goal string. The advisory stages
//! upstream (recall, research) are allowed to produce nothing, and "produced
//! nothing" has to be indistinguishable from "was never wired in".

use crate::ports::RecalledFrame;
use crate::research::ResearchFinding;

/// What the worker's user message says about how this run will be verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VerificationContract<'a> {
    /// An operator-configured oracle: disclose the command.
    Oracle(&'a str),
    /// No oracle and no independent witness author: the worker's own failing
    /// test, written first, is the only deterministic evidence the run will
    /// carry — say so on the channel the worker plans from.
    WorkerTestFirst,
    /// Nothing to add: a conversational turn, a class that never verifies, or
    /// an authored witness that will supply the oracle post-execution (its
    /// disclosure stays governed by the airlock).
    None,
}

/// The worker's first user message: the recalled frames and research findings
/// that ground the turn, the goal, and how this run will be verified.
///
/// `research` is the second sink for the pre-plan stage's findings (#2415).
/// Before it, [`crate::plan::build_planner_prompt`] was the only one — so a
/// fact a read-only sub-agent verified against this workspace reached the
/// worker only as whatever residue of it the planner chose to encode into a
/// step string, compressed through a lossy intermediary that was never asked
/// to preserve it. On a class that does not plan there was no intermediary at
/// all.
///
/// The advisory contract holds in the strongest form: **no findings must leave
/// this output byte-for-byte what it was before this parameter existed**, which
/// is what the early return and the `is_empty` guard below are for.
pub(super) fn assemble_user_message(
    goal: &str,
    frames: &[RecalledFrame],
    research: &[ResearchFinding],
    contract: VerificationContract<'_>,
) -> String {
    if frames.is_empty() && research.is_empty() && contract == VerificationContract::None {
        return goal.to_string();
    }
    let mut s = String::new();
    // Research before recall, and its own section — the same ordering and the
    // same reason as the planner prompt (`build_planner_prompt`): recall is
    // what the context plane remembered, research is what a sub-agent verified
    // against this workspace just now, and the worker cannot weigh the two
    // provenances differently if they arrive in one list.
    if !research.is_empty() {
        s.push_str("## Research findings\n");
        for finding in research {
            s.push_str("### ");
            s.push_str(finding.question.trim());
            s.push('\n');
            s.push_str(finding.answer.trim());
            s.push_str("\n\n");
        }
    }
    if !frames.is_empty() {
        s.push_str("## Recalled context\n");
        for f in frames {
            // Cite by human label (L-C4); include content as grounding.
            s.push_str("- [");
            s.push_str(&f.citation_label);
            s.push_str("] (");
            s.push_str(&f.source);
            s.push_str(")\n");
            if !f.content.trim().is_empty() {
                s.push_str("  ");
                s.push_str(f.content.trim());
                s.push('\n');
            }
        }
        s.push('\n');
    }
    s.push_str("## Task\n");
    s.push_str(goal.trim());
    // The verification contract, when the operator configured one. The
    // methodology prompt tells the worker to "run the target test" without
    // ever saying which — the command that actually gates the run was
    // withheld until the first failure disclosed it (the airlock's L1 brief
    // names it anyway). Saying it up front moves that information one failed
    // revision earlier, on the exact channel the worker plans from. Only the
    // operator-CONFIGURED command is ever disclosed here: an authored
    // witness's command does not exist yet at assembly time, and its
    // disclosure stays governed by the airlock (`crate::witness::airlock`).
    match contract {
        VerificationContract::Oracle(command) => {
            s.push_str("\n\n## Verification\n");
            s.push_str(&format!(
                "This run's primary verification is `{command}`: the accepted deterministic \
                 evidence is this command failing before your change and passing after it. \
                 Reproduce the failure with it before editing; make it pass before finishing. \
                 Do not modify the tests it runs."
            ));
        }
        VerificationContract::WorkerTestFirst => {
            s.push_str("\n\n## Verification\n");
            s.push_str(
                "No test command is configured for this run and no independent test author \
                 is available: nothing outside your own work will check this change. Before \
                 implementing, write the failing test that captures this task and run it to \
                 watch it fail; make it pass before finishing. That test is the only \
                 deterministic evidence this run will carry.",
            );
        }
        VerificationContract::None => {}
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(label: &str, content: &str) -> RecalledFrame {
        RecalledFrame {
            citation_label: label.into(),
            provider: "code-graph".into(),
            source: "code-graph".into(),
            kind: "symbol".into(),
            uri: None,
            method: None,
            content: content.into(),
            token_cost: 5,
            id: None,
            content_digest: None,
        }
    }

    fn finding(question: &str, answer: &str) -> ResearchFinding {
        ResearchFinding {
            question: question.into(),
            answer: answer.into(),
        }
    }

    #[test]
    fn assemble_user_message_puts_recall_before_the_task() {
        let frames = vec![RecalledFrame {
            citation_label: "driver.rs".into(),
            provider: "code-graph".into(),
            source: "code-graph".into(),
            kind: "symbol".into(),
            uri: None,
            method: None,
            content: "run_turn".into(),
            token_cost: 5,
            id: None,
            content_digest: None,
        }];
        let msg = assemble_user_message("do the thing", &frames, &[], VerificationContract::None);
        let recall_idx = msg.find("Recalled context").unwrap();
        let task_idx = msg.find("do the thing").unwrap();
        assert!(recall_idx < task_idx, "recall rides before the goal");
    }

    #[test]
    fn assemble_user_message_is_just_the_goal_when_no_recall() {
        assert_eq!(
            assemble_user_message("hello", &[], &[], VerificationContract::None),
            "hello"
        );
    }

    /// The configured test command is the run's actual oracle, so the worker is
    /// told it up front instead of discovering it from the first failure's
    /// disclosure. Only the operator-configured command ever rides here — an
    /// authored witness's command is airlocked, and does not exist at assembly
    /// time anyway.
    #[test]
    fn assemble_user_message_states_the_configured_verification_contract() {
        let msg = assemble_user_message(
            "fix the parser",
            &[],
            &[],
            VerificationContract::Oracle("cargo test -p parser"),
        );
        let task_idx = msg.find("fix the parser").unwrap();
        let contract_idx = msg.find("`cargo test -p parser`").unwrap();
        assert!(
            task_idx < contract_idx,
            "the task leads; the contract qualifies it"
        );
        assert!(msg.contains("failing before your change and passing after it"));
        assert!(msg.contains("Do not modify the tests it runs"));
    }

    /// With no oracle and no independent author, the worker is told up front that
    /// its own failing test — written before the fix — is the run's only
    /// deterministic evidence. This is the same-model degraded posture: the run
    /// proceeds, but test-first is now the worker's job and the message says so.
    #[test]
    fn assemble_user_message_demands_test_first_when_nothing_else_verifies() {
        let msg = assemble_user_message(
            "add retries",
            &[],
            &[],
            VerificationContract::WorkerTestFirst,
        );
        let task_idx = msg.find("add retries").unwrap();
        let contract_idx = msg.find("write the failing test").unwrap();
        assert!(
            task_idx < contract_idx,
            "the task leads; the contract qualifies it"
        );
        assert!(msg.contains("Before implementing"));
        assert!(msg.contains("only deterministic evidence"));
    }

    /// #2415: findings are their own section, ahead of both recall and the
    /// goal — grounding leads, the ask follows.
    #[test]
    fn research_findings_lead_the_message_and_stay_apart_from_recall() {
        let msg = assemble_user_message(
            "add retries",
            &[frame("driver.rs", "run_turn")],
            &[finding("who owns retries?", "stella-core::retry")],
            VerificationContract::None,
        );
        let findings_at = msg.find("## Research findings").unwrap();
        let recall_at = msg.find("## Recalled context").unwrap();
        let task_at = msg.find("## Task").unwrap();
        assert!(findings_at < recall_at && recall_at < task_at, "{msg}");
        assert!(msg.contains("who owns retries?"));
        assert!(msg.contains("stella-core::retry"));
    }

    /// The advisory contract at its narrowest: adding an empty findings list
    /// to a message that has recall changes nothing about it.
    #[test]
    fn an_empty_findings_list_adds_no_heading() {
        let frames = [frame("driver.rs", "run_turn")];
        assert_eq!(
            assemble_user_message("add retries", &frames, &[], VerificationContract::None),
            assemble_user_message("add retries", &frames, &[], VerificationContract::None),
        );
        assert!(
            !assemble_user_message("add retries", &frames, &[], VerificationContract::None)
                .contains("Research findings")
        );
    }
}
