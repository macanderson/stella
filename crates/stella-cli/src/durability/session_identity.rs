use super::*;

#[test]
fn captured_engine_binding_survives_switch_and_failed_journal_open() {
    let store = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let handle = SessionDurability::default();
    assert!(bind_session_in(&handle, store.path(), workspace.path(), "ses-first").is_none());
    let (first_context, first_sink) = handle.engine_binding();
    let first_sink = first_sink.expect("the first record opened");
    first_sink.persist("first checkpoint");

    let blocked_store = store.path().join("blocked");
    std::fs::write(&blocked_store, "a file cannot hold a journal").unwrap();
    assert!(bind_session_in(&handle, &blocked_store, workspace.path(), "ses-second").is_some());
    let (second_context, second_sink) = handle.engine_binding();
    assert_eq!(second_context.unwrap().session_id, "ses-second");
    assert!(second_sink.is_none());
    assert_eq!(first_context.unwrap().session_id, "ses-first");
    first_sink.persist("first session continues");
    assert_eq!(WorkJournal::open_in(store.path(), workspace.path(), "ses-first")
        .unwrap().checkpoint().as_deref(), Some("first session continues"));
}

#[test]
fn lane_context_and_hook_keep_the_exact_bound_key() {
    let handle = SessionDurability::default();
    handle.begin_session("lane-ses-parent-research-42");
    let (context, _) = handle.engine_binding();
    let hook = stella_core::hooks::HookPayload::session_start("/workspace")
        .with_session_context(context.as_ref());
    assert_eq!(hook.session_id.as_deref(), Some("lane-ses-parent-research-42"));
    handle.begin_session("lane-ses-parent-review-43");
    assert_eq!(context.unwrap().session_id, "lane-ses-parent-research-42");
    assert_eq!(handle.session_context().unwrap().session_id, "lane-ses-parent-review-43");
}
