//! Tests for the wired rules miner.
//!
//! The decisive one is `an_inferred_rule_is_never_blocking`: a rule
//! carrying a `RuleGuard` is Tier 2, and `evaluate_guards` denies the tool call.
//! Minting one from inference is precisely what the gate criterion "no inferred
//! directive reaches blocking by any path" forbids, and it is the thing wiring
//! this miner could most easily have got wrong.

use super::*;
use stella_context::ContextStore;
use stella_learn::rules::RuleGuard;
use stella_records::context_record::ObservationSource;

fn store() -> (tempfile::TempDir, ContextStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ContextStore::open(dir.path().join("context.db")).expect("open");
    (dir, store)
}

fn observation(text: &str, turn: u64) -> ObservationRecord {
    ObservationRecord::new(
        ObservationSource::ReflectionLesson,
        format!("reflection:{turn}"),
        format!("turn:{turn}"),
        text,
        vec!["tooling".into()],
        false,
        stella_context::format_rfc3339(turn as i64),
    )
    .expect("observation")
}

const LESSON: &str = "Always run the database migration before starting the integration suite.";

fn across_turns(text: &str, turns: &[u64]) -> Vec<ObservationRecord> {
    turns.iter().map(|t| observation(text, *t)).collect()
}

/// Write a candidate on proof that pays for a rule.
///
/// Said once here, not at every call. The gate turns down a rule mined from
/// prose. So a test about a rule on disk hands the writer a measured grade.
/// The no has its own tests below.
fn publish(root: &Path, candidate: &RuleCandidate) -> RulePublication {
    write_rule(
        root,
        candidate,
        Some(ProvenanceGrade::EnvironmentObservation),
        PublicationAuthority::Agent,
    )
    .expect("publishable")
}

/// The path a published rule landed at, or a panic naming what happened
/// instead.
fn published_path(root: &Path, candidate: &RuleCandidate) -> std::path::PathBuf {
    match publish(root, candidate) {
        RulePublication::Written(path) => path,
        other => panic!("expected a written rule, got {other:?}"),
    }
}

// ---- GATE: no inferred directive reaches blocking by any path ----

/// A mined rule is prompt-only, whatever the miner inferred.
///
/// The guard is stripped unconditionally rather than relying on the fact that
/// reflection observations carry no files today: `infer_guard` only fires for
/// `memory_kind == "gotcha"` with file evidence, so it currently returns `None`
/// by accident of the input. A future observation source with files would start
/// arming guards silently, which is why this asserts the *stripping*, by
/// handing `without_inferred_guard` a candidate that definitely has one.
#[test]
fn an_inferred_rule_is_never_blocking() {
    let with_guard = RuleCandidate {
        id: "some-lesson-abcd1234".into(),
        text: LESSON.into(),
        description: "d".into(),
        occurrences: 3,
        salient: false,
        evidence: Vec::new(),
        guard: Some(RuleGuard {
            tool: None,
            deny_path_glob: Some("src/**".into()),
            deny_command_glob: None,
            allow_command_glob: None,
        }),
        score: 30,
    };
    let stripped = without_inferred_guard(with_guard);
    assert!(
        stripped.guard.is_none(),
        "an inferred guard survived — this rule would deny tool calls"
    );

    // …and the published record carries no enforcement section at all, so
    // nothing downstream can arm a guard from it. Asserting the record rather
    // than just the struct: the record is what actually reaches the loader.
    let record = crate::context_records::inferred_rule_record(
        "acme.web",
        &stripped.id,
        &stripped.text,
        "observation",
        "proposal:rule:some-lesson-abcd1234",
        None,
    )
    .expect("a publishable record");
    assert!(
        record.enforcement.is_none(),
        "an inferred record must not carry enforcement"
    );
}

/// The same, through the whole induction path.
#[test]
fn induced_rules_carry_no_guard() {
    let (_dir, store) = store();
    let observations = across_turns(LESSON, &[100, 200, 300]);
    let induced = induce_rule_proposals(&store, &observations, &[], &MineConfig::default());
    assert_eq!(induced.len(), 1);
    assert!(induced[0].candidate.guard.is_none());
}

/// And the file the loop writes is Tier 1 too — the end of the path, not the
/// middle of it.
#[test]
fn a_written_rule_file_is_prompt_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let candidate = RuleCandidate {
        id: "written-rule-abcd1234".into(),
        text: LESSON.into(),
        description: "d".into(),
        occurrences: 3,
        salient: false,
        evidence: Vec::new(),
        guard: Some(RuleGuard {
            tool: Some("Bash".into()),
            deny_path_glob: None,
            deny_command_glob: Some("rm *".into()),
            allow_command_glob: None,
        }),
        score: 30,
    };
    let path = published_path(dir.path(), &candidate);
    assert_eq!(
        path.extension().and_then(|e| e.to_str()),
        Some("toml"),
        "mined rules publish as TOML context records"
    );
    let contents = std::fs::read_to_string(&path).expect("read back");
    let loaded = stella_records::records::load_context_file(&path.display().to_string(), &contents)
        .expect("the published record loads");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].record.statement, LESSON);
    assert!(
        loaded[0].record.enforcement.is_none(),
        "a guard reached disk: {contents}"
    );
}

// ---- GATE: distinct tasks, never raw events ----

#[test]
fn three_distinct_tasks_induce_one_eligible_rule_proposal() {
    let (_dir, store) = store();
    let observations = across_turns(LESSON, &[100, 200, 300]);
    let induced = induce_rule_proposals(&store, &observations, &[], &MineConfig::default());
    assert_eq!(induced.len(), 1);
    assert_eq!(induced[0].proposal.score.distinct_tasks, 3);
    assert!(induced[0].proposal.is_eligible(3, 3));
    assert_eq!(
        induced[0].proposal.proposal_kind,
        RecordProposalKind::Directive
    );
}

#[test]
fn thirty_repetitions_inside_one_task_induce_no_eligible_rule() {
    let (_dir, store) = store();
    let observations: Vec<_> = (0..30)
        .map(|i| observation(&format!("{LESSON} Restatement {i} of the same point."), 100))
        .collect();
    let induced = induce_rule_proposals(&store, &observations, &[], &MineConfig::default());
    assert!(!induced.is_empty(), "the miner still clusters them");
    for rule in &induced {
        assert_eq!(rule.proposal.score.distinct_tasks, 1);
        assert!(!rule.proposal.is_eligible(3, 3));
    }
}

/// A rule proposal at the default bar scores 70, below
/// `auto_activate_at_confidence` (85), so the common case is review rather than
/// silent activation. Directives steer; skills only inform.
#[test]
fn a_three_task_rule_does_not_reach_the_auto_activation_bar() {
    let (_dir, store) = store();
    let induced = induce_rule_proposals(
        &store,
        &across_turns(LESSON, &[100, 200, 300]),
        &[],
        &MineConfig::default(),
    );
    let promotion = crate::settings::InferredDirectivePromotion::default();
    assert_eq!(promotion.auto_activate_at_confidence, 85);
    assert!(
        induced[0].proposal.confidence.get() < promotion.auto_activate_at_confidence,
        "a three-task rule auto-activated at confidence {}",
        induced[0].proposal.confidence.get()
    );
}

/// Stronger evidence does clear it, so the bar is a bar and not a wall.
#[test]
fn a_four_task_rule_reaches_the_auto_activation_bar() {
    let (_dir, store) = store();
    let induced = induce_rule_proposals(
        &store,
        &across_turns(LESSON, &[100, 200, 300, 400, 500]),
        &[],
        &MineConfig::default(),
    );
    assert!(
        induced[0].proposal.confidence.get()
            >= crate::settings::InferredDirectivePromotion::default().auto_activate_at_confidence
    );
}

// ---- the shared-miner property the whole exercise is about ----

/// Both miners derive the SAME `<slug>-<hash8>` for one lesson, because they
/// share `stella_learn::mining`. That is the shared module working — and it is
/// exactly why the proposal lineage has to be namespaced by kind, or declining
/// the rule would silently decline the skill.
#[test]
fn both_miners_agree_on_identity_and_their_proposals_stay_distinct() {
    let (_dir, store) = store();
    let observations = across_turns(LESSON, &[100, 200, 300]);

    let rule = induce_rule_proposals(&store, &observations, &[], &MineConfig::default());
    let skill = crate::memory::proposals::induce_proposals(
        &store,
        &observations,
        &[],
        &[],
        &stella_learn::skills::SkillMineConfig::default(),
    );

    assert_eq!(
        rule[0].candidate.id, skill[0].candidate.name,
        "the shared clustering module must mint one identity for one lesson"
    );
    assert_ne!(
        rule[0].proposal.lineage_id, skill[0].proposal.lineage_id,
        "…but the two proposals must not collide onto one lineage"
    );
    assert!(rule[0].proposal.lineage_id.contains("directive"));
    assert!(skill[0].proposal.lineage_id.contains("knowledge"));
}

// ---- no-clobber ----

/// Never overwrite a rule the user wrote. Checks the FILESYSTEM rather than a
/// loaded list — the defect #737 describes on the skills side is a
/// list-membership guard, and a new call site should not inherit it.
#[test]
fn writing_never_clobbers_an_existing_rule_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rules_dir = workspace_rules_dir(dir.path());
    std::fs::create_dir_all(&rules_dir).expect("dir");
    let path = rules_dir.join("mine-abcd1234.md");
    let hand_written = "---\ndescription: my own rule\n---\n\nMy own words.\n";
    std::fs::write(&path, hand_written).expect("write");

    let candidate = RuleCandidate {
        id: "mine-abcd1234".into(),
        text: LESSON.into(),
        description: "mined".into(),
        occurrences: 3,
        salient: false,
        evidence: Vec::new(),
        guard: None,
        score: 30,
    };
    assert_eq!(
        publish(dir.path(), &candidate),
        RulePublication::AlreadyPresent,
        "the writer claimed to write over an existing file"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        hand_written,
        "a hand-written rule was clobbered"
    );
}

/// A rule already on disk is not re-proposed — the miner's own text guard,
/// fed by the unfiltered loader so it can actually see the file.
#[test]
fn a_rule_already_on_disk_is_not_re_proposed() {
    let (_dir, store) = store();
    let existing = vec![Rule {
        id: "existing".into(),
        description: "d".into(),
        text: LESSON.into(),
        guard: None,
        source: ".stella/rules/existing.md".into(),
    }];
    let induced = induce_rule_proposals(
        &store,
        &across_turns(LESSON, &[100, 200, 300]),
        &existing,
        &MineConfig::default(),
    );
    assert!(induced.is_empty(), "an existing rule was re-proposed");
}

/// Mined rules land where `FsRuleSource` actually reads from, which is the only
/// thing that makes them take effect.
#[test]
fn mined_rules_land_where_the_loader_reads() {
    let dir = tempfile::tempdir().expect("tempdir");
    let candidate = RuleCandidate {
        id: "landing-abcd1234".into(),
        text: LESSON.into(),
        description: "d".into(),
        occurrences: 3,
        salient: false,
        evidence: Vec::new(),
        guard: None,
        score: 30,
    };
    published_path(dir.path(), &candidate);

    // The unfiltered loader keys raw files by filename, and a record file's
    // name is its lineage — assert the lesson landed under it, which is the
    // visibility the miner's own no-clobber dedup depends on.
    let loaded = crate::rules::load_workspace_rules_unfiltered(dir.path());
    assert!(
        loaded
            .iter()
            .any(|r| r.id.contains("landing-abcd1234") && r.text.contains("database migration")),
        "a mined rule did not reach the loader: {:?}",
        loaded.iter().map(|r| &r.id).collect::<Vec<_>>()
    );
}

/// **The hop out of the ledger** (#2782): a rule published from a proposal
/// carries that proposal's evidence grade onto the record on disk.
///
/// This is the boundary where provenance was previously lost for good. A
/// proposal keeps its observations' ids; the published record keeps neither,
/// so a grade not written here cannot be recovered by anything downstream —
/// including the human reading the rule file to decide whether to trust it.
#[test]
fn a_published_rule_carries_the_grade_of_the_proposal_it_came_from() {
    let dir = tempfile::tempdir().expect("tempdir");
    let candidate = RuleCandidate {
        id: "graded-abcd1234".into(),
        text: LESSON.into(),
        description: "d".into(),
        occurrences: 3,
        salient: false,
        evidence: Vec::new(),
        guard: None,
        score: 30,
    };

    let path = published_path(dir.path(), &candidate);

    let written = std::fs::read_to_string(&path).expect("the published record is readable");
    assert!(
        written.contains("environment_observation"),
        "the published rule dropped the grade it was published on:\n{written}"
    );
}

// ---- GATE: a directive costs measured evidence ----------------------------

/// **The witness.** A rule mined from prose grades `ModelCritique`. A rule
/// costs `EnvironmentObservation`. So the writer says no and puts nothing on
/// disk.
///
/// This is what the loop holds today. `observation_grade` maps a lesson to
/// `ModelCritique`, and a pool folds to its weakest part. So no count of
/// lessons reaches the bar. Before the gate, this call wrote the file.
#[test]
fn a_rule_on_reflection_evidence_is_refused_and_nothing_lands() {
    let dir = tempfile::tempdir().expect("tempdir");
    let candidate = RuleCandidate {
        id: "critiqued-abcd1234".into(),
        text: LESSON.into(),
        description: "d".into(),
        occurrences: 3,
        salient: false,
        evidence: Vec::new(),
        guard: None,
        score: 30,
    };

    let outcome = write_rule(
        dir.path(),
        &candidate,
        Some(ProvenanceGrade::ModelCritique),
        PublicationAuthority::LocalHuman,
    )
    .expect("a refusal is an answer, not a failure");

    assert_eq!(
        outcome,
        RulePublication::Refused(PromotionRefusal::EvidenceTooWeak {
            impact: stella_protocol::provenance::ImpactClass::SteeringDirective,
            required: ProvenanceGrade::EnvironmentObservation,
            actual: ProvenanceGrade::ModelCritique,
        })
    );
    assert!(
        crate::rules::load_workspace_rules_unfiltered(dir.path()).is_empty(),
        "a refused rule reached the loader anyway"
    );
}

/// An old record with no grade is a *missing* proof, not a weak one. And
/// nothing is written for it.
///
/// That is the posture for a record from before grades were kept. It does not
/// write on its own. No actor changes that: the gate reads the proof before
/// it reads who is asking.
#[test]
fn a_rule_with_no_grade_at_all_is_refused_as_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let candidate = RuleCandidate {
        id: "ungraded-abcd1234".into(),
        text: LESSON.into(),
        description: "d".into(),
        occurrences: 3,
        salient: false,
        evidence: Vec::new(),
        guard: None,
        score: 30,
    };

    let outcome = write_rule(
        dir.path(),
        &candidate,
        None,
        PublicationAuthority::LocalHuman,
    )
    .expect("a refusal is an answer, not a failure");

    assert_eq!(
        outcome,
        RulePublication::Refused(PromotionRefusal::NoEvidence {
            impact: stella_protocol::provenance::ImpactClass::SteeringDirective,
        })
    );
    assert!(
        crate::rules::load_workspace_rules_unfiltered(dir.path()).is_empty(),
        "an ungraded rule reached the loader anyway"
    );
}
