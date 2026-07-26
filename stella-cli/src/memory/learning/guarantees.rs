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
//! ## Every guarantee runs under BOTH loops
//!
//! These were written against the **pre-migration** loop, and the migration
//! kept that loop reachable rather than deleting it: `context.lifecycle.enabled`
//! selects between the lexical path (off, the shipped default) and the typed
//! one. So each guarantee below is asserted twice, once per path, by
//! [`each_path`].
//!
//! That is what makes "behavior-compatible" a checkable claim instead of a
//! promise. A migration that deletes the thing it must stay compatible with has
//! no baseline left to compare against; keeping both runnable means any
//! divergence shows up as one of these tests passing on one path and failing on
//! the other.
//!
//! None of the assertions changed when the typed path landed. If one of them
//! ever has to be edited to accommodate it, that edit is the regression.

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
            task_id: String::new(),
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

/// Which learning loop a case is exercising.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Loop {
    /// `context.lifecycle.enabled = false` — the path that ships today.
    Lexical,
    /// `context.lifecycle.enabled = true` — the typed path.
    Typed,
}

/// Run `case` under both loops, on a fresh workspace each time, and label any
/// failure with the path it happened on.
fn each_path(case: impl Fn(Loop)) {
    for path in [Loop::Lexical, Loop::Typed] {
        case(path);
    }
}

/// Turn the typed loop on for `root` by writing the project-scope settings the
/// session reads at open. Writing real settings rather than reaching into the
/// struct is deliberate: the flag's whole job is to be readable from a user's
/// settings file, and a test that bypassed that would not prove it works.
fn enable_lifecycle(root: &Path) {
    let dir = root.join(".stella");
    std::fs::create_dir_all(&dir).expect("stella dir");
    std::fs::write(
        dir.join("settings.json"),
        r#"{"context":{"lifecycle":{"enabled":true}}}"#,
    )
    .expect("write settings");
}

/// Open a session on `root` running `path`'s loop.
fn session(root: &Path, path: Loop) -> SessionMemory {
    session_with_workspace_skills(root, path, true)
}

fn session_with_workspace_skills(
    root: &Path,
    path: Loop,
    include_workspace_skills: bool,
) -> SessionMemory {
    if path == Loop::Typed {
        enable_lifecycle(root);
    }
    SessionMemory::open_with_workspace_skills(root, false, include_workspace_skills)
        .expect("session memory")
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
    each_path(|path| {
        let dir = workspace_with_log(&three_occurrences_of(RECURRING));
        let mut memory = session(dir.path(), path);
        memory.auto_create_skills(&log_path(dir.path()), true);
        assert_eq!(
            skill_files(dir.path()),
            vec!["prefer-updating-witness-test-assertions-e2010443.md"],
            "{path:?}: the control case must mine, or the suppression tests \
             prove nothing — and the mined FILENAME must be identical across \
             paths, since it is the candidate identity"
        );
    });
}

// ---- guarantee: tombstone suppression across re-learning, on every surface ----

/// A lesson forgotten on the `Skill` surface cannot come back as a skill, even
/// though the log lines that produced it predate the tombstone and are still in
/// the append-only log.
#[test]
fn a_lesson_forgotten_as_a_skill_cannot_return_as_a_skill() {
    each_path(|path| {
        let dir = workspace_with_log(&three_occurrences_of(RECURRING));
        stella_store::Store::open(dir.path())
            .expect("store")
            .forget(ContextSurface::Skill, "some-skill-id", RECURRING, "no")
            .expect("forget");

        let mut memory = session(dir.path(), path);
        memory.auto_create_skills(&log_path(dir.path()), true);
        assert!(
            skill_files(dir.path()).is_empty(),
            "{path:?}: a forgotten lesson came back through the miner: {:?}",
            skill_files(dir.path())
        );
    });
}

/// The same, forgotten on the `Memory` surface. Both surfaces feed the sweep,
/// and a user who forgets the *recalled memory* reasonably expects the skill
/// the same lesson would have become to stay gone too.
#[test]
fn a_lesson_forgotten_as_a_memory_cannot_return_as_a_skill() {
    each_path(|path| {
        let dir = workspace_with_log(&three_occurrences_of(RECURRING));
        stella_store::Store::open(dir.path())
            .expect("store")
            .forget(ContextSurface::Memory, "nod_whatever", RECURRING, "no")
            .expect("forget");

        let mut memory = session(dir.path(), path);
        memory.auto_create_skills(&log_path(dir.path()), true);
        assert!(
            skill_files(dir.path()).is_empty(),
            "{path:?}: a memory-surface tombstone did not reach the miner: {:?}",
            skill_files(dir.path())
        );
    });
}

/// Suppression is by **restatement**, not equality: the loop re-learns
/// paraphrases, so a tombstone carrying yesterday's wording must still catch
/// today's. This is the property a plain `DELETE` could never have.
#[test]
fn a_paraphrase_of_a_forgotten_lesson_is_also_suppressed() {
    let paraphrase =
        "Prefer updating witness-test assertions so they match the live renderer's output.";
    each_path(|path| {
        let dir = workspace_with_log(&three_occurrences_of(paraphrase));
        stella_store::Store::open(dir.path())
            .expect("store")
            // The tombstone carries the ORIGINAL wording, not the paraphrase.
            .forget(ContextSurface::Skill, "skill-id", RECURRING, "no")
            .expect("forget");

        let mut memory = session(dir.path(), path);
        memory.auto_create_skills(&log_path(dir.path()), true);
        assert!(
            skill_files(dir.path()).is_empty(),
            "{path:?}: a re-worded lesson slipped past its own tombstone: {:?}",
            skill_files(dir.path())
        );
    });
}

/// A tombstone on a surface that does **not** suppress restatements must not
/// suppress anything by text. Re-authoring a rule by hand is a deliberate act,
/// and swallowing an unrelated mined skill because it resembles a rule the user
/// deleted months ago would be its own bug.
#[test]
fn an_authored_surface_tombstone_does_not_suppress_mining() {
    each_path(|path| {
        let dir = workspace_with_log(&three_occurrences_of(RECURRING));
        stella_store::Store::open(dir.path())
            .expect("store")
            .forget(ContextSurface::Rule, "some-rule", RECURRING, "no")
            .expect("forget");

        let mut memory = session(dir.path(), path);
        memory.auto_create_skills(&log_path(dir.path()), true);
        assert_eq!(
            skill_files(dir.path()).len(),
            1,
            "{path:?}: an authored-surface tombstone must not reach the \
             restatement sweep"
        );
    });
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

    each_path(|path| {
        let dir = workspace_with_log(&borrowed);
        let mut memory = session(dir.path(), path);
        memory.auto_create_skills(&log_path(dir.path()), true);
        assert_eq!(
            skill_files(dir.path()).len(),
            2,
            "{path:?}: five eligible candidates, one session, cap of 2: {:?}",
            skill_files(dir.path())
        );

        // A second pass inside the SAME session creates nothing more: the cap
        // is per session, not per mining pass.
        memory.auto_create_skills(&log_path(dir.path()), true);
        assert_eq!(
            skill_files(dir.path()).len(),
            2,
            "{path:?}: the cap survives a re-mine"
        );
    });
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
    each_path(|path| {
        let dir = workspace_with_log(&three_occurrences_of(RECURRING));
        // Workspace skills trusted — the normal configuration, and the only one
        // in which the loop reads the directory it writes to. The untrusted
        // case no longer reaches this code at all: #742 skips auto-creation
        // outright when workspace skills are not trusted, so there is no write
        // to clobber anything with.
        let mut first = session_with_workspace_skills(dir.path(), path, true);
        first.auto_create_skills(&log_path(dir.path()), true);
        let created = dir
            .path()
            .join(".stella/skills/prefer-updating-witness-test-assertions-e2010443.md");
        assert!(created.exists(), "{path:?}: the first pass wrote the skill");

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

        // A later session — `skills_created` starts at zero again, so the cap
        // is not what stops the write.
        let mut second = session_with_workspace_skills(dir.path(), path, true);
        second.auto_create_skills(&log_path(dir.path()), true);

        assert_eq!(
            std::fs::read_to_string(&created).expect("read back"),
            hand_edited,
            "{path:?}: the miner overwrote a hand-edited skill file"
        );
        assert_eq!(
            skill_files(dir.path()).len(),
            1,
            "{path:?}: and it did not write a second file beside it: {:?}",
            skill_files(dir.path())
        );
    });
}

// The #737 clobbering defect this file previously pinned is FIXED on main
// (#742): the no-clobber guard now tests the filesystem rather than the
// loaded-skill list, and auto-creation is skipped outright when workspace
// skills are untrusted. The pin asserted the buggy behavior, so it is deleted
// here rather than inverted — its own note said the fix would have to delete it
// on purpose. The guarantee is covered by
// `memory::tests::a_disabled_hand_edited_skill_is_never_overwritten_by_auto_creation`.

// ---- guarantee: failure isolation ----

/// Mining must never fail the user's task. Every one of these is a plausible
/// real failure, and each must return normally with the loop simply doing
/// nothing.
#[test]
fn mining_failures_are_isolated() {
    each_path(|path| {
        // 1. No mining log at all (a workspace that has never reflected).
        let dir = tempfile::tempdir().expect("tempdir");
        let mut memory = session(dir.path(), path);
        memory.auto_create_skills(&log_path(dir.path()), true);

        // 2. A log full of garbage that is not JSON.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".stella/private")).expect("private");
        std::fs::write(log_path(dir.path()), "not json\n{ broken\n\0\n").expect("write");
        let mut memory = session(dir.path(), path);
        memory.auto_create_skills(&log_path(dir.path()), true);

        // 3. The skills directory is a regular FILE, so every write under it
        //    fails.
        let dir = workspace_with_log(&three_occurrences_of(RECURRING));
        std::fs::create_dir_all(dir.path().join(".stella")).expect("stella dir");
        std::fs::write(dir.path().join(".stella/skills"), "i am not a directory").expect("write");
        let mut memory = session(dir.path(), path);
        memory.auto_create_skills(&log_path(dir.path()), true);

        // 4. The tombstone store is corrupt — unreadable suppression state must
        //    not take the turn down with it.
        let dir = workspace_with_log(&three_occurrences_of(RECURRING));
        let mut memory = session(dir.path(), path);
        for entry in std::fs::read_dir(dir.path().join(".stella/private"))
            .expect("private")
            .flatten()
        {
            if entry.path().extension().is_some_and(|e| e == "db") {
                std::fs::write(entry.path(), b"this is not a sqlite database").expect("corrupt");
            }
        }
        memory.auto_create_skills(&log_path(dir.path()), true);
    });
}

// ---- GATE: existing skill files are byte-identical after migration ----

/// The same evidence, mined by each loop, must produce a byte-identical file.
///
/// This is the gate criterion stated at the level a user experiences it. The
/// pure half is pinned in `stella-core`'s `skills::migration_contract`
/// (`rendered_skill_bytes_are_pinned`), but that only proves the renderer did
/// not move — it says nothing about whether the typed path hands the renderer
/// the same candidate. Everything the rendering depends on has to survive the
/// round trip through the ledger: the `<slug>-<hash8>` name, the description's
/// occurrence count, the domain list and its order, the body, and every
/// `## Evidence` line's reference, timestamp and snippet.
///
/// Compared as bytes rather than field by field on purpose. A field-wise
/// comparison would pass while a stray newline or a reordered frontmatter key
/// showed up in every user's `git diff`.
#[test]
fn both_loops_render_byte_identical_skill_files() {
    let mut rendered: Vec<(Loop, String)> = Vec::new();
    for path in [Loop::Lexical, Loop::Typed] {
        let dir = workspace_with_log(&three_occurrences_of(RECURRING));
        let mut memory = session(dir.path(), path);
        memory.auto_create_skills(&log_path(dir.path()), true);

        let files = skill_files(dir.path());
        assert_eq!(files.len(), 1, "{path:?}: expected exactly one file");
        let body = std::fs::read_to_string(dir.path().join(".stella/skills").join(&files[0]))
            .expect("read the written skill");
        rendered.push((path, body));
    }
    assert_eq!(
        rendered[0].1, rendered[1].1,
        "the typed loop writes different bytes than the lexical one it replaces — \
         a user's existing skill file would now diff against its own re-render"
    );
    // And the content is actually the mined skill, not two identical empties.
    assert!(rendered[0].1.contains("origin: auto"), "{}", rendered[0].1);
    assert!(
        rendered[0]
            .1
            .contains("- `reflection:300` (observed at 300):"),
        "{}",
        rendered[0].1
    );
}

// ---- what the typed path adds (not a compatibility guarantee) ----

/// The anti-poisoning rule, end to end through the real loop.
///
/// Thirty lessons emitted inside ONE turn cannot become a skill on the typed
/// path. The lexical path has no task identity and therefore cannot express
/// this at all, which is why this case is typed-only — see
/// `skills::migration_contract::lexical_miner_counts_events_not_distinct_tasks`
/// for the documented gap it closes.
#[test]
fn thirty_repetitions_in_one_turn_create_no_skill_on_the_typed_path() {
    // Thirty distinct wordings of one point, all stamped with the same turn.
    let lessons: Vec<(String, u64)> = (0..30)
        .map(|i| {
            (
                format!("{RECURRING} Restatement number {i} of exactly the same point."),
                100u64,
            )
        })
        .collect();
    let borrowed: Vec<(&str, u64)> = lessons.iter().map(|(t, a)| (t.as_str(), *a)).collect();

    let dir = workspace_with_log(&borrowed);
    let mut memory = session(dir.path(), Loop::Typed);
    memory.auto_create_skills(&log_path(dir.path()), true);
    assert!(
        skill_files(dir.path()).is_empty(),
        "thirty repetitions inside one task promoted a skill: {:?}",
        skill_files(dir.path())
    );

    // The same evidence spread across three turns DOES promote — so the refusal
    // above is the task count doing its job, not the loop being broken.
    let spread: Vec<(&str, u64)> = borrowed
        .iter()
        .enumerate()
        .map(|(i, (t, _))| (*t, 100 + (i as u64 % 3) * 100))
        .collect();
    let dir = workspace_with_log(&spread);
    let mut memory = session(dir.path(), Loop::Typed);
    memory.auto_create_skills(&log_path(dir.path()), true);
    assert!(
        !skill_files(dir.path()).is_empty(),
        "the same evidence across three turns must still promote"
    );
}

/// The typed path leaves an audit trail: observations and a proposal carrying
/// the distinct-task count that justified the promotion. This is the observable
/// the phase is for — "three separate tasks, here they are".
#[test]
fn the_typed_path_records_why_it_promoted() {
    let dir = workspace_with_log(&three_occurrences_of(RECURRING));
    let mut memory = session(dir.path(), Loop::Typed);
    memory.auto_create_skills(&log_path(dir.path()), true);

    use stella_core::context_record::RecordProposalKind;
    let proposals = crate::memory::proposals::all_proposals(&memory.store, 100);

    // Two proposals for one lesson: a knowledge proposal (the skill that was
    // written) and a directive proposal (the rule the same evidence supports).
    // Both miners run over one observation pool — that is deliverable 5, and
    // the shared clustering module is why their candidate ids agree.
    let knowledge = proposals
        .iter()
        .find(|p| p.proposal_kind == RecordProposalKind::Knowledge)
        .expect("a knowledge proposal was recorded");
    assert_eq!(knowledge.score.distinct_tasks, 3);
    assert_eq!(knowledge.supporting_observations.len(), 3);
    assert_eq!(
        knowledge.candidate_id, "prefer-updating-witness-test-assertions-e2010443",
        "the proposal names the skill that was written"
    );

    let directive = proposals
        .iter()
        .find(|p| p.proposal_kind == RecordProposalKind::Directive)
        .expect("a directive proposal was recorded");
    assert_eq!(directive.candidate_id, knowledge.candidate_id);
    assert_ne!(
        directive.lineage_id, knowledge.lineage_id,
        "the two must not collide onto one lineage"
    );

    // …and the rule did NOT auto-activate: three tasks score 70, below the 85
    // auto-activation bar, so a directive waits for an explicit Keep. A skill
    // informs; a rule steers, and the bar reflects that.
    assert!(
        !dir.path()
            .join(".stella/rules")
            .join(format!("{}.md", directive.candidate_id))
            .exists(),
        "a three-task rule auto-activated without review"
    );
}

/// …and the lexical path records nothing, so turning the flag off really does
/// mean "no ledger writes", not "quieter ledger writes".
#[test]
fn the_lexical_path_writes_nothing_to_the_ledger() {
    let dir = workspace_with_log(&three_occurrences_of(RECURRING));
    let mut memory = session(dir.path(), Loop::Lexical);
    memory.auto_create_skills(&log_path(dir.path()), true);

    assert_eq!(
        memory.store.record_counts().expect("counts"),
        Vec::new(),
        "the default path touched the lifecycle ledger"
    );
    assert!(
        !skill_files(dir.path()).is_empty(),
        "…while still doing its actual job"
    );
}

// ---- GATE: a declined proposal does not come back next turn ----

/// The re-proposal cooldown. Ignoring a proposal has to mean something beyond
/// the moment it is typed: the same evidence is still in the log, so without a
/// cooldown the very next reflection turn would write the skill anyway.
#[test]
fn a_declined_proposal_is_not_re_promoted() {
    let dir = workspace_with_log(&three_occurrences_of(RECURRING));
    let mut memory = session(dir.path(), Loop::Typed);

    // The loop induces a proposal — but the user gets there before the skill
    // does, which is the whole point of a review surface.
    memory.auto_create_skills(&log_path(dir.path()), true);
    let written = skill_files(dir.path());
    for name in &written {
        std::fs::remove_file(dir.path().join(".stella/skills").join(name)).expect("remove");
    }

    // The full lineage id, not the bare candidate id: both miners derive the
    // same candidate id for one lesson, so the bare form is ambiguous and
    // `resolve_proposal` refuses it rather than guessing. Declining the SKILL
    // is what this test is about.
    crate::proposals_cmd::decide_for_test(
        &memory.store,
        "prp_knowledge_prefer-updating-witness-test-assertions-e2010443",
        stella_core::context_record::PromotionAction::Rejected,
        "not a convention, just a habit",
    )
    .expect("ignore");

    // A later session over the SAME unchanged log must not write it back.
    let mut later = session(dir.path(), Loop::Typed);
    later.auto_create_skills(&log_path(dir.path()), true);
    assert!(
        skill_files(dir.path()).is_empty(),
        "a declined proposal came back on the next turn: {:?}",
        skill_files(dir.path())
    );
}

/// …and the decline is reversible. Keeping it later lets the loop promote it
/// again, so "ignore" is a decision the user can take back rather than a
/// permanent deletion (spec §5.7: suppression is reversible).
#[test]
fn reversing_a_decline_lets_the_proposal_promote_again() {
    let dir = workspace_with_log(&three_occurrences_of(RECURRING));
    let mut memory = session(dir.path(), Loop::Typed);
    memory.auto_create_skills(&log_path(dir.path()), true);
    for name in skill_files(dir.path()) {
        std::fs::remove_file(dir.path().join(".stella/skills").join(name)).expect("remove");
    }

    let id = "prp_knowledge_prefer-updating-witness-test-assertions-e2010443";
    crate::proposals_cmd::decide_for_test(
        &memory.store,
        id,
        stella_core::context_record::PromotionAction::Rejected,
        "declined",
    )
    .expect("ignore");
    crate::proposals_cmd::decide_for_test(
        &memory.store,
        id,
        stella_core::context_record::PromotionAction::Confirmed,
        "actually, keep it",
    )
    .expect("keep");

    let mut later = session(dir.path(), Loop::Typed);
    later.auto_create_skills(&log_path(dir.path()), true);
    assert_eq!(
        skill_files(dir.path()),
        vec!["prefer-updating-witness-test-assertions-e2010443.md"],
        "reversing the decline did not let the proposal promote again"
    );
}

// ---- deliverable 5: the rules half, through the same governance ----

/// A tombstoned lesson cannot return as a **rule** either.
///
/// The gate criterion says "as a proposal or a skill"; wiring a second miner
/// over the same observation pool adds a third door, and it has to be shut by
/// the same filter rather than a new one. It is: both miners consume the
/// observation list that `auto_create_skills_typed` has already swept, so
/// there is one predicate, not two (spec §5.7 — a second suppression mechanism
/// is a defect).
#[test]
fn a_forgotten_lesson_cannot_return_as_a_rule() {
    let strong = "Always run the database migration before the integration suite starts.";
    // Five turns, so the proposal would clear the auto-activation bar and
    // actually be written — otherwise this could pass because the confidence
    // gate stopped it rather than the tombstone.
    let lessons: Vec<(&str, u64)> = (1..=5).map(|i| (strong, i * 100)).collect();

    // Control: without a tombstone it really does write a rule.
    let dir = workspace_with_log(&lessons);
    let mut memory = session(dir.path(), Loop::Typed);
    memory.auto_create_skills(&log_path(dir.path()), true);
    let rules_dir = dir.path().join(".stella/rules");
    assert!(
        rules_dir.exists() && std::fs::read_dir(&rules_dir).into_iter().flatten().count() > 0,
        "the control case must write a rule, or the suppression below proves nothing"
    );

    // With a tombstone on the lesson, nothing is written.
    let dir = workspace_with_log(&lessons);
    stella_store::Store::open(dir.path())
        .expect("store")
        .forget(ContextSurface::Skill, "some-skill-id", strong, "no")
        .expect("forget");
    let mut memory = session(dir.path(), Loop::Typed);
    memory.auto_create_skills(&log_path(dir.path()), true);
    let written: Vec<String> = std::fs::read_dir(dir.path().join(".stella/rules"))
        .map(|e| {
            e.flatten()
                .map(|x| x.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        written.is_empty(),
        "a forgotten lesson came back as a rule: {written:?}"
    );
}

/// A well-evidenced rule auto-activates and lands where the loader reads it —
/// which is the only thing that makes wiring the miner observable at all.
#[test]
fn a_well_evidenced_rule_activates_and_reaches_the_loader() {
    let strong = "Always run the database migration before the integration suite starts.";
    let lessons: Vec<(&str, u64)> = (1..=5).map(|i| (strong, i * 100)).collect();
    let dir = workspace_with_log(&lessons);
    let mut memory = session(dir.path(), Loop::Typed);
    memory.auto_create_skills(&log_path(dir.path()), true);

    let loaded = crate::rules::load_workspace_rules_unfiltered(dir.path());
    let mined = loaded
        .iter()
        .find(|r| r.text.contains("database migration"))
        .expect("the mined rule reached the loader");

    // Advisory, always. A mined rule that could deny a tool call would be an
    // inferred directive reaching blocking — the thing the gate forbids.
    assert!(
        mined.guard.is_none(),
        "a mined rule arrived with a guard: {mined:?}"
    );
    assert_eq!(mined.tier(), stella_core::rules::RuleTier::Prompt);
}

/// The rules miner is off with the flag off, exactly like the rest of the
/// typed path. Nothing appears in `.stella/rules` on the default path.
#[test]
fn the_lexical_path_mines_no_rules() {
    let strong = "Always run the database migration before the integration suite starts.";
    let lessons: Vec<(&str, u64)> = (1..=5).map(|i| (strong, i * 100)).collect();
    let dir = workspace_with_log(&lessons);
    let mut memory = session(dir.path(), Loop::Lexical);
    memory.auto_create_skills(&log_path(dir.path()), true);
    assert!(
        !dir.path().join(".stella/rules").exists(),
        "the default path wrote a rule"
    );
}
