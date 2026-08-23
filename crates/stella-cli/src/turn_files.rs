// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `AgentEvent::FileChange` for a turn the pipeline never delivered (#3413).
//!
//! # The hole this closes
//!
//! Until this existed, `FileChange` had exactly one producer in the workspace:
//! `Pipeline::deliver_winner`, emitting one event per adopted change when a
//! winning candidate lands in the real tree. Every other turn — plain `stella`
//! chat, `stella run` on the engine path, the deck's lead turn, a fleet
//! worker, a subsession — reported that it had changed nothing, because
//! adoption is the only place anything measured. The TUI Files tab,
//! `stella export` and the audit log were empty for all of them, which reads
//! as "this turn touched no files" rather than "this surface never reports".
//!
//! It was not a regression anyone introduced. The 12-tool purge (#3244)
//! deleted the file-writing built-ins and the file-CRUD ledger that emitted
//! these; the docs simply kept naming a producer that no longer existed.
//!
//! # Why the answer is a measurement and not a tool hook
//!
//! The file built-ins are back — `ToolRegistry::new` registers `write_file`,
//! `edit_file` and `delete_file` alongside `bash` — so the obvious repair,
//! having the tools emit, is now *available*. It is still the wrong answer,
//! for the reason it was always the wrong answer rather than merely an
//! impossible one:
//!
//! - **Inferring from tool inputs is the known defect.** A wrapper that
//!   synthesized these events from the arguments of four hard-coded tool names
//!   is exactly what #2290 was: files edited in bulk, or by a worker lane,
//!   rendered as `+0 -0`. The counts contract exists because of it — `added`
//!   and `removed` come from a real measurement or they are not sent, and a
//!   tool's arguments are a request, not a measurement of what landed.
//! - **A tool hook could never be complete anyway.** `bash` mutates the tree
//!   without naming a path in any schema the engine reads, and so do MCP
//!   servers and custom script tools. A hook on the four file tools would
//!   report a *subset* of the turn's changes while looking exhaustive, which
//!   is worse than the empty ledger this module replaced.
//!
//! So the producer is the work journal's turn-boundary tree snapshot, which
//! measures with git's own `--numstat`. One reading, at the turn boundary,
//! after the model has answered.
//!
//! # What these events do and do not claim
//!
//! **Observability, never evidence** — the same contract `FileChange` has
//! always carried, and it has teeth here. A tree measurement answers *what
//! changed during this turn*, not *what the agent changed*: a user editing a
//! file in another window mid-turn appears in this stream indistinguishable
//! from the agent's own writes. Nothing may found a claim on it, and #2873
//! already removed every decision that read a count of these events. The
//! authority on what the agent did is the pipeline's adoption, which measures
//! a candidate against a sealed baseline and can tell the two apart.
//!
//! It lives beside `agent.rs` rather than inside it because `agent.rs` is a
//! grandfathered god file, closed to growth (AGENTS.md § "God files — plan
//! around them, never into them").

use std::sync::Arc;

use stella_core::EventSender;
use stella_core::TurnOutcome;
use stella_diag::{Cx, Fields, Level, Record};
use stella_protocol::event::{AgentEvent, FileChangeKind};
use stella_store::work_journal::{JournalChange, JournalChangeKind};
use stella_store::{FileTouchRow, Store};
use stella_tools::own_change::{OwnChange, OwnChangeKind};

use crate::config::Config;
use crate::durability::{SessionDurability, UnmeasurableReason, WorktreeSnapshot};

/// This module's `module_path!()`, so its records filter under one target.
const DIAG_TARGET: &str = "stella::turn_files";

/// This turn's per-call work-tree measurement, handed to the registry so a
/// mutating tool call that ran alone reports the change *it* made (#4175).
///
/// Everything about *which* calls are measured, why consecutive readings
/// partition a turn rather than double-counting it, and why the producer is a
/// tree reading rather than a hook on the file tools, lives on the trait —
/// see [`stella_tools::call_measure`]. This half is only the binding: the
/// session's journal, this turn's stream, this turn's execution row.
///
/// Built per turn and dropped when the registry detaches it, because two of
/// its three fields are turn-scoped. Holding it past the turn would publish
/// the next turn's changes onto a closed channel and a finished execution —
/// and, worse, keep that channel open (#960).
pub(crate) struct TurnCallMeasure {
    durability: SessionDurability,
    tx: EventSender,
    execution: Option<(Arc<Store>, i64)>,
}

impl TurnCallMeasure {
    pub(crate) fn new(
        durability: SessionDurability,
        tx: EventSender,
        execution: Option<(Arc<Store>, i64)>,
    ) -> Self {
        Self {
            durability,
            tx,
            execution,
        }
    }
}

impl stella_tools::call_measure::CallMeasure for TurnCallMeasure {
    /// The same measurement the turn boundary takes, taken now.
    ///
    /// `snapshot_worktree` consumes what it reports — it commits the tree onto
    /// the session's snapshot ref and diffs against the previous commit — so
    /// this reading and the boundary's partition the turn between them. That
    /// is the property the counters downstream depend on, and it is why this
    /// calls the boundary's own function rather than a per-call variant of it.
    fn measure_and_publish(&self, own: &[OwnChange]) {
        let measured =
            emit_measured_tree_changes(&self.durability, &self.tx, self.execution.as_ref());
        emit_own_changes(&self.tx, self.execution.as_ref(), own, &measured);
    }
}

/// Publish a call's own reading for every path the tree reading left out.
///
/// The tree reading is git's, and git does not see a gitignored path or a
/// workspace with no journal bound — which is how a `write_file` into
/// `.stella/agents/…` rendered as `wrote 9214 bytes` with no diff under it.
/// The tool's own diff (`stella_tools::own_change`) fills exactly that gap, and
/// only that gap: a path the snapshot reported keeps the snapshot's event, so
/// the Files tab still counts each change once.
///
/// Both projections are written, as for a measured change: the event the
/// transcript folds, and the `files_touched` row the reflection loop reads.
fn emit_own_changes(
    tx: &EventSender,
    execution: Option<&(Arc<Store>, i64)>,
    own: &[OwnChange],
    measured: &[String],
) {
    let unmeasured: Vec<&OwnChange> = own
        .iter()
        .filter(|change| !measured.contains(&change.path))
        .collect();
    if unmeasured.is_empty() {
        return;
    }
    if let Some((store, execution_id)) = execution {
        let rows: Vec<FileTouchRow> = unmeasured.iter().map(|c| own_touch_row(c)).collect();
        let _ = store.record_files_touched(*execution_id, &rows);
    }
    for change in unmeasured {
        let _ = tx.send(stella_tools::own_change::file_change(change.clone()));
    }
}

/// The durable row for a change the call itself read, in the same CRUD
/// alphabet as [`file_touch_row`] and with its provenance named.
fn own_touch_row(change: &OwnChange) -> FileTouchRow {
    FileTouchRow {
        path: change.path.clone(),
        ops: match change.kind {
            OwnChangeKind::Created => "C",
            OwnChangeKind::Modified => "U",
            OwnChangeKind::Deleted => "D",
        }
        .to_string(),
        lines_added: u64::from(change.added),
        lines_removed: u64::from(change.removed),
        events_json: serde_json::json!([{
            "event": "measured",
            "reason": "the call's own before/after reading",
            "lines_added": change.added,
            "lines_removed": change.removed,
        }])
        .to_string(),
    }
}

/// Everything the owner of a turn owes the **registry** when it opens the
/// turn's event stream — the opening bookend to [`close_turn_boundary`].
///
/// Both debts ride one channel and are dropped together by
/// `ToolRegistry::detach_event_stream`, so they are taken on together here for
/// the same reason the two closing debts were folded into one function: one of
/// the pair is loud when forgotten and the other is silent, and only the loud
/// one ever gets noticed.
///
/// - **Registry-born events** (task board, sub-agent lifecycle). Forgetting
///   these is loud — a sub-agent's whole lifecycle vanishes from the
///   transcript.
/// - **This turn's per-call work-tree measurement** (#4175). Forgetting it is
///   silent: every solo mutating call still renders a result row, the row
///   simply carries no diff and no `+N −M`, which is indistinguishable from a
///   call that changed nothing. That is the exact shape #4155 was reported as,
///   and the exact shape #4160 had to repair once already at the turn
///   boundary.
pub(crate) fn open_turn_streams(
    registry: &stella_tools::registry::ToolRegistry,
    cfg: &Config,
    tx: &EventSender,
    execution: Option<&(Arc<Store>, i64)>,
) {
    registry.attach_events(tx.clone());
    registry.attach_call_measure(Arc::new(TurnCallMeasure::new(
        cfg.durability.clone(),
        tx.clone(),
        execution.cloned(),
    )));
}

/// [`open_turn_streams`] for a driver holding the raw channel sender rather
/// than an [`EventSender`] — the Command Deck's lead turn, exactly as
/// [`close_turn_boundary_raw`] serves it at the other end of the turn.
pub(crate) fn open_turn_streams_raw(
    registry: &stella_tools::registry::ToolRegistry,
    cfg: &Config,
    tx: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    execution: Option<&(Arc<Store>, i64)>,
) {
    open_turn_streams(registry, cfg, &EventSender::new(tx.clone()), execution);
}

/// Everything the owner of a turn owes its event stream at the boundary, in
/// the one order that is correct.
///
/// The two debts are separate facts that have to be paid together, because
/// they are paid to the same stream in the same instant and only one ordering
/// is right: the tree measurement first — these are the turn's *own* events
/// and a consumer folding the stream must see them inside the turn they
/// describe — and then the run's terminator, which is the last thing a run
/// says (#3379).
///
/// **It is one function because it was two, and a driver forgot one of them.**
/// `emit_shared_tree_changes` had exactly one caller, `agent::run_turn`, so
/// the Command Deck — the default interactive shell on a TTY, and therefore
/// the surface almost every human actually watches — emitted no
/// [`AgentEvent::FileChange`] at all. Its Files tab (`/files`, `/diff`) read
/// `no files touched yet` for the whole of every session however many files
/// the turn created, edited or deleted. The terminator was never forgotten,
/// because a run that omits it visibly hangs every consumer; the measurement
/// was, because a missing measurement renders as an honest-looking empty
/// ledger. Folding the silent debt into the loud one is what stops the next
/// driver making the same omission — the same repair, and the same reasoning,
/// as `durability`'s `#2177` turn-boundary fence.
///
/// Call **before** the turn's event channel closes. Best-effort and silent
/// throughout: a turn that ended is not made less ended by an unmeasurable
/// tree.
pub(crate) fn close_turn_boundary(
    cfg: &Config,
    registry: &stella_tools::ToolRegistry,
    tx: &EventSender,
    execution: Option<&(Arc<Store>, i64)>,
    outcome: &TurnOutcome,
) {
    note_stale_lane(registry, emit_shared_tree_changes(cfg, tx, execution));
    crate::agent::persistence::emit_run_complete_for_turn(tx, &cfg.model_id, outcome);
}

/// [`close_turn_boundary`] for a driver holding the raw channel sender rather
/// than an [`EventSender`] — the Command Deck's lead turn, which builds its
/// channel with `mpsc::unbounded_channel` and hands clones to the forwarder.
///
/// The sender it wraps is a **temporary**, dropped when this call returns. It
/// has to be: the deck ends its turn with `close_turn_stream`, which closes
/// the channel by dropping the last sender, and a clone left alive in the
/// driver's scope would leave the forwarder's `recv()` pending forever and
/// wedge the turn future after the deck had already painted the turn done
/// (#2290).
///
/// This replaces `persistence::emit_run_complete_raw`, which paid only the
/// terminator half of the boundary — deliberately, so that the deck cannot
/// terminate a turn without also measuring what it changed.
pub(crate) fn close_turn_boundary_raw(
    cfg: &Config,
    registry: &stella_tools::ToolRegistry,
    tx: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    execution: Option<&(Arc<Store>, i64)>,
    outcome: &TurnOutcome,
) {
    close_turn_boundary(
        cfg,
        registry,
        &EventSender::new(tx.clone()),
        execution,
        outcome,
    );
}

/// How many changed files make an untouched board worth a record.
///
/// One file is the ordinary shape of a turn that is genuinely still on the
/// card it says it is on. Three is a guess, and it is meant to be: the point
/// of the record is to find out what the real distribution looks like before
/// anything is injected into the model's context.
const STALE_LANE_FILES: usize = 3;

/// Record a turn that ended with the lead's task lane still occupied while it
/// changed files (#4153).
///
/// # What this does not claim
///
/// Not a defect signal. A turn that ends mid-card having edited three files is
/// the ordinary shape of work in progress, and this fires on it. The thing
/// worth knowing is the *rate*: #4152 closed the half of the divergence that
/// has a correction point — `task_start` refuses a second unowned lane — and
/// left the half that has none, where the agent never calls `task_start` at
/// all and just starts editing. "These edits belong to a later step" is not
/// decidable from the board, so the only honest first move is to measure how
/// often a turn ends in that shape at all. `agent.turn_complete` is emitted
/// once per turn, so the ratio is a join, not a second counter.
///
/// # Which drivers it covers
///
/// The two that close a turn through [`close_turn_boundary`]: `run_turn`
/// (`stella run`, the plain REPL) and the deck's lead turn, which is the
/// surface the divergence was reported on. The goal arcs and `run_resume`
/// measure per round through [`emit_shared_tree_changes_raw`] and pay one
/// terminator at the end (#4159), so a lane observation there is a question
/// about a round rather than about a turn — a different reading, and one to
/// take only if the numbers this produces say it is worth taking.
///
/// # Why counts only
///
/// A task id and a subject are model output, and `stella-diag`'s field types
/// refuse both — deliberately, and the constraint is right here: a record
/// naming the card the agent walked past would put model-authored text into
/// the diagnostic plane for a signal that only needs an integer.
fn note_stale_lane(registry: &stella_tools::ToolRegistry, files_changed: usize) {
    let board = registry.task_board();
    let board = board
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(open_tasks) = stale_lane_open_tasks(&board, files_changed) else {
        return;
    };
    crate::diag_boot::dx().emit(Record::new(
        Level::Debug,
        "agent.task.stale_lane",
        DIAG_TARGET,
        Cx::EMPTY,
        Fields::new()
            .with("files_changed", files_changed as u64)
            .with("open_tasks", open_tasks as u64)
            .with("threshold", STALE_LANE_FILES as u64),
    ));
}

/// The decision [`note_stale_lane`] makes, as a total function of the board
/// and the count — `Some(open_tasks)` when the record is owed.
///
/// Split out because the emit half needs a `ToolRegistry`, a lock and a
/// process-wide `Dx`, and none of those is the part that can be wrong. The
/// part that can be wrong is *which* boards count, and it has two edges a
/// reader would not guess: a delegated card is owned and does not occupy the
/// lead's lane, and `open_tasks` counts pending cards too, because a plan the
/// agent stopped walking is exactly the shape worth telling from a one-card
/// board.
fn stale_lane_open_tasks(
    board: &stella_core::tasks::TaskBoard,
    files_changed: usize,
) -> Option<usize> {
    if files_changed < STALE_LANE_FILES {
        return None;
    }
    board.unowned_in_progress()?;
    Some(
        board
            .items()
            .iter()
            .filter(|task| task.status.is_open())
            .count(),
    )
}

/// Measure the shared work tree once, then write both of this turn's file
/// projections from that one reading: the [`AgentEvent::FileChange`] stream
/// and the durable `files_touched` rows.
///
/// Call **before** the turn's event channel closes — these are the turn's
/// events, and a consumer folding the stream must see them inside the turn
/// they describe.
///
/// The measurement is taken exactly once and both projections are derived from
/// it. Snapshotting twice would not merely cost a second `--numstat`: the
/// journal's snapshot advances its own baseline, so the second reading would
/// report an unchanged tree and the two projections would disagree about the
/// same turn.
///
/// Best-effort and silent, like every other turn-boundary write: a turn that
/// ended is not made less ended by an unmeasurable tree. A send failure means
/// the renderer is already gone, which is not this function's problem to
/// report.
///
/// Returns **how many** files the turn changed. The boundary sweep has no
/// call's own reading to reconcile against, so it does not need the paths —
/// but the count is the one number a turn-boundary observation can want
/// without re-reading the tree, and re-reading is precisely what the
/// baseline-advancing snapshot above forbids.
pub(crate) fn emit_shared_tree_changes(
    cfg: &Config,
    tx: &EventSender,
    execution: Option<&(Arc<Store>, i64)>,
) -> usize {
    emit_measured_tree_changes(&cfg.durability, tx, execution).len()
}

/// [`emit_shared_tree_changes`] for a driver holding the raw channel sender
/// rather than an [`EventSender`], at a boundary that is **not** the run's end
/// (#4159).
///
/// The measurement half of [`close_turn_boundary_raw`] without the terminator
/// beside it, and that separation is the opposite of the one
/// `persistence::emit_run_complete_raw` made before it was deleted. That
/// helper let a driver pay the *loud* debt alone — a terminated run with an
/// empty file ledger, which renders as an honest-looking "this turn changed
/// nothing". This one pays only the silent debt, which is what a multi-turn
/// driver actually needs: `stella goal` and `stella daemon resume` drive
/// several turns over one stream and must emit exactly one terminator for the
/// whole run (`emit_run_complete`'s own doc), so swapping in
/// `close_turn_boundary` at each of their boundaries would end the run at the
/// first one.
///
/// The sender it wraps is a **temporary**, dropped when this call returns, for
/// [`close_turn_boundary_raw`]'s reason: a clone left alive in the driver's
/// scope keeps the channel open and wedges the renderer that is waiting for it
/// to close (#960, #2290).
///
/// Call it **exactly once per boundary**. The snapshot consumes what it
/// reports — `snapshot_worktree` commits the tree onto the session's snapshot
/// ref and diffs against the previous commit — so a second caller at the same
/// boundary reports an unchanged tree.
pub(crate) fn emit_shared_tree_changes_raw(
    cfg: &Config,
    tx: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    execution: Option<&(Arc<Store>, i64)>,
) {
    emit_shared_tree_changes(cfg, &EventSender::new(tx.clone()), execution);
}

/// [`emit_shared_tree_changes`] over the durability handle alone.
///
/// The whole of `cfg` this ever needed was `cfg.durability`, and taking the
/// narrower thing is what lets the per-call measurer (#4175) hold one cheaply
/// for the length of a turn instead of cloning a `Config`. Both granularities
/// then run the *same* function, which is the load-bearing half of the
/// no-double-counting argument in `stella_tools::call_measure`: there is one
/// measurement, called more or less often, not two that have to agree.
///
/// Returns the paths it published, so a per-call reading the tool supplied
/// itself ([`emit_own_changes`]) can be published for the rest and no path
/// gets two events.
pub(crate) fn emit_measured_tree_changes(
    durability: &SessionDurability,
    tx: &EventSender,
    execution: Option<&(Arc<Store>, i64)>,
) -> Vec<String> {
    let measured = match durability.snapshot_worktree() {
        WorktreeSnapshot::Measured(changes) => changes,
        // Nothing to measure and nothing to report: the ordinary state of a
        // workspace whose durable record could not be opened, already
        // reported once by `durability::bind_session`.
        WorktreeSnapshot::NotBound => return Vec::new(),
        WorktreeSnapshot::Unmeasurable(error) => {
            // The turn stays best-effort — a turn that ended is not made less
            // ended by an unmeasurable tree — but the emptiness downstream is
            // now a recorded fact rather than a silence indistinguishable
            // from a clean tree (#4170).
            //
            // Through the process handle rather than a threaded `&Dx`
            // deliberately: the per-call measurer reaches this from inside the
            // tool registry, which is exactly the "spawned beyond reach of
            // `main`'s handle" case `diag_boot::dx` documents.
            crate::diag_boot::dx().emit(Record::new(
                Level::Warn,
                "agent.files.unmeasurable",
                DIAG_TARGET,
                Cx::EMPTY,
                Fields::new().with("reason", UnmeasurableReason::classify(&error)),
            ));
            return Vec::new();
        }
    };
    if measured.is_empty() {
        return Vec::new();
    }
    if let Some((store, execution_id)) = execution {
        let rows: Vec<FileTouchRow> = measured.iter().map(file_touch_row).collect();
        // Best-effort for the same reason `AdoptionLedger::record` is: this is
        // observability, not evidence (#2882). A telemetry write must never
        // fail a turn whose bytes are already on disk.
        let _ = store.record_files_touched(*execution_id, &rows);
    }
    let paths = measured.iter().map(|c| c.path.clone()).collect();
    for change in measured {
        let _ = tx.send(file_change(change));
    }
    paths
}

/// One measured delta as a durable row.
///
/// The durable half of #3413. That change gave the engine path a `FileChange`
/// producer but no `files_touched` producer, leaving adoption as the table's
/// only writer — so every direct-edit turn recorded zero touched files however
/// many it wrote. `Store::finalize_execution_reflection` reads exactly this
/// table for its `wrote_files` flag, so the empty table also told the
/// reflection loop that a turn which edited dozens of files had written none.
fn file_touch_row(change: &JournalChange) -> FileTouchRow {
    FileTouchRow {
        path: change.path.clone(),
        ops: ops_letter(change.kind).to_string(),
        lines_added: u64::from(change.added),
        lines_removed: u64::from(change.removed),
        events_json: serde_json::json!([{
            "event": "measured",
            "reason": "turn-boundary work-tree measurement",
            "lines_added": change.added,
            "lines_removed": change.removed,
        }])
        .to_string(),
    }
}

/// The `ops` alphabet is CRUD letters, fixed by the reader rather than by
/// preference — `Store::execution_rollup` and `finalize_execution_reflection`
/// both match `ops LIKE '%C%' OR '%U%' OR '%D%'`, so a row spelled any other
/// way is a row those queries cannot see. Same alphabet as the adoption
/// ledger's `ops_letter` (`candidate_ws/adoption_ledger.rs`), deliberately —
/// named in prose rather than linked because that module is private, and an
/// intra-doc link to it does not resolve.
fn ops_letter(kind: JournalChangeKind) -> &'static str {
    match kind {
        JournalChangeKind::Created => "C",
        JournalChangeKind::Modified => "U",
        JournalChangeKind::Deleted => "D",
    }
}

/// Map one measured delta onto the wire event.
///
/// [`FileChangeKind::Read`] is deliberately unreachable here: a tree snapshot
/// cannot observe a read, and the variant is replay-only (see its doc).
fn file_change(change: JournalChange) -> AgentEvent {
    AgentEvent::FileChange {
        path: change.path,
        kind: match change.kind {
            JournalChangeKind::Created => FileChangeKind::Created,
            JournalChangeKind::Modified => FileChangeKind::Modified,
            JournalChangeKind::Deleted => FileChangeKind::Deleted,
        },
        added: change.added,
        removed: change.removed,
        diff: change.diff,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Witness.** Every driver that owns a turn measures what that turn
    /// changed, not just that it ended.
    ///
    /// [`emit_shared_tree_changes`] had exactly ONE caller, `agent::run_turn`.
    /// The Command Deck — the default interactive shell on a TTY, and so the
    /// surface a human actually watches — drives its lead turn through
    /// `run_lead_turn`, which paid only the run terminator. It therefore
    /// emitted no [`AgentEvent::FileChange`] for the whole of any session, and
    /// its Files tab (`/files`, `/diff`) read `no files touched yet` however
    /// many files the turn created, edited or deleted.
    ///
    /// The asymmetry is the point, and it is why this needs a fence rather
    /// than trust: a driver that drops the *terminator* hangs every consumer
    /// immediately and is found in minutes, while a driver that drops the
    /// *measurement* renders an empty ledger that is indistinguishable from an
    /// honest one. Only the loud debt was ever noticed. Folding both into
    /// [`close_turn_boundary`] is the structural half of the repair; this is
    /// the half that keeps the next driver from unfolding them.
    ///
    /// A source fence rather than an end-to-end run, and the choice is the
    /// same one `durability`'s `#2177` fence names: the difference lives
    /// inside ~400-line async driver functions needing a provider, a
    /// journal-bound session and a file-touching turn to reach, and what
    /// actually decays is the *call site* — exactly as it decayed here. The
    /// measurement itself is covered end-to-end by
    /// `durability`'s baseline witnesses and by `stella-store`'s
    /// `snapshot_worktree` tests.
    #[test]
    fn every_turn_owner_pays_both_halves_of_the_boundary() {
        // Built rather than written out, so this file is not its own match.
        let closing = format!("close_turn_{}", "boundary");
        let measuring = format!("emit_shared_tree_{}", "changes");
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for (file, driver, seam) in [
            // The raw engine turn: `stella run`, the plain REPL.
            ("agent.rs", "run_turn", &closing),
            // The interactive deck's lead turn — the driver that had the hole.
            ("command_deck.rs", "run_lead_turn", &closing),
            // The three drivers of #4159, which own several turns over one
            // stream and therefore pay the two debts at different points: the
            // measurement at each turn boundary inside their loop, and the
            // run's single terminator at the end (`emit_run_complete_on_raw`).
            // They name the measuring seam rather than the closing one for
            // that reason — `close_turn_boundary` at a mid-loop boundary would
            // terminate the run on its first round.
            ("agent/goal.rs", "run_goal_turn", &measuring),
            (
                "agent/goal/goal_wrapped.rs",
                "run_goal_wrapped_turn",
                &measuring,
            ),
            ("agent/resume.rs", "run_resume", &measuring),
        ] {
            let body = std::fs::read_to_string(src.join(file))
                .unwrap_or_else(|e| panic!("cannot read {file}: {e}"));
            assert!(
                body.contains(seam.as_str()),
                "{file} ({driver}) owns a turn and no longer measures what it \
                 changed (`turn_files::{seam}`). A boundary that stops \
                 measuring does not degrade loudly — it silently empties the \
                 Files tab, `stella export` and the audit log for that whole \
                 surface while every other surface keeps working."
            );
        }
    }

    /// **Witness (#4153).** A turn that changed files while the lead's task
    /// lane sat occupied is recorded, and the three boards that look like it
    /// but are not are left alone.
    ///
    /// The delegated case is the one worth pinning: `task_assign` gives a card
    /// an owner and deliberately does not route through `set_status`, so a
    /// session that fanned work out to sub-agents has N in-progress cards and
    /// is a perfectly honest board. A predicate that counted them would report
    /// every delegating session and the measurement would be worthless.
    #[test]
    fn only_an_unowned_in_progress_card_over_the_file_threshold_is_recorded() {
        use stella_core::tasks::TaskBoard;
        use stella_protocol::event::TaskStatus;

        let mut walked_past = TaskBoard::new();
        walked_past.create("investigate the embedding pass", None, None);
        walked_past.create("rewrite the chunker", None, None);
        walked_past.set_status("1", TaskStatus::InProgress).unwrap();

        assert_eq!(
            stale_lane_open_tasks(&walked_past, STALE_LANE_FILES),
            Some(2),
            "an unowned card open across a file-changing turn is the shape #4153 is about"
        );
        assert_eq!(
            stale_lane_open_tasks(&walked_past, STALE_LANE_FILES - 1),
            None,
            "under the threshold a turn is ordinary work in progress, not a signal"
        );

        let mut delegated = TaskBoard::new();
        delegated.create("run the fan-out", None, None);
        delegated.assign("1", "sub:worker-a").unwrap();
        assert_eq!(
            stale_lane_open_tasks(&delegated, STALE_LANE_FILES * 10),
            None,
            "a delegated card carries an owner and does not occupy the lead's lane"
        );

        let mut finished = TaskBoard::new();
        finished.create("rewrite the chunker", None, None);
        finished.set_status("1", TaskStatus::InProgress).unwrap();
        finished.set_status("1", TaskStatus::Completed).unwrap();
        assert_eq!(
            stale_lane_open_tasks(&finished, STALE_LANE_FILES * 10),
            None,
            "a board the agent kept current is the whole point and must stay silent"
        );

        assert_eq!(
            stale_lane_open_tasks(&TaskBoard::new(), STALE_LANE_FILES * 10),
            None,
            "a session with no board at all cannot have walked past a card"
        );
    }

    /// **Witness (#4175).** Every driver that opens a turn's event stream
    /// opens it through [`open_turn_streams`], so the per-call measurement is
    /// attached with it rather than beside it.
    ///
    /// The same asymmetry as the boundary fence above, at the other end of the
    /// turn and one notch more subtle. A driver that forgets
    /// `attach_events` loses a sub-agent's whole lifecycle from the transcript
    /// and is found immediately. A driver that forgot only the measurer would
    /// render every mutating row *correctly shaped* and simply diffless —
    /// indistinguishable from a turn whose calls changed nothing, which is
    /// precisely how #4155 was reported and how the deck's empty Files tab
    /// survived until #4160. Folding the two into one function is the
    /// structural half; this is the half that stops the next driver unfolding
    /// them by calling `attach_events` directly.
    /// The owner list the fence below walks: every file in `stella-cli` that
    /// opens a turn's event stream, and the driver in it that does.
    ///
    /// **The list is the guard.** A door missing from it is a door the fence
    /// cannot see, which is exactly how `stella goal` and `stella resume`
    /// bypassed the seam for as long as it existed (#4507): the goal doors
    /// attached *nothing* to the channel they opened, so a whole goal run
    /// rendered no task board, no sub-agent lifecycle and no diff under any
    /// mutating call, and the resume driver spelled the pair out by hand and
    /// skipped the measurer beside them. Adding a driver here is part of
    /// writing one.
    ///
    /// Two lanes are deliberately absent and are **not** an oversight:
    /// `subsession.rs` and `fleet_cmd.rs` run on a `Config::clone` sharing one
    /// `SessionDurability` cell, so a per-call measurement there would read a
    /// journal another lane is also snapshotting. They join this list once
    /// #3233 gives a lane its own durability, and `subsession.rs:882` is the
    /// live `attach_events` call the widened `raw` check below would fail on
    /// today.
    const STREAM_OWNERS: &[(&str, &str)] = &[
        // The raw engine turn, which reaches the seam through
        // `persistence::attach_run_streams`.
        ("agent/persistence.rs", "attach_run_streams"),
        // The interactive deck's lead turn.
        ("command_deck.rs", "run_lead_turn"),
        // `stella goal`'s raw arm — the loop over `Engine::run_goal`.
        ("agent/goal.rs", "run_goal_turn"),
        // `stella goal --pipeline <variant>`: one observed sender per round,
        // republished so the round's fold sees the registry's own events.
        ("agent/goal/goal_wrapped.rs", "GoalRoundDriver::run_turn"),
        // `stella resume`, driving one restored turn.
        ("agent/resume.rs", "run_resume"),
    ];

    #[test]
    fn every_turn_owner_opens_its_streams_through_the_one_seam() {
        // Built rather than written out, so this file is not its own match.
        let seam = format!("open_turn_{}", "streams");
        // The seam's own wrapper: `attach_run_streams` is `bridge_policy_plane`
        // plus the seam, and `agent/persistence.rs` — which is in the list
        // itself — is where that composition is checked.
        let via = format!("attach_run_{}", "streams");
        let raw = format!("attach_{}(", "events");
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for (file, driver) in STREAM_OWNERS {
            let body = std::fs::read_to_string(src.join(file))
                .unwrap_or_else(|e| panic!("cannot read {file}: {e}"));
            assert!(
                body.contains(&seam) || body.contains(&via),
                "{file} ({driver}) opens a turn's event stream and no longer \
                 does it through `turn_files::open_turn_streams`. A stream \
                 opened without the per-call measurement does not degrade \
                 loudly — every mutating row still renders, just with no diff \
                 and no `+N −M`, which reads as a turn that changed nothing."
            );
            assert!(
                !body.contains(&raw),
                "{file} ({driver}) calls `attach_events` directly again. That \
                 is the seam being unfolded: it pays the loud debt (registry \
                 events) without the silent one beside it."
            );
        }
    }

    /// Every stream owner that ends its run with the cost-shaped terminator
    /// also measures what the run changed.
    ///
    /// A fence, not a witness: it passes on today's tree, and it is here
    /// because #4507 asked for the `_on_raw` spelling to join
    /// [`the_terminator_only_raw_helper_stays_deleted`]'s banned set and that
    /// is the wrong instrument.
    ///
    /// `persistence::emit_run_complete_on_raw` is the goal loops' ending: a
    /// goal run's cost is the whole arc's, not one `TurnOutcome`'s, so
    /// [`close_turn_boundary_raw`] — which reads an outcome — cannot serve it
    /// and the helper is not the deleted `emit_run_complete_raw` under another
    /// name. What it *shares* with that helper is the hazard: on its own it
    /// pays the loud debt and leaves the silent one, which is how both goal
    /// doors terminated a run they had never measured. So the ban stays where
    /// it is and this asks the question directly instead — a caller of the
    /// cost terminator must also carry a tree reading.
    ///
    /// `fleet_cmd.rs` is the one caller that carries neither, and it is
    /// deliberately not listed above: a fan-out's lanes share one
    /// `SessionDurability`, so it is blocked on #3233 with `subsession.rs`.
    #[test]
    fn a_cost_terminator_never_ships_without_a_tree_reading() {
        // A *call*, path-qualified — `agent/persistence.rs` is on the owner
        // list and is where the helper is declared, so an unqualified needle
        // would match its own definition.
        let terminator = format!("::emit_run_complete_on_{}(", "raw");
        let reading = format!("emit_shared_tree_{}", "changes");
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for (file, driver) in STREAM_OWNERS {
            let body = std::fs::read_to_string(src.join(file))
                .unwrap_or_else(|e| panic!("cannot read {file}: {e}"));
            if !body.contains(&terminator) {
                continue;
            }
            assert!(
                body.contains(&reading),
                "{file} ({driver}) ends its run with the cost terminator and \
                 measures nothing. A run that terminates without a tree \
                 reading empties the Files tab, `stella export` and the audit \
                 log for that whole door, and does it silently."
            );
        }
    }

    /// The deleted half of the fence above: the terminator-only raw helper the
    /// deck used to call must stay deleted.
    ///
    /// `persistence::emit_run_complete_raw` is what made the omission possible
    /// — it let a driver holding a raw sender pay the loud debt alone, and read
    /// exactly like a complete boundary at the call site. Re-adding a
    /// terminator-only raw helper reopens the hole this change closed, and the
    /// fence above would not catch it: a driver calling it would simply stop
    /// containing the seam string, which is a failure the author is free to
    /// "fix" by re-adding the string. This pins the removal itself.
    #[test]
    fn the_terminator_only_raw_helper_stays_deleted() {
        let banned = format!("emit_run_complete_{}(", "raw");
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let body = std::fs::read_to_string(src.join("agent/persistence.rs")).expect("persistence");
        assert!(
            !body.contains(&banned),
            "`emit_run_complete_raw` is back. It pays the run terminator \
             without the tree measurement that rides beside it, which is \
             exactly how the deck's Files tab stayed empty for every session. \
             Use `turn_files::close_turn_boundary_raw`, which pays both."
        );
    }

    /// **Witness.** A path the tree reading cannot see still reaches the
    /// stream with its diff, from the call's own reading.
    ///
    /// An unbound durability is the no-journal case exactly, and it is also
    /// what a gitignored path looks like from the snapshot's side: nothing
    /// measured. Before this, `measure_and_publish` sent nothing here and the
    /// row rendered `wrote N bytes` with no diff and no line count.
    #[test]
    fn a_call_s_own_reading_is_published_where_the_tree_saw_nothing() {
        use stella_tools::call_measure::CallMeasure as _;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let measure =
            TurnCallMeasure::new(SessionDurability::default(), EventSender::new(tx), None);
        let own = stella_tools::own_change::own_change(
            ".stella/agents/kfc/spec-tasks.md",
            None,
            "# tasks\n- one\n",
        );
        measure.measure_and_publish(std::slice::from_ref(&own));

        let Ok(AgentEvent::FileChange {
            path,
            kind,
            added,
            removed,
            diff,
        }) = rx.try_recv()
        else {
            panic!("the call's own reading must be published when nothing was measured");
        };
        assert_eq!(path, ".stella/agents/kfc/spec-tasks.md");
        assert_eq!(kind, FileChangeKind::Created);
        assert_eq!((added, removed), (2, 0));
        assert!(
            diff.as_deref().is_some_and(|d| d.contains("+- one")),
            "the diff rides the event: {diff:?}"
        );
        assert!(rx.try_recv().is_err(), "one change, one event");
    }

    /// The durable row a self-read change writes uses the CRUD letters both
    /// reader queries grep for, like a measured one.
    #[test]
    fn a_self_read_change_writes_a_crud_lettered_row() {
        let created = stella_tools::own_change::own_change("a.md", None, "x\n");
        let modified = stella_tools::own_change::own_change("b.md", Some("x\n"), "y\n");
        let deleted = stella_tools::own_change::own_delete("c.md", "x\ny\n");
        assert_eq!(own_touch_row(&created).ops, "C");
        let row = own_touch_row(&modified);
        assert_eq!(row.ops, "U");
        assert_eq!((row.lines_added, row.lines_removed), (1, 1));
        let row = own_touch_row(&deleted);
        assert_eq!(row.ops, "D", "a deletion completes the CRUD alphabet");
        assert_eq!((row.lines_added, row.lines_removed), (0, 2));
    }

    fn measured(kind: JournalChangeKind) -> JournalChange {
        JournalChange {
            path: "src/lib.rs".into(),
            kind,
            added: 12,
            removed: 3,
            diff: Some("@@ -1 +1,2 @@\n+new\n".into()),
        }
    }

    /// The letters the two readers grep for. `execution_rollup`'s
    /// `files_written` and `finalize_execution_reflection`'s `wrote_files`
    /// both match `ops LIKE '%C%' OR '%U%' OR '%D%'`, so a row spelled any
    /// other way fills the table while both queries answer zero.
    #[test]
    fn durable_rows_use_the_crud_letters_both_reader_queries_match() {
        for (kind, expected) in [
            (JournalChangeKind::Created, "C"),
            (JournalChangeKind::Modified, "U"),
            (JournalChangeKind::Deleted, "D"),
        ] {
            assert_eq!(ops_letter(kind), expected);
        }
    }

    /// The durable projection carries the same measurement as the stream one,
    /// from the same reading. Before this existed, a direct-edit turn emitted
    /// `FileChange` events and wrote no `files_touched` row at all.
    #[test]
    fn a_measured_change_becomes_a_durable_row_carrying_the_same_counts() {
        let row = file_touch_row(&measured(JournalChangeKind::Modified));
        assert_eq!(row.path, "src/lib.rs");
        assert_eq!(row.ops, "U");
        assert_eq!((row.lines_added, row.lines_removed), (12, 3));

        let events: serde_json::Value = serde_json::from_str(&row.events_json).expect("json");
        let entries = events.as_array().expect("an array");
        assert_eq!(entries.len(), 1, "one measurement is one audit entry");
        assert_eq!(entries[0]["event"], "measured");
        assert_eq!(entries[0]["lines_added"], 12);
        assert!(
            !row.events_json.contains("@@"),
            "files_touched indexes paths; the diff rides the FileChange event"
        );
    }

    #[test]
    fn every_measured_kind_maps_onto_a_mutating_wire_kind() {
        for (measured_kind, expected) in [
            (JournalChangeKind::Created, FileChangeKind::Created),
            (JournalChangeKind::Modified, FileChangeKind::Modified),
            (JournalChangeKind::Deleted, FileChangeKind::Deleted),
        ] {
            let AgentEvent::FileChange { kind, .. } = file_change(measured(measured_kind)) else {
                panic!("mapped to the wrong event");
            };
            assert_eq!(kind, expected);
            assert!(
                kind.is_mutation(),
                "a tree snapshot can only ever observe a mutation"
            );
        }
    }

    #[test]
    fn the_measured_counts_reach_the_wire_unchanged() {
        // The #2290 contract at the seam it would break: whatever git measured
        // is what ships, never a recount of the diff text.
        let AgentEvent::FileChange {
            added,
            removed,
            diff,
            path,
            ..
        } = file_change(measured(JournalChangeKind::Modified))
        else {
            panic!("mapped to the wrong event");
        };
        assert_eq!((added, removed), (12, 3));
        assert_eq!(path, "src/lib.rs");
        assert!(
            diff.is_some_and(|d| !d.contains("12")),
            "the counts are carried, not re-derivable from the rendering"
        );
    }
}
