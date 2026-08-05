//! Diff-coverage overlap (#1291): did the passing test actually run the lines
//! that changed?
//!
//! A fail→pass flip proves the test *reacted* to the change, and #870's
//! mutation check proves it *constrains* the changed lines — when a witness
//! was authored. Neither covers the plainest coincidence of all: a suite that
//! goes green without ever executing the new code. Then "the test passed" is a
//! fact about some other code, reported as evidence about this one.
//!
//! # The two lists
//!
//! [`changed_lines`] reads the candidate's own unified diff for the lines it
//! ADDED — the candidate's claim. [`overlap`] intersects them with the lines a
//! coverage run says were executed. The answer is deliberately three-valued
//! ([`DiffCoverage`]), because "the test did not run this" and "nobody could
//! tell whether the test ran this" are different findings and only one of them
//! is about the work.
//!
//! # Unproven is not failed — and unproven is not passed either
//!
//! Neither non-`Covered` answer may fail a candidate, and neither may be
//! reported as a verified pass. The two rungs differ in *what they spend*:
//!
//! - **`NotCovered`** — positive evidence that the test ran none of the
//!   changed lines. Withholds the deterministic credit and sends the turn to
//!   the verifier, the same treatment a tautological witness gets (#870). The
//!   pass is worth a second opinion, because something concrete is wrong with
//!   the evidence.
//! - **`Unmeasured`** — nobody could tell. Keeps the fast-submit (no verifier
//!   call, no extra turn) but scores the candidate `Unverified` rather than
//!   `DeterministicPass`, and the verdict's summary leads with `UNPROVEN`.
//!
//! That asymmetry is the whole design. A run is not broken by the absence of
//! a way to check it, so an absent coverage tool must not cost a turn — but
//! it must not buy the ladder's strongest badge either. Paying for the honest
//! answer with a *ranking position* instead of a *model call* is what makes
//! it affordable to be honest on every run, in every workspace, by default.
//!
//! Escalating instead was considered and rejected on the measurement #1295
//! records: a gate whose condition holds on nearly every turn is not a gate,
//! it is a tax. Most workspaces have no coverage tooling wired, so escalating
//! `Unmeasured` would route almost every deterministic pass through a paid
//! verifier call to be told what the evidence already said.
//! `PipelineConfig::require_diff_coverage` turns that stricter reading on for
//! an operator who has the tooling and wants the overlap *enforced* rather
//! than merely scored.
//!
//! # Parsing, and why it lives here
//!
//! [`parse_lcov`] and [`parse_coverage_json`] are pure functions over report
//! text — the two formats the dialects this pipeline can already *read test
//! output* for emit (`cargo llvm-cov --lcov`, `coverage.py`/`pytest-cov`'s
//! JSON). Running the instrumented suite is the host's job
//! ([`crate::ports::CoverageProbe`]); recognizing what it produced is a
//! decision over owned data, which belongs in the engine where it is testable
//! without a toolchain.

use std::collections::{BTreeMap, BTreeSet};

/// Executed lines, per repo-relative path — what a coverage run observed.
///
/// A path present with an empty set is meaningful and distinct from an absent
/// path: the file was instrumented and *none* of its lines ran. Both read as
/// "not covered" for the overlap, but only the first is evidence.
pub type CoverageReport = BTreeMap<String, BTreeSet<u32>>;

/// What a coverage observation says about the changed lines (#1291).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffCoverage {
    /// No coverage observation was available: no tooling for this dialect,
    /// the probe was not wired, or the run produced no readable report. The
    /// default, and never a finding about the work.
    #[default]
    Unmeasured,
    /// At least one line the candidate added was executed by the test run.
    Covered,
    /// The run WAS measured, the candidate did add lines, and none of them
    /// executed. The flip is a coincidence rather than evidence — unproven,
    /// never failed.
    NotCovered,
}

impl DiffCoverage {
    /// The wire/evidence token, so a rendered explanation and a recorded
    /// snapshot spell the status identically.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DiffCoverage::Unmeasured => "unmeasured",
            DiffCoverage::Covered => "covered",
            DiffCoverage::NotCovered => "not_covered",
        }
    }

    /// A sentence a verifier (or a human reading a verdict) can act on. States
    /// what was observed and what it does NOT mean — the unmeasured case in
    /// particular is a statement about the instrument, and the whole point of
    /// #973 is that those must never read as statements about the world.
    #[must_use]
    pub fn explain(self) -> &'static str {
        match self {
            DiffCoverage::Unmeasured => {
                "diff_coverage=unmeasured (no coverage tooling was available for this test \
                 command, so whether the test ran the changed lines is UNKNOWN — this is not a \
                 finding that it did not, and not a verified pass either)"
            }
            DiffCoverage::Covered => {
                "diff_coverage=covered (the test run executed lines the change added)"
            }
            DiffCoverage::NotCovered => {
                "diff_coverage=not_covered (the test run executed NONE of the lines the change \
                 added — the pass is unproven for this change, not a finding that the change is \
                 wrong)"
            }
        }
    }

    /// Whether this status may carry a deterministic fast-submit — that is,
    /// whether the ladder may *skip the verifier*.
    ///
    /// Distinct from whether the result is *proven*: an `Unmeasured` overlap
    /// takes the fast-submit under the default setting but is still scored
    /// `Unverified` (see the `SubmitFast` arm in `crate::pipeline`). This
    /// answers "must a reviewer be bought?", not "was this verified?".
    ///
    /// `strict` is `PipelineConfig::require_diff_coverage`: off, only a
    /// measured non-overlap escalates; on, an unmeasured overlap escalates
    /// too. `NotCovered` never credits under either setting.
    #[must_use]
    pub fn credits_a_deterministic_pass(self, strict: bool) -> bool {
        match self {
            DiffCoverage::Covered => true,
            DiffCoverage::NotCovered => false,
            DiffCoverage::Unmeasured => !strict,
        }
    }
}

/// The lines a unified diff ADDS, per repo-relative path, excluding
/// `excluded_paths` (the witness's own files — a test covering itself proves
/// nothing about the change).
///
/// Added lines only, and for the same reason [`crate::verify::mutation`] uses
/// them: they are the candidate's claim. A deleted line cannot be executed by
/// anything, so counting removals would make a pure deletion look uncoverable
/// and downgrade an honest change.
///
/// Line numbers are on the NEW side of the diff, which is the side a coverage
/// report describes.
#[must_use]
pub fn changed_lines(diff: &str, excluded_paths: &[String]) -> BTreeMap<String, BTreeSet<u32>> {
    let mut changed: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    let mut current_path: Option<String> = None;
    let mut new_line: u32 = 0;
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            let path = path.strip_prefix("b/").unwrap_or(path).trim();
            current_path = (path != "/dev/null").then(|| path.to_string());
            continue;
        }
        if line.starts_with("@@") {
            // `@@ -a,b +c,d @@` — the new-file cursor starts at c.
            new_line = line
                .split('+')
                .nth(1)
                .and_then(|rest| {
                    rest.split([',', ' '])
                        .next()
                        .and_then(|n| n.parse::<u32>().ok())
                })
                .unwrap_or(0);
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => {
                if let Some(path) = &current_path
                    && !excluded_paths.iter().any(|p| p == path)
                    // A blank or brace-only line is never attributed by a
                    // coverage tool, so requiring one to be "executed" would
                    // manufacture non-overlap out of formatting.
                    && is_executable_source(&line[1..])
                {
                    changed.entry(path.clone()).or_default().insert(new_line);
                }
                new_line += 1;
            }
            Some(b'-') => {}
            _ => new_line += 1,
        }
    }
    changed
}

/// Whether a source line is the kind a coverage tool attributes execution to.
///
/// Deliberately crude, and deliberately biased toward *excluding*: every line
/// wrongly kept here can only produce a false `NotCovered`, which costs a
/// verifier call on honest work. Blank lines, closing braces, and comments are
/// not emitted as coverable by lcov or coverage.py, so a change consisting
/// only of those is `Unmeasured` (no coverable lines) rather than uncovered.
fn is_executable_source(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Braces, brackets, and their comma/semicolon tails: structure, not
    // statements.
    if trimmed
        .chars()
        .all(|c| matches!(c, '{' | '}' | '(' | ')' | '[' | ']' | ',' | ';'))
    {
        return false;
    }
    !(trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*'))
}

/// Intersect the changed lines with the executed ones (#1291).
///
/// Three ways to reach [`DiffCoverage::Unmeasured`], and they are all
/// "nothing to compare" rather than "no overlap":
///
/// - no report (`None`) — no tooling, or an unreadable one;
/// - no changed line that a coverage tool would attribute (a whitespace,
///   comment, or brace-only change);
/// - a report that mentions **none** of the changed files at all, which is
///   what a partial or misrooted report looks like. Reading that as
///   `NotCovered` would let a path-prefix mismatch — the single most common
///   way coverage plumbing goes wrong — masquerade as a finding about the
///   test.
#[must_use]
pub fn overlap(
    changed: &BTreeMap<String, BTreeSet<u32>>,
    covered: Option<&CoverageReport>,
) -> DiffCoverage {
    let Some(covered) = covered else {
        return DiffCoverage::Unmeasured;
    };
    if changed.is_empty() {
        return DiffCoverage::Unmeasured;
    }
    let mut saw_a_changed_file = false;
    for (path, lines) in changed {
        let Some(executed) = covered.get(path) else {
            continue;
        };
        saw_a_changed_file = true;
        if lines.iter().any(|line| executed.contains(line)) {
            return DiffCoverage::Covered;
        }
    }
    if saw_a_changed_file {
        DiffCoverage::NotCovered
    } else {
        DiffCoverage::Unmeasured
    }
}

/// Parse an LCOV tracefile (`cargo llvm-cov --lcov`, `nyc`, `jest`) into the
/// lines that actually executed.
///
/// Only two record types matter: `SF:<path>` opens a file section and
/// `DA:<line>,<hits>` reports one line's hit count. A `DA` with zero hits is a
/// line the tool instrumented and never ran, so it is deliberately *not*
/// recorded — the report is "what executed", not "what exists".
///
/// Unknown records are skipped rather than failing the parse: LCOV grew
/// branch, function, and checksum records over time, and a tracefile carrying
/// a record this does not know is still a perfectly good statement about
/// lines.
#[must_use]
pub fn parse_lcov(text: &str) -> CoverageReport {
    let mut report = CoverageReport::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(path) = line.strip_prefix("SF:") {
            let path = normalize_path(path);
            // Insert eagerly: a file with no executed line still has to be
            // *present*, so `overlap` can tell "instrumented and never run"
            // from "this report says nothing about that file".
            report.entry(path.clone()).or_default();
            current = Some(path);
            continue;
        }
        if line == "end_of_record" {
            current = None;
            continue;
        }
        let Some(rest) = line.strip_prefix("DA:") else {
            continue;
        };
        let Some(path) = &current else { continue };
        let mut parts = rest.split(',');
        let (Some(number), Some(hits)) = (parts.next(), parts.next()) else {
            continue;
        };
        let (Ok(number), Ok(hits)) = (number.trim().parse::<u32>(), hits.trim().parse::<u64>())
        else {
            continue;
        };
        if hits > 0 {
            report.entry(path.clone()).or_default().insert(number);
        }
    }
    report
}

/// Parse a `coverage.py` JSON report (`coverage json`, `pytest --cov-report
/// =json`) into the lines that executed.
///
/// The shape read is `{"files": {"<path>": {"executed_lines": [1, 2, …]}}}`.
/// Everything else in the document — summaries, missing lines, contexts,
/// metadata — is ignored, so a newer coverage.py that adds keys still parses.
/// An unparseable document yields an empty report, which reads as
/// [`DiffCoverage::Unmeasured`] rather than as a finding.
#[must_use]
pub fn parse_coverage_json(text: &str) -> CoverageReport {
    let mut report = CoverageReport::new();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return report;
    };
    let Some(files) = value.get("files").and_then(serde_json::Value::as_object) else {
        return report;
    };
    for (path, entry) in files {
        let lines = report.entry(normalize_path(path)).or_default();
        let Some(executed) = entry
            .get("executed_lines")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for line in executed {
            if let Some(number) = line.as_u64().and_then(|n| u32::try_from(n).ok()) {
                lines.insert(number);
            }
        }
    }
    report
}

/// Normalize a reported path toward the repo-relative spelling a diff uses:
/// strip a leading `./`, and keep everything else verbatim.
///
/// Deliberately does NOT try to relativize an absolute path against a guessed
/// root. A wrong guess produces a path that matches nothing, and a report that
/// mentions none of the changed files is `Unmeasured` by construction — the
/// honest outcome for plumbing that did not line up.
fn normalize_path(path: &str) -> String {
    path.trim().trim_start_matches("./").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,3 +10,5 @@
 fn clamp(x: u32, hi: u32) -> u32 {
-    if x > hi { hi } else { x }
+    // clamp from below too
+    if x >= hi { hi } else { x }
+
 }
";

    fn covered(entries: &[(&str, &[u32])]) -> CoverageReport {
        entries
            .iter()
            .map(|(path, lines)| ((*path).to_string(), lines.iter().copied().collect()))
            .collect()
    }

    /// Only ADDED lines, numbered on the new side, and only the ones a
    /// coverage tool would attribute: the comment and the whitespace-only
    /// line are not coverable, so requiring them to execute would invent a
    /// non-overlap out of formatting.
    #[test]
    fn changed_lines_are_the_added_coverable_ones() {
        let changed = changed_lines(DIFF, &[]);
        assert_eq!(changed.len(), 1);
        let lines = &changed["src/lib.rs"];
        assert_eq!(
            lines.iter().copied().collect::<Vec<_>>(),
            vec![12],
            "line 11 is the comment, 13 is whitespace — neither is coverable"
        );
    }

    /// The witness's own files are excluded: a test that covers itself says
    /// nothing about whether it covers the change.
    #[test]
    fn excluded_paths_contribute_no_changed_lines() {
        let diff =
            "--- a/tests/witness.rs\n+++ b/tests/witness.rs\n@@ -1,0 +1,1 @@\n+assert!(x);\n";
        assert!(changed_lines(diff, &["tests/witness.rs".to_string()]).is_empty());
        assert!(!changed_lines(diff, &[]).is_empty());
    }

    /// The three outcomes, over the same changed set.
    #[test]
    fn overlap_is_three_valued() {
        let changed = changed_lines(DIFF, &[]);
        assert_eq!(
            overlap(&changed, Some(&covered(&[("src/lib.rs", &[12, 40])]))),
            DiffCoverage::Covered
        );
        assert_eq!(
            overlap(&changed, Some(&covered(&[("src/lib.rs", &[40, 41])]))),
            DiffCoverage::NotCovered,
            "the file WAS measured and the changed line did not run"
        );
        assert_eq!(
            overlap(&changed, None),
            DiffCoverage::Unmeasured,
            "no report is not a finding"
        );
    }

    /// A report that mentions none of the changed files is unmeasured, not
    /// uncovered — the shape a misrooted or partial report takes, and the one
    /// most likely to be a plumbing mistake rather than a fact about the test.
    #[test]
    fn a_report_that_names_no_changed_file_is_unmeasured() {
        let changed = changed_lines(DIFF, &[]);
        assert_eq!(
            overlap(&changed, Some(&covered(&[("src/other.rs", &[1, 2, 3])]))),
            DiffCoverage::Unmeasured
        );
        assert_eq!(
            overlap(&BTreeMap::new(), Some(&covered(&[("src/lib.rs", &[12])]))),
            DiffCoverage::Unmeasured,
            "a change with no coverable line has nothing to prove either way"
        );
    }

    /// LCOV: `DA` lines with zero hits are instrumented-but-not-run, so the
    /// file appears with no executed lines — which is a `NotCovered`, not an
    /// `Unmeasured`.
    #[test]
    fn lcov_records_only_the_lines_that_ran() {
        let lcov = "\
TN:
SF:./src/lib.rs
FN:10,clamp
DA:10,3
DA:12,0
BRDA:12,0,0,-
LF:2
LH:1
end_of_record
SF:src/other.rs
DA:1,7
end_of_record
";
        let report = parse_lcov(lcov);
        assert_eq!(
            report["src/lib.rs"].iter().copied().collect::<Vec<_>>(),
            vec![10],
            "line 12 was instrumented and never executed"
        );
        assert_eq!(report["src/other.rs"].len(), 1);
        let changed: BTreeMap<String, BTreeSet<u32>> =
            [("src/lib.rs".to_string(), [12].into_iter().collect())].into();
        assert_eq!(overlap(&changed, Some(&report)), DiffCoverage::NotCovered);
    }

    /// coverage.py JSON: the executed lines, with unknown keys ignored so a
    /// newer report still parses.
    #[test]
    fn coverage_json_reads_executed_lines_and_ignores_the_rest() {
        let json = r#"{
            "meta": {"version": "7.6.1"},
            "files": {
                "./app/service.py": {
                    "executed_lines": [1, 4, 5],
                    "missing_lines": [6],
                    "summary": {"percent_covered": 83.3}
                },
                "app/untouched.py": {"executed_lines": []}
            },
            "totals": {"percent_covered": 83.3}
        }"#;
        let report = parse_coverage_json(json);
        assert_eq!(
            report["app/service.py"].iter().copied().collect::<Vec<_>>(),
            vec![1, 4, 5]
        );
        assert!(
            report.contains_key("app/untouched.py"),
            "an instrumented file with nothing executed must still be present"
        );
        // A document that is not coverage JSON degrades to an empty report,
        // which the ladder reads as unmeasured — never as a finding.
        assert!(parse_coverage_json("not json at all").is_empty());
        assert!(parse_coverage_json(r#"{"files": 3}"#).is_empty());
    }

    /// The strictness switch: only `require_diff_coverage` makes an
    /// unmeasured overlap withhold a deterministic pass, and nothing makes a
    /// measured non-overlap credit one.
    #[test]
    fn only_strict_mode_withholds_on_an_unmeasured_overlap() {
        assert!(DiffCoverage::Covered.credits_a_deterministic_pass(false));
        assert!(DiffCoverage::Covered.credits_a_deterministic_pass(true));
        assert!(DiffCoverage::Unmeasured.credits_a_deterministic_pass(false));
        assert!(!DiffCoverage::Unmeasured.credits_a_deterministic_pass(true));
        assert!(!DiffCoverage::NotCovered.credits_a_deterministic_pass(false));
        assert!(!DiffCoverage::NotCovered.credits_a_deterministic_pass(true));
    }
}
