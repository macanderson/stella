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
        SlashCommand::new("/profile", "retune every role"),
    ];
    handle_deck_key(ch('/'), &model, &mut ui);
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert_eq!(ui.slash_selected, 1);
    // Tab completes into the buffer while the popup is open (it does NOT
    // cycle tabs).
    handle_deck_key(key(KeyCode::Tab), &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "/profile");
    assert_eq!(ui.tab, DeckTab::Session, "tab did not cycle");
    // Enter dispatches the (still-matching) selection as a prompt.
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::Enqueue {
            text: "/profile".into()
        })
    );
}

#[test]
fn slash_models_opens_the_routing_card_instead_of_enqueueing() {
    // `/models` left the driver vocabulary's enqueue path: routing is a
    // deck-local card now (D3), so the selection must open it — not spend a
    // model turn listing what a card can show.
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.slash_commands = vec![SlashCommand::new("/models", "model routing")];
    for c in "/models".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled, "/models is consumed locally");
    assert_eq!(
        ui.cards.open,
        Some(crate::deck_ui::cards::Card::Models),
        "the routing card is up"
    );
    // Esc closes the topmost card before any other Esc meaning fires.
    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert!(!ui.cards.is_open());
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
            dropped_tools: 0,
            auth_fields: vec!["Authorization".into()],
            oauth: Some(false),
            calls: 5,
            ..Default::default()
        },
        McpServerInfo {
            name: "fs".into(),
            kind: "stdio".into(),
            enabled: true,
            connected: false,
            health: None,
            tool_count: 0,
            dropped_tools: 0,
            auth_fields: vec![],
            oauth: None,
            calls: 0,
            ..Default::default()
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
        dropped_tools: 0,
        auth_fields: vec![],
        oauth: Some(false),
        calls: 0,
        ..Default::default()
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
        dropped_tools: 0,
        auth_fields: vec![],
        oauth: Some(false),
        calls: 0,
        ..Default::default()
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

#[test]
fn a_bang_command_targets_the_lane_the_reader_is_looking_at() {
    // `deck_shell` resolves a `!` command's output lane with `focused_id`, so
    // the output lands in the transcript being rendered. With several agents
    // that must follow the focus, not default to the first row.
    let model = model_with(&["lead", "worker"]);
    let mut ui = ready_ui();
    assert_eq!(focused_id(&model, &ui).as_deref(), Some("lead"));
    ui.focus_agent(1);
    assert_eq!(
        focused_id(&model, &ui).as_deref(),
        Some("worker"),
        "`! pwd` answers in the transcript the user is reading"
    );
}

#[test]
fn a_bang_command_has_no_lane_to_borrow_before_any_agent_registers() {
    // The one case that still needs the synthetic shell lane — output must
    // not be dropped just because the session has not registered yet.
    let model = model_with(&[]);
    let ui = ready_ui();
    assert_eq!(focused_id(&model, &ui), None);
}

/// The ctrl+o inspector: opening it, its modality, and its scoped `r`.
#[test]
fn ctrl_o_opens_the_mcp_inspector_for_the_highlighted_server() {
    use crate::envelope::McpServerInfo;
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.set_tab(DeckTab::Mcp);
    ui.mcp.servers = vec![
        McpServerInfo {
            name: "github".into(),
            ..Default::default()
        },
        McpServerInfo {
            name: "fs".into(),
            ..Default::default()
        },
    ];
    ui.mcp.selected = 1;

    // Opening asks for the detail WITHOUT a registry lookup: merely looking at
    // a tab must not contact a third-party service.
    let action = handle_deck_key(ctrl('o'), &model, &mut ui);
    assert!(matches!(
        action,
        DeckAction::Send(WorkspaceInput::McpInspect { ref name, lookup: false }) if name == "fs"
    ));
    assert_eq!(ui.mcp.inspector.as_ref().unwrap().server, "fs");

    // Modal: a bare `x` must not reach the list and remove the server whose
    // detail is on screen.
    let action = handle_deck_key(key(KeyCode::Char('x')), &model, &mut ui);
    assert!(
        matches!(action, DeckAction::Handled),
        "x leaked: {action:?}"
    );

    // ↓ scrolls the overlay rather than moving the list selection.
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert_eq!(ui.mcp.inspector.as_ref().unwrap().scroll, 1);
    assert_eq!(
        ui.mcp.selected, 1,
        "the list selection moved under the overlay"
    );

    // Esc closes it, and only it.
    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert!(ui.mcp.inspector.is_none());
    assert_eq!(ui.tab, DeckTab::Mcp);
}

#[test]
fn the_inspector_offers_a_registry_lookup_only_when_it_could_answer_something() {
    use crate::envelope::{McpServerDetail, McpServerInfo};
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.set_tab(DeckTab::Mcp);
    ui.mcp.servers = vec![McpServerInfo {
        name: "mcp".into(),
        ..Default::default()
    }];
    handle_deck_key(ctrl('o'), &model, &mut ui);

    // No detail yet — `r` cannot know whether a lookup would help.
    assert!(matches!(
        handle_deck_key(key(KeyCode::Char('r')), &model, &mut ui),
        DeckAction::Handled
    ));

    // A server with no description: `r` asks the registry.
    ui.mcp.apply_detail(McpServerDetail {
        name: "mcp".into(),
        ..Default::default()
    });
    assert!(matches!(
        handle_deck_key(key(KeyCode::Char('r')), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::McpInspect { lookup: true, .. })
    ));

    // One that already has a description: `r` is inert, not a wasted call.
    ui.mcp.apply_detail(McpServerDetail {
        name: "mcp".into(),
        description: Some("Payments.".into()),
        ..Default::default()
    });
    assert!(matches!(
        handle_deck_key(key(KeyCode::Char('r')), &model, &mut ui),
        DeckAction::Handled
    ));
}

#[test]
fn ctrl_o_on_the_mcp_tab_does_not_toggle_transcript_expansion() {
    // Ctrl-O is the transcript expand/collapse chord everywhere else; the MCP
    // tab claims it, so the global handler must not run first and swallow it.
    use crate::envelope::McpServerInfo;
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.set_tab(DeckTab::Mcp);
    ui.mcp.servers = vec![McpServerInfo {
        name: "mcp".into(),
        ..Default::default()
    }];
    let before = ui.transcript_expand_all;
    handle_deck_key(ctrl('o'), &model, &mut ui);
    assert_eq!(ui.transcript_expand_all, before, "the global chord ran");
    assert!(ui.mcp.inspector.is_some());
}
