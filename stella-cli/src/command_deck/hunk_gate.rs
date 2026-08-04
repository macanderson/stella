//! Per-hunk approval through the deck (#1265) — the [`HunkReviewIo`] the
//! mutating tool stack blocks on before it writes.
//!
//! Split out of `command_deck.rs` beside [`super::scope_gate`], which does the
//! same job one granularity up: that one parks a *plan* on a human, this one
//! parks a *write*.
//!
//! [`HunkReviewIo`]: stella_tools::hunk_review::HunkReviewIo

use std::sync::Arc;

use async_trait::async_trait;
use stella_protocol::{AgentEvent, HunkProposal, ToolOutput};
use stella_tui::Inbound;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// [`HunkReviewIo`] over the deck's channels. Emits the `HunkReview` card,
/// parks on the decision channel, then echoes a `ToolResult` carrying the
/// proposal's id — the event-pure clear, identical in shape to how
/// `DeckAskUserIo` closes its question.
///
/// [`HunkReviewIo`]: stella_tools::hunk_review::HunkReviewIo
#[derive(Clone)]
pub(crate) struct DeckHunkReviewIo {
    pub(crate) agent: String,
    pub(crate) inbound: UnboundedSender<Inbound>,
    pub(crate) decisions: Arc<tokio::sync::Mutex<UnboundedReceiver<Vec<usize>>>>,
}

#[async_trait]
impl stella_tools::hunk_review::HunkReviewIo for DeckHunkReviewIo {
    async fn review(&self, proposal: &HunkProposal) -> Result<Vec<usize>, String> {
        let mut decisions = self.decisions.lock().await;
        // Drop decisions stranded by a cancelled turn — they answer a card that
        // no longer exists, and here a stale answer would write files.
        while decisions.try_recv().is_ok() {}

        let _ = self.inbound.send(Inbound::Event {
            agent: self.agent.clone(),
            event: AgentEvent::HunkReview {
                proposal: proposal.clone(),
            },
        });

        let accepted = decisions
            .recv()
            .await
            .ok_or_else(|| "the deck closed before the hunks were reviewed".to_string())?;

        // Without this echo the card would keep eating keys for the rest of the
        // turn — the fold clears the pending gate on a result with this id.
        let _ = self.inbound.send(Inbound::Event {
            agent: self.agent.clone(),
            event: AgentEvent::ToolResult {
                call_id: proposal.id.clone(),
                output: ToolOutput::Ok {
                    content: format!(
                        "applying {} of {} hunk(s)",
                        accepted.len(),
                        proposal.hunks.len()
                    ),
                },
                duration_ms: 0,
                speculated: false,
            },
        });
        Ok(accepted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::ProposedHunk;
    use stella_tools::hunk_review::HunkReviewIo;
    use tokio::sync::mpsc;

    fn proposal() -> HunkProposal {
        HunkProposal {
            id: "hunk-review-1".into(),
            tool: "apply_edits".into(),
            hunks: vec![ProposedHunk {
                path: "a.rs".into(),
                diff: "@@ -1,1 +1,1 @@\n-a\n+b\n".into(),
                lines_added: 1,
                lines_removed: 1,
            }],
        }
    }

    /// A deck that went away mid-review is an unreachable reviewer, and
    /// `HunkGate` turns that into a refusal — so this must be an `Err`, never
    /// an empty (or full) selection that reads as a decision somebody made.
    #[tokio::test]
    async fn a_closed_deck_is_an_error_not_a_silent_answer() {
        let (in_tx, _in_rx) = mpsc::unbounded_channel();
        let (dtx, drx) = mpsc::unbounded_channel();
        drop(dtx);
        let io = DeckHunkReviewIo {
            agent: "lead".into(),
            inbound: in_tx,
            decisions: Arc::new(tokio::sync::Mutex::new(drx)),
        };
        assert!(io.review(&proposal()).await.is_err());
    }

    /// The round trip: the card goes out, the answer comes back verbatim, and
    /// the echoed `ToolResult` carries the proposal's id so the fold clears it.
    #[tokio::test]
    async fn the_card_goes_out_and_the_echo_clears_it() {
        let (in_tx, mut in_rx) = mpsc::unbounded_channel();
        let (dtx, drx) = mpsc::unbounded_channel();
        dtx.send(vec![0]).unwrap();
        let io = DeckHunkReviewIo {
            agent: "lead".into(),
            inbound: in_tx,
            decisions: Arc::new(tokio::sync::Mutex::new(drx)),
        };
        assert_eq!(io.review(&proposal()).await.unwrap(), vec![0]);

        let mut events = Vec::new();
        while let Ok(Inbound::Event { event, .. }) = in_rx.try_recv() {
            events.push(event);
        }
        assert!(matches!(
            events.first(),
            Some(AgentEvent::HunkReview { .. })
        ));
        assert!(matches!(
            events.last(),
            Some(AgentEvent::ToolResult { call_id, .. }) if call_id == "hunk-review-1"
        ));
    }
}
