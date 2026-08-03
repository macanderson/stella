//! A runnable demo of the accessible single-pane surface — what
//! `stella --simple` puts on screen (#936).
//!
//! ```sh
//! cargo run -p stella-tui --example simple_demo
//! ```
//!
//! It plays a short scripted turn and then stays interactive: a prompt +
//! Enter runs another scripted turn that echoes it back, and one of those
//! turns parks on an `ask_user` question so the gate round trip is drivable.
//! Ctrl-C quits.
//!
//! Unlike `deck_demo` this never touches the alternate screen. It draws
//! inline beneath whatever was already on your terminal, and each completed
//! message moves into your scrollback — scroll up after quitting and the
//! conversation is still there, as ordinary text. That is the property the
//! surface exists for, and the one `tests/simple_pty_smoke.rs` pins.

use std::time::Duration;

use serde_json::json;
use stella_protocol::{AgentEvent, FileChangeKind, StageKind, ToolCall, ToolOutput};
use stella_tui::{RunOptions, SlashCommand, UserInput, run};
use tokio::sync::mpsc;

/// Paced slowly enough that a human can watch each row land, and a pty test
/// can wait for one without racing the next.
const BEAT: Duration = Duration::from_millis(120);

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let (ev_tx, ev_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (sub_tx, mut sub_rx) = mpsc::unbounded_channel::<UserInput>();

    // The opening scripted turn.
    let opening = ev_tx.clone();
    tokio::spawn(async move {
        let send = |event| {
            let _ = opening.send(event);
        };
        tokio::time::sleep(BEAT).await;
        send(AgentEvent::Stage {
            name: StageKind::Execute,
        });
        tokio::time::sleep(BEAT).await;
        send(AgentEvent::Text {
            delta: "reading the workspace".into(),
        });
        tokio::time::sleep(BEAT).await;
        send(AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "call-1".into(),
                name: "read_file".into(),
                input: json!({ "path": "src/lib.rs" }),
            },
        });
        tokio::time::sleep(BEAT).await;
        send(AgentEvent::ToolResult {
            call_id: "call-1".into(),
            output: ToolOutput::Ok {
                content: "pub fn main() {}".into(),
            },
            duration_ms: 12,
            speculated: false,
        });
        tokio::time::sleep(BEAT).await;
        send(AgentEvent::FileChange {
            path: "src/lib.rs".into(),
            kind: FileChangeKind::Modified,
            added: 2,
            removed: 1,
            diff: Some("@@\n-old\n+new".into()),
        });
        tokio::time::sleep(BEAT).await;
        send(AgentEvent::Text {
            delta: "the opening turn is done".into(),
        });
        tokio::time::sleep(BEAT).await;
        send(AgentEvent::Complete {
            model: "demo-model".into(),
            cost_usd: 0.0042,
        });
    });

    // The reactor: everything the shell sends back comes here, and each
    // prompt gets a scripted turn that quotes it — the full user → driver →
    // user round trip a real session makes through the engine.
    let react = ev_tx.clone();
    tokio::spawn(async move {
        let mut asked = false;
        while let Some(input) = sub_rx.recv().await {
            match input {
                UserInput::Prompt { text, .. } => {
                    let _ = react.send(AgentEvent::Stage {
                        name: StageKind::Execute,
                    });
                    tokio::time::sleep(BEAT).await;
                    let _ = react.send(AgentEvent::Text {
                        delta: format!("you said: {text}"),
                    });
                    // The first prompt parks on a question, so the gate and
                    // its answer echo are reachable from a script.
                    if !asked {
                        asked = true;
                        tokio::time::sleep(BEAT).await;
                        let _ = react.send(AgentEvent::AskUser {
                            id: "ask-1".into(),
                            question: "which branch should this land on?".into(),
                            options: vec!["main".into(), "a new branch".into()],
                        });
                    } else {
                        tokio::time::sleep(BEAT).await;
                        let _ = react.send(AgentEvent::Complete {
                            model: "demo-model".into(),
                            cost_usd: 0.001,
                        });
                    }
                }
                UserInput::AskUserAnswer { id, answer } => {
                    // Echoing the answer back as the card's own ToolResult is
                    // the event-pure clear — without it the gate keeps eating
                    // keys for the rest of the turn.
                    let _ = react.send(AgentEvent::ToolResult {
                        call_id: id,
                        output: ToolOutput::Ok {
                            content: answer.clone(),
                        },
                        duration_ms: 0,
                        speculated: false,
                    });
                    let _ = react.send(AgentEvent::Text {
                        delta: format!("answered: {answer}"),
                    });
                    let _ = react.send(AgentEvent::Complete {
                        model: "demo-model".into(),
                        cost_usd: 0.002,
                    });
                }
                UserInput::ScopeDecision(decision) => {
                    let _ = react.send(AgentEvent::Text {
                        delta: format!("scope decision: {decision:?}"),
                    });
                }
                // Closing the stream is what ends the shell's loop.
                UserInput::Cancel => break,
            }
        }
    });

    // The original sender is dropped; the two task-held clones keep the event
    // stream open for the life of the session.
    drop(ev_tx);

    let opts = RunOptions {
        // The whole point of the demo: inline, scrollback-backed, one column.
        screen_reader: true,
        slash_commands: vec![
            SlashCommand::new("/help", "show the key legend"),
            SlashCommand::new("/files", "what this session has touched"),
            SlashCommand::new("/exit", "end the session"),
        ],
        ..RunOptions::default()
    };
    run(opts, ev_rx, sub_tx).await
}
