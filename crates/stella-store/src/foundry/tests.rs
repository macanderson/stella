//! The adoption ledger's own proof: an approval survives a restart, a
//! re-adoption revokes it, and the reuse metric counts only what happened
//! after adoption.

use super::*;
use crate::{ToolCallRow, ToolCallState};

fn adopted(name: &str) -> AdoptedTool {
    AdoptedTool {
        name: name.to_string(),
        signature: "cat <path>".into(),
        manifest_digest: "m0".into(),
        script_digest: "s0".into(),
        witness: "proven — output contains `alpha-contents`".into(),
        witness_input: r#"{"p1":"a.txt"}"#.into(),
        witness_expect: "alpha-contents".into(),
        enabled: false,
        adopted_at: String::new(),
    }
}

fn call(store: &Store, name: &str, ok: bool) {
    let id = store
        .begin_execution("run", "p", "zai", "glm-5.2")
        .expect("execution");
    store
        .record_tool_calls(
            id,
            &[ToolCallRow {
                call_id: format!("c-{name}-{ok}-{id}"),
                name: name.into(),
                surface: "native".into(),
                args_json: "{}".into(),
                args_digest: "d".into(),
                reason: String::new(),
                state: if ok {
                    ToolCallState::Ok
                } else {
                    ToolCallState::Error
                },
                error: String::new(),
                bytes_out: 0,
                duration_ms: 1,
            }],
        )
        .expect("record");
}

/// The claim the whole table exists for: adopt, close the workspace, reopen —
/// the approval is still there. A manifest on disk survives a restart on its
/// own; "this was proven and approved" does not, without this.
#[test]
fn an_adoption_survives_a_restart() {
    let dir = tempfile::tempdir().expect("tmp");
    {
        let store = Store::open(dir.path()).expect("store");
        store
            .adopt_foundry_tool(&adopted("cat_file"))
            .expect("adopt");
        assert!(
            store
                .set_foundry_tool_enabled("cat_file", true)
                .expect("enable")
        );
    }
    let reopened = Store::open(dir.path()).expect("reopen");
    let tools = reopened.adopted_foundry_tools().expect("read");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "cat_file");
    assert!(tools[0].enabled, "the human decision must survive");
    assert_eq!(tools[0].witness_expect, "alpha-contents");
    assert_eq!(tools[0].witness_input, r#"{"p1":"a.txt"}"#);
    assert!(
        !tools[0].adopted_at.is_empty(),
        "the store stamps the adoption time"
    );
}

/// #830's guardrail at the schema level: a fresh adoption is off, whatever the
/// caller asked for. Approval is not something the adopting code can grant.
#[test]
fn adoption_always_lands_disabled() {
    let store = Store::in_memory().expect("store");
    let mut eager = adopted("eager");
    eager.enabled = true; // the caller tries to adopt-and-enable in one step
    store.adopt_foundry_tool(&eager).expect("adopt");
    let stored = store
        .adopted_foundry_tool("eager")
        .expect("read")
        .expect("present");
    assert!(!stored.enabled, "a writer cannot grant its own approval");
}

/// Re-adoption is a *replacement*, and a replacement revokes. The approval was
/// made about specific bytes; new bytes need a new decision, or an edit rides
/// in on a judgement that was never about it.
#[test]
fn re_adopting_revokes_the_approval() {
    let store = Store::in_memory().expect("store");
    store
        .adopt_foundry_tool(&adopted("cat_file"))
        .expect("adopt");
    store
        .set_foundry_tool_enabled("cat_file", true)
        .expect("enable");

    let mut edited = adopted("cat_file");
    edited.script_digest = "s1-different".into();
    store.adopt_foundry_tool(&edited).expect("re-adopt");

    let stored = store
        .adopted_foundry_tool("cat_file")
        .expect("read")
        .expect("present");
    assert!(!stored.enabled, "new bytes need a new approval");
    assert_eq!(stored.script_digest, "s1-different");
}

/// Enabling something that was never adopted reports that, rather than
/// silently succeeding — a no-op that answers "done" is how an approval gets
/// believed for a tool that has none.
#[test]
fn enabling_an_unadopted_tool_says_so() {
    let store = Store::in_memory().expect("store");
    assert!(!store.set_foundry_tool_enabled("ghost", true).expect("set"));
    assert!(store.adopted_foundry_tool("ghost").expect("read").is_none());
}

/// Forgetting an adoption removes the approval with it, so deleting a tool's
/// files cannot leave a live grant behind for the next thing to claim the
/// name.
#[test]
fn forgetting_an_adoption_removes_its_approval() {
    let store = Store::in_memory().expect("store");
    store.adopt_foundry_tool(&adopted("gone")).expect("adopt");
    store
        .set_foundry_tool_enabled("gone", true)
        .expect("enable");
    assert!(store.forget_foundry_tool("gone").expect("forget"));
    assert!(store.adopted_foundry_tools().expect("read").is_empty());
    assert!(
        !store.forget_foundry_tool("gone").expect("forget again"),
        "forgetting twice is not a second removal"
    );
}

/// The #830 success metric, folded straight off the receipts: calls, failures,
/// and last use per adopted tool.
#[test]
fn reuse_is_counted_off_the_receipt_log() {
    let store = Store::in_memory().expect("store");
    store
        .adopt_foundry_tool(&adopted("cat_file"))
        .expect("adopt");
    store
        .adopt_foundry_tool(&adopted("never_used"))
        .expect("adopt");
    call(&store, "cat_file", true);
    call(&store, "cat_file", true);
    call(&store, "cat_file", false);
    // A call to something that is not an adopted tool must not appear.
    call(&store, "grep", true);

    let reuse = store.foundry_reuse().expect("reuse");
    assert_eq!(reuse.len(), 2, "one row per adoption, not per call");

    let used = reuse.iter().find(|r| r.tool.name == "cat_file").unwrap();
    assert_eq!(used.calls, 3);
    assert_eq!(used.errors, 1);
    assert!(used.last_used.is_some());
    assert!(!used.is_false_start());

    // The cost column: authored, proven, never reached for.
    let cold = reuse.iter().find(|r| r.tool.name == "never_used").unwrap();
    assert_eq!(cold.calls, 0);
    assert!(
        cold.is_false_start(),
        "a false start must be visible as one"
    );
    assert_eq!(cold.last_used, None);
}

/// Calls that predate the adoption are not the adopted tool's. A foundry tool
/// is named after the shell shape it replaces, so a workspace may well have
/// had something by that name before — crediting those would make the
/// self-improvement metric wrong in the flattering direction.
#[test]
fn calls_made_before_adoption_are_not_credited() {
    let store = Store::in_memory().expect("store");
    call(&store, "cat_file", true);
    call(&store, "cat_file", true);
    // SQLite's CURRENT_TIMESTAMP has one-second resolution, so an adoption in
    // the same second as the prior calls would tie rather than sort after.
    // Stamp the adoption into the future to express "strictly afterwards"
    // without sleeping a second in a unit test.
    store
        .adopt_foundry_tool(&adopted("cat_file"))
        .expect("adopt");
    {
        let conn = store.lock();
        conn.execute(
            "UPDATE foundry_tools SET adopted_at = '2099-01-01 00:00:00' WHERE name = 'cat_file'",
            [],
        )
        .expect("restamp");
    }

    let reuse = store.foundry_reuse().expect("reuse");
    assert_eq!(reuse[0].calls, 0, "prior history is not adoption-era reuse");
    assert!(reuse[0].is_false_start());
}

/// An in-flight call is not yet a use. Counting `running` rows would credit a
/// tool for a call that may still fail.
#[test]
fn an_in_flight_call_is_not_yet_a_use() {
    let store = Store::in_memory().expect("store");
    store
        .adopt_foundry_tool(&adopted("cat_file"))
        .expect("adopt");
    let id = store.begin_execution("run", "p", "z", "m").expect("exec");
    store
        .record_tool_calls(
            id,
            &[ToolCallRow {
                call_id: "in-flight".into(),
                name: "cat_file".into(),
                surface: "native".into(),
                args_json: "{}".into(),
                args_digest: "d".into(),
                reason: String::new(),
                state: ToolCallState::Running,
                error: String::new(),
                bytes_out: 0,
                duration_ms: 0,
            }],
        )
        .expect("record");
    assert_eq!(store.foundry_reuse().expect("reuse")[0].calls, 0);
}

/// A workspace with no adoptions reports none — the report resolves on every
/// path rather than depending on the table having been written to.
#[test]
fn an_untouched_workspace_reports_no_adoptions() {
    let store = Store::in_memory().expect("store");
    assert!(store.adopted_foundry_tools().expect("read").is_empty());
    assert!(store.foundry_reuse().expect("reuse").is_empty());
}
