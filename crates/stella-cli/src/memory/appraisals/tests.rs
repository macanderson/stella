//! Tests for the skill-appraisal ledger: the gate's evidence source, the
//! held-candidate queue, and the retirement sweep.

use stella_learn::ledger::ArtifactKind;
use stella_learn::self_tuning::TaskOutcome;
use stella_learn::skills::SkillOrigin;
use stella_learn::skills::appraisal::{DemotionReason, KeepSkillReason, SkillVerdict};

use super::*;

fn workspace(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "stella-appraisal-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    root
}

fn trial(succeeded: bool) -> SkillTrial {
    SkillTrial {
        task: "live".into(),
        selected: false,
        outcome: TaskOutcome {
            succeeded,
            cost_usd: 0.10,
            tokens: 1_000,
            retries: 0,
        },
        turns: 4,
    }
}

fn candidate(name: &str) -> SkillCandidate {
    SkillCandidate {
        name: name.into(),
        description: "Learned from 3 observations.".into(),
        domains: vec!["testing".into()],
        body: "b".into(),
        occurrences: 3,
        salient: false,
        evidence: vec![],
        score: 30.0,
    }
}

/// A missing ledger is an empty window, never an error — and the gate reads
/// that as `Unevaluated`, which is what stops an unproven candidate.
#[test]
fn an_absent_ledger_yields_no_verdicts() {
    let root = workspace("absent");
    assert!(latest_verdicts(&root).is_empty());
    assert!(queued_candidates(&root).is_empty());
    assert!(
        sweep(
            &root,
            ArtifactKind::Skill,
            &HashMap::new(),
            &AppraisalConfig::default()
        )
        .is_empty()
    );
}

/// The newest appraisal per skill wins: a skill that stopped helping must not
/// be held up by the run that once said it did.
#[test]
fn the_newest_verdict_per_skill_wins() {
    let root = workspace("newest");
    let helping = SkillAppraisal {
        skill: "s".into(),
        verdict: SkillVerdict::Helps { lift: 1.0 },
        report: stella_learn::comparison::compare(
            &[],
            &stella_learn::comparison::ComparisonConfig::new(
                "without_skill",
                stella_learn::comparison::Metric::PassRate,
            ),
        ),
        harm: None,
    };
    let stale = SkillAppraisal {
        verdict: SkillVerdict::Inert { trials: 40 },
        ..helping.clone()
    };
    record_appraisal(&root, &helping);
    assert_eq!(
        latest_verdicts(&root).get("s"),
        Some(&EvalEvidence::MeasuredLift)
    );
    record_appraisal(&root, &stale);
    assert_eq!(latest_verdicts(&root).get("s"), Some(&EvalEvidence::NoLift));
    let _ = std::fs::remove_dir_all(&root);
}

/// A held candidate is queued once, however many sessions re-raise it. The
/// miner re-mines the same identity every session, and an unbounded queue of
/// one repeated line is a log, not a queue.
#[test]
fn a_held_candidate_is_queued_exactly_once() {
    let root = workspace("queue");
    for _ in 0..3 {
        queue_candidate(
            &root,
            &candidate("prefer-tables-abc123"),
            EvalEvidence::Unevaluated,
            true,
        );
    }
    let queued = queued_candidates(&root);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].name, "prefer-tables-abc123");
    assert_eq!(queued[0].occurrences, 3);
    assert_eq!(queued[0].evidence, EvalEvidence::Unevaluated);
    let _ = std::fs::remove_dir_all(&root);
}

/// The join is recorded for every *trigger-matched* skill, not only the
/// injected ones: the uninjected turns are the control arm, and a ledger of
/// injections alone can only measure a skill against itself.
#[test]
fn the_turn_join_records_the_control_arm_too() {
    let root = workspace("join");
    let matched = vec!["helper".to_string(), "bystander".to_string()];
    record_turn(
        &root,
        ArtifactKind::Skill,
        &matched,
        &["helper".to_string()],
        &trial(true),
    );
    record_turn(&root, ArtifactKind::Skill, &matched, &[], &trial(false));

    let stored = stored_trials(&root);
    assert_eq!(stored.len(), 4, "two skills × two turns");
    assert!(
        stored.iter().all(|s| s.kind == ArtifactKind::Skill),
        "the skill seam writes skill rows"
    );
    let helper: Vec<&ArtifactTrial> = stored.iter().filter(|s| s.id == "helper").collect();
    assert_eq!(helper.len(), 2);
    assert!(helper[0].trial.selected, "turn one injected it");
    assert!(!helper[1].trial.selected, "turn two is its control");
    let bystander: Vec<&ArtifactTrial> = stored.iter().filter(|s| s.id == "bystander").collect();
    assert!(bystander.iter().all(|s| !s.trial.selected));
    let _ = std::fs::remove_dir_all(&root);
}

/// #1068 end to end: a regressing auto-created skill is demoted with its
/// evidence window, while a hand-authored skill with the identical window is
/// left alone.
#[test]
fn the_sweep_demotes_only_the_auto_created_regressor() {
    let root = workspace("sweep");
    let known = vec!["auto-stale".to_string(), "house-style".to_string()];
    // Eight turns with both skills injected and failing, eight without them
    // and passing: removing either confidently wins.
    for _ in 0..8 {
        record_turn(&root, ArtifactKind::Skill, &known, &known, &trial(false));
        record_turn(&root, ArtifactKind::Skill, &known, &[], &trial(true));
    }

    let origins = HashMap::from([
        ("auto-stale".to_string(), SkillOrigin::AutoCreated),
        ("house-style".to_string(), SkillOrigin::Workspace),
    ]);
    let swept = sweep(
        &root,
        ArtifactKind::Skill,
        &origins,
        &AppraisalConfig::default(),
    );
    assert_eq!(swept.len(), 2);
    // Sorted by skill name, so the order is not a hash seed's.
    let (auto, auto_decision) = &swept[0];
    assert_eq!(auto.skill, "auto-stale");
    assert!(matches!(auto.verdict, SkillVerdict::Harms { .. }));
    assert!(matches!(
        auto_decision,
        DemotionDecision::Demote {
            reason: DemotionReason::Harmful { .. }
        }
    ));
    let (hand, hand_decision) = &swept[1];
    assert_eq!(hand.skill, "house-style");
    assert!(
        matches!(hand.verdict, SkillVerdict::Harms { .. }),
        "the window regresses identically"
    );
    assert_eq!(
        hand_decision,
        &DemotionDecision::Keep {
            reason: KeepSkillReason::HandAuthored
        },
        "a hand-authored skill is never auto-demoted"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A skill whose provenance the sweep cannot establish is treated as
/// hand-authored. Unknown provenance must never be enough to retire something.
#[test]
fn an_unknown_origin_is_never_demoted() {
    let root = workspace("unknown-origin");
    let known = vec!["mystery".to_string()];
    for _ in 0..8 {
        record_turn(&root, ArtifactKind::Skill, &known, &known, &trial(false));
        record_turn(&root, ArtifactKind::Skill, &known, &[], &trial(true));
    }
    let swept = sweep(
        &root,
        ArtifactKind::Skill,
        &HashMap::new(),
        &AppraisalConfig::default(),
    );
    assert_eq!(swept.len(), 1);
    assert_eq!(
        swept[0].1,
        DemotionDecision::Keep {
            reason: KeepSkillReason::HandAuthored
        }
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A line an older build wrote is skipped, not fatal: these ledgers outlive
/// the shapes that wrote them.
#[test]
fn an_unreadable_line_costs_only_itself() {
    let root = workspace("tolerant");
    queue_candidate(&root, &candidate("good-one"), EvalEvidence::NoLift, true);
    let target = path(&root, QUEUE_FILE);
    let mut raw = std::fs::read_to_string(&target).unwrap();
    raw.insert_str(0, "{\"from\":\"a future build\"}\nnot json at all\n");
    std::fs::write(&target, raw).unwrap();

    let queued = queued_candidates(&root);
    assert_eq!(queued.len(), 1, "the readable line survives");
    assert_eq!(queued[0].name, "good-one");
    let _ = std::fs::remove_dir_all(&root);
}

/// The ledgers are owner-only, like every other file under
/// `.stella/private/`.
#[cfg(unix)]
#[test]
fn the_ledgers_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let root = workspace("perms");
    queue_candidate(&root, &candidate("x"), EvalEvidence::Unevaluated, true);
    let mode = std::fs::metadata(path(&root, QUEUE_FILE))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    let _ = std::fs::remove_dir_all(&root);
}

/// **The witness for the shared key.** Two artifacts of different kinds can
/// share an id, and the sweep for one kind must not read the other's turns.
///
/// This cannot be written against the ledger that came before: a row was
/// filed under a skill name and nothing else, so there was no way to record a
/// memory trial at all, and no second kind for a skill sweep to pick up by
/// mistake. The memory rows below say the opposite of the skill rows, so a
/// sweep that mixed them would find no lift instead of harm.
#[test]
fn a_sweep_reads_only_its_own_kind() {
    let root = workspace("kinds");
    let shared = vec!["shared-id".to_string()];
    for _ in 0..8 {
        // The skill hurts: injected turns fail, withheld turns pass.
        record_turn(&root, ArtifactKind::Skill, &shared, &shared, &trial(false));
        record_turn(&root, ArtifactKind::Skill, &shared, &[], &trial(true));
        // The memory under the same id does the reverse.
        record_turn(&root, ArtifactKind::Memory, &shared, &shared, &trial(true));
        record_turn(&root, ArtifactKind::Memory, &shared, &[], &trial(false));
    }

    let origins = HashMap::from([("shared-id".to_string(), SkillOrigin::AutoCreated)]);
    let skills = sweep(
        &root,
        ArtifactKind::Skill,
        &origins,
        &AppraisalConfig::default(),
    );
    assert_eq!(skills.len(), 1);
    assert!(
        matches!(skills[0].0.verdict, SkillVerdict::Harms { .. }),
        "the skill's own turns say it hurts: {:?}",
        skills[0].0.verdict
    );

    let memories = sweep(
        &root,
        ArtifactKind::Memory,
        &origins,
        &AppraisalConfig::default(),
    );
    assert_eq!(memories.len(), 1);
    assert!(
        matches!(memories[0].0.verdict, SkillVerdict::Helps { .. }),
        "and the memory's say it helps: {:?}",
        memories[0].0.verdict
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A workspace with history keeps it. The rows a build before this one wrote
/// live in another file, under another field name, with no kind on them —
/// and the skill sweep still reads them as the skill turns they are.
#[test]
fn the_sweep_still_reads_the_rows_an_older_build_wrote() {
    let root = workspace("legacy");
    let mut lines = String::new();
    for _ in 0..8 {
        for (selected, succeeded) in [(true, false), (false, true)] {
            lines.push_str(&format!(
                r#"{{"skill":"old-timer","task":"live-window","selected":{selected},"outcome":{{"succeeded":{succeeded},"cost_usd":0.1,"tokens":1000,"retries":0}},"turns":4}}"#
            ));
            lines.push('\n');
        }
    }
    let target = path(&root, LEGACY_TRIALS_FILE);
    std::fs::create_dir_all(target.parent().expect("a parent")).expect("the private dir");
    std::fs::write(&target, lines).expect("the legacy ledger");

    let origins = HashMap::from([("old-timer".to_string(), SkillOrigin::AutoCreated)]);
    let swept = sweep(
        &root,
        ArtifactKind::Skill,
        &origins,
        &AppraisalConfig::default(),
    );
    assert_eq!(swept.len(), 1, "the old file is still a window");
    assert_eq!(swept[0].0.skill, "old-timer");
    assert!(matches!(swept[0].0.verdict, SkillVerdict::Harms { .. }));
    assert!(swept[0].1.is_demotion());
    let _ = std::fs::remove_dir_all(&root);
}
