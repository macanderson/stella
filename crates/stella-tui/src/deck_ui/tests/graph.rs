//! Graph file picker and the installed-agents browser.

use super::*;

// ── Graph file picker ────────────────────────────────────────────────────

/// A three-file graph rooted on the busiest (`src/b.rs`), on the Graph tab.
fn ui_with_graph() -> DeckUi {
    use crate::graph::{GraphNode, GraphSnapshot};
    let mut ui = ready_ui();
    ui.tab = DeckTab::Graph;
    ui.graph = Some(GraphSnapshot {
        focus: "src/b.rs".into(),
        nodes: vec![GraphNode {
            label: "src/b.rs".into(),
            kind: "file".into(),
            location: Some("src/b.rs".into()),
        }],
        edges: vec![],
        files: vec!["src/a.rs".into(), "src/b.rs".into(), "src/c.rs".into()],
    });
    ui
}

// AGENTS tab: INSTALLED AGENTS pane

fn installed_entry(name: &str, version: u32) -> InstalledAgentEntry {
    InstalledAgentEntry {
        name: name.into(),
        description: format!("about {name}"),
        tools: Some(vec!["Read".into()]),
        scope: AgentScope::Project,
        source_path: format!("/ws/.stella/agents/{name}.md"),
        version,
        versions: (1..=version)
            .map(|v| crate::envelope::AgentVersionInfo {
                version: v,
                label: String::new(),
            })
            .collect(),
        content: format!("---\nname: {name}\n---\nbody of {name}"),
    }
}

/// A ready deck on the AGENTS tab's INSTALLED pane with `entries` loaded.
fn installed_ui(entries: Vec<InstalledAgentEntry>) -> DeckUi {
    let mut ui = ready_ui();
    ui.tab = DeckTab::Agents;
    ui.agents_pane = AgentsPane::Installed;
    ui.installed.entries = entries;
    ui.installed.loaded = true;
    ui
}

#[test]
fn slash_opens_the_picker_defaulting_to_the_current_focus() {
    let model = model_with(&["lead"]);
    let mut ui = ui_with_graph();
    let action = handle_deck_key(ch('/'), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled);
    assert!(ui.graph_picker_open, "/ opens the picker on the Graph tab");
    // Default selection is the busiest/focused file (index 1 = src/b.rs),
    // the sensible default — not forced there, just pre-selected.
    assert_eq!(ui.graph_picker_sel, 1);
    assert!(
        ui.composer.buffer().is_empty(),
        "/ did not leak into the prompt"
    );
}

#[test]
fn agents_pane_arrows_switch_and_first_visit_asks_for_the_list() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.tab = DeckTab::Agents;
    assert_eq!(ui.agents_pane, AgentsPane::Executions, "executions first");
    // → switches to INSTALLED AGENTS; the unloaded list triggers one
    // refresh request.
    let action = handle_deck_key(key(KeyCode::Right), &model, &mut ui);
    assert_eq!(ui.agents_pane, AgentsPane::Installed);
    assert_eq!(action, DeckAction::Send(WorkspaceInput::AgentsRefresh));
    // ← switches back; → again does NOT re-fetch (busy flag pending).
    handle_deck_key(key(KeyCode::Left), &model, &mut ui);
    assert_eq!(ui.agents_pane, AgentsPane::Executions);
    assert_eq!(
        handle_deck_key(key(KeyCode::Right), &model, &mut ui),
        DeckAction::Handled,
        "no duplicate refresh while one is in flight"
    );
}

#[test]
fn enter_also_opens_the_picker() {
    let model = model_with(&["lead"]);
    let mut ui = ui_with_graph();
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert!(ui.graph_picker_open);
}

#[test]
fn typing_filters_and_re_anchors_the_selection() {
    let model = model_with(&["lead"]);
    let mut ui = ui_with_graph();
    open_graph_picker(&mut ui);
    // Filter to just "a.rs" — one match, selection re-anchors to 0.
    handle_deck_key(ch('a'), &model, &mut ui);
    assert_eq!(ui.graph_picker_query, "a");
    assert_eq!(ui.graph_picker_sel, 0);
    let matches = ui
        .graph
        .as_ref()
        .unwrap()
        .matching_files(&ui.graph_picker_query);
    assert_eq!(matches, vec!["src/a.rs"]);
}

#[test]
fn enter_in_the_picker_re_roots_on_the_selected_file() {
    let model = model_with(&["lead"]);
    let mut ui = ui_with_graph();
    open_graph_picker(&mut ui);
    // A multi-char needle (`c.rs`) narrows to exactly src/c.rs — a bare
    // `c` would also match the shared `src/` prefix of every file.
    for c in "c.rs".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::FocusGraphFile {
            file: "src/c.rs".into()
        }),
        "Enter sends the re-root request for the filtered selection"
    );
    assert!(!ui.graph_picker_open, "the picker closes on selection");
}

#[test]
fn down_arrow_walks_the_filtered_matches_and_clamps() {
    let model = model_with(&["lead"]);
    let mut ui = ui_with_graph();
    open_graph_picker(&mut ui);
    ui.graph_picker_sel = 0;
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    handle_deck_key(key(KeyCode::Down), &model, &mut ui); // past the end
    assert_eq!(ui.graph_picker_sel, 2, "clamps to the last of three files");
}

#[test]
fn esc_closes_the_picker_without_re_rooting() {
    let model = model_with(&["lead"]);
    let mut ui = ui_with_graph();
    open_graph_picker(&mut ui);
    let action = handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled);
    assert!(!ui.graph_picker_open);
}

#[test]
fn the_picker_is_modal_over_the_composer() {
    // A printable key while the picker is open filters — it must NOT type
    // into the global composer (the queue-editor modality contract).
    let model = model_with(&["lead"]);
    let mut ui = ui_with_graph();
    open_graph_picker(&mut ui);
    handle_deck_key(ch('b'), &model, &mut ui);
    assert_eq!(ui.graph_picker_query, "b");
    assert!(
        ui.composer.buffer().is_empty(),
        "filter keys never reach the composer"
    );
}

#[test]
fn a_re_rooted_snapshot_resets_the_node_cursor() {
    let mut model = model_with(&["lead"]);
    let mut ui = ui_with_graph();
    ui.graph_cursor = 5; // stale cursor from the previous neighborhood
    use crate::graph::{GraphNode, GraphSnapshot};
    let rerooted = GraphSnapshot {
        focus: "src/a.rs".into(),
        nodes: vec![GraphNode {
            label: "src/a.rs".into(),
            kind: "file".into(),
            location: Some("src/a.rs".into()),
        }],
        edges: vec![],
        files: vec!["src/a.rs".into(), "src/b.rs".into(), "src/c.rs".into()],
    };
    ingest_inbound(&Inbound::GraphSnapshot(rerooted), &mut model, &mut ui);
    assert_eq!(ui.graph_cursor, 0, "the cursor lands on the new focus");
}

#[test]
fn the_picker_does_not_open_without_a_loaded_graph() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.tab = DeckTab::Graph; // no snapshot loaded
    handle_deck_key(ch('/'), &model, &mut ui);
    assert!(!ui.graph_picker_open, "nothing to pick from — stays closed");
}

#[test]
fn slash_agents_opens_the_tab_on_the_installed_pane() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.slash_commands = vec![SlashCommand::new("/agents", "agents")];
    for c in "/agents".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(ui.tab, DeckTab::Agents, "/agents opens the Agents tab");
    assert_eq!(ui.agents_pane, AgentsPane::Installed);
    assert_eq!(action, DeckAction::Send(WorkspaceInput::AgentsRefresh));
    assert!(ui.composer.buffer().is_empty(), "the composer cleared");
}

#[test]
fn agents_list_ingest_updates_the_panel_out_of_band_and_clamps() {
    let mut model = model_with(&["lead"]);
    let mut ui = installed_ui(vec![]);
    ui.installed.sel = 5;
    ui.installed.busy = true;
    ingest_inbound(
        &Inbound::AgentsList {
            entries: vec![installed_entry("reviewer", 1)],
            status: Some("saved".into()),
            creating: false,
            created: None,
        },
        &mut model,
        &mut ui,
    );
    assert_eq!(ui.installed.entries.len(), 1);
    assert_eq!(ui.installed.sel, 0, "selection clamped to the new list");
    assert!(!ui.installed.busy, "a fresh list completes the op");
    assert_eq!(ui.installed.status.as_deref(), Some("saved"));
    assert_eq!(
        model.agents.len(),
        1,
        "the model fold ignores the out-of-band list"
    );
}

#[test]
fn installed_enter_opens_the_editor_and_ctrl_s_saves_a_new_version() {
    let model = model_with(&["lead"]);
    let mut ui = installed_ui(vec![installed_entry("reviewer", 2)]);
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(ui.installed.mode, InstalledMode::Edit);
    assert_eq!(
        ui.installed.editor.buffer(),
        "---\nname: reviewer\n---\nbody of reviewer",
        "the editor holds the pinned version's content"
    );
    // Type at the end (the cursor loads at the end of the buffer), with
    // a plain Enter inserting a newline — never submitting.
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    for c in "x".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(ctrl('s'), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::AgentSave {
            name: "reviewer".into(),
            scope: AgentScope::Project,
            content: "---\nname: reviewer\n---\nbody of reviewer\nx".into(),
        }),
        "ctrl+s sends the edited content — the driver writes a NEW pinned version"
    );
    assert_eq!(ui.installed.mode, InstalledMode::Browse);
    assert!(ui.installed.busy, "save shows the working state");
}

#[test]
fn editor_esc_discards_without_sending_and_typing_never_leaks() {
    let model = model_with(&["lead"]);
    let mut ui = installed_ui(vec![installed_entry("reviewer", 1)]);
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    for c in "abc".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert!(
        ui.composer.buffer().is_empty(),
        "editor typing never reaches the global composer"
    );
    let action = handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled, "no save is sent");
    assert_eq!(ui.installed.mode, InstalledMode::Browse);
    assert!(
        ui.installed
            .status
            .as_deref()
            .is_some_and(|s| s.contains("discarded")),
        "{:?}",
        ui.installed.status
    );
}

#[test]
fn create_flow_describes_picks_scope_and_dispatches_the_llm_draft() {
    let model = model_with(&["lead"]);
    let mut ui = installed_ui(vec![]);
    handle_deck_key(ch('n'), &model, &mut ui);
    assert_eq!(ui.installed.mode, InstalledMode::CreateDescribe);
    for c in "reviews diffs".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert_eq!(ui.installed.create_desc, "reviews diffs");
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        ui.installed.mode,
        InstalledMode::CreateScope,
        "⏎ advances to the scope picker (mirrors the skills install flow)"
    );
    // Default scope is project; ↓ flips to user, ↑ flips back.
    assert_eq!(ui.installed.create_scope(), AgentScope::Project);
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert_eq!(ui.installed.create_scope(), AgentScope::User);
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::AgentCreate {
            description: "reviews diffs".into(),
            scope: AgentScope::Project,
        })
    );
    assert_eq!(
        ui.installed.mode,
        InstalledMode::Creating,
        "the dialog stays open with the in-flight spinner"
    );
    assert!(ui.installed.busy);
}

#[test]
fn agent_creating_dialog_survives_a_queued_snapshot_and_ends_in_the_detail_view() {
    let mut model = model_with(&["lead"]);
    let mut ui = installed_ui(vec![]);
    ui.installed.mode = InstalledMode::Creating;
    ui.installed.busy = true;

    // Fully modal: printable keys and ⏎ are swallowed.
    for k in [ch('n'), key(KeyCode::Enter)] {
        assert_eq!(handle_deck_key(k, &model, &mut ui), DeckAction::Handled);
    }
    assert_eq!(ui.installed.mode, InstalledMode::Creating);
    assert_eq!(ui.composer.buffer(), "");

    // A parked create's interim snapshot (`creating: true`) must NOT read as
    // completion — the dialog and its spinner stay up.
    ingest_inbound(
        &Inbound::AgentsList {
            entries: vec![],
            status: Some("agent creation queued — it runs when the current turn finishes".into()),
            creating: true,
            created: None,
        },
        &mut model,
        &mut ui,
    );
    assert_eq!(
        ui.installed.mode,
        InstalledMode::Creating,
        "a queued interim snapshot keeps the spinner up"
    );
    assert!(ui.installed.busy);

    // The settled snapshot naming the created agent transitions the dialog
    // into the detail view of exactly that entry (the ctrl+o treatment).
    ingest_inbound(
        &Inbound::AgentsList {
            entries: vec![installed_entry("older", 1), installed_entry("drafted", 1)],
            status: Some("created drafted (project scope) — v1 pinned".into()),
            creating: false,
            created: Some("drafted".into()),
        },
        &mut model,
        &mut ui,
    );
    assert_eq!(
        ui.installed.mode,
        InstalledMode::CreateDone,
        "the dialog shows the created agent, not Browse"
    );
    assert_eq!(ui.installed.created_name.as_deref(), Some("drafted"));
    assert!(ui.installed.create_error.is_none());
    assert_eq!(ui.installed.sel, 1, "the list lands on the new agent");
    assert!(!ui.installed.busy);

    // ⏎ acknowledges and returns to Browse.
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(ui.installed.mode, InstalledMode::Browse);
    assert!(ui.installed.created_name.is_none());
}

#[test]
fn agent_create_done_view_scrolls_like_the_skills_preview() {
    let model = model_with(&["lead"]);
    let mut ui = installed_ui(vec![installed_entry("drafted", 1)]);
    ui.installed.mode = InstalledMode::CreateDone;
    ui.installed.created_name = Some("drafted".into());

    // ↓ / PageDown advance the offset (render clamps it to content);
    // ↑ / Home walk it back — the same verbs as the ctrl+o skill preview.
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert_eq!(ui.installed.created_scroll, 2);
    handle_deck_key(key(KeyCode::PageDown), &model, &mut ui);
    assert_eq!(ui.installed.created_scroll, 12);
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    assert_eq!(ui.installed.created_scroll, 11);
    handle_deck_key(key(KeyCode::Home), &model, &mut ui);
    assert_eq!(ui.installed.created_scroll, 0);
    // The view is still up throughout — scrolling never closes it.
    assert_eq!(ui.installed.mode, InstalledMode::CreateDone);
    // `q` closes it and clears the transient state, like the preview.
    handle_deck_key(ch('q'), &model, &mut ui);
    assert_eq!(ui.installed.mode, InstalledMode::Browse);
    assert!(ui.installed.created_name.is_none());
    assert_eq!(ui.installed.created_scroll, 0);
}

#[test]
fn agent_failed_create_shows_the_error_in_the_dialog() {
    let mut model = model_with(&["lead"]);
    let mut ui = installed_ui(vec![]);
    ui.installed.mode = InstalledMode::Creating;
    ui.installed.busy = true;
    // A settled snapshot WITHOUT a created name is a failure — the dialog
    // shows the driver's status as the error instead of vanishing.
    ingest_inbound(
        &Inbound::AgentsList {
            entries: vec![],
            status: Some("agent creation failed: draft call failed: boom".into()),
            creating: false,
            created: None,
        },
        &mut model,
        &mut ui,
    );
    assert_eq!(ui.installed.mode, InstalledMode::CreateDone);
    assert!(
        ui.installed
            .create_error
            .as_deref()
            .is_some_and(|e| e.contains("draft call failed: boom")),
        "the failure stays in the dialog: {:?}",
        ui.installed.create_error
    );
    // Esc acknowledges and closes.
    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert_eq!(ui.installed.mode, InstalledMode::Browse);
    assert!(ui.installed.create_error.is_none());
}

#[test]
fn agent_creating_dialog_esc_hides_but_the_op_stays_busy() {
    let model = model_with(&["lead"]);
    let mut ui = installed_ui(vec![]);
    ui.installed.mode = InstalledMode::Creating;
    ui.installed.busy = true;
    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert_eq!(ui.installed.mode, InstalledMode::Browse);
    assert!(
        ui.installed.busy,
        "hiding the dialog does not cancel the driver-side create"
    );
}

#[test]
fn create_flow_requires_a_description_and_esc_steps_back() {
    let model = model_with(&["lead"]);
    let mut ui = installed_ui(vec![]);
    handle_deck_key(ch('n'), &model, &mut ui);
    // Empty description: ⏎ refuses to advance.
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(ui.installed.mode, InstalledMode::CreateDescribe);
    for c in "x".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(ui.installed.mode, InstalledMode::CreateScope);
    // Esc from the scope picker returns to the description, not Browse.
    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert_eq!(ui.installed.mode, InstalledMode::CreateDescribe);
    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert_eq!(ui.installed.mode, InstalledMode::Browse);
}

#[test]
fn version_picker_pins_an_older_version_without_editing() {
    let model = model_with(&["lead"]);
    let mut ui = installed_ui(vec![installed_entry("reviewer", 3)]);
    handle_deck_key(ch('v'), &model, &mut ui);
    assert_eq!(ui.installed.mode, InstalledMode::PickVersion);
    assert_eq!(
        ui.installed.version_sel, 2,
        "the picker opens on the pinned version"
    );
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::AgentPin {
            name: "reviewer".into(),
            scope: AgentScope::Project,
            version: 1,
        }),
        "⏎ re-pins — an AgentPin, never an AgentSave"
    );
    assert_eq!(ui.installed.mode, InstalledMode::Browse);
}

#[test]
fn version_picker_on_the_already_pinned_version_sends_nothing() {
    let model = model_with(&["lead"]);
    let mut ui = installed_ui(vec![installed_entry("reviewer", 2)]);
    handle_deck_key(ch('v'), &model, &mut ui);
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled, "re-pinning the pin is a no-op");
    assert!(
        ui.installed
            .status
            .as_deref()
            .is_some_and(|s| s.contains("already")),
        "{:?}",
        ui.installed.status
    );
}

#[test]
fn installed_browse_letter_verbs_type_when_the_composer_has_text() {
    let model = model_with(&["lead"]);
    let mut ui = installed_ui(vec![installed_entry("reviewer", 1)]);
    handle_deck_key(ch('h'), &model, &mut ui);
    handle_deck_key(ch('n'), &model, &mut ui);
    assert_eq!(
        ui.installed.mode,
        InstalledMode::Browse,
        "a typed `n` is prompt text, not the create verb"
    );
    assert_eq!(ui.composer.buffer(), "hn");
}
