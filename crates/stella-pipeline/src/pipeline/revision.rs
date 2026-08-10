//! What a revision turn is told, and how much authority that text carries.
//!
//! A child module of `pipeline` so the orchestrator's god-file ceiling is not
//! grown by wording that has no reason to live inside the loop itself.

/// Why a revision turn is being spent — and, crucially, how much authority the
/// text carries.
///
/// Both remaining causes are things the pipeline **measured or did**, and that
/// is now a property of the type rather than a convention: there is no variant
/// for a model's reading of the change, because no model reads the change.
///
/// The variant that is gone is the reason this enum exists at all. A model
/// verifier's prose is a **claim** — produced by a model reading a bounded diff
/// and a handful of counters, frequently right, and sometimes a confabulation,
/// especially when a channel it depended on was silent. Announcing it as
/// `"Verification did not pass. Evidence: …"` told the worker that a
/// measurement contradicted it, and a worker's correct response to a
/// measurement is to believe it over its own observations.
///
/// On Terminal-Bench `fix-git` that is exactly what happened. The worker
/// recovered a lost commit correctly, the reviewer claimed it had hand-written
/// the content instead, and the worker — told this was Evidence — reset
/// `master` and re-did the merge. Twice. Each round destroyed correct work to
/// satisfy a claim no measurement supported, and the trial ended on a deadline.
///
/// Typing the cause was the first repair: the claim was relabelled a claim and
/// the worker was told it could refuse. Removing the call is the second and
/// last one. A channel whose best case is "the worker correctly disbelieves it"
/// is not worth its worst case.
pub(super) enum RevisionCause<'a> {
    /// A deterministic check failed: a test went red, or the turn changed
    /// nothing. Measured, and the worker should act on it.
    Deterministic(&'a str),
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
            RevisionCause::EvidenceRequest("PAYLOAD"),
        ] {
            assert!(revision_prompt(cause).contains("PAYLOAD"));
        }
    }

    /// The witness for the removal, and the only shape one can take for a
    /// *deleted* capability: enumerate the whole vocabulary and assert that
    /// nothing in it carries a model's reading of the change.
    ///
    /// The match is exhaustive and unmatched-arm-checked, so a variant added
    /// later fails to compile here until someone states which kind of thing it
    /// is. That is what makes this a standing guarantee rather than a snapshot
    /// of today: re-introducing a reviewer channel cannot be done quietly, it
    /// has to be done by editing this test and saying so.
    #[test]
    fn no_revision_cause_carries_a_model_opinion() {
        for cause in [
            RevisionCause::Deterministic("PAYLOAD"),
            RevisionCause::EvidenceRequest("PAYLOAD"),
        ] {
            let measured_or_asked = match cause {
                // Something really ran and really failed.
                RevisionCause::Deterministic(_) => true,
                // A fixed template over the tracked command; asserts nothing
                // about the change.
                RevisionCause::EvidenceRequest(_) => true,
            };
            assert!(measured_or_asked);
            let prompt = revision_prompt(cause);
            assert!(
                !prompt.contains("reviewer"),
                "no revision turn may cite a reviewer: {prompt}"
            );
        }
    }
}
