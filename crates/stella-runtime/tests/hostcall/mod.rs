//! What the two `*_plugin_hostcall.rs` harnesses share and the four
//! conformance harnesses have no use for: grading the `.stderr.txt` sibling.
//!
//! A host-call vector is graded by two files. `common::bless_or_assert` grades
//! the response, and every harness in this directory takes it. This one grades
//! the **degradation report** — the line a plugin writes to stderr when the
//! host would not serve a call it asked for — which only exists where a harness
//! plays the host itself and can capture it. The four conformance harnesses
//! drive their vectors through `SubprocessWrapper`, which owns the child's
//! stderr, so they have nothing to pass here.
//!
//! Its own module rather than a third function in `common/mod.rs` for exactly
//! that reason: `mod common;` is compiled into all six test binaries, and an
//! item four of them cannot call is dead code in four of them.
//!
//! Until #4533 this grading was two copies of an inline `if reported.exists()`
//! block with no `BLESS` path at all, and the assertion order made the missing
//! path invisible: `BLESS=1` rewrote the response golden, then panicked on the
//! stale stderr comparison, ending red having repaired half the vector.

use std::path::Path;

/// Compare a vector's degradation report against its committed `.stderr.txt`
/// sibling, or rewrite that sibling from `actual` when `BLESS` is set (#4533).
///
/// # Why the sibling is optional in both directions
///
/// A vector whose host serves every call reports nothing, and carries no
/// `.stderr.txt`. A vector whose host degrades reports one line, and carries
/// one. Which of the two a vector is, is a property of the vector rather than
/// a choice, so blessing has to be able to move it either way: it writes the
/// sibling when there is a report, and **removes** it when there is not. Only
/// writing would leave a vector that stopped degrading graded against a report
/// it no longer produces — green, and describing something that has not
/// happened for however long.
///
/// # Trimmed, on both sides and at both ends
///
/// The comparison has always been `trim()`-to-`trim()`, so an editor that adds
/// or eats a trailing newline cannot fail a harness. Blessing writes the
/// trimmed report plus exactly one newline, which is that comparison's fixed
/// point: re-blessing an unchanged report produces the identical bytes.
///
/// # Panics
///
/// When the report differs from its committed sibling, when a vector with no
/// sibling reported anything, or when the sibling cannot be read or rewritten.
pub fn bless_or_assert_report(name: &str, report_path: &Path, actual: &str) {
    grade_report(
        name,
        report_path,
        actual,
        std::env::var_os("BLESS").is_some(),
    );
}

/// [`bless_or_assert_report`] with the `BLESS` decision handed in.
///
/// Split out so the bless path is testable: `BLESS` is process-global, and a
/// test that set it would decide the mode for every other test sharing the
/// binary. The tests in `goal_plugin_hostcall.rs` drive this directly over a
/// scratch directory instead.
pub fn grade_report(name: &str, report_path: &Path, actual: &str, bless: bool) {
    let actual = actual.trim();

    if bless {
        if actual.is_empty() {
            if report_path.exists() {
                std::fs::remove_file(report_path).expect("the stale report is removable");
            }
            return;
        }
        std::fs::write(report_path, format!("{actual}\n")).expect("the report is writable");
        return;
    }

    if !report_path.exists() {
        assert!(
            actual.is_empty(),
            "{name}: an ungraded vector reported {actual:?}. If the report is intended, \
             regenerate with BLESS=1 and read the diff."
        );
        return;
    }

    let reported = std::fs::read_to_string(report_path).expect("a readable report");
    assert_eq!(
        actual,
        reported.trim(),
        "{name}: the degradation report changed. If the change is intended, regenerate \
         with BLESS=1 and read the diff."
    );
}
