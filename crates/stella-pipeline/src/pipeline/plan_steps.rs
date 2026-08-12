//! Plan-step prompts and the worker's early close-out (#1702).
//!
//! `Pipeline::execute_plan` used to walk every plan step unconditionally: a
//! worker that finished the whole task in step 1 — the normal outcome when a
//! plan's steps are one shell script's worth of work — was then marched
//! through the remaining steps re-confirming its own finished work, at two
//! model calls per step over a transcript that grows with every one. On a
//! measured Terminal-Bench-shaped run, 63% of the run's cost bought no work.
//!
//! Two things fix that here, both pure text so the loop in `pipeline.rs`
//! stays a loop:
//!
//! - [`step_prompt`] tells the worker the truth about the plan: earlier steps
//!   may already have covered this one, confirming finished work is a no-op
//!   to report in one line, and step descriptions are sequencing hints rather
//!   than specs (the `e.g.` in a planner's step is illustration, and workers
//!   were observed renaming working identifiers to match it).
//! - [`goal_declared_complete`] is the close-out channel the prompt offers: a
//!   reply that opens or closes with an affirmative [`PLAN_COMPLETE_MARKER`]
//!   line ends the step loop.
//! - [`outstanding_work_prompt`] is #2933's narrower close-out: a step
//!   answered with no tool calls at all almost always means the rest are
//!   covered too, and the walk asks about all of them in one further turn
//!   instead of one per remaining step.
//!
//! **The close-out is screened here, not downstream, because the backstop this
//! module originally leaned on does not always exist.** The first version
//! reasoned that a false claim is refuted by the verify stage, which runs on
//! whatever the tree actually holds. That holds only for work that lands *in
//! the workspace tree*. A task whose subject is `/etc`, `/git` or a system
//! service leaves the tree unchanged, so the diff probe finds nothing, verify
//! returns `UNVERIFIABLE`, and the verdict passes — which is how a single
//! negated echo of the marker skipped nine of ten steps for reward 0.0
//! (#2104). Most of Terminal-Bench is that shape, so the screen is the
//! backstop.

use super::*;
use crate::plan::{names_an_absolute_path, parse_plan, plan_path_repair_prompt};

impl<'a> Pipeline<'a> {
    /// One bounded repair retry (L-V2) for a plan that parsed cleanly but
    /// named an absolute filesystem path — a blind planner cannot know a
    /// path it never saw is real, and #2932 measured 32% of plan steps in
    /// one benchmark run naming `/app`, most refused outright by the
    /// candidate sandbox. Degrades to the original plan on any repair
    /// failure: a plan naming a path the worker has to correct is still
    /// better than no plan.
    pub(super) async fn resolve_plan_paths(
        &self,
        steps: Vec<PlanStep>,
        resolved: &ResolvedRole<'a>,
        overrides: &RoleCallOverrides,
        spend: &mut Spend<'_>,
    ) -> Result<Vec<PlanStep>, PipelineStageAbort> {
        if !steps.iter().any(|s| names_an_absolute_path(&s.description)) {
            return Ok(steps);
        }
        match self
            .metered_raw_call(
                RawCall {
                    role: ModelCallRole::PlanRepair,
                    resolved,
                    messages: plan_path_repair_prompt(&steps).into_messages(),
                    policy: RetryPolicy::deterministic(),
                    overrides,
                    timeout: self.config.engine.model_timeout,
                },
                spend.budget,
                spend.total,
            )
            .await
        {
            Ok(repair) => Ok(parse_plan(&repair.text).unwrap_or(steps)),
            Err(RawCallError::Budget(abort) | RawCallError::Deadline(abort)) => Err(abort),
            Err(RawCallError::Provider | RawCallError::Timeout) => Ok(steps),
        }
    }
}

/// The line a worker opens with to declare the whole goal finished and end
/// the step loop. Matched at line start (after leading whitespace) so prose
/// that merely mentions the phrase mid-sentence does not close the plan.
pub(super) const PLAN_COMPLETE_MARKER: &str = "PLAN COMPLETE";

/// The user message driving one plan step: the step itself, then the
/// standing truths of #1702 — already-done work is reported, not redone;
/// descriptions are hints, not specs; and a finished goal is declared with
/// [`PLAN_COMPLETE_MARKER`] instead of walked to the end.
pub(super) fn step_prompt(index: usize, total: usize, description: &str) -> String {
    format!(
        "Step {}/{}: {}\n\n\
         Earlier steps may already have covered this one. If this step is \
         already done, say so in one line and make no tool calls — do not \
         re-verify finished work. Step descriptions are sequencing hints, not \
         specs: names and identifiers in them (especially after \"e.g.\") are \
         illustrative, so do not rename working code to match one. If the \
         ENTIRE goal is already complete, reply with a single line beginning \
         `{PLAN_COMPLETE_MARKER}:` and the remaining steps will be skipped.",
        index + 1,
        total,
        description,
    )
}

/// The one consolidated follow-up sent instead of walking the rest of the
/// plan one step per turn (#2933).
///
/// [`step_prompt`] already instructs a worker that finished a step in an
/// earlier turn to answer in one line with no tool calls — the correct
/// behavior for *that* step. The loop's fault was upstream of the prompt: a
/// worker that closed out step 1 with no tool calls almost always covered
/// the whole goal there too, and the loop re-asked every remaining step
/// individually anyway, paying one full model call per step for a reply
/// that was `Already done` every time (measured: 52 such calls in one
/// 20-trial arm, 0 in the arm with no plan stage). This prompt asks about
/// every remaining step at once, so the loop pays for at most one more
/// call instead of one per step.
pub(super) fn outstanding_work_prompt(remaining: &[PlanStep]) -> String {
    let mut listed = String::new();
    for (i, step) in remaining.iter().enumerate() {
        listed.push_str(&format!("{}. {}\n", i + 1, step.description));
    }
    format!(
        "The previous step needed no tool calls, which usually means the remaining plan steps \
         below are already covered too. This is the last check before the walk ends: if every \
         one of them is genuinely already done, say so in one line and make no tool calls. If \
         any of them are not actually done, do that work now — there will be no further \
         per-step turn to catch it.\n\n{listed}"
    )
}

/// Words that turn the declaration into its own denial (#2104).
///
/// Compared against the payload's first word — the run of letters, digits and
/// [`APOSTROPHES`] that other punctuation *terminates* — with case folded,
/// never as a prefix: `nothing left to do` is a genuine completion and must
/// not be read as `not`.
const NEGATION_OPENERS: &[&str] = &[
    "no",
    "not",
    "nope",
    "negative",
    "never",
    "false",
    "incomplete",
    "unfinished",
    "partially",
    "partial",
];

/// The code points a model actually types inside a contraction.
///
/// One list, because assuming ASCII here is how the screen below has failed
/// twice: `no—steps` slipped through when an em dash was treated as part of
/// the word, and `isn’t` slipped through when `U+2019` — which a model emits
/// as readily as `'`, and which appears in the very reply that motivated this
/// screen — terminated the word early and left the harmless `isn`. Every
/// apostrophe folds to `'` before the contraction test, so the test itself
/// stays one comparison.
const APOSTROPHES: &[char] = &['\'', '\u{2019}', '\u{02BC}'];

/// Whether a completed step turn declared the whole goal finished.
///
/// A declaration must **open or close the reply** and must not deny itself.
/// Both halves are load-bearing, and both come from #2104: on Terminal-Bench
/// `git-multibranch` a worker answering step 1 of 10 replied
///
/// ```text
/// All required packages are already installed.
///
/// PLAN COMPLETE: no — this is just step 1; only confirming this step is done.
///
/// Step 1/10 is already complete: ...
/// ```
///
/// and the old predicate — any line opening with the marker — closed the plan
/// and skipped steps 2–10, the entire actual task, for reward 0.0.
///
/// The position rule keeps #1702's "short report, then declare" allowance
/// exactly (the declaration is then the last significant line) while rejecting
/// a marker buried mid-reply with argument still running after it. The
/// negation rule catches the same denial when it *does* land last.
///
/// Still deliberately strict past that — no case folding of the marker, no
/// mid-line match. The asymmetry the original weighed has since been measured
/// and runs the other way: a missed declaration costs the redundant walk this
/// module exists to avoid, while a spurious match cost a whole trial's reward,
/// because the documented backstop ("verify runs on whatever the tree holds")
/// does not hold for tasks that mutate `/etc`, `/git` or system services —
/// there the diff probe sees an unchanged tree and returns `UNVERIFIABLE`.
pub(super) fn goal_declared_complete(text: &str) -> bool {
    let mut significant = text.lines().filter(|line| !line.trim().is_empty());
    let Some(first) = significant.next() else {
        return false;
    };
    let last = significant.next_back().unwrap_or(first);
    declares_completion(first) || declares_completion(last)
}

/// Whether one line is an affirmative close-out declaration.
///
/// The marker is a **token**, not a prefix. A letter fused to it shifts the
/// payload past the denial the polarity screen exists to read: on
/// `PLAN COMPLETED: no, steps 2-10 remain` the payload is `D: no, …`, whose
/// first word is `d`, so the whole axis is void and the denial closes the
/// plan. Requiring a non-alphanumeric (or nothing) after the marker costs at
/// most the redundant walk this module exists to avoid, which is the cheap
/// side of the asymmetry [`goal_declared_complete`] documents.
fn declares_completion(line: &str) -> bool {
    let Some(payload) = line.trim_start().strip_prefix(PLAN_COMPLETE_MARKER) else {
        return false;
    };
    if payload.starts_with(char::is_alphanumeric) {
        return false;
    }
    !opens_with_negation(payload)
}

/// Whether a declaration's payload begins by denying it.
///
/// A bare `PLAN COMPLETE:` is the contract [`step_prompt`] actually asks for,
/// so an empty payload is affirmative.
fn opens_with_negation(payload: &str) -> bool {
    // Scan to the first word rather than splitting on whitespace: a model
    // writes `no—steps 2-10 remain` and `n/a` as often as it writes `no, …`,
    // and a filter that *deletes* the separator instead of stopping at it
    // reads those as the words `nosteps` and `na` — no longer negations, so
    // the denial closes the plan, which is #2104 all over again.
    let opener = payload.trim_start_matches(|c: char| !c.is_alphanumeric());
    // `n/a` is one idiom the word scan below cannot see: it stops at the
    // slash and reads a bare `n`, which is too thin a word to deny on.
    if opener
        .get(..3)
        .is_some_and(|s| s.eq_ignore_ascii_case("n/a"))
    {
        return true;
    }
    let word: String = opener
        .chars()
        .take_while(|c| c.is_alphanumeric() || APOSTROPHES.contains(c))
        .map(|c| if APOSTROPHES.contains(&c) { '\'' } else { c })
        .flat_map(char::to_lowercase)
        .collect();
    // `isn't`, `doesn't`, `hasn't` — the contraction carries the negation.
    word.ends_with("n't") || NEGATION_OPENERS.contains(&word.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_line_closes_the_plan_when_it_opens_or_closes_the_reply() {
        assert!(goal_declared_complete("PLAN COMPLETE: nginx is serving."));
        assert!(goal_declared_complete(
            "All eight steps were covered by the setup script.\n  PLAN COMPLETE: verified."
        ));
    }

    #[test]
    fn prose_mentioning_the_phrase_mid_line_does_not() {
        assert!(!goal_declared_complete(
            "I will reply with PLAN COMPLETE once the config is verified."
        ));
        assert!(!goal_declared_complete(
            "Step 3 done; the plan is complete."
        ));
        assert!(!goal_declared_complete(""));
    }

    /// The witness for #2104, verbatim from Terminal-Bench `git-multibranch`
    /// (match `61e01ca06fc7`, worker `claude-sonnet-5`). The old predicate
    /// closed the plan here and skipped steps 2–10 for reward 0.0.
    #[test]
    fn a_negated_echo_of_the_marker_does_not_close_the_plan() {
        let observed = "All required packages (openssh-server, git, nginx, openssl) \
                        are already installed.\n\n\
                        PLAN COMPLETE: no — this is just step 1; only confirming \
                        this step is done.\n\n\
                        Step 1/10 is already complete: the packages ship in the image.\n";
        assert!(
            !goal_declared_complete(observed),
            "a worker denying completion must not close the plan (#2104)"
        );
    }

    #[test]
    fn a_denial_is_rejected_even_when_it_is_the_last_line() {
        for denial in [
            "PLAN COMPLETE: no, steps 2-10 remain.",
            "PLAN COMPLETE: not yet — nginx is still unconfigured.",
            "PLAN COMPLETE: Not complete.",
            "PLAN COMPLETE: partially; the cert is missing.",
            "PLAN COMPLETE: isn't done, one step left.",
        ] {
            assert!(
                !goal_declared_complete(denial),
                "should not close the plan: {denial}"
            );
        }
    }

    /// Models punctuate denials without a space as readily as with one. A
    /// screen that deleted the separator instead of stopping at it read these
    /// as `nosteps` / `noincomplete` / `na` and closed the plan on its own
    /// denial — the #2104 failure, one keystroke away.
    #[test]
    fn a_denial_fused_to_its_punctuation_is_still_a_denial() {
        for denial in [
            "PLAN COMPLETE: no—steps 2-10 remain.",
            "PLAN COMPLETE: no–steps 2-10 remain.",
            "PLAN COMPLETE: no/incomplete, steps remain.",
            "PLAN COMPLETE: no,steps 2-10 remain.",
            "PLAN COMPLETE — no, steps remain.",
            "PLAN COMPLETE: n/a — still on step 1.",
        ] {
            assert!(
                !goal_declared_complete(denial),
                "should not close the plan: {denial}"
            );
        }
    }

    /// The same failure as the fused-punctuation case above, one code point
    /// over: a word scan admitting only ASCII `'` stopped at `U+2019` and read
    /// `isn’t` as the harmless `isn`, so the denial closed the plan. Models
    /// type typographic punctuation by default — the reply that motivated this
    /// whole screen used an em dash.
    #[test]
    fn a_denial_written_with_a_typographic_apostrophe_is_still_a_denial() {
        for denial in [
            "PLAN COMPLETE: isn\u{2019}t done, one step left.",
            "PLAN COMPLETE: doesn\u{2019}t cover steps 2-10.",
            "PLAN COMPLETE: won\u{2019}t be done until nginx is configured.",
            "PLAN COMPLETE: hasn\u{02bc}t started step 4.",
        ] {
            assert!(
                !goal_declared_complete(denial),
                "should not close the plan: {denial}"
            );
        }
    }

    /// The marker is a token, not a prefix. A letter fused to it shifted the
    /// payload past the denial — `PLAN COMPLETED: no, …` left the first word
    /// `d` — so the polarity axis was void wherever this fired.
    #[test]
    fn a_word_fused_to_the_marker_is_not_a_declaration() {
        assert!(!goal_declared_complete(
            "PLAN COMPLETED: no, steps 2-10 remain."
        ));
        assert!(!goal_declared_complete("PLAN COMPLETENESS: 1 of 10 steps."));
        // The punctuation that does follow the marker in practice still declares.
        assert!(goal_declared_complete("PLAN COMPLETE: all ten steps done."));
        assert!(goal_declared_complete(
            "PLAN COMPLETE \u{2014} all ten steps done."
        ));
        assert!(goal_declared_complete("PLAN COMPLETE"));
    }

    /// `no` opening the payload is read as a denial even though an affirmative
    /// reading exists (`no further work needed`). Deliberate, and pinned so it
    /// is not "fixed" into a hole later: the two errors are not symmetric — a
    /// missed declaration costs the redundant walk this module exists to
    /// avoid, a spurious one cost a whole trial (#2104). `none` is not an
    /// opener, so the unambiguous phrasing still closes the plan.
    #[test]
    fn an_ambiguous_no_is_read_as_a_denial() {
        assert!(!goal_declared_complete(
            "PLAN COMPLETE: no further work needed."
        ));
        assert!(goal_declared_complete("PLAN COMPLETE: none remaining."));
    }

    /// The negation screen matches whole words, so a completion whose first
    /// word merely *starts* with one still closes the plan.
    #[test]
    fn an_affirmative_payload_that_looks_like_a_negation_still_closes() {
        assert!(goal_declared_complete("PLAN COMPLETE: nothing left to do."));
        assert!(goal_declared_complete(
            "PLAN COMPLETE: noted, all eight done."
        ));
        assert!(goal_declared_complete("PLAN COMPLETE:"));
        assert!(goal_declared_complete("PLAN COMPLETE"));
    }

    /// A marker buried mid-reply, with argument still running after it, is not
    /// a declaration — but the same marker opening or closing the reply is.
    #[test]
    fn the_declaration_must_open_or_close_the_reply() {
        assert!(!goal_declared_complete(
            "Step 1 is done.\nPLAN COMPLETE: everything is finished.\nBut steps 2-10 remain."
        ));
        assert!(goal_declared_complete(
            "PLAN COMPLETE: everything is finished.\n\nDetails: nginx serves TLS on 443."
        ));
        assert!(goal_declared_complete(
            "Ran the setup script.\n\n  PLAN COMPLETE: all eight steps covered.  "
        ));
    }

    #[test]
    fn the_prompt_numbers_from_one_and_carries_the_close_out_offer() {
        let prompt = step_prompt(0, 8, "Install nginx");
        assert!(prompt.starts_with("Step 1/8: Install nginx"));
        assert!(prompt.contains(PLAN_COMPLETE_MARKER));
    }
}
