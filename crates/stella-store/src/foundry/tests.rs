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
        disabled_reason: String::new(),
        enabled_authority: None,
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
                error_class: None,
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
                sub_agent_id: None,
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
                .set_foundry_tool_enabled("cat_file", Some(EnableAuthority::InteractiveHuman))
                .expect("enable")
        );
    }
    let reopened = Store::open(dir.path()).expect("reopen");
    let tools = reopened.adopted_foundry_tools().expect("read");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "cat_file");
    assert!(tools[0].enabled, "the human decision must survive");
    assert_eq!(
        tools[0].enabled_authority,
        Some(EnableAuthority::InteractiveHuman),
        "and so must how it was made"
    );
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
        .set_foundry_tool_enabled("cat_file", Some(EnableAuthority::InteractiveHuman))
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

/// **Witness.** Every grant records how it was made, the paths read back
/// as distinct, and withdrawing the grant withdraws the record:
/// `enabled_authority` describes the grant in force, so a row that is off
/// must not keep claiming an approval it lost.
#[test]
fn each_grant_records_its_authority_and_a_disable_clears_it() {
    let store = Store::in_memory().expect("store");
    store
        .adopt_foundry_tool(&adopted("cat_file"))
        .expect("adopt");

    let read = |store: &Store| {
        store
            .adopted_foundry_tool("cat_file")
            .expect("read")
            .expect("present")
            .enabled_authority
    };

    store
        .set_foundry_tool_enabled("cat_file", Some(EnableAuthority::FlagAssertion))
        .expect("enable");
    assert_eq!(read(&store), Some(EnableAuthority::FlagAssertion));

    store
        .set_foundry_tool_enabled("cat_file", Some(EnableAuthority::InteractiveHuman))
        .expect("re-enable");
    assert_eq!(
        read(&store),
        Some(EnableAuthority::InteractiveHuman),
        "the two consent paths are different recorded claims"
    );

    store
        .set_foundry_tool_enabled("cat_file", None)
        .expect("disable");
    assert_eq!(read(&store), None, "no grant, no authority");

    // The breaker's disable withdraws the record the same way.
    store
        .set_foundry_tool_enabled("cat_file", Some(EnableAuthority::Autonomy))
        .expect("enable");
    store
        .disable_foundry_tool_with_reason("cat_file", "3 consecutive failures")
        .expect("breaker");
    assert_eq!(read(&store), None);
}

/// **Witness.** Re-adoption revokes the approval, and the record of how
/// it was made goes with it. New bytes wearing the old grant's record would
/// be the same laundering `re_adopting_revokes_the_approval` refuses.
#[test]
fn re_adopting_clears_the_recorded_authority() {
    let store = Store::in_memory().expect("store");
    store
        .adopt_foundry_tool(&adopted("cat_file"))
        .expect("adopt");
    store
        .set_foundry_tool_enabled("cat_file", Some(EnableAuthority::InteractiveHuman))
        .expect("enable");

    store
        .adopt_foundry_tool(&adopted("cat_file"))
        .expect("re-adopt");
    let row = store
        .adopted_foundry_tool("cat_file")
        .expect("read")
        .expect("present");
    assert!(!row.enabled);
    assert_eq!(row.enabled_authority, None);
}

/// Enabling something that was never adopted reports that, rather than
/// silently succeeding — a no-op that answers "done" is how an approval gets
/// believed for a tool that has none.
#[test]
fn enabling_an_unadopted_tool_says_so() {
    let store = Store::in_memory().expect("store");
    assert!(
        !store
            .set_foundry_tool_enabled("ghost", Some(EnableAuthority::InteractiveHuman))
            .expect("set")
    );
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
        .set_foundry_tool_enabled("gone", Some(EnableAuthority::InteractiveHuman))
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
                error_class: None,
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
                sub_agent_id: None,
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

/// Version history is append-only and counts up per tool; the bytes round-trip
/// exactly, which is the whole rollback contract.
#[test]
fn version_rows_append_and_their_bytes_round_trip() {
    let store = Store::in_memory().expect("store");
    let v1 = store
        .record_foundry_version("cat_file", b"manifest-1", b"script-1", "m1", "s1", "adopt")
        .expect("v1");
    let v2 = store
        .record_foundry_version("cat_file", b"manifest-2", b"script-2", "m2", "s2", "adopt")
        .expect("v2");
    assert_eq!((v1, v2), (1, 2));
    // A second tool starts its own count.
    assert_eq!(
        store
            .record_foundry_version("other", b"m", b"s", "m", "s", "adopt")
            .expect("other"),
        1
    );

    let versions = store.foundry_versions("cat_file").expect("versions");
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].version, 1);
    assert_eq!(versions[1].manifest_digest, "m2");

    let (manifest, script) = store
        .foundry_version_bytes("cat_file", 1)
        .expect("bytes")
        .expect("v1 exists");
    assert_eq!(manifest, b"manifest-1");
    assert_eq!(script, b"script-1");
    assert!(
        store
            .foundry_version_bytes("cat_file", 9)
            .expect("query")
            .is_none(),
        "a version that never existed is None, not an error"
    );
}

/// Invocation telemetry lands one row per launch, and the breaker's window
/// reads newest-first — the order a consecutive-failure count needs.
#[test]
fn invocation_outcomes_read_newest_first() {
    let store = Store::in_memory().expect("store");
    for ok in [true, true, false, false, false] {
        store
            .record_foundry_invocation(&FoundryInvocation {
                name: "cat_file".into(),
                script_digest: "s1".into(),
                gap_id: "g1".into(),
                duration_ms: 5,
                ok,
                timed_out: false,
                output_bytes: 12,
            })
            .expect("record");
    }
    let outcomes = store
        .recent_foundry_outcomes("cat_file", 3)
        .expect("outcomes");
    assert_eq!(outcomes, vec![false, false, false]);
    let wider = store
        .recent_foundry_outcomes("cat_file", 10)
        .expect("outcomes");
    assert_eq!(wider, vec![false, false, false, true, true]);
    assert!(
        store
            .recent_foundry_outcomes("other", 10)
            .expect("outcomes")
            .is_empty()
    );
}

/// The breaker's disable records its reason; a later human enable clears it,
/// and a repin (rollback) clears it while re-pinning the digests.
#[test]
fn a_breaker_disable_records_its_reason_and_a_new_version_clears_it() {
    let store = Store::in_memory().expect("store");
    store
        .adopt_foundry_tool(&adopted("cat_file"))
        .expect("adopt");
    assert!(
        store
            .disable_foundry_tool_with_reason("cat_file", "3 consecutive failures")
            .expect("disable")
    );
    let row = store
        .adopted_foundry_tool("cat_file")
        .expect("read")
        .expect("row");
    assert!(!row.enabled);
    assert_eq!(row.disabled_reason, "3 consecutive failures");

    // Rollback re-pins the digests, re-enables, and clears the verdict.
    assert!(
        store
            .repin_foundry_tool("cat_file", "m9", "s9")
            .expect("repin")
    );
    let row = store
        .adopted_foundry_tool("cat_file")
        .expect("read")
        .expect("row");
    assert!(row.enabled);
    assert_eq!(row.disabled_reason, "");
    assert_eq!(row.manifest_digest, "m9");
    assert_eq!(row.script_digest, "s9");

    // And repin on a name that was never adopted flips nothing.
    assert!(!store.repin_foundry_tool("ghost", "m", "s").expect("repin"));
}

/// The gap detector's feeder: finished bash calls come back oldest-first with
/// their success bit, and rows whose recorded input carries no command are
/// skipped rather than guessed at.
#[test]
fn recent_shell_invocations_read_finished_bash_calls_in_order() {
    let store = Store::in_memory().expect("store");
    let id = store
        .begin_execution("run", "p", "zai", "glm-5.2")
        .expect("execution");
    let row = |seq: i64, args: &str, state: ToolCallState| ToolCallRow {
        error_class: None,
        call_id: format!("c{seq}"),
        name: "bash".into(),
        surface: "native".into(),
        args_json: args.into(),
        args_digest: "d".into(),
        reason: String::new(),
        state,
        error: String::new(),
        bytes_out: 0,
        duration_ms: 1,
        sub_agent_id: None,
    };
    store
        .record_tool_calls(
            id,
            &[
                row(0, r#"{"command":"jq '.a' a.json"}"#, ToolCallState::Ok),
                row(1, r#"{"command":"jq '.b' b.json"}"#, ToolCallState::Error),
                row(2, r#"{"no_command":true}"#, ToolCallState::Ok),
                row(3, r#"{"command":"still running"}"#, ToolCallState::Running),
            ],
        )
        .expect("record");

    let history = store.recent_shell_invocations(50).expect("history");
    assert_eq!(
        history,
        vec![
            ("jq '.a' a.json".to_string(), true),
            ("jq '.b' b.json".to_string(), false),
        ],
        "oldest first, command-less and in-flight rows skipped"
    );
}
