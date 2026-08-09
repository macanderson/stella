//! What the reviewer prompts say the diff is measured *against*, and what the
//! authored section is a claim *about*.
//!
//! Its own file rather than a block in `verify/tests.rs`: that file sits one
//! test away from the 1500-line limit, and a new god file fails the gate
//! outright rather than being grandfathered.

use super::super::*;

/// Both reviewer prompts must say what the diff is measured against.
///
/// A verifier that assumes git's default — working tree against `HEAD` — reads
/// every hunk as an uncommitted edit. Stella diffs against the commit the
/// SESSION started on (`GitDiagnosticRunner::baseline`), so staged, unstaged
/// and committed work all appear. On Terminal-Bench `fix-git` a reviewer called
/// a correctly committed merge "uncommitted edits" and failed it.
///
/// The guidance reviewer needs the same note and is easy to forget: it is the
/// one that issues course-correction, so its misreading is the one the worker
/// acts on.
#[test]
fn both_reviewer_prompts_state_what_the_diff_is_measured_against() {
    let verifier = verifier_prompt(
        "recover the lost commit",
        "@@ -1 +1 @@\n-x\n+y",
        "flip_achieved=unobserved",
        &diff_render::DiffContext::default(),
    )
    .rendered();
    let guidance = guidance_prompt(
        "recover the lost commit",
        "@@ -1 +1 @@\n-x\n+y",
        "flip_achieved=unobserved",
        &diff_render::DiffContext::default(),
    )
    .rendered();

    for (name, p) in [("verifier", &verifier), ("guidance", &guidance)] {
        assert!(
            p.contains("commit the session STARTED on"),
            "{name} prompt must name the diff baseline"
        );
        assert!(
            p.contains("not evidence that a change was left uncommitted"),
            "{name} prompt must forbid the uncommitted-edits inference"
        );
        assert!(
            p.contains("not evidence that the agent hand-wrote the content"),
            "{name} prompt must say the authored section is intent, not provenance"
        );
    }
}

/// The note names the authored header by referencing the constant the splicer
/// emits, so the prompt cannot describe a header that no longer exists —
/// the drift this file's parent already suffered three times over its own
/// framing (see `UNTRUSTED_DIFF_HEADING_SUFFIX`).
#[test]
fn the_note_quotes_the_header_the_splicer_actually_writes() {
    let p = verifier_prompt(
        "anything",
        "@@ -1 +1 @@\n-x\n+y",
        "evidence",
        &diff_render::DiffContext::default(),
    )
    .rendered();
    assert!(
        p.contains(crate::pipeline::authored::AUTHORED_SECTION_HEADER),
        "the prompt must quote the real authored-section header"
    );
}
