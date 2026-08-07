//! Pre-plan research (#1778): the pure half of the stage that answers
//! triage's questions before the planner runs.
//!
//! Every pre-execute stage is a single tool-less completion over text the
//! pipeline assembled, so the decisions with the highest downstream leverage
//! — what the plan names, what the witness pins — are taken at the point of
//! least evidence. This stage closes that gap for multi-step work: triage
//! names up to [`crate::triage::MAX_RESEARCH_QUESTIONS`] self-contained
//! questions ([`crate::triage::parse_research_questions`]), the pipeline fans
//! them out as parallel **read-only** sub-agents
//! (`Pipeline::research_stage`), and the bounded findings land in the
//! planner's split context as their own section — never mixed into recall
//! frames, whose provenance (the context plane) is different from "a model
//! read this workspace just now".
//!
//! This module owns what is pure and synchronous: the finding type, the
//! prompt fragments, and the budget clamp. The I/O sequencing — sub-agent
//! dispatch, the per-child latency ceiling, budget carves — lives in
//! `pipeline/research_stage.rs`, following the same decision/sequencing split
//! as [`crate::candidate_fanout`] and `pipeline/fanout_stage.rs`.

/// One answered research question, as it will be shown to the planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResearchFinding {
    /// Triage's question, verbatim.
    pub question: String,
    /// The sub-agent's answer. Already clamped at the source by
    /// [`SubAgentSpec::max_report_chars`](stella_core::SubAgentSpec) and again
    /// by [`bound_research_findings`] — defense in depth, since the planner
    /// prompt is paid for on every scope revision.
    pub answer: String,
}

/// Per-finding content ceiling, in chars — the same order as one recalled
/// frame (`RECALL_FRAME_CHARS` in `crate::pipeline`): an answer's job is to
/// ground the planner, not to relocate the workspace into the prompt.
pub const RESEARCH_FINDING_CHARS: usize = 4_000;

/// Total research-content budget per turn, in chars. Research is grounding
/// for one planner call; past this it displaces the plan itself.
pub const RESEARCH_PROMPT_BUDGET_CHARS: usize = 12_000;

/// The marker appended when the clamp cut or dropped anything — a bounded
/// findings section must never read as the whole story.
pub const RESEARCH_DROP_MARKER: &str = "[… research findings truncated at the prompt budget …]";

/// Clamp findings to the planner-prompt budget, mirroring the recall clamp
/// (`bound_recalled_frames`): each answer is truncated to
/// [`RESEARCH_FINDING_CHARS`] with an in-content marker, and findings past
/// [`RESEARCH_PROMPT_BUDGET_CHARS`] of cumulative content are dropped with a
/// final marker finding. Both interventions stay visible.
pub fn bound_research_findings(findings: Vec<ResearchFinding>) -> Vec<ResearchFinding> {
    let mut out: Vec<ResearchFinding> = Vec::with_capacity(findings.len());
    let mut spent = 0usize;
    let total = findings.len();
    for mut finding in findings {
        if spent >= RESEARCH_PROMPT_BUDGET_CHARS {
            let dropped = total - out.len();
            out.push(ResearchFinding {
                question: "research-budget".into(),
                answer: format!("[{dropped} research finding(s) dropped] {RESEARCH_DROP_MARKER}"),
            });
            break;
        }
        if finding.answer.chars().count() > RESEARCH_FINDING_CHARS {
            let kept: String = finding.answer.chars().take(RESEARCH_FINDING_CHARS).collect();
            finding.answer = format!("{kept}\n{RESEARCH_DROP_MARKER}");
        }
        // Chars, matching the budget's unit, for the same reason the recall
        // clamp counts chars: bytes over-charge multi-byte content.
        spent += finding.answer.chars().count();
        out.push(finding);
    }
    out
}

/// The research sub-agent's system prompt. `&'static str` on purpose — the
/// same byte-stability discipline as every management instruction block
/// (#1434): identical bytes on every research call, so the provider adapters
/// have a stable prefix to cache-mark.
pub const RESEARCH_SYSTEM_PROMPT: &str = "You are a read-only research agent inside a coding \
     agent's planning phase. Answer ONE question about this workspace by \
     reading files and searching — you cannot modify anything. Be concrete: \
     name the files (paths, not descriptions), the symbols, and the facts you \
     verified, and say plainly when you could not find an answer. Your reply \
     goes to a planner, so keep it short and load-bearing: what it must know, \
     nothing it could have guessed.";

/// The child's instruction: the question, grounded by the goal it serves so
/// the sub-agent can judge relevance. Volatile payload — the stable half is
/// [`RESEARCH_SYSTEM_PROMPT`].
pub fn research_instruction(goal: &str, question: &str) -> String {
    format!(
        "The goal this research serves:\n{goal}\n\nAnswer this question about \
         the workspace:\n{question}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(question: &str, answer: &str) -> ResearchFinding {
        ResearchFinding {
            question: question.into(),
            answer: answer.into(),
        }
    }

    #[test]
    fn small_findings_pass_through_untouched() {
        let input = vec![finding("q1", "a1"), finding("q2", "a2")];
        assert_eq!(bound_research_findings(input.clone()), input);
    }

    #[test]
    fn an_oversized_answer_is_truncated_with_a_visible_marker() {
        let long = "x".repeat(RESEARCH_FINDING_CHARS + 100);
        let out = bound_research_findings(vec![finding("q", &long)]);
        assert_eq!(out.len(), 1);
        assert!(out[0].answer.contains(RESEARCH_DROP_MARKER));
        assert!(out[0].answer.chars().count() <= RESEARCH_FINDING_CHARS + RESEARCH_DROP_MARKER.len() + 1);
    }

    #[test]
    fn findings_past_the_total_budget_are_dropped_with_a_marker_finding() {
        // Four findings at the per-finding ceiling blow the 12k total budget.
        let big = "y".repeat(RESEARCH_FINDING_CHARS);
        let input = (0..4).map(|i| finding(&format!("q{i}"), &big)).collect();
        let out = bound_research_findings(input);
        let last = out.last().expect("marker finding");
        assert!(
            last.answer.contains("dropped"),
            "the dropped tail must be summarized, not silent: {last:?}"
        );
        assert!(
            out.len() < 5,
            "dropping must actually drop: {} findings survived",
            out.len()
        );
    }

    #[test]
    fn multibyte_content_is_counted_in_chars_not_bytes() {
        // A multi-byte answer exactly at the char ceiling must survive whole.
        let exact: String = "é".repeat(RESEARCH_FINDING_CHARS);
        let out = bound_research_findings(vec![finding("q", &exact)]);
        assert!(!out[0].answer.contains(RESEARCH_DROP_MARKER));
    }

    #[test]
    fn the_instruction_carries_goal_and_question() {
        let text = research_instruction("fix the retry bug", "which module owns retries?");
        assert!(text.contains("fix the retry bug"));
        assert!(text.contains("which module owns retries?"));
    }
}
