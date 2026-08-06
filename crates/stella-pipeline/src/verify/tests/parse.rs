//! Verdict parsing: reading a verifier model's prose back as PASS or FAIL.
//!
//! Its own module because the parent had reached the 1500-line ratchet exactly,
//! and because these cases share a subject the rest of the file does not: every
//! one of them is a *string*, written the way a model actually writes it. The
//! ladder tests next door reason about structured evidence, where a bug picks
//! the wrong rung. These reason about English, where a bug inverts a verdict —
//! so they are worth keeping together and worth reading as a set.

use crate::verify::{MAX_VERDICT_REASONING_CHARS, parse_verifier_response};

#[test]
fn parses_pass_and_fail_verdicts() {
    assert_eq!(
        parse_verifier_response("PASS — looks correct").map(|v| v.passed),
        Some(true)
    );
    assert_eq!(
        parse_verifier_response("FAIL: missing edge case").map(|v| v.passed),
        Some(false)
    );
    assert_eq!(
        parse_verifier_response("Verdict: approved").map(|v| v.passed),
        Some(true)
    );
    // A PASS line whose reasoning contains "no" must not be flipped to
    // FAIL by an over-eager "no" match.
    assert_eq!(
        parse_verifier_response("PASS — no obvious issues").map(|v| v.passed),
        Some(true)
    );
    // Only the first non-empty line is authoritative.
    assert_eq!(
        parse_verifier_response("FAIL\nthe change looks fine otherwise").map(|v| v.passed),
        Some(false)
    );
}

/// A negated verdict token states the OPPOSITE verdict, and the parser used
/// to credit it as written: "The tests do not pass" parsed as a PASS — the
/// most damaging possible misread, converting a stated refusal into the
/// verdict that ends the run. The `yes`/`no` mirror of this bug was found
/// and fixed years earlier (see `parses_pass_and_fail_verdicts`); the
/// `pass`/`fail` half had no negation handling and no test.
#[test]
fn a_negated_pass_is_the_fail_it_states() {
    for line in [
        "The tests do not pass",
        "This does not pass the edge case",
        "Cannot pass this without the missing test",
        "The suite doesn't pass",
        "The change does not currently pass review",
    ] {
        assert_eq!(
            parse_verifier_response(line).map(|v| v.passed),
            Some(false),
            "{line:?} states a refusal and must parse as FAIL"
        );
    }
    // A negated FAIL is skipped, never trusted as an affirmative PASS —
    // "did not fail" is absence of failure, not the protocol's verdict —
    // and with no other token the reply is unparseable (heuristic fallback).
    assert_eq!(parse_verifier_response("The suite did not fail"), None);
    // The negation window is bounded: a negator more than two tokens back
    // does not reach the verdict word.
    assert_eq!(
        parse_verifier_response("Not a single problem here: PASS").map(|v| v.passed),
        Some(true)
    );
    // ...and the bound is exactly two tokens: the first window that shipped
    // reached one token further, inverting this approval lead-in into a FAIL.
    assert_eq!(
        parse_verifier_response("Not a problem. PASS").map(|v| v.passed),
        Some(true),
        "an approval lead-in two tokens past the negator is a genuine PASS"
    );
    // An affirmative token before the negator is untouched.
    assert_eq!(
        parse_verifier_response("PASS — this does not fail any case").map(|v| v.passed),
        Some(true)
    );
}

/// The token window cannot be the whole rule, because it measures the wrong
/// thing. "No issues. Not blocking. PASS" puts the negator ONE token from the
/// verdict — nearer than "cannot currently pass", which must keep negating —
/// so no window wide enough for the second can exclude the first. What
/// separates them is not distance but the sentence between them: a negator
/// binds inside its own clause, and terminal punctuation ends its reach.
///
/// The direction matters. This is an approval inverted into a refusal, which
/// sends a finished, correct candidate back for revision and spends a whole
/// turn's budget denying its own result.
#[test]
fn a_negator_does_not_reach_past_the_end_of_its_clause() {
    for line in [
        // The window alone reads every one of these as a refusal.
        "No issues. Not blocking. PASS",
        "Not blocking; PASS",
        "Nothing is wrong here. Not one thing! PASS",
    ] {
        assert_eq!(
            parse_verifier_response(line).map(|v| v.passed),
            Some(true),
            "{line:?} approves in a later clause and must parse as PASS"
        );
    }
    // The boundary narrows the negator's scope, never its meaning: a refusal
    // stated after one still reads as the FAIL it states.
    for line in [
        "The diff is small. It does not pass.",
        "I read the whole change; it cannot pass without the missing test.",
    ] {
        assert_eq!(
            parse_verifier_response(line).map(|v| v.passed),
            Some(false),
            "{line:?} refuses in its final clause and must parse as FAIL"
        );
    }
    // And an unpunctuated negation still has only the window to stop it, so
    // the window is still doing its job inside the clause.
    assert_eq!(
        parse_verifier_response("The tests do not pass").map(|v| v.passed),
        Some(false)
    );
    assert_eq!(
        parse_verifier_response("Not a problem PASS").map(|v| v.passed),
        Some(true)
    );
}

#[test]
fn unparseable_verifier_response_is_none() {
    assert_eq!(parse_verifier_response("hmm, hard to say"), None);
    assert_eq!(parse_verifier_response(""), None);
}

/// **Witness (#1787).** A 100 KB verifier reply does not become a 100 KB
/// revision prompt.
///
/// `reasoning` is the verifier's whole reply and it travels — into the
/// worker's next turn and into the verdict cache. Unbounded, a reasoning model
/// that thinks out loud pays that size again on every revision round, at the
/// full input rate, and leaves it in a cache entry.
#[test]
fn a_runaway_verifier_reply_is_clipped_before_it_reaches_the_worker() {
    let runaway = format!("FAIL — {}", "the tests are red. ".repeat(6_000));
    assert!(
        runaway.len() > 100_000,
        "the fixture must actually be oversized: {}",
        runaway.len()
    );

    let verdict = parse_verifier_response(&runaway).expect("a FAIL token is present");
    assert!(!verdict.passed);
    assert!(
        verdict.reasoning.chars().count() < MAX_VERDICT_REASONING_CHARS + 100,
        "reasoning survived at {} chars",
        verdict.reasoning.chars().count()
    );
    assert!(
        verdict.reasoning.contains("clipped"),
        "a clip must say so — a silently truncated verdict reads as a verifier \
         that simply stopped talking: {}",
        &verdict.reasoning[..200.min(verdict.reasoning.len())]
    );
    // The head is kept: the verdict token and its first reasons are what a
    // revision acts on.
    assert!(verdict.reasoning.starts_with("FAIL"));
}

/// An ordinary reply is untouched — the bound must be invisible in the case
/// that matters, or it would be quietly editing every verdict.
#[test]
fn an_ordinary_verdict_passes_through_the_bound_unchanged() {
    let verdict = parse_verifier_response("PASS — the witness flipped and the diff is scoped")
        .expect("a PASS token is present");
    assert!(verdict.passed);
    assert_eq!(
        verdict.reasoning,
        "PASS — the witness flipped and the diff is scoped"
    );
}

/// Clipping happens on a character boundary, not a byte one.
///
/// The reply is model output — runtime data — so a verifier answering in a
/// language of multi-byte characters must not panic the pipeline (invariant 5).
/// A `&text[..N]` byte slice would do exactly that.
#[test]
fn clipping_a_multibyte_reply_does_not_panic() {
    let runaway = format!("FAIL — {}", "これはテストです。".repeat(3_000));
    let verdict = parse_verifier_response(&runaway).expect("a FAIL token is present");
    assert!(verdict.reasoning.chars().count() < MAX_VERDICT_REASONING_CHARS + 100);
    assert!(verdict.reasoning.contains("clipped"));
}
