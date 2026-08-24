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

use stella_protocol::{AgentEvent, ToolCall, ToolOutput};
use stella_tui::Inbound;
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

/// The canonical viewport for the per-tab goldens: 160 columns pins the
/// widest layout each tab has.
/// column set renders in full — pinning the widest layout each tab has.
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

fn fixture_agents() -> Vec<stella_tui::InstalledAgentEntry> {
    let agent = |name: &str, scope, description: &str, tools: Option<&[&str]>, version| {
        stella_tui::InstalledAgentEntry {
            name: name.to_string(),
            description: description.to_string(),
            tools: tools.map(|t| t.iter().map(|s| s.to_string()).collect()),
            scope,
            source_path: format!(".stella/agents/{name}.md"),
            version,
            versions: (1..=version)
                .map(|v| stella_tui::AgentVersionInfo {
                    version: v,
                    label: String::new(),
                })
                .collect(),
            content: format!("---\nname: {name}\n---\n{description}\n"),
        }
    };
    vec![
        agent(
            "reviewer",
            stella_tui::AgentScope::Project,
            "reviews a diff against the house rules",
            Some(&["read_file", "search"]),
            2,
        ),
        agent(
            "release-captain",
            stella_tui::AgentScope::User,
            "cuts a release: bump, changelog, tag",
            None,
            1,
        ),
    ]
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
            ("get_environment", "environment"),
            ("get_state", "scratch"),
            ("save_state", "scratch"),
            ("delegate", "task"),
            ("task_assign", "task"),
            ("mcp__github__create_issue", "mcp"),
            ("deploy_to_staging", "custom"),
        ]
        .into_iter()
        .map(|(name, group)| ToolRow {
            name: name.to_string(),
            group: group.to_string(),
            locked: name == "task_assign",
            off: (name == "task_assign").then(|| ToolDenial {
                key: "task_assign".to_string(),
                scope: Some(ToolScope::Managed),
            }),
        })
        .collect(),
        switches: std::collections::BTreeMap::from([("task_assign".to_string(), false)]),
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
        // A populated list with the second row selected and the first assumed,
        // so both marks are pinned.
        DeckTab::Agents => {
            ui.installed.entries = fixture_agents();
            ui.installed.loaded = true;
            ui.installed.sel = 1;
            ui.installed.assumed = Some("reviewer".into());
        }
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

/// The SETTINGS tab's second pane — the tool-switch editor.
///
/// The deck's "tools tab": the panel that shows what this session can actually
/// do, grouped by origin, with an org-locked row that must not render as a
/// working switch.
/// A multi-line **successful** tool result in the SESSION transcript.
///
/// This golden exists because its absence was itself the finding (#3644). The
/// fold policy for a successful result was rewritten — from one previewed line
/// to six, plus JSON colouring — and **every** existing snapshot in this
/// directory passed through that rewrite unchanged, because not one of them
/// contained a successful result with more than a single line of output. A
/// suite that cannot see a change of that size is not covering the surface it
/// claims to.
///
/// Two payloads, so the frame pins both arms of the policy: a plain-text result
/// long enough to fold, and a JSON result, which takes the syntax-coloured path
/// and the un-skipped-head path (`salient_line` is bypassed for JSON so an
/// object still opens on its brace).
///
/// A golden blessed without reading it is a changelog, not a test — so if this
/// one moves, count the body lines under each result before re-blessing.
#[test]
fn deck_render_snapshots_pin_a_multiline_successful_tool_result() {
    let mut model = fixture_model();
    let ev = |event: AgentEvent| Inbound::Event {
        agent: "lead".into(),
        event,
    };
    for inbound in [
        ev(AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "fold-plain".into(),
                name: "search".into(),
                input: serde_json::json!({ "pattern": "fold_output" }),
            },
            sub_agent_id: None,
        }),
        ev(AgentEvent::ToolResult {
            call_id: "fold-plain".into(),
            output: ToolOutput::Ok {
                content: "crates/stella-transcript/src/digest.rs:100\n\
                          crates/stella-transcript/src/grid.rs:436\n\
                          crates/stella-transcript/src/html.rs:310\n\
                          crates/stella-tui/src/render/entry.rs:66\n\
                          crates/stella-cli/src/export/transcript.rs:644\n\
                          crates/stella-observatory/src/lib.rs:212\n\
                          crates/stella-fleet/src/ledger.rs:88\n\
                          crates/stella-store/src/usage.rs:41"
                    .into(),
                data: None,
            },
            duration_ms: 61,
            speculated: false,
            sub_agent_id: None,
        }),
        ev(AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "fold-json".into(),
                name: "get_state".into(),
                input: serde_json::json!({ "key": "verify" }),
            },
            sub_agent_id: None,
        }),
        ev(AgentEvent::ToolResult {
            call_id: "fold-json".into(),
            output: ToolOutput::Ok {
                content: "{\n\
                          \x20 \"flip\": true,\n\
                          \x20 \"witness\": \"tests/overfull.rs\",\n\
                          \x20 \"attempts\": 3,\n\
                          \x20 \"cost_usd\": 0.0412,\n\
                          \x20 \"stage\": \"verify\",\n\
                          \x20 \"notes\": null\n\
                          }"
                .into(),
                data: None,
            },
            duration_ms: 8,
            speculated: false,
            sub_agent_id: None,
        }),
    ] {
        model.apply_inbound(&inbound);
    }

    let mut ui = ui_for(DeckTab::Session);
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "session_tool_result_multiline",
        "a folded multi-line success and a syntax-coloured JSON result",
        W,
        H,
        &frame,
    );
}

/// **The witness (#4335).** The GRAPH query bar reports what the query cost
/// when the driver measured one.
///
/// A second golden rather than a changed `tab_graph`: the demo snapshot is
/// synthesized and carries no timing, and that is the state the bar must keep
/// rendering — `0ms` on a query nobody ran would be worse than silence. So
/// both halves are pinned, the untimed one by `tab_graph` and the timed one
/// here.
#[test]
fn deck_render_snapshots_pin_the_graph_query_time() {
    let model = fixture_model();
    let mut ui = ui_for(DeckTab::Graph);
    if let Some(graph) = ui.graph.as_mut() {
        graph.query_ms = Some(12);
    }
    let frame = render_frame(&model, &mut ui, W, H);
    assert!(
        frame.contains("· 12ms ·"),
        "the query bar reports the timing:\n{frame}"
    );
    assert_golden(
        "tab_graph_timed",
        "the GRAPH tab with a measured query time in the query bar",
        W,
        H,
        &frame,
    );
}

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

/// **The witness for #4371's other half.** A narrow frame keeps the single
/// column and the `↓ N more` edge.
///
/// The two-column layout is a *wide-frame* affordance, not a replacement: at
/// 100 columns two columns of 47 would clip every description to nothing, so
/// the threshold has to hold in both directions or the fix trades one broken
/// sheet for another.
#[test]
fn the_help_sheet_keeps_one_column_on_a_narrow_frame() {
    let model = fixture_model();
    let mut ui = ui_for(DeckTab::Skills);
    ui.help_open = true;
    let frame = render_frame(&model, &mut ui, 100, H);
    assert!(
        frame.contains("more · ⇟ space · End"),
        "a 100-column sheet must still stack and say what is below:\n{frame}"
    );
    // One column means the everywhere section is BELOW the tab's keys rather
    // than beside them, so both headings are on their own rows.
    for line in frame.lines() {
        assert!(
            !(line.contains("SKILLS tab") && line.contains("everywhere")),
            "a narrow frame laid the sections side by side: {line}"
        );
    }
}

/// SPEC 5 sent five status cells behind `?` as well as to the AGENTS tab, and
/// `?` is the only one of the two left: the overlay drew keybindings only until
/// #4188, and the AGENTS half went with the executions dashboard #4342 removed.
/// So this golden is the whole coverage those five cells have.
///
/// A named assertion beside the golden, deliberately. #4186 is the cautionary
/// tale: a golden pins the whole frame, so a row that quietly stops rendering
/// moves it, and the reviewer's job becomes noticing an absence inside a
/// hundred-line diff. This says which values have to be there, so their loss is
/// a sentence rather than a shifted block.
#[test]
fn the_help_overlay_carries_the_metrics_spec_5_sends_behind_it() {
    let model = fixture_model();
    let mut ui = ui_for(DeckTab::Skills);
    ui.help_open = true;
    // At 160 columns the key sections sit side by side, so the numbers are on
    // the FIRST screen of a 40-row frame — no scroll, and nothing below to
    // announce (#4371). A sheet that fits and still claims there is more
    // below would be lying about its own edge, so that is asserted too.
    let frame = render_frame(&model, &mut ui, W, H);
    assert!(
        !frame.contains("more · ⇟ space · End"),
        "the two-column sheet still clips on a 160×40 frame:\n{frame}"
    );

    for needle in [
        // MODEL detail — which pin serves each role, the question the status
        // bar's single slug cannot answer.
        "worker",
        "zai/glm-5.2-air",
        // ENGINE, CPU and MEM.
        "engine",
        "3 active · 3 total",
        "cpu",
        "42%",
        "mem",
        "148M",
    ] {
        assert!(
            frame.contains(needle),
            "the ? overlay does not carry {needle:?}:\n{frame}"
        );
    }

    // WARMTH is the fifth cell and is deliberately **absent** here: this
    // fixture has no measured cache warmth, and `0s` would say the prompt
    // cache has just expired rather than that nothing has been cached. The
    // elision is the behaviour under test, so it is asserted rather than left
    // to look like an oversight.
    assert!(
        !frame.contains("warmth"),
        "an unmeasured warmth rendered a row anyway:\n{frame}"
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

/// The two questions the `ask_question` overlay fixtures are built from.
///
/// Deliberately unlike each other: the first is single-select with a
/// description on one option and none on the other (so both option shapes are
/// pinned in one frame), the second is multi-select (so the tab strip has a
/// second entry and the `[x]` accumulation is reachable). The asker is a
/// sub-agent, because the attribution chip is the half a driver answering a
/// fanned-out delegation cannot do without — and a golden is the only thing
/// that would notice it silently vanishing from the title row.
fn fixture_question_request(count: usize) -> stella_protocol::QuestionRequest {
    let questions = vec![
        stella_protocol::Question {
            header: "Auth method".into(),
            question: "Which auth should the new endpoint use?".into(),
            options: vec![
                stella_protocol::QuestionOption {
                    label: "Session cookie".into(),
                    description: "Matches the rest of the app".into(),
                },
                stella_protocol::QuestionOption {
                    label: "Bearer token".into(),
                    description: String::new(),
                },
            ],
            multi_select: false,
        },
        stella_protocol::Question {
            header: "Rollout".into(),
            question: "Which environments should it reach first?".into(),
            options: vec![
                stella_protocol::QuestionOption {
                    label: "staging".into(),
                    description: "the usual soak".into(),
                },
                stella_protocol::QuestionOption {
                    label: "canary".into(),
                    description: String::new(),
                },
            ],
            multi_select: true,
        },
    ];
    stella_protocol::QuestionRequest {
        asker: Some("research-child".into()),
        questions: questions.into_iter().take(count).collect(),
    }
}

/// Golden frames for the `ask_question` overlay (#4220) — the wizard a
/// **parked turn** waits on.
///
/// This overlay earns goldens more than most: it is the one surface where a
/// layout regression costs a decision rather than a glance. Every affordance
/// it offers is drawn as a glyph — `▸` selection, `[x]`/`[ ]` choice,
/// `●`/`○` per-question progress, the `▏` editor caret — specifically so a
/// style-stripped golden can pin all of them. A `[x]` that stops rendering,
/// or a tab strip that loses the question it is on, is a driver answering the
/// wrong question, and neither shows up in a `contains` assertion.
#[test]
fn deck_render_snapshots_pin_the_question_overlay() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let model = fixture_model();
    let press = |ui: &mut DeckUi, code: KeyCode| {
        ui.question.key(KeyEvent::new(code, KeyModifiers::NONE));
    };

    // 1. One question, nothing chosen yet — the card as it first appears.
    let mut ui = ui_for(DeckTab::Session);
    ui.question.open(fixture_question_request(1));
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "overlay_question_single",
        "one parked question, freshly raised — options, the appended free-text row, the asker chip",
        W,
        H,
        &frame,
    );

    // 2. Two questions, the first answered — so the tab strip shows both and
    //    marks which one is settled.
    let mut ui = ui_for(DeckTab::Session);
    ui.question.open(fixture_question_request(2));
    press(&mut ui, KeyCode::Enter);
    press(&mut ui, KeyCode::Tab);
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "overlay_question_tabstrip",
        "two questions, the first answered — the tab strip, on the multi-select second",
        W,
        H,
        &frame,
    );

    // 3. The note editor open over a chosen option — the affordance the plain
    //    TTY spells ` # note`.
    let mut ui = ui_for(DeckTab::Session);
    ui.question.open(fixture_question_request(1));
    press(&mut ui, KeyCode::Down);
    press(&mut ui, KeyCode::Enter);
    press(&mut ui, KeyCode::Char('n'));
    for c in "only for the admin routes".chars() {
        press(&mut ui, KeyCode::Char(c));
    }
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "overlay_question_note",
        "the note editor open on a chosen option, mid-typing",
        W,
        H,
        &frame,
    );

    // 4. The review pane: the whole answer set, then the three ways out.
    let mut ui = ui_for(DeckTab::Session);
    ui.question.open(fixture_question_request(2));
    press(&mut ui, KeyCode::Enter);
    press(&mut ui, KeyCode::Char('n'));
    for c in "admin routes only".chars() {
        press(&mut ui, KeyCode::Char(c));
    }
    press(&mut ui, KeyCode::Enter);
    press(&mut ui, KeyCode::Tab);
    press(&mut ui, KeyCode::Enter);
    press(&mut ui, KeyCode::Char('r'));
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "overlay_question_review",
        "the review pane — every answer with its note, then submit / chat / cancel",
        W,
        H,
        &frame,
    );

    // 5. Accessible mode spans the card across the frame instead of floating
    //    it. A float clips its rows at its right border, and this is the one
    //    overlay where a half-heard option is a decision made on half the
    //    facts — so the widening gets a golden of its own rather than living
    //    on a `ui.accessible` nobody would notice going missing.
    let mut ui = ui_for(DeckTab::Session);
    ui.accessible = true;
    ui.question.open(fixture_question_request(1));
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "overlay_question_accessible",
        "accessible mode — the card spans the frame, so no option is clipped",
        W,
        H,
        &frame,
    );
}

/// Golden frames for the approval card (#4240) — the yes/no a parked
/// **dispatch** waits on.
///
/// The fields are the point. `read_only` renders as the word `mutating` or
/// `read-only`, and `gate` gets a labelled row, precisely because the generic
/// `AskUser` card this replaces carried neither to the person deciding. Both
/// are words rather than colours so a style-stripped golden can pin them —
/// if either stops rendering, this test says so.
#[test]
fn deck_render_snapshots_pin_the_approval_card() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let model = fixture_model();
    let press = |ui: &mut DeckUi, code: KeyCode| {
        ui.approval.key(KeyEvent::new(code, KeyModifiers::NONE));
    };
    let request = stella_tools::registry::approval::ApprovalRequest {
        parked: stella_tools::registry::approval::ApprovalSubject::Tool {
            name: "bash".into(),
            read_only: false,
        },
        reason: "matched rule no-destructive-shell".into(),
        gate: "command.started".into(),
        subject: Some("rm -rf build/".into()),
    };

    // 1. The card as it first appears — cursor on Deny, never on Allow.
    let mut ui = ui_for(DeckTab::Session);
    ui.approval.open(request.clone());
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "overlay_approval",
        "a parked approval — tool, mutating mark, subject, gate, reason; cursor on deny",
        W,
        H,
        &frame,
    );

    // 2. The reason editor, where a deny becomes a redirection the turn can
    //    act on rather than a wall it has to guess around.
    let mut ui = ui_for(DeckTab::Session);
    ui.approval.open(request);
    press(&mut ui, KeyCode::Down);
    press(&mut ui, KeyCode::Enter);
    for c in "use the staging bucket instead".chars() {
        press(&mut ui, KeyCode::Char(c));
    }
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "overlay_approval_reason",
        "denying with words — the reason editor open, mid-typing",
        W,
        H,
        &frame,
    );

    // 3. A read-only call, so the mark's other value is pinned too. Nothing
    //    else distinguishes "this reads a file" from "this rewrites one".
    let mut ui = ui_for(DeckTab::Session);
    ui.approval
        .open(stella_tools::registry::approval::ApprovalRequest {
            parked: stella_tools::registry::approval::ApprovalSubject::Tool {
                name: "read_file".into(),
                read_only: true,
            },
            reason: "matched rule secrets-are-off-limits".into(),
            gate: "tool.call.requested".into(),
            subject: Some(".stella/private/credentials.toml".into()),
        });
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "overlay_approval_read_only",
        "a read-only call — the other value of the mark the generic card never showed",
        W,
        H,
        &frame,
    );
}

/// The session-override pickers (`/model`, `/agent`) — the modal lists a
/// selection leaves as `ModelOverride` / `AgentAssume`.
///
/// The `· current` word on the model rows is the point of the first golden:
/// the session's live pin renders as a WORD (style-stripped goldens can pin
/// it), and the fixture's worker pin (`zai/glm-5.2-air`, folded from its
/// latest `StepUsage`) is among the candidates so the marker actually
/// appears.
#[test]
fn deck_render_snapshots_pin_the_session_pickers() {
    let model = fixture_model();

    let mut ui = ui_for(DeckTab::Session);
    ui.engine.state = Some(stella_tui::EngineConfigState {
        catalog_models: vec![
            "zai/glm-5.2-air".to_string(),
            "anthropic/claude-fable-5".to_string(),
            "openrouter/openai/gpt-5.5".to_string(),
        ],
        ..Default::default()
    });
    ui.model_picker.raise();
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "overlay_model_picker",
        "the /model session picker — candidates with the live pin marked current",
        W,
        H,
        &frame,
    );

    let mut ui = ui_for(DeckTab::Session);
    ui.installed.entries = fixture_agents();
    ui.agent_picker.raise();
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "overlay_agent_picker",
        "the /agent session picker — installed definitions with scope and description",
        W,
        H,
        &frame,
    );
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

/// The status bar's CLOCK cell (#2435, #4126): a run with an armed task
/// deadline shows the wall clock it has left, third on the row, right after
/// the stage (SPEC 5).
///
/// A golden **and** a `contains`, and the pair is deliberate. The golden is
/// here because a new cell **moves columns** — every cell to its right shifts,
/// and only the whole grid can say whether the band still fits and what it
/// dropped to make room. The `contains` is here because this test already
/// failed once in the only way a golden can fail: #4123 replaced the two-row
/// statline with the one-row bar, the clock stopped rendering entirely, and
/// this snapshot was re-blessed to a frame with no clock in it while its own
/// docstring still said the clock was there. A golden pins whatever it is
/// shown; it cannot pin what it is *for*. The assertion can, so it does.
///
/// The unarmed case needs no golden of its own: every other snapshot in this
/// file is one, and none of them grew a cell when this landed. That is the
/// evidence for "absent, not zeroed" — a `0s` would have shown up in all of
/// them.
#[test]
fn deck_render_snapshots_pin_the_armed_deadline_statline() {
    let mut model = fixture_model();
    let mut ui = ui_for(DeckTab::Session);
    let agent = model.agents[ui.focused].meta.id.clone();
    model.apply_inbound(&stella_tui::envelope::Inbound::Event {
        agent,
        event: stella_protocol::AgentEvent::BudgetTick {
            spent_usd: 3.77,
            limit_usd: None,
            mode: stella_protocol::BudgetMode::Off,
            session_spent_usd: None,
            session_limit_usd: None,
            // 12m 34s — long enough to exercise the minutes form, and not a
            // round number, so a golden that silently truncated would show it.
            deadline_remaining_ms: Some(754_000),
        },
    });
    let frame = render_frame(&model, &mut ui, W, H);
    assert!(
        frame.contains("deadline 12m 34s"),
        "the armed deadline reached no surface on a default deck — the \
         countdown to a SIGKILL is not something the bar may drop (#4126):\n\
         {frame}"
    );
    assert_golden(
        "statline_deadline_armed",
        "the status bar with a task deadline armed — the countdown after the stage",
        W,
        H,
        &frame,
    );
}

/// SPEC 5 re-homes the v1 wall's PR cell to the ISSUES tab (§9.4), and the
/// half of it with teeth is failing CI: v1 gave a failing PR an elevated drop
/// priority so a narrow row could not hide it, and that intent has to survive
/// the move.
///
/// The demo fixture's PR is `… running`, so the tab goldens cover the ordinary
/// case; this covers the one that is supposed to be impossible to miss. A
/// `contains` rather than a golden because the strip does not move any column
/// the tab goldens already pin — it is one line, and what is under test is
/// that the failure is *stated in words*, not only in a colour the snapshot
/// strips anyway.
#[test]
fn deck_render_snapshots_state_a_failing_pr_on_the_issues_tab() {
    let mut model = fixture_model();
    let mut ui = ui_for(DeckTab::Issues);
    let agent = model.agents[ui.focused].meta.id.clone();
    model.apply_inbound(&stella_tui::envelope::Inbound::Event {
        agent,
        event: stella_protocol::AgentEvent::Pr {
            url: "https://github.com/macanderson/stella/pull/981".into(),
            status: stella_protocol::PrStatus::Open,
            number: Some(981),
            ci: Some(stella_protocol::CiStatus::Failing),
        },
    });
    let frame = render_frame(&model, &mut ui, W, H);
    assert!(
        frame.contains("⇢ #981 open · CI ✗ failing"),
        "a failing PR is not stated on the tab SPEC 5 sends it to:\n{frame}"
    );
}

/// Golden frame for the SUB-AGENTS overlay (`ctrl-a`, `/subagents`): one
/// block per dispatched lane — status, clock, model, the effort its calls
/// are pinned to, spend; then what the lane is for; then where it is in the
/// task, read off its own fold. Every one of those is a word, so the
/// style-stripped golden pins them; the bottom rule pins the verbs
/// (stop · pause/resume · restart · focus) that #4334 lost with the AGENTS
/// dashboard.
#[test]
fn deck_render_snapshots_pin_the_subagents_overlay() {
    let mut model = fixture_model();
    // The driver supplies a purpose and a pinned effort with the lane's
    // registration; the demo scenario registers lanes bare, so give the first
    // one both here and leave the second bare, pinning the fallback too.
    let mut meta = stella_tui::AgentMeta::new("sub:auth", "wire automations triggers API", 0)
        .with_role("subagent")
        .with_purpose("Wire the automations triggers API to the new router.")
        .with_effort("high");
    meta.model = Some("glm-5.2".into());
    model.apply_inbound(&Inbound::Register(meta));
    let mut ui = ui_for(DeckTab::Session);
    ui.subagents.open = true;
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "overlay_subagents",
        "every dispatched lane: vitals with model and effort, its purpose, where it is; the control verbs on the rule",
        W,
        H,
        &frame,
    );
}
