// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Every deck tab, drawn at every plausible terminal geometry, must not panic.
//!
//! The golden frames pin what the deck *looks like* at one or two sizes. They
//! say nothing about the sizes nobody chose to golden, and a terminal is
//! resized by dragging — so every width between the two goldens is a size a
//! real reader will hold the deck at, including the ones no test has ever
//! drawn.
//!
//! A panic here is not a cosmetic defect. The deck owns the terminal in raw
//! mode; a panic mid-draw leaves the reader with a wedged terminal and loses
//! whatever the session had not written down. That is why this sweeps rather
//! than sampling: the arithmetic in a renderer is width-dependent, and the
//! failing width is exactly the one nobody thought to pin.

use stella_tui::deck_render::render_deck;
use stella_tui::{DeckTab, DeckUi, WorkspaceModel};

use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// Every tab the deck can show.
const TABS: [DeckTab; 9] = [
    DeckTab::Session,
    DeckTab::Agents,
    DeckTab::Traces,
    DeckTab::Graph,
    DeckTab::Files,
    DeckTab::Skills,
    DeckTab::Mcp,
    DeckTab::Issues,
    DeckTab::Settings,
];

/// The deck's boolean overlays, by the name a failure should report.
///
/// An overlay is the width-sensitive half of the deck: it draws into a popup
/// `Rect` derived from the terminal's, so its arithmetic is the arithmetic most
/// likely to underflow at a size nobody goldened. A sweep with every overlay
/// shut proves the tabs and nothing else.
/// How an overlay is opened: a name for the failure report, and the flag to set.
type OverlayCase = (&'static str, fn(&mut DeckUi));

const OVERLAYS: [OverlayCase; 10] = [
    ("none", |_| {}),
    ("help", |ui| ui.help_open = true),
    ("graph_picker", |ui| ui.graph_picker_open = true),
    ("files_diff", |ui| ui.files_diff_open = true),
    ("queue", |ui| ui.queue_open = true),
    ("sessions", |ui| ui.sessions_open = true),
    ("sessions_all", |ui| {
        ui.sessions_open = true;
        ui.sessions_show_all = true;
    }),
    ("context", |ui| ui.context_open = true),
    ("inbox", |ui| ui.inbox_open = true),
    ("inspect", |ui| ui.inspect_open = true),
];

/// Draw one tab at one geometry, returning whether it panicked.
fn draws_with(tab: DeckTab, w: u16, h: u16, open: fn(&mut DeckUi)) -> bool {
    // A populated model, not an empty one. An empty deck exercises almost no
    // width arithmetic — every renderer takes its early return — so a sweep
    // over `WorkspaceModel::new()` proves the frames, not the maths. The demo
    // scenario is the same content the golden frames are drawn from.
    let mut model = WorkspaceModel::new();
    for inbound in stella_tui::scenario::demo_inbound(0, std::process::id()) {
        model.apply_inbound(&inbound);
    }
    let mut ui = DeckUi {
        tab,
        ..DeckUi::default()
    };
    open(&mut ui);
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("backend");
        terminal
            .draw(|frame| render_deck(&model, &mut ui, frame))
            .expect("draw");
    }))
    .is_ok()
}

/// **The sweep.** No tab panics at any geometry a terminal can be dragged to.
///
/// The range starts at 1x1. A one-column terminal is not a
/// realistic *working* size, but it is a size a window manager will hand the
/// deck for a frame or two mid-drag, and "we never draw at that size" has to be
/// true rather than assumed — nothing in the render path refuses a `Rect`.
#[test]
fn no_tab_panics_at_any_terminal_geometry() {
    // Silence the panic hook: a caught panic still prints its message, and a
    // sweep that reports 400 backtraces buries the one line that matters.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let mut failures: Vec<(DeckTab, &str, u16, u16)> = Vec::new();
    for tab in TABS {
        for (name, open) in OVERLAYS {
            for w in [
                1u16, 2, 3, 5, 8, 13, 20, 32, 40, 64, 80, 100, 120, 160, 200, 400,
            ] {
                for h in [1u16, 2, 3, 5, 8, 13, 24, 40, 60] {
                    if !draws_with(tab, w, h, open) {
                        failures.push((tab, name, w, h));
                    }
                }
            }
        }
    }

    std::panic::set_hook(hook);

    assert!(
        failures.is_empty(),
        "{} geometry/tab combination(s) panicked; smallest first:\n{}",
        failures.len(),
        failures
            .iter()
            .take(20)
            .map(|(tab, overlay, w, h)| format!("  {tab:?} + {overlay} at {w}x{h}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}
