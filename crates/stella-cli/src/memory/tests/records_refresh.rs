//! Witnesses for mid-session record freshness (`records_refresh`): a record
//! added, retired, or broken while the session runs reaches — or visibly
//! fails to reach — the next recall block, with the registry loaded at
//! session start surviving a half-saved edit.

use crate::memory::*;

fn write_rule(root: &std::path::Path, name: &str, contents: &str) {
    let dir = root.join(".stella").join("rules");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), contents).unwrap();
}

fn record_toml(force: &str, lineage: &str, statement: &str) -> String {
    format!(
        r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[defaults]
origin = "user"
status = "active"

[[record]]
lineage_id = "{lineage}"
kind = "preference"
statement = "{statement}"

[record.steering]
force = "{force}"
"#
    )
}

fn session(root: &std::path::Path) -> SessionMemory {
    SessionMemory::open_with_workspace_skills(root, false, true)
        .expect("session memory opens in a temp workspace")
}

async fn next_block(memory: &SessionMemory) -> Option<String> {
    let signal = stella_core::steering::TurnSignal {
        prompt: "keep the service healthy",
        ..Default::default()
    };
    memory
        .signal_recall_block(
            &signal,
            &crate::memory::steering::ProducedSteering::default(),
        )
        .await
        .text
}

/// **The witness for a mid-session edit.** A record file that lands while
/// the session runs is picked up by the boundary refresh: the generation
/// bumps (which is what forces the one re-query) and the very next recall
/// block carries the record.
#[tokio::test]
async fn a_record_added_mid_session_reaches_the_next_recall_block() {
    let dir = tempfile::tempdir().unwrap();
    let memory = session(dir.path());
    let before = memory.records_generation();
    write_rule(
        dir.path(),
        "acme.web.toml",
        &record_toml(
            "info",
            "ctx.acme.web.staging-url",
            "The staging deploy answers on port 8788.",
        ),
    );
    let text = next_block(&memory).await.expect("the new record renders");
    assert!(text.contains("port 8788"), "{text}");
    assert!(
        memory.records_generation() > before,
        "a swap bumps the generation, so the re-query fingerprint moves"
    );
}

/// **The witness for retirement.** A record file deleted mid-session leaves
/// the volatile channel on the same refresh — suppression is one mechanism
/// in the running session too.
#[tokio::test]
async fn a_record_deleted_mid_session_leaves_the_volatile_channel() {
    let dir = tempfile::tempdir().unwrap();
    write_rule(
        dir.path(),
        "acme.web.toml",
        &record_toml(
            "info",
            "ctx.acme.web.staging-url",
            "The staging deploy answers on port 8788.",
        ),
    );
    let memory = session(dir.path());
    assert!(
        next_block(&memory)
            .await
            .is_some_and(|text| text.contains("port 8788")),
        "the record renders while its file exists"
    );
    std::fs::remove_file(dir.path().join(".stella/rules/acme.web.toml")).unwrap();
    let after = next_block(&memory).await;
    assert!(
        !after.as_deref().unwrap_or_default().contains("port 8788"),
        "a deleted record must stop steering: {after:?}"
    );
}

/// **The witness for a half-saved edit.** A rules file that stops parsing
/// keeps the registry loaded before the edit — the session is not silently
/// un-steered by a buffer mid-save — and the next block says so.
#[tokio::test]
async fn a_rules_file_that_stops_parsing_keeps_the_last_good_registry() {
    let dir = tempfile::tempdir().unwrap();
    write_rule(
        dir.path(),
        "acme.web.toml",
        &record_toml(
            "info",
            "ctx.acme.web.staging-url",
            "The staging deploy answers on port 8788.",
        ),
    );
    let memory = session(dir.path());
    assert!(
        next_block(&memory)
            .await
            .is_some_and(|text| text.contains("port 8788")),
        "the record renders before the bad edit"
    );
    write_rule(
        dir.path(),
        "acme.web.toml",
        "schema = \"context-record/v0.1\"\n[[record]\n",
    );
    let text = next_block(&memory)
        .await
        .expect("the kept registry still renders");
    assert!(
        text.contains("port 8788"),
        "the last good registry stays in effect: {text}"
    );
    assert!(
        text.contains("does not parse"),
        "the failure is reported, never silent: {text}"
    );
}

/// **The witness for the prefix wall.** A pinned (`must`) record that lands
/// mid-session cannot enter the byte-stable system prompt, so the block says
/// exactly what applies now and what binds next session — and says it once.
#[tokio::test]
async fn a_pinned_record_change_is_delivered_as_guidance_with_its_caveat() {
    let dir = tempfile::tempdir().unwrap();
    let memory = session(dir.path());
    write_rule(
        dir.path(),
        "acme.web.toml",
        &record_toml(
            "must",
            "ctx.acme.web.no-force-push",
            "Never force-push to main.",
        ),
    );
    let text = next_block(&memory).await.expect("the note renders a block");
    assert!(
        text.contains("changed while this session runs") && text.contains("Never force-push"),
        "the pinned change rides the volatile channel with its caveat: {text}"
    );
    let again = next_block(&memory).await;
    assert!(
        !again
            .as_deref()
            .unwrap_or_default()
            .contains("changed while this session runs"),
        "the note is one-shot: {again:?}"
    );
}
