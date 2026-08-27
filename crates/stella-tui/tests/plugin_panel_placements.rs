// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The three placements, each drawn from a real plugin's real frame — SPEC
//! 12.2.
//!
//! `hello_panel_demo` proves the protocol end to end against one rectangle
//! chosen by the test. This proves the *placement*: the plugin's frame goes
//! through the deck's own state and its own layout, so a green run says the
//! panel is on the screen a person is looking at rather than in a buffer a
//! test constructed.
//!
//! Every case drives `plugins/stella-hello/`, which declares all three
//! surfaces, and renders through `stella_tui`'s public deck path — the same
//! `render_deck` / `views::settings::render` a session calls.
//!
//! Read the panels:
//!
//! ```text
//! cargo test -p stella-tui --test plugin_panel_placements -- --nocapture
//! ```

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use stella_plugin::{PanelLease, PanelRect, PanelSurface, PluginManifest, Runtime};
use stella_tui::deck::{DeckTab, WorkspaceModel};
use stella_tui::deck_ui::DeckUi;
use stella_tui::envelope::{Inbound, PanelSeat, WorkspaceInput};
use stella_tui::views::settings::SettingsPane;

/// The manifest and the process, resolved from the repository rather than
/// written inline — a test that invented its own manifest would prove nothing
/// about the one that ships.
fn hello_plugin() -> (PluginManifest, Runtime) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/stella-hello")
        .canonicalize()
        .expect("the hello plugin ships in this repository");
    let manifest = PluginManifest::from_toml_str(
        &std::fs::read_to_string(dir.join("plugin.toml")).expect("its manifest is readable"),
    )
    .expect("its manifest parses");
    let panel = manifest.panel.clone().expect("it declares a panel");
    let mut process = panel.process.clone().expect("it declares a process");
    process.argv = process
        .argv
        .iter()
        .map(|arg| arg.replace("${plugin_dir}", &dir.display().to_string()))
        .collect();
    (manifest, process)
}

fn python_is_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Seat all three of the hello plugin's surfaces, the way the driver's
/// `plugin_panels::seat` does for an admitted install.
fn seat_hello(ui: &mut DeckUi, manifest: &PluginManifest) {
    let seats = vec![
        PanelSeat {
            plugin: manifest.name.clone(),
            surface: PanelSurface::Settings,
            command: None,
        },
        PanelSeat {
            plugin: manifest.name.clone(),
            surface: PanelSurface::Overlay,
            command: None,
        },
        PanelSeat {
            plugin: manifest.name.clone(),
            surface: PanelSurface::Command,
            command: Some("hello".to_string()),
        },
    ];
    let mut model = WorkspaceModel::new();
    stella_tui::deck_ui::ingest_inbound(&Inbound::PanelsSeated(seats), &mut model, ui);
}

/// Run one full loop turn for `slot`: draw once so the deck measures the lease,
/// take the request that raises, ask the real plugin, land the frame, draw
/// again.
///
/// This is the whole architecture in one function — the draw never awaits, the
/// ask happens between draws, and the frame is in state before the next one.
async fn pump(ui: &mut DeckUi, slot: usize, process: &Runtime, mut draw: impl FnMut(&mut DeckUi)) {
    draw(ui);
    let (tick, cols, rows) = ui
        .panels
        .requests()
        .into_iter()
        .find_map(|input| match input {
            WorkspaceInput::PanelFrameWanted {
                slot: asked,
                tick,
                cols,
                rows,
            } if asked == slot => Some((tick, cols, rows)),
            _ => None,
        })
        .expect("the drawn panel asked for a frame");
    assert!(cols > 0 && rows > 0, "the lease has a rectangle");

    let lease = PanelLease::new(
        ui.panels.slots()[slot].plugin(),
        ui.panels.slots()[slot].surface(),
        tick,
        PanelRect::new(cols, rows),
        33,
    );
    let answered = stella_runtime::panel_host::ask(process, lease.clone())
        .await
        .expect("the hello plugin answers a frame");
    let frame = answered.frame.expect("it drew one");
    lease.admits(&frame).expect("the frame answers this lease");
    let mut model = WorkspaceModel::new();
    stella_tui::deck_ui::ingest_inbound(
        &Inbound::PanelFrame {
            slot,
            frame: Box::new(frame),
        },
        &mut model,
        ui,
    );
    draw(ui);
}

fn text_of(buf: &Buffer) -> String {
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

fn ready_ui() -> DeckUi {
    let mut ui = DeckUi::default();
    ui.splash.skip();
    ui
}

/// **The `settings` witness.** A plugin that declares the surface gets a pane
/// of its own in the SETTINGS tab, reachable by name on the nav, and the pane
/// is the plugin's own frame inside the host's chrome.
#[tokio::test]
async fn the_settings_pane_draws_the_plugins_own_frame() {
    if !python_is_available() {
        eprintln!("skipped: python3 is not on PATH, and the hello plugin is written in it");
        return;
    }
    let (manifest, process) = hello_plugin();
    let mut ui = ready_ui();
    ui.set_tab(DeckTab::Settings);
    seat_hello(&mut ui, &manifest);

    // The nav grew a pane, after the three the product ships and never before
    // them.
    let panes = SettingsPane::panes(&ui.panels);
    assert_eq!(
        &panes[..3],
        &SettingsPane::BUILTIN,
        "the built-ins stay first"
    );
    assert_eq!(
        panes.len(),
        4,
        "one pane per settings-surface plugin: {panes:?}"
    );
    assert_eq!(panes[3].label(&ui.panels), "hello");
    ui.settings_pane = panes[3];

    let area = Rect::new(0, 0, 72, 16);
    let model = WorkspaceModel::new();
    let mut painted = String::new();
    pump(&mut ui, 0, &process, |ui| {
        let mut buf = Buffer::empty(area);
        stella_tui::views::settings::render(&model, ui, area, &mut buf);
        painted = text_of(&buf);
    })
    .await;

    println!("\n{painted}\n");
    assert!(painted.contains("hello from a plugin"), "{painted}");
    assert!(
        painted.contains("◳ panel · hello"),
        "the host's chrome: {painted}"
    );
    let nav = painted.lines().next().unwrap_or_default();
    assert!(
        nav.contains("AGENTS") && nav.contains("hello"),
        "nav: {nav:?}"
    );
}

/// **The `overlay` witness.** The same plugin's frame appears as a bordered
/// block in the SESSION tab, above the conversation, without displacing the
/// composer or the status bar.
#[tokio::test]
async fn the_overlay_block_draws_in_the_session_transcript() {
    if !python_is_available() {
        eprintln!("skipped: python3 is not on PATH, and the hello plugin is written in it");
        return;
    }
    let (manifest, process) = hello_plugin();
    let mut ui = ready_ui();
    seat_hello(&mut ui, &manifest);

    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("a test terminal");
    let model = lead_workspace();
    let mut painted = String::new();
    // Slot 1 is the overlay seat — settings, overlay, command, in seating
    // order.
    pump(&mut ui, 1, &process, |ui| {
        terminal
            .draw(|f| stella_tui::deck_render::render_deck(&model, ui, f))
            .expect("the deck draws");
        painted = text_of(terminal.backend().buffer());
    })
    .await;

    println!("\n{painted}\n");
    assert!(painted.contains("hello from a plugin"), "{painted}");
    assert!(
        painted.contains("◳ panel · hello"),
        "the host's chrome: {painted}"
    );
    assert!(
        painted.contains(stella_tui::views::frame::PROMPT_PREFIX.trim()),
        "the composer is still where the reader left it: {painted}"
    );
}

/// **The `command` witness.** `/hello` opens the centred popup, the plugin's
/// frame is inside it, and Esc closes it — SPEC 13's rule for every overlay.
#[tokio::test]
async fn slash_hello_opens_the_centred_popup() {
    if !python_is_available() {
        eprintln!("skipped: python3 is not on PATH, and the hello plugin is written in it");
        return;
    }
    let (manifest, process) = hello_plugin();
    let mut ui = ready_ui();
    seat_hello(&mut ui, &manifest);
    let model = lead_workspace();

    // Nothing is open until the name is typed.
    assert!(ui.panels.open_popup().is_none());
    assert_eq!(
        submit(&mut ui, &model, "/hello"),
        stella_tui::deck_ui::DeckAction::Handled,
        "`/hello` is claimed by the deck rather than sent to the driver"
    );
    assert_eq!(ui.panels.open_popup(), Some(2), "the command seat is open");

    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("a test terminal");
    let mut painted = String::new();
    pump(&mut ui, 2, &process, |ui| {
        terminal
            .draw(|f| stella_tui::deck_render::render_deck(&model, ui, f))
            .expect("the deck draws");
        painted = text_of(terminal.backend().buffer());
    })
    .await;

    println!("\n{painted}\n");
    assert!(painted.contains("hello from a plugin"), "{painted}");
    assert!(
        painted.contains("◳ panel · hello"),
        "the host's chrome: {painted}"
    );

    stella_tui::deck_ui::handle_deck_key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ),
        &model,
        &mut ui,
    );
    assert!(ui.panels.open_popup().is_none(), "esc closes it");
}

/// The namespaced alias is derived and always available, whatever the manifest
/// named — `PanelGrant::command` refuses a manifest that spells it itself.
#[test]
fn the_plugin_namespaced_alias_opens_the_same_popup() {
    let (manifest, _) = hello_plugin();
    let mut ui = ready_ui();
    seat_hello(&mut ui, &manifest);
    let model = lead_workspace();

    assert_eq!(
        submit(&mut ui, &model, "/plugin:hello"),
        stella_tui::deck_ui::DeckAction::Handled
    );
    assert_eq!(ui.panels.open_popup(), Some(2));
}

/// A name no seat answers to keeps falling through to the driver, exactly as
/// an unknown slash always did.
#[test]
fn an_unseated_name_is_not_claimed_by_the_panel_deck() {
    let (manifest, _) = hello_plugin();
    let mut ui = ready_ui();
    seat_hello(&mut ui, &manifest);
    let model = lead_workspace();

    assert_ne!(
        submit(&mut ui, &model, "/nobody"),
        stella_tui::deck_ui::DeckAction::Handled
    );
    assert!(ui.panels.open_popup().is_none());
}

/// A workspace with one registered agent, so the SESSION tab draws its
/// transcript rather than its empty state.
fn lead_workspace() -> WorkspaceModel {
    let mut model = WorkspaceModel::new();
    model.apply_inbound(&Inbound::Register(stella_tui::AgentMeta::new(
        "lead",
        "wire the panel placements",
        0,
    )));
    model
}

/// Type `text` into the composer and submit it, the way a person does.
///
/// Through `handle_deck_key` rather than a test-only entry point, so what is
/// under test is the path a keystroke actually takes — including every handler
/// that could have claimed the name first.
fn submit(ui: &mut DeckUi, model: &WorkspaceModel, text: &str) -> stella_tui::deck_ui::DeckAction {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for ch in text.chars() {
        stella_tui::deck_ui::handle_deck_key(
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            model,
            ui,
        );
    }
    stella_tui::deck_ui::handle_deck_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        model,
        ui,
    )
}
