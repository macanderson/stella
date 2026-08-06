//! Skill-version usage telemetry, recorded at the shared execution seam.
//!
//! `Store::record_skill_usage` used to have exactly one caller — the deck's
//! turn dispatch — so `stella run` (the default pipeline path), the raw
//! one-shot, goal runs, and every other headless surface injected skills but
//! wrote no `skill_usage` rows. Every reader of the table — the observatory's
//! Sessions tab, the memory tab's skills aggregate, and skill appraisal
//! (which joins selections to receipts and feeds retirement) — under-counted
//! for exactly the paths that produce most usage, and appraisal could retire
//! a skill for phantom non-usage (#1872).
//!
//! The recorder now lives beside the execution-id stamp every turn-building
//! path already performs, so recording is a property of *beginning an
//! execution with session memory*, not of which front-end ran the turn.
//!
//! Semantics are the deck's, now shared: rows land **once per execution, at
//! turn start** — the skills are injected regardless of how the turn ends,
//! and each path begins its execution exactly once, so the append-only table
//! (no UNIQUE constraint) holds at most one batch per execution by
//! construction. Best-effort like every other telemetry write: a failed
//! insert never fails the turn.

use std::path::Path;

use super::*;

/// Stamp `execution` onto the session memory (the post-turn self-review is
/// stored 1:1 with an execution, so a turn that cannot name its row files an
/// id-less reflection) and record the skills recall selected for `prompt`,
/// at their pinned versions, keyed to that execution.
///
/// Call this **after** [`SessionMemory::arm_recall_control`] has armed the
/// turn: selection honours the A/B recall control, so a control turn that
/// injects no skills records no usage. A missing store, execution, or memory
/// is a quiet no-op — those paths inject no skills either.
pub(crate) fn stamp_execution_and_record_skill_usage(
    execution: &Option<(Arc<Store>, i64)>,
    memory: Option<&mut SessionMemory>,
    prompt: &str,
    workspace_root: &Path,
) {
    let (Some((store, id)), Some(memory)) = (execution, memory) else {
        return;
    };
    memory.set_execution_id(*id);
    let selected = memory.selected_skills(prompt);
    if selected.is_empty() {
        return;
    }
    let versions = crate::skill_manager::pinned_versions(workspace_root);
    let rows: Vec<stella_store::SkillUsageRow> = selected
        .into_iter()
        .map(|(skill, reason)| stella_store::SkillUsageRow {
            version: versions.get(&skill).copied().unwrap_or(1),
            skill,
            reason,
        })
        .collect();
    let _ = store.record_skill_usage(*id, &rows);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace whose skills directory holds one skill that
    /// `select_skills` matches on the prompt "review the database".
    fn workspace_with_reviewer_skill() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let skill_dir = dir.path().join(".stella/skills/reviewer");
        std::fs::create_dir_all(&skill_dir).expect("skill dir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: reviewer\ndescription: database review\n---\nALWAYS_REVIEW_DATABASES",
        )
        .expect("skill file");
        dir
    }

    fn session_memory(root: &Path) -> SessionMemory {
        // Workspace skills ride the same authority switch as project prompts,
        // exactly as `open_for_session` resolves it for every shipped surface.
        let authority = crate::settings::AuthorityPolicy {
            project_prompts_allowed: true,
            ..crate::settings::AuthorityPolicy::default()
        };
        SessionMemory::open_for_session(
            root,
            false,
            &authority,
            &crate::rules::ResolvedRules::default(),
        )
        .expect("session memory")
    }

    /// The #1872 witness: a non-deck execution ("pipeline" kind, the default
    /// `stella run` path) flowing through the shared seam leaves a
    /// `skill_usage` row. On main only the deck's inline recorder wrote one,
    /// so every headless run reported zero skill telemetry.
    #[test]
    fn a_pipeline_shaped_execution_records_skill_usage_at_the_seam() {
        let dir = workspace_with_reviewer_skill();
        let mut memory = session_memory(dir.path());
        let store = Arc::new(Store::in_memory().expect("store"));
        let id = store
            .begin_execution("pipeline", "review the database", "anthropic", "claude")
            .expect("begin");

        stamp_execution_and_record_skill_usage(
            &Some((store.clone(), id)),
            Some(&mut memory),
            "review the database",
            dir.path(),
        );

        let json = store
            .export_all_json()
            .expect("export")
            .into_iter()
            .find_map(|(table, json)| (table == "skill_usage").then_some(json))
            .expect("skill_usage export");
        let rows: Vec<serde_json::Value> = serde_json::from_str(&json).expect("rows");
        assert_eq!(
            rows.len(),
            1,
            "the seam records the selected skill for a non-deck execution"
        );
        assert_eq!(rows[0]["skill"], "reviewer");
        assert_eq!(
            rows[0]["execution_id"].as_i64(),
            Some(id),
            "rows are keyed to the seam's execution"
        );
    }

    /// The seam subsumes the per-path execution-id stamp it replaced: a
    /// reflection following the turn can still name its execution row.
    #[test]
    fn the_seam_stamps_the_execution_id_onto_memory() {
        let dir = workspace_with_reviewer_skill();
        let mut memory = session_memory(dir.path());
        let store = Arc::new(Store::in_memory().expect("store"));
        let id = store
            .begin_execution("goal", "unrelated prompt", "anthropic", "claude")
            .expect("begin");

        stamp_execution_and_record_skill_usage(
            &Some((store.clone(), id)),
            Some(&mut memory),
            "unrelated prompt",
            dir.path(),
        );

        assert_eq!(memory.execution_id_for_test(), Some(id));
        assert_eq!(
            store.count("skill_usage").expect("count"),
            0,
            "a prompt that selects no skill records no phantom usage"
        );
    }

    /// A path with no memory (sub-sessions, resume) injects no skills, so the
    /// seam records nothing — and must not panic reaching for a store.
    #[test]
    fn absent_memory_or_execution_is_a_no_op() {
        let dir = workspace_with_reviewer_skill();
        let store = Arc::new(Store::in_memory().expect("store"));
        let id = store
            .begin_execution("deck-sub", "review the database", "anthropic", "claude")
            .expect("begin");

        stamp_execution_and_record_skill_usage(
            &Some((store.clone(), id)),
            None,
            "review the database",
            dir.path(),
        );
        let mut memory = session_memory(dir.path());
        stamp_execution_and_record_skill_usage(&None, Some(&mut memory), "review", dir.path());

        assert_eq!(store.count("skill_usage").expect("count"), 0);
    }
}
