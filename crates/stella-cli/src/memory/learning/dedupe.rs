//! What `partition_known` must store, divert, and drop.
//!
//! The commit that added the filter deferred these ("tests follow"), so the
//! behavior shipped unpinned. It sits on the path that persists learning — the
//! failure mode is silent, and what it misroutes is the only record of what a
//! turn taught. That is worth pinning precisely.
//!
//! Four properties, in the order they matter:
//!
//! * a restatement of something already stored is **diverted, not dropped** —
//!   it must stay out of the context store (recall has a budget) while still
//!   reaching the caller, who appends it to the mining log where it counts as
//!   a recurrence (#2358: dropping it here starved the skill and rule miners
//!   of the most common recurrence shape there is);
//! * a novel lesson lands in the novel half (the thing a too-eager filter
//!   would break);
//! * a batch that says one thing twice keeps one copy, **including on an empty
//!   store** — one reflection call returns several lessons and routinely
//!   repeats itself, and the store being empty is exactly when a fresh
//!   workspace is learning its first facts;
//! * a within-batch repeat of a *diverted* lesson is also dropped — the model
//!   stuttering must not manufacture extra mining occurrences.
//!
//! The strings are chosen against the real predicate rather than by feel:
//! matching is Jaccard over token sets at `SIMILARITY_THRESHOLD` (0.5), so each
//! case below states the ratio it relies on. A test that merely *looks* like a
//! paraphrase would pass or fail on tokenizer details rather than on intent.

use stella_context::{ContextDelta, MemoryInput};

use crate::memory::{LessonKind, ReflectionLesson, SessionMemory};

/// A lesson carrying `text`; every other field is irrelevant to the split,
/// which reads `lesson` and nothing else.
fn lesson(text: &str) -> ReflectionLesson {
    ReflectionLesson {
        lesson: text.to_string(),
        domains: Vec::new(),
        occurred_at: 0,
        task_id: String::new(),
        trigger: String::new(),
        saves: String::new(),
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
/// the split reads it back the same way a real second turn would.
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
async fn a_paraphrase_of_a_stored_memory_is_diverted_and_a_new_fact_is_stored() {
    let (_dir, memory) = session();
    remember(&memory, "commands are registered in registry.py").await;

    // 5 stored tokens, 8 candidate tokens, 5 shared → 0.625, over the 0.5 bar.
    let restatement = "commands are registered in registry.py for the cli";
    // Shares no token with the stored memory → 0.0.
    let novel = "the witness stage rejects a blind diff probe";

    let (kept, diverted) = memory.partition_known(vec![lesson(restatement), lesson(novel)]);

    assert_eq!(
        texts(&kept),
        vec![novel],
        "a paraphrase of a stored memory must stay out of the store and an \
         unrelated lesson must land in it; storing the paraphrase would spend \
         a recall slot restating what the store already holds"
    );
    assert_eq!(
        texts(&diverted),
        vec![restatement],
        "and the paraphrase must come back as a restatement rather than \
         vanish — it is the recurrence signal the skill and rule miners \
         count, and dropping it is how the learning loop starved (#2358)"
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

    let (kept, diverted) =
        memory.partition_known(vec![lesson(first), lesson(second), lesson(other)]);

    assert_eq!(
        texts(&kept),
        vec![first, other],
        "one reflection call routinely says the same thing twice; the batch \
         must collapse to the first phrasing even when the store is empty"
    );
    assert!(
        diverted.is_empty(),
        "a within-batch stutter is one turn repeating itself, not the \
         codebase recurring — counting it as a mining occurrence would let a \
         single verbose reflection impersonate three turns of evidence: {:?}",
        texts(&diverted)
    );
}

#[tokio::test]
async fn an_empty_store_keeps_every_distinct_lesson() {
    let (_dir, memory) = session();

    let a = "the pre-push gate exports GIT_DIR to every hook it runs";
    let b = "best-of-n candidates share one tool registry";

    let (kept, diverted) = memory.partition_known(vec![lesson(a), lesson(b)]);

    assert_eq!(
        texts(&kept),
        vec![a, b],
        "nothing is known yet, so nothing is a restatement — a filter that \
         swallowed a fresh workspace's first lessons would be worse than the \
         duplication it exists to prevent"
    );
    assert!(diverted.is_empty());
}

#[tokio::test]
async fn a_batch_repeat_of_a_diverted_restatement_is_dropped_not_double_counted() {
    let (_dir, memory) = session();
    remember(&memory, "commands are registered in registry.py").await;

    // Both are restatements of the stored memory (each shares its 5 tokens;
    // 5/8 = 0.625 and 5/7 = 0.714), and the second restates the first
    // (7 shared of 8 → 0.875).
    let restated = "commands are registered in registry.py for the cli";
    let restated_again = "commands are registered in registry.py for cli";

    let (kept, diverted) = memory.partition_known(vec![lesson(restated), lesson(restated_again)]);

    assert!(kept.is_empty(), "nothing here is new knowledge");
    assert_eq!(
        texts(&diverted),
        vec![restated],
        "one turn's stutter must contribute ONE mining occurrence, not two — \
         occurrences are the miners' evidence, and a batch echo is not \
         evidence of anything but verbosity"
    );
}
