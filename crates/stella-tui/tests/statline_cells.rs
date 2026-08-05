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
//!
//! The band opens with MODEL and the stage box. The brand glyph, the agent
//! id and the `● execute` dot that used to lead it are gone: two of the three
//! never changed on a single-agent session, and the third repeated the stage
//! stepper one row up. What replaced them both move — which pin is answering
//! (named by the model's own vendor, not the gateway that proxied it) and
//! how far through the task board the stage has gotten.

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

    // Every cell names itself on the row above. `EXECUTE` is the stage box's
    // label — the one label that is live state rather than a constant.
    for label in [
        "MODEL", "EXECUTE", "CPU", "CONTEXT", "SPEND", "CACHE", "SAVED", "WARMTH", "ENGINE",
        "PIPELINE",
    ] {
        assert!(labels.contains(label), "label {label:?} renders:\n{labels}");
    }
    // The values sit below, not beside, their labels. (One registered agent
    // is running, so ENGINE reads `1 active`.)
    for value in ["1 active", "0/200k"] {
        assert!(values.contains(value), "value {value:?} renders:\n{values}");
        assert!(
            !labels.contains(value),
            "value {value:?} belongs on the value row only:\n{labels}"
        );
    }
    // MODEL leads the band — the brand glyph, the agent id and the stage dot
    // that used to sit here are gone.
    assert!(labels.trim_start().starts_with("MODEL"), "{labels}");
    for gone in ["✦ stella", "● execute", "AGENT"] {
        assert!(
            !labels.contains(gone) && !values.contains(gone),
            "{gone:?} left the band:\n{labels}\n{values}"
        );
    }
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

/// A session that has served one worker call through a gateway and built a
/// 5-task board with the 4th in progress.
fn model_serving_a_board() -> WorkspaceModel {
    use stella_protocol::{TaskItem, TaskStatus};
    let mut m = running_model();
    m.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::StepUsage {
            output_text: None,
            step: 1,
            role: stella_protocol::event::ModelCallRole::Worker,
            provider: "openrouter".into(),
            model: "z-ai/glm-5.2".into(),
            input_tokens: 10_000,
            output_tokens: 500,
            cached_input_tokens: 0,
            cache_write_tokens: 0,
            estimated_input_tokens: 0,
            cost_usd: 0.05,
            duration_ms: 100,
            retries: 0,
            tool_calls: 0,
            complete: true,
            finish_reason: None,
        },
    });
    let task = |id: &str, status| TaskItem {
        id: id.into(),
        subject: format!("task {id}"),
        description: None,
        status,
        owner: None,
    };
    m.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::TaskUpdate {
            tasks: vec![
                task("1", TaskStatus::Completed),
                task("2", TaskStatus::Completed),
                task("3", TaskStatus::Completed),
                task("4", TaskStatus::InProgress),
                task("5", TaskStatus::Pending),
            ],
        },
    });
    m
}

#[test]
fn the_model_cell_names_the_serving_role_and_its_vendor() {
    let model = model_serving_a_board();
    let (labels, values, models) = statline_band(&model, 200);
    assert!(labels.contains("MODEL"), "{labels}");
    assert!(
        values.contains("worker: z-ai/glm-5.2"),
        "the role word and the vendor slug render:\n{values}"
    );
    // The gateway that proxied the call is not the model's identity, on
    // either surface.
    assert!(
        !values.contains("openrouter") && !models.contains("openrouter"),
        "the API gateway is stripped:\n{values}\n{models}"
    );
}

#[test]
fn the_stage_box_stacks_the_stage_name_over_the_board_position() {
    let model = model_serving_a_board();
    let (labels, values, _) = statline_band(&model, 200);
    assert!(
        labels.contains("EXECUTE"),
        "the stage name heads its own cell:\n{labels}"
    );
    assert!(
        values.contains("Step 4 of 5"),
        "the board position sits under it:\n{values}"
    );
    // The step is the position being worked, not the three that are done.
    assert!(!values.contains("Step 3 of 5"), "{values}");
}

#[test]
fn a_session_with_no_task_board_invents_no_step() {
    let model = running_model(); // no TaskUpdate ever folded
    let (labels, _, _) = statline_band(&model, 200);
    assert!(labels.contains("EXECUTE"), "the stage still names itself");
    // `statline_items` is the honest surface: no board, no step.
    let items = statline_items(&model, &DeckUi::default());
    let stage = items.iter().find(|i| i.key == "stage").expect("stage");
    let text: String = stage.spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "—", "no board means no invented step");
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
    for label in ["MODEL", "CPU", "CACHE", "SAVED", "WARMTH", "PIPELINE"] {
        assert!(
            labels.contains(label),
            "at 200 cols {label} renders:\n{labels}"
        );
    }

    // As the row narrows, the lowest-priority cell leaves first and leaves
    // whole. MODEL is must-keep, so it survives every width.
    let mut gone_at = std::collections::HashMap::new();
    for w in (24..=200u16).rev() {
        let (labels, _, _) = statline_band(&model, w);
        for label in ["PIPELINE", "CACHE", "CPU", "SPEND"] {
            if !labels.contains(label) {
                gone_at.entry(label).or_insert(w);
            }
        }
        assert!(
            labels.contains("MODEL"),
            "the model anchor survives {w} cols:\n{labels}"
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
        vec!["model", "stage", "ctx"],
        "what is answering, where the run is, and how much window is left"
    );
}

#[test]
fn every_card_collapses_the_band_to_its_context_cells() {
    let model = running_model();
    for (card, expect) in [
        // `/plan` is one card where `/tasks`, `/scope` and `/witness` used to
        // be three, and it carries what all three needed: the turn's cost and
        // where the plan stands.
        (Card::Plan, vec!["model", "stage", "turn", "plan"]),
        (Card::Models, vec!["model", "stage", "ctx", "spend"]),
        (Card::Budget, vec!["model", "stage", "ctx", "spend"]),
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
