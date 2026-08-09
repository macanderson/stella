//! The encoding's two obligations: the composed text must carry the trigger
//! into the embedded body, and the body must come back out **exactly**, because
//! the restatement band compares what comes back out.
//!
//! The round trip is stated as a property rather than a table because the
//! adversary is real: `trigger` is model prose, and the one input that can
//! break a "split at the separator" decoder is a trigger that contains the
//! separator. A table of hand-picked strings is a table of cases the author
//! thought of.

use proptest::prelude::*;

use super::{lesson_body, recall_text};

#[test]
fn a_lesson_with_no_trigger_is_stored_exactly_as_it_always_was() {
    let body = "the pre-push gate exports GIT_DIR to every hook it runs";
    assert_eq!(
        recall_text(body, ""),
        body,
        "a lesson mined before `trigger` existed, or by a model that declined \
         to state a condition, must reach the store byte-for-byte unchanged — \
         otherwise this change rewrites the corpus the restatement band was \
         tuned on, which is the one thing #2459 exists to avoid"
    );
    assert_eq!(recall_text(body, "   \n  "), body, "whitespace is not a trigger");
}

#[test]
fn the_trigger_lands_in_the_text_that_gets_embedded() {
    let composed = recall_text(
        "the witness stage refuses a diff it has not read",
        "a task edits the witness stage",
    );
    assert_eq!(
        composed,
        "the witness stage refuses a diff it has not read (applies when: a \
         task edits the witness stage)",
        "the whole point is that the trigger is part of `content`: \
         `ContextStore::upsert` keys a vector by sha256(content), so a field \
         outside content has no vector and cannot be recalled on"
    );
}

#[test]
fn the_body_comes_back_out_and_the_band_sees_what_it_always_saw() {
    let body = "commands are registered in registry.py";
    let composed = recall_text(body, "a task adds or renames a CLI command");
    assert_eq!(
        lesson_body(&composed),
        body,
        "`partition_known` compares this string against the next candidate \
         lesson's body; if the trigger leaked in, the union would grow on one \
         side only and every paraphrase would read as novel"
    );
}

#[test]
fn a_body_that_looks_composed_does_not_confuse_its_own_trigger() {
    // A lesson *about* this encoding is the realistic way a body acquires the
    // separator. Splitting at the first occurrence would return "docs say" and
    // silently shorten what the band compares.
    let body = "docs say (applies when: x) is the stored form";
    let composed = recall_text(body, "a task touches memory/learning");
    assert_eq!(
        lesson_body(&composed),
        body,
        "the decoder splits at the LAST separator, and `recall_text` escapes \
         the trigger so no later one can exist"
    );
}

#[test]
fn a_trigger_that_spells_the_separator_is_escaped_not_trusted() {
    let composed = recall_text("a fact", "x (applies when: y");
    assert_eq!(
        composed, "a fact (applies when: x applies when: y)",
        "an unescaped trigger would give the composed string a second split \
         point after the one we appended, and the round trip below would stop \
         being total"
    );
    assert_eq!(lesson_body(&composed), "a fact");
}

/// The counterexample the property found, kept by name because the regression
/// file that also holds it is a hash.
///
/// A trigger of `" (applies when: …"` **trims** to `"(applies when: …"`, which
/// contains no separator to escape — and then lands immediately after the
/// separator's own trailing space, synthesizing a second separator across the
/// join. `lesson_body` split at that one and returned ` (applies when:` as the
/// lesson. Escaping the paren-first form is what closes it: the trigger cannot
/// borrow our space to complete a match.
#[test]
fn a_trigger_that_only_becomes_a_separator_after_trimming_is_still_escaped() {
    let composed = recall_text("", " (applies when: x");
    assert_eq!(lesson_body(&composed), "");
    assert_eq!(composed, " (applies when: applies when: x)");
}

#[test]
fn an_uncomposed_memory_is_returned_whole() {
    for stored in [
        "money is stored in integer minor units",
        "the deck redraws on resize (see deck_ui.rs)",
        "",
    ] {
        assert_eq!(
            lesson_body(stored),
            stored,
            "every memory written before this change must read back \
             unchanged, or the band's stored side moves for the whole \
             existing corpus"
        );
    }
}

proptest! {
    /// **The obligation.** For any body and any non-empty trigger, the body
    /// survives the round trip byte-for-byte.
    ///
    /// This is what discharges #2459's "re-measure the restatement band"
    /// requirement without a benchmark: the band's input is not approximately
    /// preserved, it is *identical*, so there is no distribution left to
    /// measure. The trigger is generated adversarially — it may contain the
    /// separator, parens, and newlines — because it is model output and
    /// nothing constrains it.
    #[test]
    fn recomposing_any_body_and_trigger_recovers_the_body(
        body in ".{0,120}",
        trigger in "[^ ]{0,20}( \\(applies when: |[a-z )(]){0,10}[^ ]{0,20}",
    ) {
        prop_assume!(!trigger.trim().is_empty());
        let composed = recall_text(&body, &trigger);
        prop_assert_eq!(
            lesson_body(&composed),
            body.as_str(),
            "the decoder must be the exact inverse of the encoder for every \
             trigger a model can emit — the restatement band's correctness \
             rests on it"
        );
    }

    /// Composition adds the trigger and changes nothing else: the composed
    /// text always starts with the body it was built from.
    #[test]
    fn composition_only_ever_appends(
        body in ".{0,120}",
        trigger in ".{0,80}",
    ) {
        let composed = recall_text(&body, &trigger);
        prop_assert!(
            composed.starts_with(&body),
            "a lesson's own words must reach the embedding unmodified; \
             rewriting them would move the body's contribution to every \
             similarity score in the store"
        );
    }
}
