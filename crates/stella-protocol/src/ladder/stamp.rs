// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Who claimed the work was done, beside what was seen.
//!
//! `LadderSnapshot` holds the evidence. A stamp holds the observer. So a
//! second observer can agree, disagree, or hold back on the same record. It
//! never writes over the first claim.

use serde::{Deserialize, Serialize};

/// What one observer concluded about the work.
///
/// Four values, not a bool. `NotDone` is a claim about the work.
/// `Inconclusive` is a claim about the tool: it looked, and it could not tell.
/// `LadderRung` splits `Unverified` from `Unverifiable` for that same reason.
/// `NotApplicable` says there was nothing here to judge.
///
/// Closed, like every nested vocabulary here. A token from a newer build
/// fails the event. It is never read as a weaker one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StampAssessment {
    /// This observer holds the work done.
    Done,
    /// This observer holds the work not done.
    NotDone,
    /// This observer looked and could not tell. Never a claim that the work
    /// fell short.
    Inconclusive,
    /// There was nothing here for this observer to judge.
    NotApplicable,
}

impl StampAssessment {
    /// The wire token — the same string serde writes. A rendered line and a
    /// JSON stream then name the value the same way.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            StampAssessment::Done => "done",
            StampAssessment::NotDone => "not_done",
            StampAssessment::Inconclusive => "inconclusive",
            StampAssessment::NotApplicable => "not_applicable",
        }
    }
}

/// One observer's claim about the evidence a verdict was decided from.
///
/// A stamp is a record, never a vote. Nothing reads one to decide anything.
/// Three `Done` stamps on an unverified snapshot leave it unverified. Counting
/// agreement is how an ablated verifier scored every run a pass, with nothing
/// to notice. Arbitration is a separate call, filed as its own issue.
///
/// A stamp carries no signature. Integrity and identity are different threat
/// models, and this type answers the second one. The host fills
/// [`VerdictStamp::author`] from the manifest it loaded, so no plugin can
/// speak in another's name. [`VerdictStamp::preimage_hash`] ties the claim to
/// the evidence, not to a key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct VerdictStamp {
    /// Who made this claim. The host fills it from the manifest it loaded, so
    /// no plugin can name itself something else. `"engine"` is the host's own
    /// call.
    pub author: String,
    /// The author's own version string, copied word for word. A reader may
    /// show it. Nothing branches on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_version: Option<String>,
    /// What this observer concluded.
    pub assessment: StampAssessment,
    /// One line saying what was checked and what it showed. Prose for a
    /// human — never parsed.
    pub summary: String,
    /// `sha256:<64 hex>` over the RFC 8785 canonical bytes of the snapshot
    /// this claim was made against, with `stamps` dropped from the preimage.
    /// `LadderSnapshot::stamp_preimage` builds that object, and the
    /// record-hash primitive (ADR 0004) digests it. So one hashing rule
    /// covers the tree.
    ///
    /// Dropping `stamps` lets a later observer stamp the same record without
    /// breaking the claims already on it. It also lets a replay prove the
    /// claim was made against the evidence the run produced, and not against
    /// a later edit of it.
    pub preimage_hash: String,
    /// Pointers to the artifacts behind the summary, in the vocabulary
    /// `VerdictEvidence::evidence_refs` uses. A reader can go and check the
    /// claim rather than take it on faith.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    /// When the claim was made, in milliseconds since the Unix epoch.
    pub decided_at_ms: u64,
    /// How long this observer took, in milliseconds.
    pub duration_ms: u64,
    /// The observer ran out of time. Its assessment is then what it had when
    /// the clock stopped. A timed-out `Inconclusive` and a considered one are
    /// different facts.
    #[serde(default)]
    pub timed_out: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp() -> VerdictStamp {
        VerdictStamp {
            author: "vera".into(),
            author_version: Some("2.1.0".into()),
            assessment: StampAssessment::Done,
            summary: "the authored witness went red then green".into(),
            preimage_hash: format!("sha256:{}", "a1".repeat(32)),
            evidence_refs: vec!["trace:t1#verify".into()],
            decided_at_ms: 1_767_225_600_000,
            duration_ms: 4_210,
            timed_out: false,
        }
    }

    /// AGENTS.md #4: the stamp round-trips byte for byte, on every
    /// assessment, with its optional fields present and absent.
    #[test]
    fn the_stamp_round_trips() {
        for assessment in [
            StampAssessment::Done,
            StampAssessment::NotDone,
            StampAssessment::Inconclusive,
            StampAssessment::NotApplicable,
        ] {
            for value in [
                VerdictStamp {
                    assessment,
                    ..stamp()
                },
                VerdictStamp {
                    assessment,
                    author_version: None,
                    evidence_refs: vec![],
                    timed_out: true,
                    ..stamp()
                },
            ] {
                let json = serde_json::to_string(&value).unwrap();
                let back: VerdictStamp = serde_json::from_str(&json).unwrap();
                assert_eq!(value, back);
            }
        }
    }

    /// An unset version and an empty list of refs emit no key. A stamp
    /// written without them reads as the quiet value on each: no version, no
    /// refs, and a claim that was not cut short.
    #[test]
    fn the_optional_fields_are_absent_rather_than_null() {
        let bare = VerdictStamp {
            author_version: None,
            evidence_refs: vec![],
            ..stamp()
        };
        let json = serde_json::to_string(&bare).unwrap();
        for key in ["author_version", "evidence_refs"] {
            assert!(!json.contains(key), "an unset {key} emits no key: {json}");
        }

        let minimal = r#"{"author":"engine","assessment":"inconclusive",
            "summary":"no probe could look","preimage_hash":"sha256:00",
            "decided_at_ms":1,"duration_ms":0}"#;
        let parsed: VerdictStamp = serde_json::from_str(minimal).unwrap();
        assert_eq!(parsed.author_version, None);
        assert!(parsed.evidence_refs.is_empty());
        assert!(!parsed.timed_out);
    }

    /// `as_str` and serde spell every assessment the same way. Two spellings
    /// of one value is how a rendered line stops matching the stream.
    #[test]
    fn the_tokens_match_what_serde_writes() {
        for assessment in [
            StampAssessment::Done,
            StampAssessment::NotDone,
            StampAssessment::Inconclusive,
            StampAssessment::NotApplicable,
        ] {
            assert_eq!(
                serde_json::to_string(&assessment).unwrap(),
                format!("\"{}\"", assessment.as_str())
            );
        }
    }

    /// `inconclusive` is not `not_done` on the wire. A reader that mixed them
    /// up would report that the observer found the work wrong. The observer
    /// said it could not tell.
    #[test]
    fn an_abstention_does_not_serialise_as_a_finding() {
        let abstained = serde_json::to_string(&StampAssessment::Inconclusive).unwrap();
        let found = serde_json::to_string(&StampAssessment::NotDone).unwrap();
        assert_ne!(abstained, found);
    }
}
