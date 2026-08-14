//! Detects a "confident zero" (#1477): a turn whose completing step carries
//! no further tool calls, but whose whole history shows no real work either —
//! it poked at the workspace read-only, then trailed off mid-orientation
//! instead of stating a result, and would otherwise reach
//! [`crate::driver::TurnOutcome::Completed`] exactly like a genuine answer.
//!
//! Observed 2026-08-04 (issue #1477): 3 model calls, 2 tool calls, no
//! artifact, closing text `"I'm working with a checksum file."` — the
//! issue's own words for it are "a sentence fragment mid-orientation, not a
//! result" — and the turn reached `complete` anyway. A timeout at least
//! looks like a failure; this looked like success until an external grader
//! scored it zero.
//!
//! # Why this keys off orientation phrasing, not length or punctuation
//!
//! The first cut of this fix flagged any short, tool-less-artifact completion
//! that didn't end in `.`/`!`/`?`. That is unsound: this codebase's own test
//! suite treats bare, unpunctuated closers like `"done"`, `"answered"`, and
//! genuine substantive answers of similar length (`"the retry policy is in
//! retry.rs"`) as completely ordinary short completions after read-only
//! investigation — and they are. Length and punctuation cannot tell a stated
//! result from a stray thought; `"I'm working with a checksum file"` and
//! `"the retry policy is in retry.rs"` are nearly the same length and both
//! lack a trailing period, yet only one is abandoned mid-task.
//!
//! What actually marks a stray thought is *grammatical stance*: it narrates
//! an action in progress ("I'm …", "let me …", "still …") rather than
//! stating a completed one. [`ONGOING_ACTIVITY_PREFIXES`] is a small,
//! explicit allowlist of that stance, matched case-insensitively against the
//! start of the trimmed text — the same kind of pragmatic prefix match this
//! module's neighbors already use (`SUMMARY_MARKER_PREFIX`,
//! `LOOP_STEER_PREFIX`), not a grammar or sentiment model. `stella-core` does
//! no semantic judging; this is a shape check on a narrow, well-evidenced
//! shape, not a verifier.
//!
//! # Both conditions have to hold
//!
//! - **Zero tool calls this turn** never trips this check. A turn that
//!   answers a question directly, with no investigation at all, is a
//!   perfectly ordinary short completion — the fix must not become a hard
//!   gate that refuses to finish legitimately short tasks; some tasks
//!   genuinely are three steps.
//! - **Any workspace-changing tool call** never trips this check either,
//!   however the turn ends — real work happened, whatever the closing line
//!   says.
//! - Only a turn that invested tool calls in looking around, produced no
//!   artifact from any of them, and closes on a short line that reads as
//!   narrated activity rather than a stated result is flagged.
//!
//! # Why the empty-response check lives here too
//!
//! [`check`] also carries the pre-existing "empty and not a deliberate
//! out-of-time stop" abort that used to sit inline in `dispatch_completion`,
//! immediately above where the confident-zero check landed. Both are the
//! same category of decision — is a tool-less completing step actually a
//! real answer? — made at the same call site, in the same order as before.
//! Consolidating them here (rather than adding a second minimal call site to
//! `driver.rs`) is what keeps this fix net-neutral on `driver.rs`'s line
//! count: both `driver.rs` and `driver/tests.rs` sit exactly at their
//! file-size ratchet ceiling, so new logic has to be paid for by moving an
//! equivalent amount out — the same trade `driver::settlement` already made
//! for the budget checks.

use std::collections::HashSet;

use stella_protocol::{AgentEvent, CompletionMessage, FinishReason, MessageRole};

use super::TurnOutcome;
use super::loop_evidence::turn_start_index;
use super::truncation::MAX_LENGTH_CONTINUATIONS;
use crate::event_sender::EventSender;
use crate::step::AbortKind;

/// A completing step's text longer than this is never considered a stray
/// thought, however it opens — a genuine orientation fragment is short by
/// nature, and a longer passage that merely opens with "I'm …" before
/// delivering a full answer is not the failure mode this module catches.
const FRAGMENT_MAX_CHARS: usize = 160;

/// Case-insensitive prefixes (matched against the trimmed, lowercased text)
/// that mark a line as narrating an action in progress rather than stating
/// its result — the grammatical stance a model's stray, cut-off thought
/// actually takes, and one a deliberate final answer essentially never does.
/// Deliberately small and English-specific: a pragmatic heuristic, not a
/// language model. A phrase missing from this list is a false negative
/// (an uncaught confident zero), which is the safe direction to be wrong in
/// — never a false positive that gates a real answer.
const ONGOING_ACTIVITY_PREFIXES: &[&str] = &[
    "i'm ",
    "i\u{2019}m ", // curly apostrophe variant of "I'm "
    "i am ",
    "i'll ",
    "i\u{2019}ll ",
    "i will ",
    "let me ",
    "i need to ",
    "still ",
    "currently ",
    "looking at ",
    "looking into ",
    "checking ",
];

/// The prove-it nudge (#2663), and its once-only marker: the message is
/// prefix-detected in the turn's own transcript, so "at most one nudge per
/// turn" needs no state field and survives a checkpoint resume for free.
///
/// The gate has no in-turn proof channel: every gated mutating turn is asked
/// exactly once, and the model's answer — run a check through whatever tools
/// the session carries, or say why none can exist — is the whole exchange.
pub(super) const PROVE_IT_PREFIX: &str = "Before declaring this task complete:";
const PROVE_IT_NUDGE: &str = "Before declaring this task complete: nothing in this turn proved \
     the work. Run the task's own test or check command, read what it actually says, and fix \
     anything it reveals. If no check can exist for this change, say so explicitly and why, \
     then declare completion.";

/// This turn's tool-call tally, the evidence [`detect`] reasons over.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct TurnActivity {
    /// Every tool call issued so far this turn, whatever it touched.
    tool_calls: usize,
    /// The subset of `tool_calls` that named a tool NOT in the caller's
    /// read-only set — i.e. calls that could have changed the workspace.
    artifact_calls: usize,
}

/// Tally `messages`' current-turn tool calls, classified by whether their
/// tool is read-only. Windowed with [`turn_start_index`] so a prior turn's
/// real work in a long-lived REPL session is never mistaken for THIS turn's.
fn turn_activity(
    messages: &[CompletionMessage],
    read_only_tools: &HashSet<String>,
) -> TurnActivity {
    let start = turn_start_index(messages);
    let mut activity = TurnActivity::default();
    for message in &messages[start..] {
        if message.role != MessageRole::Assistant {
            continue;
        }
        for call in &message.tool_calls {
            activity.tool_calls += 1;
            if !read_only_tools.contains(&call.name) {
                activity.artifact_calls += 1;
            }
        }
    }
    activity
}

/// True when `text` reads as narrated activity in progress ("I'm …", "let
/// me …") rather than a stated result — short, and opening on one of
/// [`ONGOING_ACTIVITY_PREFIXES`].
fn is_orientation_fragment(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().count() > FRAGMENT_MAX_CHARS {
        return false;
    }
    let lower = trimmed.to_lowercase();
    ONGOING_ACTIVITY_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

/// Evidence backing a confident-zero verdict, carried back to the caller so
/// its abort reason can cite the actual tally rather than a vague claim.
/// Private to this module: only [`check`] (the `pub(super)` entry point)
/// needs it outside the unit tests below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConfidentZero {
    /// How many tool calls this turn made before trailing off — always
    /// `>= 1` (see module docs: zero tool calls never trips this).
    tool_calls: usize,
}

/// Decide whether a tool-less completing step is a confident zero: this
/// turn's history shows investigation with no artifact, and `text` — the
/// step's own final line — reads as narrated activity rather than a
/// stated result.
///
/// `messages` is the live transcript *before* this step's own assistant
/// message is appended (mirroring every other check in
/// `driver::dispatch_completion`), so `text` is passed separately rather than
/// read off the last message.
fn detect(
    messages: &[CompletionMessage],
    read_only_tools: &HashSet<String>,
    text: &str,
) -> Option<ConfidentZero> {
    let activity = turn_activity(messages, read_only_tools);
    if activity.tool_calls > 0 && activity.artifact_calls == 0 && is_orientation_fragment(text) {
        Some(ConfidentZero {
            tool_calls: activity.tool_calls,
        })
    } else {
        None
    }
}

/// The window a once-per-turn nudge reasons over: from the last user message
/// that is NOT itself one. [`turn_start_index`] restarts at any user message
/// — including a nudge — so the plain window would put the once-only marker
/// outside its own scan. Measured on the prove-it gate's first field trial
/// (run `gate-ab`, task pypi-server): three nudges in one turn, each re-armed
/// by that reset, riding a refuted `verify_done` into the 900s ceiling — the
/// exact spend the gate exists to stop.
///
/// **Every** engine-authored nudge prefix must be excluded here, not just the
/// asking gate's own: two gates sharing a turn each append a user message, so
/// a window that skipped only its own would let the other's message reopen it
/// and reproduce that regression across gates rather than within one. The
/// end-of-turn service assertion ([`super::live_services`], #2764) is the
/// second such gate.
pub(super) fn nudge_turn_start(messages: &[CompletionMessage]) -> usize {
    messages
        .iter()
        .rposition(|m| m.role == MessageRole::User && !is_engine_nudge(&m.content))
        .unwrap_or(0)
}

/// Whether a user message is one the engine wrote rather than one the user
/// (or host) sent. Exactly the set of once-per-turn nudge prefixes; a new
/// gate adds its own here in the same change that adds the gate.
fn is_engine_nudge(content: &str) -> bool {
    content.starts_with(PROVE_IT_PREFIX)
        || content.starts_with(super::live_services::SERVICES_PREFIX)
}

/// True when this turn did mutating work and the prove-it nudge has not
/// been issued yet — the #2663 condition: work was declared, never proven,
/// and not yet asked about. Windowed with [`nudge_turn_start`], so the
/// nudge itself never reopens the window it closed.
fn wants_prove_it_nudge(messages: &[CompletionMessage], read_only_tools: &HashSet<String>) -> bool {
    if turn_activity(messages, read_only_tools).artifact_calls == 0 {
        return false;
    }
    let start = nudge_turn_start(messages);
    // One nudge per turn: asked and answered, whatever the answer.
    !messages[start..]
        .iter()
        .any(|m| m.role == MessageRole::User && m.content.starts_with(PROVE_IT_PREFIX))
}

/// [`check`]'s verdict on a tool-less completing step. `Nudged` means the
/// declaration and the prove-it reply are already appended to the transcript
/// and the caller should let the turn continue; `Clean` means proceed to
/// build `Completed` normally.
pub(super) enum CompletionRuling {
    Abort(TurnOutcome),
    Nudged,
    Clean,
}

/// The tool-less-completion legitimacy gate `dispatch_completion` calls
/// before it is willing to build a `TurnOutcome::Completed`. Runs, in order:
///
/// 1. The empty-response abort: `text` is blank and this is not a
///    deliberate out-of-time stop (`out_of_time`) — the model was cut off
///    with nothing to show, and recording that as `Completed` is the "turn
///    ends with no feedback" defect.
/// 2. Skip confident-zero entirely when `finish_reason` is `Length`: a
///    length-truncated non-empty answer is a distinct, already-honest "ran
///    out of room" ending (`driver::truncation`), not a voluntary stop, and
///    `dispatch_completion`'s own next block already tells the user so.
/// 3. The confident-zero check ([`detect`], #1477).
/// 4. When `completion_gate` is set: the prove-it nudge (#2663) — a turn
///    that mutated the workspace and closes without having been challenged
///    is sent back once, with a message asking it to prove the work (or say
///    why nothing can), before its declaration is accepted. Off by default:
///    hosts running task mode (the CLI's headless run path, the pipeline's
///    execute stage) opt in; a conversational turn is never gated because a
///    turn with no mutating calls never trips the condition anyway.
///
/// On `Nudged`, the step's own declaration and the prove-it reply are
/// appended here, so the transcript reads claim-then-challenge and the
/// caller only has to keep looping.
pub(super) fn check(
    messages: &mut Vec<CompletionMessage>,
    read_only_tools: &HashSet<String>,
    result: &stella_protocol::CompletionResult,
    out_of_time: bool,
    total_cost_usd: f64,
    completion_gate: bool,
    events: &EventSender,
) -> CompletionRuling {
    let text = &result.text;
    let finish_reason = result.finish_reason;
    let output_tokens = result.usage.output_tokens;
    if text.trim().is_empty() && !out_of_time {
        let reason = match finish_reason {
            // Advised `/compact` until #712; no such command exists, and
            // compaction is automatic every step anyway.
            Some(FinishReason::Length) => format!(
                "The model reached its output-token limit ({output_tokens} tokens) before \
                 producing any visible response — its budget was spent on reasoning — and did \
                 so on every one of its {MAX_LENGTH_CONTINUATIONS} continuations. Retry or raise \
                 the output cap; context compacts automatically each step."
            ),
            _ => format!(
                "The model returned an empty response this turn — no text and no tool call \
                 ({output_tokens} output tokens). Retry, or switch to a different model."
            ),
        };
        let _ = events.send(AgentEvent::Error {
            message: reason.clone(),
            retryable: true,
        });
        // A `Failure`, not a deliberate stop: the model produced nothing
        // usable, which is the run falling over — unlike the confident-zero
        // close below, where the engine itself refuses a trailing-off turn.
        return CompletionRuling::Abort(TurnOutcome::Aborted {
            reason,
            kind: AbortKind::Failure,
            cost_usd: total_cost_usd,
        });
    }
    if finish_reason == Some(FinishReason::Length) {
        return CompletionRuling::Clean;
    }
    if let Some(zero) = detect(messages, read_only_tools, text) {
        let reason = format!(
            "the model ended this turn after {} tool call(s) that changed nothing in the \
             workspace, closing on a line that narrates ongoing activity rather than stating a \
             result (\"{}\") — this reads as an abandoned attempt, not a finished answer. Retry, \
             or continue with more specific direction.",
            zero.tool_calls,
            text.trim(),
        );
        let _ = events.send(AgentEvent::Error {
            message: reason.clone(),
            retryable: true,
        });
        return CompletionRuling::Abort(TurnOutcome::Aborted {
            reason,
            kind: AbortKind::DeliberateStop,
            cost_usd: total_cost_usd,
        });
    }
    if completion_gate && wants_prove_it_nudge(messages, read_only_tools) {
        let _ = events.send(AgentEvent::Text {
            text: "\n⚖ Completion declared without proof — asking the model to verify its work \
                   before accepting it."
                .to_string(),
        });
        messages.push(CompletionMessage::assistant(text));
        messages.push(CompletionMessage::user(PROVE_IT_NUDGE));
        return CompletionRuling::Nudged;
    }
    CompletionRuling::Clean
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use stella_protocol::{CompletionMessage, MessageRole, ToolCall, ToolOutput, ToolResult};

    use super::{ConfidentZero, detect, is_orientation_fragment};

    fn read_only(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn assistant_call(name: &str, call_id: &str) -> CompletionMessage {
        CompletionMessage {
            role: MessageRole::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                call_id: call_id.into(),
                name: name.into(),
                input: serde_json::json!({}),
            }],
            tool_results: Vec::new(),
            attachments: Vec::new(),
        }
    }

    fn tool_result(call_id: &str, content: &str) -> CompletionMessage {
        CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_results: vec![ToolResult {
                call_id: call_id.into(),
                output: ToolOutput::Ok {
                    content: content.into(),
                },
            }],
            attachments: Vec::new(),
        }
    }

    #[test]
    fn orientation_shape_is_short_and_narrates_activity() {
        assert!(is_orientation_fragment("I'm working with a checksum file"));
        assert!(is_orientation_fragment("I'm working with a checksum file."));
        assert!(is_orientation_fragment("Let me check that next"));
        assert!(is_orientation_fragment("still looking"));
        // A genuine stated result, however short or unpunctuated, is not
        // orientation — this codebase's own tests rely on exactly this
        // shape (`"done"`, `"answered"`) being an ordinary completion.
        assert!(!is_orientation_fragment("done"));
        assert!(!is_orientation_fragment("answered"));
        assert!(!is_orientation_fragment("the retry policy is in retry.rs"));
        assert!(!is_orientation_fragment("No, it does not exist."));
        assert!(!is_orientation_fragment(""));
        assert!(!is_orientation_fragment("   "));
        // A long passage that merely opens with an orientation phrase before
        // delivering a full answer is not treated as a stray thought.
        let long = format!(
            "I'm about to explain: {}",
            "x".repeat(super::FRAGMENT_MAX_CHARS)
        );
        assert!(!is_orientation_fragment(&long));
    }

    #[test]
    fn zero_tool_calls_never_trips_even_on_a_bare_orientation_line() {
        // A direct answer with no investigation at all — "some tasks
        // genuinely are three steps" must never be gated on step count.
        let messages = vec![CompletionMessage::user("what's 2+2")];
        assert_eq!(detect(&messages, &read_only(&[]), "4"), None);
    }

    #[test]
    fn any_mutating_call_never_trips_it() {
        let messages = vec![
            CompletionMessage::user("fix the bug"),
            assistant_call("edit_file", "c1"),
            tool_result("c1", "ok"),
        ];
        // "edit_file" is NOT in the read-only set, so this turn produced an
        // artifact even though the closing line narrates ongoing activity.
        assert_eq!(
            detect(&messages, &read_only(&["read_file"]), "still checking"),
            None
        );
    }

    #[test]
    fn a_genuine_short_answer_after_investigation_never_trips_it() {
        let messages = vec![
            CompletionMessage::user("does config.toml exist?"),
            assistant_call("read_file", "c1"),
            tool_result("c1", "no such file"),
        ];
        assert_eq!(
            detect(
                &messages,
                &read_only(&["read_file"]),
                "No, it does not exist."
            ),
            None
        );
        // The unpunctuated, single-word idiom this codebase's own tests use
        // for "the turn is finished" must never be gated either.
        assert_eq!(detect(&messages, &read_only(&["read_file"]), "done"), None);
    }

    #[test]
    fn read_only_investigation_with_a_trailing_orientation_line_trips_it() {
        // The reported shape: two read-only calls, then a short line
        // narrating ongoing activity instead of stating a result.
        let messages = vec![
            CompletionMessage::user("check the gcov coverage"),
            assistant_call("read_file", "c1"),
            tool_result("c1", "checksum.gcov contents"),
            assistant_call("glob", "c2"),
            tool_result("c2", "no matches"),
        ];
        assert_eq!(
            detect(
                &messages,
                &read_only(&["read_file", "glob"]),
                "I'm working with a checksum file"
            ),
            Some(ConfidentZero { tool_calls: 2 })
        );
    }

    #[test]
    fn a_prior_turns_work_never_leaks_into_this_turns_tally() {
        // A long-lived REPL session: turn 1 did real work; turn 2 is the one
        // being judged and did none of its own — the prior turn's mutating
        // call must not be counted toward turn 2.
        let messages = vec![
            CompletionMessage::user("edit the file"),
            assistant_call("edit_file", "c1"),
            tool_result("c1", "ok"),
            CompletionMessage::user("now check something else"),
            assistant_call("read_file", "c2"),
            tool_result("c2", "contents"),
        ];
        assert_eq!(
            detect(&messages, &read_only(&["read_file"]), "looking into it"),
            Some(ConfidentZero { tool_calls: 1 })
        );
    }

    fn done_result(text: &str) -> stella_protocol::CompletionResult {
        stella_protocol::CompletionResult {
            upstream_provider: None,
            text: text.into(),
            tool_calls: vec![],
            usage: stella_protocol::CompletionUsage {
                reported: true,
                output_tokens: 5,
                ..stella_protocol::CompletionUsage::default()
            },
            model: "scripted".into(),
            cost_usd: 0.0,
            finish_reason: None,
        }
    }

    fn events() -> crate::event_sender::EventSender {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        crate::event_sender::EventSender::new(tx)
    }

    /// **The #2663 witness.** A gated turn that mutated the workspace and
    /// declares done unchallenged is nudged exactly once: the first ruling
    /// is `Nudged` (and appends claim-then-challenge), the second — same
    /// declaration, nudge already on the transcript — is `Clean`. Fails on
    /// the old `check`, which had no gate and no ruling.
    #[test]
    fn a_mutating_turn_declared_without_proof_is_nudged_exactly_once() {
        let mut messages = vec![
            CompletionMessage::user("fix the bug"),
            assistant_call("edit_file", "c1"),
            tool_result("c1", "ok"),
        ];
        let ruling = super::check(
            &mut messages,
            &read_only(&["read_file"]),
            &done_result("Done — the bug is fixed."),
            false,
            0.0,
            true,
            &events(),
        );
        assert!(matches!(ruling, super::CompletionRuling::Nudged));
        assert!(
            messages
                .last()
                .is_some_and(|m| m.content.starts_with(super::PROVE_IT_PREFIX)),
            "the challenge follows the claim on the transcript"
        );

        let again = super::check(
            &mut messages,
            &read_only(&["read_file"]),
            &done_result("Done — the bug is fixed."),
            false,
            0.0,
            true,
            &events(),
        );
        assert!(
            matches!(again, super::CompletionRuling::Clean),
            "asked and answered: one nudge per turn, whatever the answer"
        );
    }

    /// **The `gate-ab` field regression, pinned.** The nudge is a user
    /// message, so the plain turn window restarts right after it — and a
    /// failed check plus one more edit then re-armed the nudge, three times
    /// in one trial, into the 900s ceiling. With the window anchored at the
    /// last NON-nudge user message, the answered nudge stays in scope and
    /// the second declaration is `Clean`, whatever the model did after the
    /// ask. Fails on the `turn_start_index`-windowed code.
    #[test]
    fn later_work_after_the_nudge_never_rearms_it() {
        let mut messages = vec![
            CompletionMessage::user("fix the bug"),
            assistant_call("edit_file", "c1"),
            tool_result("c1", "ok"),
            CompletionMessage::assistant("Done — the bug is fixed."),
            CompletionMessage::user(super::PROVE_IT_NUDGE),
            assistant_call("edit_file", "c2"),
            tool_result("c2", "ok"),
        ];
        let ruling = super::check(
            &mut messages,
            &read_only(&["read_file"]),
            &done_result("Done for real this time."),
            false,
            0.0,
            true,
            &events(),
        );
        assert!(
            matches!(ruling, super::CompletionRuling::Clean),
            "one nudge per turn means per TURN — its own message must not \
             reopen the window it closed"
        );
    }

    /// The gate off (every caller today except task mode) and the no-mutation
    /// turn (a conversational answer) are both untouched.
    #[test]
    fn the_gate_never_fires_when_disabled_or_without_mutations() {
        let mut mutated = vec![
            CompletionMessage::user("fix the bug"),
            assistant_call("edit_file", "c1"),
            tool_result("c1", "ok"),
        ];
        let off = super::check(
            &mut mutated,
            &read_only(&["read_file"]),
            &done_result("Done."),
            false,
            0.0,
            false,
            &events(),
        );
        assert!(matches!(off, super::CompletionRuling::Clean));

        let mut read_only_turn = vec![
            CompletionMessage::user("what does retry.rs do?"),
            assistant_call("read_file", "c1"),
            tool_result("c1", "contents"),
        ];
        let unmutated = super::check(
            &mut read_only_turn,
            &read_only(&["read_file"]),
            &done_result("It implements the retry policy."),
            false,
            0.0,
            true,
            &events(),
        );
        assert!(matches!(unmutated, super::CompletionRuling::Clean));
    }
}
