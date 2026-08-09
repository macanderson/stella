//! The assertions — the point of the harness (`doc:trace-replay-learning-harness` §5).
//!
//! Each pins a mechanism with no other end-to-end proof. Every one is paired
//! with its negative control, because "the loop built a skill" is not evidence
//! that the *gate* works — only "and it did not build one when the gate should
//! have stopped it" is.
//!
//! ## What replaying the real loop taught us, and had to be designed around
//!
//! A fixture cannot be written for this suite without knowing three things that
//! are not obvious from any single module, and that were measured here first.
//!
//! **1. Dedup and clustering are different predicates in different token
//! spaces, and the corpus may be worded naturally because #2358 is fixed.**
//! `partition_known` diverts a store-restatement to the mining log instead of
//! dropping it, so recurrence survives dedup by construction, and the miners'
//! `min_similarity` (0.4) is measured in `stella_core::mining::terms` space
//! rather than inherited from the store's dedup constant — see
//! [`the_dedup_and_clustering_predicates_hold_the_declared_relationship`] for
//! the relationship, asserted with this corpus's own strings.
//!
//! **2. The foundry only holes a *value-like* argument.** `classify_argument`
//! makes a positional argument a parameter only if it is a path or a number (or
//! quoted, or after `=`); a bareword stays in the skeleton. So
//! `cargo test -p <crate>` yields a *different signature per crate* and can
//! never cluster, while `rg -n "<pattern>" <path>` clusters immediately. Right
//! behaviour — a bareword is usually a subcommand — but a fixture that varies a
//! bareword measures nothing. Since #2378 the converse is also true: a fixture
//! that varies its arguments on *every* invocation measures nothing either,
//! because `min_reuse_ratio` reads 1.0× reuse as the shell itself rather than a
//! tool. A fixture must make its argument sets **recur**.
//!
//! **3. A file only lands under a trusted project.** With
//! `include_workspace_skills` false, `write_candidates` and `induce_rules` both
//! decline to write to disk (#737) while still recording proposals. A harness
//! that left it false would replay a whole corpus, behave perfectly correctly,
//! and build nothing.

use std::path::Path;

use stella_core::skills::{self, SkillMineConfig, SkillObservation};

use super::summary::ReplaySummary;
use super::trace::*;
use super::*;

// ---- fixture construction -------------------------------------------------

const T0: i64 = 1_700_000_000;
const DAY: i64 = 86_400;

/// The file every recurring lesson in the corpus names. One shared path is what
/// gives the miner five common terms while costing the dedup filter one token.
const REGISTRY: &str = "crates/stella-tools/src/registry.rs";

fn lesson(text: &str) -> TraceLesson {
    TraceLesson {
        lesson: text.to_string(),
        domains: Vec::new(),
        kind: crate::memory::LessonKind::Domain,
    }
}

/// A turn with one scripted lesson and no shell history.
fn turn(at: i64, text: &str) -> TraceTurn {
    TraceTurn {
        at,
        prompt: "work".to_string(),
        transcript: vec![TraceMessage {
            role: TraceRole::User,
            content: "work".to_string(),
        }],
        reflection: ScriptedReflection::Lessons {
            lessons: vec![lesson(text)],
            provenance: Provenance::Recorded,
        },
        shell: Vec::new(),
        succeeded: true,
        forget: Vec::new(),
        removed_files: Vec::new(),
    }
}

/// A turn whose reflection the parser cannot read.
fn unreadable_turn(at: i64, text: &str) -> TraceTurn {
    TraceTurn {
        reflection: ScriptedReflection::Unreadable {
            text: text.to_string(),
        },
        ..turn(at, "")
    }
}

/// A turn whose model call fails outright.
fn model_error_turn(at: i64, message: &str) -> TraceTurn {
    TraceTurn {
        reflection: ScriptedReflection::ModelError {
            message: message.to_string(),
        },
        ..turn(at, "")
    }
}

fn shell(commands: &[&str]) -> Vec<TraceShell> {
    commands
        .iter()
        .map(|command| TraceShell {
            command: (*command).to_string(),
            succeeded: true,
        })
        .collect()
}

/// One session per turn — each turn its own task, the shape that satisfies the
/// distinct-task gate.
fn one_task_each(turns: Vec<TraceTurn>) -> Vec<TraceSession> {
    turns
        .into_iter()
        .enumerate()
        .map(|(index, turn)| TraceSession {
            id: format!("session-{index}"),
            task_id: format!("task-{index}"),
            turns: vec![turn],
        })
        .collect()
}

/// Every turn inside ONE session and ONE task — the negative control for the
/// distinct-task gate. Same lessons, same instants, same everything else.
fn all_one_task(turns: Vec<TraceTurn>) -> Vec<TraceSession> {
    vec![TraceSession {
        id: "session-single".to_string(),
        task_id: "task-single".to_string(),
        turns,
    }]
}

fn trace(description: &str, sessions: Vec<TraceSession>) -> Trace {
    Trace {
        version: TRACE_VERSION,
        description: description.to_string(),
        workspace: WorkspaceSeed {
            domains: vec!["tools".to_string(), "engine".to_string()],
            files: [(REGISTRY.to_string(), String::new())]
                .into_iter()
                .collect(),
        },
        sessions,
    }
}

/// Five re-encounters of one convention, phrased as an engineer would phrase
/// them on five different days — naturally varied, plus one near-restatement
/// of the first. Both recurrence shapes a real engagement produces, and both
/// were once invisible to the miners (#2358): the varied wordings sat below
/// the inherited 0.5 clustering threshold, and the restatement was dropped by
/// dedup before it could reach the mining log.
fn recurring_registry_lessons() -> Vec<TraceTurn> {
    [
        "Tool registration happens in crates/stella-tools/src/registry.rs — add the entry there.",
        "Register a new tool by editing the ToolRegistry in crates/stella-tools/src/registry.rs.",
        "A tool nobody registered in crates/stella-tools/src/registry.rs is invisible to the agent.",
        "Every builtin is wired through crates/stella-tools/src/registry.rs before it can be called.",
        // Close enough to the first wording that `partition_known` keeps it out
        // of the context store — and it still counts as a mining occurrence,
        // which is the #2358 fix in one line.
        "Tool registration happens in crates/stella-tools/src/registry.rs, so add the new entry there.",
    ]
    .iter()
    .enumerate()
    .map(|(index, text)| turn(T0 + index as i64 * DAY, text))
    .collect()
}

/// Replay into a temp dir, under a **fixed** directory name.
///
/// The fixed name is load-bearing for assertion 8, and finding out why is one of
/// the things replaying the real loop taught us. A published record's lineage is
/// `ctx.<set-id>.<mined-suffix>`, and `derive_set_id` falls back to the
/// *workspace directory name* when there is no git remote — so replaying into
/// `/tmp/.tmpQ7x1a9` and `/tmp/.tmpB3k0zz` produced `ctx.tmpq7x1a9.…` and
/// `ctx.tmpb3k0zz.…` for the same learned rule.
///
/// That is correct behaviour, not a bug: a record's lineage is scoped to the
/// workspace it belongs to, and two workspaces genuinely differ. The wrong fix
/// would have been to strip the set id out of the summary, which hides real
/// nondeterminism behind a narrower report. Naming the workspace instead makes
/// the harness deterministic, and keeps the full lineage id — the thing a
/// teammate would actually see — inside what assertion 8 compares.
fn replay_root(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("stella-replay-workspace")
}

async fn replay(corpus: &Trace) -> (ReplaySummary, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let summary = Replayer::run(corpus, &replay_root(&dir))
        .await
        .expect("the trace replays");
    (summary, dir)
}

// ---- PR 2's own witnesses -------------------------------------------------

/// Invariant 4: every type crossing the fixture boundary round-trips through
/// `serde_json` byte for byte.
#[test]
fn the_trace_format_round_trips_byte_for_byte() {
    let original = trace("round trip", one_task_each(recurring_registry_lessons()));
    let json = serde_json::to_string_pretty(&original).expect("serialize");
    let parsed = Trace::parse(&json).expect("parse");
    assert_eq!(original, parsed, "a trace must survive a round trip");
    assert_eq!(
        json,
        serde_json::to_string_pretty(&parsed).expect("re-serialize"),
        "and re-serializing must produce identical bytes"
    );
}

/// An unknown version is a refusal, never a best-effort parse.
///
/// A newer fixture read by an older loader would be missing whatever the new
/// version added, and the harness would then report a confident summary of a
/// corpus it only partly understood — the worst failure available to a tool
/// whose entire output is a claim about what was learned.
#[test]
fn an_unknown_trace_version_is_refused_rather_than_partly_parsed() {
    let mut future = trace("from the future", one_task_each(vec![turn(T0, "a lesson")]));
    future.version = TRACE_VERSION + 1;
    let json = serde_json::to_string(&future).expect("serialize");
    assert_eq!(
        Trace::parse(&json),
        Err(TraceError::UnsupportedVersion {
            found: TRACE_VERSION + 1,
            supported: TRACE_VERSION,
        })
    );
}

/// The replayer sets the clock per turn rather than advancing it, so a trace
/// that steps backwards would replay happily and produce a log whose recency
/// order contradicts its own line order. Refused at load.
#[test]
fn a_trace_that_steps_time_backwards_is_refused() {
    let backwards = trace(
        "backwards",
        one_task_each(vec![turn(T0 + DAY, "later"), turn(T0, "earlier")]),
    );
    let json = serde_json::to_string(&backwards).expect("serialize");
    assert!(
        matches!(Trace::parse(&json), Err(TraceError::Unreplayable(_))),
        "non-decreasing time is a load-time contract"
    );
}

/// The trivial case, end to end: two turns, one durable lesson, one memory.
#[tokio::test]
async fn a_two_turn_trace_replays_and_builds_exactly_one_memory() {
    let corpus = trace(
        "two turns, one durable lesson",
        one_task_each(vec![
            turn(
                T0,
                "Register the tool in crates/stella-tools/src/registry.rs",
            ),
            // A restatement close enough for `partition_known` to divert it to
            // the mining log, so two turns yield one memory — the dedup path,
            // asserted rather than assumed.
            turn(
                T0 + DAY,
                "Register the tool in crates/stella-tools/src/registry.rs please",
            ),
        ]),
    );
    let (summary, _dir) = replay(&corpus).await;
    assert_eq!(summary.turns, 2);
    assert_eq!(
        summary.memories, 1,
        "the second turn restates the first, so only one memory should exist"
    );
    assert!(
        summary.model_errors.is_empty(),
        "neither turn should report an error: {:?}",
        summary.model_errors
    );
}

// ---- §5 assertion 1: distinct-task counting -------------------------------

/// A lesson recurring across **distinct tasks** yields artifacts; the same
/// lessons inside **one** task yield none.
///
/// This is the exact confusion `SessionMemory::set_task_id` exists to fix, and
/// the two halves differ in nothing but the task boundary — same texts, same
/// instants, same workspace.
#[tokio::test]
async fn recurrence_across_distinct_tasks_builds_what_recurrence_within_one_task_does_not() {
    let across = trace(
        "one convention, five tasks",
        one_task_each(recurring_registry_lessons()),
    );
    let (spread, _a) = replay(&across).await;

    let within = trace(
        "one convention, one task",
        all_one_task(recurring_registry_lessons()),
    );
    let (single, _b) = replay(&within).await;

    assert_eq!(spread.distinct_tasks, 5);
    assert_eq!(single.distinct_tasks, 1);
    assert_eq!(
        spread.memories, single.memories,
        "both corpora record the same lessons — only the task boundary differs"
    );
    assert_eq!(
        spread.memories, 4,
        "the fifth turn restates the first, so the store holds four distinct \
         memories while the mining log holds five occurrences — the #2358 \
         split, visible from the outside"
    );

    assert!(
        !spread.skills.is_empty(),
        "a naturally-worded convention recurring across distinct tasks should \
         clear the eligibility gate: {spread:?}"
    );
    assert!(
        !spread.rules.is_empty(),
        "and with four occurrences over four distinct tasks its confidence \
         clears the auto-activation bar: {spread:?}"
    );
    assert!(
        single.skills.is_empty(),
        "five turns of ONE task must not mint a skill — that is the \
         anti-poisoning rule, and it is the whole reason task identity is \
         carried at all. Built: {:?}",
        single.skills
    );
    assert!(single.rules.is_empty(), "nor a rule: {:?}", single.rules);
}

/// The lesson texts of [`recurring_registry_lessons`], for asserting on the
/// corpus's own strings rather than a copy that could drift from it.
fn registry_lesson_texts() -> Vec<String> {
    recurring_registry_lessons()
        .into_iter()
        .filter_map(|turn| match turn.reflection {
            ScriptedReflection::Lessons { mut lessons, .. } => lessons.pop().map(|l| l.lesson),
            _ => None,
        })
        .collect()
}

/// The #2358 contract, declared: store dedup and miner clustering are
/// **different predicates in different token spaces**, and each must hold its
/// own half of the band on the corpus above.
///
/// `stella_store::SIMILARITY_THRESHOLD` (0.5) is measured over
/// `forget::tokens`, which keeps a file path as one token; the miners'
/// `min_similarity` (0.4) is measured over `stella_core::mining::terms`, which
/// shatters the same path into five terms. The same pair of lessons scores
/// differently under each, so neither number may be "aligned" with the other
/// or moved without re-measuring in its own space — the measurements live on
/// [`SkillMineConfig::min_similarity`]'s doc comment. When one of them (or a
/// tokenizer) moved before, the mining band silently closed and the loop kept
/// mining lessons every turn while producing nothing durable, with no error
/// anywhere. This test is the tripwire: it fails if either side moves in a way
/// the other half was not re-measured against.
#[test]
fn the_dedup_and_clustering_predicates_hold_the_declared_relationship() {
    let mining = SkillMineConfig::default();
    assert_eq!(
        mining.min_similarity,
        stella_core::rules::MineConfig::default().min_similarity,
        "the skills and rules miners cluster one observation pool; a rule \
         threshold drifting from the skill threshold would make one artifact \
         class see recurrence the other cannot"
    );
    assert!(
        mining.min_similarity < stella_store::forget::SIMILARITY_THRESHOLD,
        "clustering must see recurrence in lessons the store keeps as \
         distinct: natural variation scores lower under `mining::terms` than \
         restatement scores under `forget::tokens`, so the clustering bar \
         sits below the dedup bar"
    );

    let texts = registry_lesson_texts();
    let (varied, restated) = (&texts[..4], texts[4].as_str());

    // The store half: the four natural wordings are distinct knowledge (each
    // gets stored), while the fifth restates the first (diverted to the log).
    for (index, wording) in varied.iter().enumerate() {
        for other in &varied[index + 1..] {
            assert!(
                !stella_store::is_restatement(wording, other),
                "the corpus's natural wordings must each survive dedup, or \
                 the store never learns them: {wording:?} vs {other:?}"
            );
        }
    }
    assert!(
        stella_store::is_restatement(&varied[0], restated),
        "the fifth wording must trip dedup — it exists to prove a deduped \
         restatement still reaches the miners"
    );

    // The miner half: clustering sees one convention in those same strings —
    // including the restatement dedup kept out of the store.
    let observations = texts
        .iter()
        .enumerate()
        .map(|(index, text)| SkillObservation {
            text: text.clone(),
            occurred_at: index as u64,
            domains: Vec::new(),
            salient: false,
            reference: format!("trace:{index}"),
        })
        .collect();
    let candidates = skills::mine_skill_candidates(observations, &[], &mining);
    let candidate = candidates
        .first()
        .expect("the naturally-worded convention must mine a candidate");
    assert!(
        candidate.occurrences >= mining.min_occurrences,
        "the cluster must clear the recurrence bar on its own: {candidate:?}"
    );
    assert!(
        candidate
            .evidence
            .iter()
            .any(|evidence| evidence.reference == "trace:4"),
        "the store-deduped restatement must count as a mining occurrence — \
         that ordering is the other half of the #2358 fix: {candidate:?}"
    );
}

// ---- §5 assertion 2: promotion is advisory, and never clobbers ------------

/// An auto-activated rule is **advisory** — `force = should`, no enforcement
/// block — so an inferred directive can never deny a tool call.
///
/// The published TOML is read back off disk rather than inspected in memory:
/// the file is what a teammate's session actually loads, and an assertion
/// against the in-memory record would pass even if the writer dropped a field.
#[tokio::test]
async fn an_auto_activated_rule_is_advisory_and_never_blocking() {
    let corpus = trace(
        "one convention, five tasks",
        one_task_each(recurring_registry_lessons()),
    );
    let (summary, dir) = replay(&corpus).await;
    assert!(
        !summary.rules.is_empty(),
        "expected an auto-activated rule: {summary:?}"
    );

    for name in &summary.rules {
        let path = crate::memory::rules_mining::workspace_rules_dir(&replay_root(&dir))
            .join(format!("{name}.toml"));
        let toml = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("published rule {} unreadable: {e}", path.display()));
        assert!(
            toml.contains("force = \"should\"") || toml.contains("force = 'should'"),
            "a mined rule must be advisory, not `must`: {toml}"
        );
        assert!(
            !toml.contains("[enforcement]"),
            "an inferred directive must never carry an enforcement block — that \
             is the line between suggesting a convention and denying a tool \
             call: {toml}"
        );
        assert!(
            toml.contains("inferred"),
            "and it must be labelled as inferred: {toml}"
        );
    }
}

/// Auto-activation never clobbers a file already on disk (#737).
///
/// A hand-written record at the path the miner would publish to must survive
/// the replay byte for byte.
#[tokio::test]
async fn auto_activation_never_clobbers_an_existing_record() {
    // First, learn where the miner publishes.
    let corpus = trace(
        "one convention, five tasks",
        one_task_each(recurring_registry_lessons()),
    );
    let (first, dir) = replay(&corpus).await;
    let name = first
        .rules
        .first()
        .cloned()
        .expect("the corpus should auto-activate a rule");
    drop(dir);

    // Now replay the same corpus into a workspace where that file already
    // exists, hand-written.
    let second = tempfile::tempdir().expect("tempdir");
    let root = replay_root(&second);
    let rules = crate::memory::rules_mining::workspace_rules_dir(&root);
    std::fs::create_dir_all(&rules).expect("rules dir");
    let occupied = rules.join(format!("{name}.toml"));
    let hand_written = "# hand-written; the miner must not touch this\n";
    std::fs::write(&occupied, hand_written).expect("seed the record");

    Replayer::run(&corpus, &root)
        .await
        .expect("the trace replays");

    assert_eq!(
        std::fs::read_to_string(&occupied).expect("still readable"),
        hand_written,
        "auto-activation overwrote a file it did not create"
    );
}

// ---- §5 assertion 3: skills need recurrence -------------------------------

/// A one-off lesson never becomes a skill, however good it is.
#[tokio::test]
async fn a_one_off_lesson_does_not_become_a_skill() {
    let corpus = trace(
        "four unrelated one-off lessons",
        one_task_each(vec![
            turn(
                T0,
                "Register the tool in crates/stella-tools/src/registry.rs",
            ),
            turn(
                T0 + DAY,
                "Budget aborts happen between model calls in crates/stella-core/src/driver.rs",
            ),
            turn(
                T0 + 2 * DAY,
                "SSE parsing for Anthropic sits in crates/stella-model/src/anthropic.rs",
            ),
            turn(
                T0 + 3 * DAY,
                "Golden deck frames are regenerated with BLESS=1 under stella-tui",
            ),
        ]),
    );
    let (summary, _dir) = replay(&corpus).await;
    assert_eq!(
        summary.memories, 4,
        "four distinct lessons should all be recorded"
    );
    assert!(
        summary.skills.is_empty(),
        "four unrelated lessons are four facts, not a recurring pattern: {:?}",
        summary.skills
    );
}

// ---- §5 assertion 4: the foundry needs variation *and* reuse --------------

/// A shape recurring with **≥2 distinct argument sets, each retyped**, yields a
/// tool proposal; the identical command repeated yields none.
///
/// The negative half is the interesting one: an exact repeat is loop detection's
/// business, not the foundry's, and `min_distinct_arguments: 2` draws that line.
/// The other end of the same line — variation with no reuse at all — is
/// [`a_shape_whose_arguments_never_repeat_is_not_minted`].
#[tokio::test]
async fn a_shape_with_varying_arguments_mints_a_tool_and_an_exact_repeat_does_not() {
    // Two argument sets, each reconstructed on all three days: the incantation
    // shape the foundry exists for, at 3.0× reuse.
    let recurring_greps = shell(&[
        "rg -n \"unwrap\" crates/stella-core/src/driver.rs",
        "rg -n \"expect\" crates/stella-cli/src/agent.rs",
    ]);
    let varied = trace(
        "one shape, two argument sets, each retyped three times",
        one_task_each(vec![
            TraceTurn {
                shell: recurring_greps.clone(),
                ..turn(T0, "Grepping for unwrap is how the panic audit starts")
            },
            TraceTurn {
                shell: recurring_greps.clone(),
                ..turn(
                    T0 + DAY,
                    "The agent loop lives in crates/stella-cli/src/agent.rs",
                )
            },
            TraceTurn {
                shell: recurring_greps,
                ..turn(
                    T0 + 2 * DAY,
                    "OpenAI's adapter is crates/stella-model/src/openai.rs",
                )
            },
        ]),
    );
    let (with_variation, _a) = replay(&varied).await;
    assert!(
        !with_variation.tools.is_empty(),
        "one shape reused across two argument sets should propose a tool: \
         {with_variation:?}"
    );

    let repeated = trace(
        "one shape, one argument set, three times",
        one_task_each(vec![
            TraceTurn {
                shell: shell(&["rg -n \"unwrap\" crates/stella-core/src/driver.rs"]),
                ..turn(T0, "Grepping for unwrap is how the panic audit starts")
            },
            TraceTurn {
                shell: shell(&["rg -n \"unwrap\" crates/stella-core/src/driver.rs"]),
                ..turn(
                    T0 + DAY,
                    "The agent loop lives in crates/stella-cli/src/agent.rs",
                )
            },
            TraceTurn {
                shell: shell(&["rg -n \"unwrap\" crates/stella-core/src/driver.rs"]),
                ..turn(
                    T0 + 2 * DAY,
                    "OpenAI's adapter is crates/stella-model/src/openai.rs",
                )
            },
        ]),
    );
    let (without_variation, _b) = replay(&repeated).await;
    assert!(
        without_variation.tools.is_empty(),
        "an exact repeat is a loop, not a parameterized tool: {:?}",
        without_variation.tools
    );
}

/// The other end of the same line: a shape whose arguments are *never* repeated
/// mints nothing, however often the program name recurs.
///
/// This is the half assertion 4 was missing. `min_distinct_arguments` is a floor
/// on variation, so unbounded variation cleared it — and against a real 81,684-
/// command history that asymmetry proposed 2,073 tools, almost all of them the
/// shell wearing a costume (#2378). Nine invocations here, nine argument sets,
/// zero proposals: `rg -n <str> <path>` at 1.0× reuse is `rg`, not a tool.
#[tokio::test]
async fn a_shape_whose_arguments_never_repeat_is_not_minted() {
    let files = [
        "crates/stella-core/src/driver.rs",
        "crates/stella-cli/src/agent.rs",
        "crates/stella-model/src/openai.rs",
    ];
    let turns: Vec<TraceTurn> = files
        .iter()
        .enumerate()
        .map(|(day, path)| {
            let commands: Vec<String> = ["unwrap", "expect", "panic"]
                .iter()
                .map(|needle| format!("rg -n \"{needle}-{day}\" {path}"))
                .collect();
            let borrowed: Vec<&str> = commands.iter().map(String::as_str).collect();
            TraceTurn {
                shell: shell(&borrowed),
                ..turn(
                    T0 + day as i64 * DAY,
                    "Grepping for unwrap is how the panic audit starts",
                )
            }
        })
        .collect();

    let (summary, _dir) = replay(&trace(
        "one shape, nine argument sets",
        one_task_each(turns),
    ))
    .await;
    assert!(
        summary.tools.is_empty(),
        "a shape with one argument set per use is the shell, not a tool: {:?}",
        summary.tools
    );
}

// ---- §5 assertion 5: forgetting is durable --------------------------------

/// A tombstoned lesson is never re-learned — **including when a later turn
/// restates it in different words.**
///
/// The paraphrase is the entire assertion. A corpus that tombstones a lesson and
/// then re-emits it *verbatim* proves almost nothing: byte-identical content
/// already collapses on its own through content-hash lineage. What needs proving
/// is `retain_unforgotten`'s restatement matching, so the corpus must paraphrase.
#[tokio::test]
async fn a_forgotten_lesson_is_not_re_learned_from_a_paraphrase() {
    let forgotten = "Register the tool in crates/stella-tools/src/registry.rs";
    let paraphrase = "Register the tool inside crates/stella-tools/src/registry.rs";

    let corpus = trace(
        "a lesson, forgotten, then restated",
        one_task_each(vec![
            turn(T0, forgotten),
            TraceTurn {
                forget: vec![forgotten.to_string()],
                ..turn(T0 + DAY, paraphrase)
            },
        ]),
    );
    let (summary, dir) = replay(&corpus).await;

    let texts = super::summary::memory_texts(&replay_root(&dir));
    assert!(
        !texts.iter().any(|text| text == paraphrase),
        "a paraphrase of a forgotten lesson was re-learned: {texts:?}"
    );
    assert_eq!(
        summary.lessons_recorded, 1,
        "only the pre-tombstone turn should have recorded anything"
    );
}

// ---- §5 assertion 6: retirement --------------------------------------------

/// A memory whose every path anchor has left the tree is retired by the
/// deterministic sweep, and lands in the set recall suppresses.
///
/// **Only testable since #2320.** The sweep's `now` used to come from the wall
/// clock, so no test could position a trace's timeline relative to it.
///
/// The assertion is against `retirement::retired_ids` — the exact set
/// `suppression::suppression_reader` unions into every recall — and *not*
/// against the store's live nodes. That distinction is the mechanism, not a
/// detail: retirement deliberately does **not** write `node.superseded_at`,
/// because that column is filtered by `node_by_public_id`, and projecting
/// retirement onto it would make a retired record impossible to fetch by id —
/// when staying explicitly retrievable is exactly what separates retirement
/// from forgetting. So a test asserting "the memory is gone from the store"
/// would fail against a perfectly working sweep, and one asserting "the count
/// went down" would quietly re-specify retirement as deletion.
#[tokio::test]
async fn a_memory_whose_files_all_vanished_is_retired() {
    let anchored = "Register the tool in crates/stella-tools/src/registry.rs";
    let corpus = trace(
        "a lesson about a file that is later deleted",
        one_task_each(vec![
            turn(T0, anchored),
            // Six simulated months later, the file is gone.
            TraceTurn {
                removed_files: vec![REGISTRY.to_string()],
                ..turn(
                    T0 + 180 * DAY,
                    "Budget aborts happen between model calls in crates/stella-core/src/driver.rs",
                )
            },
        ]),
    );
    let (_summary, dir) = replay(&corpus).await;
    let root = replay_root(&dir);

    let path = crate::memory::context_db_path(&root).expect("context db path");
    let store = stella_context::ContextStore::open_with(
        &path,
        std::sync::Arc::new(stella_context::HashEmbedder::default()),
        std::sync::Arc::new(stella_context::SystemClock),
    )
    .expect("context store");

    let vanished_node = store
        .memory_nodes()
        .expect("memory nodes")
        .into_iter()
        .find(|node| node.content == anchored)
        .expect("the anchored lesson should still be STORED — retirement is reversible");
    let retired = crate::memory::retirement::retired_ids(&store);

    assert!(
        retired.contains(&vanished_node.public_id),
        "a memory whose only anchor left the tree should be in the retired set \
         that recall suppresses. Retired: {retired:?}"
    );
}

// ---- §5 assertion 7: the starvation guard ---------------------------------

/// A corpus of unreadable reflections builds **nothing**, and the harness
/// **says so loudly**.
///
/// This is the most valuable assertion in the suite. `reflect_and_record`
/// returns `recorded: 0` both for "nothing worth learning" and for "I could not
/// read the response", and a clean zero on every turn looks exactly like an
/// agent that keeps getting things right. What separates them is that the
/// unreadable path *reports a reason*, so the summary must carry one per turn.
#[tokio::test]
async fn an_unreadable_corpus_builds_nothing_and_says_why() {
    let corpus = trace(
        "a model that narrates instead of answering",
        one_task_each(vec![
            unreadable_turn(T0, "Sure! Here is what I learned this turn: ..."),
            unreadable_turn(T0 + DAY, "Let me think about that step by step."),
            unreadable_turn(T0 + 2 * DAY, "I have reflected on the turn."),
            model_error_turn(T0 + 3 * DAY, "provider refused the reflection call"),
        ]),
    );
    let (summary, _dir) = replay(&corpus).await;

    assert_eq!(summary.memories, 0, "nothing should have been learned");
    assert!(summary.skills.is_empty());
    assert!(summary.rules.is_empty());
    assert_eq!(
        summary.model_errors.len(),
        4,
        "every starved turn must report why — a silent zero is \
         indistinguishable from a turn with nothing to teach: {summary:?}"
    );
    let report = summary.render();
    assert!(
        report.contains("taught the lifecycle nothing and said so"),
        "the rendered summary must surface starvation rather than printing a \
         tidy row of zeroes:\n{report}"
    );
}

/// The distinction the guard rests on: a turn with genuinely nothing to say is
/// **not** an error, and must not be reported as one.
#[tokio::test]
async fn an_empty_reflection_is_not_reported_as_starvation() {
    let empty = || TraceTurn {
        reflection: ScriptedReflection::Lessons {
            lessons: Vec::new(),
            provenance: Provenance::Recorded,
        },
        ..turn(T0, "")
    };
    let corpus = trace(
        "turns with nothing worth learning",
        one_task_each(vec![
            empty(),
            TraceTurn {
                at: T0 + DAY,
                ..empty()
            },
        ]),
    );
    let (summary, _dir) = replay(&corpus).await;
    assert_eq!(summary.memories, 0);
    assert!(
        summary.model_errors.is_empty(),
        "an empty lessons array is a legitimate 'nothing to record', not a \
         failure to read the model: {:?}",
        summary.model_errors
    );
}

// ---- §5 assertion 8: determinism -------------------------------------------

/// Two replays of one trace produce byte-identical summaries.
///
/// The determinism contract itself, and the reason the summary may carry no
/// `edge.public_id`: `EDGE_SEQ` is a process-global counter, so the same logical
/// edge is minted with different ids by two replays *inside one process*.
#[tokio::test]
async fn two_replays_of_one_trace_are_byte_identical() {
    let corpus = committed_corpus();
    let (first, _a) = replay(&corpus).await;
    let (second, _b) = replay(&corpus).await;
    assert_eq!(
        first, second,
        "two replays of one trace must build the same things"
    );
    assert_eq!(
        first.render(),
        second.render(),
        "and must render byte-identical reports"
    );
}

// ---- the committed corpus ---------------------------------------------------

fn committed_corpus() -> Trace {
    Trace::fixture("simulated-months").expect("the committed corpus loads")
}

/// The committed corpus replays and builds one of everything.
///
/// This is the epic's "done when": a synthetic simulated-months trace that
/// replays deterministically in CI, with the §6 metrics printed per run.
#[tokio::test]
async fn the_committed_corpus_builds_a_memory_a_skill_a_rule_and_a_tool() {
    let corpus = committed_corpus();
    let (summary, _dir) = replay(&corpus).await;
    println!("{}", summary.render());

    assert!(summary.turns >= 8, "a months corpus needs turns to span");
    assert!(
        summary.distinct_tasks >= 3,
        "and distinct tasks to count: {summary:?}"
    );
    assert!(summary.memories > 0, "memories: {summary:?}");
    assert!(!summary.skills.is_empty(), "skills: {summary:?}");
    assert!(!summary.rules.is_empty(), "rules: {summary:?}");
    assert!(!summary.tools.is_empty(), "tools: {summary:?}");
}

/// `make replay-learning`'s entry point — prints the §6 summary for the
/// committed corpus.
///
/// A test rather than a binary because `stella-cli` is bin-only, and a report is
/// not a pass/fail gate: it is deliberately **not** a `make gate` step, so it
/// adds no shared cell for the three gate documents to disagree about.
#[tokio::test]
async fn replay_learning_report() {
    let corpus = committed_corpus();
    let (summary, _dir) = replay(&corpus).await;
    println!("\n{}", summary.render());
}

/// The harness reads no ambient state — its workspace is all it sees.
///
/// The pattern `crates/stella-runtime/tests/no_ambient_reads.rs` already
/// enforces for the runtime, applied here because a replay that quietly picked
/// up the developer's `~/.stella` would produce a summary nobody else can
/// reproduce — passing locally and failing in CI, or worse, the other way round.
#[test]
fn the_harness_workspace_is_the_only_state_it_reads() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(
        !dir.path().join(".stella").exists(),
        "a fresh replay workspace starts empty"
    );
    assert_eq!(
        super::summary::memory_texts(dir.path()),
        Vec::<String>::new(),
        "and holds no memories before a trace has run"
    );
    assert_eq!(
        super::summary::published_rules(dir.path()),
        Vec::<String>::new(),
        "and no rules"
    );
}

/// A seed path may not escape the workspace root.
#[tokio::test]
async fn a_seed_path_cannot_escape_the_workspace() {
    let mut escaping = trace("escaping seed", one_task_each(vec![turn(T0, "a lesson")]));
    escaping
        .workspace
        .files
        .insert("../escaped.txt".to_string(), "no".to_string());
    let dir = tempfile::tempdir().expect("tempdir");
    let root = replay_root(&dir);
    assert!(
        matches!(
            Replayer::run(&escaping, &root).await,
            Err(TraceError::Unreplayable(_))
        ),
        "a committed fixture must not be able to write outside the temp root"
    );
    assert!(!escaped_path(&root).exists());
}

fn escaped_path(root: &Path) -> std::path::PathBuf {
    root.parent().unwrap_or(root).join("escaped.txt")
}
