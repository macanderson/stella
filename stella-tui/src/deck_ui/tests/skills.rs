//! SKILLS tab: browse, search, preview, and the install prompt.

use super::*;

// ── SKILLS tab ──────────────────────────────────────────────────────────

// `SkillOp`, `SkillScope`, `SkillSearchHit`, `SkillsView` arrive via
// `use super::*`; only `SkillRow` is not imported at module scope.
use crate::envelope::SkillRow;

fn skills_ui() -> DeckUi {
    let mut ui = ready_ui();
    ui.tab = DeckTab::Skills;
    ui
}

fn a_row(name: &str, scope: SkillScope, enabled: bool) -> SkillRow {
    SkillRow {
        scope,
        name: name.to_string(),
        description: "d".to_string(),
        body: "b".to_string(),
        origin: "workspace".to_string(),
        enabled,
        version: 1,
        latest: 1,
        removable: true,
    }
}

#[test]
fn skills_search_pane_types_a_query_and_dispatches_a_search() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.focus = SkillsFocus::Search;
    for c in "pdf tools".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert_eq!(ui.skills.query, "pdf tools", "typed into the query");
    assert!(
        ui.composer.is_empty(),
        "the global composer never saw the keys"
    );
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::Skill(SkillOp::Search {
            query: "pdf tools".into()
        }))
    );
}

#[test]
fn skills_enter_on_a_hit_opens_the_scope_prompt_then_installs_scoped() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.focus = SkillsFocus::Search;
    ui.skills.hits = vec![SkillSearchHit {
        id: "acme/auth".into(),
        installs: "1.2K installs".into(),
        installs_rank: 1200,
        url: "https://skills.sh/acme/auth".into(),
    }];
    ui.skills.query = "auth".into();
    ui.skills.query_dirty = false;

    let a = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(a, DeckAction::Handled);
    assert!(matches!(
        ui.skills.prompt,
        Some(SkillPrompt::Scope {
            action: ScopeAction::Install { .. },
            ..
        })
    ));
    handle_deck_key(ch('u'), &model, &mut ui);
    let a = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        a,
        DeckAction::Send(WorkspaceInput::Skill(SkillOp::Install {
            scope: SkillScope::User,
            id: "acme/auth".into()
        }))
    );
    assert!(ui.skills.prompt.is_none());
}

#[test]
fn skills_ctrl_o_on_installed_opens_preview_with_local_body() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    let mut r = a_row("sql-style", SkillScope::Project, true);
    r.body = "# SQL Style\n\nUse lowercase keywords.".to_string();
    ui.skills.view = SkillsView {
        rows: vec![r],
        status: None,
        busy: false,
    };
    // ctrl+o must NOT toggle chain-of-thought on the SKILLS tab — it opens
    // the preview, with the body on hand (no driver round-trip).
    let a = handle_deck_key(ctrl('o'), &model, &mut ui);
    assert_eq!(a, DeckAction::Handled);
    let preview = ui.skills.preview.as_ref().expect("preview opened");
    assert_eq!(preview.pending, None, "installed body is local");
    assert!(
        preview.body.as_deref().unwrap().contains("SQL Style"),
        "body carried from the row"
    );
}

#[test]
fn skills_ctrl_o_on_search_hit_requests_preview_and_shows_loading() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.focus = SkillsFocus::Search;
    ui.skills.hits = vec![SkillSearchHit {
        id: "acme/auth@oauth".into(),
        installs: "1.2K installs".into(),
        installs_rank: 1200,
        url: "https://skills.sh/acme/auth/oauth".into(),
    }];
    let a = handle_deck_key(ctrl('o'), &model, &mut ui);
    assert_eq!(
        a,
        DeckAction::Send(WorkspaceInput::Skill(SkillOp::Preview {
            id: "acme/auth@oauth".into()
        }))
    );
    let preview = ui.skills.preview.as_ref().expect("preview opened");
    assert_eq!(preview.pending.as_deref(), Some("acme/auth@oauth"));
    assert_eq!(preview.body, None, "loading until the driver replies");
}

#[test]
fn skills_preview_ingest_fills_matching_id_ignores_stale_and_esc_closes() {
    let mut model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.preview = Some(SkillPreview {
        title: "acme/auth@oauth".into(),
        subtitle: String::new(),
        pending: Some("acme/auth@oauth".into()),
        body: None,
        scroll: 0,
    });
    // A reply for a DIFFERENT hit is dropped (stale / re-targeted).
    ingest_inbound(
        &Inbound::SkillPreview {
            id: "other/skill@x".into(),
            body: "wrong".into(),
            status: None,
        },
        &mut model,
        &mut ui,
    );
    assert_eq!(ui.skills.preview.as_ref().unwrap().body, None, "stale drop");
    // The matching reply fills the body and clears the pending marker.
    ingest_inbound(
        &Inbound::SkillPreview {
            id: "acme/auth@oauth".into(),
            body: "# OAuth\n\nbody".into(),
            status: None,
        },
        &mut model,
        &mut ui,
    );
    let preview = ui.skills.preview.as_ref().unwrap();
    assert!(preview.body.as_deref().unwrap().contains("OAuth"));
    assert_eq!(preview.pending, None);
    // Esc closes the overlay.
    let a = handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert_eq!(a, DeckAction::Handled);
    assert!(ui.skills.preview.is_none(), "esc closes the preview");
}

#[test]
fn skills_preview_scroll_keys_move_the_offset() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.preview = Some(SkillPreview {
        title: "x".into(),
        subtitle: String::new(),
        pending: None,
        body: Some("a\nb\nc".into()),
        scroll: 0,
    });
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert_eq!(ui.skills.preview.as_ref().unwrap().scroll, 1);
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    assert_eq!(ui.skills.preview.as_ref().unwrap().scroll, 0);
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    assert_eq!(
        ui.skills.preview.as_ref().unwrap().scroll,
        0,
        "clamped at 0"
    );
}

#[test]
fn skills_space_toggles_and_two_ctrl_x_uninstalls() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.view = SkillsView {
        rows: vec![a_row("sql-style", SkillScope::Project, true)],
        status: None,
        busy: false,
    };
    let a = handle_deck_key(ch(' '), &model, &mut ui);
    assert_eq!(
        a,
        DeckAction::Send(WorkspaceInput::Skill(SkillOp::SetEnabled {
            scope: SkillScope::Project,
            name: "sql-style".into(),
            enabled: false
        }))
    );
    assert!(!ui.skills.view.rows[0].enabled, "optimistic flip");

    let a1 = handle_deck_key(ctrl('x'), &model, &mut ui);
    assert_eq!(a1, DeckAction::Handled);
    assert!(ui.skills.uninstall_armed);
    let a2 = handle_deck_key(ctrl('x'), &model, &mut ui);
    assert_eq!(
        a2,
        DeckAction::Send(WorkspaceInput::Skill(SkillOp::Uninstall {
            scope: SkillScope::Project,
            name: "sql-style".into()
        }))
    );
}

#[test]
fn skills_e_opens_edit_overlay_and_ctrl_s_saves() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    let mut r = a_row("sql-style", SkillScope::Project, true);
    r.body = "old body".into();
    ui.skills.view = SkillsView {
        rows: vec![r],
        status: None,
        busy: false,
    };
    handle_deck_key(ch('e'), &model, &mut ui);
    assert!(matches!(
        ui.skills.prompt,
        Some(SkillPrompt::Edit { ref buffer, .. }) if buffer == "old body"
    ));
    for c in " +more".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let a = handle_deck_key(ctrl('s'), &model, &mut ui);
    assert_eq!(
        a,
        DeckAction::Send(WorkspaceInput::Skill(SkillOp::Edit {
            scope: SkillScope::Project,
            name: "sql-style".into(),
            body: "old body +more".into(),
        }))
    );
}

#[test]
fn skills_manage_hotkeys_yield_to_a_nonempty_composer() {
    // THE skills-key P1: the installed-pane manage hotkeys (space/e/p/n)
    // were claimed unconditionally, so typing a prompt on the SKILLS tab was
    // hijacked — 'n' opened the create flow, 'e' the edit overlay, space
    // toggled a skill. They must honor the deck-wide "hotkeys only from an
    // empty composer" contract and, mid-prompt, build the prompt instead.
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    let r = a_row("sql-style", SkillScope::Project, true);
    ui.skills.view = SkillsView {
        rows: vec![r],
        status: None,
        busy: false,
    };

    // 'r' is not a hotkey → it falls through to the composer, so the
    // composer is now non-empty.
    handle_deck_key(ch('r'), &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "r");

    // Now the manage-hotkey characters type into the composer, not fire.
    for c in "enp e".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert!(
        ui.skills.prompt.is_none(),
        "no manage overlay may open while a prompt is being typed"
    );
    assert!(
        ui.skills.view.rows[0].enabled,
        "space must not toggle the skill mid-prompt"
    );
    assert_eq!(ui.composer.buffer(), "renp e");

    // From an EMPTY composer, 'e' still opens the edit overlay as designed —
    // the gate only defers to a prompt in progress, it doesn't disable the
    // hotkeys.
    ui.composer.clear();
    handle_deck_key(ch('e'), &model, &mut ui);
    assert!(matches!(ui.skills.prompt, Some(SkillPrompt::Edit { .. })));
}

#[test]
fn skills_p_opens_pin_picker_and_enter_pins() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    let mut r = a_row("s", SkillScope::Project, true);
    r.version = 3;
    r.latest = 3;
    ui.skills.view = SkillsView {
        rows: vec![r],
        status: None,
        busy: false,
    };
    handle_deck_key(ch('p'), &model, &mut ui);
    assert!(matches!(
        ui.skills.prompt,
        Some(SkillPrompt::Pin {
            sel: 3,
            latest: 3,
            ..
        })
    ));
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    let a = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        a,
        DeckAction::Send(WorkspaceInput::Skill(SkillOp::Pin {
            scope: SkillScope::Project,
            name: "s".into(),
            version: 1,
        }))
    );
}

#[test]
fn skills_n_creates_via_description_then_scope() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    handle_deck_key(ch('n'), &model, &mut ui);
    assert!(matches!(
        ui.skills.prompt,
        Some(SkillPrompt::CreateDescription { .. })
    ));
    for c in "extract tables from pdfs".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let a = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(a, DeckAction::Handled);
    assert!(matches!(
        ui.skills.prompt,
        Some(SkillPrompt::Scope {
            action: ScopeAction::Create { .. },
            ..
        })
    ));
    let a = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        a,
        DeckAction::Send(WorkspaceInput::Skill(SkillOp::Create {
            scope: SkillScope::Project,
            description: "extract tables from pdfs".into(),
        }))
    );
}

#[test]
fn skills_snapshot_ingest_updates_view_and_clears_searching() {
    let mut model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.searching = true;
    let view = SkillsView {
        rows: vec![a_row("a", SkillScope::Project, true)],
        status: Some("done".into()),
        busy: false,
    };
    ingest_inbound(&Inbound::Skills(view), &mut model, &mut ui);
    assert_eq!(ui.skills.view.rows.len(), 1);
    assert!(!ui.skills.searching, "a fresh list clears the spinner");
    assert_eq!(ui.skills.status.as_deref(), Some("done"));
    assert!(
        model.agents.is_empty(),
        "model fold ignores skills snapshots"
    );
}

#[test]
fn skills_tab_still_leaves_via_tab_key() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    handle_deck_key(key(KeyCode::Tab), &model, &mut ui);
    // The MCP tab now sits after SKILLS in the cycle, so Tab leaves SKILLS
    // for MCP (still proving SKILLS is not a dead end).
    assert_eq!(ui.tab, DeckTab::Mcp, "Tab cycles Skills → Mcp");
}
