//! Prompt queue editor + slash popup + MCP tab key handling.

use super::*;

#[test]
fn bang_prefix_runs_a_shell_command_immediately_never_enqueued() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    for c in "!cargo build".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(action, DeckAction::Shell("cargo build".into()));
}

#[test]
fn bang_prefix_only_strips_the_single_dispatch_marker() {
    // The command text itself starts with `!` (e.g. `!important`), so
    // the typed line is `!!important` — only the first `!` is the
    // dispatch marker; the second belongs to the command.
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    for c in "!!important".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(action, DeckAction::Shell("!important".into()));
}

#[test]
fn bang_prefix_beats_a_pending_ask_user_gate() {
    let mut model = model_with(&["lead"]);
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::AskUser {
            id: "q1".into(),
            question: "which db?".into(),
            options: vec!["sqlite".into()],
        },
    });
    let mut ui = ready_ui();
    for c in "!ls".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Shell("ls".into()),
        "a shell line is never swallowed as a gate answer"
    );
}

#[test]
fn ctrl_t_toggles_the_queue_editor() {
    let model = model_with_queue(&["one"]);
    let mut ui = ready_ui();
    assert!(!ui.queue_open);
    handle_deck_key(ctrl('t'), &model, &mut ui);
    assert!(ui.queue_open);
    handle_deck_key(ctrl('t'), &model, &mut ui);
    assert!(!ui.queue_open);
}

#[test]
fn up_arrow_on_session_opens_the_queue_editor_on_the_newest_prompt() {
    let model = model_with_queue(&["first", "second", "third"]);
    let mut ui = ready_ui();
    assert_eq!(ui.tab, DeckTab::Session);
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    assert!(ui.queue_open, "up from an empty composer opens the queue");
    assert_eq!(ui.queue_sel, 2, "newest prompt selected");
    // With no queue, Up falls back to transcript scrolling (no popup).
    let empty = model_with(&["lead"]);
    let mut ui2 = ready_ui();
    handle_deck_key(key(KeyCode::Up), &empty, &mut ui2);
    assert!(!ui2.queue_open);
}

#[test]
fn up_arrow_does_not_open_the_queue_editor_over_a_pasted_chip() {
    // The live buffer is empty, but a pasted chip is still attached —
    // editing a queued item would `Composer::load` and silently drop it,
    // so the composer must not read as "empty" here.
    let model = model_with_queue(&["first"]);
    let mut ui = ready_ui();
    // Force a chip regardless of the deck's (high) paste threshold — this
    // test is about the chip interaction, not where the threshold sits.
    ui.composer = crate::composer::Composer::with_paste_threshold(3);
    ui.composer
        .paste("line1\nline2\nline3\nline4\nline5\nline6");
    assert!(ui.composer.buffer().is_empty());
    assert!(
        !ui.composer.is_empty(),
        "the chip keeps the composer non-empty"
    );
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    assert!(
        !ui.queue_open,
        "opening the queue editor here would drop the pasted chip on next edit"
    );
}

#[test]
fn queue_editor_navigates_deletes_and_edits_as_a_list() {
    let model = model_with_queue(&["first", "second", "third"]);
    let mut ui = ready_ui();
    handle_deck_key(ctrl('t'), &model, &mut ui);
    // Navigate to the second prompt.
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert_eq!(ui.queue_sel, 1);
    // ctrl+x deletes exactly the selected prompt.
    assert_eq!(
        handle_deck_key(ctrl('x'), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::QueueRemove { index: 1 })
    );
    // Enter pulls the selected prompt back into the composer for editing.
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::QueueRemove { index: 1 })
    );
    assert_eq!(ui.composer.buffer(), "second", "prompt loaded for editing");
    assert!(!ui.queue_open, "editing returns to the composer");
}

#[test]
fn queue_clear_requires_two_ctrl_d_presses() {
    let model = model_with_queue(&["a", "b"]);
    let mut ui = ready_ui();
    handle_deck_key(ctrl('t'), &model, &mut ui);
    // First press only arms the confirm.
    assert_eq!(
        handle_deck_key(ctrl('d'), &model, &mut ui),
        DeckAction::Handled
    );
    assert!(ui.queue_confirm_clear);
    // Any other key disarms it.
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert!(!ui.queue_confirm_clear, "other keys disarm the confirm");
    // Two consecutive presses clear.
    handle_deck_key(ctrl('d'), &model, &mut ui);
    assert_eq!(
        handle_deck_key(ctrl('d'), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::QueueClear)
    );
    assert!(!ui.queue_open, "clearing closes the editor");
}

#[test]
fn ctrl_r_toggles_thinking_from_any_tab() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    assert!(!ui.thinking_expanded, "collapsed by default");
    handle_deck_key(ctrl('r'), &model, &mut ui);
    assert!(ui.thinking_expanded);
}

#[test]
fn deck_slash_popup_selects_completes_and_dispatches() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.slash_commands = vec![
        SlashCommand::new("/help", "show help"),
        SlashCommand::new("/models", "list models"),
    ];
    handle_deck_key(ch('/'), &model, &mut ui);
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert_eq!(ui.slash_selected, 1);
    // Tab completes into the buffer while the popup is open (it does NOT
    // cycle tabs).
    handle_deck_key(key(KeyCode::Tab), &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "/models");
    assert_eq!(ui.tab, DeckTab::Session, "tab did not cycle");
    // Enter dispatches the (still-matching) selection as a prompt.
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::Enqueue {
            text: "/models".into()
        })
    );
}

#[test]
fn slash_files_switches_to_files_and_closes_an_open_diff() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.slash_commands = vec![SlashCommand::new("/files", "files")];
    ui.files_diff_open = true; // a diff was left open from a prior view
    for c in "/files".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled, "/files is consumed locally");
    assert_eq!(ui.tab, DeckTab::Files);
    assert!(!ui.files_diff_open, "/files shows the tree, not a diff");
}

#[test]
fn slash_mcp_switches_to_the_mcp_tab() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.slash_commands = vec![SlashCommand::new("/mcp", "mcp")];
    for c in "/mcp".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled, "/mcp is consumed locally");
    assert_eq!(ui.tab, DeckTab::Mcp);
}

#[test]
fn slash_mcp_search_jumps_straight_into_registry_search() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.slash_commands = vec![
        SlashCommand::new("/mcp", "mcp"),
        SlashCommand::new("/mcp-search", "search the MCP registry"),
    ];
    for c in "/mcp-search".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled, "/mcp-search is deck-local");
    assert_eq!(ui.tab, DeckTab::Mcp);
    assert_eq!(ui.mcp.mode, crate::views::mcp::McpMode::Search);
}

#[test]
fn slash_on_the_mcp_tab_opens_the_command_menu_not_search() {
    // The old `/`-enters-search trigger collided with the command menu;
    // `/` must now behave on the MCP tab exactly as everywhere else —
    // it starts a slash query in the composer.
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.slash_commands = vec![SlashCommand::new("/mcp-search", "search")];
    ui.set_tab(DeckTab::Mcp);
    handle_deck_key(ch('/'), &model, &mut ui);
    assert_eq!(
        ui.mcp.mode,
        crate::views::mcp::McpMode::Browse,
        "`/` no longer enters MCP search"
    );
    assert_eq!(ui.composer.buffer(), "/", "the slash query is typing");
    assert!(
        !slash_matches(&ui).is_empty(),
        "…and the command menu is open over it"
    );
}

#[test]
fn mcp_tab_navigates_toggles_and_enters_search() {
    use crate::envelope::McpServerInfo;
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.set_tab(DeckTab::Mcp);
    ui.mcp.servers = vec![
        McpServerInfo {
            name: "github".into(),
            kind: "http".into(),
            enabled: true,
            connected: true,
            health: Some("live".into()),
            tool_count: 3,
            auth_fields: vec!["Authorization".into()],
            oauth: Some(false),
            calls: 5,
        },
        McpServerInfo {
            name: "fs".into(),
            kind: "stdio".into(),
            enabled: true,
            connected: false,
            health: None,
            tool_count: 0,
            auth_fields: vec![],
            oauth: None,
            calls: 0,
        },
    ];
    // ↓ moves the selection.
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert_eq!(ui.mcp.selected, 1);
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    assert_eq!(ui.mcp.selected, 0);

    // `e` toggles the selected server (session enable/disable).
    let action = handle_deck_key(ch('e'), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::McpToggle {
            name: "github".into()
        })
    );

    // `s` enters search mode; typing builds the query; Enter searches.
    // (`/` no longer does — it belongs to the command menu everywhere.)
    handle_deck_key(ch('s'), &model, &mut ui);
    assert_eq!(ui.mcp.mode, crate::views::mcp::McpMode::Search);
    for c in "git".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert_eq!(ui.mcp.query, "git");
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::McpSearch {
            query: "git".into()
        })
    );
    assert!(ui.mcp.searching);
    // Esc leaves search mode.
    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert_eq!(ui.mcp.mode, crate::views::mcp::McpMode::Browse);
}

#[test]
fn mcp_auth_prompt_captures_a_masked_value_as_a_redacted_secret() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.set_tab(DeckTab::Mcp);
    ui.mcp.servers = vec![crate::envelope::McpServerInfo {
        name: "github".into(),
        kind: "http".into(),
        enabled: true,
        connected: true,
        health: Some("live".into()),
        tool_count: 1,
        auth_fields: vec![],
        oauth: Some(false),
        calls: 0,
    }];
    // `a` enters auth mode.
    handle_deck_key(ch('a'), &model, &mut ui);
    assert_eq!(ui.mcp.mode, crate::views::mcp::McpMode::Auth);
    // Type the field name, Enter advances to the value step.
    for c in "TOKEN".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(ui.mcp.auth.step, crate::views::mcp::AuthStep::Value);
    for c in "sk-secret".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    match action {
        DeckAction::Send(WorkspaceInput::McpAuth {
            server,
            field,
            value,
        }) => {
            assert_eq!(server, "github");
            assert_eq!(field, "TOKEN");
            assert_eq!(value.reveal(), "sk-secret");
            // The secret never appears under Debug.
            assert!(!format!("{value:?}").contains("sk-secret"));
        }
        other => panic!("expected McpAuth, got {other:?}"),
    }
    assert_eq!(ui.mcp.mode, crate::views::mcp::McpMode::Browse);
}

#[test]
fn a_pasted_secret_lands_in_the_credential_value_never_the_composer() {
    // THE paste-routing security P1: a bracketed paste while the MCP auth
    // VALUE step is focused used to fall through to the global composer, so
    // a pasted API token landed — in plaintext — in the prompt buffer (and
    // could be sent, or shown in the transcript). It must route to the
    // credential value input instead.
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.set_tab(DeckTab::Mcp);
    ui.mcp.servers = vec![crate::envelope::McpServerInfo {
        name: "github".into(),
        kind: "http".into(),
        enabled: true,
        connected: true,
        health: Some("live".into()),
        tool_count: 1,
        auth_fields: vec![],
        oauth: Some(false),
        calls: 0,
    }];
    handle_deck_key(ch('a'), &model, &mut ui); // enter auth
    for c in "TOKEN".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui); // → Value step
    assert_eq!(ui.mcp.auth.step, crate::views::mcp::AuthStep::Value);

    // Paste a multi-line secret blob.
    ui.paste("sk-pasted-token\nextra");

    // It went into the credential value (newlines dropped — a one-line
    // field), and did NOT leak into the global composer.
    assert_eq!(ui.mcp.auth.value, "sk-pasted-tokenextra");
    assert!(
        ui.composer.buffer().is_empty(),
        "a pasted secret must never reach the composer: {:?}",
        ui.composer.buffer()
    );
}

#[test]
fn a_paste_in_skills_search_types_into_the_query_not_the_composer() {
    // The SKILLS tab is keyboard-owning: a paste in its search pane must
    // build the query, exactly like typed characters do, never the composer.
    // (paste() is a direct DeckUi method, so no WorkspaceModel is needed.)
    let mut ui = ready_ui();
    ui.set_tab(DeckTab::Skills);
    ui.skills.focus = SkillsFocus::Search;

    ui.paste("postgres\nmigrations");

    assert_eq!(ui.skills.query, "postgresmigrations");
    assert!(
        ui.composer.buffer().is_empty(),
        "a skills-tab paste must not reach the composer: {:?}",
        ui.composer.buffer()
    );
}

#[test]
fn slash_diff_switches_to_files_and_opens_the_diff() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.slash_commands = vec![SlashCommand::new("/diff", "diff")];
    for c in "/diff".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(ui.tab, DeckTab::Files);
    assert!(ui.files_diff_open, "/diff opens the viewer");
}

#[test]
fn a_refreshed_graph_snapshot_updates_the_view_out_of_band() {
    use crate::graph::{GraphNode, GraphSnapshot};
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    assert!(ui.graph.is_none());
    let snapshot = GraphSnapshot {
        focus: "src/lib.rs".into(),
        nodes: vec![GraphNode {
            label: "src/lib.rs".into(),
            kind: "file".into(),
            location: Some("src/lib.rs".into()),
        }],
        edges: vec![],
        files: vec!["src/lib.rs".into()],
    };
    ingest_inbound(
        &Inbound::GraphSnapshot(snapshot.clone()),
        &mut model,
        &mut ui,
    );
    assert_eq!(ui.graph.as_ref(), Some(&snapshot));
}
