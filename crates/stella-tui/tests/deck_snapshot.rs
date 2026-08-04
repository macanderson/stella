//! Full-deck render snapshot: fold the scripted demo scenario into a
//! `WorkspaceModel`, then render every tab through the real `render_deck`
//! entrypoint into a `TestBackend` and assert the expected content appears.
//! Also writes the rendered frames to `deck-snapshots.txt` under this target's
//! `CARGO_TARGET_TMPDIR` as a human-readable artifact (a text "screenshot" —
//! the honest headless equivalent of a TTY capture). The path is printed by the
//! test; it deliberately stays out of the source tree so running the suite
//! never dirties the working tree and concurrent runs never race on one path.

use std::fmt::Write as _;

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use stella_tui::scenario::{demo_graph, demo_inbound};
use stella_tui::{
    DeckTab, DeckUi, SettingsPane, ToolDenial, ToolPolicyState, ToolRow, ToolScope, WorkspaceModel,
    render_deck,
};

fn folded_model() -> WorkspaceModel {
    let mut model = WorkspaceModel::new();
    model.now_ms = 312_000; // ~5:12 elapsed, so the dashboard timers read nicely
    for inbound in demo_inbound(0, std::process::id()) {
        model.apply_inbound(&inbound);
    }
    model
}

/// Flatten a rendered buffer to one string per row, styling stripped —
/// this suite asserts on content, never on raw ANSI.
fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
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

/// A `DeckUi` past the splash with the scenario's plan-review gate answered —
/// the state every assertion about a *tab's body* below is actually about.
///
/// The gate is a **modal** dialog (#1633): while the focused agent's scope
/// review is pending it is drawn centered over whatever tab is up, because it
/// is the surface that halts the session. The demo scenario reaches a pending
/// plan, so a bare `DeckUi::default()` puts that dialog over the body these
/// tests examine — and does it *quietly*, because the dialog is centered and a
/// needle that happens to sit near an edge survives. Answering it here is what
/// keeps each test about the tab it names instead of about where the dialog
/// landed; [`a_pending_plan_review_is_modal_over_the_tab_body`] is where the
/// modality itself is pinned.
fn answered_ui(model: &WorkspaceModel) -> DeckUi {
    let mut ui = DeckUi::default();
    ui.splash.skip(); // past the splash so the tabs draw
    for entry in &model.agents {
        ui.scope_answered.insert(entry.meta.id.clone());
    }
    ui
}

fn render_tab(model: &WorkspaceModel, tab: DeckTab, w: u16, h: u16) -> String {
    let mut ui = answered_ui(model);
    ui.tab = tab;
    ui.graph = Some(demo_graph());
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| render_deck(model, &mut ui, f)).unwrap();
    buffer_text(terminal.backend().buffer())
}

#[test]
fn deck_renders_every_tab_with_real_content() {
    let model = folded_model();
    assert_eq!(model.agents.len(), 3, "scenario registered 3 agents");

    let cases = [
        (DeckTab::Session, "lead"),
        (DeckTab::Agents, "sub:auth"),
        (DeckTab::Traces, "Which auth guard"),
        (DeckTab::Graph, "run_turn"),
        (DeckTab::Files, "automations"),
    ];
    for (tab, needle) in cases {
        let text = render_tab(&model, tab, 120, 36);
        assert!(
            text.contains(needle),
            "the {tab:?} tab should show {needle:?}, got:\n{text}"
        );
        // The comfy-tabs bar labels are always present — UPPERCASE by the
        // deck's tab-label convention. (Assert on a left-anchored label: at
        // 120 cols the 9-tab bar overflows, so the rightmost SETTINGS clips.)
        assert!(text.contains("SESSION"), "tab bar should render on {tab:?}");
    }

    // The redesigned chrome, on the same scripted session (D1/D2/D5): the
    // two-row labeled statline, the unified stage stepper, and the nested
    // subagent rows under the lead's header.
    let session = render_tab(&model, DeckTab::Session, 190, 44);
    for needle in [
        "MODEL",     // the statline's leading cell
        "CONTEXT",   // a micro-label on the statline's upper row…
        "/200k",     // …over its value on the lower row
        "SPEND",     // the session money cell
        "✓ plan",    // stepper: completed stage
        "▸ execute", // stepper: active stage beside the track
        "◆",         // a nested subagent's identity mark
        "subagent",  // its dim role word
        "returns:",  // its contract line
    ] {
        assert!(
            session.contains(needle),
            "the SESSION chrome should show {needle:?}, got:\n{session}"
        );
    }

    // Each floating card over the same scenario (D3–D6), with its
    // load-bearing copy.
    // Each floating card over the same scenario, with its load-bearing copy.
    // `/plan` is one case where there used to be three: `/tasks`, `/scope` and
    // `/witness` all described the same plan and none of them could show a
    // step's text. The verification records the witness panel carried are now
    // the session frame's DONE VERIFICATION panel, not a card.
    let card_cases: [(stella_tui::deck_ui::cards::Card, &[&str]); 3] = [
        (
            stella_tui::deck_ui::cards::Card::Plan,
            &[
                // the plan and its state
                "plan",
                "pending approval",
                "Refactor the automations store",
                // the steps, readable, with their rings
                "extract types",
                "○",
                // …over the operating envelope
                "repo",
                "write",
                "budget",
                "think",
                "verify",
                "oracle flips red → green",
                "(confirmed from evidence, not self-report)",
            ],
        ),
        (
            stella_tui::deck_ui::cards::Card::Models,
            &["models", "think", "work", "verify"],
        ),
        (
            stella_tui::deck_ui::cards::Card::Budget,
            &["budget", "new cap"],
        ),
    ];
    for (card, needles) in card_cases {
        let mut ui = answered_ui(&model);
        ui.graph = Some(demo_graph());
        // `/models` renders the driver's resolved wiring, which the real deck
        // holds from session start — without it the card would honestly say it
        // has no answer yet, and this golden would pin the empty state.
        ui.engine.pristine = Some(stella_tui::scenario::demo_engine_config());
        ui.cards.raise(card);
        let mut terminal = Terminal::new(TestBackend::new(190, 44)).unwrap();
        terminal.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
        let text = buffer_text(terminal.backend().buffer());
        for needle in needles {
            assert!(
                text.contains(needle),
                "the {card:?} card should show {needle:?}, got:\n{text}"
            );
        }
    }

    // Write all five tabs to a human-readable artifact under the target dir —
    // never into the source tree, which `cargo test` must leave clean.
    let mut out = String::new();
    for tab in DeckTab::ALL {
        let _ = writeln!(out, "\n═══ {} tab ═══\n", tab.title());
        let _ = writeln!(out, "{}", render_tab(&model, tab, 150, 32));
    }
    let artifact = concat!(env!("CARGO_TARGET_TMPDIR"), "/deck-snapshots.txt");
    // Best-effort: never fail the test on an artifact write, but do say where
    // it landed so the "screenshot" stays discoverable under `--nocapture`.
    match std::fs::write(artifact, out) {
        Ok(()) => println!("deck snapshots written to {artifact}"),
        Err(err) => println!("deck snapshots not written to {artifact}: {err}"),
    }
}

/// #1633: a pending plan review is **modal**, so it is drawn over whatever tab
/// happens to be up rather than only over SESSION.
///
/// Reachable exactly as rendered here: a reviewer browsing AGENTS is still
/// browsing it when the lead proposes a plan, and the gate that halts the
/// session has to be the thing they see. Every other test in this file answers
/// the gate through [`answered_ui`] so it can assert on a tab body; this one
/// leaves it pending, which is what keeps the modality a tested property
/// rather than an accident of where the dialog lands.
#[test]
fn a_pending_plan_review_is_modal_over_the_tab_body() {
    let model = folded_model();
    assert!(
        model
            .agents
            .iter()
            .any(|entry| entry.model.pending_scope_review.is_some()),
        "the demo scenario must reach a pending plan review, or this test \
         asserts nothing and `answered_ui` guards nothing"
    );

    // Deliberately NOT `answered_ui`: the unanswered gate is the subject.
    let mut ui = DeckUi::default();
    ui.splash.skip();
    ui.tab = DeckTab::Agents;
    let mut terminal = Terminal::new(TestBackend::new(240, 20)).unwrap();
    terminal.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
    let text = buffer_text(terminal.backend().buffer());

    assert!(
        text.contains("plan review"),
        "the pending gate must draw its dialog over the AGENTS tab:\n{text}"
    );
    assert!(
        text.contains("one keypress decides"),
        "the dialog's own legend is what tells the reviewer it is answerable \
         where they stand:\n{text}"
    );
}

#[test]
fn agents_dashboard_shows_status_and_spend_columns() {
    let model = folded_model();
    // The dashboard is dense (11 columns) and now fills the whole tab (the
    // engine panel moved to SETTINGS) — render at a roomy width so every
    // column shows.
    let text = render_tab(&model, DeckTab::Agents, 240, 20);
    // Column headers and at least one agent's live status all render.
    for needle in ["CPU%", "MEM", "In/Out", "Activity", "needs input"] {
        assert!(
            text.contains(needle),
            "dashboard missing {needle:?}:\n{text}"
        );
    }
    // The config editor no longer shares this tab — its focus hint (unique to
    // the engine panel) must not appear here; it lives on SETTINGS now.
    assert!(
        !text.contains("edit agents config"),
        "the config panel must not render on the AGENTS tab:\n{text}"
    );

    // Below the compact threshold the table drops its density columns (the
    // Goal column must survive, CPU%/MEM/etc. go). The dashboard now fills the
    // whole tab, so this needs a genuinely narrow terminal to trip.
    let text = render_tab(&model, DeckTab::Agents, 130, 20);
    assert!(!text.contains("CPU%"), "compact set drops CPU%:\n{text}");
    assert!(
        text.contains("Goal"),
        "compact set keeps the Goal column:\n{text}"
    );
}

#[test]
fn settings_tab_hosts_the_agents_config_editor() {
    let model = folded_model();
    // The config editor is one of the SETTINGS tab's two sections.
    let text = render_tab(&model, DeckTab::Settings, 120, 24);
    assert!(
        text.contains("agents"),
        "the config panel title renders on SETTINGS:\n{text}"
    );
    // Its GLOBAL / per-agent sub-tabs and the focus hint render.
    assert!(
        text.contains("GLOBAL"),
        "the engine panel's GLOBAL sub-tab renders:\n{text}"
    );
    assert!(
        text.contains("edit agents config"),
        "the unfocused focus hint renders:\n{text}"
    );
}

/// The SETTINGS tab's second pane: the tool-switch editor, listing what
/// this session actually has — including a connected MCP server's tool and a
/// tool the customer registered themselves, neither of which any compiled-in
/// table knows about.
#[test]
fn settings_tab_lists_the_sessions_tools_grouped_with_mcp_and_custom_sections() {
    let model = folded_model();
    let mut ui = answered_ui(&model);
    ui.tab = DeckTab::Settings;
    // The tab shows one pane at a time behind its ←/→ nav — walk to TOOLS,
    // which is what a user pressing → from the default AGENTS pane sees.
    ui.settings_pane = SettingsPane::Tools;
    ui.tools.state = Some(ToolPolicyState {
        tools: [
            ("read_file", "file"),
            ("bash", "bash"),
            ("start_process", "process"),
            ("send_stdin", "process"),
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
    });

    let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
    terminal.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
    let text = buffer_text(terminal.backend().buffer());

    // The frame goes to the same discoverable artifact the whole-deck snapshot
    // uses — the honest headless stand-in for a TTY capture of this panel.
    let artifact = concat!(env!("CARGO_TARGET_TMPDIR"), "/settings-tools-panel.txt");
    match std::fs::write(artifact, &text) {
        Ok(()) => println!("SETTINGS tab frame written to {artifact}"),
        Err(err) => println!("SETTINGS tab frame not written to {artifact}: {err}"),
    }

    assert!(
        text.contains("tools ·"),
        "the tool panel fills the tab on the TOOLS pane:\n{text}"
    );
    for section in ["FILE", "PROCESS", "MCP", "CUSTOM"] {
        assert!(
            text.contains(section),
            "the {section} section header renders:\n{text}"
        );
    }
    assert!(
        text.contains("deploy_to_staging"),
        "a customer's own tool is listed by name:\n{text}"
    );
    assert!(
        text.contains("mcp__github__create_is"),
        "an MCP server's tool is listed by name:\n{text}"
    );
    assert!(
        text.contains("org-locked") && text.contains("locked"),
        "an org-denied row renders locked rather than as a working switch:\n{text}"
    );
    // `e` edits the pane you are on, so that is the hint the visible panel
    // teaches — the pane-specific `t` survives only as an accelerator.
    assert!(
        text.contains("e edit tool switches"),
        "the unfocused focus hint renders:\n{text}"
    );
    assert!(
        text.contains("AGENTS") && text.contains("TOOLS"),
        "the ←/→ pane nav sits above the panel:\n{text}"
    );
}

/// The `?` help overlay is context-aware: it lists the active tab's own keys
/// plus the deck-wide keys — and nothing from other tabs — as one aligned
/// `key  description` row per shortcut.
#[test]
fn help_overlay_shows_only_the_active_tabs_shortcuts() {
    let model = folded_model();
    let render_help = |tab: DeckTab| {
        let mut ui = answered_ui(&model);
        ui.tab = tab;
        ui.help_open = true;
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
        buffer_text(terminal.backend().buffer())
    };

    let traces = render_help(DeckTab::Traces);
    assert!(
        traces.contains("TRACES tab"),
        "titled for the tab:\n{traces}"
    );
    assert!(traces.contains("cycle the per-agent filter"), "{traces}");
    // Deck-wide keys are always present…
    assert!(traces.contains("switch tabs"), "{traces}");
    assert!(traces.contains("quit stella"), "{traces}");
    // …but other tabs' keys are not.
    assert!(!traces.contains("search skills"), "{traces}");
    assert!(!traces.contains("OAuth login"), "{traces}");

    let skills = render_help(DeckTab::Skills);
    assert!(skills.contains("SKILLS tab"), "{skills}");
    assert!(skills.contains("search skills"), "{skills}");
    assert!(!skills.contains("cycle the per-agent filter"), "{skills}");
}

/// The INSPECT overlay in both of its modes, through the real `render_deck`
/// entrypoint. This is the closest headless equivalent of opening it in a TTY:
/// if the prompt bytes are not on the screen, the feature does not work.
#[test]
fn inspect_overlay_renders_the_call_list_then_the_context_sent() {
    use stella_tui::{InspectMessage, InspectView, RecordedCallInfo};

    let model = folded_model();
    let call = |step: u64, call_seq: u64, role: &str| RecordedCallInfo {
        turn_instance: 0,
        step,
        call_seq,
        call_role: role.into(),
        provider: "anthropic".into(),
        model: "claude-opus".into(),
        estimated_input_tokens: 1234,
    };

    let render = |ui: &mut DeckUi| {
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| render_deck(&model, ui, f)).unwrap();
        buffer_text(terminal.backend().buffer())
    };

    // Mode 1: the call list. Both calls of step 3 must be distinguishable —
    // that is the whole point of keying receipts on call_seq.
    let mut ui = answered_ui(&model);
    ui.inspect_open = true;
    ui.inspect_calls = vec![
        call(0, 0, "worker"),
        call(3, 1, "summarization"),
        call(3, 0, "worker"),
    ];
    ui.inspect_sel = 1;
    let list = render(&mut ui);
    assert!(list.contains("recorded calls"), "titled:\n{list}");
    assert!(
        list.contains("summarization"),
        "the auxiliary call is listed:\n{list}"
    );
    assert!(list.contains("worker"), "{list}");
    assert!(
        list.contains("show the context it was sent"),
        "the footer says what ⏎ does:\n{list}"
    );

    // Mode 2: the reconstructed context. The system prompt must be readable.
    ui.inspect_view = Some(Box::new(InspectView {
        call: call(3, 1, "summarization"),
        messages: vec![
            InspectMessage {
                role: "system".into(),
                content: "Condense this span faithfully.".into(),
            },
            InspectMessage {
                role: "user".into(),
                content: "t0 t1 t2".into(),
            },
        ],
        verified: true,
        unresolved: 0,
        digest_mismatches: 0,
    }));
    let detail = render(&mut ui);
    assert!(detail.contains("context sent"), "titled:\n{detail}");
    assert!(
        detail.contains("Condense this span faithfully."),
        "the system prompt is on screen — the feature:\n{detail}"
    );
    assert!(
        detail.contains("turn 0 · step 3 · call-seq 1"),
        "the coordinate is shown:\n{detail}"
    );
    assert!(
        detail.contains("verified"),
        "the verdict is shown:\n{detail}"
    );

    // A digest mismatch must read differently from a coverage gap.
    if let Some(view) = ui.inspect_view.as_mut() {
        view.verified = false;
        view.digest_mismatches = 2;
    }
    let mismatched = render(&mut ui);
    assert!(
        mismatched.contains("did not re-hash"),
        "a digest mismatch is called out, not folded into 'unverified':\n{mismatched}"
    );
    // ...and it must NOT read as tampering. The common cause is a compaction
    // rewrite the journal never learned about, so the line names that instead
    // of accusing the journal of being torn or altered. Pinned because the
    // wording IS the feature here: an integrity alarm that fires on ordinary
    // housekeeping is one the reader learns to skip.
    for accusation in ["torn", "altered", "tamper"] {
        assert!(
            !mismatched.contains(accusation),
            "the mismatch line must not imply tampering, found {accusation:?}:\n{mismatched}"
        );
    }
    assert!(
        mismatched.contains("compaction rewrite"),
        "the likely benign cause is named:\n{mismatched}"
    );
}

/// Witness for #689. `stella-mcp` has recorded per-server tool truncation
/// since #551, but nothing rendered it: an over-advertising server lost every
/// tool past the cap and the operator was never told, so the model silently
/// had less surface than the server offered.
///
/// Renders through the real `render_deck` entrypoint, so it fails if the tab
/// stops drawing the signal for any reason — not just if the field is dropped.
#[test]
fn mcp_tab_renders_truncation_distinctly_from_an_unavailable_server() {
    fn server(name: &str, connected: bool, dropped: usize) -> stella_tui::McpServerInfo {
        stella_tui::McpServerInfo {
            name: name.to_string(),
            kind: "stdio".to_string(),
            enabled: true,
            connected,
            health: connected.then(|| "live".to_string()),
            tool_count: if connected { 256 } else { 0 },
            dropped_tools: dropped,
            auth_fields: Vec::new(),
            oauth: None,
            calls: 0,
            ..Default::default()
        }
    }

    let model = folded_model();
    let mut ui = answered_ui(&model);
    ui.tab = DeckTab::Mcp;
    ui.mcp.servers = vec![server("greedy", true, 12), server("dead", false, 0)];

    let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
    terminal.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
    let screen = buffer_text(terminal.backend().buffer());

    assert!(
        screen.contains("12 dropped past cap"),
        "the truncated server's lost surface is on its row:\n{screen}"
    );
    assert!(
        screen.contains("1 truncated, 12 tools dropped"),
        "the session total rides in the tab title, so an operator scanning \
         tabs sees it without opening this one:\n{screen}"
    );
    // `dead` is the unavailable one; `greedy` is up and routing. The two must
    // not render the same, which is the whole reason this signal is separate
    // from `failed_servers()`.
    assert!(
        screen.contains("not connected"),
        "the genuinely-down server still reads as not connected:\n{screen}"
    );
}

/// A well-behaved session must not grow a truncation notice — a warning that
/// fires at zero trains operators to ignore the real one.
#[test]
fn mcp_tab_says_nothing_about_the_cap_when_no_server_truncated() {
    let model = folded_model();
    let mut ui = answered_ui(&model);
    ui.tab = DeckTab::Mcp;
    ui.mcp.servers = vec![stella_tui::McpServerInfo {
        name: "files".to_string(),
        kind: "stdio".to_string(),
        enabled: true,
        connected: true,
        health: Some("live".to_string()),
        tool_count: 7,
        dropped_tools: 0,
        auth_fields: Vec::new(),
        oauth: None,
        calls: 3,
        ..Default::default()
    }];

    let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
    terminal.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
    let screen = buffer_text(terminal.backend().buffer());

    assert!(!screen.contains("dropped"), "no cap notice:\n{screen}");
    assert!(!screen.contains("truncated"), "no cap notice:\n{screen}");
}

/// End-to-end proof of the reported problem and its fix: a real render of the
/// MCP tab, through `render_deck`, must identify each server.
///
/// The bug was that a configured server showed as its config alias and nothing
/// else — `mcp [http] not connected` — with no way to learn which vendor it
/// was short of starting its OAuth flow and reading the browser.
#[test]
fn mcp_tab_identifies_each_server_rather_than_showing_a_bare_alias() {
    let model = folded_model();
    let mut ui = answered_ui(&model);
    ui.tab = DeckTab::Mcp;
    ui.mcp.servers = vec![
        // Installed from the registry: the publisher's own words.
        stella_tui::McpServerInfo {
            name: "mcp".into(),
            title: Some("Stripe".into()),
            description: Some("Payments, refunds, and balance reads.".into()),
            endpoint: "https://mcp.stripe.com/v1".into(),
            kind: "http".into(),
            enabled: true,
            oauth: Some(false),
            ..Default::default()
        },
        // Hand-written, never in a registry: the endpoint carries the meaning.
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
    ];

    let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
    terminal.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
    let screen = buffer_text(terminal.backend().buffer());

    assert!(
        screen.contains("Stripe"),
        "the vendor behind the `mcp` alias is unnamed:\n{screen}"
    );
    assert!(
        screen.contains("Payments, refunds"),
        "the recorded description is not on screen:\n{screen}"
    );
    assert!(
        screen.contains("mcp"),
        "the alias is the routing token and must survive:\n{screen}"
    );
    assert!(
        screen.contains("server-filesystem"),
        "a server with no description falls back to its endpoint:\n{screen}"
    );
    assert!(
        screen.contains("ctrl+o"),
        "the inspector is not discoverable from the footer:\n{screen}"
    );
}

/// The ctrl+o inspector, rendered through `render_deck` over the real list.
///
/// Also writes both frames to the target tmpdir — the headless stand-in for a
/// TTY screenshot, the same convention the whole-deck snapshot uses.
#[test]
fn mcp_inspector_renders_over_the_list_and_answers_what_the_row_cannot() {
    let model = folded_model();
    let mut ui = answered_ui(&model);
    ui.tab = DeckTab::Mcp;
    ui.mcp.servers = vec![
        stella_tui::McpServerInfo {
            name: "mcp".into(),
            title: Some("Stripe".into()),
            description: Some("Payments, refunds, disputes, and balance reads.".into()),
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
            auth_fields: vec!["ROOT".into()],
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
    ];

    let mut out = String::new();
    let mut terminal = Terminal::new(TestBackend::new(150, 20)).unwrap();
    terminal.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
    let _ = writeln!(out, "\n═══ MCP tab — list ═══\n");
    let _ = writeln!(out, "{}", buffer_text(terminal.backend().buffer()));

    ui.mcp.inspector = Some(stella_tui::views::mcp::McpInspector {
        server: "mcp".into(),
        detail: Some(stella_tui::McpServerDetail {
            name: "mcp".into(),
            title: Some("Stripe".into()),
            description: Some(
                "Payments, refunds, disputes, and balance reads over the Stripe API.".into(),
            ),
            registry_name: Some("com.stripe/mcp".into()),
            repository: Some("https://github.com/stripe/agent-toolkit".into()),
            version: Some("0.4.2".into()),
            kind: "http".into(),
            endpoint: "https://mcp.stripe.com/v1".into(),
            oauth: Some(true),
            enabled: true,
            connected: true,
            health: Some("live".into()),
            calls: 14,
            live: Some(stella_tui::McpLiveIdentity {
                name: Some("stripe-mcp".into()),
                version: Some("2.1.0".into()),
                protocol_version: "2025-06-18".into(),
                ..Default::default()
            }),
            tools: vec![
                stella_tui::McpToolRow {
                    name: "create_refund".into(),
                    description: "Refund a charge, in full or in part.".into(),
                    calls: 3,
                    ..Default::default()
                },
                stella_tui::McpToolRow {
                    name: "list_customers".into(),
                    description: "Page through customers on the account.".into(),
                    calls: 11,
                    safe_to_retry: true,
                },
            ],
            ..Default::default()
        }),
        scroll: 0,
    });
    let mut terminal = Terminal::new(TestBackend::new(150, 46)).unwrap();
    terminal.draw(|f| render_deck(&model, &mut ui, f)).unwrap();
    let _ = writeln!(out, "\n═══ MCP tab — ctrl+o inspector ═══\n");
    let _ = writeln!(out, "{}", buffer_text(terminal.backend().buffer()));

    // The inspector answers the questions the list only gestures at — assert
    // that here rather than leaving this test a pure artifact writer.
    for needle in [
        "com.stripe/mcp",    // which registry entry this alias came from
        "agent-toolkit",     // whose code is behind it
        "stripe-mcp v2.1.0", // what the live process announces itself as
        "2025-06-18",        // the protocol it negotiated
        "logged in",         // how it authenticates
        "create_refund",     // what it can actually do
        "Refund a charge",   // and in the server's own words
    ] {
        assert!(
            out.contains(needle),
            "the inspector should show {needle:?}:\n{out}"
        );
    }

    let artifact = concat!(env!("CARGO_TARGET_TMPDIR"), "/mcp-tab.txt");
    match std::fs::write(artifact, &out) {
        Ok(()) => println!("MCP tab frames written to {artifact}"),
        Err(err) => println!("MCP tab frames not written to {artifact}: {err}"),
    }
}
