//! The volatile record channel, end to end: a `may`/`info` record on disk
//! reaching the recall block of a session the constructor opened.
//!
//! Its own module rather than more of `memory/tests.rs` because it tests a
//! different seam — not what recall *renders*, but which sessions are wired to
//! receive it at all. That distinction is the whole bug these pin (epic #897).

use super::*;

/// A `may` record planted in the workspace, on the volatile channel by force.
fn plant_volatile_record(root: &std::path::Path) {
    let dir = root.join(crate::context_records::RULES_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ctx.acme.staging.toml"),
        r#"
schema = "context-record/v0.1"
set_id = "acme"

[defaults]
sharing_scope = "repository"
origin = "user"
status = "active"

[[record]]
lineage_id = "ctx.acme.staging-url"
kind = "preference"
statement = "The staging deploy answers on port 8788."

[record.steering]
force = "may"
"#,
    )
    .unwrap();
}

/// Attaching the volatile record channel is the constructor's job, not the
/// driver's.
///
/// It used to be a second call every session surface had to remember, and
/// forgetting it had no failure mode anyone could see: recall still returned a
/// block, just without the `may`/`info` records and anything the truth sweep
/// demoted. Three of the five surfaces forgot it that way — the Command Deck
/// (the surface most users actually touch), `/goal`, and `stella run`'s
/// single-turn path — so those sessions ran with facts that were on disk,
/// parsed, rendered, and then dropped on the floor.
///
/// Folding it into [`SessionMemory::open_for_session`] is what makes this
/// testable at all: there is now one door to check instead of five call sites
/// to keep in sync.
#[tokio::test]
async fn open_for_session_carries_volatile_records_into_the_recall_block() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    plant_volatile_record(root);

    let authority = crate::settings::AuthorityPolicy {
        project_prompts_allowed: true,
        ..Default::default()
    };
    let active_rules = crate::rules::load_workspace_rules(root, &authority);

    // The control. If the registry rendered nothing on the volatile channel the
    // assertion below would be checking that an empty string is absent — a pass
    // for entirely the wrong reason.
    assert!(
        active_rules
            .registry()
            .render(stella_core::records::Channel::Volatile, None)
            .text
            .contains("port 8788"),
        "the planted `may` record must render on the volatile channel before \
         this test can say anything about who receives it"
    );

    let memory = SessionMemory::open_for_session(root, false, &authority, &active_rules)
        .expect("session memory opens in a temp workspace");
    let block = memory
        .recall_block("where does staging answer?")
        .await
        .expect("a session holding a volatile record has a recall block");
    assert!(
        block.contains("The staging deploy answers on port 8788."),
        "the volatile record must ride the recall block of every session the \
         constructor opens:\n{block}"
    );
}

/// A scoped record rides only the turns its `applies_to` matches.
///
/// This is the selector half of the channel (the `applies_to` field spent its
/// first year parsed, validated, displayed by `stella context explain` — and
/// never consulted). The same session, two prompts: the record scoped to
/// `deny.toml`/`install` reaches the turn about installing a dependency and
/// sits out the unrelated one.
#[tokio::test]
async fn a_scoped_record_rides_only_the_turns_its_applies_to_matches() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let rules = root.join(crate::context_records::RULES_DIR);
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(
        rules.join("ctx.acme.licensing.toml"),
        r#"
schema = "context-record/v0.1"
set_id = "acme"

[defaults]
sharing_scope = "repository"
origin = "user"
status = "active"

[[record]]
lineage_id = "ctx.acme.license-allowlist"
kind = "preference"
statement = "New dependencies must clear the deny.toml license allowlist."

[record.steering]
force = "may"
applies_to = { paths = ["deny.toml"], tasks = ["install"] }
"#,
    )
    .unwrap();

    let authority = crate::settings::AuthorityPolicy {
        project_prompts_allowed: true,
        ..Default::default()
    };
    let active_rules = crate::rules::load_workspace_rules(root, &authority);
    let memory = SessionMemory::open_for_session(root, false, &authority, &active_rules)
        .expect("session memory opens in a temp workspace");

    let matching = memory
        .recall_block("install leftpad and update deny.toml")
        .await
        .expect("a matching turn carries the scoped record");
    assert!(
        matching.contains("license allowlist"),
        "a turn naming the scoped path and task must receive the record:\n{matching}"
    );

    assert!(
        memory
            .recall_block("write a haiku about the ocean")
            .await
            .is_none_or(|block| !block.contains("license allowlist")),
        "a turn matching none of the record's scope must not pay for it"
    );
}

/// The same constructor with nothing to say stays quiet.
///
/// The channel is opt-out by emptiness, not by flag: a workspace with no
/// volatile records must not spend a single token on an empty section header,
/// which is the whole reason `turn_record_section` returns `None` for a blank
/// render instead of pushing it.
#[tokio::test]
async fn open_for_session_adds_no_record_section_when_there_are_no_records() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let authority = crate::settings::AuthorityPolicy {
        project_prompts_allowed: true,
        ..Default::default()
    };
    let active_rules = crate::rules::load_workspace_rules(root, &authority);
    let memory = SessionMemory::open_for_session(root, false, &authority, &active_rules)
        .expect("session memory opens in a temp workspace");

    assert!(
        memory
            .recall_block("where does staging answer?")
            .await
            .is_none_or(|block| !block.contains("port 8788")),
        "an empty registry must contribute nothing to the recall block"
    );
}
