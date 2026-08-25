//! The volatile block's bytes, pinned (#3349).
//!
//! The steering migration's whole contract is "same selection, same bytes":
//! routing the record channel, the skills, and the recalled frames through
//! [`stella_core::steering::SteeringPlane`] is a refactor, and this golden is
//! what makes that claim evidence instead of a sentence in a PR description.
//! A fixture with one of each source renders the assembled block, and the
//! assertion is byte-for-byte — a heading that moved, a separator that
//! doubled, or a section that swapped places fails here even though every
//! `contains` assertion in the suite would still pass.
//!
//! Two spliced values keep it honest rather than tautological: the memory id
//! (`nod_…`, deterministic but digest-shaped) and the date line (genuinely
//! volatile — computed at both ends of the call so a UTC-midnight crossing
//! cannot flake the run). Everything else is a literal.

use crate::memory::*;

/// One record, one skill, one recallable memory — the three context sources
/// #3349 migrates. The prompt below selects all three.
fn fixture() -> (tempfile::TempDir, SessionMemory) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".stella")).unwrap();
    let skill_dir = dir.path().join(".stella/skills/reviewer");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: reviewer\ndescription: database review\n---\nALWAYS_REVIEW_DATABASES",
    )
    .unwrap();
    let mut memory =
        SessionMemory::open_with_workspace_skills(dir.path(), false, true).expect("session memory");
    let record_file = stella_core::rules::RuleFile {
        path: ".stella/rules/ctx.acme.staging.toml".to_string(),
        contents: r#"
schema = "context-record/v0.1"
set_id = "acme"

[[record]]
lineage_id = "ctx.acme.staging-url"
kind = "preference"
statement = "The staging URL is https://stage.example."
status = "active"
origin = "user"

[record.steering]
force = "may"
"#
        .to_string(),
        contributed_by: None,
    };
    memory.set_record_registry(stella_core::records::registry::load(
        &[],
        &[record_file],
        &stella_core::records::Facts::default(),
    ));
    (dir, memory)
}

/// **Golden (#3349).** The assembled volatile block, byte-for-byte.
#[tokio::test]
async fn the_assembled_volatile_block_is_byte_identical_to_the_pinned_bytes() {
    let (_dir, memory) = fixture();
    let lesson = "always use the frobnicator for database migrations";
    memory
        .store
        .upsert(ContextDelta {
            memories: vec![MemoryInput::reflection(lesson, Vec::<String>::new())],
            ..ContextDelta::default()
        })
        .await
        .unwrap();

    let frames = memory
        .recalled_frames_reporting(lesson, |_| {})
        .await
        .frames;
    let nod = frames
        .iter()
        .find_map(|frame| frame.id.clone())
        .expect("the fixture memory recalls with its nod_… id");

    // The date section is computed on both sides of the render so a run that
    // crosses UTC midnight between the two reads accepts either day.
    let before = render_today_section(unix_now_secs());
    let block = memory
        .recall_block_reported("review the database migrations", &[])
        .await
        .text
        .expect("all three sources render");
    let after = render_today_section(unix_now_secs());

    let expected_with = |today: &str| {
        format!(
            "{RECALL_MARKER}\n\n\
             Relevant context:\n\
             - [{nod}] {lesson}\n\n\
             \n## Applicable skills (selected for this task — apply the relevant ones)\n\
             \n### reviewer\ndatabase review\n\nALWAYS_REVIEW_DATABASES\n\n\n\
             ## Relevant context\n\
             - The staging URL is https://stage.example. ^staging-url\n\n\
             {today}"
        )
    };
    assert!(
        block == expected_with(&before) || block == expected_with(&after),
        "the volatile block's bytes moved:\n--- got ---\n{block}\n--- want ---\n{}",
        expected_with(&before)
    );
}
