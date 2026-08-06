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
//!
//! ## Scope: the WHOLE task board (#1692)
//!
//! The board is not scoped to the lead, and `/clear` wipes all of it. The
//! maintainer's call, verbatim: *"clear the entire task board yes — this is
//! effectively a session destroy and reset function."*
//!
//! Three things follow, and they are one decision, not three:
//!
//! 1. **The live board is emptied** ([`stella_core::tasks::TaskBoard::clear`]).
//!    It is registry-level shared state, so blanking the deck pane alone left
//!    the real board intact behind an empty-looking Tasks tab, waiting for the
//!    next snapshot to repaint every row.
//! 2. **The persisted mirror goes with it**
//!    ([`Store::clear_session_tasks`]). A destroyed board that survives in the
//!    `tasks` table is a resurrectable copy; see that module's docs for why
//!    deletion, not supersession, is the right shape there.
//! 3. **Workers that predate the clear may no longer write to the board.**
//!    They keep running — that is #1631's decision and this does not revisit
//!    it — but their closeout is *sealed out* of the new board by the spawn
//!    generation watermark ([`SubSessions::seal_task_board`]), and
//!    [`settle_worker_task`] quarantines the late report as a note on the
//!    lead's lane instead of folding it silently back in. Without the seal
//!    the drop would not even be safe: board ids restart at "1" after a
//!    clear, so a pre-clear worker for task "1" would complete whatever
//!    *new* task "1" the cleared session had since created.
//!
//! ### The tension this does NOT resolve
//!
//! "Session destroy and reset" and "live sidecar workers survive" (#1631) are
//! not obviously the same posture, and nothing here decides between them.
//! The decision on record is scoped to the board, so that is all this
//! implements: the workers keep running, keep their lanes, and keep reporting
//! on them — they simply have no board to report *onto* any more. Whether a
//! worker with no board should still be running at all is a maintainer
//! question, filed rather than answered.

use super::*;

/// Reset the lead session to its seq-0 state: the LLM history becomes the
/// system prompt alone, the whole task board is wiped (live, persisted, and
/// sealed against pre-clear workers — see the module docs), the deck pane
/// blanks ([`Inbound::SessionReset`]), surviving sidecar workers are named in
/// the blanked pane, the dashboard returns to waiting
/// ([`AgentStatus::WaitingInput`] — also the journal's settle marker), and the
/// durable boundary snapshot is rewritten so a resume continues from the
/// cleared state, not from before it.
///
/// The board is cleared BEFORE [`Inbound::SessionReset`] goes out, so no
/// snapshot of the doomed board can be taken between the two. No
/// `AgentEvent::TaskUpdate` is emitted for the empty board and none is
/// wanted: `SessionReset` already replaces the lead's whole
/// `stella_tui::SessionModel`, task list included, and an empty snapshot
/// behind it would only add a "tasks 0/0" row to a transcript the user just
/// emptied.
///
/// Emitting the surviving-worker note *between* the blank and the status flip
/// is load-bearing twice over: after the blank so the note is not wiped by it,
/// and before the status flip because the note is an `AgentEvent::Text`, which
/// the deck's fold reads as the lead running. The trailing `WaitingInput` is
/// what puts the lead back to idle.
pub(super) fn reset_lead(
    messages: &mut Vec<CompletionMessage>,
    system_prompt: &str,
    sidecar_dir: &std::path::Path,
    subs: &mut SubSessions,
    registry: &ToolRegistry,
    store: Option<(&Store, &str)>,
    in_tx: &UnboundedSender<Inbound>,
) {
    let live_lanes = subs.live_lanes();
    messages.clear();
    messages.push(CompletionMessage::system(system_prompt.to_string()));
    let mirror_cleared = clear_task_board(subs, registry, store);
    let _ = in_tx.send(Inbound::SessionReset {
        agent: LEAD.to_string(),
    });
    // After the blank, for the same reason the note is: an error emitted
    // before `SessionReset` would be wiped by it, and a clear that silently
    // left a copy of the board behind is exactly what the user must hear.
    if !mirror_cleared {
        let _ = in_tx.send(Inbound::Event {
            agent: LEAD.to_string(),
            event: AgentEvent::Error {
                message: "store write failed — the task board was cleared here, but its \
                          persisted mirror was not"
                    .to_string(),
                retryable: true,
            },
        });
    }
    if let Some(note) = surviving_workers_note(&live_lanes) {
        let _ = in_tx.send(chrome_note(note));
    }
    let _ = in_tx.send(Inbound::Status {
        agent: LEAD.to_string(),
        status: AgentStatus::WaitingInput,
    });
    let _ = crate::session_persist::snapshot_history(sidecar_dir, messages);
}

/// Destroy the task board in all three places it exists: the live registry
/// board, the `tasks` mirror for this session, and the generation watermark
/// that decides which workers are still allowed to write to it.
///
/// Sealing first is deliberate. `seal_task_board` reads `next_generation`,
/// which only [`SubSessions::started`] moves, and the driver is
/// single-threaded across both — but stating the order here means a future
/// caller cannot get a board that is empty while stale workers still count as
/// current.
///
/// A failed mirror delete is reported on the lead's lane rather than
/// swallowed: the user asked for the board to be gone, and a silent partial
/// clear leaves a copy behind that nothing else would ever mention.
fn clear_task_board(
    subs: &mut SubSessions,
    registry: &ToolRegistry,
    store: Option<(&Store, &str)>,
) -> bool {
    subs.seal_task_board();
    registry
        .task_board()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
    match store {
        Some((store, session_id)) => store.clear_session_tasks(session_id).is_ok(),
        None => true,
    }
}

/// Close out a `sub:<task-id>` worker against the task board — the delegation
/// loop closing without the lead's involvement.
///
/// A worker that finished successfully completes its board task; a failed or
/// stopped worker leaves the task in progress, because the board must not
/// claim done what wasn't (the inbox notification names a failure, and a stop
/// was the user's own act).
///
/// **Unless the worker predates a `/clear`** (#1692). Then there is no board
/// row of its to close: the board it was assigned from was destroyed, and its
/// task id now addresses whatever task the cleared session numbered "1" since.
/// Such a report is quarantined — announced on the lead's lane as the
/// pre-clear result it is, and written to neither the live board nor the
/// mirror — never folded back in.
///
/// Lives here rather than inline in the driver's `SupervisorMsg::Ended` arm:
/// the arm is in a god file closed to growth, and which board a worker may
/// write to is `/clear`'s own business.
pub(super) fn settle_worker_task(
    lane: &str,
    generation: u64,
    end: &subsession::WorkerEnd,
    subs: &SubSessions,
    registry: &ToolRegistry,
    mirror: Option<BoardMirror<'_>>,
    in_tx: &UnboundedSender<Inbound>,
) {
    let Some(task_id) = lane.strip_prefix("sub:") else {
        return;
    };
    if subs.predates_task_board(generation) {
        let _ = in_tx.send(chrome_note(stale_worker_note(task_id, end)));
        return;
    }
    let board = registry.task_board();
    let items: Vec<TaskItem> = {
        let mut guard = board.lock().unwrap_or_else(|p| p.into_inner());
        if matches!(end, subsession::WorkerEnd::Done) {
            let _ = guard.set_status(task_id, stella_protocol::TaskStatus::Completed);
        }
        guard.items().to_vec()
    };
    let _ = in_tx.send(Inbound::Event {
        agent: LEAD.to_string(),
        event: AgentEvent::TaskUpdate {
            tasks: items.clone(),
        },
    });
    if let Some(mirror) = mirror {
        let _ = mirror.store.record_task_board(
            mirror.execution_id,
            Some(mirror.session_id),
            &items,
            now_ms(),
        );
    }
}

/// Where a settled worker's board snapshot is mirrored durably: the store,
/// the session the rows are keyed to, and the execution that produced them.
///
/// One borrowed struct rather than three parameters because the three are
/// only ever meaningful together — a store with no execution has nothing to
/// key a row on — and [`Self::of`] makes "any of them missing means no
/// mirror" a single expression the caller cannot get half right.
pub(super) struct BoardMirror<'a> {
    pub(super) store: &'a Store,
    pub(super) session_id: &'a str,
    pub(super) execution_id: i64,
}

impl<'a> BoardMirror<'a> {
    pub(super) fn of(
        store: Option<&'a Arc<Store>>,
        session_id: &'a str,
        execution_id: Option<i64>,
    ) -> Option<Self> {
        Some(Self {
            store: store?,
            session_id,
            execution_id: execution_id?,
        })
    }
}

/// What the user is told when a pre-clear worker reports in. It has to say
/// three things or it is worse than silence: which task it was, that the
/// result is real (the work happened), and that the board deliberately did
/// not move — otherwise a user watching the Tasks tab reads the missing row
/// as a dropped result.
fn stale_worker_note(task_id: &str, end: &subsession::WorkerEnd) -> String {
    let outcome = match end {
        subsession::WorkerEnd::Done => "finished",
        _ => "ended without finishing",
    };
    format!(
        "worker for task #{task_id} {outcome}, from before this conversation was cleared — \
         its output stayed on its own lane and the task board was not changed (the board it \
         was assigned from no longer exists)."
    )
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
        let (registry, mut subs) = board_fixture(&[]);

        reset_lead(
            &mut messages,
            "SYSTEM",
            &dir,
            &mut subs,
            &registry,
            None,
            &in_tx,
        );

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
        let (registry, mut subs) = board_fixture(&[]);
        for lane in &live {
            subs.started_for_test(lane);
        }

        reset_lead(
            &mut messages,
            "SYSTEM",
            &dir,
            &mut subs,
            &registry,
            None,
            &in_tx,
        );

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

    /// **Witness for #1692, half one.** The maintainer's call is that
    /// `/clear` is a session destroy-and-reset, so the board it destroys is
    /// the WHOLE board — not the lead's rows, not the open ones. Pinned as
    /// the registry-level state it actually is: the deck pane blanking is a
    /// view, and blanking a view over a board that is still populated is the
    /// bug this fixes.
    #[test]
    fn clearing_empties_the_entire_task_board() {
        let dir = scratch("board");
        let (registry, mut subs) = board_fixture(&["lead work", "delegated work", "third"]);
        registry
            .task_board()
            .lock()
            .unwrap()
            .assign("2", "sub:2")
            .expect("task 2 is delegated");
        let mut messages = vec![CompletionMessage::system("SYSTEM".to_string())];
        let (in_tx, _in_rx) = mpsc::unbounded_channel::<Inbound>();

        reset_lead(
            &mut messages,
            "SYSTEM",
            &dir,
            &mut subs,
            &registry,
            None,
            &in_tx,
        );

        assert!(
            registry.task_board().lock().unwrap().is_empty(),
            "every row goes, including the delegated one: {:?}",
            registry.task_board().lock().unwrap().items()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Witness for #1692, half two — the resurrection.** A `sub:` worker
    /// that was already running at clear-time finishes afterwards. Its
    /// closeout must not reach the board, and the reason it must not is
    /// concrete rather than tidy: board ids restart at "1" after a clear, so
    /// the pre-clear worker for task "1" would complete an unrelated task
    /// the cleared session created since.
    ///
    /// Three things are pinned, because each is a way the drop could be a
    /// different bug: the new task stays untouched, NO `TaskUpdate` is
    /// emitted (an empty-board snapshot would flip the Tasks tab as loudly
    /// as a wrong one), and the user is told the result was quarantined
    /// rather than lost.
    #[test]
    fn a_worker_from_before_the_clear_cannot_resurrect_a_row() {
        let dir = scratch("stale");
        let (registry, mut subs) = board_fixture(&["the pre-clear task"]);
        let stale = subs.started_for_test("sub:1");
        registry
            .task_board()
            .lock()
            .unwrap()
            .assign("1", "sub:1")
            .expect("task 1 is delegated to the worker that outlives the clear");
        let mut messages = vec![CompletionMessage::system("SYSTEM".to_string())];
        let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Inbound>();

        reset_lead(
            &mut messages,
            "SYSTEM",
            &dir,
            &mut subs,
            &registry,
            None,
            &in_tx,
        );
        // The cleared session goes on to plan again — and ordinal ids
        // restart, so this brand-new task is also "1".
        registry
            .task_board()
            .lock()
            .unwrap()
            .create("a task the user created after clearing", None);
        while in_rx.try_recv().is_ok() {}

        settle_worker_task(
            "sub:1",
            stale,
            &subsession::WorkerEnd::Done,
            &subs,
            &registry,
            None,
            &in_tx,
        );

        let items = registry.task_board().lock().unwrap().items().to_vec();
        assert_eq!(
            items.len(),
            1,
            "the post-clear board is untouched: {items:?}"
        );
        assert_eq!(items[0].subject, "a task the user created after clearing");
        assert_eq!(
            items[0].status,
            stella_protocol::TaskStatus::Pending,
            "the pre-clear worker must not complete the task that reused its id"
        );

        let sent: Vec<Inbound> = std::iter::from_fn(|| in_rx.try_recv().ok()).collect();
        assert!(
            !sent.iter().any(|i| matches!(
                i,
                Inbound::Event {
                    event: AgentEvent::TaskUpdate { .. },
                    ..
                }
            )),
            "no board snapshot may ride a cleared board: {sent:?}"
        );
        let [
            Inbound::Event {
                agent,
                event: AgentEvent::Text { delta },
            },
        ] = &sent[..]
        else {
            panic!("expected exactly the quarantine note, got {sent:?}");
        };
        assert_eq!(agent, LEAD);
        assert!(
            delta.starts_with(stella_tui::NOTICE_MARKER),
            "the program speaking, not the model: {delta}"
        );
        assert!(delta.contains("#1"), "name the task: {delta}");
        assert!(
            delta.contains("cleared"),
            "say why the board did not move: {delta}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The seal is a watermark, not an off switch: a worker spawned AFTER
    /// the clear closes its task exactly as it always did. Without this the
    /// fix above could ship as "delegation never completes anything again"
    /// and both other tests would still pass.
    #[test]
    fn a_worker_spawned_after_the_clear_still_completes_its_task() {
        let dir = scratch("fresh");
        let (registry, mut subs) = board_fixture(&["pre-clear"]);
        let mut messages = vec![CompletionMessage::system("SYSTEM".to_string())];
        let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Inbound>();
        reset_lead(
            &mut messages,
            "SYSTEM",
            &dir,
            &mut subs,
            &registry,
            None,
            &in_tx,
        );
        registry
            .task_board()
            .lock()
            .unwrap()
            .create("post-clear work", None);
        let fresh = subs.started_for_test("sub:1");
        registry
            .task_board()
            .lock()
            .unwrap()
            .assign("1", "sub:1")
            .unwrap();
        while in_rx.try_recv().is_ok() {}

        settle_worker_task(
            "sub:1",
            fresh,
            &subsession::WorkerEnd::Done,
            &subs,
            &registry,
            None,
            &in_tx,
        );

        assert_eq!(
            registry.task_board().lock().unwrap().items()[0].status,
            stella_protocol::TaskStatus::Completed
        );
        let sent: Vec<Inbound> = std::iter::from_fn(|| in_rx.try_recv().ok()).collect();
        assert!(
            sent.iter().any(|i| matches!(
                i,
                Inbound::Event {
                    event: AgentEvent::TaskUpdate { .. },
                    ..
                }
            )),
            "a current worker still publishes its board: {sent:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failed pre-clear worker is quarantined the same way, and the note
    /// says so — reporting a failure as "finished" would be the mirror image
    /// of the bug being fixed.
    #[test]
    fn the_quarantine_note_distinguishes_a_failure_from_a_finish() {
        let done = stale_worker_note("3", &subsession::WorkerEnd::Done);
        assert!(done.contains("finished"), "{done}");
        let failed = stale_worker_note("3", &subsession::WorkerEnd::Failed("boom".into()));
        assert!(failed.contains("without finishing"), "{failed}");
        let stopped = stale_worker_note("3", &subsession::WorkerEnd::Stopped);
        assert!(stopped.contains("without finishing"), "{stopped}");
    }

    /// A registry whose board carries `subjects`, plus the sub-session
    /// bookkeeping that owns the generation watermark.
    fn board_fixture(subjects: &[&str]) -> (ToolRegistry, SubSessions) {
        let registry = ToolRegistry::new(
            std::env::temp_dir(),
            stella_tools::registry::RegistryOptions::default(),
        );
        {
            let board = registry.task_board();
            let mut guard = board.lock().unwrap();
            for subject in subjects {
                guard.create(*subject, None);
            }
        }
        (registry, SubSessions::new())
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("stella-session-clear-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
