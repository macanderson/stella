#![allow(clippy::field_reassign_with_default)]

use super::*;
use crate::envelope::{AgentMeta, Inbound};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use stella_protocol::{AgentEvent, StageKind};

/// End-to-end: render the whole deck frame (tab bar · view · progress bar ·
/// composer · footer · statline) and assert every §-level element is
/// present, the rocket/garble is gone, and nothing panics at 80 columns.
/// Run with `--nocapture` to eyeball the frame.
#[test]
fn full_deck_frame_composes_every_band_at_80_cols() {
    let mut model = running_model_with_queue();
    if let Some(a) = model.agents.first_mut() {
        a.tokens_out = 900;
        a.tokens_in = 42_000;
        a.cost_usd = 0.14;
        a.meta.started_ms = 0;
    }
    model.now_ms = 10_000;
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::Text {
            delta: "Found the root cause.".into(),
        },
    });

    let mut ui = DeckUi::default();
    ui.splash.skip(); // the splash owns the frame until done
    for c in "add nav".chars() {
        ui.composer.insert_char(c);
    }

    // 190 exercises the full labeled statline; 80 the cell-drop path.
    for (w, h) in [(80u16, 24u16), (190, 40)] {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
        let text = buffer_text(term.backend().buffer());
        if w == 80 {
            eprintln!("\n──── deck @ {w}×{h} ────\n{text}\n");
        }
        for needle in [
            ">>>",      // accent prompt prefix (§4)
            "add nav",  // typed prompt text
            "new line", // composer footer affordance (§4)
            "queue",    // footer / queue status
            "plan",     // progress stage labels (§3)
            "execute", "verify", // progress stage labels (§3)
            "MODEL",  // the statline's leading cell (§5)
        ] {
            assert!(
                text.contains(needle),
                "deck @{w}×{h} missing {needle:?}:\n{text}"
            );
        }
        // The full cell set needs the width; at 190 every cell renders, each
        // as an uppercase micro-label over its value.
        if w >= 120 {
            for needle in ["CONTEXT", "CPU", "CACHE", "SPEND", "SAVED", "WARMTH"] {
                assert!(
                    text.contains(needle),
                    "deck @{w}×{h} missing statline cell {needle:?}:\n{text}"
                );
            }
        }
        // The killed rocket/garble leave no trace.
        assert!(
            !text.contains("}=>"),
            "rocket sprite still rendered:\n{text}"
        );
        assert!(!text.contains("<●>"), "UFO sprite still rendered:\n{text}");
    }
}

fn buffer_text(buf: &Buffer) -> String {
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
fn full_deck_frame_grows_a_second_statline_row_for_a_diagnosed_agent() {
    // The acceptance case: an opt-in provider (anthropic), 4 calls (past
    // MIN_TURNS=3), 0% hit rate, 0 cache writes — the marker never
    // engaged. `render_deck` must reserve the statline's second row and
    // `statline::render` must fill it with the full-sentence hint, not a
    // clipped fragment.
    let mut model = running_model_with_queue();
    for step in 1..=4usize {
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::StepUsage {
                output_text: None,
                step,
                role: stella_protocol::event::ModelCallRole::Worker,
                provider: "anthropic".into(),
                model: "claude-fable-5".into(),
                input_tokens: 10_000,
                output_tokens: 500,
                cached_input_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: None,
                estimated_input_tokens: 0,
                cost_usd: 0.05,
                duration_ms: 100,
                retries: 0,
                tool_calls: 0,
                complete: true,
                finish_reason: None,
            },
        });
    }
    model.apply_inbound(&Inbound::CacheInsight {
        agent: "lead".into(),
        savings_usd_delta: 0.0,
        ttl_secs: 300,
        is_opt_in_provider: true,
    });

    let mut ui = DeckUi::default();
    ui.splash.skip();
    let mut term = Terminal::new(TestBackend::new(190, 40)).unwrap();
    term.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
    let text = buffer_text(term.backend().buffer());
    assert!(
        text.contains("cache opt-in never engaged"),
        "diagnosis row missing from the full frame:\n{text}"
    );

    // A healthy session (no StepUsage at all) never grows the row or
    // shows the sentence — the common case stays the compact two rows.
    let healthy = running_model_with_queue();
    let mut healthy_ui = DeckUi::default();
    healthy_ui.splash.skip();
    let mut healthy_term = Terminal::new(TestBackend::new(190, 40)).unwrap();
    healthy_term
        .draw(|f| render_deck(&healthy, &mut healthy_ui, f))
        .unwrap();
    let healthy_text = buffer_text(healthy_term.backend().buffer());
    assert!(
        !healthy_text.contains("cache opt-in never engaged")
            && !healthy_text.contains("prompt prefix is unstable"),
        "a healthy session must not show a diagnosis:\n{healthy_text}"
    );
}

#[test]
fn trace_strip_shows_a_rule_and_the_newest_trace_entry() {
    let mut model = running_model_with_queue();
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::Text {
            delta: "Found the root cause.".into(),
        },
    });
    let area = Rect::new(0, 0, 60, 2);
    let mut buf = Buffer::empty(area);
    render_trace_strip(&model, area, &mut buf);
    let text = buffer_text(&buf);
    let rows: Vec<&str> = text.lines().collect();
    assert!(
        rows[0].starts_with("──"),
        "hairline rule on top: {:?}",
        rows[0]
    );
    assert!(rows[1].contains("text"), "kind label shown:\n{text}");
    assert!(
        rows[1].contains("Found the root cause."),
        "newest entry summarized:\n{text}"
    );

    // No trace yet → a quiet idle line, never a panic.
    let empty = WorkspaceModel::new();
    let mut buf = Buffer::empty(area);
    render_trace_strip(&empty, area, &mut buf);
    assert!(buffer_text(&buf).contains("no activity yet"));
}

fn running_model_with_queue() -> WorkspaceModel {
    let mut m = WorkspaceModel::new();
    m.now_ms = 1_000;
    m.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
    m.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::Stage {
            name: StageKind::Execute,
        },
    });
    m.queue.enqueue("write the tests".into(), 1);
    m.queue.enqueue("open a pr".into(), 2);
    m
}

#[test]
fn empty_composer_is_a_single_accent_prompt_line_with_the_caret() {
    let ui = DeckUi::default(); // blank composer
    let layout = crate::composer::layout(&ui.composer, 40);
    let area = Rect::new(0, 0, 40, 4);
    let mut buf = Buffer::empty(area);
    render_composer(&layout, area, &mut buf);
    let text = buffer_text(&buf);
    let rows: Vec<&str> = text.lines().collect();
    assert!(
        rows[0].starts_with(">>> "),
        "row 0 is the accent prompt: {:?}",
        rows[0]
    );
    // Exactly one prompt line — the rest of the box is empty.
    assert!(
        rows[1..].iter().all(|r| !r.contains(">>>")),
        "only one prompt line:\n{text}"
    );
    // The caret sits right after the prefix (a reversed cell at col 4).
    assert!(
        buf.cell((4, 0))
            .is_some_and(|c| c.modifier.contains(Modifier::REVERSED)),
        "caret right after the prefix"
    );
}

#[test]
fn a_multiline_paste_prefixes_every_row_and_scrolls_instead_of_chipping() {
    // 8 lines — well past the old 6-line chip threshold, but the deck's
    // composer keeps it inline (one `>>>` per line) and scrolls, rather
    // than collapsing it to a `[pasted: N lines]` chip (acceptance §4/#5).
    let mut ui = DeckUi::default();
    let paste: String = (1..=8).map(|n| format!("line{n}\n")).collect();
    ui.composer.paste(&paste);
    assert!(
        ui.composer.chips().is_empty(),
        "no chip — rendered per line"
    );

    let layout = crate::composer::layout(&ui.composer, 40);
    let area = Rect::new(0, 0, 40, 4); // capped at DECK_COMPOSER_MAX_ROWS
    let mut buf = Buffer::empty(area);
    render_composer(&layout, area, &mut buf);
    let text = buffer_text(&buf);
    assert!(!text.contains("[pasted"), "not chipped:\n{text}");
    for (i, row) in text.lines().enumerate() {
        assert!(row.starts_with(">>> "), "row {i} prefixed: {row:?}");
    }
    // Beyond 4 rows the box scrolls — the gutter shows a violet thumb.
    assert!(
        (0..area.height).any(|yy| buf
            .cell((area.width - 1, yy))
            .is_some_and(|c| c.symbol() == "▐")),
        "scroll indicator present:\n{text}"
    );
}

#[test]
fn the_prompt_prefix_is_a_steady_uniform_accent() {
    // The prefix used to march a bright chevron over two dim ones while the
    // agent worked. It carried nothing the progress bar and the STAGE cell
    // did not already carry, and a terminal has no `prefers-reduced-motion`
    // for a reader who needs motion off — so it is one still `>>> `.
    let ui = DeckUi::default();
    let layout = crate::composer::layout(&ui.composer, 40);
    let area = Rect::new(0, 0, 40, 4);
    let mut buf = Buffer::empty(area);
    render_composer(&layout, area, &mut buf);
    assert!(
        buffer_text(&buf)
            .lines()
            .next()
            .unwrap()
            .starts_with(">>> "),
        "the prefix keeps its glyphs and its 4-column width"
    );
    assert!(
        (0..3u16).all(|x| buf.cell((x, 0)).unwrap().fg == crate::theme::ACCENT),
        "every chevron is the same accent — no lit/dim pair to animate"
    );
}

#[test]
fn composer_cursor_position_sits_right_after_the_prefix_on_an_empty_composer() {
    let layout = ComposerLayout {
        rows: vec![String::new()],
        cursor_row: 0,
        cursor_col: 0,
    };
    let area = Rect::new(0, 0, 40, 4);
    // Matches `empty_composer_is_a_single_accent_prompt_line_with_the_caret`,
    // which asserts the drawn reversed-cell caret sits at column 4.
    assert_eq!(composer_cursor_position(&layout, area), Some((4, 0)));
}

#[test]
fn composer_cursor_position_offsets_by_the_composer_areas_origin() {
    let layout = ComposerLayout {
        rows: vec!["hi".into()],
        cursor_row: 0,
        cursor_col: 2,
    };
    let area = Rect::new(10, 20, 30, 3);
    assert_eq!(composer_cursor_position(&layout, area), Some((16, 20)));
}

#[test]
fn composer_cursor_position_tracks_the_caret_through_the_scroll_window() {
    // 9 rows in a 4-row viewport, caret on the last row — the same shape
    // `a_multiline_paste_prefixes_every_row_and_scrolls_instead_of_chipping`
    // renders behind a `▐` gutter thumb. `first` must land on the same
    // window `render_composer` scrolls to, or the caret drifts from what's
    // actually drawn.
    let layout = ComposerLayout {
        rows: (0..9).map(|i| format!("row{i}")).collect(),
        cursor_row: 8,
        cursor_col: 4, // "row8" is 4 columns wide
    };
    let area = Rect::new(0, 0, 40, 4);
    // visible=4, first = cursor_row + 1 - visible = 5, row_in_view = 3.
    assert_eq!(composer_cursor_position(&layout, area), Some((8, 3)));
}

#[test]
fn composer_cursor_position_is_none_for_a_degenerate_area() {
    let layout = ComposerLayout {
        rows: vec![String::new()],
        cursor_row: 0,
        cursor_col: 0,
    };
    assert_eq!(
        composer_cursor_position(&layout, Rect::new(0, 0, 0, 4)),
        None
    );
    assert_eq!(
        composer_cursor_position(&layout, Rect::new(0, 0, 40, 0)),
        None
    );
}

#[test]
fn render_deck_positions_the_hardware_cursor_at_the_composer_caret() {
    let model = running_model_with_queue();
    let mut ui = DeckUi::default();
    ui.splash.skip();
    for c in "hi".chars() {
        ui.composer.insert_char(c);
    }

    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| render_deck(&model, &mut ui, f)).unwrap();

    assert!(
        term.backend().cursor_visible(),
        "nothing owns the keyboard ahead of the composer — the terminal \
         cursor must be shown, or CJK IME composition has nowhere to anchor"
    );
    let text = buffer_text(term.backend().buffer());
    let composer_row = text
        .lines()
        .position(|l| l.starts_with(">>> hi"))
        .expect("the typed composer row is on screen");
    let pos = term.backend().cursor_position();
    assert_eq!(
        pos.y as usize, composer_row,
        "cursor sits on the composer row"
    );
    assert_eq!(
        pos.x, 6,
        "cursor sits right after '>>> hi' (4-col prefix + 2 chars)"
    );
}

#[test]
fn render_deck_hides_the_hardware_cursor_under_a_modal_overlay() {
    let model = running_model_with_queue();
    let mut ui = DeckUi::default();
    ui.splash.skip();
    ui.help_open = true;

    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| render_deck(&model, &mut ui, f)).unwrap();

    assert!(
        !term.backend().cursor_visible(),
        "the help overlay owns the keyboard; the caret must not sit in the \
         composer underneath it"
    );
}

#[test]
fn composer_footer_shows_keys_line_counter_and_queue_status() {
    let model = running_model_with_queue();
    let ui = DeckUi::default();
    let layout = crate::composer::layout(&ui.composer, 40);
    let area = Rect::new(0, 0, 100, 1);
    let mut buf = Buffer::empty(area);
    render_composer_footer(&model, &ui, &layout, area, &mut buf);
    let text = buffer_text(&buf);
    assert!(text.contains("new line"), "newline affordance:\n{text}");
    assert!(
        text.contains("queue (never blocks)"),
        "queue affordance:\n{text}"
    );
    assert!(text.contains("1 line"), "live line counter:\n{text}");
    assert!(
        text.contains("2 queued"),
        "queue status on the right:\n{text}"
    );
}

#[test]
fn composer_footer_reports_a_held_queue() {
    let model = running_model_with_queue();
    let mut ui = DeckUi::default();
    ui.dispatch_held = true;
    let layout = crate::composer::layout(&ui.composer, 40);
    let area = Rect::new(0, 0, 100, 1);
    let mut buf = Buffer::empty(area);
    render_composer_footer(&model, &ui, &layout, area, &mut buf);
    assert!(buffer_text(&buf).contains("2 held"), "held status shown");
}

/// `>` is how a user redirects a turn that is going the wrong way, and it was
/// advertised nowhere — the footer offered only "queue (never blocks)", which
/// describes what happens to a prompt typed mid-turn without saying that the
/// alternative exists. It appears while the focused agent is running.
#[test]
fn the_footer_advertises_the_steer_marker_while_a_turn_runs() {
    let model = running_model_with_queue();
    let ui = DeckUi::default();
    let layout = crate::composer::layout(&ui.composer, 40);
    let area = Rect::new(0, 0, 100, 1);
    let mut buf = Buffer::empty(area);
    render_composer_footer(&model, &ui, &layout, area, &mut buf);
    let text = buffer_text(&buf);
    assert!(
        text.contains("steer this turn"),
        "steer affordance:\n{text}"
    );
}

/// …and only then. At an idle agent a leading `>` is not a steer marker, it
/// is the first character of the next prompt, so advertising it there would
/// be a lie that costs the row width the counter needs.
#[test]
fn the_footer_hides_the_steer_marker_when_nothing_is_running() {
    let mut model = WorkspaceModel::new();
    model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
    model.apply_inbound(&Inbound::Status {
        agent: "lead".into(),
        status: crate::AgentStatus::WaitingInput,
    });
    let ui = DeckUi::default();
    let layout = crate::composer::layout(&ui.composer, 40);
    let area = Rect::new(0, 0, 100, 1);
    let mut buf = Buffer::empty(area);
    render_composer_footer(&model, &ui, &layout, area, &mut buf);
    let text = buffer_text(&buf);
    assert!(
        !text.contains("steer"),
        "no steer affordance at rest:\n{text}"
    );
}

#[test]
fn queue_popup_lists_prompts_and_arms_the_clear_confirm() {
    let model = running_model_with_queue();
    let mut ui = DeckUi::default();
    ui.queue_open = true;
    ui.queue_sel = 1;
    let area = Rect::new(0, 0, 80, 20);
    let mut buf = Buffer::empty(area);
    crate::views::queue_popup::render(&model, &ui, area, &mut buf);
    let text = buffer_text(&buf);
    assert!(text.contains("queue · 2 pending"), "title:\n{text}");
    assert!(text.contains("write the tests"), "row 1:\n{text}");
    assert!(text.contains("▸ 2. open a pr"), "row 2 selected:\n{text}");
    assert!(text.contains("ctrl+x delete"), "legend:\n{text}");
    // Armed confirm swaps the legend for the warning.
    ui.queue_confirm_clear = true;
    let mut buf2 = Buffer::empty(area);
    crate::views::queue_popup::render(&model, &ui, area, &mut buf2);
    let warned = buffer_text(&buf2);
    assert!(
        warned.contains("press ctrl+d again"),
        "confirm warning:\n{warned}"
    );
}

/// A `DeckUi` on the Graph tab with an `n`-file snapshot rooted on the
/// middle file, its picker open.
fn ui_with_graph_picker(n: usize) -> DeckUi {
    use crate::graph::{GraphNode, GraphSnapshot};
    let files: Vec<String> = (0..n).map(|i| format!("crate/mod_{i:02}.rs")).collect();
    let focus = files[n / 2].clone();
    let mut ui = DeckUi::default();
    ui.tab = DeckTab::Graph;
    ui.graph = Some(GraphSnapshot {
        focus: focus.clone(),
        nodes: vec![GraphNode {
            label: focus,
            kind: "file".into(),
            location: None,
        }],
        edges: vec![],
        files,
    });
    ui.graph_picker_open = true;
    ui
}

#[test]
fn graph_picker_lists_files_marks_the_current_and_shows_the_legend() {
    let mut ui = ui_with_graph_picker(4);
    ui.graph_picker_sel = 2; // the focus file (crate/mod_02.rs)
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render_graph_picker(&ui, area, &mut buf);
    let text = buffer_text(&buf);
    assert!(text.contains("files · 4 indexed"), "title:\n{text}");
    assert!(text.contains("filter"), "filter query line:\n{text}");
    assert!(text.contains("crate/mod_00.rs"), "lists files:\n{text}");
    assert!(text.contains("· current"), "marks the rooted file:\n{text}");
    assert!(text.contains("type to filter"), "legend:\n{text}");
}

#[test]
fn graph_picker_windows_the_list_to_keep_the_selection_visible() {
    // Far more files than the popup's rows, selection at the end: the
    // window must slide (via the shared `scroll_window_start`) so the last
    // file shows and the first scrolls out.
    let mut ui = ui_with_graph_picker(60);
    ui.graph_picker_sel = 59;
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render_graph_picker(&ui, area, &mut buf);
    let text = buffer_text(&buf);
    assert!(
        text.contains("crate/mod_59.rs"),
        "selection scrolled in:\n{text}"
    );
    assert!(
        !text.contains("crate/mod_00.rs"),
        "head scrolled out:\n{text}"
    );
}

#[test]
fn graph_picker_narrows_to_the_filter_query() {
    let mut ui = ui_with_graph_picker(20);
    ui.graph_picker_query = "mod_1".to_string();
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    render_graph_picker(&ui, area, &mut buf);
    let text = buffer_text(&buf);
    // mod_10..mod_19 match; mod_00..mod_09 (except none contain "mod_1")
    // do not.
    assert!(text.contains("crate/mod_10.rs"), "matches shown:\n{text}");
    assert!(
        !text.contains("crate/mod_02.rs"),
        "non-matches hidden:\n{text}"
    );
}

#[test]
fn inspect_overlay_scroll_saturates_past_u16_instead_of_wrapping() {
    // A reconstructed context can exceed 65 535 rows; a plain `as u16` on
    // the scroll offset would wrap and snap the view back near the top.
    // Saturation pins it at the deepest reachable row instead.
    let mut ui = DeckUi::default();
    ui.inspect_open = true;
    let content: String = (0..70_000).map(|i| format!("ctx-{i:05}\n")).collect();
    ui.inspect_view = Some(Box::new(crate::envelope::InspectView {
        call: crate::envelope::RecordedCallInfo {
            turn_instance: 1,
            step: 1,
            call_seq: 0,
            call_role: "worker".into(),
            provider: "openrouter".into(),
            model: "m".into(),
            estimated_input_tokens: 0,
        },
        messages: vec![crate::envelope::InspectMessage {
            role: "user".into(),
            content,
        }],
        verified: false,
        unresolved: 0,
        digest_mismatches: 0,
    }));
    ui.inspect_scroll = 66_000;

    let area = Rect::new(0, 0, 100, 30);
    let mut buf = Buffer::empty(area);
    render_inspect_overlay(&mut ui, area, &mut buf);
    let text = buffer_text(&buf);
    assert!(
        text.contains("ctx-655"),
        "the window pins at the u16 ceiling (rows ≈65 535), not at a wrapped offset:\n{text}"
    );
    assert!(
        !text.contains("ctx-004"),
        "66_000 as u16 would have wrapped to ~464:\n{text}"
    );
}

#[test]
fn sessions_overlay_repeats_the_heading_when_scrolled_into_a_group_midway() {
    // Selection deep inside a long group: the group's first rows are above
    // the window, but the heading must still render above the visible rows —
    // without it the phase of what's on screen is unreadable.
    fn session(i: usize, phase: crate::envelope::SessionPhase) -> crate::envelope::SessionInfo {
        crate::envelope::SessionInfo {
            id: format!("ses-{i}"),
            title: format!("session {i:02}"),
            summary: String::new(),
            workspace: "/tmp/w".into(),
            phase,
            started_ms: 0,
            updated_ms: 0,
            mine: false,
            resumable: false,
        }
    }
    let model = WorkspaceModel::new();
    let mut ui = DeckUi::default();
    ui.sessions_open = true;
    ui.sessions = (0..5)
        .map(|i| session(i, crate::envelope::SessionPhase::InProgress))
        .chain((5..30).map(|i| session(i, crate::envelope::SessionPhase::Complete)))
        .collect();
    ui.sessions_sel = 20; // mid-Complete: the group starts at flat index 5

    let area = Rect::new(0, 0, 100, 30);
    let mut buf = Buffer::empty(area);
    render_sessions_overlay(&model, &ui, area, &mut buf);
    let text = buffer_text(&buf);
    assert!(
        text.contains("COMPLETE (25)"),
        "the heading repeats for a group entered mid-way:\n{text}"
    );
    assert!(
        !text.contains("IN PROGRESS"),
        "a group scrolled fully out keeps no stray heading:\n{text}"
    );
    assert!(
        text.contains("session 20"),
        "the selected row is in the window:\n{text}"
    );
}

/// L-T7 for the deck (#933): a panic inside one band renders an error card in
/// that band, every other band still draws, and the next frame is the real
/// view again — the `DeckUi` came through the caught panic intact.
///
/// The panic is injected at `panel_guard`'s test-only seam rather than crafted
/// from a model, because no model or `DeckUi` makes a view panic:
/// `tests/render_robustness.rs` already sweeps all nine tabs and every overlay
/// across 121 geometries without one. The injection fires inside the guarded
/// closure, exactly where a view's own panic would surface.
#[test]
fn a_panicking_deck_view_renders_an_error_card_and_the_session_survives() {
    let model = running_model_with_queue();

    // The reference frame: a deck that never saw a panic.
    let mut clean_ui = DeckUi::default();
    clean_ui.splash.skip();
    let mut clean = Terminal::new(TestBackend::new(100, 30)).unwrap();
    clean
        .draw(|f| render_deck(&model, &mut clean_ui, f))
        .unwrap();
    let reference = buffer_text(clean.backend().buffer());

    let mut ui = DeckUi::default();
    ui.splash.skip();
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();

    // Frame 1: the SESSION band panics mid-draw. `draw` still returns Ok —
    // nothing unwound out of it, so the event loop keeps its terminal.
    {
        let _armed = crate::panel_guard::arm_panic("SESSION");
        term.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
    }
    let text = buffer_text(term.backend().buffer());
    assert!(
        text.contains("panel 'SESSION' panicked"),
        "the error card names the band that failed:\n{text}"
    );
    assert!(
        text.contains("injected panic in band SESSION"),
        "the error card carries the panic message:\n{text}"
    );
    for surviving in [">>>", "MODEL", "AGENTS"] {
        assert!(
            text.contains(surviving),
            "the surviving bands still rendered ({surviving:?} missing):\n{text}"
        );
    }

    // Frame 2, injection gone: the real view is back, and the frame is
    // byte-for-byte what a deck that never panicked draws — nothing tore.
    term.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
    let recovered = buffer_text(term.backend().buffer());
    assert!(
        !recovered.contains("panicked"),
        "the error card clears once the view stops panicking:\n{recovered}"
    );
    assert_eq!(
        recovered, reference,
        "the frame after a caught panic matches one that never saw it"
    );
}
