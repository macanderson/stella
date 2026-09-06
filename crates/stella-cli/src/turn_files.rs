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
//! The durable half no longer *states* what it cannot know: since #4386 each
//! `files_touched` row carries the provenance of the reading that produced it,
//! and a row measured while another live session shared the work tree says so
//! rather than naming this turn as the author. [`attribution`] holds that
//! decision and the argument for labelling rather than filtering.
//!
//! It lives beside `agent.rs` rather than inside it because `agent.rs` sits
//! close to the 1500-line ratchet (AGENTS.md § "God files — plan around them,
//! never into them") — new logic lands in a sibling.

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

pub(crate) mod attribution;
mod stale_lane;

use attribution::Provenance;

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
            emit_measured_tree_changes(&self.durability, &self.tx, self.execution.as_ref(), own);
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
        events_json: touch_events(
            Provenance::OwnReading,
            u64::from(change.added),
            u64::from(change.removed),
        ),
    }
}

/// The `events_json` both row builders write: one `measured` entry naming the
/// reading that produced it and whether that reading can name an author.
///
/// One function rather than two literals because `attributed` is the field a
/// reader has to be able to trust across both shapes — an export that renders
/// it for a tree reading and omits it for a call's own reading would make its
/// absence mean two things (#4386).
fn touch_events(provenance: Provenance, added: u64, removed: u64) -> String {
    serde_json::json!([{
        "event": "measured",
        "reason": provenance.reason(),
        "attributed": provenance.attributed(),
        "lines_added": added,
        "lines_removed": removed,
    }])
    .to_string()
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
/// - **Who else is in the work tree** (#4386). The per-call measurements this
///   attaches read the cached answer, so the turn's first reading has to have
///   one; the closing bookend re-asks for a session that started mid-turn.
/// - **Which board task the turn's work belongs to** (#5039). Forgetting this
///   is silent in the third way: every event still lands, it simply carries no
///   task tag, so the plan panel's evidence and cost lines are empty for a
///   session that was in fact working through a plan. It rides here rather
///   than at each engine construction site because the tag belongs to the
///   *stream*, not to the engine: the registry's own events (file changes, the
///   board mirror) need it exactly as much as the engine's do, and this is the
///   one place both are in scope.
pub(crate) fn open_turn_streams(
    registry: &stella_tools::registry::ToolRegistry,
    cfg: &Config,
    tx: &EventSender,
    execution: Option<&(Arc<Store>, i64)>,
) {
    cfg.durability.refresh_worktree_sharers();
    // Before the attachments below, so no event can be admitted through this
    // sender untagged: `attach_running_task` reaches every clone, but only the
    // ones that have not been sent through yet.
    tx.attach_running_task(registry.running_task());
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
///
/// Returns the sender it built, which the deck's lead turn drives its engine
/// through (#5039). It has to: the running-task source attached above is
/// shared by a sender's *clones*, and a raw channel handed to
/// [`stella_core::Engine::run_turn`] is wrapped in a second, unattached
/// sender inside the engine — so a driver that kept using the raw handle would
/// tag its registry's events and none of its engine's. Every other door
/// already holds one `EventSender` and passes it to both.
#[must_use]
pub(crate) fn open_turn_streams_raw(
    registry: &stella_tools::registry::ToolRegistry,
    cfg: &Config,
    tx: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    execution: Option<&(Arc<Store>, i64)>,
) -> EventSender {
    let sender = EventSender::new(tx.clone());
    open_turn_streams(registry, cfg, &sender, execution);
    sender
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
    stale_lane::note_stale_lane(
        &crate::diag_boot::dx(),
        registry,
        emit_shared_tree_changes(cfg, tx, execution),
    );
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
///
/// The sharer set is re-asked here rather than reused from the turn's opening
/// bookend, because this sweep covers the whole turn and a session that opened
/// halfway through it wrote into the same window (#4386). No call owns this
/// reading, so it carries no `own` paths to vouch for any of it.
pub(crate) fn emit_shared_tree_changes(
    cfg: &Config,
    tx: &EventSender,
    execution: Option<&(Arc<Store>, i64)>,
) -> usize {
    cfg.durability.refresh_worktree_sharers();
    emit_measured_tree_changes(&cfg.durability, tx, execution, &[]).len()
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
/// then run the *same* function, which is what makes the no-double-counting
/// argument in `stella_tools::call_measure` hold: there is one measurement,
/// called more or less often, not two that have to agree.
///
/// Returns the paths it published, so a per-call reading the tool supplied
/// itself ([`emit_own_changes`]) can be published for the rest and no path
/// gets two events.
pub(crate) fn emit_measured_tree_changes(
    durability: &SessionDurability,
    tx: &EventSender,
    execution: Option<&(Arc<Store>, i64)>,
    own: &[OwnChange],
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
        // The sharer set is per turn, not per path, but the classification is
        // per path: a call's own reading vouches for the path it named and for
        // nothing else (#4386).
        let sharers = durability.worktree_sharers();
        let rows: Vec<FileTouchRow> = measured
            .iter()
            .map(|change| {
                file_touch_row(change, attribution::provenance(&change.path, own, &sharers))
            })
            .collect();
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
fn file_touch_row(change: &JournalChange, provenance: Provenance) -> FileTouchRow {
    FileTouchRow {
        path: change.path.clone(),
        ops: ops_letter(change.kind).to_string(),
        lines_added: u64::from(change.added),
        lines_removed: u64::from(change.removed),
        events_json: touch_events(
            provenance,
            u64::from(change.added),
            u64::from(change.removed),
        ),
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
        // `git diff-tree -p` computed this, not `stella_diff::unified_diff` —
        // there is no area cap to trip.
        minimal: true,
        task_id: None,
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
            // In `agent/turn.rs` since the split moved `run_turn` out of
            // `agent.rs`, the same way the deck's row below moved.
            ("agent/turn.rs", "run_turn", &closing),
            // The interactive deck's lead turn — the driver that had the hole.
            // In `command_deck/lead_turn.rs` since #4775 split the deck's
            // driver loop into sibling modules.
            ("command_deck/lead_turn.rs", "run_lead_turn", &closing),
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
    /// **The list is the guard, and [`ENGINE_DRIVERS`] is what keeps it
    /// complete.** A door missing from here is a door this fence cannot see,
    /// which is exactly how `stella goal` and `stella resume` bypassed the
    /// seam for as long as it existed (#4507): the goal doors attached
    /// *nothing* to the channel they opened, so a whole goal run rendered no
    /// task board, no sub-agent lifecycle and no diff under any mutating call,
    /// and the resume driver spelled the pair out by hand and skipped the
    /// measurer beside them. Adding a driver here is part of writing one — and
    /// since #3421, forgetting to fails `ENGINE_DRIVERS`' own fence rather than
    /// waiting for a bench run to notice.
    ///
    /// Two lanes are absent and are **not** an oversight — they are named,
    /// with the reason each is still out, in
    /// [`LANES_OUTSIDE_THE_SEAM`], which
    /// [`only_the_seam_and_the_declared_lanes_attach_a_turn_stream`] enforces
    /// from both sides. Read that table rather than a summary here: this
    /// comment said both lanes shared one `SessionDurability` cell and would
    /// join the list "once #3233 gives a lane its own durability", and by the
    /// time anyone read it both lanes had held their own for months. What
    /// actually blocks each is a different problem, and neither is #3233.
    const STREAM_OWNERS: &[(&str, &str)] = &[
        // The raw engine turn, which reaches the seam through
        // `persistence::attach_run_streams`.
        ("agent/persistence.rs", "attach_run_streams"),
        // The interactive deck's lead turn (moved to its own module by #4775).
        ("command_deck/lead_turn.rs", "run_lead_turn"),
        // `stella goal`'s raw arm — the loop over `Engine::run_goal`.
        ("agent/goal.rs", "run_goal_turn"),
        // `stella goal --pipeline <variant>`: one observed sender per round,
        // republished so the round's fold sees the registry's own events.
        ("agent/goal/goal_wrapped.rs", "GoalRoundDriver::run_turn"),
        // `stella resume`, driving one restored turn.
        ("agent/resume.rs", "run_resume"),
        // `stella run --pipeline <variant>`'s between-rounds stream, which a
        // plugin's own model calls meter into (#3802). Not a turn, and on the
        // list for the reason a turn is: it publishes a channel on the registry
        // and owes it the same two debts.
        ("wrapper_plugin/child_stream.rs", "PluginChildStream::open"),
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

    /// What a file that builds an engine owes the turn-file ledger.
    ///
    /// Every engine-building file in this crate declares one, and "nothing" is
    /// not among them: a driver that pays no boundary has to say *why*, and a
    /// driver blocked on something else has to name the issue — AGENTS.md's
    /// rule 10 pointed at a producer instead of a consumer.
    #[derive(Debug)]
    enum DriverPosture {
        /// Owns a turn's stream and pays both halves of the boundary. Which
        /// halves and where is [`STREAM_OWNERS`]' question, not this one.
        Owns,
        /// Runs under a stream someone else opened, so the debt is not its to
        /// pay — the parent's boundary sweep measures what it wrote.
        Nested(&'static str),
        /// Owns a turn and cannot measure it yet, with the issue that unblocks
        /// it. Never a silence: the Files tab is empty for this door and that
        /// is a tracked gap rather than an oversight.
        Blocked(&'static str),
    }

    /// Every file in this crate that builds an engine, and what it owes.
    ///
    /// The two lists above are hand-maintained and can only check the doors
    /// they already know about — "the list is the guard", and a door missing
    /// from it is a door the fence cannot see. That is not a hypothetical: the
    /// goal doors and `stella resume` bypassed the stream seam for as long as
    /// it existed and were found by a bench run rather than by a test (#4507).
    ///
    /// This list is checked in **both** directions against the tree, so a file
    /// that starts building an engine has to join it and a row whose file
    /// stopped building one has to leave. That is what closes the discovery
    /// gap #3421 named: the next driver cannot be added silently, whatever it
    /// is called and wherever it lands.
    const ENGINE_DRIVERS: &[(&str, DriverPosture)] = &[
        // `stella run` and the plain chat loop. Reaches the opening seam
        // through `persistence::attach_run_streams`, which is why
        // `STREAM_OWNERS` names that file and this one names the driver's own.
        ("agent/turn.rs", DriverPosture::Owns),
        ("command_deck/lead_turn.rs", DriverPosture::Owns),
        ("agent/goal.rs", DriverPosture::Owns),
        ("agent/goal/goal_wrapped.rs", DriverPosture::Owns),
        ("agent/resume.rs", DriverPosture::Owns),
        (
            "subagent.rs",
            DriverPosture::Nested(
                "a dispatched child runs on its parent's stream and inherits no \
                 durable identity (`durability`'s \"what must NOT inherit a \
                 binding\"), so its writes reach the ledger through the \
                 parent's turn boundary",
            ),
        ),
        (
            "subsession.rs",
            DriverPosture::Blocked(
                "#4507: the lane has held its own `lane_durability` since \
                 #3233's first slice. What blocks it now is that \
                 `open_turn_streams` reads `cfg.durability` and the lane's \
                 handle is on its `EngineConfig` instead — plus the question a \
                 seam taking an explicit handle would force: the lead and the \
                 lane snapshot one shared work tree from two baselines, so a \
                 lane's writes would be attributed to both",
            ),
        ),
        (
            "fleet_cmd.rs",
            DriverPosture::Blocked(
                "#4507, and a different problem from the lane's: the attempt \
                 has its own `attempt_durability` (#3232), bound at the \
                 invocation root while the worker rebinds `cfg.workspace_root` \
                 to its own worktree — so a snapshot here would measure the \
                 wrong tree, and the fix is the journal's root rather than a \
                 durability handle",
            ),
        ),
    ];

    /// **Fence (#3421).** Every file that builds an engine declares what it
    /// owes the turn-file ledger, and nothing builds one without saying.
    ///
    /// Keyed on the engine construction rather than on a turn-entry spelling,
    /// because the drivers do not agree on one: `run_turn_with_sender`,
    /// `run_goal` and the resume path are three different entries and
    /// building an engine is the one thing all of them do first.
    ///
    /// Keyed on [`stella_core::Engine::assemble`]. That call is the only way
    /// to build an engine. So a door has no second spelling to hide behind,
    /// and this fence sees them all.
    ///
    /// Shipping files only. A `#[cfg(test)]` engine is a fixture, and the
    /// question here is which *door* drives a turn.
    #[test]
    fn every_engine_driver_declares_what_it_owes_the_ledger() {
        // Built rather than written out, so this file is not its own match,
        // and with the open paren so a doc comment naming a constructor is
        // prose rather than a driver.
        let constructor = format!("Engine::{}(", "assemble");
        let builds = |body: &str| body.contains(&constructor);
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

        for (file, posture) in ENGINE_DRIVERS {
            let body = std::fs::read_to_string(src.join(file))
                .unwrap_or_else(|e| panic!("cannot read {file}: {e}"));
            assert!(
                builds(&body),
                "{file} is declared a turn driver ({posture:?}) and no longer \
                 builds an engine. A stale row is worse than no row: it makes \
                 the list look complete while the door it stood for has moved."
            );
            match posture {
                // `Owns` is checked by the two fences above, which is where
                // the debt itself lives.
                DriverPosture::Owns => {}
                DriverPosture::Nested(reason) => assert!(
                    !reason.is_empty(),
                    "{file} is declared nested and says nothing about whose \
                     stream it runs on. A driver that pays no boundary owes an \
                     explanation a reviewer can check."
                ),
                DriverPosture::Blocked(reason) => assert!(
                    reason.contains('#'),
                    "{file} is blocked and names no issue. An empty Files tab \
                     with nothing tracking it is the silence this table exists \
                     to refuse."
                ),
            }
        }

        for path in shipping_sources(&src) {
            let body = std::fs::read_to_string(&path).expect("a listed source");
            if !builds(&body) {
                continue;
            }
            let relative = path
                .strip_prefix(&src)
                .expect("walked from src")
                .to_string_lossy()
                .replace('\\', "/");
            assert!(
                ENGINE_DRIVERS.iter().any(|(file, _)| *file == relative),
                "{relative} builds an engine and is in no row of \
                 `ENGINE_DRIVERS`. A driver that owns a turn owes the boundary \
                 a measurement, and one that does not owes an explanation — \
                 declare which, and name the issue if it is blocked."
            );
        }
    }

    /// Every `.rs` file under `src` that ships, newest-first order irrelevant.
    ///
    /// `tests.rs` and anything under a `tests/` directory are skipped: a
    /// fixture engine is not a door, and counting one would make the fence
    /// demand a posture for a test module.
    fn shipping_sources(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot walk {dir:?}: {e}"));
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if name != "tests" {
                    out.extend(shipping_sources(&path));
                }
            } else if name.ends_with(".rs") && name != "tests.rs" {
                out.push(path);
            }
        }
        out
    }

    /// The part of a source file that ships: everything above its inline
    /// `#[cfg(test)] mod … {`.
    ///
    /// [`shipping_sources`] drops `tests.rs` and `tests/` directories, which
    /// is the whole answer for a crate that keeps its tests in separate files.
    /// It is not the answer for an inline test module, and
    /// `command_deck/forwarder.rs` is exactly that: its #2290 witness builds a
    /// registry and attaches a sender to it by hand, because standing a real
    /// turn up around `close_turn_stream` would be testing something else. A
    /// scan that counted it would report a bypassing door that does not exist,
    /// and the fix a reader would reach for is to make the fixture lie.
    ///
    /// **The marker is the module, never the attribute**, and the difference
    /// is not cosmetic. `#[cfg(test)]` also sits on individual items —
    /// `subsession.rs` puts it on two test-only helpers around line 230, eight
    /// hundred lines above the call the fence below exists to see. Cutting at
    /// the first attribute truncated that file to its first fifth and reported
    /// a clean scan, which is how this helper's first version passed while
    /// looking at almost nothing. So the cut needs a bare `#[cfg(test)]` on
    /// its own line followed by a module *body*: an item-level attribute is
    /// indented, and a bodiless `#[cfg(test)] mod tests;` declaration hides
    /// nothing because its file is skipped already.
    fn ships(body: &str) -> &str {
        let mut offset = 0;
        let mut chunks = body.split_inclusive('\n').peekable();
        while let Some(chunk) = chunks.next() {
            let opens_a_test_module = chunks.peek().is_some_and(|next| {
                let next = next.trim_end();
                next.starts_with("mod ") && next.ends_with('{')
            });
            if chunk.trim_end() == "#[cfg(test)]" && opens_a_test_module {
                return &body[..offset];
            }
            offset += chunk.len();
        }
        body
    }

    /// The lanes that still open a turn's event stream by hand, and the issue
    /// deciding each.
    ///
    /// **A closed set, not a list to add to.** It records debt that predates
    /// the fence below — the legitimate reason for a ratchet, and the only one
    /// (AGENTS.md § "Ports, not direct dependencies", rule 5). A new file
    /// reaching for `attach_events`
    /// fails that fence, and the fix is the seam, never a row here.
    ///
    /// Each lane is blocked on its own problem, and **neither is the shared
    /// `SessionDurability` cell** the surrounding comments blamed until now:
    ///
    /// - `subsession.rs` has held its own `lane_durability` since #3233's
    ///   first slice landed. What stops it is that [`open_turn_streams`] reads
    ///   `cfg.durability`, and the lane's handle is on its `EngineConfig`
    ///   instead (`agent::engine::subsession_engine_config_for`). Routing it
    ///   through needs a seam taking an explicit handle — and a decision about
    ///   the lead and the lane snapshotting one shared work tree from two
    ///   baselines, which would attribute a lane's writes to both.
    /// - `fleet_cmd/wrapped.rs` has `attempt_durability` (#3232), bound at the
    ///   invocation root while the worker's config is rebound to its worktree.
    ///   A measurer attached there would snapshot the wrong tree.
    const LANES_OUTSIDE_THE_SEAM: &[(&str, &str)] = &[
        ("subsession.rs", "#4507"),
        ("fleet_cmd/wrapped.rs", "#4507"),
    ];

    /// **The fence [`every_turn_owner_opens_its_streams_through_the_one_seam`]
    /// cannot be.** That one asks whether each *listed* owner reaches the
    /// seam; this asks whether anything else in the crate opens a turn's
    /// stream at all.
    ///
    /// The gap between them is a file on no list. `ENGINE_DRIVERS` closes half
    /// of it — a new file that builds an engine must declare a posture — and
    /// the other half is a file that attaches a stream without building one,
    /// which is what both lanes below do and what nothing could see.
    ///
    /// It asks in both directions, and the second is what makes it #4507's
    /// acceptance test rather than a snapshot: a declared lane that no longer
    /// calls `attach_events` **fails**, so routing one through the seam is
    /// finished by deleting its row, and the list cannot outlive the debt it
    /// records.
    #[test]
    fn only_the_seam_and_the_declared_lanes_attach_a_turn_stream() {
        // Built rather than written out, so this file is not its own match —
        // the convention the fences above follow for the same reason.
        let raw = format!("attach_{}(", "events");
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

        let mut found: Vec<String> = Vec::new();
        for path in shipping_sources(&src) {
            let body = std::fs::read_to_string(&path).expect("a shipping source");
            if !ships(&body).contains(&raw) {
                continue;
            }
            let relative = path
                .strip_prefix(&src)
                .expect("walked from src")
                .to_string_lossy()
                .replace('\\', "/");
            // The seam itself, which is where the call belongs.
            if relative == "turn_files.rs" {
                continue;
            }
            assert!(
                LANES_OUTSIDE_THE_SEAM
                    .iter()
                    .any(|(lane, _)| *lane == relative),
                "{relative} opens a turn's event stream by hand. It gets no \
                 per-call measurer, so that turn's file changes reach no Files \
                 tab, no `stella export` and no audit log — silently. Use \
                 `open_turn_streams`; adding a row to \
                 `LANES_OUTSIDE_THE_SEAM` is not the fix."
            );
            found.push(relative);
        }

        for (lane, issue) in LANES_OUTSIDE_THE_SEAM {
            assert!(
                found.iter().any(|seen| seen == lane),
                "{lane} is declared as still outside the seam ({issue}) and no \
                 longer calls it. If it now routes through `open_turn_streams`, \
                 delete its row — a declared exception that has been fixed \
                 teaches the next reader that a door is broken when it is not."
            );
        }
    }

    /// The cut is the subtle half of the fence above, so it is checked
    /// directly rather than trusted: a green scan must mean the shipping half
    /// was searched, not that the search stopped before it.
    ///
    /// The item-level case is the bug this helper shipped with for one run:
    /// an item-level `#[cfg(test)]` truncating a file eight hundred lines
    /// above the call the fence is looking for.
    #[test]
    fn the_shipping_half_stops_at_a_test_module_and_nothing_else() {
        let inline = "fn ships_me() {}\n#[cfg(test)]\nmod tests {\n fn hidden() {}\n}\n";
        assert!(ships(inline).contains("ships_me"));
        assert!(!ships(inline).contains("hidden"));

        // No test module: the whole file ships.
        assert_eq!(ships("fn only() {}"), "fn only() {}");

        // An item-level attribute is not a module and must not cut — the
        // shape `subsession.rs` has, with the interesting call below it.
        let item_level = "impl T {\n    #[cfg(test)]\n    fn helper() {}\n}\nfn later_call() {}\n";
        assert!(
            ships(item_level).contains("later_call"),
            "a test-only helper must not blind the scan to the rest of the file"
        );

        // A bodiless declaration points at a file `shipping_sources` already
        // skips, so it hides nothing and must not cut either.
        let declaration = "#[cfg(test)]\nmod tests;\nfn later_call() {}\n";
        assert!(ships(declaration).contains("later_call"));
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
    /// `fleet_cmd.rs` is the one caller that carries neither, and it is not
    /// listed above. Why it is still out is in [`LANES_OUTSIDE_THE_SEAM`] —
    /// its journal is rooted where the worker's tree is not, which is a
    /// different problem from the shared `SessionDurability` this comment used
    /// to blame and from what blocks `subsession.rs`.
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
            minimal,
            ..
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
        assert!(minimal, "two lines never trips the area cap");
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
        let row = file_touch_row(
            &measured(JournalChangeKind::Modified),
            Provenance::SoleWriter,
        );
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

    /// **Witness (#4386).** The reported shape: two sessions in one checkout,
    /// one of them writing a file the other never opened. Session A's snapshot
    /// sees it — that is what a shared work tree means and no fix changes it —
    /// so the durable row must say the reading cannot name an author, where it
    /// used to state "turn-boundary work-tree measurement" and let the export
    /// present another session's file as this turn's work.
    #[test]
    fn a_file_another_live_session_wrote_is_not_claimed_by_this_turn() {
        let guard = tempfile::tempdir().unwrap();
        let ws = guard.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let store_root = guard.path().join("store");
        let registry_dir = guard.path().join("sessions");

        // Two sessions on one tree, each with its own journal — the same
        // arrangement `WorkJournal` gives them in a real checkout.
        let session_a = stella_store::work_journal::WorkJournal::open_in(&store_root, &ws, "ses-a")
            .expect("journal A");
        let _session_b =
            stella_store::work_journal::WorkJournal::open_in(&store_root, &ws, "ses-b")
                .expect("journal B");
        // A's baseline, discarded exactly as `SessionDurability::bind` does.
        let _ = session_a.snapshot_worktree().expect("baseline");

        // Session B writes. Session A does nothing at all.
        std::fs::write(ws.join("prose_score.py"), "print('b')\n").unwrap();

        let registry = stella_store::SessionRegistry::open(&registry_dir);
        for id in ["ses-a", "ses-b"] {
            registry
                .upsert(&stella_store::SessionRecord {
                    id: id.into(),
                    pid: std::process::id(),
                    workspace: ws.to_string_lossy().into_owned(),
                    title: String::new(),
                    summary: String::new(),
                    description: None,
                    status: stella_store::SessionStatus::InProgress,
                    started_at_ms: 0,
                    updated_at_ms: 0,
                    supervisor: None,
                })
                .expect("register");
        }
        let sharers = attribution::sharers_of(&registry.list(), &ws, "ses-a");
        assert_eq!(
            sharers,
            vec!["ses-b".to_string()],
            "A must see B as sharing its tree"
        );

        let measured = session_a.snapshot_worktree().expect("A measures the tree");
        let rows: Vec<FileTouchRow> = measured
            .iter()
            .map(|change| {
                file_touch_row(change, attribution::provenance(&change.path, &[], &sharers))
            })
            .collect();
        let row = rows
            .iter()
            .find(|row| row.path == "prose_score.py")
            .expect("the shared tree puts B's file in A's reading — that is the defect's premise");
        let events: serde_json::Value = serde_json::from_str(&row.events_json).expect("json");
        assert_eq!(
            events[0]["attributed"], false,
            "a change measured while another session shared the tree must not be \
             recorded as this turn's: {}",
            row.events_json
        );
        assert!(
            row.events_json.contains("another session"),
            "the reason string is what `stella export` renders: {}",
            row.events_json
        );
    }

    /// The other side: the same reading, with nobody else in the tree, is still
    /// this session's work and still says so.
    #[test]
    fn a_lone_session_still_claims_what_it_measured() {
        let row = file_touch_row(
            &measured(JournalChangeKind::Created),
            attribution::provenance("src/lib.rs", &[], &[]),
        );
        let events: serde_json::Value = serde_json::from_str(&row.events_json).expect("json");
        assert_eq!(events[0]["attributed"], true);
        assert_eq!(events[0]["reason"], "turn-boundary work-tree measurement");
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
