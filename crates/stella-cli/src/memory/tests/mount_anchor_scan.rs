//! The witness for the real mount path. Just opening a session — no
//! `stella memory validate --end-stale` call — ends world validity for
//! an anchor whose file is gone.

use crate::memory::*;

#[tokio::test]
async fn mount_ends_world_validity_for_an_anchor_whose_file_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let context_db = stella_store::workspace_private_sqlite_path(dir.path(), "context.db").unwrap();

    // Seed the anchor first, the way an older session's reflection
    // would have. The open call below is the thing under test, so it
    // must not be what makes the anchor too.
    {
        let context = stella_context::ContextStore::open(&context_db).unwrap();
        context
            .upsert(ContextDelta {
                memories: vec![
                    MemoryInput::reflection("about a file since deleted", Vec::<String>::new())
                        .with_anchors(["gone.rs"]),
                ],
                ..ContextDelta::default()
            })
            .await
            .unwrap();
        assert_eq!(context.open_anchors().unwrap().len(), 1);
    }
    // `gone.rs` is never written. The anchor points at a path that was
    // never in this workspace — the same as a file that got deleted.

    // The real mount path. No `--end-stale` call anywhere in this test.
    drop(SessionMemory::open(dir.path(), false).expect("session memory opens"));

    let context = stella_context::ContextStore::open(&context_db).unwrap();
    assert_eq!(
        context.open_anchors().unwrap().len(),
        0,
        "mounting the session alone ended world validity for the gone file's anchor"
    );

    // The memory itself is still true. Ending an anchor does not
    // retract the lesson it came from.
    assert_eq!(
        context
            .facts_as_of(None)
            .unwrap()
            .iter()
            .filter(|f| f.predicate == stella_context::ANCHOR_REL)
            .count(),
        1,
        "the anchor remains believed; only its world validity ended"
    );
}
