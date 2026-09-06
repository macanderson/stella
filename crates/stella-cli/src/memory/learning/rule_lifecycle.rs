//! The rule lifecycle twin. A mined rule that stops helping is retracted from
//! what the turns measured. A rule a person wrote is not.
//!
//! The trials here are real ledger rows. `appraisals::record_turn` writes
//! them, and `SessionMemory::auto_create_skills` reads them back. Both are
//! production doors. Nothing hands the sweep a verdict.
//!
//! Recall notes what a turn offered and what it showed. That half has its own
//! witness, in `memory::trials`.

use std::path::Path;

use stella_learn::ledger::ArtifactKind;
use stella_records::context_record::RecordStatus;

use crate::memory::{SessionMemory, appraisals, trials};

/// Turns per arm. The floor is five per arm. Eight is well clear of it, and
/// it is the count the memory twin uses.
const TURNS_PER_ARM: usize = 8;

/// The handle of the rule the loop mined. It is the last part of the lineage,
/// which is what the render pass cites and so what the trial rows are keyed
/// by.
const MINED: &str = "regenerate-gen";

/// The handle of the rule a person wrote. Same rows, different origin.
const HAND_WRITTEN: &str = "billing-retries";

/// Both records, in one published file.
///
/// One file rather than two, so the retraction has a sibling record to leave
/// alone: the writer rewrites the whole file, and a rebuild from the one
/// resolved record would drop the other.
const PUBLISHED: &str = r#"schema = "context-record/v0.1"
set_id = "acme"

[[record]]
lineage_id = "ctx.acme.regenerate-gen"
kind = "rule"
statement = "Regenerate gen/ rather than editing it by hand."
status = "active"
origin = "inferred"

[record.steering]
force = "may"

[[record]]
lineage_id = "ctx.acme.billing-retries"
kind = "rule"
statement = "Retry the billing webhook three times."
status = "active"
origin = "user"

[record.steering]
force = "may"
"#;

fn session(root: &Path) -> SessionMemory {
    SessionMemory::open_with_workspace_skills(root, false, true).expect("session memory")
}

fn log_path(root: &Path) -> std::path::PathBuf {
    root.join(".stella/private/reflections.jsonl")
}

/// Publish both records under `.stella/rules/`.
fn publish_rules(root: &Path) {
    let dir = root.join(".stella").join("rules");
    std::fs::create_dir_all(&dir).expect("the rules directory");
    std::fs::write(dir.join("acme.toml"), PUBLISHED).expect("publish the records");
}

/// The status the file on disk now carries for `lineage`.
fn status_of(root: &Path, lineage: &str) -> Option<RecordStatus> {
    let body = std::fs::read_to_string(root.join(".stella/rules/acme.toml")).expect("the file");
    let file: stella_records::ingest::record::ContextFile =
        toml::from_str(&body).expect("the file still parses");
    file.records
        .iter()
        .find(|record| record.lineage_id == lineage)
        .expect("the record is still in the file")
        .status
}

/// A window that says these rules hurt. Every turn that showed them failed.
/// Every turn that withheld them passed.
///
/// Both rules ride every turn. Their rows are the same, so only the origin
/// can tell them apart.
fn record_a_harming_window(root: &Path, ids: &[String]) {
    for _ in 0..TURNS_PER_ARM {
        appraisals::record_turn(
            root,
            ArtifactKind::Rule,
            ids,
            ids,
            &trials::live_trial(false),
        );
        appraisals::record_turn(
            root,
            ArtifactKind::Rule,
            ids,
            &[],
            &trials::live_trial(true),
        );
    }
}

/// **The witness.** The mined rule is retracted, and the ledger says who did
/// it and why. The hand-written one carries the same rows and is kept.
///
/// It fails on the code this landed against by construction (`#6143`): nothing
/// there passes `ArtifactKind::Rule` to a sweep, so no rule trial is ever read
/// and both records stay active.
#[test]
fn a_mined_rule_that_stops_helping_is_retracted_and_a_hand_written_one_is_kept() {
    let _env = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    // The registry reads the user tier too, so point it at an empty fixture:
    // a record in the developer's own `~/.stella/rules` must not join this
    // measurement.
    let _home = crate::test_env::home_sandbox(&root.join("home"));
    publish_rules(root);
    record_a_harming_window(root, &[MINED.to_string(), HAND_WRITTEN.to_string()]);

    let mut memory = session(root);
    memory.auto_create_skills(&log_path(root), true);

    assert_eq!(
        status_of(root, "ctx.acme.regenerate-gen"),
        Some(RecordStatus::Retracted),
        "its own turns say withholding it won"
    );
    assert_eq!(
        status_of(root, "ctx.acme.billing-retries"),
        Some(RecordStatus::Active),
        "a rule a person wrote is kept, whatever the numbers say"
    );

    // Retraction appends. The governance ledger names the lineage and says
    // what measured it.
    let ledger = std::fs::read_to_string(root.join(".stella/rules/promotions.jsonl"))
        .expect("the promotion ledger");
    assert!(
        ledger.contains("ctx.acme.regenerate-gen") && ledger.contains("lift"),
        "the ledger must carry the retraction and its reason: {ledger}"
    );
    assert!(
        !ledger.contains("ctx.acme.billing-retries"),
        "nothing was decided about the hand-written rule: {ledger}"
    );

    // A second sweep re-earns the same verdict from the same rows and writes
    // nothing: a retracted record is not active, so the sweep leaves it alone.
    let before = std::fs::read_to_string(root.join(".stella/rules/promotions.jsonl")).unwrap();
    memory.auto_create_skills(&log_path(root), true);
    let after = std::fs::read_to_string(root.join(".stella/rules/promotions.jsonl")).unwrap();
    assert_eq!(before, after, "a retraction happens once");
}

/// A window with no separation retracts nothing. Without it, the case above
/// would pass on a sweep that retracted every rule handed to it.
#[test]
fn a_rule_the_turns_cannot_separate_is_left_alone() {
    let _env = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let _home = crate::test_env::home_sandbox(&root.join("home"));
    publish_rules(root);

    // Every turn passes, shown or withheld. Neither side wins. `Inert` needs
    // a full window in both arms, and this is far short of one.
    let ids = [MINED.to_string()];
    for _ in 0..TURNS_PER_ARM {
        appraisals::record_turn(
            root,
            ArtifactKind::Rule,
            &ids,
            &ids,
            &trials::live_trial(true),
        );
        appraisals::record_turn(
            root,
            ArtifactKind::Rule,
            &ids,
            &[],
            &trials::live_trial(true),
        );
    }

    let mut memory = session(root);
    memory.auto_create_skills(&log_path(root), true);

    assert_eq!(
        status_of(root, "ctx.acme.regenerate-gen"),
        Some(RecordStatus::Active),
        "no measurement, no retraction"
    );
    assert!(
        !root.join(".stella/rules/promotions.jsonl").exists(),
        "nothing was decided, so nothing is written"
    );
}

/// The grade half. A rule whose own turns say it helps earns
/// `EnvironmentObservation`, which is what a steering directive costs. One
/// with rows that say nothing earns no grade at all, so the mining grade is
/// what the gate weighs and the gate refuses it.
#[test]
fn a_measured_rule_earns_the_grade_a_directive_costs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    // Shown and it passed; withheld and it failed. That is the rule helping.
    let ids = [MINED.to_string()];
    for _ in 0..TURNS_PER_ARM {
        appraisals::record_turn(
            root,
            ArtifactKind::Rule,
            &ids,
            &ids,
            &trials::live_trial(true),
        );
        appraisals::record_turn(
            root,
            ArtifactKind::Rule,
            &ids,
            &[],
            &trials::live_trial(false),
        );
    }

    assert_eq!(
        crate::memory::rule_efficacy::measured_grade(root, MINED),
        Some(stella_protocol::provenance::ProvenanceGrade::EnvironmentObservation),
        "the environment answered, which is the grade a directive costs"
    );
    assert_eq!(
        crate::memory::rule_efficacy::measured_grade(root, HAND_WRITTEN),
        None,
        "a rule with no rows has no measured window"
    );
}
