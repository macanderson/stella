//! Witness-hunk stripping for verifier-facing diff renders — split out
//! of `verify/tests.rs` to keep it under the file-size ratchet; a child
//! module, so the strip internals stay reachable via `super::*`.

use super::*;

/// The witness is grafted into the candidate tree, so the working-tree diff
/// carries the verifier's own test. It must not ride into the verifier prompt
/// as "worker-authored data" — drop its chunks, return its paths for the
/// trusted evidence zone, and leave every worker chunk byte-intact.
#[test]
fn witness_chunks_are_stripped_and_named_never_billed() {
    let diff = "diff --git a/src/lib.rs b/src/lib.rs\n\
                --- a/src/lib.rs\n\
                +++ b/src/lib.rs\n\
                @@ -1,2 +1,3 @@\n\
                 fn f() {}\n\
                +fn g() {}\n\
                diff --git a/tests/witness_g.rs b/tests/witness_g.rs\n\
                --- /dev/null\n\
                +++ b/tests/witness_g.rs\n\
                @@ -0,0 +1,2 @@\n\
                +#[test]\n\
                +fn g_works() { g(); }\n";
    let stripped = strip_witness_hunks(diff, &["tests/witness_g.rs".to_string()]);
    assert!(
        stripped.diff.contains("+fn g() {}"),
        "worker chunk survives:\n{}",
        stripped.diff
    );
    assert!(
        !stripped.diff.contains("witness_g"),
        "witness chunk is gone:\n{}",
        stripped.diff
    );
    assert_eq!(stripped.omitted, vec!["tests/witness_g.rs".to_string()]);
}

/// No witness, no change: the common test-command path must pass the diff
/// through byte-identically.
#[test]
fn stripping_with_no_witness_paths_is_the_identity() {
    let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n+x\n";
    let stripped = strip_witness_hunks(diff, &[]);
    assert_eq!(stripped.diff, diff);
    assert!(stripped.omitted.is_empty());
}

/// Preamble lines before the first `diff --git` (the honest-diff banner, the
/// untracked-change notes) belong to no file and always survive; and an added
/// content line that happens to start with `+++ ` cannot hijack the chunk's
/// path because only the FIRST `+++ ` per chunk is read.
#[test]
fn preamble_survives_and_content_cannot_hijack_the_chunk_path() {
    let diff = "3 files changed but the probe saw an empty diff\n\
                diff --git a/src/a.rs b/src/a.rs\n\
                --- a/src/a.rs\n\
                +++ b/src/a.rs\n\
                @@ -1 +1,2 @@\n\
                +++ b/tests/witness_g.rs\n\
                +real added line\n";
    let stripped = strip_witness_hunks(diff, &["tests/witness_g.rs".to_string()]);
    assert!(stripped.diff.contains("empty diff"), "preamble survives");
    assert!(
        stripped.diff.contains("+real added line"),
        "the chunk keyed on its real header (src/a.rs), not on forged content:\n{}",
        stripped.diff
    );
    assert!(stripped.omitted.is_empty());
}
