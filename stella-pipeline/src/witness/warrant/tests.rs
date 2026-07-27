//! Tests for the witness warrant. The load-bearing property is the *closed*
//! direction: anything mixed, unrecognized, or invisible must fall through to
//! `Required`, because a false "no test needed" ships unverified behavior.

use super::*;

fn diff(paths: &[&str], body: &str) -> String {
    let mut out = String::new();
    for path in paths {
        out.push_str(&format!("--- a/{path}\n+++ b/{path}\n"));
    }
    out.push_str(body);
    out
}

#[test]
fn a_question_that_touched_nothing_needs_no_test() {
    assert_eq!(
        warrant("", 0),
        WitnessWarrant::NotRequired(NoWitnessReason::NothingChanged)
    );
}

#[test]
fn a_turn_that_touched_files_invisibly_still_needs_one() {
    // The honesty guard: FileChange events fired but the diff is blind. This
    // must never read as "nothing happened".
    assert_eq!(warrant("", 3), WitnessWarrant::Required);
}

#[test]
fn docs_only_needs_no_test() {
    let d = diff(&["README.md", "docs/design/x.md"], "+Some new prose.\n");
    assert_eq!(
        warrant(&d, 2),
        WitnessWarrant::NotRequired(NoWitnessReason::DocsOnly)
    );
}

#[test]
fn tests_only_needs_no_test() {
    let d = diff(&["stella-core/src/foo/tests.rs"], "+assert_eq!(1, 1);\n");
    assert_eq!(
        warrant(&d, 1),
        WitnessWarrant::NotRequired(NoWitnessReason::TestsOnly)
    );
}

#[test]
fn config_only_needs_no_test() {
    let d = diff(&[".github/workflows/ci.yml"], "+  timeout-minutes: 30\n");
    assert_eq!(
        warrant(&d, 1),
        WitnessWarrant::NotRequired(NoWitnessReason::ConfigOnly)
    );
}

#[test]
fn comments_only_needs_no_test() {
    let d = diff(
        &["stella-core/src/driver.rs"],
        "-// old note\n+// a clearer note\n+\n",
    );
    assert_eq!(
        warrant(&d, 1),
        WitnessWarrant::NotRequired(NoWitnessReason::CommentsOnly)
    );
}

#[test]
fn a_pure_removal_needs_no_test() {
    let d = diff(
        &["stella-core/src/dead.rs"],
        "-pub fn unused() -> u32 {\n-    7\n-}\n",
    );
    assert_eq!(
        warrant(&d, 1),
        WitnessWarrant::NotRequired(NoWitnessReason::PureRemoval)
    );
}

#[test]
fn a_removal_that_leaves_a_note_is_still_a_removal() {
    let d = diff(
        &["stella-core/src/dead.rs"],
        "-pub fn unused() -> u32 {\n-    7\n-}\n+// removed: superseded by `live()`\n",
    );
    assert_eq!(
        warrant(&d, 1),
        WitnessWarrant::NotRequired(NoWitnessReason::PureRemoval)
    );
}

// ---- the closed direction: these must all buy the test ----

#[test]
fn docs_plus_source_is_a_source_change() {
    let d = diff(
        &["README.md", "stella-core/src/driver.rs"],
        "+let x = compute();\n",
    );
    assert_eq!(
        warrant(&d, 2),
        WitnessWarrant::Required,
        "a source change that also updated its docs is still a source change"
    );
}

#[test]
fn tests_plus_source_is_a_source_change() {
    let d = diff(
        &["tests/a.rs", "stella-core/src/driver.rs"],
        "+let x = compute();\n",
    );
    assert_eq!(warrant(&d, 2), WitnessWarrant::Required);
}

#[test]
fn a_removal_that_adds_real_code_is_a_rewrite() {
    let d = diff(
        &["stella-core/src/driver.rs"],
        "-pub fn old() -> u32 { 7 }\n+pub fn new() -> u32 { 8 }\n",
    );
    assert_eq!(
        warrant(&d, 1),
        WitnessWarrant::Required,
        "replacing code is a behavior change, not a removal"
    );
}

#[test]
fn an_unrecognized_path_buys_the_test() {
    let d = diff(&["some/unknown/thing.zz"], "+data\n");
    assert_eq!(warrant(&d, 1), WitnessWarrant::Required);
}

#[test]
fn a_source_file_under_a_docs_named_crate_is_not_docs() {
    // `stella-docs/src/lib.rs` is Rust, not prose.
    let d = diff(&["stella-docs/src/lib.rs"], "+pub fn render() {}\n");
    assert_eq!(warrant(&d, 1), WitnessWarrant::Required);
}

#[test]
fn every_reason_states_why_rather_than_merely_asserting() {
    for reason in [
        NoWitnessReason::NothingChanged,
        NoWitnessReason::DocsOnly,
        NoWitnessReason::TestsOnly,
        NoWitnessReason::ConfigOnly,
        NoWitnessReason::CommentsOnly,
        NoWitnessReason::PureRemoval,
    ] {
        let sentence = reason.sentence();
        assert!(sentence.len() > 20, "{reason:?} has no stated reason");
        assert!(
            sentence.contains(';') || sentence.contains(','),
            "{reason:?} states what but not why: {sentence}"
        );
    }
}
