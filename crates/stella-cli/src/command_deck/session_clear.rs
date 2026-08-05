//! `/clear` as an event, not a queued prompt.
//!
//! The deck sends [`WorkspaceInput::SessionClear`](stella_tui::WorkspaceInput)
//! the moment `/clear` is submitted — it never enters the prompt backlog, so
//! it can never sit in the queue popup as "pending" behind a running turn
//! (the reported bug). The driver resets immediately: between turns from the
//! idle arm, mid-turn by dropping the turn future (`TurnEnd::Cleared`) and
//! resetting at the boundary.
//!
//! This module is the reset itself, shared by both arms. Ordering matters on
//! the FIFO inbound channel: everything a dying turn already emitted precedes
//! the [`Inbound::SessionReset`] sent here, so the deck blanks its pane AFTER
//! the stale events land, and nothing can repaint a transcript the user just
//! cleared.

use super::*;

/// Reset the lead session to its seq-0 state: the LLM history becomes the
/// system prompt alone, the deck pane blanks ([`Inbound::SessionReset`]), the
/// dashboard returns to waiting ([`AgentStatus::WaitingInput`] — also the
/// journal's settle marker), and the durable boundary snapshot is rewritten
/// so a resume continues from the cleared state, not from before it.
pub(super) fn reset_lead(
    messages: &mut Vec<CompletionMessage>,
    system_prompt: &str,
    sidecar_dir: &std::path::Path,
    in_tx: &UnboundedSender<Inbound>,
) {
    messages.clear();
    messages.push(CompletionMessage::system(system_prompt.to_string()));
    let _ = in_tx.send(Inbound::SessionReset {
        agent: LEAD.to_string(),
    });
    let _ = in_tx.send(Inbound::Status {
        agent: LEAD.to_string(),
        status: AgentStatus::WaitingInput,
    });
    let _ = crate::session_persist::snapshot_history(sidecar_dir, messages);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reset leaves exactly one message — the system prompt — and tells
    /// the deck to blank the pane, then to show the lead as waiting. Order
    /// matters: `SessionReset` must precede the status flip, so the pane is
    /// never painted "waiting" over a transcript that is about to vanish.
    #[test]
    fn reset_lead_rewinds_history_and_blanks_the_deck_in_order() {
        let dir =
            std::env::temp_dir().join(format!("stella-session-clear-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut messages = vec![
            CompletionMessage::system("SYSTEM".to_string()),
            CompletionMessage::user("a prompt"),
            CompletionMessage::assistant("an answer"),
        ];
        let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Inbound>();

        reset_lead(&mut messages, "SYSTEM", &dir, &in_tx);

        assert_eq!(messages.len(), 1, "only the system prompt survives");
        assert!(matches!(
            in_rx.try_recv(),
            Ok(Inbound::SessionReset { agent }) if agent == LEAD
        ));
        assert!(matches!(
            in_rx.try_recv(),
            Ok(Inbound::Status { agent, status: AgentStatus::WaitingInput }) if agent == LEAD
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
