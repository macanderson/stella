//! SKILLS tab: browse, search, preview, and the install prompt.

use super::*;

// ── SKILLS tab ──────────────────────────────────────────────────────────

// `SkillOp` and `SkillPrompt` arrive via `use super::*`. The rest of the
// SKILLS envelope types are imported here directly, since `deck_ui.rs`
// itself has none of its own use for them — `SkillsPanel` and its kin live
// in `skills_state`.
use crate::envelope::{RejectedSkillRow, SkillRow, SkillScope, SkillSearchHit, SkillsView};

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
        evidence_grade: None,
        learned: None,
        enabled,
        version: 1,
        latest: 1,
        removable: true,
        contributed_by: None,
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
        rejections: vec![],
        busy: false,
        created: None,
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
        rejections: vec![],
        busy: false,
        created: None,
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
        rejections: vec![],
        busy: false,
        created: None,
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
        rejections: vec![],
        busy: false,
        created: None,
    };

    // 'z' is not a hotkey → it falls through to the composer, so the
    // composer is now non-empty. (This seed used to be 'r', which #5046
    // promoted to the learned-skill rename; it only has to be a character the
    // tab does not claim.)
    handle_deck_key(ch('z'), &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "z");

    // Now the manage-hotkey characters type into the composer, not fire —
    // including #5046's `r` and `x`.
    for c in "enprx e".chars() {
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
    assert!(
        !ui.skills.reject_armed,
        "and `x` must not arm a rejection mid-prompt"
    );
    assert_eq!(ui.composer.buffer(), "zenprx e");

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
        rejections: vec![],
        busy: false,
        created: None,
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
    // The dialog does NOT close on dispatch: the creating state keeps an
    // animated spinner up until the refreshed skills snapshot folds in.
    assert!(
        matches!(ui.skills.prompt, Some(SkillPrompt::Creating { .. })),
        "dispatch keeps the dialog open in the creating state: {:?}",
        ui.skills.prompt
    );
}

#[test]
fn skills_creating_dialog_becomes_the_preview_of_the_created_skill() {
    let mut model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.prompt = Some(SkillPrompt::Creating {
        description: "extract tables from pdfs".into(),
        scope: SkillScope::Project,
    });
    ui.skills.searching = true;

    // Fully modal: printable keys and ⏎ are swallowed — nothing re-dispatches
    // and nothing leaks into the composer.
    for k in [ch('x'), key(KeyCode::Enter)] {
        assert_eq!(handle_deck_key(k, &model, &mut ui), DeckAction::Handled);
    }
    assert!(matches!(
        ui.skills.prompt,
        Some(SkillPrompt::Creating { .. })
    ));
    assert_eq!(ui.composer.buffer(), "");

    // The refreshed snapshot naming the created skill is the completion
    // signal: the dialog becomes the ctrl+o preview of exactly that skill.
    let mut created = a_row("pdf-tables", SkillScope::Project, true);
    created.body = "# PDF Tables\nextract tables".into();
    let view = SkillsView {
        rows: vec![a_row("other", SkillScope::User, true), created],
        status: Some("created pdf-tables (project) — v1".into()),
        rejections: vec![],
        busy: false,
        created: Some("pdf-tables".into()),
    };
    ingest_inbound(&Inbound::Skills(view), &mut model, &mut ui);
    assert!(ui.skills.prompt.is_none(), "the create dialog is done");
    assert!(!ui.skills.searching);
    let preview = ui.skills.preview.as_ref().expect("the preview opened");
    assert_eq!(preview.title, "pdf-tables");
    assert_eq!(
        preview.body.as_deref(),
        Some("# PDF Tables\nextract tables"),
        "the created skill's body shows — same as ctrl+o on the row"
    );
    assert_eq!(ui.skills.sel, 1, "the list lands on the new skill");
}

#[test]
fn skills_failed_create_shows_the_error_in_the_dialog() {
    let mut model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.prompt = Some(SkillPrompt::Creating {
        description: "d".into(),
        scope: SkillScope::Project,
    });
    ui.skills.searching = true;
    // A completion snapshot WITHOUT a created name is a failure: the dialog
    // shows the driver's status as the error instead of vanishing.
    let view = SkillsView {
        rows: vec![],
        status: Some("the model did not return a valid SKILL.md — try again".into()),
        rejections: vec![],
        busy: false,
        created: None,
    };
    ingest_inbound(&Inbound::Skills(view), &mut model, &mut ui);
    assert!(
        matches!(
            ui.skills.prompt,
            Some(SkillPrompt::CreateFailed { ref error })
                if error.contains("did not return a valid SKILL.md")
        ),
        "the failure stays in the dialog: {:?}",
        ui.skills.prompt
    );
    assert!(ui.skills.preview.is_none(), "no preview on failure");
    // Fully modal: other printable keys are swallowed, never re-dispatched
    // and never leaked into the composer.
    for k in [ch('x'), ch('n')] {
        assert_eq!(handle_deck_key(k, &model, &mut ui), DeckAction::Handled);
    }
    assert!(matches!(
        ui.skills.prompt,
        Some(SkillPrompt::CreateFailed { .. })
    ));
    assert_eq!(ui.composer.buffer(), "");
    // Esc acknowledges and closes.
    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert!(ui.skills.prompt.is_none());
}

#[test]
fn skills_creating_dialog_esc_hides_but_creation_still_folds_in() {
    let mut model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.prompt = Some(SkillPrompt::Creating {
        description: "d".into(),
        scope: SkillScope::User,
    });
    ui.skills.searching = true;
    // Esc hides the dialog — the driver-side creation keeps running.
    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert!(ui.skills.prompt.is_none());
    assert!(
        ui.skills.searching,
        "hiding the dialog does not stop the op"
    );
    // The snapshot still folds in normally — with the dialog gone, no
    // preview pops (the user opted out of watching).
    let view = SkillsView {
        rows: vec![a_row("d-skill", SkillScope::User, true)],
        status: Some("created d-skill (user) — v1".into()),
        rejections: vec![],
        busy: false,
        created: Some("d-skill".into()),
    };
    ingest_inbound(&Inbound::Skills(view), &mut model, &mut ui);
    assert!(!ui.skills.searching);
    assert!(ui.skills.preview.is_none(), "no surprise popup after esc");
    assert_eq!(
        ui.skills.status.as_deref(),
        Some("created d-skill (user) — v1")
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
        rejections: vec![],
        busy: false,
        created: None,
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

/// **The witness (#4368).** `→` crosses from the installed pane to the
/// registry search and `←` comes back; past the search, with the query empty,
/// `→` rises to the tab strip instead of being swallowed — the keymap row's
/// parenthetical, which nothing pressed.
#[test]
fn skills_left_and_right_cross_the_panes_and_then_leave_the_tab() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    assert_eq!(ui.skills.focus, SkillsFocus::Installed);

    handle_deck_key(key(KeyCode::Right), &model, &mut ui);
    assert_eq!(ui.skills.focus, SkillsFocus::Search);
    handle_deck_key(key(KeyCode::Left), &model, &mut ui);
    assert_eq!(ui.skills.focus, SkillsFocus::Installed);

    handle_deck_key(key(KeyCode::Right), &model, &mut ui);
    handle_deck_key(key(KeyCode::Right), &model, &mut ui);
    assert_eq!(
        ui.tab,
        DeckTab::Skills.next(),
        "→ past the search is the next tab"
    );
}

/// A half-typed query claims `→` the way the composer does: leaving the tab
/// under it would strand the search the user is in the middle of writing.
#[test]
fn skills_a_typed_query_holds_on_to_the_right_arrow() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.focus = SkillsFocus::Search;
    handle_deck_key(ch('p'), &model, &mut ui);
    handle_deck_key(key(KeyCode::Right), &model, &mut ui);
    assert_eq!(ui.tab, DeckTab::Skills, "the tab did not move");
    assert_eq!(ui.skills.query, "p");
}

// ── The learned-skill lifecycle (#5046, SPEC 9.2) ───────────────────────

/// A learned row as the driver hands it over: `origin: auto`, with the
/// provenance assembled from its file and its scope sidecar.
fn a_learned_row(name: &str, was: &str) -> SkillRow {
    SkillRow {
        origin: "auto".to_string(),
        learned: Some(crate::envelope::LearnedProvenance {
            traces: 3,
            turn: Some(37),
            was: was.to_string(),
            sources: vec![crate::envelope::LearnedSource {
                reference: "reflection:1787462110".into(),
                observed_at: 1_787_462_110,
                snippet: "money amounts must be stored as minor units".into(),
            }],
        }),
        ..a_row(name, SkillScope::Project, true)
    }
}

/// `r` opens the rename dialog pre-filled with the current name and carrying
/// the `was <hash>` it promises to keep; ⏎ dispatches the rename.
#[test]
fn skills_r_renames_a_learned_skill_and_keeps_its_provenance() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.view.rows = vec![a_learned_row("money-is-minor-units-a1b2c3d4", "a1b2c3d4")];

    handle_deck_key(ch('r'), &model, &mut ui);
    assert_eq!(
        ui.skills.prompt,
        Some(SkillPrompt::Rename {
            scope: SkillScope::Project,
            name: "money-is-minor-units-a1b2c3d4".into(),
            buffer: "money-is-minor-units-a1b2c3d4".into(),
            was: "a1b2c3d4".into(),
        }),
        "the dialog opens on the current name, carrying the hash"
    );

    // Retype the name: a rename is an edit of what is already there.
    for _ in 0..9 {
        handle_deck_key(key(KeyCode::Backspace), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::Skill(SkillOp::Rename {
            scope: SkillScope::Project,
            from: "money-is-minor-units-a1b2c3d4".into(),
            to: "money-is-minor-units".into(),
        }))
    );
    assert!(ui.skills.prompt.is_none(), "the dialog closed");
    assert!(
        ui.composer.is_empty(),
        "the typing never reached the composer"
    );
}

/// The rename dialog is fully modal and esc abandons it — nothing is sent and
/// nothing typed leaks into the composer behind it.
#[test]
fn skills_rename_is_modal_and_esc_abandons_it() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.view.rows = vec![a_learned_row("mined-a1b2c3d4", "a1b2c3d4")];
    handle_deck_key(ch('r'), &model, &mut ui);
    for c in "xyz".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled);
    assert!(ui.skills.prompt.is_none());
    assert!(ui.composer.is_empty(), "no keystroke reached the composer");
}

/// `r` on a skill a human wrote refuses, and says why — a name its author
/// chose is not a hash suffix waiting to be replaced.
#[test]
fn skills_r_refuses_a_skill_nobody_learned() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.view.rows = vec![a_row("rust-review", SkillScope::Project, true)];
    handle_deck_key(ch('r'), &model, &mut ui);
    assert!(ui.skills.prompt.is_none(), "no dialog opened");
    assert!(
        ui.skills
            .status
            .as_deref()
            .is_some_and(|s| s.contains("not learned from traces")),
        "{:?}",
        ui.skills.status
    );
}

/// **The witness, deck-side (#5046).** `x` twice on a learned row dispatches a
/// `Reject`, not an `Uninstall` — the op that carries the negative signal.
/// One press only arms it, and says what the second press will do.
#[test]
fn skills_x_twice_rejects_a_learned_skill() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.view.rows = vec![a_learned_row("money-is-minor-units-a1b2c3d4", "a1b2c3d4")];

    let first = handle_deck_key(ch('x'), &model, &mut ui);
    assert_eq!(first, DeckAction::Handled, "one press only arms it");
    assert!(ui.skills.reject_armed);
    assert!(
        ui.skills
            .status
            .as_deref()
            .is_some_and(|s| s.contains("REJECT") && s.contains("learner")),
        "the arming line says what the second press costs: {:?}",
        ui.skills.status
    );

    let second = handle_deck_key(ch('x'), &model, &mut ui);
    assert_eq!(
        second,
        DeckAction::Send(WorkspaceInput::Skill(SkillOp::Reject {
            scope: SkillScope::Project,
            name: "money-is-minor-units-a1b2c3d4".into(),
        })),
        "reject, not uninstall — the signal is the point"
    );
}

/// The two destructive verbs cannot complete each other: `ctrl+x` then `x` is
/// a user changing their mind, and must arm rather than fire.
#[test]
fn skills_a_ctrl_x_does_not_confirm_a_pending_reject() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.view.rows = vec![a_learned_row("mined-a1b2c3d4", "a1b2c3d4")];

    handle_deck_key(ch('x'), &model, &mut ui);
    assert!(ui.skills.reject_armed);
    let action = handle_deck_key(ctrl('x'), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled);
    assert!(!ui.skills.reject_armed, "the rejection was disarmed");
    assert!(
        ui.skills.uninstall_armed,
        "and ctrl+x armed its own verb instead"
    );

    // And the other direction.
    let action = handle_deck_key(ch('x'), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled);
    assert!(!ui.skills.uninstall_armed);
    assert!(ui.skills.reject_armed);
}

/// `x` on a skill a human wrote has no learner to teach, so it refuses and
/// names the key that does delete it.
#[test]
fn skills_x_refuses_a_skill_nobody_learned() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.view.rows = vec![a_row("rust-review", SkillScope::Project, true)];
    handle_deck_key(ch('x'), &model, &mut ui);
    handle_deck_key(ch('x'), &model, &mut ui);
    assert!(!ui.skills.reject_armed);
    assert!(
        ui.skills
            .status
            .as_deref()
            .is_some_and(|s| s.contains("ctrl+x twice")),
        "{:?}",
        ui.skills.status
    );
}

/// `ctrl+o` on a learned row opens on its **source traces** (SPEC 9.2), with
/// the provenance as the sub-line and the body still reachable below.
#[test]
fn skills_ctrl_o_on_a_learned_row_lists_its_source_traces() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.view.rows = vec![a_learned_row("money-is-minor-units-a1b2c3d4", "a1b2c3d4")];
    handle_deck_key(ctrl('o'), &model, &mut ui);
    let preview = ui.skills.preview.as_ref().expect("preview opened");
    let body = preview.body.as_deref().expect("local body");
    assert!(body.starts_with("## Source traces"), "{body}");
    assert!(body.contains("reflection:1787462110"), "{body}");
    assert!(
        body.contains("## The skill"),
        "the body is still reachable below the traces: {body}"
    );
    assert_eq!(
        preview.subtitle, "from 3 traces · turn 37 · was a1b2c3d4",
        "the provenance is the sub-line"
    );
}

/// A learned skill whose file kept no evidence says so, rather than showing an
/// empty heading that reads as a rendering bug.
#[test]
fn skills_ctrl_o_says_so_when_a_learned_skill_kept_no_traces() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    let mut row = a_learned_row("mined-a1b2c3d4", "a1b2c3d4");
    if let Some(learned) = row.learned.as_mut() {
        learned.traces = 0;
        learned.sources.clear();
    }
    ui.skills.view.rows = vec![row];
    handle_deck_key(ctrl('o'), &model, &mut ui);
    let body = ui
        .skills
        .preview
        .as_ref()
        .and_then(|p| p.body.clone())
        .expect("body");
    assert!(body.contains("records no traces"), "{body}");
}

/// `ctrl+o` on an ordinary skill is unchanged: the `SKILL.md` body, no trace
/// section invented for a skill that has none.
#[test]
fn skills_ctrl_o_on_an_authored_row_still_previews_the_body() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.view.rows = vec![a_row("rust-review", SkillScope::Project, true)];
    handle_deck_key(ctrl('o'), &model, &mut ui);
    let preview = ui.skills.preview.as_ref().expect("preview opened");
    assert_eq!(preview.body.as_deref(), Some("b"));
    assert!(!preview.subtitle.contains("traces"), "{preview:?}");
}

// ── The rejected-skills review (`!`) ─────────────────────────────────────

fn a_rejection(name: &str, scope: SkillScope) -> RejectedSkillRow {
    RejectedSkillRow {
        scope,
        name: name.to_string(),
        mined_as: format!("{name}-a1b2c3d4"),
        rejected_at: 1_700_000_000,
    }
}

/// `!` with nothing rejected refuses out loud, instead of opening an empty
/// picker.
#[test]
fn skills_bang_refuses_when_nothing_is_rejected() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    handle_deck_key(ch('!'), &model, &mut ui);
    assert!(ui.skills.prompt.is_none());
    assert!(
        ui.skills
            .status
            .as_deref()
            .is_some_and(|s| s.contains("nothing rejected")),
        "{:?}",
        ui.skills.status
    );
}

/// `!` with rejections on record opens the review, at row zero.
#[test]
fn skills_bang_opens_the_rejected_review() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.view.rejections = vec![
        a_rejection("bench-rig-access", SkillScope::Project),
        a_rejection("prefer-tables", SkillScope::User),
    ];
    handle_deck_key(ch('!'), &model, &mut ui);
    assert_eq!(ui.skills.prompt, Some(SkillPrompt::Rejected { sel: 0 }));
}

/// **The witness, deck-side.** `↓` moves the selection. `u` dispatches
/// `Unreject` for the highlighted row, not the first one. That proves
/// navigation reaches the dispatch.
#[test]
fn skills_rejected_review_navigates_and_unrejects_the_highlighted_row() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.view.rejections = vec![
        a_rejection("bench-rig-access", SkillScope::Project),
        a_rejection("prefer-tables", SkillScope::User),
    ];
    ui.skills.prompt = Some(SkillPrompt::Rejected { sel: 0 });

    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert_eq!(ui.skills.prompt, Some(SkillPrompt::Rejected { sel: 1 }));

    let action = handle_deck_key(ch('u'), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::Skill(SkillOp::Unreject {
            scope: SkillScope::User,
            mined_as: "prefer-tables-a1b2c3d4".into(),
        })),
        "the SECOND row, which the down-arrow selected"
    );
    assert!(ui.skills.prompt.is_none(), "the dialog closes on dispatch");
}

/// `⏎` is the same verb as `u`. Either one confirms the highlighted row.
#[test]
fn skills_rejected_review_enter_also_unrejects() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.view.rejections = vec![a_rejection("bench-rig-access", SkillScope::Project)];
    ui.skills.prompt = Some(SkillPrompt::Rejected { sel: 0 });

    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::Skill(SkillOp::Unreject {
            scope: SkillScope::Project,
            mined_as: "bench-rig-access-a1b2c3d4".into(),
        }))
    );
}

/// Esc abandons the review with nothing dispatched. Reading the list is not
/// itself a change.
#[test]
fn skills_rejected_review_esc_cancels() {
    let model = WorkspaceModel::new();
    let mut ui = skills_ui();
    ui.skills.view.rejections = vec![a_rejection("bench-rig-access", SkillScope::Project)];
    ui.skills.prompt = Some(SkillPrompt::Rejected { sel: 0 });

    let action = handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled);
    assert!(ui.skills.prompt.is_none());
}
