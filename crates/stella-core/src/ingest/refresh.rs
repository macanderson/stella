//! The re-ingest diff — deciding what happens to already-published records
//! when their source file is re-read (#2708, the second slice of #2683).
//!
//! Records are bitemporal and never edited in place, so "the file changed"
//! is not an overwrite: it is a *plan* over the lineage's published records,
//! each of which lands in exactly one of three buckets:
//!
//! - **unchanged** — the new text still asserts the same claim. The published
//!   record is kept untouched, preserving its age, appraisal history and
//!   recall stats. This is the churn guard: a one-line edit to a large file
//!   retires one record, not hundreds.
//! - **changed** — the new text asserts a different claim at the same stable
//!   identity. Nothing is retired yet: the replacement is a *proposal* until a
//!   reviewer keeps it, and retiring the old record before the new one is
//!   published would open a steering gap. Supersession happens at keep time,
//!   with a `supersedes_record_id` link so selection can never carry both.
//! - **retired** — the new text no longer asserts the claim at all. The
//!   record's valid time closes now: re-running ingest on the file was the
//!   user's deliberate act, and the source it cites no longer says it.
//!
//! # Identity is content first, anchor second
//!
//! The issue's stable identity is `(section_anchor, normalized_content_hash)`.
//! In this pipeline the anchor is the extractor-assigned lineage suffix, and
//! the extractor is a model — the same sentence can come back under a
//! different slug on a different day. Matching by statement *first* makes the
//! churn guard robust to that: a claim whose text survived is unchanged even
//! if its slug moved, and only a claim whose text is gone falls through to the
//! anchor comparison. The alternative order would retire-and-re-add identical
//! content on every slug wobble, which is precisely the churn the issue
//! forbids.
//!
//! # Purity
//!
//! Like the rest of `stella-core` this module performs no I/O. Loading the
//! published records, re-running extraction, rewriting files and appending
//! retirement events all live in the CLI; this is the decision, testable
//! without a workspace.

/// One already-published record from the lineage being refreshed, reduced to
/// what the diff needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedClaim {
    /// The record's stable lineage id (`ctx.acme.web.pkg-manager`).
    pub lineage_id: String,
    /// The published revision's content-derived id — what a superseding
    /// revision will point at.
    pub record_id: String,
    /// The published claim text.
    pub statement: String,
}

/// One claim the *new* text asserts, whether or not it will become a proposal
/// (claims withheld as already-decided still count as asserted — a withheld
/// claim is exactly the "unchanged" case).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssertedClaim {
    /// The extractor-assigned lineage id for this claim.
    pub lineage_id: String,
    /// The claim text.
    pub statement: String,
}

/// The refresh decision for one source file: every published record of the
/// lineage, partitioned. Buckets preserve the input order of `published`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefreshPlan {
    /// Still asserted, byte-for-byte up to whitespace — keep untouched.
    pub unchanged: Vec<PublishedClaim>,
    /// Same anchor, different text — awaiting review; keeping the new
    /// proposal supersedes these.
    pub changed: Vec<PublishedClaim>,
    /// No longer asserted at all — retire now.
    pub retired: Vec<PublishedClaim>,
}

/// Partition `published` against what the re-read source now asserts.
#[must_use]
pub fn plan(published: Vec<PublishedClaim>, asserted: &[AssertedClaim]) -> RefreshPlan {
    let mut out = RefreshPlan::default();
    for record in published {
        let statement = normalized(&record.statement);
        if asserted
            .iter()
            .any(|claim| normalized(&claim.statement) == statement)
        {
            out.unchanged.push(record);
        } else if asserted
            .iter()
            .any(|claim| claim.lineage_id == record.lineage_id)
        {
            out.changed.push(record);
        } else {
            out.retired.push(record);
        }
    }
    out
}

/// Whether two statements are the same claim under this module's identity —
/// whitespace-insensitive, case-sensitive. Public because supersession at
/// `stella context keep` must draw the same/different line exactly where the
/// refresh diff draws it; two copies of this predicate is how they drift.
#[must_use]
pub fn same_statement(a: &str, b: &str) -> bool {
    normalized(a) == normalized(b)
}

/// Whitespace-insensitive spelling of a statement: trimmed, internal runs
/// collapsed. Case is preserved — a case change changes what the claim says
/// (`must` vs `MUST` is a real edit), while a rewrap is presentation.
fn normalized(statement: &str) -> String {
    statement.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn published(lineage: &str, id: &str, statement: &str) -> PublishedClaim {
        PublishedClaim {
            lineage_id: lineage.to_string(),
            record_id: id.to_string(),
            statement: statement.to_string(),
        }
    }

    fn asserted(lineage: &str, statement: &str) -> AssertedClaim {
        AssertedClaim {
            lineage_id: lineage.to_string(),
            statement: statement.to_string(),
        }
    }

    #[test]
    fn a_still_asserted_claim_is_unchanged() {
        let plan = plan(
            vec![published("ctx.a.pkg", "rec_1", "use pnpm, not npm")],
            &[asserted("ctx.a.pkg", "use pnpm, not npm")],
        );
        assert_eq!(plan.unchanged.len(), 1);
        assert!(plan.changed.is_empty() && plan.retired.is_empty());
    }

    /// The churn guard against a rewrap: same words, different whitespace, is
    /// the same claim.
    #[test]
    fn a_rewrapped_claim_is_unchanged() {
        let plan = plan(
            vec![published("ctx.a.pkg", "rec_1", "use pnpm,   not\nnpm")],
            &[asserted("ctx.a.pkg", "use pnpm, not npm")],
        );
        assert_eq!(plan.unchanged.len(), 1);
    }

    /// The churn guard against the model: identical text under a wobbled slug
    /// is still the same claim. Anchor-first matching would retire-and-re-add
    /// here, which is the churn the issue forbids.
    #[test]
    fn identical_text_under_a_moved_anchor_is_unchanged() {
        let plan = plan(
            vec![published("ctx.a.pkg-manager", "rec_1", "use pnpm, not npm")],
            &[asserted("ctx.a.package-manager", "use pnpm, not npm")],
        );
        assert_eq!(plan.unchanged.len(), 1);
        assert!(plan.retired.is_empty());
    }

    /// A case change is a real edit, not presentation.
    #[test]
    fn a_case_change_is_a_change() {
        let plan = plan(
            vec![published("ctx.a.pkg", "rec_1", "you should use pnpm")],
            &[asserted("ctx.a.pkg", "you MUST use pnpm")],
        );
        assert_eq!(plan.changed.len(), 1);
    }

    #[test]
    fn new_text_at_the_same_anchor_is_a_change_never_a_retirement() {
        let plan = plan(
            vec![published("ctx.a.pkg", "rec_1", "use npm")],
            &[asserted("ctx.a.pkg", "use pnpm, not npm")],
        );
        assert_eq!(plan.changed.len(), 1);
        assert!(plan.retired.is_empty() && plan.unchanged.is_empty());
    }

    #[test]
    fn a_claim_the_source_dropped_is_retired() {
        let plan = plan(
            vec![
                published("ctx.a.pkg", "rec_1", "use pnpm"),
                published("ctx.a.deploy", "rec_2", "deploys run from main"),
            ],
            &[asserted("ctx.a.pkg", "use pnpm")],
        );
        assert_eq!(plan.unchanged.len(), 1);
        assert_eq!(plan.retired.len(), 1);
        assert_eq!(plan.retired[0].record_id, "rec_2");
    }

    /// An extraction that asserts nothing retires everything — the file was
    /// emptied or rewritten wholesale, and the user asked for the refresh.
    #[test]
    fn an_empty_assertion_set_retires_every_record() {
        let plan = plan(
            vec![
                published("ctx.a.pkg", "rec_1", "use pnpm"),
                published("ctx.a.deploy", "rec_2", "deploys run from main"),
            ],
            &[],
        );
        assert_eq!(plan.retired.len(), 2);
    }
}
