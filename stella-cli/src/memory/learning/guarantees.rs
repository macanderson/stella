//! The wired half of the spec §8 behavior-compatibility contract.
//!
//! Adaptive-context spec §8 lists five guarantees the shipped lexical skills
//! loop makes and the typed loop must not regress. `stella-core`'s
//! `skills::migration_contract` pins the three that are pure functions
//! (candidate identity, rendered bytes, the cap/no-clobber decision). The other
//! two only exist once the loop is wired to a store and a filesystem, and are
//! pinned here:
//!
//! * **tombstone suppression across re-learning, on every surface** — the loop
//!   re-reads the whole append-only reflection log every turn, so lines written
//!   before a tombstone existed are still in it. Filtering at mine time is the
//!   only thing that stops a forgotten lesson coming back as a skill.
//! * **failure isolation** — mining must never fail or slow the user's turn.
//!
//! plus the two end-to-end shapes of the pure guarantees, because a correct
//! decision function that the loop calls with the wrong arguments is still a
//! regression:
//!
//! * the per-session cap, counted across the loop's own accounting;
//! * a hand-edited file surviving a re-mine of the same lesson.
//!
//! These were written against the **pre-migration** loop and must keep passing
//! unchanged afterwards. If one of them has to be edited to accommodate the
//! typed path, that edit is the regression.

use std::path::Path;

use stella_store::ContextSurface;

use crate::memory::{ReflectionLesson, SessionMemory};

/// A workspace with a `.stella/private` mining log containing `lessons`,
/// returned with its `TempDir` guard so the caller controls the lifetime.
fn workspace_with_log(lessons: &[(&str, u64)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let private = dir.path().join(".stella/private");
    std::fs::create_dir_all(&private).expect("private dir");
    let mut log = String::new();
    for (text, at) in lessons {
        let lesson = ReflectionLesson {
            lesson: (*text).to_string(),
            domains: vec!["testing".into()],
            occurred_at: *at,
        };
        log.push_str(&serde_json::to_string(&lesson).expect("serialize lesson"));
        log.push('\n');
    }
    std::fs::write(private.join("reflections.jsonl"), log).expect("write log");
    dir
}

fn log_path(root: &Path) -> std::path::PathBuf {
    root.join(".stella/private/reflections.jsonl")
}

/// Every `.md` under the workspace skills dir, sorted — what the loop actually
/// wrote.
fn skill_files(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join(".stella/skills"))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".md"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Three occurrences of one lesson, which is exactly the shipped threshold.
const RECURRING: &str =
    "Prefer updating witness-test assertions to match the live renderer output.";

fn three_occurrences_of(text: &str) -> Vec<(&str, u64)> {
    vec![(text, 100), (text, 200), (text, 300)]
}

// ---- guarantee: the loop actually mines (the control for everything below) ----

/// Without this, every suppression test below could pass for the wrong reason.
#[test]
fn a_recurring_lesson_becomes_a_skill() {
    let dir = workspace_with_log(&three_occurrences_of(RECURRING));
    let mut memory = SessionMemory::open(dir.path(), false).expect("memory");
    memory.auto_create_skills(&log_path(dir.path()), true);
    assert_eq!(
        skill_files(dir.path()),
        vec!["prefer-updating-witness-test-assertions-e2010443.md"],
        "the control case must mine, or the suppression tests prove nothing"
    );
}

// ---- guarantee: tombstone suppression across re-learning, on every surface ----

/// A lesson forgotten on the `Skill` surface cannot come back as a skill, even
/// though the log lines that produced it predate the tombstone and are still in
/// the append-only log.
#[test]
fn a_lesson_forgotten_as_a_skill_cannot_return_as_a_skill() {
    let dir = workspace_with_log(&three_occurrences_of(RECURRING));
    stella_store::Store::open(dir.path())
        .expect("store")
        .forget(ContextSurface::Skill, "some-skill-id", RECURRING, "no")
        .expect("forget");

    let mut memory = SessionMemory::open(dir.path(), false).expect("memory");
    memory.auto_create_skills(&log_path(dir.path()), true);
    assert!(
        skill_files(dir.path()).is_empty(),
        "a forgotten lesson came back through the miner: {:?}",
        skill_files(dir.path())
    );
}

/// The same, forgotten on the `Memory` surface. Both surfaces feed the sweep,
/// and a user who forgets the *recalled memory* reasonably expects the skill
/// the same lesson would have become to stay gone too.
#[test]
fn a_lesson_forgotten_as_a_memory_cannot_return_as_a_skill() {
    let dir = workspace_with_log(&three_occurrences_of(RECURRING));
    stella_store::Store::open(dir.path())
        .expect("store")
        .forget(ContextSurface::Memory, "nod_whatever", RECURRING, "no")
        .expect("forget");

    let mut memory = SessionMemory::open(dir.path(), false).expect("memory");
    memory.auto_create_skills(&log_path(dir.path()), true);
    assert!(
        skill_files(dir.path()).is_empty(),
        "a memory-surface tombstone did not reach the miner: {:?}",
        skill_files(dir.path())
    );
}

/// Suppression is by **restatement**, not equality: the loop re-learns
/// paraphrases, so a tombstone carrying yesterday's wording must still catch
/// today's. This is the property a plain `DELETE` could never have.
#[test]
fn a_paraphrase_of_a_forgotten_lesson_is_also_suppressed() {
    let paraphrase =
        "Prefer updating witness-test assertions so they match the live renderer's output.";
    let dir = workspace_with_log(&three_occurrences_of(paraphrase));
    stella_store::Store::open(dir.path())
        .expect("store")
        // The tombstone carries the ORIGINAL wording, not the paraphrase.
        .forget(ContextSurface::Skill, "skill-id", RECURRING, "no")
        .expect("forget");

    let mut memory = SessionMemory::open(dir.path(), false).expect("memory");
    memory.auto_create_skills(&log_path(dir.path()), true);
    assert!(
        skill_files(dir.path()).is_empty(),
        "a re-worded lesson slipped past its own tombstone: {:?}",
        skill_files(dir.path())
    );
}

/// A tombstone on a surface that does **not** suppress restatements must not
/// suppress anything by text. Re-authoring a rule by hand is a deliberate act,
/// and swallowing an unrelated mined skill because it resembles a rule the user
/// deleted months ago would be its own bug.
#[test]
fn an_authored_surface_tombstone_does_not_suppress_mining() {
    let dir = workspace_with_log(&three_occurrences_of(RECURRING));
    stella_store::Store::open(dir.path())
        .expect("store")
        .forget(ContextSurface::Rule, "some-rule", RECURRING, "no")
        .expect("forget");

    let mut memory = SessionMemory::open(dir.path(), false).expect("memory");
    memory.auto_create_skills(&log_path(dir.path()), true);
    assert_eq!(
        skill_files(dir.path()).len(),
        1,
        "an authored-surface tombstone must not reach the restatement sweep"
    );
}

/// The sweep enumerates surfaces through `restatement_suppressing()` rather
/// than naming `Memory | Skill` inline. Pin the two together: a newly
/// regenerable surface that updates the predicate but not the sweep would be
/// silently skipped, which is exactly the drift the enumeration exists to stop.
#[test]
fn the_sweep_covers_every_restatement_suppressing_surface() {
    let expected: Vec<ContextSurface> = ContextSurface::all()
        .into_iter()
        .filter(|s| s.suppresses_restatements())
        .collect();
    assert_eq!(ContextSurface::restatement_suppressing(), expected);
    assert_eq!(
        expected,
        vec![ContextSurface::Memory, ContextSurface::Skill],
        "the regenerable surfaces changed — the miner's sweep must be re-checked"
    );
}

// ---- guarantee: the per-session creation cap ----

/// Five independent recurring lessons, one session: exactly two files. The rest
/// wait for the next session's pass. Auto-creation has to feel magical rather
/// than spammy, and a session that silently spawns five skill files is how the
/// mechanism loses the user's trust.
#[test]
fn the_per_session_cap_holds_end_to_end() {
    let mut lessons: Vec<(String, u64)> = Vec::new();
    let texts = [
        "Always run the database migration before the integration suite starts.",
        "Reach for ripgrep instead of grep when searching this repository.",
        "Keep the generated protobuf bindings out of version control entirely.",
        "Terraform plans belong in review before anyone applies them to staging.",
        "Feature flags default closed so an unfinished rollout cannot leak.",
    ];
    for (i, text) in texts.iter().enumerate() {
        for occurrence in 0..3u64 {
            lessons.push(((*text).to_string(), 100 + (i as u64 * 10) + occurrence));
        }
    }
    let borrowed: Vec<(&str, u64)> = lessons.iter().map(|(t, a)| (t.as_str(), *a)).collect();
    let dir = workspace_with_log(&borrowed);

    let mut memory = SessionMemory::open(dir.path(), false).expect("memory");
    memory.auto_create_skills(&log_path(dir.path()), true);
    assert_eq!(
        skill_files(dir.path()).len(),
        2,
        "five eligible candidates, one session, cap of 2: {:?}",
        skill_files(dir.path())
    );

    // A second pass inside the SAME session creates nothing more: the cap is
    // per session, not per mining pass.
    memory.auto_create_skills(&log_path(dir.path()), true);
    assert_eq!(
        skill_files(dir.path()).len(),
        2,
        "the cap survives a re-mine"
    );
}

// ---- guarantee: never clobber a hand-edited file ----

/// The user edits an auto-created skill; the same lesson is still in the log;
/// the next session re-mines it. The file must survive byte-for-byte.
///
/// The edit deliberately replaces the body with unrelated prose, so the miner's
/// own `already_captured` text guard no longer matches and the candidate is
/// genuinely re-minted. That leaves the no-clobber guard as the only thing
/// standing between the log and an overwrite — which is the point: a test where
/// `already_captured` short-circuits first would pass without exercising the
/// guarantee at all.
#[test]
fn a_hand_edited_skill_file_is_never_clobbered() {
    let dir = workspace_with_log(&three_occurrences_of(RECURRING));
    // Workspace skills trusted — the normal configuration, and the only one in
    // which the loop reads the directory it writes to. See
    // `finding_mining_clobbers_hand_edited_skills_when_workspace_skills_are_untrusted`
    // for what happens when it does not.
    let mut first =
        SessionMemory::open_with_workspace_skills(dir.path(), false, true).expect("memory");
    first.auto_create_skills(&log_path(dir.path()), true);
    let created = dir
        .path()
        .join(".stella/skills/prefer-updating-witness-test-assertions-e2010443.md");
    assert!(created.exists(), "the first pass wrote the skill");

    // The user rewrites it, keeping the filename (and so the identity) but
    // replacing the content entirely.
    let hand_edited = "---\n\
        name: prefer-updating-witness-test-assertions-e2010443\n\
        description: My own wording for this convention.\n\
        origin: user\n\
        ---\n\n\
        Deployment freezes begin at noon on Friday and lift Monday morning; \
        coordinate any schema change around that window with the on-call rotation \
        before opening a pull request against the release branch.\n";
    std::fs::write(&created, hand_edited).expect("hand edit");

    // A later session — `skills_created` starts at zero again, so the cap is
    // not what stops the write.
    let mut second =
        SessionMemory::open_with_workspace_skills(dir.path(), false, true).expect("memory");
    second.auto_create_skills(&log_path(dir.path()), true);

    assert_eq!(
        std::fs::read_to_string(&created).expect("read back"),
        hand_edited,
        "the miner overwrote a hand-edited skill file"
    );
    assert_eq!(
        skill_files(dir.path()).len(),
        1,
        "and it did not write a second file beside it: {:?}",
        skill_files(dir.path())
    );
}

/// **FINDING — a pre-existing defect in the shipped lexical loop, pinned as
/// current behavior rather than fixed here.**
///
/// `SessionMemory::open_with_authority` passes `authority.project_prompts_allowed`
/// through as `include_workspace_skills`. That flag governs *reading*
/// `.stella/skills` — and only reading. Mining still *writes* there
/// unconditionally.
///
/// So in a workspace where project prompts are not trusted, `load_skills()`
/// returns nothing from the workspace directory, `existing_paths` is empty, and
/// the no-clobber guard is handed an empty list to check against. It duly
/// decides `Create` for a path that already holds a file, and the write
/// destroys whatever the user had there. The two text guards that might
/// otherwise have caught it (`already_captured` against existing skills, and
/// the `FileExists` check) are both blinded by the same empty list.
///
/// Spec §8 names "never clobbering a hand-edited file" as a guarantee to
/// preserve. It does not hold on this path today. Fixing it is a one-line
/// change — enumerate the skills directory for the no-clobber check regardless
/// of whether its contents are trusted as *context* — but it is a behavior
/// change to the shipped loop and does not belong buried inside a migration
/// commit. Pinned here so the migration cannot silently make it worse, and so
/// the fix, whenever it lands, has to delete this test on purpose.
#[test]
fn finding_mining_clobbers_hand_edited_skills_when_workspace_skills_are_untrusted() {
    let dir = workspace_with_log(&three_occurrences_of(RECURRING));
    // `include_workspace_skills: false` — what `open_with_authority` passes for
    // a workspace whose project prompts are not trusted.
    let mut first =
        SessionMemory::open_with_workspace_skills(dir.path(), false, false).expect("memory");
    first.auto_create_skills(&log_path(dir.path()), true);
    let created = dir
        .path()
        .join(".stella/skills/prefer-updating-witness-test-assertions-e2010443.md");

    let hand_edited = "---\nname: mine\ndescription: my own\n---\n\nmy own content\n";
    std::fs::write(&created, hand_edited).expect("hand edit");

    let mut second =
        SessionMemory::open_with_workspace_skills(dir.path(), false, false).expect("memory");
    second.auto_create_skills(&log_path(dir.path()), true);

    assert_ne!(
        std::fs::read_to_string(&created).expect("read back"),
        hand_edited,
        "the defect is fixed — delete this test and the finding it documents"
    );
}

// ---- guarantee: failure isolation ----

/// Mining must never fail the user's task. Every one of these is a plausible
/// real failure, and each must return normally with the loop simply doing
/// nothing.
#[test]
fn mining_failures_are_isolated() {
    // 1. No mining log at all (a workspace that has never reflected).
    let dir = tempfile::tempdir().expect("tempdir");
    let mut memory = SessionMemory::open(dir.path(), false).expect("memory");
    memory.auto_create_skills(&log_path(dir.path()), true);

    // 2. A log full of garbage that is not JSON.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".stella/private")).expect("private");
    std::fs::write(log_path(dir.path()), "not json\n{ broken\n\0\n").expect("write");
    let mut memory = SessionMemory::open(dir.path(), false).expect("memory");
    memory.auto_create_skills(&log_path(dir.path()), true);

    // 3. The skills directory is a regular FILE, so every write under it fails.
    let dir = workspace_with_log(&three_occurrences_of(RECURRING));
    std::fs::create_dir_all(dir.path().join(".stella")).expect("stella dir");
    std::fs::write(dir.path().join(".stella/skills"), "i am not a directory").expect("write");
    let mut memory = SessionMemory::open(dir.path(), false).expect("memory");
    memory.auto_create_skills(&log_path(dir.path()), true);

    // 4. The tombstone store is corrupt — unreadable suppression state must not
    //    take the turn down with it.
    let dir = workspace_with_log(&three_occurrences_of(RECURRING));
    let mut memory = SessionMemory::open(dir.path(), false).expect("memory");
    for entry in std::fs::read_dir(dir.path().join(".stella/private"))
        .expect("private")
        .flatten()
    {
        if entry.path().extension().is_some_and(|e| e == "db") {
            std::fs::write(entry.path(), b"this is not a sqlite database").expect("corrupt");
        }
    }
    memory.auto_create_skills(&log_path(dir.path()), true);
}
