use super::*;
use crate::deck::WorkspaceModel;
use crate::deck_ui::{handle_deck_key, ingest_inbound};
use crate::envelope::{Inbound, ToolDenial};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn ch(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}

fn tool(name: &str, group: &str) -> ToolRow {
    ToolRow {
        name: name.into(),
        group: group.into(),
        locked: false,
        off: None,
    }
}

/// A session with the built-in families, an MCP server's tool, and a
/// customer's own registered tool. The pane must show all three sources.
fn sample_state() -> ToolPolicyState {
    ToolPolicyState {
        tools: vec![
            tool("get_environment", "environment"),
            tool("delegate", "task"),
            tool("get_state", "scratch"),
            tool("save_state", "scratch"),
            tool("mcp__gh__create_issue", "mcp"),
            tool("deploy_to_staging", "custom"),
        ],
        switches: BTreeMap::new(),
        overlay_switches: BTreeMap::new(),
    }
}

fn open_ui() -> (WorkspaceModel, DeckUi) {
    let model = WorkspaceModel::new();
    let mut ui = DeckUi::default();
    ui.splash.skip();
    ui.set_tab(DeckTab::Settings);
    ui.tools.focused = true;
    ui.tools.state = Some(sample_state());
    (model, ui)
}

#[test]
fn rows_group_every_source_including_mcp_and_custom_tools() {
    let state = sample_state();
    let labels: Vec<String> = rows(&state)
        .into_iter()
        .map(|row| match row {
            ToolsRow::Group(g) => format!("[{g}]"),
            ToolsRow::Tool(i) => state.tools[i].name.clone(),
        })
        .collect();
    assert_eq!(
        labels,
        vec![
            "[custom]".to_string(),
            // A customer's own tool gets its own section, by name.
            "deploy_to_staging".to_string(),
            "[environment]".to_string(),
            "get_environment".to_string(),
            "[mcp]".to_string(),
            "mcp__gh__create_issue".to_string(),
            "[scratch]".to_string(),
            // Sorted within the group.
            "get_state".to_string(),
            "save_state".to_string(),
            "[task]".to_string(),
            "delegate".to_string(),
        ],
        "groups sorted, tools sorted within them, one header each"
    );
}

#[test]
fn a_row_resolves_most_specific_key_first_over_saved_switches_and_edits() {
    let mut state = sample_state();
    state.switches.insert(WILDCARD.into(), false);
    state.switches.insert("scratch".into(), true);
    state.switches.insert("save_state".into(), false);
    let none = BTreeMap::new();

    let at = |name: &str| {
        state
            .tools
            .iter()
            .find(|t| t.name == name)
            .expect("tool present")
    };
    assert!(
        !tool_enabled(&state, &none, at("get_environment")),
        "wildcard off"
    );
    assert!(
        tool_enabled(&state, &none, at("get_state")),
        "group on beats the wildcard"
    );
    assert!(
        !tool_enabled(&state, &none, at("save_state")),
        "exact off beats the group"
    );

    // An edit at a LESS specific level must not defeat a saved exact key.
    // If it did, the pane would show a tool as on that the runtime keeps off.
    let mut edits = BTreeMap::new();
    edits.insert("scratch".to_string(), true);
    assert!(
        !tool_enabled(&state, &edits, at("save_state")),
        "a pending group grant does not outrank a saved exact denial"
    );
    // …and an edit at the SAME level does.
    edits.insert("save_state".to_string(), true);
    assert!(tool_enabled(&state, &edits, at("save_state")));
}

/// Over `save_state`, whose group (`scratch`) has a
/// DIFFERENT name. No catalog row is named after its own group now. A row
/// whose name and group matched would hide the bug, so this test uses a
/// row where they differ.
#[test]
fn toggling_a_tool_writes_its_exact_name_never_its_group() {
    let (model, mut ui) = open_ui();
    let rows = ui.tools.rows();
    let state = ui.tools.state.clone().unwrap();
    ui.tools.row = rows
        .iter()
        .position(
            |row| matches!(row, ToolsRow::Tool(i) if state.tools[*i].name == "save_state"),
        )
        .expect("save_state row");

    handle_deck_key(ch(' '), &model, &mut ui);
    assert_eq!(
        ui.tools.edits,
        BTreeMap::from([("save_state".to_string(), false)]),
        "the exact tool key, and only it — never the `scratch` group"
    );
    // Its sibling in the same group is untouched, which is the whole point
    // of writing the most specific key.
    let get_state = state.tools.iter().find(|t| t.name == "get_state").unwrap();
    assert!(tool_enabled(&state, &ui.tools.edits, get_state));

    // Toggling back flips the same key rather than deleting it.
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        ui.tools.edits,
        BTreeMap::from([("save_state".to_string(), true)])
    );
    // `x` drops the unsaved edit entirely.
    handle_deck_key(ch('x'), &model, &mut ui);
    assert!(ui.tools.edits.is_empty(), "x clears the pending edit");
    assert!(!ui.tools.dirty());
}

#[test]
fn toggling_a_group_header_writes_the_group_key() {
    let (model, mut ui) = open_ui();
    let rows = ui.tools.rows();
    ui.tools.row = rows
        .iter()
        .position(|row| matches!(row, ToolsRow::Group(g) if g == "scratch"))
        .expect("scratch header");
    handle_deck_key(ch(' '), &model, &mut ui);
    assert_eq!(
        ui.tools.edits,
        BTreeMap::from([("scratch".to_string(), false)]),
        "the group key covers the family in one line"
    );
    let state = ui.tools.state.clone().unwrap();
    for name in ["get_state", "save_state"] {
        let tool = state.tools.iter().find(|t| t.name == name).unwrap();
        assert!(
            !tool_enabled(&state, &ui.tools.edits, tool),
            "{name} is off"
        );
    }
    // A member edit made afterwards is more specific and wins.
    ui.tools.row = rows
        .iter()
        .position(|row| matches!(row, ToolsRow::Tool(i) if state.tools[*i].name == "get_state"))
        .expect("get_state row");
    handle_deck_key(ch(' '), &model, &mut ui);
    assert_eq!(ui.tools.edits.get("get_state"), Some(&true));
}

#[test]
fn a_managed_denied_row_is_locked_and_refuses_to_toggle_on() {
    let (model, mut ui) = open_ui();
    let mut state = sample_state();
    state.switches.insert("delegate".into(), false);
    for tool in &mut state.tools {
        if tool.name == "delegate" {
            tool.locked = true;
            tool.off = Some(ToolDenial {
                key: "delegate".into(),
                scope: Some(ToolScope::Managed),
            });
        }
    }
    ui.tools.state = Some(state);
    let rows = ui.tools.rows();
    let state_snapshot = ui.tools.state.clone().unwrap();
    ui.tools.row = rows
        .iter()
        .position(
            |row| matches!(row, ToolsRow::Tool(i) if state_snapshot.tools[*i].name == "delegate"),
        )
        .expect("the `delegate` tool row");

    handle_deck_key(ch(' '), &model, &mut ui);
    assert!(
        ui.tools.edits.is_empty(),
        "a locked row must not produce a switch the org will drop"
    );
    assert!(
        ui.tools
            .status
            .as_deref()
            .is_some_and(|s| s.contains("org-managed")),
        "the refusal says why: {:?}",
        ui.tools.status
    );
    let state = ui.tools.state.as_ref().unwrap();
    let delegate = state.tools.iter().find(|t| t.name == "delegate").unwrap();
    assert!(!tool_enabled(state, &ui.tools.edits, delegate));
    assert_eq!(
        off_reason(delegate).as_deref(),
        Some("locked · \"delegate\" off in org-managed settings")
    );

    // Even a forged edit (a group or wildcard grant reaching the map any
    // other way) cannot render it on.
    let mut edits = BTreeMap::new();
    edits.insert(WILDCARD.to_string(), true);
    edits.insert("delegate".to_string(), true);
    assert!(
        !tool_enabled(state, &edits, delegate),
        "locked short-circuits every level of the precedence ladder"
    );
    // The group header the org fully denies is locked too.
    assert!(group_locked(state, "task"));
}

/// `delegate` as the launch overlay denies it: off, locked, and named.
fn overlay_denied_state() -> ToolPolicyState {
    let mut state = sample_state();
    state.switches.insert("delegate".into(), false);
    for tool in &mut state.tools {
        if tool.name == "delegate" {
            tool.locked = true;
            tool.off = Some(ToolDenial {
                key: "delegate".into(),
                scope: Some(ToolScope::Overlay),
            });
        }
    }
    state
}

/// The launch overlay's key beats the user and project files, so a switch
/// saved to either would stay off. The row locks, refuses the toggle, and
/// names the overlay rather than the org.
#[test]
fn an_overlay_denied_row_is_locked_and_names_the_launch_overlay() {
    let (model, mut ui) = open_ui();
    ui.tools.state = Some(overlay_denied_state());
    let state = overlay_denied_state();
    let rows = ui.tools.rows();
    ui.tools.row = rows
        .iter()
        .position(|row| matches!(row, ToolsRow::Tool(i) if state.tools[*i].name == "delegate"))
        .expect("the `delegate` tool row");

    handle_deck_key(ch(' '), &model, &mut ui);
    assert!(ui.tools.edits.is_empty(), "the toggle is refused");
    assert!(
        ui.tools
            .status
            .as_deref()
            .is_some_and(|s| s.contains("launch overlay") && !s.contains("org-managed")),
        "the refusal names the overlay: {:?}",
        ui.tools.status
    );

    let delegate = state.tools.iter().find(|t| t.name == "delegate").unwrap();
    assert!(!tool_enabled(&state, &ui.tools.edits, delegate));
    assert_eq!(
        off_reason(delegate).as_deref(),
        Some("locked · \"delegate\" off in launch overlay settings")
    );
    assert!(group_locked(&state, "task"));
    assert_eq!(group_lock_source(&state, "task"), "launch overlay");
    let tag = lock_tag(group_lock_source(&state, "task"));
    assert_eq!(tag, "overlay-locked", "the header's tag");
}

/// An edit made before the row locked is dropped when the locked
/// snapshot lands. Kept, it would read as unsaved for good: no save to
/// the user or project file can turn the tool on. An edit to a row that
/// is still a switch stays.
#[test]
fn a_snapshot_that_locks_a_row_drops_its_pending_edit() {
    let mut model = WorkspaceModel::new();
    let mut ui = DeckUi::default();
    ui.splash.skip();
    ui.tools.state = Some(sample_state());
    ui.tools.edits.insert("delegate".into(), true);
    ui.tools.edits.insert("deploy_to_staging".into(), false);

    ingest_inbound(
        &Inbound::ToolPolicy {
            state: overlay_denied_state(),
            status: None,
        },
        &mut model,
        &mut ui,
    );

    assert_eq!(ui.tools.edits.get("delegate"), None, "dropped");
    assert_eq!(
        ui.tools.edits.get("deploy_to_staging"),
        Some(&false),
        "an edit to an open row stays"
    );
}

/// The overlay turns the `scratch` group off, and the user file turns
/// `save_state` back on by name. So `get_state` locks and `save_state`
/// stays a switch, and the header is not locked.
fn overlay_holds_group_state() -> ToolPolicyState {
    let mut state = sample_state();
    state.switches.insert("scratch".into(), false);
    state.switches.insert("save_state".into(), true);
    state.overlay_switches.insert("scratch".into(), false);
    for tool in &mut state.tools {
        if tool.name == "get_state" {
            tool.locked = true;
            tool.off = Some(ToolDenial {
                key: "scratch".into(),
                scope: Some(ToolScope::Overlay),
            });
        }
    }
    state
}

/// The header writes the group key, and the overlay sets that key. A save
/// to the user or project file would not change it, so the edit would sit
/// unsaved for good. The header refuses and names the overlay, and a
/// snapshot drops such an edit if one got in.
#[test]
fn a_header_whose_group_key_the_overlay_sets_refuses_to_toggle() {
    let (mut model, mut ui) = open_ui();
    let state = overlay_holds_group_state();
    ui.tools.state = Some(state.clone());
    assert!(
        !group_locked(&state, "scratch"),
        "save_state is still a switch, so the header is not locked"
    );
    let rows = ui.tools.rows();
    ui.tools.row = rows
        .iter()
        .position(|row| matches!(row, ToolsRow::Group(g) if g == "scratch"))
        .expect("the `scratch` header");

    handle_deck_key(ch(' '), &model, &mut ui);
    assert!(ui.tools.edits.is_empty(), "the header toggle is refused");
    assert!(
        ui.tools
            .status
            .as_deref()
            .is_some_and(|s| s.contains("\"scratch\"") && s.contains("launch overlay")),
        "the refusal names the key and the overlay: {:?}",
        ui.tools.status
    );

    ui.tools.edits.insert("scratch".into(), true);
    ingest_inbound(
        &Inbound::ToolPolicy {
            state,
            status: None,
        },
        &mut model,
        &mut ui,
    );
    assert_eq!(ui.tools.edits.get("scratch"), None, "dropped");
}

/// The overlay turns `delegate` on by name. Switching it off in the user
/// or project file would not land either, so the row refuses too.
#[test]
fn a_row_whose_name_the_overlay_sets_refuses_to_toggle() {
    let (model, mut ui) = open_ui();
    let mut state = sample_state();
    state.switches.insert("delegate".into(), true);
    state.overlay_switches.insert("delegate".into(), true);
    ui.tools.state = Some(state.clone());
    let rows = ui.tools.rows();
    ui.tools.row = rows
        .iter()
        .position(|row| matches!(row, ToolsRow::Tool(i) if state.tools[*i].name == "delegate"))
        .expect("the `delegate` tool row");

    handle_deck_key(ch(' '), &model, &mut ui);
    assert!(ui.tools.edits.is_empty(), "the toggle is refused");
    assert!(
        ui.tools
            .status
            .as_deref()
            .is_some_and(|s| s.contains("launch overlay")),
        "the refusal names the overlay: {:?}",
        ui.tools.status
    );
}

#[test]
fn s_and_shift_s_send_only_the_edited_keys_at_the_chosen_scope() {
    let (model, mut ui) = open_ui();
    ui.tools.row = 1; // deploy_to_staging, the first tool row
    handle_deck_key(ch(' '), &model, &mut ui);

    handle_deck_key(ch('s'), &model, &mut ui);
    assert_eq!(
        ui.pending_inputs,
        vec![WorkspaceInput::ToolsSave {
            switches: BTreeMap::from([("deploy_to_staging".to_string(), false)]),
            scope: AgentScope::User,
        }],
        "only the changed key goes out, at user scope"
    );
    assert!(ui.tools.busy);

    ui.pending_inputs.clear();
    handle_deck_key(ch('S'), &model, &mut ui);
    assert_eq!(
        ui.pending_inputs,
        vec![WorkspaceInput::ToolsSave {
            switches: BTreeMap::from([("deploy_to_staging".to_string(), false)]),
            scope: AgentScope::Project,
        }]
    );
}

#[test]
fn the_save_echo_retires_the_edit_and_a_failure_keeps_it() {
    let mut model = WorkspaceModel::new();
    let mut ui = DeckUi::default();
    ui.splash.skip();
    ui.tools.state = Some(sample_state());
    ui.tools.edits.insert("delegate".into(), false);
    ui.tools.busy = true;

    // A failed save: the snapshot still says `delegate` is on, so the edit
    // stands.
    ingest_inbound(
        &Inbound::ToolPolicy {
            state: sample_state(),
            status: Some("save failed: cannot read".into()),
        },
        &mut model,
        &mut ui,
    );
    assert_eq!(
        ui.tools.edits.get("delegate"),
        Some(&false),
        "still unsaved"
    );
    assert!(!ui.tools.busy, "a snapshot always ends the in-flight op");
    assert!(ui.tools.dirty());

    // The successful echo carries the switch — the marker clears.
    let mut saved = sample_state();
    saved.switches.insert("delegate".into(), false);
    ingest_inbound(
        &Inbound::ToolPolicy {
            state: saved.clone(),
            status: Some("saved to user settings".into()),
        },
        &mut model,
        &mut ui,
    );
    assert!(ui.tools.edits.is_empty(), "the write landed");
    assert!(!ui.tools.dirty());
    assert_eq!(ui.tools.state.as_ref(), Some(&saved));
    assert_eq!(ui.tools.status.as_deref(), Some("saved to user settings"));
}

#[test]
fn t_on_the_settings_tab_focuses_the_panel_and_esc_releases_it() {
    let model = WorkspaceModel::new();
    let mut ui = DeckUi::default();
    ui.splash.skip();
    ui.set_tab(DeckTab::Settings);

    let action = handle_deck_key(ch('t'), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::ToolsRefresh),
        "focusing asks the driver for the live tool list"
    );
    assert!(ui.tools.focused);

    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert!(!ui.tools.focused, "esc hands the keyboard back to the tab");
    // `e` then reaches the engine panel, which takes the keyboard from the
    // tools pane — one editor owns the SETTINGS keyboard at a time.
    handle_deck_key(ch('t'), &model, &mut ui);
    crate::views::engine_panel::focus_panel(&mut ui);
    assert!(ui.engine.focused);
    assert!(
        !ui.tools.focused,
        "focusing the engine releases the tools pane"
    );
    focus_panel(&mut ui);
    assert!(ui.tools.focused);
    assert!(!ui.engine.focused, "and the other way around");
}

/// While one editor is modal, the other's focus key is swallowed. It does
/// not leak into the composer behind the pane. Every modal surface on the
/// deck follows that rule.
#[test]
fn a_focused_editor_swallows_the_other_editors_focus_key() {
    let (model, mut ui) = open_ui();
    let action = handle_deck_key(ch('e'), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled);
    assert!(!ui.engine.focused, "e did not reach the engine panel");
    assert!(ui.tools.focused);
    assert!(ui.composer.is_empty(), "and nothing reached the composer");
}

#[test]
fn the_reason_names_the_key_and_the_scope_that_switched_it_off() {
    let mut off = tool("save_state", "scratch");
    off.off = Some(ToolDenial {
        key: "scratch".into(),
        scope: Some(ToolScope::Project),
    });
    assert_eq!(
        off_reason(&off).as_deref(),
        Some("\"scratch\" off in project settings")
    );
    assert_eq!(off_reason(&tool("get_state", "scratch")), None);
}

fn buffer_text(buf: &Buffer) -> String {
    let area = buf.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn drawn(ui: &DeckUi, w: u16, h: u16) -> String {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    render_panel(ui, area, &mut buf);
    buffer_text(&buf)
}

fn locked_ui() -> DeckUi {
    let (_model, mut ui) = open_ui();
    let mut state = sample_state();
    state.switches.insert("scratch".into(), false);
    for tool in &mut state.tools {
        if tool.group == "scratch" {
            tool.off = Some(ToolDenial {
                key: "scratch".into(),
                scope: Some(ToolScope::User),
            });
        }
        if tool.name == "delegate" {
            tool.locked = true;
            tool.off = Some(ToolDenial {
                key: "delegate".into(),
                scope: Some(ToolScope::Managed),
            });
        }
    }
    ui.tools.state = Some(state);
    ui
}

#[test]
fn render_smoke_draws_groups_states_and_reasons() {
    let ui = locked_ui();
    let text = drawn(&ui, 96, 24);
    assert!(text.contains("tools ·"), "header drawn");
    assert!(text.contains("SCRATCH"), "group headers drawn");
    assert!(text.contains("MCP"), "an MCP section is listed");
    assert!(text.contains("CUSTOM"), "a custom-tool section is listed");
    assert!(
        text.contains("deploy_to_staging"),
        "the customer's own tool"
    );
    assert!(
        text.contains("off in user settings"),
        "an off row explains itself"
    );
    assert!(text.contains("locked"), "an org-denied row says so");
    assert!(text.contains("org-locked"), "the header counts locked rows");
}

/// The state column holds under a wide-character tool name.
///
/// An MCP server names its own tools, so the name is not ASCII by
/// construction. The field was cut by `char` count and padded by
/// `{:<width$}`, which is also a `char` count. So a CJK name drew at twice
/// its width. It pushed `on`/`off` and all after it to the right, on that
/// row alone. Measured with ratatui's own `Span::width`, not with
/// the helper under test.
#[test]
fn a_wide_character_tool_name_does_not_move_the_state_column() {
    let (_model, ui) = open_ui();
    let mut state = sample_state();
    state
        .tools
        .push(tool("mcp__仓库__创建议题并附上一段很长的说明", "mcp"));
    let ascii = state
        .tools
        .iter()
        .position(|t| t.name == "mcp__gh__create_issue")
        .unwrap();
    let wide = state.tools.len() - 1;

    // The caret and the name field are what precede the state cell.
    let field = |i: usize| -> usize {
        let line = render_row(&ui.tools, &state, &ToolsRow::Tool(i), false, 96);
        line.spans[..2].iter().map(Span::width).sum()
    };
    assert_eq!(
        field(ascii),
        field(wide),
        "a wide name moved the state column"
    );
}

/// **The witness for the port.** The pane fills the body the frame carved
/// out. It draws no box of its own. The counts are a header row, and the
/// verbs are a key row. So no column or row goes to a border that repeats
/// what the tab strip already said.
#[test]
fn the_pane_draws_no_box_of_its_own() {
    let ui = locked_ui();
    let text = drawn(&ui, 96, 24);
    for edge in ['┌', '┐', '└', '┘', '│', '─', '╭', '╮', '╰', '╯'] {
        assert!(
            !text.contains(edge),
            "the pane drew a border glyph {edge:?}:\n{text}"
        );
    }
    let first = text.lines().next().unwrap_or_default();
    assert!(
        first.trim_start().starts_with("tools ·"),
        "the header is the first row, not a border: {first:?}"
    );
    assert!(
        first.contains("6 tools"),
        "the header carries the session's tool count: {first:?}"
    );
}

/// The keys are the pane's last row either way. They name what the
/// keyboard does right now. While the pane has focus, they are the verbs.
/// While it does not, they are the one key that gives it focus.
#[test]
fn the_key_row_follows_the_keyboard() {
    let mut ui = locked_ui();
    let focused = drawn(&ui, 96, 24);
    let last = focused.lines().last().unwrap_or_default().to_string();
    assert!(last.contains("⏎/space toggle"), "{last:?}");
    assert!(last.contains("esc done"), "{last:?}");

    ui.tools.focused = false;
    let browsing = drawn(&ui, 96, 24);
    let last = browsing.lines().last().unwrap_or_default().to_string();
    assert_eq!(last.trim_end(), " e edit tool switches");
}

/// A pane too short for its own bands still draws what fits rather than
/// panicking or painting outside the body.
#[test]
fn a_short_pane_still_renders() {
    let ui = locked_ui();
    for h in 1..=4u16 {
        let text = drawn(&ui, 96, h);
        assert_eq!(text.lines().count(), h as usize);
    }
    // Narrower than the caret plus a name is not a pane at all.
    assert_eq!(drawn(&ui, 3, 24).trim(), "");
}

#[test]
fn the_modified_marker_appears_only_with_unsaved_edits() {
    let mut ui = locked_ui();
    assert!(!drawn(&ui, 96, 24).contains("modified"));
    ui.tools.edits.insert("get_state".into(), false);
    assert!(drawn(&ui, 96, 24).contains("· modified"));
}
