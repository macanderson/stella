//! Failure-fingerprint extraction (#867): which *tests* a runner's output
//! says passed and failed, parsed from the tails the pipeline already holds.
//!
//! The oracle's same-command rule pins the command; this pins the *failure*.
//! The hole it closes is fix-by-disappearance: a candidate that deletes or
//! renames the failing test makes the suite exit 0, and the exit code alone
//! reads that as a verified fail→pass flip of nothing. With the parsed test
//! lists, a pass that names its tests and does NOT name the baseline's
//! failures among them is a pass that demonstrably fixed a *different*
//! failure — `NoEvidence`, mirroring the same-command rule one level down.
//!
//! Everything here degrades open. Output that parses to nothing — a runner
//! dialect we don't know, a tail whose test lines were truncated away —
//! returns `None`, and the caller keeps the exit-code behavior it had before
//! this module existed. The refusal fires only on positive evidence of a
//! different fix, never on absence of evidence.

use std::collections::BTreeSet;

/// Test identities one run's output names, split by result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TestResults {
    pub passed: BTreeSet<String>,
    pub failed: BTreeSet<String>,
    /// The pass count the runner's own summary line(s) reported — summed
    /// across libtest's per-binary `test result:` lines. `None` when no
    /// summary survived the tail.
    pub reported_passed: Option<u32>,
}

impl TestResults {
    /// Whether the output named any test at all — the gate on every consumer:
    /// an empty parse is "this dialect/tail showed nothing", never "no tests
    /// ran".
    pub fn named_any(&self) -> bool {
        !self.passed.is_empty() || !self.failed.is_empty()
    }

    /// Whether the parsed pass-list is demonstrably COMPLETE: the runner's
    /// own summary count equals the names parsed. This is the license the
    /// #867 refusal requires — a middle-out-truncated tail can drop the one
    /// `ok` line that mattered while keeping others, and refusing a flip on
    /// a hole in the *listing* would fail honest fixes. No summary, or a
    /// count mismatch, means the listing proves nothing about absence.
    pub fn pass_listing_complete(&self) -> bool {
        self.reported_passed == Some(self.passed.len() as u32)
    }
}

/// Parse the test names a runner's combined output tail reports, or `None`
/// when no known dialect matched anything. Understands:
///
/// - **cargo test / libtest**: `test path::to::case ... ok` / `... FAILED`
///   (ignored/benched lines are neither passed nor failed).
/// - **pytest**: `PASSED tests/x.py::case` / `FAILED tests/x.py::case`
///   (short summary), and the `-v` order `tests/x.py::case PASSED [ 42%]`.
///
/// Truncated tails parse to whatever lines survived — consumers must treat
/// the sets as "what the output *named*", not as the full suite.
pub fn parse_test_results(output: &str) -> Option<TestResults> {
    let mut results = TestResults::default();
    for line in output.lines() {
        let line = line.trim();
        parse_libtest(line, &mut results);
        parse_pytest(line, &mut results);
        parse_summary(line, &mut results);
    }
    results.named_any().then_some(results)
}

/// Accumulate the runner's own pass counts: libtest's per-binary
/// `test result: ok. 3 passed; …` (a workspace run prints one per test
/// binary — they sum) and pytest's `== 3 passed in 0.12s ==`.
fn parse_summary(line: &str, results: &mut TestResults) {
    let counted = if let Some(rest) = line.strip_prefix("test result:") {
        count_before_keyword(rest, "passed")
    } else if line.starts_with('=') && line.contains(" passed") {
        count_before_keyword(line.trim_matches(['=', ' ']), "passed")
    } else {
        None
    };
    if let Some(n) = counted {
        *results.reported_passed.get_or_insert(0) += n;
    }
}

/// `… 3 passed …` → 3: the number token immediately before `passed`.
fn count_before_keyword(text: &str, keyword: &str) -> Option<u32> {
    let mut previous: Option<&str> = None;
    for token in text.split([' ', ',', ';', '.']) {
        if token == keyword {
            return previous?.parse().ok();
        }
        if !token.is_empty() {
            previous = Some(token);
        }
    }
    None
}

/// `test name ... ok` / `test name ... FAILED`. The `test result:` summary
/// line and doc-test headers share the prefix but not the ` ... ` shape.
fn parse_libtest(line: &str, results: &mut TestResults) {
    let Some(rest) = line.strip_prefix("test ") else {
        return;
    };
    let Some((name, verdict)) = rest.rsplit_once(" ... ") else {
        return;
    };
    let name = name.trim();
    if name.is_empty() || name.contains(' ') {
        // `test result: ok. 3 passed …` and friends.
        return;
    }
    match verdict.trim() {
        "ok" => {
            results.passed.insert(name.to_string());
        }
        "FAILED" => {
            results.failed.insert(name.to_string());
        }
        _ => {}
    }
}

/// pytest's two orders: `FAILED tests/x.py::case - assert …` (short summary)
/// and `tests/x.py::case FAILED [ 42%]` (verbose progress).
fn parse_pytest(line: &str, results: &mut TestResults) {
    for (marker, passed) in [("PASSED", true), ("FAILED", false)] {
        if let Some(rest) = line.strip_prefix(marker) {
            let name = rest.split_whitespace().next().unwrap_or_default();
            if name.contains("::") {
                insert(results, name, passed);
            }
            continue;
        }
        // Verbose order: the node id first, the verdict after.
        let mut parts = line.split_whitespace();
        if let (Some(name), Some(verdict)) = (parts.next(), parts.next())
            && name.contains("::")
            && verdict == marker
        {
            insert(results, name, passed);
        }
    }
}

fn insert(results: &mut TestResults, name: &str, passed: bool) {
    // The trailing ` - assert…` reason is split off by whitespace already;
    // strip a trailing comma some wrappers add.
    let name = name.trim_end_matches(',');
    if passed {
        results.passed.insert(name.to_string());
    } else {
        results.failed.insert(name.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libtest_lines_parse_into_passed_and_failed() {
        let out = "\nrunning 3 tests\n\
                   test verify::tests::flip_works ... ok\n\
                   test verify::tests::oracle_locks ... FAILED\n\
                   test verify::tests::slow_case ... ignored\n\
                   \ntest result: FAILED. 1 passed; 1 failed; 1 ignored\n";
        let r = parse_test_results(out).expect("libtest lines parse");
        assert!(r.passed.contains("verify::tests::flip_works"));
        assert!(r.failed.contains("verify::tests::oracle_locks"));
        assert_eq!(r.passed.len(), 1);
        assert_eq!(r.failed.len(), 1);
    }

    #[test]
    fn the_summary_line_is_not_a_test() {
        assert_eq!(
            parse_test_results("test result: ok. 3 passed; 0 failed"),
            None
        );
    }

    #[test]
    fn pytest_short_summary_and_verbose_orders_both_parse() {
        let short = "FAILED tests/test_x.py::test_alpha - assert 1 == 2";
        let r = parse_test_results(short).expect("short summary parses");
        assert!(r.failed.contains("tests/test_x.py::test_alpha"));

        let verbose = "tests/test_x.py::test_beta PASSED [ 50%]\n\
                       tests/test_x.py::test_gamma FAILED [100%]";
        let r = parse_test_results(verbose).expect("verbose parses");
        assert!(r.passed.contains("tests/test_x.py::test_beta"));
        assert!(r.failed.contains("tests/test_x.py::test_gamma"));
    }

    #[test]
    fn unknown_dialects_parse_to_none_not_empty_certainty() {
        assert_eq!(parse_test_results("make: *** [all] Error 2"), None);
        assert_eq!(parse_test_results(""), None);
    }
}
