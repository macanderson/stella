//! The deterministic safety gate every extracted proposal passes through.
//!
//! Extraction from a document is a model call, and the content is untrusted even
//! when the source text is an explicit human instruction (`05` header: "the
//! content may be authoritative; the extraction is always inferred"). Three
//! rules turn that asymmetry into a mechanism, and all three are deterministic —
//! they belong here, in the pure core, not in the extraction prompt:
//!
//! 1. **Atomicity.** A record is atomic if it can receive exactly one refutation
//!    verdict. A compound claim cannot be refuted at all, so it is dismissed for
//!    re-extraction rather than published (`05` deploy example).
//! 2. **Executable-content quarantine.** A document that asks for a shell command
//!    must never put one in front of the agent — a dependency's README or a
//!    pasted gist could otherwise inject `rm -rf`. The raw text is preserved for
//!    a human to ratify; it is never a field the engine will run.
//! 3. **Probe-gating by origin.** A gated probe (`command_succeeds`, `http_ok`)
//!    is an exfiltration channel from an untrusted source, so it is never honored
//!    on an imported or inferred record.
//!
//! The model is asked to split atomically and to surface any executable content;
//! this gate is the backstop that does not depend on the model getting it right.

use super::super::context_record::kind::{Origin, RecordProposalStatus};
use super::record::{ProbeKind, Quarantine, Validation};

/// `imported` and `inferred` are the untrusted origins for gating: an imported
/// record was extracted from a document, an inferred one from a pattern, and
/// neither is a human decree. `user`/`system`/`observed` are not produced by
/// ingest, but the predicate is total so the rule has exactly one home.
pub fn origin_is_untrusted(origin: Origin) -> bool {
    matches!(origin, Origin::Imported | Origin::Inferred)
}

/// Whether a probe of `kind` may run against a record of `origin`.
///
/// A gated probe (runs a command or reaches the network) is honored only on a
/// trusted origin. On an imported/inferred record it is silently unavailable —
/// the caller strips it, so it can never execute. Ungated probes (`path_exists`,
/// `path_absent`, `file_contains`, `manual`) are always honored: they read the
/// tracked tree and execute nothing.
pub fn probe_honored(origin: Origin, kind: ProbeKind) -> bool {
    !(kind.is_gated() && origin_is_untrusted(origin))
}

/// A conservative structural test for a compound statement.
///
/// The model is the primary verifier of atomicity; this is the deterministic
/// backstop for the obvious case — an Oxford-comma enumeration of three or more
/// independent assertions ("Deploys run from main, happen on Tuesdays, and are
/// triggered with make ship"). It keys on `", and "` with at least two commas so
/// it does not fire on a two-clause sentence or on a conjunction inside a noun
/// phrase ("npm and yarn must not be used" has no comma and is one claim).
pub fn detect_compound(statement: &str) -> bool {
    let lower = statement.to_lowercase();
    lower.contains(", and ") && statement.matches(',').count() >= 2
}

/// The atomicity finding for a claim, if it is compound. `model_flagged_compound`
/// is the extractor's own verdict; either signal is enough.
pub fn atomicity_validation(statement: &str, model_flagged_compound: bool) -> Option<Validation> {
    if !(model_flagged_compound || detect_compound(statement)) {
        return None;
    }
    Some(Validation {
        rule: "one_verdict_per_record".to_string(),
        verdict: "compound".to_string(),
        detail: "the statement carries more than one independently falsifiable assertion, so a \
                 single refutation verdict cannot describe it"
            .to_string(),
        action: "re-extract as separate atomic records".to_string(),
    })
}

/// The quarantine record for executable content from an untrusted origin, if it
/// applies. `field` names where the content would have landed
/// (`enforcement.autofix`); `raw` is the verbatim command, preserved but never
/// honored.
pub fn quarantine_for(origin: Origin, field: &str, raw: &str) -> Option<Quarantine> {
    if !origin_is_untrusted(origin) {
        return None;
    }
    Some(Quarantine {
        reason: "executable_content_from_imported_origin".to_string(),
        field: field.to_string(),
        raw: raw.to_string(),
        requires: "human ratification; basis must be decree and verified_by set".to_string(),
        never_honored_when: vec![
            "origin = imported".to_string(),
            "origin = inferred".to_string(),
        ],
    })
}

/// The outcome of running one candidate through the gate: the proposal status it
/// earns and any finding attached to it.
#[derive(Debug, Clone, PartialEq)]
pub struct GateOutcome {
    /// `Eligible` when the claim is atomic and carries no executable content;
    /// `Dismissed` otherwise.
    pub status: RecordProposalStatus,
    /// The dismissal reason (`compound_claim`, `quarantined_executable`), when
    /// dismissed.
    pub dismissed_reason: Option<String>,
    /// The atomicity finding, when the claim was compound.
    pub validation: Option<Validation>,
    /// The quarantine record, when executable content was refused.
    pub quarantine: Option<Quarantine>,
}

impl GateOutcome {
    /// True when the proposal cleared the gate.
    pub fn is_eligible(&self) -> bool {
        self.status == RecordProposalStatus::Eligible
    }
}

/// Run one candidate claim through the gate.
///
/// Compound-ness is checked first: a compound claim will be re-extracted as
/// several records, so quarantining its executable content now would be wasted —
/// the split records each get gated on their own. Otherwise executable content
/// dismisses the proposal (the record is preserved for ratification but is not
/// eligible). A claim that is atomic and script-free is eligible.
pub fn gate_proposal(
    origin: Origin,
    statement: &str,
    model_flagged_compound: bool,
    executable: Option<(&str, &str)>,
) -> GateOutcome {
    if let Some(validation) = atomicity_validation(statement, model_flagged_compound) {
        return GateOutcome {
            status: RecordProposalStatus::Dismissed,
            dismissed_reason: Some("compound_claim".to_string()),
            validation: Some(validation),
            quarantine: None,
        };
    }
    if let Some((field, raw)) = executable
        && let Some(quarantine) = quarantine_for(origin, field, raw)
    {
        return GateOutcome {
            status: RecordProposalStatus::Dismissed,
            dismissed_reason: Some("quarantined_executable".to_string()),
            validation: None,
            quarantine: Some(quarantine),
        };
    }
    GateOutcome {
        status: RecordProposalStatus::Eligible,
        dismissed_reason: None,
        validation: None,
        quarantine: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gated_probes_are_never_honored_on_untrusted_origins() {
        for origin in [Origin::Imported, Origin::Inferred] {
            assert!(!probe_honored(origin, ProbeKind::CommandSucceeds));
            assert!(!probe_honored(origin, ProbeKind::HttpOk));
            // Ungated probes stay available — they read the tree, run nothing.
            assert!(probe_honored(origin, ProbeKind::PathExists));
            assert!(probe_honored(origin, ProbeKind::FileContains));
            assert!(probe_honored(origin, ProbeKind::Manual));
        }
        // A decree could honor a gated probe; ingest never produces one.
        assert!(probe_honored(Origin::User, ProbeKind::CommandSucceeds));
    }

    #[test]
    fn compound_heuristic_flags_enumerations_not_two_clause_sentences() {
        assert!(detect_compound(
            "Deploys run from main, happen on Tuesdays, and are triggered with make ship."
        ));
        // One constraint with a noun-phrase conjunction — not compound.
        assert!(!detect_compound(
            "This repository uses pnpm exclusively; npm and yarn must not be used."
        ));
        // A single clause.
        assert!(!detect_compound("All development happens on Node 20.x."));
    }

    #[test]
    fn compound_claim_is_dismissed_for_re_extraction() {
        let outcome = gate_proposal(
            Origin::Imported,
            "Deploys run from main, happen on Tuesdays, and are triggered with make ship.",
            false,
            None,
        );
        assert_eq!(outcome.status, RecordProposalStatus::Dismissed);
        assert_eq!(outcome.dismissed_reason.as_deref(), Some("compound_claim"));
        assert!(outcome.validation.is_some());
        assert!(outcome.quarantine.is_none());
    }

    #[test]
    fn model_flag_alone_dismisses_a_claim_the_heuristic_would_pass() {
        let outcome = gate_proposal(Origin::Imported, "A short claim.", true, None);
        assert_eq!(outcome.dismissed_reason.as_deref(), Some("compound_claim"));
    }

    #[test]
    fn executable_content_from_an_import_is_quarantined() {
        let outcome = gate_proposal(
            Origin::Imported,
            "A broken local environment is reset by clearing node_modules and reinstalling.",
            false,
            Some((
                "enforcement.autofix",
                "rm -rf node_modules && pnpm install --force",
            )),
        );
        assert_eq!(outcome.status, RecordProposalStatus::Dismissed);
        assert_eq!(
            outcome.dismissed_reason.as_deref(),
            Some("quarantined_executable")
        );
        let q = outcome.quarantine.expect("quarantine present");
        assert_eq!(q.raw, "rm -rf node_modules && pnpm install --force");
        assert!(q.never_honored_when.iter().any(|w| w.contains("imported")));
    }

    #[test]
    fn an_atomic_scriptless_claim_is_eligible() {
        let outcome = gate_proposal(
            Origin::Imported,
            "The staging API is served from https://api.staging.acme.dev.",
            false,
            None,
        );
        assert!(outcome.is_eligible());
        assert!(outcome.dismissed_reason.is_none());
    }
}
