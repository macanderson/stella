//! What `retain_unknown` must and must not drop.
//!
//! The commit that added the filter deferred these ("tests follow"), so the
//! behavior shipped unpinned. It is a *lossy* filter on the path that persists
//! learning — the failure mode is silent, and the thing it silently discards is
//! the only record of what a turn taught. That is worth pinning precisely.
//!
//! Three properties, in the order they matter:
//!
//! * a restatement of something already stored is dropped (the point);
//! * a novel lesson survives (the thing a too-eager filter would break);
//! * a batch that says one thing twice keeps one copy, **including on an empty
//!   store** — one reflection call returns up to three lessons and routinely
//!   repeats itself, and the store being empty is exactly when a fresh
//!   workspace is learning its first facts.
//!
//! The strings are chosen against the real predicate rather than by feel:
//! matching is Jaccard over token sets at `SIMILARITY_THRESHOLD` (0.5), so each
//! case below states the ratio it relies on. A test that merely *looks* like a
//! paraphrase would pass or fail on tokenizer details rather than on intent.

use stella_context::{ContextDelta, MemoryInput};

use crate::memory::{LessonKind, ReflectionLesson, SessionMemory};

/// A lesson carrying `text`; every other field is irrelevant to the filter,
/// which reads `lesson` and nothing else.
fn lesson(text: &str) -> ReflectionLesson {
    ReflectionLesson {
        lesson: text.to_string(),
        domains: Vec::new(),
        occurred_at: 0,
        task_id: String::new(),
        kind: LessonKind::Domain,
    }
}

/// A session over an empty workspace, plus the tempdir guard that owns it.
fn session() -> (tempfile::TempDir, SessionMemory) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".stella")).expect("workspace");
    let memory = SessionMemory::open(dir.path(), false).expect("session memory");
    (dir, memory)
}

/// Store `text` as a reflection memory — the same shape the loop writes, so
/// the filter reads it back the same way a real second turn would.
async fn remember(memory: &SessionMemory, text: &str) {
    memory
        .store
        .upsert(ContextDelta {
            memories: vec![MemoryInput::reflection(text, Vec::<String>::new())],
            ..ContextDelta::default()
        })
        .await
        .expect("upsert");
}

fn texts(lessons: &[ReflectionLesson]) -> Vec<&str> {
    lessons.iter().map(|l| l.lesson.as_str()).collect()
}

#[tokio::test]
async fn a_paraphrase_of_a_stored_memory_is_dropped_and_a_new_fact_survives() {
    let (_dir, memory) = session();
    remember(&memory, "commands are registered in registry.py").await;

    // 5 stored tokens, 8 candidate tokens, 5 shared → 0.625, over the 0.5 bar.
    let restatement = "commands are registered in registry.py for the cli";
    // Shares no token with the stored memory → 0.0.
    let novel = "the witness stage rejects a blind diff probe";

    let kept = memory.retain_unknown(vec![lesson(restatement), lesson(novel)]);

    assert_eq!(
        texts(&kept),
        vec![novel],
        "a paraphrase of a stored memory must be dropped and an unrelated \
         lesson must survive; dropping the novel one would make the filter \
         cost more than the duplicates it removes"
    );
}

#[tokio::test]
async fn one_batch_saying_the_same_thing_twice_keeps_the_first_on_an_empty_store() {
    // No `remember` call: the store is empty, which is precisely when a fresh
    // workspace is learning its first facts. An early return on "nothing
    // stored" would skip the within-batch check and let the pair through.
    let (_dir, memory) = session();

    let first = "auth tokens expire after fifteen minutes silently";
    // 6 of the 7 tokens above → 0.857.
    let second = "auth tokens expire after fifteen minutes";
    let other = "the migration runner reorders columns on rollback";

    let kept = memory.retain_unknown(vec![lesson(first), lesson(second), lesson(other)]);

    assert_eq!(
        texts(&kept),
        vec![first, other],
        "one reflection call routinely says the same thing twice; the batch \
         must collapse to the first phrasing even when the store is empty"
    );
}

#[tokio::test]
async fn an_empty_store_keeps_every_distinct_lesson() {
    let (_dir, memory) = session();

    let a = "the pre-push gate exports GIT_DIR to every hook it runs";
    let b = "best-of-n candidates share one tool registry";

    let kept = memory.retain_unknown(vec![lesson(a), lesson(b)]);

    assert_eq!(
        texts(&kept),
        vec![a, b],
        "nothing is known yet, so nothing is a restatement — a filter that \
         swallowed a fresh workspace's first lessons would be worse than the \
         duplication it exists to prevent"
    );
}
