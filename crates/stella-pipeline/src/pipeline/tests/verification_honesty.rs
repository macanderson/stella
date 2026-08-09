//! The verification-honesty guard (`verify_probes::verification_honest_diff`)
//! observed at its seam: an empty diff beside real file-change events must
//! read as "couldn't capture", never "verified nothing". Split out of
//! `tests.rs`, which is closed to growth.

use super::super::verify_probes::verification_honest_diff;

/// The archetypal lie: the turn emitted file-change events, but the diff
/// came back empty (committed work, a baseline miss, an uncaptured file).
/// A bare empty string reads to a verifier as "no changes were made" — the
/// signal that once drove an agent to reinitialize git. The guard must
/// turn it into an honest "couldn't capture", never "verified nothing".
#[test]
fn a_blind_empty_diff_with_file_changes_is_reported_as_uncaptured_not_absent() {
    let out = verification_honest_diff(String::new(), 3);
    assert!(
        !out.trim().is_empty(),
        "an empty diff with file changes must not stay empty"
    );
    assert!(out.contains("could not be captured"), "{out}");
    assert!(
        out.contains("NOT evidence that nothing changed"),
        "the marker must foreclose the 'no changes' reading: {out}"
    );
    assert!(out.contains('3'), "it names the file-change count: {out}");
}

/// A genuinely empty diff — no file-change events — is the truth and stays
/// empty. The guard must not invent changes that did not happen.
#[test]
fn a_truly_empty_diff_with_no_file_changes_stays_empty() {
    assert_eq!(verification_honest_diff(String::new(), 0), "");
    assert_eq!(verification_honest_diff("   \n".to_string(), 0), "   \n");
}

/// A real diff is passed through untouched, regardless of the count.
#[test]
fn a_real_diff_passes_through_unchanged() {
    let diff = "@@ -1 +1 @@\n-a\n+b".to_string();
    assert_eq!(verification_honest_diff(diff.clone(), 0), diff);
    assert_eq!(verification_honest_diff(diff.clone(), 5), diff);
}
