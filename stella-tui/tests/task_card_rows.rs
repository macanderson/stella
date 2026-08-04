//! Witness: the task card renders each board state as its own glyph — done
//! `✓` (struck through), active `▸` with the live `elapsed · $cost` readout,
//! queued `○` — with the selection carried by a marker glyph, and its title
//! bar painting a fraction bar that never uses the brand fill's `█`.
//!
//! Before D4 the board had no card at all: `TaskUpdate` folds rendered only
//! as the one-row strip digest and the `⌃S` recall overlay, so there was
//! nothing to pin. These tests pin the card's row vocabulary, the
//! accessible labeled-record form, and the `█`-free title bar that keeps
//! `tests/progress_brand_fill.rs`'s fill isolation unambiguous.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use stella_protocol::{AgentEvent, TaskItem, TaskStatus};
use stella_tui::deck_ui::cards::Card;
use stella_tui::{AgentMeta, DeckUi, Inbound, WorkspaceModel, render_deck};

fn board_model() -> WorkspaceModel {
    let mut m = WorkspaceModel::new();
    m.now_ms = 100_000;
    m.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
    let task = |id: &str, subject: &str, status| TaskItem {
        id: id.into(),
        subject: subject.into(),
        description: None,
        status,
        owner: Some("lead".into()),
    };
    m.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::TaskUpdate {
            tasks: vec![
                task("1", "finished thing", TaskStatus::Completed),
                task("2", "active thing", TaskStatus::InProgress),
                task("3", "queued thing", TaskStatus::Pending),
            ],
        },
    });
    // The active stamp is taken at the fold; advancing the clock gives the
    // card a real elapsed to print.
    m.now_ms = 172_000; // 72s later → 1:12
    m
}

fn frame(model: &WorkspaceModel, accessible: bool) -> String {
    let mut ui = DeckUi::default();
    ui.splash.skip();
    ui.accessible = accessible;
    ui.cards.raise(Card::Tasks);
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).expect("TestBackend");
    terminal
        .draw(|f| render_deck(model, &mut ui, f))
        .expect("render_deck");
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
fn each_row_state_has_its_own_glyph_and_the_active_row_its_live_readout() {
    let model = board_model();
    let text = frame(&model, false);
    // Scope the search to the card's own rows — the SESSION rail behind the
    // card lists the same subjects with its own (different) glyphs.
    let lines: Vec<&str> = text.lines().collect();
    let title = lines
        .iter()
        .position(|l| l.contains("of 3 done"))
        .unwrap_or_else(|| panic!("no card title row:\n{text}"));
    let card: Vec<String> = lines[title..title + 5].iter().map(|l| l.to_string()).collect();
    let row_with = |needle: &str| {
        card.iter()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no card row carries {needle:?}:\n{text}"))
            .to_string()
    };
    assert!(row_with("finished thing").contains('✓'), "done rows carry ✓");
    assert!(row_with("active thing").contains('▸'), "the active row carries ▸");
    assert!(row_with("queued thing").contains('○'), "queued rows carry ○");
    // The active task's elapsed · cost readout: 72 seconds since the board
    // flipped it active.
    assert!(
        row_with("active thing").contains("1:12"),
        "the active row shows its live elapsed:\n{text}"
    );
    // The title: counts + the mini done-fraction bar.
    assert!(text.contains("1 of 3 done"), "the title carries the fraction");
    assert!(text.contains('▰'), "the title carries the mini bar's fill glyph");
    // The selection affordance is a glyph, not just a tint — the golden
    // suite strips style, so a style-only selection would be invisible.
    assert!(
        row_with("finished thing").contains('▸'),
        "the selected row carries the ▸ marker:\n{text}"
    );
}

#[test]
fn the_title_bar_never_paints_the_brand_fill_glyph() {
    // `tests/progress_brand_fill.rs` isolates the run bar's fill cells by
    // the `█` glyph; a second `█`-painting widget would make that witness
    // ambiguous. The card row of the frame must not contain one. (The run
    // progress band still may — scope the check to the card's own rows.)
    let model = board_model();
    let text = frame(&model, false);
    let card_rows: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("of 3 done") || l.contains("thing"))
        .collect();
    for row in card_rows {
        assert!(!row.contains('█'), "a card row paints █: {row:?}");
    }
}

#[test]
fn accessible_mode_reads_rows_as_labeled_records() {
    let model = board_model();
    let text = frame(&model, true);
    let row = text
        .lines()
        .find(|l| l.contains("· task 2"))
        .unwrap_or_else(|| panic!("no labeled active-task record:\n{text}"))
        .trim_matches(|c| c == '│' || c == ' ');
    for label in ["task", "status", "elapsed", "cost"] {
        assert!(
            row.contains(&format!("· {label} ")),
            "the row must say what each value is; {label:?} is unlabelled in {row:?}"
        );
    }
    assert!(
        !row.contains("  "),
        "a run of spaces is column alignment, and a reader collapses it: {row:?}"
    );
}
