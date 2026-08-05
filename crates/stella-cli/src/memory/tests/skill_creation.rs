//! Spec §8: auto-creation must never clobber a hand-edited file (#737).
//!
//! The guarantee lives in `docs/design/adaptive-context/adaptive-context.md` §8. Its two pure
//! halves — the per-session cap and the no-clobber comparison itself — are
//! tested in `stella-core/src/skills.rs`, and tombstone suppression is tested
//! in `stella-store/src/forget/tests.rs`. What could not be tested there is the
//! part that was actually broken: `stella-core` only ever sees the path list
//! this crate hands it, and this crate used to build that list from the skills
//! that LOADED rather than the files that EXIST. These tests own that seam.

use crate::memory::*;

/// A mining log holding three occurrences of each lesson —
/// `SkillMineConfig::default()` requires `min_occurrences: 3` before a cluster
/// is worth a skill. Returns the log path `auto_create_skills` reads.
fn mining_log(root: &Path, lessons: &[&str]) -> PathBuf {
    let dir = root.join(".stella").join("private");
    std::fs::create_dir_all(&dir).expect("private dir");
    let mut out = String::new();
    let mut occurred_at = 1_000u64;
    for lesson in lessons {
        for _ in 0..3 {
            occurred_at += 1;
            let line = serde_json::json!({
                "lesson": lesson,
                "domains": [],
                "occurred_at": occurred_at,
            });
            out.push_str(&line.to_string());
            out.push('\n');
        }
    }
    let path = dir.join("reflections.jsonl");
    std::fs::write(&path, out).expect("mining log");
    path
}

/// Every `*.md` actually sitting in the workspace skills dir, sorted.
fn skill_files_on_disk(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root.join(".stella").join("skills")) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "md"))
        .collect();
    files.sort();
    files
}

/// Mine `log` in a fresh session — a new `SessionMemory` each time, so the
/// per-session creation counter starts at zero exactly as it would tomorrow.
fn mine_in_a_fresh_session(root: &Path, log: &Path, workspace_skills: bool) {
    let mut memory = SessionMemory::open_with_workspace_skills(root, false, workspace_skills)
        .expect("session memory");
    memory.auto_create_skills(log, true);
}

/// THE regression (#737). A skill disabled from the SKILLS tab keeps its file
/// on disk by design and drops out of `load_skills()`. Feeding that loaded list
/// to the no-clobber guard meant the guard could not see the file, and because
/// mined identity is a stable `{slug}-{hash8}`, the next recurrence of the same
/// lesson re-targeted the exact path the user had edited and `std::fs::write`
/// destroyed it — silently, with no prompt, no backup, and no version history.
///
/// This is the reachable trigger: disabling a skill is an ordinary supported
/// action needing no unusual configuration.
#[test]
fn a_disabled_hand_edited_skill_is_never_overwritten_by_auto_creation() {
    const LESSON: &str =
        "always run database migrations inside a transaction with an explicit lock timeout";
    let dir = tempfile::tempdir().expect("workspace");
    let root = dir.path();
    let log = mining_log(root, &[LESSON]);

    // Session one mines the lesson and auto-creates the skill.
    mine_in_a_fresh_session(root, &log, true);
    let created = skill_files_on_disk(root);
    assert_eq!(created.len(), 1, "the recurring lesson produced one skill");
    let skill_path = created[0].clone();
    let name = skill_path
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("skill file stem")
        .to_string();

    // The user rewrites the body and then disables the skill. The frontmatter
    // name is kept because that is what the SKILLS tab disables by.
    let hand_edited = format!(
        "---\nname: {name}\ndescription: my own notes\n---\n\n\
         Hand-written by the user. Losing this is the bug.\n"
    );
    std::fs::write(&skill_path, &hand_edited).expect("hand edit");
    crate::skill_manager::set_enabled(stella_tui::SkillScope::Project, &name, false, root)
        .expect("disable from the SKILLS tab");

    // Precondition — without this the test would prove nothing: the file is on
    // disk, and it is absent from the loaded list the guard used to be given.
    assert!(skill_path.exists(), "disabling keeps the file on disk");
    assert!(
        !load_workspace_skills_with_authority(root, true)
            .skills
            .iter()
            .any(|s| s.source_path == skill_path.display().to_string()),
        "a disabled skill is excluded from the loaded list"
    );

    // A later session re-mines the identical cluster onto the identical path.
    mine_in_a_fresh_session(root, &log, true);

    assert_eq!(
        std::fs::read_to_string(&skill_path).expect("the file still exists"),
        hand_edited,
        "auto-creation overwrote a hand-edited skill that was merely disabled"
    );
}

/// Reading and writing the workspace skills directory are one authority.
/// Auto-creation used to compute its target from `workspace_skills_dir()`
/// unconditionally, so a session forbidden from READING project prompts still
/// WROTE mined prose into that directory — where the same flag guaranteed it
/// would never be loaded back.
#[test]
fn auto_creation_never_writes_into_a_skills_dir_the_session_may_not_read() {
    const LESSON: &str = "pin the toolchain version in continuous integration so a floating minor cannot change builds";
    let dir = tempfile::tempdir().expect("workspace");
    let root = dir.path();
    let log = mining_log(root, &[LESSON]);

    mine_in_a_fresh_session(root, &log, false);
    assert!(
        skill_files_on_disk(root).is_empty(),
        "a session that may not read workspace skills must not write one"
    );

    // The control: the very same log DOES create a skill once the session is
    // allowed the workspace scope, so the assertion above is about authority
    // and not about the log failing to mine.
    mine_in_a_fresh_session(root, &log, true);
    assert_eq!(skill_files_on_disk(root).len(), 1);
}

/// An untrusted workspace withholds the skill FILE, not the whole lifecycle.
///
/// The authority gate used to sit on the mining dispatcher, so a workspace
/// without project trust skipped attribution, retirement, observation
/// extraction and proposal induction along with the file write. Those write
/// only to the session's own private store — which the very same turn has
/// already written reflection memories to — so skipping them protected nothing
/// and left `context_records` permanently empty in every fresh checkout, every
/// sandbox, and every CI job. Reflection kept mining lessons that went nowhere,
/// and nothing reported a problem.
#[test]
fn an_untrusted_workspace_still_extracts_observations_even_though_no_skill_lands() {
    const LESSON: &str = "pin the toolchain version in continuous integration so a floating minor cannot change builds";
    let dir = tempfile::tempdir().expect("workspace");
    let root = dir.path();
    let log = mining_log(root, &[LESSON]);

    mine_in_a_fresh_session(root, &log, false);

    assert!(
        skill_files_on_disk(root).is_empty(),
        "the file write stays gated: no skill in a workspace we may not read"
    );

    let db = stella_store::workspace_private_sqlite_path(root, "context.db").expect("db path");
    let store = stella_context::ContextStore::open(db).expect("context store opens");
    let observations = crate::memory::observations::all_observations(&store, 100);
    assert!(
        !observations.is_empty(),
        "the ledger half of the lifecycle must run: an untrusted workspace \
         still turns reflection lessons into observations, or nothing \
         downstream of reflection can ever happen"
    );
}

/// The guarantees that already held must still hold, now that the guard is fed
/// a different list: the per-session cap, and the ordinary case of not
/// clobbering a skill that IS loaded.
#[test]
fn auto_creation_still_caps_each_session_and_spares_loaded_skills() {
    const LESSONS: [&str; 3] = [
        "always run database migrations inside a transaction with an explicit lock timeout",
        "prefer structured logging over printf debugging when tracing request flow",
        "check the response status before parsing the body in every http client wrapper",
    ];
    let dir = tempfile::tempdir().expect("workspace");
    let root = dir.path();
    let log = mining_log(root, &LESSONS);

    // `AutoCreateConfig::default().max_per_session` is 2: three qualifying
    // clusters, two files. Auto-creation must feel magical, not spammy.
    let mut memory =
        SessionMemory::open_with_workspace_skills(root, false, true).expect("session memory");
    memory.auto_create_skills(&log, true);
    assert_eq!(skill_files_on_disk(root).len(), 2, "per-session cap");
    // Mining again inside the SAME session stays capped.
    memory.auto_create_skills(&log, true);
    assert_eq!(skill_files_on_disk(root).len(), 2, "the cap is per session");
    drop(memory);

    // Hand-edit one of the two, leaving it enabled and therefore loaded.
    let first = skill_files_on_disk(root)[0].clone();
    let name = first
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("skill file stem")
        .to_string();
    let hand_edited =
        format!("---\nname: {name}\ndescription: my own notes\n---\n\nEdited, still enabled.\n");
    std::fs::write(&first, &hand_edited).expect("hand edit");

    // Tomorrow's session: the cap resets, so the third lesson lands — and the
    // edited file is untouched.
    mine_in_a_fresh_session(root, &log, true);
    assert_eq!(
        skill_files_on_disk(root).len(),
        3,
        "a fresh session creates the third skill"
    );
    assert_eq!(
        std::fs::read_to_string(&first).expect("the file still exists"),
        hand_edited,
        "a loaded skill's file must never be rewritten either"
    );
}

/// Tombstone filtering still gates auto-creation: a forgotten lesson must not
/// come back as a skill, even though the append-only mining log still contains
/// every line written before the tombstone existed.
#[test]
fn a_forgotten_lesson_still_cannot_return_as_an_auto_created_skill() {
    const LESSON: &str =
        "cache the compiled regular expression instead of rebuilding it on every call";
    let dir = tempfile::tempdir().expect("workspace");
    let root = dir.path();
    let log = mining_log(root, &[LESSON]);

    stella_store::Store::open(root)
        .expect("store")
        .forget(
            stella_store::ContextSurface::Memory,
            "nod_forgotten",
            LESSON,
            "the user forgot it",
        )
        .expect("forget");

    mine_in_a_fresh_session(root, &log, true);
    assert!(
        skill_files_on_disk(root).is_empty(),
        "a tombstoned lesson must not be resurrected as a skill"
    );
}
