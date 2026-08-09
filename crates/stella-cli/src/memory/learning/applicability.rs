//! Where a lesson's trigger participates in retrieval, and how the restatement
//! band stays out of its way (#2459).
//!
//! A [`ReflectionLesson`](crate::memory::ReflectionLesson) arrives with a
//! `trigger` — the condition a future task has to satisfy for the lesson to be
//! worth reading. That field is written in the register of a *goal* ("a task
//! that touches the witness stage"), while the lesson body is written in the
//! register of a *fact* ("the witness stage refuses a diff it has not read").
//! Recall scores a memory by embedding its body and comparing it to the goal,
//! so the trigger is the half of a lesson that lives in the space the query is
//! asked in, and the body is the half that does not.
//!
//! ## Why the trigger is folded into the body
//!
//! Embeddings in `stella-context` are **content-addressed**: a vector is keyed
//! `(sha256(content), embedder fingerprint)`, which is what makes the
//! byte-compat skip in `L-C2` sound (`ContextStore::upsert`). A field that is
//! not part of `content` therefore has no vector, and the lexical pass
//! (`candidates::scan_terms`) reads `content` too. Folding is the only way the
//! trigger reaches retrieval without a schema migration, a second embedding per
//! memory, and a fusion weight nothing has data to tune.
//!
//! The two alternatives #2459 lists were both weighed and are worse here:
//!
//! * a **separate scored field** buys the ability to weight trigger-match
//!   against body-match independently — a knob with no measurement behind it —
//!   at the cost of a `stella-context` schema change, a second vector per
//!   reflection memory, and a new join for `NodeMeta` to carry;
//! * a **post-retrieval filter** turns a prose guess into a hard drop. Its
//!   failure mode is silently withholding a memory that was correct, which is
//!   strictly worse than today, where the memory at least competes.
//!
//! ## Why the restatement band does not move
//!
//! `SessionMemory::partition_known` decides whether a candidate lesson is novel
//! or a paraphrase of one already held, by Jaccard over token sets at 0.5
//! (`stella_store::is_suppressed`). It compares the candidate's body against
//! each stored node's `content`. Folding the trigger into `content` without
//! this module would make that comparison **asymmetric** — extra tokens on the
//! stored side only, inflating the union and depressing every score at once, so
//! genuine paraphrases would read as novel and flood the store.
//!
//! Composing *both* sides is not the repair; it is a worse break. Take a
//! five-token body stored and restated verbatim, under two six-token triggers
//! sharing two tokens: the pair scores 1.0 on bodies alone and
//! `(5+2)/(5+10) = 0.47` composed — an exact duplicate fact, classified novel.
//! Trigger verbosity would decide a question about facts.
//!
//! So the band keeps comparing **bodies to bodies**, and this module is what
//! lets it: `recall_text` composes on the way into the store and
//! `lesson_body` recovers the body on the way out. They are exact inverses
//! for every lesson that has a trigger (`applicability/tests.rs` pins the round
//! trip as a property over arbitrary bodies and triggers), so the strings
//! `partition_known` compares are byte-identical to the ones it compared before
//! this change — which is why #2459's re-measurement is discharged by
//! construction rather than by a benchmark. A benchmark could only have
//! reported the same equality with less confidence, and only for the corpus it
//! happened to run on.
//!
//! The deliberate consequence: two lessons stating one fact under two different
//! conditions collapse to one memory. That is right. Recall would otherwise
//! spend two of five frames restating a single fact, and *which* fact it is has
//! not changed — only when it was noticed.

/// The separator that carries a lesson's trigger inside its stored body.
///
/// Chosen to read as ordinary prose in the recalled-context block, where a
/// frame renders as `- [nod_…] label — content` on one line: a memory reads
/// `the witness stage refuses a blind diff (applies when: a task edits the
/// witness stage)`. A bracketed form would collide visually with the `[nod_…]`
/// citation ids the same line carries.
const APPLIES_WHEN: &str = " (applies when: ";

/// [`APPLIES_WHEN`] without its leading space — the substring a trigger must
/// not contain.
///
/// The leading space is deliberately excluded, and the property test is what
/// found out why. [`recall_text`] trims the trigger, so a trigger of
/// `" (applies when: x"` trims to `"(applies when: x"`, which contains no
/// [`APPLIES_WHEN`] to escape — and then lands directly after the separator's
/// own trailing space, *synthesizing* a second separator across the join.
/// Escaping the paren-first form cannot be dodged that way: no space of ours
/// can complete a match the trigger no longer contains.
const APPLIES_WHEN_CORE: &str = "(applies when: ";

/// What [`APPLIES_WHEN_CORE`] is rewritten to inside a trigger.
///
/// Dropping the paren is enough to break the match, and keeps the trigger
/// readable rather than mangling it. See [`recall_text`] for why the
/// substitution has to happen at all.
const APPLIES_WHEN_ESCAPED: &str = "applies when: ";

/// The text a lesson is stored and embedded as: the body, plus its trigger when
/// it has one.
///
/// An empty trigger composes to the body alone, so a lesson mined before the
/// field existed — or by a model that declined to state a condition — is stored
/// exactly as it always was. That is not a fallback: `trigger` carries
/// `#[serde(default, skip_serializing_if = "String::is_empty")]` precisely so
/// the empty case is a first-class one.
///
/// The trigger is escaped rather than trusted. It is model prose, and a trigger
/// that itself contained the separator would give the composed string two
/// candidate split points — at which the round trip [`lesson_body`] guarantees
/// stops being total. Rewriting the one substring that can do that makes the
/// inverse exact for *every* pair of inputs, which is what
/// `recomposing_any_body_and_trigger_recovers_the_body` asserts.
pub(super) fn recall_text(lesson: &str, trigger: &str) -> String {
    let trigger = trigger.trim();
    if trigger.is_empty() {
        return lesson.to_string();
    }
    let trigger = trigger.replace(APPLIES_WHEN_CORE, APPLIES_WHEN_ESCAPED);
    format!("{lesson}{APPLIES_WHEN}{trigger})")
}

/// The lesson body inside a stored memory — the exact inverse of
/// [`recall_text`], and the string the restatement band compares.
///
/// Splits at the **last** separator, not the first. A body may legitimately
/// contain the phrase (a lesson *about* this encoding, for one), and only the
/// separator this module appended is guaranteed to be last, because
/// [`recall_text`] escapes the trigger so it can contribute no later one.
///
/// A memory that does not end in `)` was never composed, and is returned whole.
/// That leaves one benign misread: a memory written before this change whose
/// own text happens to end in `)` *and* contain the separator would be trimmed
/// here. The consequence is bounded to this comparison — a slightly shorter
/// string on one side of a similarity score — and nothing is rewritten in the
/// store, so the alternative (a marker byte in the content) would cost every
/// reader a stripping step to avoid a case that has never occurred.
pub(super) fn lesson_body(stored: &str) -> &str {
    let Some(inner) = stored.strip_suffix(')') else {
        return stored;
    };
    match inner.rfind(APPLIES_WHEN) {
        Some(at) => &stored[..at],
        None => stored,
    }
}

#[cfg(test)]
mod tests;
