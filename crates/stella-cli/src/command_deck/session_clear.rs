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
//!
//! ## Scope: the lead, and only the lead (#1631)
//!
//! `/clear` resets the LEAD session. Live sidecar workers (`req:n`, `sub:n`
//! lanes) keep running, keep their own context, and keep streaming into their
//! own panes — and the reset now **says so**, naming every live lane in the
//! freshly-blanked pane ([`surviving_workers_note`]).
//!
//! That is not a preference; it is what the rest of the driver already does:
//!
//! * **Running work is never torn down implicitly.** The only other
//!   session-wide operation, `WorkspaceInput::SessionResume`, *refuses* rather
//!   than stopping workers, and tells the user how many are live and how to
//!   stop them. A `/clear` that silently killed them would make it the one
//!   deck action that destroys parallel work without being asked to.
//! * **Stopping a worker is an addressed act.** Stop routes by lane
//!   (`UserInput::Cancel` / `AgentControl::Stop` with an `agent`), and the
//!   only caller of [`SubSessions::stop_all`] is
//!   [`subsession::shutdown_workers`] — process teardown. `/clear` is not
//!   teardown: the session id, its registry record, its sidecar dir and its
//!   lanes all survive.
//! * **Refusing, `SessionResume`-style, would be wrong here.** That refusal
//!   exists because a *switch* would orphan workers that stream into this
//!   session's lanes and settle against its records. `/clear` changes no
//!   identity, so the workers stay correctly attached — and the point of the
//!   instant reset is that `/clear` acts NOW, never parks behind other work.
//!
//! What was actually broken was the silence. A blanked pane and a lead back
//! at `WaitingInput` read as an idle session while sidecar work is still
//! streaming; the note is what stops the reset from painting a false picture.

use super::*;

/// Reset the lead session to its seq-0 state: the LLM history becomes the
/// system prompt alone, the deck pane blanks ([`Inbound::SessionReset`]),
/// surviving sidecar workers are named in the blanked pane, the dashboard
/// returns to waiting ([`AgentStatus::WaitingInput`] — also the journal's
/// settle marker), and the durable boundary snapshot is rewritten so a resume
/// continues from the cleared state, not from before it.
///
/// `live_lanes` is [`SubSessions::live_lanes`] at the moment of the clear —
/// the lanes that survive it. Emitting the note *between* the blank and the
/// status flip is load-bearing twice over: after the blank so the note is not
/// wiped by it, and before the status flip because the note is an
/// `AgentEvent::Text`, which the deck's fold reads as the lead running. The
/// trailing `WaitingInput` is what puts the lead back to idle.
pub(super) fn reset_lead(
    messages: &mut Vec<CompletionMessage>,
    system_prompt: &str,
    sidecar_dir: &std::path::Path,
    live_lanes: &[String],
    in_tx: &UnboundedSender<Inbound>,
) {
    messages.clear();
    messages.push(CompletionMessage::system(system_prompt.to_string()));
    let _ = in_tx.send(Inbound::SessionReset {
        agent: LEAD.to_string(),
    });
    if let Some(note) = surviving_workers_note(live_lanes) {
        let _ = in_tx.send(chrome_note(note));
    }
    let _ = in_tx.send(Inbound::Status {
        agent: LEAD.to_string(),
        status: AgentStatus::WaitingInput,
    });
    let _ = crate::session_persist::snapshot_history(sidecar_dir, messages);
}

/// What `/clear` tells the user about the workers it did **not** clear.
///
/// Three jobs, and a rewording that drops one of them is the regression:
/// say the workers are still running, name every lane so the claim is
/// checkable against the dashboard, and say how to stop one. `None` when
/// nothing survived — a clear with no sidecar work says nothing extra.
pub(super) fn surviving_workers_note(live_lanes: &[String]) -> Option<String> {
    let (first, rest) = live_lanes.split_first()?;
    let lanes = std::iter::once(first.as_str())
        .chain(rest.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(", ");
    let subject = if rest.is_empty() {
        "1 sidecar worker is".to_string()
    } else {
        format!("{} sidecar workers are", live_lanes.len())
    };
    Some(format!(
        "conversation cleared — the lead starts over. {subject} still running on {lanes}, \
         with their own context, and their output stays on their own lanes. Press `s` on a \
         lane to stop that worker."
    ))
}

/// Close out the execution a mid-turn `/clear` interrupted. It is recorded
/// as `cancelled` — the clear drops the turn future exactly as a cancel does,
/// so the outcome label matches the thing that happened — and a failed store
/// write is surfaced rather than swallowed, because a clear that quietly lost
/// its telemetry row leaves a hole in the session's journal that nothing else
/// would report.
///
/// Lives here rather than inline in the driver's `TurnEnd::Cleared` arm: the
/// arm is in a god file closed to growth, and the closeout is `/clear`'s own
/// business.
pub(super) fn close_cleared_execution(
    execution: Option<&(Arc<Store>, i64)>,
    registry: &ToolRegistry,
    files_before: usize,
    cleared_cost: f64,
    in_tx: &UnboundedSender<Inbound>,
) {
    let Some((store, id)) = execution else {
        return;
    };
    if agent::record_execution_end(
        store,
        *id,
        registry,
        files_before,
        "cancelled",
        cleared_cost,
        false,
    ) {
        return;
    }
    let _ = in_tx.send(Inbound::Event {
        agent: LEAD.to_string(),
        event: AgentEvent::Error {
            message: "store write failed — this cleared execution was not recorded".to_string(),
            retryable: true,
        },
    });
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
        let dir = scratch("order");
        let mut messages = vec![
            CompletionMessage::system("SYSTEM".to_string()),
            CompletionMessage::user("a prompt"),
            CompletionMessage::assistant("an answer"),
        ];
        let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Inbound>();

        reset_lead(&mut messages, "SYSTEM", &dir, &[], &in_tx);

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

    /// **Witness for #1631.** `/clear` issued while sidecar workers are live
    /// has exactly one defined outcome: the lead resets, the workers are left
    /// alone, and the freshly-blanked pane names them. Pinned end to end,
    /// because every part of it is a way the reset could lie:
    ///
    /// * the note lands AFTER the blank (before it, it would be wiped);
    /// * it names every live lane, so the claim is checkable against the
    ///   dashboard rows that are still there;
    /// * the LAST thing the deck hears is still `WaitingInput` — the note is
    ///   an `AgentEvent::Text`, which the deck's fold reads as the lead
    ///   running, so a note emitted after the status flip would leave the
    ///   lead permanently "running" with nothing running;
    /// * and nothing in the sequence deregisters or stops a lane.
    #[test]
    fn clearing_with_live_sidecars_keeps_them_and_names_them_in_the_blanked_pane() {
        let dir = scratch("sidecars");
        let mut messages = vec![
            CompletionMessage::system("SYSTEM".to_string()),
            CompletionMessage::user("a prompt"),
        ];
        let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Inbound>();
        let live = vec!["req:1".to_string(), "sub:t-9".to_string()];

        reset_lead(&mut messages, "SYSTEM", &dir, &live, &in_tx);

        let sent: Vec<Inbound> = std::iter::from_fn(|| in_rx.try_recv().ok()).collect();
        assert!(
            matches!(&sent[0], Inbound::SessionReset { agent } if agent == LEAD),
            "the blank comes first: {sent:?}"
        );
        let Inbound::Event {
            agent,
            event: AgentEvent::Text { delta },
        } = &sent[1]
        else {
            panic!(
                "expected the surviving-worker note second, got {:?}",
                sent[1]
            );
        };
        assert_eq!(agent, LEAD, "the note belongs in the lead's cleared pane");
        assert!(
            delta.starts_with(stella_tui::NOTICE_MARKER),
            "the note is the program speaking, not the model: {delta}"
        );
        for lane in &live {
            assert!(delta.contains(lane), "the note must name {lane}: {delta}");
        }
        assert!(
            matches!(
                &sent[2],
                Inbound::Status { agent, status: AgentStatus::WaitingInput } if agent == LEAD
            ),
            "the lead must end idle, not running: {sent:?}"
        );
        assert_eq!(
            sent.len(),
            3,
            "no lane is stopped or deregistered: {sent:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A clear with nothing running says nothing extra — the note exists to
    /// correct a false picture, and there is no false picture to correct.
    #[test]
    fn a_clear_with_no_live_workers_adds_no_note() {
        assert_eq!(surviving_workers_note(&[]), None);
    }

    /// Singular and plural both read as English, and both name every lane:
    /// a count the user cannot cross-check against lane names is exactly the
    /// half-truth this note exists to avoid.
    #[test]
    fn the_note_counts_and_names_every_surviving_lane() {
        let one = surviving_workers_note(&["req:1".to_string()]).expect("a note for one worker");
        assert!(one.contains("1 sidecar worker is"), "{one}");
        assert!(one.contains("req:1"), "{one}");

        let many = surviving_workers_note(&[
            "req:1".to_string(),
            "req:2".to_string(),
            "sub:t-1".to_string(),
        ])
        .expect("a note for three workers");
        assert!(many.contains("3 sidecar workers are"), "{many}");
        assert!(many.contains("req:1, req:2, sub:t-1"), "{many}");
        assert!(
            many.contains("stop that worker"),
            "the note must say how to stop one: {many}"
        );
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("stella-session-clear-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
