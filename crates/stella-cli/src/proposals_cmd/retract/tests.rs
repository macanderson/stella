// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for `stella proposals retract`.
//!
//! The decisive one is `a_retracted_rule_stops_reaching_the_system_prefix`: a
//! rollback artifact that leaves the rule steering is not a rollback, and that
//! is the half a status flip alone could get wrong.

use super::*;

use stella_learn::rules::RuleCandidate;
use stella_records::records::Channel;

const LESSON: &str = "Always run the database migration before starting the integration suite.";

/// A workspace with one published mined rule, and the candidate behind it.
fn published_workspace() -> (tempfile::TempDir, RuleCandidate) {
    let dir = tempfile::tempdir().expect("tempdir");
    let candidate = RuleCandidate {
        id: "retraction-abcd1234".into(),
        text: LESSON.into(),
        description: "d".into(),
        occurrences: 3,
        salient: false,
        evidence: Vec::new(),
        guard: None,
        score: 30,
    };
    crate::memory::rules_mining::write_rule(dir.path(), &candidate, None)
        .expect("publishable")
        .expect("written");
    (dir, candidate)
}

/// The text this workspace's records put in front of the model.
///
/// The real prompt path: `assemble_system_prompt` renders exactly this from
/// `load_workspace_rules`'s registry, so a rule that is absent here is absent
/// from the system prefix.
fn cached_prefix(root: &Path) -> String {
    let authority = crate::settings::AuthorityPolicy {
        project_prompts_allowed: true,
        ..Default::default()
    };
    crate::rules::load_workspace_rules(root, &authority)
        .registry()
        .render(Channel::Cached, None)
        .text
}

/// **Witness (#4866).** A published rule can be taken back, and taking it back
/// stops it steering.
///
/// Before this command the only reversal was `rm .stella/rules/<id>.toml` or a
/// git revert: `stella proposals` offered list/keep/edit/ignore/refresh, none
/// of which touches a rule already on disk, and `stella memory retire` writes
/// standings the file-backed loader never consults.
#[test]
fn a_retracted_rule_stops_reaching_the_system_prefix() {
    let (dir, candidate) = published_workspace();

    assert!(
        cached_prefix(dir.path()).contains("database migration"),
        "the published rule must reach the prefix before it is retracted: {}",
        cached_prefix(dir.path())
    );

    retract_rule(dir.path(), &candidate.id, "it was wrong about this repo")
        .expect("the published rule is retractable");

    assert!(
        !cached_prefix(dir.path()).contains("database migration"),
        "a retracted rule must not steer the next turn: {}",
        cached_prefix(dir.path())
    );
}

/// Retraction is append-only: the record stays, marked retracted, rather than
/// being deleted. Deleting would lose the record that Stella ever believed it,
/// which is the thing the ledger exists to keep.
#[test]
fn the_retracted_record_stays_on_disk_marked_retracted() {
    let (dir, candidate) = published_workspace();
    let published = crate::memory::rules_mining::workspace_rules_dir(dir.path());
    let before = resolve_published(dir.path(), &candidate.id)
        .expect("published")
        .record()
        .record_id
        .clone();

    retract_rule(dir.path(), &candidate.id, "superseded by a house rule").expect("retractable");

    let file = resolve_published(dir.path(), &candidate.id).expect("still findable");
    assert_eq!(file.record().status, Some(RecordStatus::Retracted));
    assert_eq!(
        file.record().supersedes_record_id,
        before,
        "the retracted revision supersedes the active revision it replaces"
    );
    assert_ne!(
        file.record().record_id,
        before,
        "re-stamping mints a new revision id, so the file still verifies on load"
    );
    assert!(
        std::fs::read_dir(&published)
            .expect("rules dir")
            .flatten()
            .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("toml")),
        "the record file must survive its own retraction"
    );
}

/// The hash-chained ledger records who retracted what and why, and still
/// verifies afterwards.
#[test]
fn the_retraction_is_appended_to_the_hash_chained_ledger() {
    let (dir, candidate) = published_workspace();

    retract_rule(dir.path(), &candidate.id, "the migration order changed").expect("retractable");

    let events = crate::context_records::read_promotions(dir.path())
        .expect("the ledger still verifies after the append");
    let retirement = events
        .iter()
        .find(|e| e.action == stella_records::records::promotion::LedgerAction::Retired)
        .expect("a retirement event");
    assert!(retirement.lineage_id.ends_with(&candidate.id));
    assert_eq!(retirement.from, "active");
    assert_eq!(retirement.to, "retracted");
    assert!(
        retirement.reason.contains("the migration order changed"),
        "the reason a person gave must survive onto the event: {}",
        retirement.reason
    );
    assert!(
        !retirement.approver.trim().is_empty(),
        "a retraction must name who made it"
    );
}

/// Retracting twice is refused rather than appending a second event that says
/// nothing new — and the message says what the standing already is.
#[test]
fn a_second_retraction_is_refused_and_says_why() {
    let (dir, candidate) = published_workspace();
    retract_rule(dir.path(), &candidate.id, "once").expect("retractable");

    let again = retract_rule(dir.path(), &candidate.id, "twice").expect_err("already retracted");
    assert!(
        again.contains("already retracted"),
        "the refusal must name the standing: {again}"
    );
}

/// A governance decision with no reason is not auditable, so an empty one is
/// refused before anything is written — the same rule `stella memory retire`
/// applies.
#[test]
fn a_retraction_with_no_reason_is_refused_before_anything_is_written() {
    let (dir, candidate) = published_workspace();

    let refused = retract_rule(dir.path(), &candidate.id, "   ").expect_err("no reason");
    assert!(refused.contains("reason"), "{refused}");
    assert!(
        cached_prefix(dir.path()).contains("database migration"),
        "a refused retraction must leave the rule exactly as it was"
    );
}

/// A name nothing published is an error naming where to look, not a silent
/// success that leaves the user believing a live rule was taken back.
#[test]
fn retracting_an_unpublished_name_is_refused() {
    let (dir, _candidate) = published_workspace();

    let refused = retract_rule(dir.path(), "no-such-rule-0000ffff", "because")
        .expect_err("nothing named that");
    assert!(refused.contains("no-such-rule-0000ffff"), "{refused}");
}
