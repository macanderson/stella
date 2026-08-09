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
