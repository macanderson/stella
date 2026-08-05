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
    let d = diff(&["README.md", "docs/spec/x.md"], "+Some new prose.\n");
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
    for path in [
        ".github/workflows/ci.yml",
        "Cargo.lock",
        "web/yarn.lock",
        "go.sum",
        ".gitlab-ci.yml",
    ] {
        let d = diff(&[path], "+  timeout-minutes: 30\n");
        assert_eq!(
            warrant(&d, 1),
            WitnessWarrant::NotRequired(NoWitnessReason::ConfigOnly),
            "{path} is a build/CI/dependency manifest"
        );
    }
}

#[test]
fn behavior_bearing_yaml_is_not_config() {
    // The hole this closes: `is_config` matched every `*.yml`/`*.yaml`/`*.lock`
    // by suffix, so a compose file, a Helm values file, or an app's own config
    // completed with a PASS asserting "the build and gate verify these" while
    // nothing had verified anything. Yaml is a syntax, not a category.
    for path in [
        "docker-compose.yml",
        "deploy/values.yaml",
        "config/database.yml",
        "k8s/deployment.yaml",
        "vendor/flock.lock",
    ] {
        let d = diff(&[path], "+  replicas: 3\n");
        assert_eq!(
            warrant(&d, 1),
            WitnessWarrant::Required,
            "{path} carries behavior and must keep its verification"
        );
    }
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
fn a_rust_deref_assignment_is_code_not_a_comment() {
    // `*counter = …` starts with `*`, the block-comment continuation marker,
    // but writes through a pointer. It must not read as comments-only.
    let d = diff(
        &["stella-core/src/driver.rs"],
        "-    *counter = 0;\n+    *counter = 1;\n",
    );
    assert_eq!(
        warrant(&d, 1),
        WitnessWarrant::Required,
        "a deref assignment is a behavior change, not a comment"
    );
}

#[test]
fn a_js_private_field_and_c_preprocessor_are_code_not_comments() {
    let js = diff(
        &["ui/src/wallet.ts"],
        "-    #balance = 0;\n+    #balance = 100;\n",
    );
    assert_eq!(warrant(&js, 1), WitnessWarrant::Required);
    let c = diff(
        &["native/buffer.c"],
        "-#define BUFFER 256\n+#define BUFFER 512\n",
    );
    assert_eq!(warrant(&c, 1), WitnessWarrant::Required);
}

#[test]
fn a_decrement_is_code_not_a_sql_comment() {
    let d = diff(
        &["stella-core/src/driver.rs"],
        "-    --count;\n+    --count;\n+    step();\n",
    );
    assert_eq!(warrant(&d, 1), WitnessWarrant::Required);
}

#[test]
fn block_and_sql_comments_still_read_as_comments() {
    // The delimited markers must still recognize genuine comments: a
    // block-comment continuation, a block close, and a spaced SQL comment.
    let d = diff(
        &["stella-core/src/driver.rs"],
        "+ * a doc line\n+ */\n+-- a note\n+# a python note\n",
    );
    assert_eq!(
        warrant(&d, 1),
        WitnessWarrant::NotRequired(NoWitnessReason::CommentsOnly)
    );
}

#[test]
fn an_unrecognized_path_buys_the_test() {
    let d = diff(&["some/unknown/thing.zz"], "+data\n");
    assert_eq!(warrant(&d, 1), WitnessWarrant::Required);
}

#[test]
fn an_untracked_source_file_beside_a_docs_edit_is_a_source_change() {
    // The hole this closes: `git diff` cannot see untracked files, so a NEW
    // source file arrives only as the synthetic marker line — which is not a
    // diff header, so no path rule ever saw it. The change classified as
    // DocsOnly and skipped both the witness and the reviewer while shipping
    // new code.
    let d = format!(
        "{}{}\n",
        diff(&["README.md"], "+Some new prose.\n"),
        untracked_change_line("src/backdoor.rs", 42)
    );
    assert_eq!(
        warrant(&d, 2),
        WitnessWarrant::Required,
        "an untracked source file must count as a source change"
    );
}

#[test]
fn an_untracked_docs_file_beside_a_docs_edit_stays_docs_only() {
    let d = format!(
        "{}{}\n",
        diff(&["README.md"], "+Some new prose.\n"),
        untracked_change_line("docs/notes.md", 8)
    );
    assert_eq!(
        warrant(&d, 2),
        WitnessWarrant::NotRequired(NoWitnessReason::DocsOnly)
    );
}

#[test]
fn an_untracked_only_change_still_buys_the_test() {
    // No diff headers at all: the marker names a path but shows no content,
    // so the warrant cannot read the change and must not waive it — even for
    // a docs-shaped path.
    for path in ["src/new_module.rs", "docs/new_page.md"] {
        let d = format!("{}\n", untracked_change_line(path, 12));
        assert_eq!(
            warrant(&d, 1),
            WitnessWarrant::Required,
            "{path}: an untracked-only change is unreadable and must fail closed"
        );
    }
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
