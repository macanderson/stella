//! Render robustness: the deck must survive every terminal geometry a user can
//! produce with a mouse drag, and every byte an agent can put in a transcript.
//!
//! A TUI's two cheapest ways to die are a `Rect` arithmetic panic at some size
//! nobody tried and a slice at a non-char boundary in some text nobody typed.
//! Both are unit-testable with `TestBackend`, so both are pinned here rather
//! than discovered in a resize. Every case renders through the real
//! `render_deck` entrypoint — the same path the shell draws.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use stella_tui::scenario::{demo_graph, demo_inbound};
use stella_tui::{DeckTab, DeckUi, WorkspaceModel, render_deck};

fn folded_model() -> WorkspaceModel {
    let mut model = WorkspaceModel::new();
    model.now_ms = 312_000;
    for inbound in demo_inbound(0, std::process::id()) {
        model.apply_inbound(&inbound);
    }
    model
}

#[test]
fn tiny_terminals_never_panic() {
    let model = folded_model();
    for tab in DeckTab::ALL {
        for w in [1u16, 2, 3, 5, 8, 13, 20, 40, 41, 79, 200] {
            for h in [1u16, 2, 3, 4, 5, 6, 7, 8, 12, 24, 60] {
                let mut ui = DeckUi::default();
                ui.splash.skip();
                ui.tab = tab;
                ui.graph = Some(demo_graph());
                let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    terminal.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
                }));
                assert!(r.is_ok(), "panic at {tab:?} {w}x{h}");
            }
        }
    }
}

#[test]
fn tiny_terminals_with_overlays_never_panic() {
    let model = folded_model();
    for w in [1u16, 2, 4, 9, 17, 40, 120] {
        for h in [1u16, 2, 3, 5, 9, 24] {
            for which in 0..8 {
                let mut ui = DeckUi::default();
                ui.splash.skip();
                ui.graph = Some(demo_graph());
                match which {
                    0 => ui.help_open = true,
                    1 => ui.queue_open = true,
                    2 => ui.graph_picker_open = true,
                    3 => ui.sessions_open = true,
                    4 => ui.inbox_open = true,
                    5 => ui.context_open = true,
                    6 => ui.inspect_open = true,
                    _ => {
                        ui.search.open = true;
                        ui.search.query = "a".into();
                    }
                }
                let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    terminal.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
                }));
                assert!(r.is_ok(), "panic with overlay {which} at {w}x{h}");
            }
        }
    }
}

#[test]
fn splash_tiny_never_panics() {
    let model = WorkspaceModel::new();
    for w in [1u16, 2, 5, 20, 80] {
        for h in [1u16, 2, 5, 20, 40] {
            let mut ui = DeckUi::default();
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                terminal.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
            }));
            assert!(r.is_ok(), "splash panic {w}x{h}");
        }
    }
}

use stella_protocol::{AgentEvent, ToolCall, ToolOutput};
use stella_tui::envelope::{AgentMeta, Inbound};

fn nasty_texts() -> Vec<String> {
    vec![
        "日本語のテキストがとても長い行になるときの折り返し動作を確認する".into(),
        "emoji 👨‍👩‍👧‍👦 family and 🇯🇵 flag and 🏳️‍🌈 rainbow".into(),
        "combining e\u{0301}\u{0301}\u{0301} marks".into(),
        "\u{1b}[31mred\u{1b}[0m and \u{1b}[1;32mgreen\u{1b}[0m ansi".into(),
        "tab\there\tand\rcarriage".into(),
        "a".repeat(500),
        "".into(),
        "\u{200b}zero width\u{200b}space".into(),
        "мир مرحبا שלום".into(),
    ]
}

#[test]
fn nasty_unicode_transcript_never_panics_at_any_width() {
    for text in nasty_texts() {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "t", 0)));
        for ev in [
            AgentEvent::Text {
                delta: text.clone(),
            },
            AgentEvent::Reasoning {
                delta: text.clone(),
            },
            AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: "c".into(),
                    name: text.clone(),
                    input: serde_json::json!({"path": text}),
                },
            },
            AgentEvent::ToolResult {
                call_id: "c".into(),
                output: ToolOutput::Error {
                    message: text.clone(),
                },
                duration_ms: 5,
                speculated: false,
            },
        ] {
            model.apply_inbound(&Inbound::Event {
                agent: "lead".into(),
                event: ev,
            });
        }
        for w in [1u16, 2, 3, 4, 6, 10, 20, 41, 80] {
            for h in [3u16, 9, 24] {
                let mut ui = DeckUi::default();
                ui.splash.skip();
                ui.transcript_expand_all = true;
                let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    terminal.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
                }));
                assert!(r.is_ok(), "panic {w}x{h} for {text:?}");
            }
        }
    }
}
