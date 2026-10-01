// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The labels the loop writes, put on a tracker that lacks them.
//!
//! Triage answers in the set ladder and kinds, then writes those labels. A
//! tracker that lacks a label turns the write down. The issue then comes
//! back on each cycle. A new repository has GitHub's stock labels and no
//! more, so this is the common case. It lets the loop run on a repository
//! nobody set up for it.
//!
//! Each label gets the color of its family and the text the label list
//! gives it (ADR 0046). A label the tracker has, in any case, is left as it
//! is.

use stella_autonomy::labels;
use stella_autonomy::priority::TriagePolicy;

use crate::issue_provider::ensure_label;
use crate::self_driving_cmd::triage::{BLOCKED_LABEL, READY_LABEL, SIZE_SCALE};

/// The color of a priority label.
pub(super) const PRIORITY: &str = "FFFFFF";
/// The color of a kind label.
pub(super) const KIND: &str = "000000";
/// The color of a size label.
pub(super) const SIZE: &str = "A1A1AA";
/// The color of a status label.
pub(super) const STATUS: &str = "5B93D6";
/// The color of an alarm label, such as the escalation mark.
pub(super) const ALARM: &str = "D5584D";
/// The color of a label automation applies to its own pull requests.
pub(super) const AUTOMATION: &str = "71717A";

/// The description of a defect kind an operator set.
const OPERATOR_KIND: &str = "Work the self-driving loop may take.";

/// Put each label triage may write on the tracker, if it can.
///
/// A failure here does not stop the run. The write that needs the label
/// fails on its own, later, and says why.
pub(super) fn install(policy: &TriagePolicy) {
    for (index, rung) in policy.ladder.rungs.iter().enumerate() {
        let description = format!("Priority {index}, installed by the self-driving loop.");
        let _ = ensure_label(rung, PRIORITY, &description);
    }
    for kind in &policy.defect_kinds {
        let (color, description) = kind_label(kind);
        let _ = ensure_label(kind, color, description);
    }
    for (_, label, band) in SIZE_SCALE {
        let _ = ensure_label(label, SIZE, band);
    }
    let _ = ensure_label(
        READY_LABEL,
        STATUS,
        "Ready for Stella's self-driving loop to pick up",
    );
    let _ = ensure_label(
        BLOCKED_LABEL,
        STATUS,
        "Waiting on an issue named in the body as `Blocked by: #N`",
    );
}

/// The color and description a defect kind is created with.
///
/// The two defaults carry the manifest's text. A kind an operator set gets
/// the kind color and a plain line.
fn kind_label(kind: &str) -> (&'static str, &'static str) {
    if labels::same(kind, "TRIAGE") {
        (
            STATUS,
            "Untriaged: the only label a creator applies. Triage replaces it",
        )
    } else if labels::same(kind, "KIND:BUG") {
        (KIND, "Something that exists behaves wrongly")
    } else {
        (KIND, OPERATOR_KIND)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every default kind gets the manifest's text, in its own family's color.
    #[test]
    fn the_default_kinds_carry_the_manifest_text() {
        for kind in &TriagePolicy::default().defect_kinds {
            let (_, description) = kind_label(kind);
            assert_ne!(description, OPERATOR_KIND, "{kind}");
        }
        assert_eq!(kind_label("triage").0, STATUS, "a status");
        assert_eq!(kind_label("KIND:BUG").0, KIND);
    }

    /// GitHub turns down a label whose text is longer than 100 characters.
    #[test]
    fn every_description_fits_the_trackers_limit() {
        let kinds = ["TRIAGE", "KIND:BUG", "other"].map(|kind| kind_label(kind).1);
        let bands = SIZE_SCALE.map(|(_, _, band)| band);
        for description in kinds.iter().chain(&bands) {
            assert!(description.len() <= 100, "{description}");
        }
    }
}
