//! ISSUES tab: browse, search, the create form, and type-ahead.

use super::*;

// ── ISSUES tab ─────────────────────────────────────────────────────────

use crate::envelope::{EntityField, EntityHit, IssueAction, IssueRow};

fn issues_ui() -> DeckUi {
    let mut ui = ready_ui();
    ui.set_tab(DeckTab::Issues);
    ui
}

fn a_issue(key: &str) -> IssueRow {
    IssueRow {
        key: key.to_string(),
        title: format!("title of {key}"),
        state: "open".into(),
        labels: vec!["bug".into()],
        assignee: Some("@octocat".into()),
        url: format!("https://github.com/o/r/issues/{key}"),
        updated_at: None,
        linked: None,
    }
}

fn a_hit(kind: &str, label: &str, insert: &str) -> EntityHit {
    EntityHit {
        kind: kind.into(),
        label: label.into(),
        description: format!("about {label}"),
        insert: insert.into(),
    }
}

/// Open the create form and move focus to `field`.
fn form_on(ui: &mut DeckUi, model: &WorkspaceModel, field: IssueField) {
    handle_deck_key(ch('n'), model, ui);
    assert_eq!(ui.issues.mode, IssuesMode::Create);
    while ui.issues.form_field != field {
        handle_deck_key(key(KeyCode::Tab), model, ui);
    }
}

#[test]
fn issues_first_tab_visit_queues_a_refresh() {
    let model = WorkspaceModel::new();
    let mut ui = ready_ui();
    ui.set_tab(DeckTab::Mcp); // ISSUES is Mcp's Tab successor
    handle_deck_key(key(KeyCode::Tab), &model, &mut ui);
    assert_eq!(ui.tab, DeckTab::Issues);
    assert!(ui.issues.busy, "the first visit loads without a keypress");
    assert!(matches!(
        ui.pending_inputs.as_slice(),
        [WorkspaceInput::IssuesRefresh {
            query: None,
            state: None,
            page: 0,
            seq: 1,
        }]
    ));
    // A second visit does not re-fetch (busy / loaded gate).
    handle_deck_key(key(KeyCode::Tab), &model, &mut ui);
    handle_deck_key(key(KeyCode::BackTab), &model, &mut ui);
    assert_eq!(ui.pending_inputs.len(), 1, "no duplicate refresh");
}

#[test]
fn p_opens_a_confirmation_and_enter_submits_and_forwards_to_the_transcript() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.issues.rows = vec![a_issue("#7"), a_issue("#8")];
    ui.issues.loaded = true;
    // Pick both rows with space, then hit p.
    handle_deck_key(key(KeyCode::Char(' ')), &model, &mut ui);
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    handle_deck_key(key(KeyCode::Char(' ')), &model, &mut ui);
    let action = handle_deck_key(ch('p'), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled, "p only opens the popup");
    assert_eq!(ui.issues.mode, IssuesMode::ConfirmSend);
    assert_eq!(ui.tab, DeckTab::Issues, "still on the issues tab");

    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(ui.issues.mode, IssuesMode::Browse, "the popup closed");
    assert_eq!(
        ui.tab,
        DeckTab::Session,
        "enter forwards to the transcript tab"
    );
    assert!(ui.issues.picked.is_empty(), "the picks were consumed");
    let text = match action {
        DeckAction::Send(WorkspaceInput::Enqueue { text }) => text,
        other => panic!("expected Enqueue, got {other:?}"),
    };
    for needle in [
        "#7",
        "#8",
        "title of #7",
        "https://github.com/o/r/issues/#7",
        "Labels: bug",
        "Source: stella command deck ISSUES tab",
        "Read the ENTIRE issue body and EVERY comment",
        "definition of done",
    ] {
        assert!(
            text.contains(needle),
            "prompt is missing {needle:?}:\n{text}"
        );
    }
}

#[test]
fn p_confirmation_esc_cancels_without_submitting() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.issues.rows = vec![a_issue("#7")];
    ui.issues.loaded = true;
    handle_deck_key(key(KeyCode::Char(' ')), &model, &mut ui);
    handle_deck_key(ch('p'), &model, &mut ui);
    assert_eq!(ui.issues.mode, IssuesMode::ConfirmSend);
    let action = handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled);
    assert_eq!(ui.issues.mode, IssuesMode::Browse);
    assert_eq!(ui.tab, DeckTab::Issues, "esc stays on the issues tab");
    assert!(
        !ui.issues.picked.is_empty(),
        "esc keeps the picks — cancelling is not discarding"
    );
}

#[test]
fn p_with_no_rows_notifies_instead_of_opening_the_popup() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.issues.loaded = true;
    let action = handle_deck_key(ch('p'), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled);
    assert_eq!(ui.issues.mode, IssuesMode::Browse, "no popup without rows");
    assert!(ui.issues.notice.is_some());
}

#[test]
fn issues_browse_keys_refresh_and_open_the_start_work_draft() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.issues.rows = vec![a_issue("#7")];
    ui.issues.loaded = true;
    let action = handle_deck_key(ch('r'), &model, &mut ui);
    assert!(matches!(
        action,
        DeckAction::Send(WorkspaceInput::IssuesRefresh { query: None, .. })
    ));
    let action = handle_deck_key(ch('w'), &model, &mut ui);
    match action {
        DeckAction::Send(WorkspaceInput::IssueDraftPlan { key, seq }) => {
            assert_eq!(key, "#7");
            assert_eq!(
                ui.issues.start_work.wait, seq,
                "the panel waits on the request it just sent"
            );
        }
        other => panic!("expected IssueDraftPlan, got {other:?}"),
    }
    assert_eq!(ui.issues.mode, IssuesMode::StartWork);
    assert_eq!(ui.issues.start_work.issue_key, "#7");
    assert!(
        ui.issues.start_work.draft.is_none(),
        "the overlay opens empty — the draft is the driver's answer"
    );
}

#[test]
fn issues_tracker_search_fires_on_enter_and_esc_returns() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    handle_deck_key(ch('/'), &model, &mut ui);
    assert_eq!(ui.issues.mode, IssuesMode::SearchTracker);
    for c in "flaky".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert!(
        ui.composer.buffer().is_empty(),
        "search typing never reaches the composer"
    );
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    match action {
        DeckAction::Send(WorkspaceInput::IssuesRefresh { query, .. }) => {
            assert_eq!(query.as_deref(), Some("flaky"));
        }
        other => panic!("expected IssuesRefresh, got {other:?}"),
    }
    assert_eq!(ui.issues.mode, IssuesMode::Browse);
}

#[test]
fn issues_comment_prompt_sends_the_act_for_the_selected_issue() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.issues.rows = vec![a_issue("ENG-42")];
    handle_deck_key(ch('c'), &model, &mut ui);
    assert_eq!(ui.issues.mode, IssuesMode::Comment);
    for c in "lgtm".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::IssueAct {
            key: "ENG-42".into(),
            action: IssueAction::Comment("lgtm".into()),
            seq: 1,
        })
    );
}

#[test]
fn typeahead_opens_on_the_first_char_and_fires_per_keystroke() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    form_on(&mut ui, &model, IssueField::Assignee);
    assert!(!ui.issues.typeahead.open(), "closed until the first char");

    // `@` alone opens the popup and searches the EMPTY query (the
    // backend lists all members for it).
    handle_deck_key(ch('@'), &model, &mut ui);
    assert!(ui.issues.typeahead.open(), "first char opens the popup");
    assert!(matches!(
        ui.pending_inputs.last(),
        Some(WorkspaceInput::EntitySearch {
            field: EntityField::Assignee,
            query,
            ..
        }) if query.is_empty()
    ));

    // Every subsequent edit re-fires — insert and backspace alike.
    handle_deck_key(ch('m'), &model, &mut ui);
    assert!(matches!(
        ui.pending_inputs.last(),
        Some(WorkspaceInput::EntitySearch { query, .. }) if query == "m"
    ));
    handle_deck_key(key(KeyCode::Backspace), &model, &mut ui);
    assert!(matches!(
        ui.pending_inputs.last(),
        Some(WorkspaceInput::EntitySearch { query, .. }) if query.is_empty()
    ));
    assert_eq!(ui.pending_inputs.len(), 3, "one request per keystroke");

    // Deleting the last character closes the popup entirely.
    handle_deck_key(key(KeyCode::Backspace), &model, &mut ui);
    assert!(!ui.issues.typeahead.open(), "empty field ⇒ popup closed");
}

#[test]
fn typeahead_drops_stale_hits_and_applies_the_newest() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    form_on(&mut ui, &model, IssueField::Assignee);
    handle_deck_key(ch('m'), &model, &mut ui); // seq 1
    handle_deck_key(ch('a'), &model, &mut ui); // seq 2
    let newest_seq = ui.issues.typeahead.seq;
    let mut m = WorkspaceModel::new();

    // The keystroke-1 reply lands late: stale, dropped.
    ingest_inbound(
        &Inbound::EntityHits {
            field: EntityField::Assignee,
            seq: newest_seq - 1,
            query: "m".into(),
            hits: vec![a_hit("Person", "stale", "@stale")],
        },
        &mut m,
        &mut ui,
    );
    assert!(ui.issues.typeahead.hits.is_empty(), "stale reply dropped");

    // The newest reply applies.
    ingest_inbound(
        &Inbound::EntityHits {
            field: EntityField::Assignee,
            seq: newest_seq,
            query: "ma".into(),
            hits: vec![a_hit("Person", "macanderson", "@macanderson")],
        },
        &mut m,
        &mut ui,
    );
    assert_eq!(ui.issues.typeahead.hits.len(), 1);
    assert!(!ui.issues.typeahead.loading);

    // A reply for the WRONG field never lands either.
    ingest_inbound(
        &Inbound::EntityHits {
            field: EntityField::Label,
            seq: newest_seq + 5,
            query: "ma".into(),
            hits: vec![a_hit("Label", "major", "major")],
        },
        &mut m,
        &mut ui,
    );
    assert_eq!(ui.issues.typeahead.hits[0].label, "macanderson");
}

#[test]
fn typeahead_enter_replaces_the_assignee_and_esc_keeps_typed_text() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    form_on(&mut ui, &model, IssueField::Assignee);
    for c in "mac".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    ui.issues.typeahead.hits = vec![a_hit("Person", "macanderson", "@macanderson")];
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        ui.issues.form_assignee, "@macanderson",
        "enter REPLACES the assignee field with the hit's insert"
    );
    assert!(!ui.issues.typeahead.open(), "picking closes the popup");
    assert_eq!(ui.issues.mode, IssuesMode::Create, "still in the form");

    // Esc with the popup open closes it but keeps the field text.
    handle_deck_key(ch('x'), &model, &mut ui);
    assert!(ui.issues.typeahead.open());
    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert!(!ui.issues.typeahead.open());
    assert_eq!(ui.issues.form_assignee, "@macandersonx", "text kept");
    assert_eq!(ui.issues.mode, IssuesMode::Create, "form still open");
}

#[test]
fn typeahead_tab_appends_labels_comma_separated() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    form_on(&mut ui, &model, IssueField::Labels);
    for c in "bug, urg".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    // The query is the segment being typed, not the whole field.
    assert!(matches!(
        ui.pending_inputs.last(),
        Some(WorkspaceInput::EntitySearch {
            field: EntityField::Label,
            query,
            ..
        }) if query == "urg"
    ));
    ui.issues.typeahead.hits = vec![a_hit("Label", "urgent", "urgent")];
    handle_deck_key(key(KeyCode::Tab), &model, &mut ui);
    assert_eq!(
        ui.issues.form_labels, "bug, urgent",
        "the picked label replaces the partial segment, comma-appended"
    );
}

#[test]
fn entity_query_and_insert_helpers_cover_both_fields() {
    assert_eq!(entity_query(EntityField::Assignee, "@mac"), "mac");
    assert_eq!(entity_query(EntityField::Assignee, "@"), "");
    assert_eq!(entity_query(EntityField::Label, "bug, ur"), "ur");
    assert_eq!(entity_query(EntityField::Label, "bug"), "bug");

    let mut assignee = "mac".to_string();
    apply_entity_insert(&mut assignee, EntityField::Assignee, "@macanderson");
    assert_eq!(assignee, "@macanderson");

    let mut labels = "ur".to_string();
    apply_entity_insert(&mut labels, EntityField::Label, "urgent");
    assert_eq!(labels, "urgent", "a lone partial segment is replaced");
    labels.push_str(", b");
    apply_entity_insert(&mut labels, EntityField::Label, "bug");
    assert_eq!(labels, "urgent, bug");
}

#[test]
fn issue_form_ctrl_s_submits_the_parsed_fields() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    form_on(&mut ui, &model, IssueField::Title);
    for c in "Fix the flake".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui); // → Body
    assert_eq!(ui.issues.form_field, IssueField::Body);
    for c in "line one".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui); // newline in body
    for c in "line two".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    handle_deck_key(key(KeyCode::Tab), &model, &mut ui); // → Labels
    for c in "bug".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    // Close the popup the typing opened, then Tab to Assignee.
    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    handle_deck_key(key(KeyCode::Tab), &model, &mut ui);
    assert_eq!(ui.issues.form_field, IssueField::Assignee);

    let action = handle_deck_key(ctrl('s'), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::IssueCreate {
            title: "Fix the flake".into(),
            body: "line one\nline two".into(),
            labels: vec!["bug".into()],
            assignee: None,
            seq: 4, // the three label keystrokes consumed seqs 1–3
        }),
    );
    assert_eq!(ui.issues.mode, IssuesMode::Browse);
    assert!(ui.issues.busy);
}

#[test]
fn issues_list_ingest_folds_rows_clears_busy_and_drops_stale() {
    let mut model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.issues.busy = true;
    ui.issues.list_wait = 3;
    ui.issues.sel = 9;

    // A stale reply (an older request) is ignored outright.
    ingest_inbound(
        &Inbound::IssuesList {
            seq: 2,
            outcome: Ok(vec![a_issue("#1")]),
        },
        &mut model,
        &mut ui,
    );
    assert!(ui.issues.rows.is_empty(), "stale list dropped");
    assert!(ui.issues.busy, "…and the newer request is still awaited");

    // The awaited reply folds in: rows, clamped selection, notice.
    ingest_inbound(
        &Inbound::IssuesList {
            seq: 3,
            outcome: Ok(vec![a_issue("#1"), a_issue("#2")]),
        },
        &mut model,
        &mut ui,
    );
    assert_eq!(ui.issues.rows.len(), 2);
    assert_eq!(ui.issues.sel, 1, "selection clamped to the new list");
    assert!(!ui.issues.busy);
    assert!(ui.issues.loaded);
    assert_eq!(
        model.agents.len(),
        0,
        "the model fold ignores the out-of-band list"
    );

    // An error outcome lands in the notice line (the no-tracker hint).
    ui.issues.list_wait = 4;
    ingest_inbound(
        &Inbound::IssuesList {
            seq: 4,
            outcome: Err("no tracker connected — run `stella connect github`".into()),
        },
        &mut model,
        &mut ui,
    );
    assert!(
        ui.issues
            .notice
            .as_deref()
            .is_some_and(|n| n.contains("no tracker connected")),
        "{:?}",
        ui.issues.notice
    );
}

/// The witness for SPEC 9.4's heat sort: a list reply arrives in the tracker's
/// order and is adopted in the graph's, heaviest first, with the caption flag
/// the tab reads set. The graph is the deck's own snapshot — the same one the
/// GRAPH tab ranks coupling from — so the ordering has a source to name.
#[test]
fn issues_list_ingest_orders_the_backlog_by_the_graphs_coupling() {
    let mut model = WorkspaceModel::new();
    model.now_ms = 1_770_940_800_000; // 2026-02-13T00:00:00Z
    let mut ui = issues_ui();
    ui.graph = Some(crate::scenario::demo_graph());
    ui.issues.list_wait = 1;

    let claimed = |key: &str, files: &[&str]| IssueRow {
        updated_at: Some("2026-01-14T09:30:00Z".into()),
        linked: Some(crate::envelope::LinkedWork {
            touched_files: files.iter().map(|f| (*f).to_string()).collect(),
            ..crate::scenario::demo_linked_work()
        }),
        ..a_issue(key)
    };

    ingest_inbound(
        &Inbound::IssuesList {
            seq: 1,
            outcome: Ok(vec![
                // The graph has never heard of this file, so the row keeps the
                // tracker's place rather than being ranked against a guess.
                claimed("#unknown", &["src/nowhere.rs"]),
                a_issue("#unclaimed"),
                claimed("#coupled", &["stella-core/src/driver.rs"]),
            ]),
        },
        &mut model,
        &mut ui,
    );

    let keys: Vec<&str> = ui.issues.rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, vec!["#coupled", "#unknown", "#unclaimed"]);
    assert!(ui.issues.heat_sorted, "the tab may caption this ordering");
}

/// With no graph loaded there is no coupling to sort by, so the tracker's own
/// order stands and the tab draws no caption over it.
#[test]
fn issues_list_ingest_keeps_the_tracker_order_when_no_graph_is_loaded() {
    let mut model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.graph = None;
    ui.issues.list_wait = 1;
    ingest_inbound(
        &Inbound::IssuesList {
            seq: 1,
            outcome: Ok(vec![a_issue("#3"), a_issue("#1"), a_issue("#2")]),
        },
        &mut model,
        &mut ui,
    );
    let keys: Vec<&str> = ui.issues.rows.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, vec!["#3", "#1", "#2"]);
    assert!(!ui.issues.heat_sorted);
}

#[test]
fn issue_act_done_ingest_reports_the_outcome() {
    let mut model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.issues.act_wait = 2;
    ui.issues.busy = true;
    ingest_inbound(
        &Inbound::IssueActDone {
            seq: 2,
            key: "#7".into(),
            outcome: Ok("created #7 — https://github.com/o/r/issues/7".into()),
        },
        &mut model,
        &mut ui,
    );
    assert!(!ui.issues.busy);
    assert!(
        ui.issues
            .notice
            .as_deref()
            .is_some_and(|n| n.contains("created #7")),
        "{:?}",
        ui.issues.notice
    );
}

#[test]
fn issues_space_toggles_a_pick_and_the_pick_follows_the_key() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.issues.rows = vec![a_issue("#7"), a_issue("#8")];
    ui.issues.loaded = true;

    handle_deck_key(key(KeyCode::Char(' ')), &model, &mut ui);
    assert!(
        ui.issues.picked.contains("#7"),
        "space picks the cursor row"
    );

    // Moving the cursor leaves the pick on the issue, not the row index.
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert!(ui.issues.picked.contains("#7"));
    assert!(!ui.issues.picked.contains("#8"));

    // A second space on the same row unpicks it.
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    handle_deck_key(key(KeyCode::Char(' ')), &model, &mut ui);
    assert!(ui.issues.picked.is_empty(), "space again unpicks");
}

#[test]
fn issues_refresh_prunes_picks_that_left_the_list() {
    let mut model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.issues.rows = vec![a_issue("#7"), a_issue("#8")];
    ui.issues.picked.insert("#7".into());
    ui.issues.picked.insert("#8".into());
    ui.issues.list_wait = 1;
    ingest_inbound(
        &Inbound::IssuesList {
            seq: 1,
            outcome: Ok(vec![a_issue("#8")]),
        },
        &mut model,
        &mut ui,
    );
    assert!(
        !ui.issues.picked.contains("#7"),
        "gone from the list, gone from the picks"
    );
    assert!(ui.issues.picked.contains("#8"));
}

#[test]
fn issues_x_closes_an_open_issue_and_reopens_a_closed_one() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    let mut closed = a_issue("#9");
    closed.state = "closed".into();
    ui.issues.rows = vec![a_issue("#7"), closed];
    ui.issues.loaded = true;

    // Cursor on the open row: close.
    let action = handle_deck_key(ch('x'), &model, &mut ui);
    match action {
        DeckAction::Send(WorkspaceInput::IssueAct { key, action, .. }) => {
            assert_eq!(key, "#7");
            assert_eq!(action, IssueAction::Close);
        }
        other => panic!("expected IssueAct::Close, got {other:?}"),
    }

    // Cursor on the closed row: re-open. Its own variant, not
    // `SetStatus("open")` — the driver selects the provider call by matching
    // the action, never by comparing a status string.
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    let action = handle_deck_key(ch('x'), &model, &mut ui);
    match action {
        DeckAction::Send(WorkspaceInput::IssueAct { key, action, .. }) => {
            assert_eq!(key, "#9");
            assert_eq!(action, IssueAction::Reopen);
        }
        other => panic!("expected IssueAct::Reopen, got {other:?}"),
    }
    assert!(
        ui.issues
            .notice
            .as_deref()
            .is_some_and(|n| n.starts_with("re-opening #9")),
        // The row key already carries its `#`; a `#{key}` here would say
        // "re-opening ##9".
        "{:?}",
        ui.issues.notice
    );
}

#[test]
fn issues_p_submits_the_picked_issues_as_a_prompt() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.issues.rows = vec![a_issue("#7"), a_issue("#8")];
    ui.issues.loaded = true;
    ui.issues.picked.insert("#7".into());
    ui.issues.picked.insert("#8".into());

    // p opens the confirmation; ⏎ submits.
    handle_deck_key(ch('p'), &model, &mut ui);
    assert_eq!(ui.issues.mode, IssuesMode::ConfirmSend);
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    match action {
        DeckAction::Send(WorkspaceInput::Enqueue { text }) => {
            // Exact keys, not `contains`: the row key already carries its
            // `#`, so a `#{key}` in the prompt builder would produce "##7".
            assert!(text.contains("Issue #7\n"), "{text}");
            assert!(text.contains("Issue #8\n"), "{text}");
            assert!(text.contains("title of #7"), "{text}");
        }
        other => panic!("expected an enqueued prompt, got {other:?}"),
    }
    assert!(ui.issues.picked.is_empty(), "submitting clears the picks");
}

#[test]
fn issues_p_with_no_picks_submits_the_cursor_row() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.issues.rows = vec![a_issue("#7")];
    ui.issues.loaded = true;

    // No picks: p stages the cursor row, ⏎ submits it.
    handle_deck_key(ch('p'), &model, &mut ui);
    assert_eq!(ui.issues.mode, IssuesMode::ConfirmSend);
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    match action {
        DeckAction::Send(WorkspaceInput::Enqueue { text }) => {
            assert!(text.contains("Issue #7\n"), "{text}");
        }
        other => panic!("expected an enqueued prompt, got {other:?}"),
    }
}

#[test]
fn issues_brackets_page_the_active_query() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    // A full page of rows — a short page is how the tab knows the list is
    // exhausted, so paging needs `ISSUES_PAGE_SIZE` rows to move forward.
    ui.issues.rows = (0..30).map(|i| a_issue(&format!("#{i}"))).collect();
    ui.issues.loaded = true;
    ui.issues.active_query = Some("flaky".into());

    let action = handle_deck_key(ch(']'), &model, &mut ui);
    match action {
        DeckAction::Send(WorkspaceInput::IssuesRefresh { query, page, .. }) => {
            assert_eq!(
                query.as_deref(),
                Some("flaky"),
                "paging re-issues the search"
            );
            // The witness for the paging defect. `page` has to ride the
            // request: without it the driver read a literal offset 0, so `]`
            // re-fetched page one under a notice that said "page 2".
            assert_eq!(page, 1, "the request carries the page it is asking for");
        }
        other => panic!("expected IssuesRefresh, got {other:?}"),
    }
    assert_eq!(ui.issues.page, 1);

    let action = handle_deck_key(ch('['), &model, &mut ui);
    match action {
        DeckAction::Send(WorkspaceInput::IssuesRefresh { page, .. }) => {
            assert_eq!(page, 0, "paging back asks for the page it moved to");
        }
        other => panic!("expected IssuesRefresh, got {other:?}"),
    }
    assert_eq!(ui.issues.page, 0);

    // `[` on the first page goes nowhere.
    let action = handle_deck_key(ch('['), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled);
    assert_eq!(ui.issues.page, 0);
}

#[test]
fn issues_bracket_on_a_short_page_says_there_is_no_next() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.issues.rows = vec![a_issue("#7")];
    ui.issues.loaded = true;

    let action = handle_deck_key(ch(']'), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled, "a short page is the last page");
    assert_eq!(ui.issues.page, 0);
    assert!(
        ui.issues
            .notice
            .as_deref()
            .is_some_and(|n| n.contains("no next page")),
        "{:?}",
        ui.issues.notice
    );
}

#[test]
fn issues_search_resets_the_page_and_remembers_the_query() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.issues.page = 3;
    handle_deck_key(ch('/'), &model, &mut ui);
    for c in "race".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(ui.issues.page, 0, "a new search is a new list");
    assert_eq!(ui.issues.active_query.as_deref(), Some("race"));
}

#[test]
fn a_failed_page_fetch_leaves_the_header_on_the_page_still_shown() {
    let mut model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.issues.rows = (0..30).map(|i| a_issue(&format!("#{i}"))).collect();
    ui.issues.loaded = true;

    // Page forward, and let that fetch fail.
    handle_deck_key(ch(']'), &model, &mut ui);
    assert_eq!(ui.issues.page, 1, "the request asked for page 2");
    let seq = ui.issues.list_wait;
    ingest_inbound(
        &Inbound::IssuesList {
            seq,
            outcome: Err("gh: could not connect".into()),
        },
        &mut model,
        &mut ui,
    );
    assert_eq!(
        ui.issues.loaded_page, 0,
        "the rows on screen are still page 1, so the header must say so"
    );

    // A page that lands moves it.
    handle_deck_key(ch(']'), &model, &mut ui);
    let seq = ui.issues.list_wait;
    ingest_inbound(
        &Inbound::IssuesList {
            seq,
            outcome: Ok(vec![a_issue("#99")]),
        },
        &mut model,
        &mut ui,
    );
    assert_eq!(ui.issues.loaded_page, ui.issues.page);
}

/// **The witness (#4368).** `s` opens the status prompt for the selected
/// issue, and says why rather than doing nothing when the list is empty —
/// the keymap row nothing pressed.
#[test]
fn issues_s_opens_the_status_prompt_for_the_selection() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    handle_deck_key(ch('s'), &model, &mut ui);
    assert_eq!(
        ui.issues.mode,
        IssuesMode::Browse,
        "nothing to set a status on"
    );
    assert!(ui.issues.notice.is_some(), "and the key says why");

    ui.issues.rows = vec![a_issue("7")];
    ui.issues.input = "stale".into();
    handle_deck_key(ch('s'), &model, &mut ui);
    assert_eq!(ui.issues.mode, IssuesMode::SetStatus);
    assert!(ui.issues.input.is_empty(), "the prompt starts blank");
}

/// `o` hands the selected issue to the browser. Witnessed on a row with no
/// url — the arm that reports instead of launching one — because a witness
/// must not open a browser window on whoever runs the suite.
#[test]
fn issues_o_opens_the_selection_in_the_browser() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    let mut row = a_issue("7");
    row.url.clear();
    ui.issues.rows = vec![row];

    assert_eq!(
        handle_deck_key(ch('o'), &model, &mut ui),
        DeckAction::Handled
    );
    let notice = ui.issues.notice.clone().unwrap_or_default();
    assert!(
        notice.contains("no url"),
        "the key reached the arm: {notice:?}"
    );

    handle_deck_key(ch('z'), &model, &mut ui);
    ui.issues.notice = None;
    handle_deck_key(ch('o'), &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "zo", "a prompt in progress wins");
    assert!(ui.issues.notice.is_none());
}

// ── SPEC 8.2: the start-work draft and its approval gate ───────────────

fn a_draft(key: &str) -> crate::start_work::StartWorkDraft {
    use crate::start_work::{DraftContract, DraftSources, DraftTask, StartWorkDraft};
    StartWorkDraft {
        issue_key: key.to_string(),
        issue_title: "dedup digest persists across CI runs".into(),
        sources: DraftSources::default(),
        tasks: vec![
            DraftTask {
                subject: "read the seen-set write path".into(),
                contract: None,
            },
            DraftTask {
                subject: "persist the digest set".into(),
                contract: Some(DraftContract {
                    done_means: "the file exists after a run".into(),
                    mechanism: "graph".into(),
                    deterministic: true,
                }),
            },
        ],
        gates: 5,
        estimate: None,
    }
}

/// Open the overlay on `#7` and let the driver's draft land on it.
fn drafted(model: &mut WorkspaceModel, ui: &mut DeckUi) {
    ui.issues.rows = vec![a_issue("#7")];
    ui.issues.loaded = true;
    handle_deck_key(ch('w'), model, ui);
    let seq = ui.issues.start_work.wait;
    ingest_inbound(
        &Inbound::IssueDraft {
            seq,
            outcome: Ok(Box::new(a_draft("#7"))),
        },
        model,
        ui,
    );
}

/// SPEC 8.2's acceptance: **nothing runs before `a`**.
///
/// The witness is an absence, so it is asserted the way
/// `no_tool_call_reaches_an_ungranted_server_before_the_grant` asserts one:
/// drive every key the overlay accepts *except* the approval, and prove the
/// deck emitted no [`WorkspaceInput::IssueStartWork`] — the only request that
/// opens a branch, takes the `issue:<n>` claim, or authors a plan revision.
/// Then press `a` and prove it emits exactly one.
#[test]
fn nothing_runs_before_the_approval_key() {
    let mut model = WorkspaceModel::new();
    let mut ui = issues_ui();
    let mut sent: Vec<WorkspaceInput> = Vec::new();
    let press = |ui: &mut DeckUi, model: &WorkspaceModel, c: char, sent: &mut Vec<_>| {
        if let DeckAction::Send(input) = handle_deck_key(ch(c), model, ui) {
            sent.push(input);
        }
    };

    ui.issues.rows = vec![a_issue("#7")];
    ui.issues.loaded = true;
    press(&mut ui, &model, 'w', &mut sent);
    let seq = ui.issues.start_work.wait;
    ingest_inbound(
        &Inbound::IssueDraft {
            seq,
            outcome: Ok(Box::new(a_draft("#7"))),
        },
        &mut model,
        &mut ui,
    );
    // Everything the overlay offers short of approving: open the editor, walk
    // the tasks, take one out, put it back, close the editor.
    for c in ['e', 'j', 'k', ' ', ' ', 'e'] {
        press(&mut ui, &model, c, &mut sent);
    }
    assert!(
        !sent
            .iter()
            .any(|input| matches!(input, WorkspaceInput::IssueStartWork { .. })),
        "the draft is a read — no key but `a` may start work: {sent:?}"
    );
    assert!(
        sent.iter().all(|input| matches!(
            input,
            WorkspaceInput::IssueDraftPlan { .. } | WorkspaceInput::IssuesRefresh { .. }
        )),
        "and the only requests it made were reads: {sent:?}"
    );

    press(&mut ui, &model, 'a', &mut sent);
    let approvals: Vec<&WorkspaceInput> = sent
        .iter()
        .filter(|input| matches!(input, WorkspaceInput::IssueStartWork { .. }))
        .collect();
    match approvals.as_slice() {
        [WorkspaceInput::IssueStartWork { key, tasks, .. }] => {
            assert_eq!(key, "#7");
            assert_eq!(
                tasks,
                &vec![
                    "read the seen-set write path".to_string(),
                    "persist the digest set".to_string(),
                ],
                "the approval carries the plan the human is looking at"
            );
        }
        other => panic!("expected exactly one approval, got {other:?}"),
    }
    assert_eq!(
        ui.issues.mode,
        IssuesMode::Browse,
        "the overlay closes on a"
    );
}

/// `x` and `esc` both leave without approving, and say so.
#[test]
fn cancelling_the_draft_starts_nothing() {
    let mut model = WorkspaceModel::new();
    let mut ui = issues_ui();
    drafted(&mut model, &mut ui);
    let action = handle_deck_key(ch('x'), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled);
    assert_eq!(ui.issues.mode, IssuesMode::Browse);
    assert!(ui.issues.start_work.draft.is_none());
    assert_eq!(
        ui.issues.notice.as_deref(),
        Some("start work cancelled — nothing ran")
    );

    drafted(&mut model, &mut ui);
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(ui.issues.mode, IssuesMode::Browse);
}

/// The edit is what the approval sends: a task taken out with `e`/Space is
/// absent from the request, not merely greyed on screen.
#[test]
fn a_task_taken_out_never_reaches_the_approval() {
    let mut model = WorkspaceModel::new();
    let mut ui = issues_ui();
    drafted(&mut model, &mut ui);
    handle_deck_key(ch('e'), &model, &mut ui);
    handle_deck_key(ch(' '), &model, &mut ui);
    match handle_deck_key(ch('a'), &model, &mut ui) {
        DeckAction::Send(WorkspaceInput::IssueStartWork { tasks, .. }) => {
            assert_eq!(tasks, vec!["persist the digest set".to_string()]);
        }
        other => panic!("expected an approval, got {other:?}"),
    }
}

/// An approval with nothing left to do opens no branch.
#[test]
fn an_emptied_plan_is_refused_rather_than_approved() {
    let mut model = WorkspaceModel::new();
    let mut ui = issues_ui();
    drafted(&mut model, &mut ui);
    handle_deck_key(ch('e'), &model, &mut ui);
    handle_deck_key(ch(' '), &model, &mut ui);
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    handle_deck_key(ch(' '), &model, &mut ui);
    assert_eq!(
        handle_deck_key(ch('a'), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(
        ui.issues.mode,
        IssuesMode::StartWork,
        "the overlay stays up"
    );
    let error = ui.issues.start_work.error.clone().unwrap_or_default();
    assert!(error.contains("nothing to approve"), "{error}");
}

/// `a` before the driver has answered starts nothing either — there is no
/// plan to approve yet.
#[test]
fn approving_before_the_draft_lands_starts_nothing() {
    let model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.issues.rows = vec![a_issue("#7")];
    ui.issues.loaded = true;
    handle_deck_key(ch('w'), &model, &mut ui);
    assert_eq!(
        handle_deck_key(ch('a'), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(ui.issues.mode, IssuesMode::StartWork);
    assert!(
        ui.issues
            .start_work
            .error
            .as_deref()
            .is_some_and(|e| e.contains("has not arrived"))
    );
}

/// A draft that arrives after the human closed the overlay does not re-open
/// it, and a stale seq never overwrites a newer draft.
#[test]
fn a_late_draft_reopens_nothing_and_a_stale_one_is_dropped() {
    let mut model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.issues.rows = vec![a_issue("#7")];
    ui.issues.loaded = true;
    handle_deck_key(ch('w'), &model, &mut ui);
    let seq = ui.issues.start_work.wait;
    handle_deck_key(ch('x'), &model, &mut ui);
    ingest_inbound(
        &Inbound::IssueDraft {
            seq,
            outcome: Ok(Box::new(a_draft("#7"))),
        },
        &mut model,
        &mut ui,
    );
    assert_eq!(ui.issues.mode, IssuesMode::Browse);
    assert!(ui.issues.start_work.draft.is_none());

    drafted(&mut model, &mut ui);
    ingest_inbound(
        &Inbound::IssueDraft {
            seq: ui.issues.start_work.wait - 1,
            outcome: Err("an older request failed".into()),
        },
        &mut model,
        &mut ui,
    );
    assert!(
        ui.issues.start_work.error.is_none(),
        "an older lane's failure never lands on a newer draft"
    );
}

/// A failed draft is shown in the overlay rather than swallowed.
#[test]
fn a_failed_draft_lands_on_the_overlay() {
    let mut model = WorkspaceModel::new();
    let mut ui = issues_ui();
    ui.issues.rows = vec![a_issue("#7")];
    ui.issues.loaded = true;
    handle_deck_key(ch('w'), &model, &mut ui);
    let seq = ui.issues.start_work.wait;
    ingest_inbound(
        &Inbound::IssueDraft {
            seq,
            outcome: Err("no tracker connected".into()),
        },
        &mut model,
        &mut ui,
    );
    assert_eq!(
        ui.issues.start_work.error.as_deref(),
        Some("no tracker connected")
    );
    assert_eq!(ui.issues.mode, IssuesMode::StartWork);
}
