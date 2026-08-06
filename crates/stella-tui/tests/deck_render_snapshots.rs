//! Golden-frame snapshots of the command deck (#874).
//!
//! `tests/deck_snapshot.rs` already drives every tab through the real
//! [`render_deck`] entrypoint, but it asserts with `text.contains(needle)` —
//! which is blind to *position*. A substring match still passes when a group
//! header slides a column, when a table loses a row, or when a panel and its
//! neighbour swap places. Those are exactly the breakages that survive
//! `cargo test` and ship.
//!
//! This suite closes that gap by asserting the **whole frame**: each fixture
//! renders into a fixed-size `TestBackend` and the complete character grid is
//! compared against a committed golden under `tests/snapshots/deck/`. Nothing
//! can move without the diff saying so.
//!
//! ## Regenerating
//!
//! ```text
//! BLESS=1 cargo test -p stella-tui --test deck_render_snapshots
//! ```
//!
//! Then **read the diff before committing it**. A golden suite is only worth
//! its failures: blessing without looking converts a regression detector into a
//! changelog.
//!
//! Every test here is named `deck_render_snapshots_*` on purpose, so the
//! filter form — `cargo test -p stella-tui deck_render_snapshots`, with no
//! `--test` — selects them too. Cargo matches filters against test *names*, so
//! under any other naming that command would match nothing, run zero tests, and
//! still print `ok`: a bless that silently blesses nothing. Keep the prefix.
//!
//! ## What a golden does and does not pin
//!
//! Captured: every glyph, at every cell, at a fixed viewport — content,
//! alignment, wrapping, panel geometry, borders, and any selection drawn with a
//! marker glyph (the SKILLS list's `▸ `, for instance).
//!
//! Not captured: colour and text modifiers. Styling is stripped, deliberately.
//! The deck remaps its palette for light terminals, degrades by colour depth,
//! and drops colour entirely under `NO_COLOR` ([`render_deck`]'s own doc
//! comment covers this), so a style-bearing golden would be a golden about the
//! environment. The cost is honest and narrow: a regression that leaves the
//! glyphs alone and only stops *highlighting* a row — the FILES list marks its
//! selection with style alone — is invisible here and needs a behavioural test.
//!
//! ## Determinism
//!
//! [`render_deck`] is a pure function of `(model, ui)`: it reads no clock, no
//! environment, and no process table, and the model's agent list is a `Vec`,
//! so row order is fold order. The one field that escapes the fold is
//! `AgentEntry::res`, the out-of-band CPU/MEM sample — `tests/deck_snapshot.rs`
//! feeds the scenario `std::process::id()` so a live demo shows real numbers,
//! which would make a golden differ on every run. Here the pid is a constant
//! and the samples are written explicitly, so the frame is fixture data all the
//! way down.

use std::fmt::Write as _;
use std::path::PathBuf;

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use stella_tui::scenario::{demo_graph, demo_inbound};
use stella_tui::{
    DeckTab, DeckUi, IssueRow, ResourceSample, ScopeAction, SettingsPane, SkillPrompt, SkillRow,
    SkillScope, SkillsView, ToolDenial, ToolPolicyState, ToolRow, ToolScope, WorkspaceModel,
    render_deck,
};

/// The command used to regenerate every golden in this file. Quoted verbatim in
/// each snapshot header and in every failure message, so nobody has to go
/// looking for it.
const BLESS_CMD: &str = "BLESS=1 cargo test -p stella-tui --test deck_render_snapshots";

/// A fixed pid, deliberately **not** `std::process::id()`. The scenario stamps
/// this on every agent and the resource monitor samples whatever pid it finds;
/// a live one makes the AGENTS dashboard's CPU%/MEM columns differ per run.
const FIXTURE_PID: u32 = 424_242;

/// The canonical viewport for the per-tab goldens.
///
/// 160 columns is above the AGENTS dashboard's compact threshold, so the dense
/// column set renders in full — pinning the widest layout each tab has.
/// `deck_render_snapshots_pin_the_compact_agents_dashboard` takes the other
/// side of that threshold.
const W: u16 = 160;
const H: u16 = 40;

// ───────────────────────────── the harness ─────────────────────────────

fn snapshot_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots/deck")
        .join(format!("{name}.txt"))
}

/// `BLESS=1` rewrites the goldens instead of asserting against them.
fn blessing() -> bool {
    std::env::var("BLESS").is_ok_and(|v| v == "1")
}

/// Render one deck state into a fixed viewport and flatten it to text, one
/// line per terminal row, styling stripped.
///
/// Rows are right-trimmed. Every row of a rendered frame is padded to the full
/// width, and committed trailing whitespace is the kind of thing editors and
/// hooks silently eat — a golden that can be corrupted by saving it is not a
/// golden. Nothing is lost: trailing blanks carry no visual information, and a
/// glyph that vanishes off the right edge still vanishes from the line.
fn render_frame(model: &WorkspaceModel, ui: &mut DeckUi, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("TestBackend");
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
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The full golden file: a self-describing header, then the frame.
///
/// The header carries the viewport, so a resize shows up as a header diff
/// rather than as forty mysteriously re-wrapped lines, and carries the bless
/// command, so a reader who opens the file knows how to regenerate it.
fn golden_body(name: &str, description: &str, w: u16, h: u16, frame: &str) -> String {
    format!(
        "# deck golden · {name}\n\
         # {description}\n\
         # viewport {w}×{h} · rendered through render_deck, styling stripped\n\
         # regenerate: {BLESS_CMD}\n\
         \n\
         {frame}\n"
    )
}

/// Assert a rendered frame against its committed golden, or rewrite it under
/// `BLESS=1`.
fn assert_golden(name: &str, description: &str, w: u16, h: u16, frame: &str) {
    let body = golden_body(name, description, w, h, frame);
    let path = snapshot_path(name);

    if blessing() {
        let dir = path.parent().expect("snapshot path has a parent");
        std::fs::create_dir_all(dir)
            .unwrap_or_else(|err| panic!("create {}: {err}", dir.display()));
        std::fs::write(&path, &body)
            .unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
        println!("blessed {}", path.display());
        return;
    }

    let Ok(expected) = std::fs::read_to_string(&path) else {
        panic!(
            "no golden for {name:?} at {}.\n\
             Create it with:  {BLESS_CMD}\n\
             \n\
             This run rendered:\n{body}",
            path.display()
        );
    };
    // Normalize line endings: a Windows checkout with `core.autocrlf` on would
    // otherwise fail every golden on the first character of every line.
    let expected = expected.replace("\r\n", "\n");

    assert!(
        expected == body,
        "the {name:?} frame no longer matches its golden at {}.\n\
         \n{}\n\
         If the change is intended, re-bless and read the diff:  {BLESS_CMD}",
        path.display(),
        describe_diff(&expected, &body),
    );
}

/// A readable account of how the frame moved.
///
/// A golden suite is worth exactly what its failure output is worth: a bare
/// equality assert on two forty-line frames prints two walls of box-drawing and
/// leaves the reader to eyeball which column slipped. This reports the changed
/// rows only, each with the column where they diverge.
fn describe_diff(expected: &str, actual: &str) -> String {
    /// Enough rows to see the shape of a break without burying the terminal.
    const MAX_ROWS: usize = 12;
    /// Width of the `  golden │` gutter, so the caret lines up under the row.
    const GUTTER: usize = 10;

    let expected: Vec<&str> = expected.lines().collect();
    let actual: Vec<&str> = actual.lines().collect();
    let mut out = String::new();

    if expected.len() != actual.len() {
        let _ = writeln!(
            out,
            "the frame changed height: {} lines → {} lines",
            expected.len(),
            actual.len()
        );
    }

    let mut differing = 0usize;
    for i in 0..expected.len().max(actual.len()) {
        let (e, a) = (
            expected.get(i).copied().unwrap_or("<past end of golden>"),
            actual.get(i).copied().unwrap_or("<past end of frame>"),
        );
        if e == a {
            continue;
        }
        differing += 1;
        if differing > MAX_ROWS {
            continue;
        }
        // Character index, not display column: box-drawing is single-width but
        // a wide glyph would skew the caret. Close enough to point at.
        let col = e
            .chars()
            .zip(a.chars())
            .position(|(x, y)| x != y)
            .unwrap_or_else(|| e.chars().count().min(a.chars().count()));
        let _ = writeln!(
            out,
            "line {}, first difference at character {}:",
            i + 1,
            col + 1
        );
        let _ = writeln!(out, "  golden │{e}│");
        let _ = writeln!(out, "  actual │{a}│");
        let _ = writeln!(out, "{}{}^", " ".repeat(GUTTER), " ".repeat(col));
    }

    if differing > MAX_ROWS {
        let _ = writeln!(out, "… and {} more changed lines", differing - MAX_ROWS);
    }
    if differing == 0 {
        // Every line matched, yet the caller found the two unequal — `lines()`
        // is blind to how the file ends. The realistic cause is an editor that
        // added or stripped a trailing newline on save. Name it, rather than
        // printing an empty report and sending the reader hunting a glyph.
        out.push_str(
            "no line differs — the golden and the frame disagree only in \
             trailing newlines, which usually means an editor rewrote the \
             snapshot file on save.\n",
        );
    }
    out
}

// ───────────────────────────── the fixtures ─────────────────────────────

/// The scripted demo scenario, folded — the same representative session the
/// existing render tests use, with every out-of-band field pinned.
fn fixture_model() -> WorkspaceModel {
    let mut model = WorkspaceModel::new();
    // ~5:12 elapsed, so the dashboard's timers read as a session in progress
    // rather than as a stopwatch at zero.
    model.now_ms = 312_000;
    for inbound in demo_inbound(0, FIXTURE_PID) {
        model.apply_inbound(&inbound);
    }

    // The resource sample is the one thing the dashboard renders that no event
    // folds, so it has to be written here or the frame reports this machine.
    // Three distinct readings, one of them idle, so the gauge colouring and the
    // byte humanizer are both exercised by real values.
    let samples = [
        ResourceSample {
            cpu_pct: 42.0,
            mem_bytes: 148 * 1024 * 1024,
        },
        ResourceSample {
            cpu_pct: 7.5,
            mem_bytes: 61 * 1024 * 1024,
        },
        ResourceSample {
            cpu_pct: 0.0,
            mem_bytes: 12 * 1024 * 1024,
        },
    ];
    for (entry, res) in model.agents.iter_mut().zip(samples) {
        entry.res = res;
    }
    model.global_cpu_pct = 49.5;
    model
}

/// A deck past the splash, on `tab`, with the code graph loaded — the state a
/// user is in a few seconds after launch.
fn base_ui(tab: DeckTab) -> DeckUi {
    let mut ui = DeckUi::default();
    ui.splash.skip();
    ui.tab = tab;
    ui.graph = Some(demo_graph());
    // The driver seeds this at session start, so every frame the real deck
    // draws has it — including the one `/models` renders from.
    ui.engine.pristine = Some(stella_tui::scenario::demo_engine_config());
    ui
}

fn fixture_skills() -> SkillsView {
    let skill =
        |scope, name: &str, description: &str, origin: &str, enabled, version, latest| SkillRow {
            scope,
            name: name.to_string(),
            description: description.to_string(),
            body: format!("# {name}\n\n{description}\n"),
            origin: origin.to_string(),
            enabled,
            version,
            latest,
            removable: origin != "workspace",
        };
    SkillsView {
        rows: vec![
            skill(
                SkillScope::Project,
                "release-checklist",
                "Cut a release: version bump, changelog, tag, artifacts.",
                "workspace",
                true,
                3,
                3,
            ),
            // Pinned behind the latest version on disk (2 < 4) — the state the
            // list has to distinguish from an up-to-date row.
            skill(
                SkillScope::User,
                "rust-review",
                "Review Rust diffs for ownership, error handling, and API shape.",
                "user",
                true,
                2,
                4,
            ),
            // Disabled: still on disk, excluded from recall.
            skill(
                SkillScope::User,
                "sql-tuning",
                "Read a query plan and propose indexes.",
                "installed",
                false,
                1,
                1,
            ),
            // Mined from the session rather than authored — `origin: auto`.
            skill(
                SkillScope::Project,
                "bench-rig-access",
                "Reach the benchmark rig and read a run's reward.",
                "auto",
                true,
                1,
                1,
            ),
        ],
        status: Some("4 skills · 3 enabled".to_string()),
        busy: false,
        created: None,
    }
}

fn fixture_issues() -> Vec<IssueRow> {
    let issue = |key: &str, title: &str, state: &str, labels: &[&str], assignee: Option<&str>| {
        IssueRow {
            key: key.to_string(),
            title: title.to_string(),
            state: state.to_string(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            assignee: assignee.map(|s| s.to_string()),
            url: format!("https://github.com/example/repo/issues/{}", &key[1..]),
            // A fixed stamp: `updated_at` is a rendered string, so a live one
            // would age the golden out from under the suite.
            updated_at: Some("2026-01-14T09:30:00Z".to_string()),
        }
    };
    vec![
        issue(
            "#874",
            "Command-deck render golden tests",
            "open",
            &["enhancement", "area:tui"],
            Some("dev"),
        ),
        issue(
            "#826",
            "run_deck event-loop pty harness",
            "open",
            &["enhancement"],
            None,
        ),
        issue(
            "#689",
            "Surface per-server MCP tool truncation",
            "closed",
            &["bug"],
            Some("dev"),
        ),
    ]
}

fn fixture_tool_policy() -> ToolPolicyState {
    ToolPolicyState {
        tools: [
            ("read_file", "file"),
            ("write_file", "file"),
            ("bash", "bash"),
            ("start_process", "process"),
            ("mcp__github__create_issue", "mcp"),
            ("deploy_to_staging", "custom"),
        ]
        .into_iter()
        .map(|(name, group)| ToolRow {
            name: name.to_string(),
            group: group.to_string(),
            locked: name == "bash",
            off: (name == "bash").then(|| ToolDenial {
                key: "bash".to_string(),
                scope: Some(ToolScope::Managed),
            }),
        })
        .collect(),
        switches: std::collections::BTreeMap::from([("bash".to_string(), false)]),
    }
}

fn fixture_mcp_servers() -> Vec<stella_tui::McpServerInfo> {
    vec![
        stella_tui::McpServerInfo {
            name: "stripe".into(),
            title: Some("Stripe".into()),
            description: Some("Payments, refunds, and balance reads.".into()),
            endpoint: "https://mcp.stripe.com/v1".into(),
            kind: "http".into(),
            enabled: true,
            connected: true,
            health: Some("live".into()),
            tool_count: 21,
            oauth: Some(true),
            calls: 14,
            ..Default::default()
        },
        stella_tui::McpServerInfo {
            name: "fs".into(),
            endpoint: "npx -y @modelcontextprotocol/server-filesystem /w".into(),
            kind: "stdio".into(),
            enabled: true,
            connected: true,
            health: Some("live".into()),
            tool_count: 4,
            ..Default::default()
        },
        stella_tui::McpServerInfo {
            name: "linear".into(),
            title: Some("Linear".into()),
            description: Some("Issues, projects, cycles, and documents.".into()),
            endpoint: "https://mcp.linear.app/mcp".into(),
            kind: "http".into(),
            enabled: false,
            oauth: Some(false),
            ..Default::default()
        },
    ]
}

/// The representative state for each tab: populated entries, a moved cursor,
/// and whatever out-of-band snapshot that tab reads. A tab rendered from
/// `DeckUi::default()` would pin its empty state and nothing else.
fn ui_for(tab: DeckTab) -> DeckUi {
    let mut ui = base_ui(tab);
    match tab {
        // The transcript of the focused agent, scrolled to the live tail.
        DeckTab::Session => ui.focused = 0,
        // Not row 0: a selection that never moves cannot pin the highlight.
        DeckTab::Agents => ui.focused = 1,
        DeckTab::Traces => ui.trace_filter = None,
        DeckTab::Graph => ui.graph_cursor = 1,
        DeckTab::Files => ui.files_sel = 1,
        DeckTab::Skills => {
            ui.skills.view = fixture_skills();
            ui.skills.sel = 1;
        }
        DeckTab::Mcp => ui.mcp.servers = fixture_mcp_servers(),
        DeckTab::Issues => {
            ui.issues.rows = fixture_issues();
            ui.issues.loaded = true;
            ui.issues.sel = 1;
        }
        // Lands on the AGENTS pane; `settings_tools_pane` pins the other one.
        DeckTab::Settings => ui.tools.state = Some(fixture_tool_policy()),
    }
    ui
}

/// The golden's file name and its one-line description, per tab.
fn tab_snapshot_name(tab: DeckTab) -> &'static str {
    match tab {
        DeckTab::Session => "tab_session",
        DeckTab::Agents => "tab_agents",
        DeckTab::Traces => "tab_traces",
        DeckTab::Graph => "tab_graph",
        DeckTab::Files => "tab_files",
        DeckTab::Skills => "tab_skills",
        DeckTab::Mcp => "tab_mcp",
        DeckTab::Issues => "tab_issues",
        DeckTab::Settings => "tab_settings",
    }
}

// ───────────────────────────── the goldens ─────────────────────────────

/// Every tab, whole frame, at the canonical viewport.
///
/// One test over `DeckTab::ALL` rather than nine hand-written ones: the array
/// is the deck's own list, so a tab added later arrives here with no golden and
/// fails until someone blesses one. A tab that has to be added to a test by
/// hand is a tab that will be forgotten.
#[test]
fn deck_render_snapshots_cover_every_tab() {
    let model = fixture_model();
    for tab in DeckTab::ALL {
        let mut ui = ui_for(tab);
        let frame = render_frame(&model, &mut ui, W, H);
        assert_golden(
            tab_snapshot_name(tab),
            &format!("the {} tab over the scripted demo session", tab.title()),
            W,
            H,
            &frame,
        );
    }
}

/// The AGENTS dashboard below its compact threshold.
///
/// The table drops its density columns (CPU%, MEM, …) on a narrow terminal and
/// keeps the identifying ones. That responsive break is a layout decision with
/// no other guard: `tests/deck_snapshot.rs` asserts only that `CPU%` is absent
/// and `Goal` present, which stays true however badly the surviving columns are
/// spaced. This pins the actual narrow layout.
#[test]
fn deck_render_snapshots_pin_the_compact_agents_dashboard() {
    let model = fixture_model();
    let mut ui = ui_for(DeckTab::Agents);
    let frame = render_frame(&model, &mut ui, 120, 24);
    assert_golden(
        "tab_agents_compact",
        "the AGENTS dashboard below its 130-column compact threshold",
        120,
        24,
        &frame,
    );
}

/// The SETTINGS tab's second pane — the tool-switch editor.
///
/// The deck's "tools tab": the panel that shows what this session can actually
/// do, grouped by origin, with an org-locked row that must not render as a
/// working switch.
#[test]
fn deck_render_snapshots_pin_the_settings_tools_pane() {
    let model = fixture_model();
    let mut ui = ui_for(DeckTab::Settings);
    ui.settings_pane = SettingsPane::Tools;
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "pane_settings_tools",
        "the SETTINGS tab's TOOLS pane, with an org-locked row",
        W,
        H,
        &frame,
    );
}

/// The `?` help overlay, drawn over a tab.
///
/// The overlay is context-aware — it lists the active tab's keys plus the
/// deck-wide ones — so its content and its geometry both belong in a golden.
#[test]
fn deck_render_snapshots_pin_the_help_overlay() {
    let model = fixture_model();
    let mut ui = ui_for(DeckTab::Skills);
    ui.help_open = true;
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "overlay_help_skills",
        "the ? help overlay over the SKILLS tab",
        W,
        H,
        &frame,
    );
}

/// The SKILLS create dialog, mid-description.
///
/// A modal drawn over a populated list: the golden pins both the dialog and how
/// much of the list it covers.
#[test]
fn deck_render_snapshots_pin_the_skills_create_dialog() {
    let model = fixture_model();
    let mut ui = ui_for(DeckTab::Skills);
    ui.skills.prompt = Some(SkillPrompt::CreateDescription {
        buffer: "review a terraform plan for destructive changes".to_string(),
    });
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "overlay_skills_create_description",
        "the SKILLS create dialog with a typed description",
        W,
        H,
        &frame,
    );
}

/// The scope picker that follows the create dialog: project or user.
#[test]
fn deck_render_snapshots_pin_the_skills_scope_picker() {
    let model = fixture_model();
    let mut ui = ui_for(DeckTab::Skills);
    ui.skills.prompt = Some(SkillPrompt::Scope {
        action: ScopeAction::Create {
            description: "review a terraform plan for destructive changes".to_string(),
        },
        user: false,
    });
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "overlay_skills_scope",
        "the SKILLS scope picker, project highlighted",
        W,
        H,
        &frame,
    );
}

/// The three floating cards (D3–D6) over the scripted session.
///
/// Cards are glyph-heavy by design — the `▸` selection marker, the plan
/// glyph vocabulary — precisely so a
/// style-blind golden can pin them. One golden per card; the statline's
/// collapsed form is captured in the same frame.
#[test]
fn deck_render_snapshots_pin_the_floating_cards() {
    use stella_tui::deck_ui::cards::Card;
    let model = fixture_model();
    for (card, name, description) in [
        (
            Card::Plan,
            "card_plan",
            "the /plan card over SESSION — every step, then the envelope",
        ),
        (Card::Models, "card_models", "the /models routing card"),
        (Card::Budget, "card_budget", "the /budget editor"),
    ] {
        let mut ui = ui_for(DeckTab::Session);
        ui.cards.raise(card);
        let frame = render_frame(&model, &mut ui, W, H);
        assert_golden(name, description, W, H, &frame);
    }
}

// ─────────────────────────── the harness itself ───────────────────────────

/// The suite's own guard: rendering the same fixture twice must produce the
/// same bytes.
///
/// Everything above is worthless if the frame is not a pure function of the
/// fixture — a golden that drifts gets blessed on every run until nobody reads
/// the diffs. This is the assertion that would have caught the live-pid CPU
/// sample this fixture had to pin.
#[test]
fn deck_render_snapshots_are_reproducible() {
    for tab in DeckTab::ALL {
        let first = render_frame(&fixture_model(), &mut ui_for(tab), W, H);
        let second = render_frame(&fixture_model(), &mut ui_for(tab), W, H);
        assert!(
            first == second,
            "the {} tab rendered differently on a second pass — something in \
             the frame is reading the clock, the process table, or an unordered \
             collection:\n{}",
            tab.title(),
            describe_diff(&first, &second),
        );
    }
}

/// The differ has to be right, or every future failure reads as noise.
#[test]
fn deck_render_snapshots_diff_locates_a_shifted_column() {
    // A header row whose right border slipped one column. Characters 1–8
    // (`│ NAME  `) are identical; the ninth is where they part.
    let golden = "│ NAME  │";
    let actual = "│ NAME   │";
    let report = describe_diff(golden, actual);
    assert!(
        report.contains("first difference at character 9"),
        "the differ should point at the shifted column:\n{report}"
    );
    assert!(
        report.contains("golden │"),
        "both rows are shown:\n{report}"
    );
    assert!(
        report.contains("actual │"),
        "both rows are shown:\n{report}"
    );
}

/// A height change is called out on its own line: it explains why every
/// following row looks different.
#[test]
fn deck_render_snapshots_diff_reports_a_height_change() {
    let report = describe_diff("a\nb\nc", "a\nb");
    assert!(
        report.contains("changed height: 3 lines → 2 lines"),
        "{report}"
    );
}

/// Two files that differ only past the last newline compare unequal but have
/// no differing line. The differ must name that rather than print an empty
/// report — it is what an editor "helpfully" rewriting a golden looks like.
#[test]
fn deck_render_snapshots_diff_explains_a_trailing_newline() {
    let report = describe_diff("frame\n", "frame");
    assert!(report.contains("trailing newlines"), "{report}");
    assert!(
        !report.contains("first difference at character"),
        "there is no differing character to point at:\n{report}"
    );
}
