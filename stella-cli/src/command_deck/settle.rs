// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The settle window: everything true about a turn AFTER the deck has already
//! called it finished.
//!
//! `AgentEvent::Complete` leaves the turn and the deck instantly paints
//! `✓ complete · 100% · done`. Two separate things are wrong with the picture
//! at that instant, and this module owns both, because they are the same
//! mistake seen from two sides — the deck reports an ending the driver has not
//! reached.
//!
//! 1. [`run_while_listening`] — the driver is not ready for the next prompt.
//!    It has left its `select!` and is doing post-turn bookkeeping, one item of
//!    which is a live model call. Anything typed in that window went unread.
//!
//! 2. [`ends_with_a_question`] — the turn may not be over at all. If it closed
//!    by asking the user something, `done` is the opposite of the truth.
//!
//! ## Telling "the work is finished" apart from "the agent asked you something"
//!
//! A lead turn ends by emitting [`stella_protocol::AgentEvent::Complete`],
//! which the deck's fold maps to [`stella_tui::AgentStatus::Done`] — a status
//! that reports `is_terminal()`. For a worker that is exactly right: it was
//! dispatched, it finished, its row is over. For the lead it is a lie in the common case. The
//! lead is never *done*; it is between turns. And when the closing message is
//! a question, painting the row `done · complete · 100%` tells the user the
//! opposite of what is true — that nothing is expected of them.
//!
//! There is already a precise channel for this: an agent that calls the
//! ask-user gate emits [`stella_protocol::AgentEvent::AskUser`], which the
//! fold maps to [`stella_tui::AgentStatus::WaitingInput`] (rendered
//! `needs input`, `?` glyph, warning colour) and the driver parks on until
//! answered. That path is unchanged and remains the exact one. This module
//! covers the case it cannot see: a model that ends its turn by asking in
//! **prose** rather than through the gate — "Want me to dig into which of
//! these to fix?" — which is by far how models actually ask.
//!
//! ## The bias is deliberate
//!
//! The two errors are not symmetric, so this predicate does not try to be
//! balanced.
//!
//! A false positive paints the lane `needs input` on a turn that ended with a
//! rhetorical question. The cost is a `?` where a `✓` belonged — and the lane
//! is *still* accurate, because the lead genuinely is waiting for the user.
//! Nothing is blocked, nothing is lost, and the next prompt clears it.
//!
//! A false negative is the bug this exists to kill: the agent asks a real
//! question, the deck reports the session complete, and the user is left
//! reading a finished-looking screen with an unanswered question on it.
//!
//! So the rule is tuned to catch, not to be conservative.

use stella_protocol::{CompletionMessage, MessageRole};
use stella_tui::{UserInput, WorkspaceInput};
use tokio::sync::mpsc;

use crate::subsession;

/// Run `work` — a turn's post-turn bookkeeping — WITHOUT going deaf to the
/// deck, returning the inputs it could not service itself.
///
/// This window is the gap the deck cannot see. The deck paints the turn
/// finished the moment `Complete` lands, but the driver is nowhere near ready
/// for the next prompt: it has left its `select!` and is recording the turn
/// episode and running the reflection pass — and reflection is a *live model
/// call*. Until it returns, the driver polls nothing. Every prompt submitted in
/// that window sits unread in the channel while the deck, which mirrors
/// submissions locally, shows `N queued` under a turn that reads over. From the
/// user's chair the deck has stopped accepting input, and the wait scales with
/// how much the turn did — it is disk and network work, so long turns are
/// worse.
///
/// [`subsession::SteeringTap::mark_settling`] was the fix for the *first* half
/// of this window — the part still inside the turn future, which the driver's
/// `select!` keeps polling. It cannot help here, because here there is no
/// `select!` left running to latch anything for.
///
/// So this is that select, for the second half. Prompts route exactly as
/// [`subsession::route_mid_turn`] routes them for a settling turn (they become
/// the next turn, `>` and all), queue edits mirror as they do everywhere else,
/// and anything this window cannot honestly service is handed back for the idle
/// arm to run — dropping a worker `Stop` because it arrived during the lead's
/// bookkeeping would just be a quieter version of the same bug.
pub(super) async fn run_while_listening<F>(
    work: F,
    sub_rx: &mut mpsc::UnboundedReceiver<WorkspaceInput>,
    queue: &mut crate::session_persist::DurableQueue,
) -> Vec<WorkspaceInput>
where
    F: std::future::Future<Output = ()>,
{
    let mut deferred = Vec::new();
    tokio::pin!(work);
    loop {
        tokio::select! {
            () = &mut work => break,
            input = sub_rx.recv() => match input {
                // The deck is gone — there is nobody left to service, and a
                // closed channel reports `None` forever, so stop selecting.
                //
                // Finish the work rather than dropping it. `break`ing here
                // would CANCEL it, and the first thing it does is write the
                // turn's episode: losing a durable record because the user
                // quit while it was being written is exactly the kind of
                // preventable loss the driver is supposed to ride out.
                None => {
                    (&mut work).await;
                    break;
                }
                Some(WorkspaceInput::Enqueue { text })
                | Some(WorkspaceInput::ToAgent {
                    input: UserInput::Prompt { text, .. }, ..
                }) => {
                    // `settling = true` unconditionally: the turn is not just
                    // past its last step boundary, it has already returned.
                    // Deferring to `route_mid_turn` rather than re-deriving the
                    // rule is what keeps the two settle windows from ever
                    // disagreeing about where a prompt belongs.
                    //
                    // That call answers `NextTurn` for every input while
                    // settling; the other two arms are unreachable today and are
                    // written to queue anyway, so a later change to the routing
                    // table cannot strand a prompt here — which is the exact
                    // failure this function exists to prevent.
                    match subsession::route_mid_turn(text, true) {
                        subsession::MidTurnRoute::NextTurn(text)
                        | subsession::MidTurnRoute::Steer(text)
                        | subsession::MidTurnRoute::Sidecar(text) => queue.push_back(text),
                    }
                }
                Some(WorkspaceInput::EnqueueFront { text }) => queue.push_front(text),
                Some(WorkspaceInput::QueueRemove { index }) => {
                    if index < queue.len() {
                        queue.remove(index);
                    }
                }
                Some(WorkspaceInput::QueueClear) => queue.clear(),
                Some(other) => deferred.push(other),
            },
        }
    }
    deferred
}

/// Whether `messages` ends with the agent putting the ball in the user's
/// court.
///
/// Reads the LAST assistant message with prose in it (an assistant message
/// that only made tool calls carries an empty `content` and says nothing about
/// how the turn closed), and asks whether its **final paragraph** contains a
/// sentence ending in `?`.
///
/// Final paragraph, not final character: models very rarely end on the
/// question mark. The shape that prompted this — and the one in the report —
/// asks and then keeps talking:
///
/// ```text
/// Want me to dig into which of these to fix? The highest-leverage one is
/// #1 (judge diff baseline) — it's a clear correctness bug. I can trace
/// exactly where the diff is built and why it ignored the file-change events.
/// ```
///
/// A `ends_with('?')` test answers "no" there, which is precisely wrong. The
/// closing paragraph is the unit that carries the ask.
pub(super) fn ends_with_a_question(messages: &[CompletionMessage]) -> bool {
    let Some(closing) = messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Assistant && !m.content.trim().is_empty())
    else {
        return false;
    };
    last_paragraph(&strip_code(&closing.content)).is_some_and(|p| p.contains('?'))
}

/// `text` with fenced blocks and inline spans removed.
///
/// A `?` inside code is not a question to anyone — `grep -c '?'`, a glob, a
/// ternary, a URL query string. Without this, a turn that closes by showing
/// the command it ran would read as an unanswered question forever.
///
/// Fences are matched on a line whose first non-space run is ``` or ~~~, which
/// is what the markdown renderer on the other side of this already assumes; an
/// unterminated fence swallows the rest of the message, which is the correct
/// failure (that text is code as far as anyone reading it is concerned).
fn strip_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_fence = false;
    for line in text.lines() {
        let head = line.trim_start();
        if head.starts_with("```") || head.starts_with("~~~") {
            in_fence = !in_fence;
            // Emit the (now blank) line so paragraph structure around a fence
            // survives: a fence between two paragraphs must not weld them.
            out.push('\n');
            continue;
        }
        if !in_fence {
            out.push_str(&strip_inline_code(line));
        }
        out.push('\n');
    }
    out
}

/// One line with `` `…` `` spans blanked. An unclosed backtick is left as
/// ordinary text — treating the rest of the line as code on the strength of a
/// single stray tick loses more than it saves.
fn strip_inline_code(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('`') {
            Some(close) => rest = &after[close + 1..],
            None => {
                out.push_str(after);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The last blank-line-separated paragraph with content, or `None` when there
/// is none.
fn last_paragraph(text: &str) -> Option<&str> {
    // `last`, not `next_back`: splitting on a multi-char `&str` pattern is not
    // a double-ended iterator, because the pattern has no reverse searcher.
    text.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .last()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(content: &str) -> CompletionMessage {
        CompletionMessage {
            role: MessageRole::Assistant,
            content: content.to_string(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            attachments: Vec::new(),
        }
    }

    fn user(content: &str) -> CompletionMessage {
        CompletionMessage {
            role: MessageRole::User,
            ..assistant(content)
        }
    }

    /// The reported case, verbatim in shape: the question opens the closing
    /// paragraph and three sentences of context follow it. This is the test
    /// that a `ends_with('?')` implementation fails.
    #[test]
    fn a_question_that_is_not_the_last_sentence_still_counts() {
        let turn = [assistant(
            "The irony: the underlying task succeeded, but stella scored 0.0.\n\n\
             Want me to dig into which of these to fix? The highest-leverage one \
             is #1 (judge diff baseline) — it's a clear correctness bug in \
             pipeline.rs's evidence assembly. I can trace exactly where the diff \
             is built and why it ignored the file-change events.",
        )];
        assert!(ends_with_a_question(&turn));
    }

    #[test]
    fn a_turn_that_closes_on_a_statement_is_done() {
        let turn = [assistant(
            "Fixed the diff baseline and added a regression test.\n\n\
             The gate is green: 412 passed, 0 failed.",
        )];
        assert!(!ends_with_a_question(&turn));
    }

    /// A question in an EARLIER paragraph the agent then answered itself is not
    /// an open ask — the closing paragraph is the unit that carries intent.
    #[test]
    fn a_question_the_agent_answered_itself_does_not_count() {
        let turn = [assistant(
            "So why did the judge see an empty diff? Because it shells out to \
             git in a sandbox that has no git.\n\n\
             I fell back to the file_change stream and the ladder now agrees \
             with its own counters.",
        )];
        assert!(!ends_with_a_question(&turn));
    }

    /// The whole reason `strip_code` exists.
    #[test]
    fn a_question_mark_inside_a_fence_is_not_a_question() {
        let turn = [assistant(
            "Ran the sweep:\n\n\
             ```sh\n\
             rg -n 'why\\?' crates/\n\
             ```\n\n\
             ```sh\n\
             cargo test -p stella-cli\n\
             ```",
        )];
        assert!(!ends_with_a_question(&turn));
    }

    #[test]
    fn a_question_mark_inside_inline_code_is_not_a_question() {
        let turn = [assistant(
            "Set the flag with `stella --ask?=on` to enable it.",
        )];
        assert!(!ends_with_a_question(&turn));
    }

    /// Prose either side of a fence must not be welded into one paragraph by
    /// the stripping — the fence lines become blanks, which keeps the split.
    #[test]
    fn a_fence_does_not_weld_the_paragraphs_around_it() {
        let turn = [assistant(
            "Should I apply this?\n\
             ```sh\n\
             cargo build\n\
             ```\n\
             Done — the build is clean.",
        )];
        assert!(!ends_with_a_question(&turn));
    }

    /// The closing message is the last assistant message with PROSE. A
    /// tool-only assistant message (empty content) after it says nothing about
    /// how the turn ended and must not blank the answer.
    #[test]
    fn a_trailing_tool_only_message_does_not_hide_the_question() {
        let turn = [
            assistant("Want me to build the fix?"),
            CompletionMessage {
                content: String::new(),
                ..assistant("")
            },
        ];
        assert!(ends_with_a_question(&turn));
    }

    /// A user message can end in `?` on every turn — reading it as the
    /// agent's ask would leave every session painted `needs input`.
    #[test]
    fn the_users_own_question_is_not_the_agents() {
        let turn = [user("why is the judge diff empty?"), assistant("Fixed it.")];
        assert!(!ends_with_a_question(&turn));
    }

    /// The bug: `AgentEvent::Complete` leaves the turn and the deck instantly
    /// paints `✓ complete · 100% · done`, but the driver has left its `select!`
    /// and is inside post-turn bookkeeping — including reflection, which is a
    /// live model call. A prompt submitted at that finished-looking turn used to
    /// sit unread in the channel for as long as that call took, showing as
    /// `N queued` under a turn that reads over.
    ///
    /// [`run_while_listening`] is the select for that window. Bookkeeping here
    /// is a 30-second sleep on a PAUSED clock, so it is genuinely pending for
    /// the whole drain: every assertion below describes what the user gets
    /// while the work is still in flight, which is the property that was
    /// missing.
    #[tokio::test(start_paused = true)]
    async fn a_prompt_typed_during_post_turn_bookkeeping_reaches_the_backlog() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let dir = std::env::temp_dir().join(format!("stella-settle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut queue = crate::session_persist::DurableQueue::fresh(dir.clone());

        tx.send(WorkspaceInput::Enqueue {
            text: "yes, build the fix".to_string(),
        })
        .unwrap();
        tx.send(WorkspaceInput::Enqueue {
            text: "> and start with the judge".to_string(),
        })
        .unwrap();
        tx.send(WorkspaceInput::Control {
            agent: "req:1".to_string(),
            control: stella_tui::AgentControl::Stop,
        })
        .unwrap();
        // Closing the deck's side is what ends the drain; the work outlives it.
        drop(tx);

        let finished = std::cell::Cell::new(false);
        let deferred = run_while_listening(
            async {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                finished.set(true);
            },
            &mut rx,
            &mut queue,
        )
        .await;

        // The deck closing must not CANCEL the bookkeeping: its first act is
        // the durable episode write, and quitting mid-write would lose it.
        assert!(finished.get(), "the bookkeeping was dropped, not finished");

        // Both prompts are queued in submission order, and the durable backlog —
        // the authoritative copy — agrees.
        assert_eq!(queue.len(), 2);
        assert_eq!(
            stella_store::journal::read_queue(&dir),
            vec![
                "yes, build the fix".to_string(),
                // The `>` is stripped and continued, never dropped: a settling
                // turn has no step boundary left to steer.
                "and start with the judge".to_string(),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);

        // What this window cannot service is handed back rather than dropped: a
        // worker `Stop` that lands during the lead's bookkeeping must still stop
        // that worker.
        assert!(
            matches!(
                deferred.as_slice(),
                [WorkspaceInput::Control { agent, .. }] if agent == "req:1"
            ),
            "a worker control must survive the window, got {deferred:?}"
        );
    }

    /// Queue edits mirror inside the window too. The deck applies each edit to its
    /// own view at send time, so a window that ignored them would leave the two
    /// queues disagreeing about what runs next.
    #[tokio::test(start_paused = true)]
    async fn queue_edits_during_the_settle_window_keep_the_two_queues_in_step() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let dir = std::env::temp_dir().join(format!("stella-settle-edit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut queue = crate::session_persist::DurableQueue::fresh(dir.clone());
        queue.push_back("already waiting".to_string());

        tx.send(WorkspaceInput::EnqueueFront {
            text: "run me first".to_string(),
        })
        .unwrap();
        tx.send(WorkspaceInput::QueueRemove { index: 1 }).unwrap();
        // Out of range: the deck's view and the driver's can differ by one pop, so
        // this must be a no-op rather than a panic.
        tx.send(WorkspaceInput::QueueRemove { index: 9 }).unwrap();
        drop(tx);

        let deferred = run_while_listening(
            tokio::time::sleep(std::time::Duration::from_secs(30)),
            &mut rx,
            &mut queue,
        )
        .await;

        assert_eq!(
            stella_store::journal::read_queue(&dir),
            vec!["run me first".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            deferred.is_empty(),
            "queue edits are serviced, not deferred"
        );
    }

    #[test]
    fn a_turn_with_no_assistant_prose_is_not_a_question() {
        assert!(!ends_with_a_question(&[user("go")]));
        assert!(!ends_with_a_question(&[]));
    }
}
