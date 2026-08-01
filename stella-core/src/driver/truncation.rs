//! What a step that ran out of output tokens without calling a tool does next,
//! and what it leaves behind in the transcript.
//!
//! Completing such a step is the defect that shipped the zero-tool
//! Terminal-Bench trials: the turn ends, verification finds nothing observable,
//! and the run reports a task it never touched (30 of 30 cap-hit steps in the
//! 2026-07-31 bundle carried no tool call). So the turn continues instead — a
//! *continuation with new input* rather than a retry, because at temperature 0
//! the identical request re-truncates identically and the nudge is the only
//! thing that changes the outcome.
//!
//! # Two shapes, told apart by whether any text survived
//!
//! A cut-off step arrives in one of two states, and the difference is the whole
//! reason this module has a policy rather than a constant:
//!
//! - **Text is empty** — the model spent its entire budget deliberating and was
//!   cut off before writing anything. There is nothing to resume *from*; what
//!   was lost is working-out the nudge below already tells the model not to
//!   repeat. History keeps [`REASONING_ONLY_PARTIAL`], one line recording that
//!   a step happened and produced nothing.
//! - **Text is non-empty** — a real answer was cut mid-sentence. That *is* worth
//!   resuming from, so it is retained: verbatim when small, and middle-elided
//!   when large enough for the difference to matter
//!   ([`crate::compaction::elide_truncated_partial`]).
//!
//! On the Chat Completions dialects the first shape used to be invisible. The
//! zai/OpenRouter adapter promoted chain-of-thought into `text` whenever a turn
//! would otherwise be blank, so a truncated reasoning-only step arrived looking
//! like a truncated *answer* — and the whole scratchpad, up to a full
//! `max_output_tokens` of it, was retained as protected context that no
//! compaction pass may touch and every remaining step of the turn re-sent.
//! `zai.rs` now suppresses that promotion for truncated streams, which is what
//! makes `text.is_empty()` a usable signal here.
//!
//! # Why eliding is a better prompt, not just a cheaper one
//!
//! The nudge tells the model to resume from exactly where it stopped and not to
//! restate its reasoning. Retained whole, those two referents are the last
//! handful of tokens of the partial and the ~16k before them — the instruction
//! points at something buried under the noise it is telling the model to
//! ignore. Elided, the resume point sits immediately above the instruction that
//! names it. The saving and the improvement are the same edit.

use std::time::Duration;

use super::CompletionMessage;

/// How many times one turn may continue past a step that ended at the
/// output-token limit having called no tool (`FinishReason::Length`,
/// `tool_calls` empty).
///
/// Four, measured. This was 2, on the argument that the first continuation
/// finishes the thinking and the second lands the action — which held for the
/// shape it was tuned against, where an adapter had promoted chain-of-thought
/// into `text` and part of the reasoning had already landed. It does not
/// transfer to an all-reasoning step that arrives empty, which is the shape
/// the Anthropic dialect produces and which now reaches here.
///
/// On a ten-task adversarial Terminal-Bench 2.1 set at effort `xhigh`,
/// `circuit-fibsqrt` hit the cap three times, spent both continuations and
/// aborted on the third; a one-hit task recovered on its first; and a task
/// that had previously died at the cap passed outright. Two was one short of
/// the worst observed case.
///
/// Four and not more because the allowance is not independent of the agent
/// timeout. A capped step at `xhigh` costs roughly 170–280s of wall clock, so
/// three cap hits is already ~500–840s against a 900s agent timeout: past
/// about four, a larger allowance stops buying recoveries and starts buying
/// timeouts, which fail a harness-health bar exactly the way the abort does.
/// Cap size, this count, and the agent timeout are one budget, not three.
///
/// Each continuation is still a paid call, and a turn that exhausts the
/// allowance falls through to the truncation warning with the verification
/// ladder's no-op rung behind it — a model truncating a fifth time is saying
/// the effort tier is mispriced for the task, not that it needs one more retry.
pub(super) const MAX_LENGTH_CONTINUATIONS: u32 = 4;

/// Prefix of the engine-injected output-limit continuation nudge.
///
/// The third engine-written `User`-role message, and it needs the marker for
/// exactly the two reasons the other two do. `driver::loop_evidence` would
/// otherwise take it as a turn boundary and discard every call the turn had
/// made so far — on the one path where the turn demonstrably *is* still the
/// same turn, so a stuck model that also truncates would have its evidence
/// erased up to twice per turn. And `receipts::user_block_kind` would file it as
/// [`stella_protocol::BlockKind::UserGoal`], attributing engine text to the
/// person, which is the misattribution Phase 2 introduced that function to fix.
pub(crate) const CONTINUATION_MARKER_PREFIX: &str = "[output-limit continuation";

/// The body of the nudge, pushed behind [`CONTINUATION_MARKER_PREFIX`].
///
/// Written for the two shapes the trigger cannot distinguish and does not need
/// to: chain-of-thought that reached `text`, and a genuine prose answer cut off
/// mid-sentence. Both are told the same thing — the turn is not over, go do the
/// work, with tool calls rather than narration.
pub(super) const LENGTH_CONTINUATION_NUDGE: &str = "Your previous message hit the output-token \
     limit and was cut off before any tool call or finished answer. The turn is not over. \
     Continue from exactly where you stopped: if work remains, emit the tool calls that perform \
     it now — writing files, running commands, applying edits. Do not restate your reasoning or \
     repeat text you already produced; describing the work is not doing it.";

/// Stands in history for a step that spent its whole output budget on reasoning
/// and was cut off with no answer and no tool call.
///
/// Keeping the assistant turn — rather than pushing the nudge bare — keeps the
/// transcript's role sequence well-formed and leaves an honest record that the
/// step happened and produced nothing.
pub(super) const REASONING_ONLY_PARTIAL: &str = "[no answer produced: this step's entire output budget went to reasoning and was cut off \
     at the token limit before any answer or tool call — nothing was kept]";

/// The two messages a continuation appends, plus the line the user sees.
///
/// Returned as data rather than pushed here so the decision stays a pure
/// function over owned values: the driver owns the transcript and the event
/// stream, this module owns the policy.
pub(super) struct Continuation {
    /// What the assistant turn records — a marker, an elided partial, or the
    /// partial verbatim.
    pub(super) retained: String,
    /// The `⚠` line streamed to the user. The full partial was already streamed
    /// as its own `Text` event, so only what the *model* re-reads is shrunk.
    pub(super) note: String,
}

impl Continuation {
    /// The user-visible note and the two messages to append, in order — one
    /// method so the caller cannot take one and forget the other.
    pub(super) fn into_parts(self) -> (String, [CompletionMessage; 2]) {
        (
            self.note,
            [
                CompletionMessage {
                    role: stella_protocol::MessageRole::Assistant,
                    content: self.retained,
                    tool_calls: Vec::new(),
                    tool_results: Vec::new(),
                    attachments: Vec::new(),
                },
                CompletionMessage::user(format!(
                    "{CONTINUATION_MARKER_PREFIX}] {LENGTH_CONTINUATION_NUDGE}"
                )),
            ],
        )
    }
}

/// Decide what a length-truncated, tool-less step continues with, or `None`
/// when the turn's continuation allowance is spent and the caller should fall
/// through to its terminal handling.
///
/// `spent` is the turn's running count and is incremented by the caller on
/// `Some` — threaded rather than owned so the bound survives across steps.
/// Callers establish `FinishReason::Length` and an empty `tool_calls` before
/// calling; this decides only what happens next.
/// What one turn has left to spend on continuing, in wall clock.
///
/// The count alone cannot see the constraint that actually binds. A harness
/// kills a trial on elapsed time — Terminal-Bench at 900s — and the engine
/// continues blind to it, so a turn can spend its whole allowance and be
/// destroyed externally mid-continuation. That arrives as
/// `AgentTimeoutError`: a harness death, indistinguishable at the results
/// layer from the agent failing the task, where an honest truncated answer
/// would have been an ordinary result.
///
/// Measured on a ten-task adversarial Terminal-Bench set at effort `xhigh`:
/// four of ten trials died on the wall clock, two of them without ever
/// reaching the output cap. No continuation allowance can fix that, because
/// more retries is the opposite of the fix.
#[derive(Debug, Clone, Copy)]
pub(super) struct ContinuationBudget {
    /// Wall clock left before the turn's deadline.
    pub(super) remaining: Duration,
    /// How long the step that just truncated took.
    pub(super) last_step: Duration,
}

impl ContinuationBudget {
    /// Whether there is time to finish a continuation, not merely start one.
    ///
    /// A continuation re-runs the work that just truncated — same prompt shape,
    /// same reasoning, same cap — so it costs at least what its predecessor
    /// cost. Starting one with less time than that is choosing an external kill
    /// over a truthful partial: the answer is thrown away and the trial reports
    /// a timeout instead of a result.
    ///
    /// Strictly greater, and no safety margin invented here: the caller owns
    /// the deadline and can set a conservative one. A margin baked in at this
    /// level would be a second, invisible policy.
    fn affords_another(&self) -> bool {
        self.remaining > self.last_step
    }
}

pub(super) fn plan_continuation(
    text: &str,
    output_tokens: u64,
    spent: u32,
    budget: Option<ContinuationBudget>,
) -> Option<Continuation> {
    if spent >= MAX_LENGTH_CONTINUATIONS {
        return None;
    }
    // Checked after the count, so a turn with no deadline configured behaves
    // exactly as before — the budget is opt-in and absent by default.
    if let Some(budget) = budget
        && !budget.affords_another()
    {
        return None;
    }
    let attempt = spent + 1;
    if text.trim().is_empty() {
        return Some(Continuation {
            retained: REASONING_ONLY_PARTIAL.to_string(),
            note: format!(
                "\n\n⚠ Output-token limit reached ({output_tokens} tokens) with the budget spent \
                 entirely on reasoning — no answer and no tool call; continuing \
                 ({attempt}/{MAX_LENGTH_CONTINUATIONS})."
            ),
        });
    }
    Some(Continuation {
        retained: crate::compaction::elide_truncated_partial(text)
            .unwrap_or_else(|| text.to_string()),
        note: format!(
            "\n\n⚠ Output-token limit reached ({output_tokens} tokens) before any tool call; \
             continuing ({attempt}/{MAX_LENGTH_CONTINUATIONS})."
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A distinctive fragment of the marker `elide_truncated_partial` splices
    /// in, so this module can assert the model was told bytes went missing
    /// without owning a second copy of the whole string.
    const PARTIAL_ELISION_SIGNPOST: &str = "middle of this cut-off message elided";

    #[test]
    fn a_spent_allowance_declines_to_continue() {
        assert!(plan_continuation("cut off", 16_384, MAX_LENGTH_CONTINUATIONS, None).is_none());
        assert!(plan_continuation("", 16_384, MAX_LENGTH_CONTINUATIONS, None).is_none());
        // ...and every attempt below it does continue.
        for spent in 0..MAX_LENGTH_CONTINUATIONS {
            assert!(plan_continuation("cut off", 16_384, spent, None).is_some());
        }
    }

    #[test]
    fn a_continuation_it_cannot_finish_is_declined() {
        // The continuation costs what its predecessor cost, so 60s of work with
        // 30s left is a continuation that gets killed mid-flight. Declining
        // keeps the truthful partial and lets the turn end as an ordinary
        // result; taking it trades that for an external timeout, which reads at
        // the results layer exactly like the agent failing the task.
        let broke = ContinuationBudget {
            remaining: Duration::from_secs(30),
            last_step: Duration::from_secs(60),
        };
        assert!(plan_continuation("cut off", 16_384, 0, Some(broke)).is_none());
        assert!(plan_continuation("", 16_384, 0, Some(broke)).is_none());
    }

    #[test]
    fn a_continuation_it_can_finish_proceeds() {
        let flush = ContinuationBudget {
            remaining: Duration::from_secs(600),
            last_step: Duration::from_secs(60),
        };
        assert!(plan_continuation("cut off", 16_384, 0, Some(flush)).is_some());
    }

    #[test]
    fn exactly_enough_time_is_not_enough() {
        // Equal is refused, not allowed. The forecast is an estimate drawn from
        // one prior sample, so spending the last of the budget on it lands
        // exactly on the deadline in the best case and past it in every other.
        let exact = ContinuationBudget {
            remaining: Duration::from_secs(60),
            last_step: Duration::from_secs(60),
        };
        assert!(plan_continuation("cut off", 16_384, 0, Some(exact)).is_none());
    }

    #[test]
    fn without_a_budget_the_allowance_is_the_only_bound() {
        // The default path: no deadline configured, so behaviour is the pure
        // count it was before — a caller that knows nothing about its wall
        // clock must not have work declined on a guess.
        for spent in 0..MAX_LENGTH_CONTINUATIONS {
            assert!(plan_continuation("cut off", 16_384, spent, None).is_some());
        }
    }

    #[test]
    fn a_reasoning_only_step_keeps_a_marker_not_a_scratchpad() {
        // The shape the zai adapter used to hide by promoting reasoning into
        // `text`: no answer at all. There is nothing to resume from, so the
        // transcript records the fact and none of the deliberation.
        let plan = plan_continuation("   \n  ", 16_384, 0, None).expect("continues");
        assert_eq!(plan.retained, REASONING_ONLY_PARTIAL);
        assert!(
            plan.note.contains("spent entirely on reasoning"),
            "the note must name why nothing was kept: {}",
            plan.note
        );
    }

    #[test]
    fn a_short_partial_survives_verbatim() {
        // The guard that keeps this honest: a genuinely cut-off short answer is
        // never trimmed, so eliding can never lose an answer small enough to
        // have been worth reading whole.
        let answer = "The fix is to widen the guard in ";
        let plan = plan_continuation(answer, 16_384, 0, None).expect("continues");
        assert_eq!(plan.retained, answer);
    }

    #[test]
    fn a_long_partial_keeps_its_end_which_is_where_it_was_cut() {
        // Head for orientation, tail for the resume point, middle gone — and
        // the tail gets the larger share, because that is the only part the
        // continuation actually resumes from.
        let head = "FIRST-BYTES-MARKER ";
        let tail = " and therefore the next step is to edit src/lib.rs at line";
        // Distinguishable thirds, so "the middle is gone" is actually testable —
        // homogeneous filler would let tail bytes satisfy a middle assertion.
        let partial = format!(
            "{head}{}MIDDLE-ONLY-MARKER{}{tail}",
            "a".repeat(5_000),
            "b".repeat(5_000)
        );
        let plan = plan_continuation(&partial, 16_384, 0, None).expect("continues");

        assert!(
            plan.retained.len() < partial.len() / 4,
            "a 10k partial must shrink by much more than a quarter: {} of {}",
            plan.retained.len(),
            partial.len()
        );
        assert!(
            plan.retained.starts_with(head),
            "the head is kept for orientation: {:?}",
            &plan.retained[..plan.retained.len().min(40)]
        );
        assert!(
            plan.retained.ends_with(tail),
            "the resume point — the very end — must survive intact"
        );
        assert!(
            !plan.retained.contains("MIDDLE-ONLY-MARKER"),
            "the middle must actually be gone"
        );
        assert!(
            plan.retained.contains(PARTIAL_ELISION_SIGNPOST),
            "and the model must be told something was dropped: {}",
            plan.retained
        );
        // Tail-weighted, not symmetric: the resume point gets the larger share.
        let kept_b = plan.retained.matches('b').count();
        let kept_a = plan.retained.matches('a').count();
        assert!(
            kept_b > kept_a * 4,
            "the end must be kept far more generously than the start: {kept_a} vs {kept_b}"
        );
    }

    #[test]
    fn the_nudge_is_marked_so_it_is_never_mistaken_for_the_user() {
        let (_note, [assistant, nudge]) = plan_continuation("cut off", 16_384, 0, None)
            .expect("continues")
            .into_parts();
        assert_eq!(assistant.role, stella_protocol::MessageRole::Assistant);
        assert_eq!(nudge.role, stella_protocol::MessageRole::User);
        assert!(
            nudge.content.starts_with(CONTINUATION_MARKER_PREFIX),
            "the marker is what keeps this out of the loop window and out of \
             the user's own block kind: {}",
            nudge.content
        );
    }
}
