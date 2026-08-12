//! #2873 removed the tool-call tally from every ladder predicate; #2934
//! removed it from the wire entirely (`LadderInputs`/`LadderSnapshot` no
//! longer carry a `file_change_events` field). The two tests that swept that
//! field across the ladder's decision surface (`the_decision_never_moves_with_
//! the_tool_tally`, `no_predicate_reads_the_tool_tally`) tested a premise —
//! "a field the ladder must ignore" — that no longer exists once the field is
//! gone, so they were deleted rather than adapted.
//!
//! This module's one surviving case is orthogonal to the field: it checks
//! that the human-readable `UNVERIFIABLE` summary never claims a file-change
//! count as evidence, regardless of what did or didn't produce one.

use crate::verify::LadderInputs;

/// The verdict a human reads must not present a tool-call tally as a statement
/// about the tree. Both `UNVERIFIABLE` summaries carried the sentence
/// "file-change events recorded = 0" — one of them hardcoded — and a reader of
/// the panel's `configure-git-webserver` verdict saw it beside 24 mutating
/// calls that had, in fact, changed files.
#[test]
fn the_unverifiable_summary_makes_no_file_change_count_claim() {
    let escaped = LadderInputs {
        mutating_actions: 24,
        diff_available: true,
        diff_lines: 0,
        ..Default::default()
    };
    let blind = LadderInputs {
        diff_available: false,
        ..escaped
    };
    for (label, inputs) in [("escaped", &escaped), ("blind", &blind)] {
        let summary = crate::verify::unverifiable_evidence(inputs).summary;
        assert!(
            !summary.contains("file-change event"),
            "{label}: the summary still reports a tool tally as evidence: {summary}"
        );
        assert!(
            summary.contains("24"),
            "{label}: and the dispatch count that contradicts a clean tree has to stay: {summary}"
        );
    }
}
