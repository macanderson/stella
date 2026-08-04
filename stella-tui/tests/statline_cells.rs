//! Witness: the statline is a **two-row** band — a dim micro-label row
//! directly over its value row — cells drop whole by priority as the row
//! narrows, and any open card collapses the band to at most four cells.
//!
//! The one-row zoned strip that briefly replaced this had to name each value
//! inline (`ctx 91k/200k · cpu 73%`), which spends scarce horizontal columns
//! on labels that vertical space gives away free — and left no room for the
//! CPU/CONTEXT meters, the CACHE volumes or the MODELS pins. These tests pin
//! the restored contract: every cell labels itself on the row above, the
//! label and value columns stay aligned, CPU and CONTEXT lead with the `▮▯`
//! meter, MODELS is never the row that got dropped, and `statline_items`
//! remains the single decision function.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use stella_protocol::{AgentEvent, StageKind};
use stella_tui::deck_ui::cards::Card;
use stella_tui::statline::{MUST_KEEP, statline_items};
use stella_tui::{AgentMeta, DeckUi, Inbound, WorkspaceModel, render_deck};

fn running_model() -> WorkspaceModel {
    let mut m = WorkspaceModel::new();
    m.now_ms = 10_000;
    m.apply_inbound(&Inbound::Register(
        AgentMeta::new("lead", "goal", 0).with_role("lead"),
    ));
    m.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::Stage {
            name: StageKind::Execute,
        },
    });
    m
}

/// The statline band of a full-deck render at `w` columns, as plain text:
/// `(labels, values, models)`. Located by finding the MODELS row rather than
/// by a hardcoded offset, so a layout change fails loudly here instead of
/// silently asserting against the wrong rows.
fn statline_band(model: &WorkspaceModel, w: u16) -> (String, String, String) {
    let mut ui = DeckUi::default();
    ui.splash.skip();
    let mut terminal = Terminal::new(TestBackend::new(w, 24)).expect("TestBackend");
    terminal
        .draw(|f| render_deck(model, &mut ui, f))
        .expect("render_deck");
    let buf = terminal.backend().buffer();
    let area = *buf.area();
    let row = |y: u16| -> String {
        (0..area.width)
            .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
            .collect()
    };
    let models_y = (0..area.height)
        .find(|&y| row(y).contains("MODELS"))
        .unwrap_or_else(|| panic!("no MODELS row at {w} cols"));
    assert!(models_y >= 2, "the label/value pair sits above MODELS");
    (row(models_y - 2), row(models_y - 1), row(models_y))
}

#[test]
fn the_band_is_a_label_row_over_a_value_row() {
    let model = running_model();
    let (labels, values, _) = statline_band(&model, 200);

    // Every cell names itself on the row above.
    for label in [
        "AGENT", "STAGE", "CPU", "CONTEXT", "SPEND", "CACHE", "SAVED", "WARMTH", "ENGINE",
        "PIPELINE",
    ] {
        assert!(labels.contains(label), "label {label:?} renders:\n{labels}");
    }
    // The values sit below, not beside, their labels. (One registered agent
    // is running, so ENGINE reads `1 active`.)
    for value in ["lead", "● execute", "1 active"] {
        assert!(values.contains(value), "value {value:?} renders:\n{values}");
        assert!(
            !labels.contains(value),
            "value {value:?} belongs on the value row only:\n{labels}"
        );
    }
    // The brand is pinned left on the value row, with blanks held above it.
    assert!(values.trim_start().starts_with("✦ stella"), "{values}");
    assert!(
        labels.starts_with(&" ".repeat("✦ stella".chars().count())),
        "the label row reserves the brand's width:\n{labels}"
    );
}

#[test]
fn each_label_sits_in_the_same_column_as_its_value() {
    // Alignment is the whole point of two rows: a label that drifts off its
    // value turns the band back into a guessing game.
    let model = running_model();
    let (labels, values, _) = statline_band(&model, 200);
    let l: Vec<char> = labels.chars().collect();
    let v: Vec<char> = values.chars().collect();
    let dividers: Vec<usize> = labels
        .char_indices()
        .filter(|(_, c)| *c == '│')
        .map(|(i, _)| labels[..i].chars().count())
        .collect();
    assert!(dividers.len() >= 8, "the band draws a divider per cell");
    for col in dividers {
        assert_eq!(
            v.get(col),
            Some(&'│'),
            "the divider at column {col} is shared by both rows:\n{labels}\n{values}"
        );
        // " │ " is three columns, so the cell's own text starts at col + 2 on
        // both rows.
        assert!(
            l.get(col + 2).is_some_and(|c| !c.is_whitespace()),
            "a label starts right after the divider at {col}:\n{labels}"
        );
    }
}

#[test]
fn cpu_and_context_carry_a_six_cell_meter() {
    let model = running_model();
    let (_, values, _) = statline_band(&model, 200);
    let meters = values.matches('▯').count() + values.matches('▮').count();
    assert!(
        meters >= 12,
        "CPU and CONTEXT each draw six meter cells, saw {meters}:\n{values}"
    );
    assert!(
        values.contains("/200k"),
        "CONTEXT names the window:\n{values}"
    );
}

#[test]
fn the_cache_cell_shows_its_read_write_volumes() {
    let mut model = running_model();
    model.agents[0].tokens_in = 200_000;
    model.agents[0].cache_read_tokens = 150_000;
    model.agents[0].cache_write_tokens = 40_000;
    let (_, values, _) = statline_band(&model, 200);
    assert!(values.contains("75%"), "the hit rate renders:\n{values}");
    assert!(
        values.contains("rd ·") && values.contains("wr"),
        "the volumes render beside it:\n{values}"
    );
}

#[test]
fn models_is_never_the_row_that_got_dropped() {
    // The pins a scored run is read against must survive every width the deck
    // is usable at — the cell row could never hold three `provider/model`
    // slugs, which is why they get their own row.
    let model = running_model();
    for w in [80u16, 120, 160, 200] {
        let (_, _, models) = statline_band(&model, w);
        assert!(models.contains("MODELS"), "MODELS survives {w} cols");
        for initial in ["T·", "W·", "J·"] {
            assert!(
                models.contains(initial),
                "{initial} pin renders at {w} cols:\n{models}"
            );
        }
    }
}

#[test]
fn cells_drop_whole_by_priority_as_the_row_narrows() {
    let model = running_model();

    // Wide: every cell renders.
    let (labels, _, _) = statline_band(&model, 200);
    for label in ["AGENT", "CPU", "CACHE", "SAVED", "WARMTH", "PIPELINE"] {
        assert!(
            labels.contains(label),
            "at 200 cols {label} renders:\n{labels}"
        );
    }

    // As the row narrows, the lowest-priority cell leaves first and leaves
    // whole. The brand survives every width.
    let mut gone_at = std::collections::HashMap::new();
    for w in (24..=200u16).rev() {
        let (labels, values, _) = statline_band(&model, w);
        for label in ["PIPELINE", "CACHE", "CPU", "SPEND"] {
            if !labels.contains(label) {
                gone_at.entry(label).or_insert(w);
            }
        }
        assert!(
            values.contains('✦'),
            "the brand survives {w} cols:\n{values}"
        );
    }
    let at = |label: &str| {
        *gone_at
            .get(label)
            .unwrap_or_else(|| panic!("{label} drops somewhere above 24 cols"))
    };
    assert!(
        at("PIPELINE") >= at("CACHE"),
        "priority 3 (PIPELINE, {}) drops before priority 4 (CACHE, {})",
        at("PIPELINE"),
        at("CACHE")
    );
    assert!(
        at("CACHE") >= at("CPU"),
        "priority 4 (CACHE, {}) drops before priority 5 (CPU, {})",
        at("CACHE"),
        at("CPU")
    );
    assert!(
        at("CPU") >= at("SPEND"),
        "priority 5 (CPU, {}) drops before priority 6 (SPEND, {})",
        at("CPU"),
        at("SPEND")
    );
}

#[test]
fn only_the_load_bearing_cells_are_must_keep() {
    let model = running_model();
    let items = statline_items(&model, &DeckUi::default());
    let pinned: Vec<&str> = items
        .iter()
        .filter(|i| i.priority >= MUST_KEEP)
        .map(|i| i.key)
        .collect();
    assert_eq!(
        pinned,
        vec!["stage", "ctx"],
        "the stage word and the token meter are the two undroppable cells"
    );
}

#[test]
fn every_card_collapses_the_band_to_its_context_cells() {
    let model = running_model();
    for (card, expect) in [
        (Card::Tasks, vec!["agent", "stage", "turn"]),
        (Card::Scope, vec!["agent", "stage", "ctx", "spend"]),
        (Card::Witness, vec!["agent", "stage", "witness", "spend"]),
        (Card::Models, vec!["agent", "stage", "ctx", "spend"]),
        (Card::Budget, vec!["agent", "stage", "ctx", "spend"]),
    ] {
        let mut ui = DeckUi::default();
        ui.cards.raise(card);
        let items = statline_items(&model, &ui);
        let keys: Vec<&str> = items.iter().map(|i| i.key).collect();
        assert!(
            items.len() <= 4,
            "{card:?} collapses to at most four cells, got {keys:?}"
        );
        for key in expect {
            assert!(keys.contains(&key), "{card:?} keeps {key:?}, got {keys:?}");
        }
    }
}

#[test]
fn any_open_overlay_collapses_the_band_too() {
    // The rule is "any overlay/card", not just the named cards — a help or
    // queue overlay on top gets the same quiet floor.
    let model = running_model();
    for flag in 0..3 {
        let mut ui = DeckUi::default();
        match flag {
            0 => ui.help_open = true,
            1 => ui.queue_open = true,
            _ => ui.inbox_open = true,
        }
        assert!(
            statline_items(&model, &ui).len() <= 4,
            "overlay {flag} collapses the statline"
        );
    }
}
