//! Tests for typed observation extraction.

use super::*;
use stella_context::ContextStore;

fn store() -> (tempfile::TempDir, ContextStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ContextStore::open(dir.path().join("context.db")).expect("open");
    (dir, store)
}

fn write_log(dir: &Path, lessons: &[(&str, u64)]) -> std::path::PathBuf {
    let path = dir.join("reflections.jsonl");
    let mut body = String::new();
    for (text, at) in lessons {
        body.push_str(
            &serde_json::to_string(&ReflectionLesson {
                lesson: (*text).to_string(),
                domains: vec!["testing".into()],
                occurred_at: *at,
                task_id: String::new(),
                trigger: String::new(),
                saves: String::new(),
                kind: crate::memory::LessonKind::Process,
            })
            .expect("serialize"),
        );
        body.push('\n');
    }
    std::fs::write(&path, body).expect("write log");
    path
}

#[test]
fn lessons_become_typed_observations() {
    let (dir, store) = store();
    let log = write_log(dir.path(), &[("Prefer rg over grep.", 100)]);

    assert_eq!(extract_reflection_observations(&store, &log), 1);
    let observations = all_observations(&store, 100);
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].text, "Prefer rg over grep.");
    assert_eq!(observations[0].task_id, "turn:100");
    assert_eq!(observations[0].source_ref, "reflection:100");
    assert!(!observations[0].redacted);
    assert_eq!(observations[0].domains, vec!["testing"]);
}

/// The property the whole design turns on. Extraction runs every turn over an
/// append-only log; running it repeatedly must not accumulate duplicates.
#[test]
fn extraction_is_replay_idempotent() {
    let (dir, store) = store();
    let log = write_log(dir.path(), &[("A.", 100), ("B.", 200)]);

    assert_eq!(extract_reflection_observations(&store, &log), 2);
    assert_eq!(
        extract_reflection_observations(&store, &log),
        0,
        "a second pass over unchanged evidence extracted something"
    );
    assert_eq!(all_observations(&store, 100).len(), 2);
}

/// …and it holds even with the cursor thrown away, which is what a crash
/// between the record write and the cursor advance looks like. This is why
/// derived ids carry the correctness claim and the cursor is only an
/// optimization.
#[test]
fn a_lost_cursor_costs_a_rescan_but_never_a_duplicate() {
    let (dir, store) = store();
    let log = write_log(dir.path(), &[("A.", 100), ("B.", 200)]);
    extract_reflection_observations(&store, &log);

    store
        .set_extraction_cursor("reflections.jsonl", "0")
        .expect("rewind");
    assert_eq!(
        extract_reflection_observations(&store, &log),
        0,
        "the rescan re-derived the same ids, so the ledger absorbed them"
    );
    assert_eq!(all_observations(&store, 100).len(), 2);
}

/// A truncated or rotated log must not leave new content stranded under a stale
/// offset.
#[test]
fn a_shrunken_log_restarts_from_the_beginning() {
    let (dir, store) = store();
    let log = write_log(dir.path(), &[("A.", 100), ("B.", 200), ("C.", 300)]);
    extract_reflection_observations(&store, &log);
    assert_eq!(all_observations(&store, 100).len(), 3);

    // The log is replaced with something shorter but different.
    write_log(dir.path(), &[("D.", 400)]);
    assert_eq!(
        extract_reflection_observations(&store, &log),
        1,
        "the new line under the stale offset was skipped"
    );
    assert_eq!(all_observations(&store, 100).len(), 4);
}

#[test]
fn only_new_lines_are_extracted_after_a_cursor_advance() {
    let (dir, store) = store();
    let log = write_log(dir.path(), &[("A.", 100)]);
    assert_eq!(extract_reflection_observations(&store, &log), 1);

    write_log(dir.path(), &[("A.", 100), ("B.", 200)]);
    assert_eq!(extract_reflection_observations(&store, &log), 1);
    assert_eq!(all_observations(&store, 100).len(), 2);
}

/// Secrets are removed **before** the record is constructed, so no unredacted
/// typed record exists at any point — and the fact of redaction is recorded so
/// a reader can see it happened.
#[test]
fn secrets_are_redacted_before_persistence() {
    let (dir, store) = store();
    let log = write_log(
        dir.path(),
        &[(
            "The reflection call needs sk-ant-api03-AbCdEf123456789 in the header.",
            100,
        )],
    );
    extract_reflection_observations(&store, &log);

    let observations = all_observations(&store, 100);
    assert_eq!(observations.len(), 1);
    assert!(
        !observations[0].text.contains("sk-ant-api03"),
        "a credential reached the ledger: {}",
        observations[0].text
    );
    assert!(
        observations[0].redacted,
        "the redaction happened but was not recorded"
    );

    // And it is gone from the raw stored row too, not merely from the typed
    // view — the body is what a later export or inspection reads.
    let rows = store.records_of_kind("observation", 10).expect("rows");
    assert!(!rows[0].body.contains("sk-ant-api03"), "{}", rows[0].body);
}

/// Thirty lessons emitted by ONE reflection call share one `occurred_at`, and
/// therefore one task id. This is the field that makes the anti-poisoning gate
/// enforceable at all.
#[test]
fn lessons_from_one_turn_share_one_task_id() {
    let (dir, store) = store();
    let same_turn: Vec<(&str, u64)> = (0..30).map(|_| ("Prefer rg over grep.", 100u64)).collect();
    let log = write_log(dir.path(), &same_turn);
    extract_reflection_observations(&store, &log);

    let observations = all_observations(&store, 100);
    // Thirty byte-identical lessons in one turn collapse to ONE observation:
    // same content, same task, therefore the same derived id.
    assert_eq!(
        observations.len(),
        1,
        "identical evidence in one turn should be one observation"
    );
    assert_eq!(observations[0].task_id, "turn:100");
}

/// Different turns produce different task ids, which is what lets three
/// separate tasks clear the bar.
#[test]
fn lessons_from_different_turns_have_different_task_ids() {
    let (dir, store) = store();
    let log = write_log(
        dir.path(),
        &[
            ("Prefer rg over grep.", 100),
            ("Prefer rg over grep.", 200),
            ("Prefer rg over grep.", 300),
        ],
    );
    extract_reflection_observations(&store, &log);

    let tasks: std::collections::BTreeSet<String> = all_observations(&store, 100)
        .into_iter()
        .map(|o| o.task_id)
        .collect();
    assert_eq!(
        tasks,
        ["turn:100", "turn:200", "turn:300"]
            .into_iter()
            .map(String::from)
            .collect()
    );
}

/// An explicitly supplied task id wins over the turn fallback, so a caller with
/// a real task boundary can use it without a log-format change.
#[test]
fn an_explicit_task_id_overrides_the_turn_fallback() {
    let lesson = ReflectionLesson {
        lesson: "x".into(),
        domains: vec![],
        occurred_at: 100,
        task_id: "goal:fix-the-tenancy-leak".into(),
        trigger: String::new(),
        saves: String::new(),
        kind: crate::memory::LessonKind::Process,
    };
    assert_eq!(task_id_for(&lesson), "goal:fix-the-tenancy-leak");
}

/// Failure isolation: nothing here may fail the turn.
#[test]
fn extraction_failures_are_isolated() {
    let (dir, store) = store();

    // No log at all.
    assert_eq!(
        extract_reflection_observations(&store, &dir.path().join("absent.jsonl")),
        0
    );

    // A log of garbage.
    let log = dir.path().join("garbage.jsonl");
    std::fs::write(&log, "not json\n{ broken\n\n").expect("write");
    assert_eq!(extract_reflection_observations(&store, &log), 0);

    // A log of well-formed but empty lessons.
    let log = write_log(dir.path(), &[("   ", 100)]);
    assert_eq!(extract_reflection_observations(&store, &log), 0);
}

/// Old log lines written before `task_id` existed still parse — the field is
/// `#[serde(default)]`, so the format change is backward compatible.
#[test]
fn a_pre_task_id_log_line_still_parses() {
    let legacy = r#"{"lesson":"Prefer rg.","domains":["testing"],"occurred_at":100}"#;
    let lesson: ReflectionLesson = serde_json::from_str(legacy).expect("legacy line parses");
    assert_eq!(lesson.task_id, "");
    assert_eq!(task_id_for(&lesson), "turn:100");
}

/// **Witness (#5323).** A lesson whose append failed is re-scanned next turn,
/// not stepped over.
///
/// Fails on the base, where the loop swallowed a failed append and then set the
/// cursor to `lines.len()` regardless — putting the failed line below the
/// cursor and out of reach forever. The module header claimed "a crash costs
/// one re-scan, never a duplicate"; a *failure* cost the record outright, and
/// the lexical path (which re-reads the whole log every turn) did not, so the
/// two diverged exactly here.
///
/// # What the two codes do here, and why the last assertion is the witness
///
/// A rival holds the write lock just past the store's 5s `busy_timeout` and
/// then lets go, which is the #5323 shape: a concurrent writer the append
/// cannot outwait.
///
/// Both codes do the same thing to the lessons on this pass: the first one's
/// append loses to the lock, the loop carries on, and the second lands once the
/// lock clears. They differ in one place only — **where the cursor ends up**.
///
/// - **Base:** the cursor is set to 2, past a line that never appended. The
///   first lesson is below it and the next scan starts after it. Gone.
/// - **Here:** the cursor stops at the first failure, so the next scan sees
///   both lines again and the first one lands.
///
/// So the discriminating fact is the ledger's contents *after a second scan*:
/// two observations here, one on the base, forever.
#[test]
fn a_lesson_whose_append_failed_is_recovered_on_the_next_scan() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("context.db");
    let store = ContextStore::open(&db).expect("open");
    let log = write_log(
        dir.path(),
        &[("Prefer rg.", 100), ("Pin the toolchain.", 200)],
    );

    // Just past the 5s `busy_timeout` the schema sets, so the first append
    // waits it out and fails rather than queueing behind the rival.
    const HELD_FOR: std::time::Duration = std::time::Duration::from_millis(5_600);
    let (locked, taken) = std::sync::mpsc::channel();
    let rival = std::thread::spawn(move || {
        let conn = rusqlite::Connection::open(&db).expect("second connection");
        conn.busy_timeout(std::time::Duration::from_millis(0))
            .expect("no wait");
        conn.execute_batch("BEGIN IMMEDIATE")
            .expect("take the lock");
        locked.send(()).expect("announce");
        std::thread::sleep(HELD_FOR);
        conn.execute_batch("ROLLBACK").expect("release the lock");
    });
    taken.recv().expect("the rival holds the write lock");

    assert_eq!(
        extract_reflection_observations(&store, &log),
        1,
        "the first lesson's append loses to the lock; the second still lands"
    );
    rival.join().expect("rival");

    // The cursor is where the two codes differ: it must not have stepped over
    // the lesson that never landed.
    assert_eq!(
        store
            .extraction_cursor("reflections.jsonl")
            .expect("read cursor"),
        Some("0".to_string()),
        "the cursor stops at the first line whose append failed"
    );

    // The witness. On the base this appends nothing and the ledger stays at
    // one: the first lesson sits below a cursor that advanced past it.
    assert_eq!(
        extract_reflection_observations(&store, &log),
        1,
        "the next scan recovers the lost lesson, replaying the one that landed"
    );
    assert_eq!(
        all_observations(&store, 10).len(),
        2,
        "and nothing was lost"
    );
}

/// A line that can never be represented is stepped over rather than becoming a
/// permanent barrier.
///
/// The other half of the fix, and the reason it is not "stop at the first
/// failure" alone: a cursor parked on a line that will never append would
/// re-scan the whole log on every turn, forever, growing the per-turn cost
/// without ever making progress. Deterministic refusals are skipped like the
/// malformed JSON above them.
#[test]
fn an_unrepresentable_line_does_not_wedge_the_cursor() {
    let (dir, store) = store();
    let log = write_log(
        dir.path(),
        &[
            ("   ", 100),
            ("Prefer rg.", 200),
            ("Pin the toolchain.", 300),
        ],
    );

    assert_eq!(extract_reflection_observations(&store, &log), 2);
    assert_eq!(
        store
            .extraction_cursor("reflections.jsonl")
            .expect("read cursor"),
        Some("3".to_string()),
        "the cursor reaches the end of a log with nothing retryable in it"
    );
}
