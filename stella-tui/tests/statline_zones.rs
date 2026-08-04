//! Witness: the statline's four zones drop whole on a narrow row, in a fixed
//! order, and collapse to at most four context-chosen items under a card.
//!
//! Before the D1 regroup the statline was a flat priority-scored cell strip:
//! nothing pinned *which* cell gave way first, a squeezed row could clip a
//! cell mid-token, and an open overlay changed nothing — the full strip kept
//! rendering under a card that had the user's attention. These tests pin the
//! new contract: `statline_items` is the single decision function, the drop
//! order is zone B's `cpu` first → the rest of zone B → zone C down to
//! `run` → the rest, with zone A's brand glyph and zone D's inbox badge
//! surviving to the narrowest widths, and every open card collapses the row
//! to ≤4 items chosen for that context.
//!
//! The zone *contents* moved once since: the cache hit rate folded into the
//! `ctx` meter it qualifies, `saved` left for the context overlay's SESSION
//! VITALS, the lane's `role` left outright, and zone A gained the pin the
//! lane runs on plus its step counter. What is pinned here is the layout law
//! those changes had to obey, not the specific cells.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use stella_protocol::{AgentEvent, StageKind, TaskItem, TaskStatus};
use stella_tui::deck_ui::cards::Card;
use stella_tui::statline::statline_items;
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

/// `running_model` with everything zone A and B can show: a registered pin, a
/// three-step board with one closed, and metered input with cache reads.
fn furnished_model() -> WorkspaceModel {
    let mut m = running_model();
    let task = |id: &str, status| TaskItem {
        id: id.into(),
        subject: format!("step {id}"),
        description: None,
        status,
        owner: None,
    };
    m.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::TaskUpdate {
            tasks: vec![
                task("1", TaskStatus::Completed),
                task("2", TaskStatus::InProgress),
                task("3", TaskStatus::Pending),
            ],
        },
    });
    let a = &mut m.agents[0];
    a.meta.model = Some("zai/glm-5.1-fast".into());
    a.tokens_in = 100_000;
    a.cache_read_tokens = 78_000;
    m
}

/// The statline row of a full-deck render at `w` columns, as plain text.
fn statline_row(model: &WorkspaceModel, w: u16) -> String {
    let mut ui = DeckUi::default();
    ui.splash.skip();
    let mut terminal = Terminal::new(TestBackend::new(w, 24)).expect("TestBackend");
    terminal
        .draw(|f| render_deck(model, &mut ui, f))
        .expect("render_deck");
    let buf = terminal.backend().buffer();
    let area = *buf.area();
    let y = area.height - 1; // the statline is the deck's bottom row
    (0..area.width)
        .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
        .collect()
}

#[test]
fn zones_drop_whole_in_the_documented_order_as_the_row_narrows() {
    let model = furnished_model();

    // Wide: all four zones render, in the documented layout.
    let full = statline_row(&model, 200);
    for needle in [
        "✦",
        "lead",
        "(zai/glm-5.1-fast)",
        "● execute",
        "1/3",
        "ctx ",
        "(78% hit)",
        "cpu ",
        "1 session",
        "turn ",
        "run ",
        "✉",
    ] {
        assert!(
            full.contains(needle),
            "at 200 cols {needle:?} renders:\n{full}"
        );
    }
    // Zone A is one phrase, not a list — no `·` between the lane, the pin and
    // the stage.
    assert!(
        full.contains("✦ lead (zai/glm-5.1-fast) ● execute 1/3 │"),
        "zone A joins with spaces:\n{full}"
    );
    // The cells that left: `cache` folded into `ctx`, `saved` moved to the
    // context overlay, and the lane's role is gone outright.
    for gone in ["cache ", "saved"] {
        assert!(!full.contains(gone), "{gone:?} left the row:\n{full}");
    }

    // As the row narrows, items leave whole and in order: `cpu` before the
    // rest of zone B, zone B before zone C's `turn`, and `run` last of zone C
    // — while the brand glyph and the inbox badge survive.
    let mut cpu_gone_at = None;
    let mut sessions_gone_at = None;
    let mut ctx_gone_at = None;
    let mut turn_gone_at = None;
    let mut run_gone_at = None;
    let mut steps_gone_at = None;
    let mut model_gone_at = None;
    for w in (20..=200u16).rev() {
        let row = statline_row(&model, w);
        if cpu_gone_at.is_none() && !row.contains("cpu") {
            cpu_gone_at = Some(w);
        }
        if sessions_gone_at.is_none() && !row.contains("session") {
            sessions_gone_at = Some(w);
        }
        if ctx_gone_at.is_none() && !row.contains("ctx") {
            ctx_gone_at = Some(w);
        }
        if turn_gone_at.is_none() && !row.contains("turn") {
            turn_gone_at = Some(w);
        }
        if run_gone_at.is_none() && !row.contains("run") {
            run_gone_at = Some(w);
        }
        if steps_gone_at.is_none() && !row.contains("1/3") {
            steps_gone_at = Some(w);
        }
        if model_gone_at.is_none() && !row.contains("glm-5.1-fast") {
            model_gone_at = Some(w);
        }
        assert!(row.contains('✦'), "the brand survives {w} cols:\n{row}");
        if w >= 24 {
            assert!(
                row.contains('✉'),
                "the inbox badge survives {w} cols:\n{row}"
            );
        }
    }
    let (cpu, sessions, ctx, turn, run, steps, pin) = (
        cpu_gone_at.expect("cpu drops somewhere"),
        sessions_gone_at.expect("sessions drops somewhere"),
        ctx_gone_at.expect("ctx drops somewhere"),
        turn_gone_at.expect("turn drops somewhere"),
        run_gone_at.expect("run drops somewhere"),
        steps_gone_at.expect("steps drops somewhere"),
        model_gone_at.expect("the pin drops somewhere"),
    );
    assert!(
        cpu >= sessions,
        "cpu ({cpu}) drops before the lane count ({sessions})"
    );
    assert!(
        sessions >= ctx,
        "the lane count ({sessions}) drops before the rest of zone B ({ctx})"
    );
    assert!(
        ctx >= turn,
        "zone B ({ctx}) drops before zone C's turn ({turn})"
    );
    assert!(
        turn >= run,
        "zone C keeps only run for a while ({turn} vs {run})"
    );
    // Zone A's extras outlive all of zone C, and the pin outlives the step
    // counter — the work rail shows the same `1/3`, nothing else shows the
    // pin.
    assert!(
        run >= steps,
        "zone C goes before zone A's step counter ({run} vs {steps})"
    );
    assert!(
        steps >= pin,
        "the step counter goes before the pin ({steps} vs {pin})"
    );
}

#[test]
fn every_card_collapses_the_row_to_its_context_items() {
    let model = running_model();
    for (card, expect) in [
        (Card::Tasks, vec!["agent", "stage", "turn"]),
        (Card::Scope, vec!["agent", "stage", "ctx", "run"]),
        (Card::Witness, vec!["agent", "stage", "witness", "run"]),
        (Card::Models, vec!["agent", "stage", "ctx", "run"]),
        (Card::Budget, vec!["agent", "stage", "ctx", "run"]),
    ] {
        let mut ui = DeckUi::default();
        ui.cards.raise(card);
        let items = statline_items(&model, &ui);
        let keys: Vec<&str> = items.iter().map(|i| i.key).collect();
        assert!(
            items.len() <= 4,
            "{card:?} collapses to at most four items, got {keys:?}"
        );
        for key in expect {
            assert!(keys.contains(&key), "{card:?} keeps {key:?}, got {keys:?}");
        }
    }
}

#[test]
fn any_open_overlay_collapses_the_row_too() {
    // The rule is "any overlay/card", not just the three named cards — a
    // help or queue overlay on top gets the same quiet floor.
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
