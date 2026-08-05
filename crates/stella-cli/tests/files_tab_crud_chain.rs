//! The Files tab's whole chain, end to end, in one test.
//!
//! Every link here was already covered — and the tab still went blank. The
//! emitter's witnesses (`stella-tools`' `registry::tests::file_change`) proved a real
//! `read_file` announces a `Read`; the view's witnesses
//! (`stella-tui`'s `views::files::tests`) proved a populated ledger renders
//! rows. Both suites passed, green, while a recorded session made 85 file-tool
//! calls and wrote **zero** `file_change` events to its store, and the tab read
//! "no files touched yet" from the first call to the last.
//!
//! Nothing tested the *join*: a real `ToolRegistry` announcing onto a real
//! channel, folded by the real deck ingest, rendered by the real view. So this
//! file owns that seam, and only that seam. It deliberately builds nothing by
//! hand — no synthesized `FileChange`, no hand-populated `FileLedger` — because
//! a hand-built event is exactly the assumption that broke.
//!
//! ## Why this cannot flake
//!
//! No clock, no sleep, no spawned task, no wall-clock ordering. `execute()` is
//! awaited to completion before anything is drained, and `record_touch` sends
//! on an **unbounded** channel from the calling task, so every event this run
//! will ever produce is already queued when `try_recv` first runs. The render
//! target is a fixed `Rect`, so layout is a pure function of the ledger.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use stella_protocol::{AgentEvent, FileChangeKind};
use stella_tools::ToolRegistry;
use stella_tui::deck::WorkspaceModel;
use stella_tui::deck_ui::{DeckUi, ingest_inbound};
use stella_tui::envelope::{AgentMeta, Inbound};

const LEAD: &str = "lead";

/// A registry over a fresh tempdir, announcing on a real channel — the shape a
/// turn wires in `command_deck::run_lead_turn`.
fn announcing_registry() -> (
    tempfile::TempDir,
    ToolRegistry,
    tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
) {
    let dir = tempfile::tempdir().unwrap();
    let reg = ToolRegistry::with_issue_backend(dir.path().to_path_buf(), None);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    reg.attach_events(stella_core::EventSender::new(tx));
    (dir, reg, rx)
}

async fn exec_ok(reg: &ToolRegistry, name: &str, input: serde_json::Value) {
    let out = reg.execute(name, &input).await;
    assert!(!out.is_error(), "{name} {input} failed: {out:?}");
}

/// Fold every queued event through the **production** deck ingest — the same
/// call `deck_shell::drain_inbound` makes — rather than reaching into the model.
fn fold_into_deck(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    model: &mut WorkspaceModel,
    ui: &mut DeckUi,
) -> usize {
    let mut folded = 0;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, AgentEvent::FileChange { .. }) {
            folded += 1;
        }
        ingest_inbound(
            &Inbound::Event {
                agent: LEAD.to_string(),
                event,
            },
            model,
            ui,
        );
    }
    folded
}

/// Render the Files tab exactly as `deck_render` does for `DeckTab::Files`.
fn render_files_tab(model: &WorkspaceModel, ui: &mut DeckUi) -> String {
    let area = Rect::new(0, 0, 100, 24);
    let mut buf = Buffer::empty(area);
    stella_tui::views::files::render(model, ui, area, &mut buf);
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn deck() -> (WorkspaceModel, DeckUi) {
    let mut model = WorkspaceModel::new();
    model.apply_inbound(&Inbound::Register(AgentMeta::new(LEAD, "goal", 0)));
    (model, DeckUi::default())
}

/// The regression, stated as the user states it: do the reads and the mutations
/// a turn actually performed appear on the Files tab?
///
/// Every CRUD shape in one run, because the break was never one op — it was the
/// whole stream going quiet at once.
#[tokio::test]
async fn every_crud_touch_a_turn_makes_reaches_the_rendered_files_tab() {
    let (dir, reg, mut rx) = announcing_registry();
    std::fs::write(dir.path().join("read_me.rs"), "one\ntwo\n").unwrap();
    std::fs::write(dir.path().join("edit_me.rs"), "one\ntwo\n").unwrap();
    std::fs::write(dir.path().join("delete_me.rs"), "gone\n").unwrap();
    std::fs::write(dir.path().join("bulk.rs"), "keep\n").unwrap();

    exec_ok(
        &reg,
        "read_file",
        serde_json::json!({ "path": "read_me.rs" }),
    )
    .await;
    exec_ok(
        &reg,
        "write_file",
        serde_json::json!({ "path": "created.rs", "content": "a\nb\nc\n" }),
    )
    .await;
    exec_ok(
        &reg,
        "edit_file",
        serde_json::json!({ "path": "edit_me.rs", "old_string": "two", "new_string": "TWO" }),
    )
    .await;
    exec_ok(
        &reg,
        "apply_edits",
        serde_json::json!({
            "edits": [{ "path": "bulk.rs", "old_string": "keep", "new_string": "kept" }]
        }),
    )
    .await;
    exec_ok(
        &reg,
        "delete_file",
        serde_json::json!({ "path": "delete_me.rs" }),
    )
    .await;

    let (mut model, mut ui) = deck();
    let folded = fold_into_deck(&mut rx, &mut model, &mut ui);
    assert_eq!(
        folded, 5,
        "one FileChange per touch — a read, a create, an edit, a bulk edit, a delete"
    );

    let text = render_files_tab(&model, &mut ui);
    assert!(
        !text.contains("no files touched yet"),
        "the tab claimed an untouched tree after five real touches:\n{text}"
    );
    for path in [
        "read_me.rs",
        "created.rs",
        "edit_me.rs",
        "bulk.rs",
        "delete_me.rs",
    ] {
        assert!(
            text.contains(path),
            "{path} was touched but never rendered:\n{text}"
        );
    }
    assert!(
        text.contains("5 files"),
        "the footer must count every touched path:\n{text}"
    );
}

/// Reads are the half that has no other way home. A mutation can still reach the
/// tab late (adoption re-emits it); a read that is not announced when it happens
/// is gone — git has no record of it. The blind session's 40 `read_file` calls
/// are this test.
#[tokio::test]
async fn a_read_only_turn_still_fills_the_files_tab() {
    let (dir, reg, mut rx) = announcing_registry();
    std::fs::write(dir.path().join("a.rs"), "one\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "two\n").unwrap();

    exec_ok(&reg, "read_file", serde_json::json!({ "path": "a.rs" })).await;
    exec_ok(&reg, "read_file", serde_json::json!({ "path": "b.rs" })).await;
    // Re-reading one path is a second read of the same row, never a second row.
    exec_ok(&reg, "read_file", serde_json::json!({ "path": "a.rs" })).await;

    let (mut model, mut ui) = deck();
    assert_eq!(fold_into_deck(&mut rx, &mut model, &mut ui), 3);

    assert_eq!(model.ledger.total_reads(), 3, "three reads, counted");
    assert_eq!(model.ledger.file_count(), 2, "across two distinct paths");

    let text = render_files_tab(&model, &mut ui);
    assert!(
        !text.contains("no files touched yet"),
        "a read-only turn is still a turn that touched files:\n{text}"
    );
    assert!(text.contains("a.rs") && text.contains("b.rs"), "{text}");
    assert!(
        text.contains("3 reads"),
        "the footer must report the read tally:\n{text}"
    );
}

/// The counts the tab shows are the emitter's measurement, carried intact
/// through the fold and out to the footer. Re-deriving them anywhere in the
/// chain is how a real rewrite once rendered as `+0 -0`.
#[tokio::test]
async fn the_rendered_totals_are_the_emitters_own_measurement() {
    let (dir, reg, mut rx) = announcing_registry();
    std::fs::write(dir.path().join("m.rs"), "one\ntwo\nthree\n").unwrap();

    exec_ok(
        &reg,
        "edit_file",
        serde_json::json!({ "path": "m.rs", "old_string": "two", "new_string": "TWO\nAND\nMORE" }),
    )
    .await;

    // What the emitter said, taken off the wire before anything folds it.
    let announced = {
        let event = rx.try_recv().expect("the edit announced itself");
        match event {
            AgentEvent::FileChange {
                added,
                removed,
                kind,
                ..
            } => {
                assert_eq!(kind, FileChangeKind::Modified);
                (added, removed)
            }
            other => panic!("expected a FileChange, got {other:?}"),
        }
    };
    assert_ne!(announced, (0, 0), "the fixture must produce a real delta");

    let (mut model, mut ui) = deck();
    ingest_inbound(
        &Inbound::Event {
            agent: LEAD.to_string(),
            event: AgentEvent::FileChange {
                path: "m.rs".into(),
                kind: FileChangeKind::Modified,
                added: announced.0,
                removed: announced.1,
                diff: None,
            },
        },
        &mut model,
        &mut ui,
    );

    assert_eq!(
        (model.ledger.total_added(), model.ledger.total_removed()),
        announced,
        "the ledger must carry the emitter's numbers, not a recount"
    );
    let text = render_files_tab(&model, &mut ui);
    assert!(
        text.contains(&format!("+{}", announced.0)) && text.contains(&format!("-{}", announced.1)),
        "the footer must show the announced delta:\n{text}"
    );
}

/// The isolated-run posture, end to end: `create_worktrees` (and Best-of-N, and
/// an authored witness) run the turn inside a candidate registry, which
/// announces reads but withholds mutations for adoption to re-emit.
///
/// Pinned here as well as at the emitter because the property that matters is
/// the *rendered* one: an isolated run must never again show a blank tab, and
/// must never show an edit the user's checkout has not received.
#[tokio::test]
async fn an_isolated_run_shows_its_reads_and_withholds_its_unadopted_edits() {
    let dir = tempfile::tempdir().unwrap();
    let reg = ToolRegistry::with_issue_backend(dir.path().to_path_buf(), None);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    reg.attach_read_events(stella_core::EventSender::new(tx));
    std::fs::write(dir.path().join("seen.rs"), "one\n").unwrap();

    exec_ok(&reg, "read_file", serde_json::json!({ "path": "seen.rs" })).await;
    exec_ok(
        &reg,
        "write_file",
        serde_json::json!({ "path": "candidate_only.rs", "content": "x\n" }),
    )
    .await;

    let (mut model, mut ui) = deck();
    fold_into_deck(&mut rx, &mut model, &mut ui);

    let text = render_files_tab(&model, &mut ui);
    assert!(
        text.contains("seen.rs"),
        "an isolated run's reads are the user's to see:\n{text}"
    );
    assert!(
        !text.contains("candidate_only.rs"),
        "an unadopted edit must not be claimed as the checkout's:\n{text}"
    );
    assert_eq!(
        reg.mutations_recorded(),
        1,
        "withholding the event must never withhold the count (#973)"
    );
}
