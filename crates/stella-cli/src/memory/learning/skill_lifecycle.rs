//! The skill lifecycle twin — the end-to-end shape [`super::guarantees`]
//! pins for rules, pointed at skills.
//!
//! The rules pipeline has a mine→write→load→apply twin; the skill
//! pipeline's miner, store reload and `select_skills` were only ever tested
//! in isolation, so nothing end-to-end proved a mined skill is the same
//! skill a later session selects. These tests close that, and extend the
//! twin over the newly wired halves: the measured promotion gate holding
//! and then releasing a candidate, and the retirement sweep demoting a
//! skill that stops helping.
//!
//! Every fixture drives the production doors — `auto_create_skills`,
//! `note_turn_skills`, `record_episode` — never the pure functions behind
//! them, because the pure halves are already pinned in `stella-core` and
//! the seams between them are exactly the part that spent years unwired.

use std::path::Path;

use stella_context::EpisodeOutcome;
use stella_core::self_tuning::TaskOutcome;
use stella_core::skills::appraisal::{AppraisalConfig, SkillTrial, SkillVerdict, appraise};

use crate::memory::{ReflectionLesson, SessionMemory, appraisals};

/// The recurring lesson, worded so the mined skill's description shares
/// enough vocabulary with [`MATCHING_PROMPT`] for `select_skills` to fire.
const LESSON: &str = "Prefer updating witness-test assertions to match the live renderer output.";

/// A prompt about the lesson's own subject — what a matching task looks like.
const MATCHING_PROMPT: &str =
    "update the witness test assertions so they match the live renderer output";

/// A prompt sharing no scoring vocabulary with the skill — the control arm.
const UNRELATED_PROMPT: &str = "hello";

/// A workspace whose mining log holds three occurrences of [`LESSON`] —
/// exactly the shipped `min_occurrences` threshold.
fn workspace_with_log() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let private = dir.path().join(".stella/private");
    std::fs::create_dir_all(&private).expect("private dir");
    let mut log = String::new();
    for at in [100u64, 200, 300] {
        let lesson = ReflectionLesson {
            lesson: LESSON.to_string(),
            domains: vec!["testing".into()],
            occurred_at: at,
            task_id: String::new(),
            trigger: String::new(),
            saves: String::new(),
            kind: crate::memory::LessonKind::Process,
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

/// Write the session settings: the lifecycle stays at its shipped default,
/// and the measured gate is set per case — `false` is the bootstrap mode,
/// `true` the shipped default, written explicitly so each test names the mode
/// it runs under.
fn set_gate(root: &Path, require_measured_lift: bool) {
    let dir = root.join(".stella");
    std::fs::create_dir_all(&dir).expect("stella dir");
    std::fs::write(
        dir.join("settings.json"),
        format!(
            r#"{{"context":{{"promotion":{{"skill":{{"require_measured_lift":{require_measured_lift}}}}}}}}}"#
        ),
    )
    .expect("write settings");
}

fn session(root: &Path) -> SessionMemory {
    SessionMemory::open_with_workspace_skills(root, false, true).expect("session memory")
}

/// Every `.md` under the workspace skills dir, sorted.
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

/// **The twin: mine → write → load → select.** A recurring lesson is
/// mined into a `SKILL.md`, a *fresh* session reloads the store from disk,
/// and `select_skills` returns the mined skill for a task about its subject —
/// with the unrelated-prompt control proving the selection is a match, not a
/// constant.
///
/// Bootstrap mode (`require_measured_lift: false`), because this twin pins
/// the pipeline's plumbing; the gate has its own twin below.
#[test]
fn a_mined_skill_lands_reloads_and_is_selected_for_a_matching_task() {
    let dir = workspace_with_log();
    set_gate(dir.path(), false);

    let mut memory = session(dir.path());
    memory.auto_create_skills(&log_path(dir.path()), true);
    let written = skill_files(dir.path());
    assert_eq!(written.len(), 1, "the recurring lesson mined one skill");
    let name = written[0].trim_end_matches(".md").to_string();

    // A fresh session — the reload a real next-day session performs.
    let later = session(dir.path());
    let selected = later.selected_skills(MATCHING_PROMPT);
    assert!(
        selected.iter().any(|(n, _)| *n == name),
        "the mined skill must be selected for a matching task: {selected:?}"
    );
    assert!(
        later.selected_skills(UNRELATED_PROMPT).is_empty(),
        "an unrelated prompt must select nothing, or the assertion above proves nothing"
    );
}

/// **The measured gate, both modes.** Under the shipped default a
/// mined candidate is HELD — queued, not written — and a recorded
/// `MeasuredLift` appraisal is what turns it into a file, through the queue
/// alone: the mining log is emptied before the second pass, so the miner
/// cannot re-raise the candidate and `queued_candidates` is the only door the
/// skill can arrive through.
#[test]
fn the_measured_gate_holds_a_candidate_until_a_recorded_lift_promotes_it() {
    let dir = workspace_with_log();
    set_gate(dir.path(), true);

    let mut memory = session(dir.path());
    memory.auto_create_skills(&log_path(dir.path()), true);
    assert_eq!(
        skill_files(dir.path()),
        Vec::<String>::new(),
        "the gate must hold an unevaluated candidate"
    );
    let queued = appraisals::queued_candidates(dir.path());
    assert_eq!(queued.len(), 1, "held is queued, not dropped: {queued:?}");
    let candidate = &queued[0];
    assert!(
        candidate.body.is_some(),
        "the queue carries the body so an appraisal can promote without re-mining"
    );

    // A real appraisal over trials that show a lift — the same engine the
    // sweep runs, recorded through the same ledger writer.
    let mut trials: Vec<SkillTrial> = Vec::new();
    for selected in [false, true] {
        for _ in 0..6 {
            trials.push(SkillTrial {
                task: "fix-git".to_string(),
                selected,
                outcome: TaskOutcome {
                    succeeded: selected,
                    cost_usd: 0.0,
                    tokens: 0,
                    retries: 0,
                },
                turns: 1,
            });
        }
    }
    let appraisal = appraise(&candidate.name, &trials, &AppraisalConfig::default());
    assert!(
        matches!(appraisal.verdict, SkillVerdict::Helps { .. }),
        "the fixture must measure a lift: {:?}",
        appraisal.verdict
    );
    appraisals::record_appraisal(dir.path(), &appraisal);

    // Empty the mining log: the queue is now the only path to a file.
    std::fs::write(log_path(dir.path()), "").expect("truncate log");
    let mut later = session(dir.path());
    later.auto_create_skills(&log_path(dir.path()), true);
    assert_eq!(
        skill_files(dir.path()),
        vec![format!("{}.md", candidate.name)],
        "a recorded lift promotes the held candidate from the queue"
    );
}

/// **The retirement half: promote → three negative appraisals →
/// demoted → not selected.** The skill is minted through the real miner,
/// selected and injected through the real turn seams, measured through the
/// real trial ledger, and demoted by the real sweep — then a fresh session
/// proves the demotion is durable: the skill is out of selection while its
/// file and the append-only ledger row both survive.
#[tokio::test]
async fn a_promoted_skill_that_stops_helping_is_demoted_and_no_longer_selected() {
    let dir = workspace_with_log();
    set_gate(dir.path(), false);

    // Promote: the miner writes the skill.
    let mut memory = session(dir.path());
    memory.auto_create_skills(&log_path(dir.path()), true);
    let written = skill_files(dir.path());
    assert_eq!(written.len(), 1, "the lesson promoted into a skill");
    let name = written[0].trim_end_matches(".md").to_string();

    // Live turns through the production seams: eight matching turns where
    // the skill was injected and the turn failed, eight unrelated turns
    // where it was not and the turn succeeded. The unselected turns are the
    // control arm — record_turn's whole reason to take the offered set.
    let memory = session(dir.path());
    for _ in 0..8 {
        let selected = memory.note_turn_skills(MATCHING_PROMPT);
        assert!(
            selected.iter().any(|(n, _)| *n == name),
            "the fixture's with-skill arm must actually select the skill: {selected:?}"
        );
        memory
            .record_episode(MATCHING_PROMPT, EpisodeOutcome::Failure, &[], 1_000, None)
            .await;

        assert!(
            memory.note_turn_skills(UNRELATED_PROMPT).is_empty(),
            "the without-skill arm must not select it"
        );
        memory
            .record_episode(UNRELATED_PROMPT, EpisodeOutcome::Success, &[], 1_000, None)
            .await;
    }

    // Three reflection passes: each sweep re-appraises the window and records
    // the negative verdict; the third consecutive one demotes (the shipped
    // `demote_after_consecutive_negatives`).
    let mut memory = session(dir.path());
    for pass in 1..=3 {
        memory.auto_create_skills(&log_path(dir.path()), true);
        let negatives = appraisals::consecutive_negative_appraisals(dir.path(), &name);
        assert_eq!(
            negatives, pass,
            "each sweep records exactly one negative appraisal"
        );
    }
    assert!(
        appraisals::demoted_skills(&memory.store).contains(&name),
        "three consecutive negatives demote the skill"
    );

    // Durable, and demotion is not deletion: a fresh session excludes the
    // skill from loading and selection while the file stays on disk.
    let later = session(dir.path());
    assert!(
        later.selected_skills(MATCHING_PROMPT).is_empty(),
        "a demoted skill must not be selected for the task that matches it"
    );
    assert!(
        !later.load_skills().iter().any(|s| s.name == name),
        "a demoted skill is out of the loaded set"
    );
    assert_eq!(
        skill_files(dir.path()),
        vec![format!("{name}.md")],
        "demotion leaves the file in place — restore must survive it"
    );

    // The reason is on the record, in the append-only ledger.
    let events = crate::proposals_cmd::promotion_events(&later.store);
    assert!(
        events.iter().any(|e| {
            e.proposal_lineage_id == format!("skill:{name}")
                && e.action == stella_core::context_record::PromotionAction::Retired
        }),
        "the demotion is a Retired promotion_event against the skill's lineage: {events:?}"
    );

    // And a later mining pass cannot resurrect it: the file occupies the
    // path, so the no-clobber guard holds the door shut.
    let mut resurrect = session(dir.path());
    resurrect.auto_create_skills(&log_path(dir.path()), true);
    assert!(
        !session(dir.path())
            .load_skills()
            .iter()
            .any(|s| s.name == name),
        "a re-mine of the same lesson must not undo the demotion"
    );
}
