//! Accessible mode's grid-view contract (#1258, phase 4): **every grid view
//! has a linear text rendering, reachable row by row, with no column alignment
//! carrying meaning.**
//!
//! Five views are grid-shaped — TRACES, ISSUES, TOOLS, AGENTS (executions) and
//! INSTALLED — and a table is a compression scheme that only works on an eye.
//! `$0.05` under a `Cost` header two rows up is a labelled value to someone who
//! can see the header and the column; read aloud it is an unqualified number,
//! and the alignment that made it mean "cost" is whitespace, which is exactly
//! what a reader collapses.
//!
//! Each test asserts two things about the same rendered row:
//!
//! * the record's values are present **with their labels**, so the row is
//!   self-describing; and
//! * the row contains no internal run of two or more spaces, which is the
//!   mechanical form of "no column alignment is carrying meaning". Padding a
//!   value out to a fixed column is precisely the thing a reader deletes.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use stella_protocol::{AgentEvent, StageKind};
use stella_tui::deck_ui::AgentsPane;
use stella_tui::{
    AgentMeta, AgentScope, DeckTab, DeckUi, Inbound, InstalledAgentEntry, IssueRow, SettingsPane,
    ToolPolicyState, ToolRow, WorkspaceModel, ingest_inbound, render_deck,
};

const W: u16 = 160;
const H: u16 = 40;

fn frame(model: &WorkspaceModel, ui: &mut DeckUi) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(W, H)).expect("TestBackend");
    terminal
        .draw(|f| render_deck(model, ui, f))
        .expect("render_deck");
    let buf = terminal.backend().buffer();
    let area = *buf.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
        })
        .collect()
}

fn accessible_ui(tab: DeckTab) -> DeckUi {
    let mut ui = DeckUi::default();
    ui.splash.skip();
    ui.notice.dismiss();
    ui.tab = tab;
    ui.accessible = true;
    ui
}

/// The one row carrying `needle`, with the deck's own border chrome and the
/// trailing pad removed — what is left is the record itself.
fn record_row(rows: &[String], needle: &str) -> String {
    let row = rows
        .iter()
        .find(|r| r.contains(needle))
        .unwrap_or_else(|| panic!("no rendered row carries {needle:?}:\n{}", rows.join("\n")));
    row.trim_matches(|c| c == '│' || c == ' ').to_string()
}

/// Every label names the value that follows it, and nothing is separated by
/// padding.
fn assert_labelled(row: &str, labels: &[&str]) {
    for label in labels {
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

// ────────────────────────── AGENTS · EXECUTIONS ──────────────────────────

#[test]
fn the_executions_dashboard_reads_as_labelled_records() {
    let mut model = WorkspaceModel::new();
    model.apply_inbound(&Inbound::Register(AgentMeta::new(
        "lead",
        "refactor the automations store",
        0,
    )));
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::Stage {
            name: StageKind::Execute,
        },
    });
    let mut ui = accessible_ui(DeckTab::Agents);
    ui.agents_pane = AgentsPane::Executions;

    let row = record_row(&frame(&model, &mut ui), "refactor the automations store");
    assert_labelled(
        &row,
        &[
            "status",
            "goal",
            "ctx",
            "cost",
            "$/hr",
            "elapsed",
            "cpu",
            "mem",
            "tokens in/out",
        ],
    );
}

// ─────────────────────── AGENTS · INSTALLED AGENTS ───────────────────────

#[test]
fn the_installed_agents_list_reads_as_labelled_records() {
    let mut model = WorkspaceModel::new();
    let mut ui = accessible_ui(DeckTab::Agents);
    ui.agents_pane = AgentsPane::Installed;
    ingest_inbound(
        &Inbound::AgentsList {
            entries: vec![InstalledAgentEntry {
                name: "reviewer".into(),
                description: "reviews a diff against the house rules".into(),
                tools: Some(vec!["read_file".into(), "grep".into()]),
                scope: AgentScope::Project,
                source_path: ".stella/agents/reviewer.md".into(),
                version: 2,
                versions: Vec::new(),
                content: String::new(),
            }],
            status: None,
            creating: false,
            created: None,
        },
        &mut model,
        &mut ui,
    );

    let row = record_row(&frame(&model, &mut ui), "reviewer");
    assert_labelled(&row, &["scope", "version", "description", "toolbelt"]);
    assert!(row.contains("v2"), "{row:?}");
}

// ──────────────────────────────── TRACES ─────────────────────────────────

#[test]
fn the_traces_timeline_reads_as_labelled_records() {
    let mut model = WorkspaceModel::new();
    model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::Text {
            text: "building the auth refactor".into(),
        },
    });

    let mut ui = accessible_ui(DeckTab::Traces);
    let row = record_row(&frame(&model, &mut ui), "building the auth refactor");
    assert_labelled(&row, &["agent", "kind", "summary"]);
    assert!(
        !row.contains('['),
        "a bracketed chip is a visual container; spoken it is punctuation: {row:?}"
    );
}

// ──────────────────────────────── ISSUES ─────────────────────────────────

#[test]
fn the_issues_list_reads_as_labelled_records() {
    let mut model = WorkspaceModel::new();
    let mut ui = accessible_ui(DeckTab::Issues);
    ingest_inbound(
        &Inbound::IssuesList {
            seq: 0,
            outcome: Ok(vec![IssueRow {
                key: "#1258".into(),
                title: "make the command deck accessible".into(),
                state: "open".into(),
                labels: vec!["P1".into(), "area:tui".into()],
                assignee: Some("macanderson".into()),
                url: "https://example.invalid/1258".into(),
                updated_at: None,
            }]),
        },
        &mut model,
        &mut ui,
    );

    let row = record_row(&frame(&model, &mut ui), "make the command deck accessible");
    assert_labelled(&row, &["state", "title", "assignee", "labels"]);
    assert!(
        !row.contains("[open]"),
        "the state chip's brackets are punctuation read aloud: {row:?}"
    );
}

// ───────────────────────────────── TOOLS ─────────────────────────────────

#[test]
fn the_tools_panel_reads_as_labelled_records() {
    let mut model = WorkspaceModel::new();
    let mut ui = accessible_ui(DeckTab::Settings);
    ui.settings_pane = SettingsPane::Tools;
    ingest_inbound(
        &Inbound::ToolPolicy {
            state: ToolPolicyState {
                tools: vec![ToolRow {
                    name: "mcp__github__create_issue".into(),
                    group: "mcp".into(),
                    locked: false,
                    off: None,
                }],
                switches: Default::default(),
            },
            status: None,
        },
        &mut model,
        &mut ui,
    );

    let rows = frame(&model, &mut ui);
    let row = record_row(&rows, "mcp__github__create_issue");
    assert_labelled(&row, &["state", "group"]);

    // The group header is a record too, and it says what its numbers are.
    let group = record_row(&rows, "group MCP");
    assert_labelled(&group, &["state", "tools"]);
}
