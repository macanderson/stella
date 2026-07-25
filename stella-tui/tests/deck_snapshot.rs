//! Full-deck render snapshot: fold the scripted demo scenario into a
//! `WorkspaceModel`, then render every tab through the real `render_deck`
//! entrypoint into a `TestBackend` and assert the expected content appears.
//! Also writes the rendered frames to `deck-snapshots.txt` under this target's
//! `CARGO_TARGET_TMPDIR` as a human-readable artifact (a text "screenshot" —
//! the honest headless equivalent of a TTY capture). The path is printed by the
//! test; it deliberately stays out of the source tree so running the suite
//! never dirties the working tree and concurrent runs never race on one path.

use std::fmt::Write as _;

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use stella_tui::scenario::{demo_graph, demo_inbound};
use stella_tui::{DeckTab, DeckUi, WorkspaceModel, render_deck};

fn folded_model() -> WorkspaceModel {
    let mut model = WorkspaceModel::new();
    model.now_ms = 312_000; // ~5:12 elapsed, so the dashboard timers read nicely
    for inbound in demo_inbound(0, std::process::id()) {
        model.apply_inbound(&inbound);
    }
    model
}

fn render_tab(model: &WorkspaceModel, tab: DeckTab, w: u16, h: u16) -> String {
    let mut ui = DeckUi::default();
    ui.splash.skip(); // past the splash so the tabs draw
    ui.tab = tab;
    ui.graph = Some(demo_graph());
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| render_deck(model, &mut ui, f)).unwrap();
    let buf = terminal.backend().buffer();
    let area = *buf.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn deck_renders_every_tab_with_real_content() {
    let model = folded_model();
    assert_eq!(model.agents.len(), 3, "scenario registered 3 agents");

    let cases = [
        (DeckTab::Session, "lead"),
        (DeckTab::Agents, "sub:auth"),
        (DeckTab::Traces, "Which auth guard"),
        (DeckTab::Graph, "run_turn"),
        (DeckTab::Files, "automations"),
    ];
    for (tab, needle) in cases {
        let text = render_tab(&model, tab, 120, 36);
        assert!(
            text.contains(needle),
            "the {tab:?} tab should show {needle:?}, got:\n{text}"
        );
        // The comfy-tabs bar labels are always present — UPPERCASE by the
        // deck's tab-label convention. (Assert on a left-anchored label: at
        // 120 cols the 9-tab bar overflows, so the rightmost SETTINGS clips.)
        assert!(text.contains("SESSION"), "tab bar should render on {tab:?}");
    }

    // Write all five tabs to a human-readable artifact under the target dir —
    // never into the source tree, which `cargo test` must leave clean.
    let mut out = String::new();
    for tab in DeckTab::ALL {
        let _ = writeln!(out, "\n═══ {} tab ═══\n", tab.title());
        let _ = writeln!(out, "{}", render_tab(&model, tab, 150, 32));
    }
    let artifact = concat!(env!("CARGO_TARGET_TMPDIR"), "/deck-snapshots.txt");
    // Best-effort: never fail the test on an artifact write, but do say where
    // it landed so the "screenshot" stays discoverable under `--nocapture`.
    match std::fs::write(artifact, out) {
        Ok(()) => println!("deck snapshots written to {artifact}"),
        Err(err) => println!("deck snapshots not written to {artifact}: {err}"),
    }
}

#[test]
fn agents_dashboard_shows_status_and_spend_columns() {
    let model = folded_model();
    // The dashboard is dense (11 columns) and now fills the whole tab (the
    // engine panel moved to SETTINGS) — render at a roomy width so every
    // column shows.
    let text = render_tab(&model, DeckTab::Agents, 240, 20);
    // Column headers and at least one agent's live status all render.
    for needle in ["CPU%", "MEM", "In/Out", "Activity", "needs input"] {
        assert!(
            text.contains(needle),
            "dashboard missing {needle:?}:\n{text}"
        );
    }
    // The config editor no longer shares this tab — its focus hint (unique to
    // the engine panel) must not appear here; it lives on SETTINGS now.
    assert!(
        !text.contains("edit agents config"),
        "the config panel must not render on the AGENTS tab:\n{text}"
    );

    // Below the compact threshold the table drops its density columns (the
    // Goal column must survive, CPU%/MEM/etc. go). The dashboard now fills the
    // whole tab, so this needs a genuinely narrow terminal to trip.
    let text = render_tab(&model, DeckTab::Agents, 130, 20);
    assert!(!text.contains("CPU%"), "compact set drops CPU%:\n{text}");
    assert!(
        text.contains("Goal"),
        "compact set keeps the Goal column:\n{text}"
    );
}

#[test]
fn settings_tab_hosts_the_agents_config_editor() {
    let model = folded_model();
    // The config editor is the full-width body of the SETTINGS tab now.
    let text = render_tab(&model, DeckTab::Settings, 120, 24);
    assert!(
        text.contains("agents"),
        "the config panel title renders on SETTINGS:\n{text}"
    );
    // Its GLOBAL / per-agent sub-tabs and the focus hint render.
    assert!(
        text.contains("GLOBAL"),
        "the engine panel's GLOBAL sub-tab renders:\n{text}"
    );
    assert!(
        text.contains("edit agents config"),
        "the unfocused focus hint renders:\n{text}"
    );
}

/// The `?` help overlay is context-aware: it lists the active tab's own keys
/// plus the deck-wide keys — and nothing from other tabs — as one aligned
/// `key  description` row per shortcut.
#[test]
fn help_overlay_shows_only_the_active_tabs_shortcuts() {
    let model = folded_model();
    let render_help = |tab: DeckTab| {
        let mut ui = DeckUi::default();
        ui.splash.skip();
        ui.tab = tab;
        ui.help_open = true;
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
        let buf = terminal.backend().buffer();
        let area = *buf.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let traces = render_help(DeckTab::Traces);
    assert!(
        traces.contains("TRACES tab"),
        "titled for the tab:\n{traces}"
    );
    assert!(traces.contains("cycle the per-agent filter"), "{traces}");
    // Deck-wide keys are always present…
    assert!(traces.contains("switch tabs"), "{traces}");
    assert!(traces.contains("quit stella"), "{traces}");
    // …but other tabs' keys are not.
    assert!(!traces.contains("search skills"), "{traces}");
    assert!(!traces.contains("OAuth login"), "{traces}");

    let skills = render_help(DeckTab::Skills);
    assert!(skills.contains("SKILLS tab"), "{skills}");
    assert!(skills.contains("search skills"), "{skills}");
    assert!(!skills.contains("cycle the per-agent filter"), "{skills}");
}

/// The INSPECT overlay in both of its modes, through the real `render_deck`
/// entrypoint. This is the closest headless equivalent of opening it in a TTY:
/// if the prompt bytes are not on the screen, the feature does not work.
#[test]
fn inspect_overlay_renders_the_call_list_then_the_context_sent() {
    use stella_tui::{InspectMessage, InspectView, RecordedCallInfo};

    let model = folded_model();
    let call = |step: u64, call_seq: u64, role: &str| RecordedCallInfo {
        turn_instance: 0,
        step,
        call_seq,
        call_role: role.into(),
        provider: "anthropic".into(),
        model: "claude-opus".into(),
        estimated_input_tokens: 1234,
    };

    let render = |ui: &mut DeckUi| {
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| render_deck(&model, ui, f)).unwrap();
        let buf = terminal.backend().buffer();
        let area = *buf.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // Mode 1: the call list. Both calls of step 3 must be distinguishable —
    // that is the whole point of keying receipts on call_seq.
    let mut ui = DeckUi::default();
    ui.splash.skip();
    ui.inspect_open = true;
    ui.inspect_calls = vec![
        call(0, 0, "worker"),
        call(3, 1, "summarization"),
        call(3, 0, "worker"),
    ];
    ui.inspect_sel = 1;
    let list = render(&mut ui);
    assert!(list.contains("recorded calls"), "titled:\n{list}");
    assert!(
        list.contains("summarization"),
        "the auxiliary call is listed:\n{list}"
    );
    assert!(list.contains("worker"), "{list}");
    assert!(
        list.contains("show the context it was sent"),
        "the footer says what ⏎ does:\n{list}"
    );

    // Mode 2: the reconstructed context. The system prompt must be readable.
    ui.inspect_view = Some(Box::new(InspectView {
        call: call(3, 1, "summarization"),
        messages: vec![
            InspectMessage {
                role: "system".into(),
                content: "Condense this span faithfully.".into(),
            },
            InspectMessage {
                role: "user".into(),
                content: "t0 t1 t2".into(),
            },
        ],
        verified: true,
        unresolved: 0,
        digest_mismatches: 0,
    }));
    let detail = render(&mut ui);
    assert!(detail.contains("context sent"), "titled:\n{detail}");
    assert!(
        detail.contains("Condense this span faithfully."),
        "the system prompt is on screen — the feature:\n{detail}"
    );
    assert!(
        detail.contains("turn 0 · step 3 · call-seq 1"),
        "the coordinate is shown:\n{detail}"
    );
    assert!(
        detail.contains("verified"),
        "the verdict is shown:\n{detail}"
    );

    // A torn journal must read differently from a coverage gap.
    if let Some(view) = ui.inspect_view.as_mut() {
        view.verified = false;
        view.digest_mismatches = 2;
    }
    let torn = render(&mut ui);
    assert!(
        torn.contains("did not re-hash"),
        "a digest mismatch is called out, not folded into 'unverified':\n{torn}"
    );
}
