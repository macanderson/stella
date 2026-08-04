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
            seq: 1,
        }]
    ));
    // A second visit does not re-fetch (busy / loaded gate).
    handle_deck_key(key(KeyCode::Tab), &model, &mut ui);
    handle_deck_key(key(KeyCode::BackTab), &model, &mut ui);
    assert_eq!(ui.pending_inputs.len(), 1, "no duplicate refresh");
}

#[test]
fn issues_browse_keys_refresh_and_start_work() {
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
        DeckAction::Send(WorkspaceInput::IssueAct { key, action, .. }) => {
            assert_eq!(key, "#7");
            assert_eq!(action, IssueAction::StartWork);
        }
        other => panic!("expected IssueAct, got {other:?}"),
    }
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
