// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Keep the deck's input channel serviced while a slash command runs.
//!
//! A slash command is awaited by the driver between turns, outside the turn
//! loop's `select!` — and that `select!` is the only place an
//! [`UserInput::AskUserAnswer`] was forwarded to the channel
//! [`super::mid_turn_ask::DeckAskUserIo`] parks on. So a slash command that
//! raised a question (`/init`'s markdown→TOML conversion offer, #3642) could
//! never receive the answer: the deck sent it, nothing read it, the card's
//! `ToolResult` never came back to clear the gate, and every key after that
//! was swallowed by the pending question. Ctrl-C was the only way out.
//!
//! [`await_answerable`] is the missing arm. It races the command against the
//! deck's channel: an answer is forwarded, a quit aborts the command, and any
//! other input is *deferred* — pushed onto the idle arm's replay buffer so a
//! prompt typed during `/init` keeps its place ahead of anything typed after
//! it, exactly as the turn loop's `deferred` queue already promises.
//!
//! Separate from `command_deck.rs` because that file is a god file closed to
//! growth (`scripts/file-size-baseline.txt`); the call site there is one
//! expression.

use std::collections::VecDeque;
use std::future::Future;

use stella_tui::{UserInput, WorkspaceInput};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// How a slash command awaited through [`await_answerable`] ended.
#[derive(Debug)]
pub(super) enum CommandWake<T> {
    /// The command ran to completion.
    Finished(T),
    /// The deck quit (or its channel closed) mid-command; the command's
    /// future was dropped unfinished.
    Quit,
}

/// Await `command` while forwarding the deck's ask answers to `ask_tx`.
///
/// Every [`WorkspaceInput`] that arrives on `sub_rx` during the command is
/// handled in one of three ways:
///
/// - [`UserInput::AskUserAnswer`] → its text goes to `ask_tx`, which is the
///   channel the pending question's `prompt()` is blocked on;
/// - [`WorkspaceInput::Quit`], or the channel closing → the command is
///   dropped and [`CommandWake::Quit`] returned;
/// - anything else → `deferred.push_back`, for the idle arm to replay in
///   arrival order once the command returns.
pub(super) async fn await_answerable<F, T>(
    command: F,
    sub_rx: &mut UnboundedReceiver<WorkspaceInput>,
    ask_tx: &UnboundedSender<String>,
    deferred: &mut VecDeque<WorkspaceInput>,
) -> CommandWake<T>
where
    F: Future<Output = T>,
{
    tokio::pin!(command);
    loop {
        tokio::select! {
            outcome = &mut command => return CommandWake::Finished(outcome),
            input = sub_rx.recv() => match input {
                None | Some(WorkspaceInput::Quit) => return CommandWake::Quit,
                Some(WorkspaceInput::ToAgent {
                    input: UserInput::AskUserAnswer { answer, .. },
                    ..
                }) => {
                    let _ = ask_tx.send(answer);
                }
                Some(other) => deferred.push_back(other),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    fn answer(text: &str) -> WorkspaceInput {
        WorkspaceInput::ToAgent {
            agent: "lead".to_string(),
            input: UserInput::AskUserAnswer {
                id: "deck-ask-0".to_string(),
                answer: text.to_string(),
            },
        }
    }

    /// The witness for the deadlock: a command parked on the ask channel
    /// completes once the deck's answer arrives on the input channel.
    #[tokio::test]
    async fn an_answer_sent_during_the_command_reaches_the_parked_question() {
        let (sub_tx, mut sub_rx) = unbounded_channel::<WorkspaceInput>();
        let (ask_tx, mut ask_rx) = unbounded_channel::<String>();
        let mut deferred = VecDeque::new();

        // Stands in for `/init`: blocked until an answer lands on `ask_rx`,
        // exactly as `DeckAskUserIo::prompt` is.
        let command = async move { ask_rx.recv().await };
        sub_tx.send(answer("2")).expect("deck input channel open");

        let wake = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            await_answerable(command, &mut sub_rx, &ask_tx, &mut deferred),
        )
        .await
        .expect("the command must not hang once the answer is sent");

        match wake {
            CommandWake::Finished(Some(text)) => assert_eq!(text, "2"),
            other => panic!("expected the forwarded answer, got {other:?}"),
        }
        assert!(deferred.is_empty(), "an answer is consumed, never deferred");
    }

    /// A prompt typed while the command runs is neither lost nor acted on
    /// early: it waits in `deferred`, in arrival order.
    #[tokio::test]
    async fn other_input_is_deferred_in_arrival_order() {
        let (sub_tx, mut sub_rx) = unbounded_channel::<WorkspaceInput>();
        let (ask_tx, mut ask_rx) = unbounded_channel::<String>();
        let mut deferred = VecDeque::new();

        sub_tx
            .send(WorkspaceInput::Enqueue {
                text: "first".to_string(),
            })
            .expect("open");
        sub_tx
            .send(WorkspaceInput::Enqueue {
                text: "second".to_string(),
            })
            .expect("open");
        sub_tx.send(answer("1")).expect("open");

        let command = async move { ask_rx.recv().await };
        let wake = await_answerable(command, &mut sub_rx, &ask_tx, &mut deferred).await;
        assert!(matches!(wake, CommandWake::Finished(Some(ref t)) if t == "1"));

        let texts: Vec<String> = deferred
            .iter()
            .map(|input| match input {
                WorkspaceInput::Enqueue { text } => text.clone(),
                other => panic!("unexpected deferred input {other:?}"),
            })
            .collect();
        assert_eq!(texts, ["first", "second"]);
    }

    /// Quit aborts the command rather than leaving the driver parked on a
    /// question nobody will answer.
    #[tokio::test]
    async fn quit_drops_the_command() {
        let (sub_tx, mut sub_rx) = unbounded_channel::<WorkspaceInput>();
        let (ask_tx, mut ask_rx) = unbounded_channel::<String>();
        let mut deferred = VecDeque::new();

        sub_tx.send(WorkspaceInput::Quit).expect("open");
        let command = async move { ask_rx.recv().await };
        let wake = await_answerable(command, &mut sub_rx, &ask_tx, &mut deferred).await;
        assert!(matches!(wake, CommandWake::Quit));
    }
}
