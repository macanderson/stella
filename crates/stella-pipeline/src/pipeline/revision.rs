//! What a revision turn is told, and how much authority that text carries.
//!
//! A child module of `pipeline` so the orchestrator's god-file ceiling is not
//! grown by wording that has no reason to live inside the loop itself.

/// Why a revision turn is being spent — and, crucially, how much authority the
/// text carries.
///
/// Four call sites reach [`revision_prompt`] with four different kinds of
/// thing, and for a long time all four were announced identically as
/// `"Verification did not pass. Evidence: … Fix the issue"`. Two of them are
/// not evidence and one of them is not a failure, so that sentence was false
/// three times over — and its falsehood was not cosmetic:
///
/// A model verifier's prose is a **claim**. It is produced by a model reading a
/// bounded diff and a handful of counters, it is frequently right, and it is
/// sometimes a confabulation — especially when a channel it depends on was
/// silent. Labelling it `Evidence` and following it with `Fix the issue` tells
/// the worker that a measurement contradicts it, and the worker's correct
/// response to a measurement is to believe it over its own observations.
///
/// On Terminal-Bench `fix-git` that is exactly what happened. The worker
/// recovered a lost commit correctly, the reviewer claimed it had hand-written
/// the content instead, and the worker — told this was Evidence — reset
/// `master` and re-did the merge. Twice. Each round destroyed correct work to
/// satisfy a claim no measurement supported, and the trial ended on a deadline.
///
/// So the cause is typed. A deterministic failure still speaks with full
/// authority, because something really was measured. A reviewer's claim is
/// named as a claim and the worker is told to check it before acting — and told
/// explicitly that it may refuse. An evidence request stops pretending to be a
/// verdict at all.
pub(super) enum RevisionCause<'a> {
    /// A deterministic check failed: a test went red, or the turn changed
    /// nothing. Measured, and the worker should act on it.
    Deterministic(&'a str),
    /// An independent reviewer withheld a pass. Prose about the change, not a
    /// measurement of it.
    ReviewerClaim(&'a str),
    /// Not a failure. The ladder could not corroborate a pass and is asking for
    /// the one piece of evidence that would settle it (#1295).
    EvidenceRequest(&'a str),
}

/// The instruction appended to a revision turn.
pub(super) fn revision_prompt(cause: RevisionCause<'_>) -> String {
    match cause {
        RevisionCause::Deterministic(reason) => format!(
            "Verification did not pass. A deterministic check reported this — it is a \
             measurement, not an opinion:\n{}\n\nFix the issue and complete the task.",
            reason.trim()
        ),
        RevisionCause::ReviewerClaim(claim) => format!(
            "An independent reviewer did not pass this change. What follows is the \
             reviewer's CLAIM, not a measurement: it is a model's reading of a bounded \
             diff and a few counters, and it can be wrong — including about things you \
             observed directly and it did not.\n\nReviewer's claim:\n{}\n\nCheck the \
             claim against the workspace before you change anything. If it holds, fix it \
             and complete the task. If it does not, say so, state what you checked and \
             what you found, and leave your work in place — do not undo correct work to \
             satisfy a claim you have disproved.",
            claim.trim()
        ),
        RevisionCause::EvidenceRequest(ask) => format!(
            "Verification could not be completed: the result looks right but nothing \
             corroborates it. This is a request for evidence, not a report of a \
             defect — your change may well be correct as it stands.\n\n{}",
            ask.trim()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAIM: &str = "the agent hand-wrote the content instead of recovering it";

    /// The amplifier, at the grain that caused it: a model's prose must not
    /// reach the worker wearing the word the deterministic channel uses.
    #[test]
    fn a_reviewer_claim_is_never_called_evidence() {
        let prompt = revision_prompt(RevisionCause::ReviewerClaim(CLAIM));
        assert!(
            !prompt.contains("Evidence:"),
            "a model's reading must not be labelled Evidence: {prompt}"
        );
        assert!(prompt.contains("CLAIM"), "{prompt}");
        assert!(
            prompt.contains(CLAIM),
            "the claim itself still reaches the worker"
        );
    }

    /// …and the worker is told it may refuse. Without this the relabel is
    /// cosmetic: a worker that reads "reviewer's claim" and still believes it
    /// unconditionally destroys correct work exactly as before.
    #[test]
    fn a_reviewer_claim_tells_the_worker_to_check_before_changing_anything() {
        let prompt = revision_prompt(RevisionCause::ReviewerClaim(CLAIM));
        assert!(
            prompt.contains("before you change anything"),
            "validate first: {prompt}"
        );
        assert!(
            prompt.contains("do not undo correct work"),
            "refusing is permitted: {prompt}"
        );
    }

    /// A measurement keeps its authority. Relabelling everything as a claim
    /// would be the opposite error — a red test is not an opinion, and a worker
    /// invited to argue with one wastes the round.
    #[test]
    fn a_deterministic_failure_still_speaks_as_a_measurement() {
        let prompt = revision_prompt(RevisionCause::Deterministic("2 tests failed"));
        assert!(prompt.contains("measurement, not an opinion"), "{prompt}");
        assert!(prompt.contains("Fix the issue"), "{prompt}");
        assert!(!prompt.contains("CLAIM"), "{prompt}");
    }

    /// An evidence request is not a verdict. Announcing "Verification did not
    /// pass" over an ask told the worker its correct change was broken and sent
    /// it looking for a defect that was never claimed to exist.
    #[test]
    fn an_evidence_request_does_not_report_a_failure() {
        let prompt = revision_prompt(RevisionCause::EvidenceRequest("run `cargo test`"));
        assert!(
            !prompt.contains("did not pass"),
            "an ask is not a verdict: {prompt}"
        );
        assert!(prompt.contains("request for evidence"), "{prompt}");
        assert!(
            prompt.contains("may well be correct"),
            "the worker is told its change might be fine: {prompt}"
        );
    }

    /// Every arm carries its payload through intact — the relabel must not
    /// swallow the thing the worker actually needs to read.
    #[test]
    fn every_cause_forwards_its_text() {
        for cause in [
            RevisionCause::Deterministic("PAYLOAD"),
            RevisionCause::ReviewerClaim("PAYLOAD"),
            RevisionCause::EvidenceRequest("PAYLOAD"),
        ] {
            assert!(revision_prompt(cause).contains("PAYLOAD"));
        }
    }
}
