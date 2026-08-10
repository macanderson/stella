//! End-to-end tests for the refresh reconciliation: real published record
//! files in a temp tree, the real diff, and the real durable rewrite.

use std::path::Path;

use stella_core::context_record::{Origin, RecordStatus};
use stella_core::ingest::lineage;
use stella_core::ingest::record::{
    ContextFile, Defaults, Provenance, Record, RecordKind, SharingScope,
};
use stella_core::ingest::refresh::AssertedClaim;

use super::*;

fn publish(root: &Path, suffix: &str, statement: &str, source: &str) -> String {
    let mut record = Record {
        lineage_id: format!("ctx.acme.web.{suffix}"),
        record_id: None,
        record_hash: None,
        kind: RecordKind::Rule,
        statement: statement.to_string(),
        tags: Vec::new(),
        origin: Some(Origin::Imported),
        sharing_scope: Some(SharingScope::Repository),
        status: Some(RecordStatus::Active),
        supersedes_record_id: None,
        provenance: Some(Provenance {
            source_uri: Some(source.to_string()),
            ..Default::default()
        }),
        steering: None,
        enforcement: None,
        truth: None,
        links: Vec::new(),
    };
    record.stamp(&Defaults::default()).expect("stamps");
    let path = root
        .join(crate::context_records::RULES_DIR)
        .join(format!("{}.toml", record.lineage_id));
    crate::context_records::write_record(&path, "acme.web", &record).expect("publishes");
    record.record_id.expect("stamped id")
}

fn read_published(root: &Path, suffix: &str) -> Record {
    let path = root
        .join(crate::context_records::RULES_DIR)
        .join(format!("ctx.acme.web.{suffix}.toml"));
    let contents = std::fs::read_to_string(&path).expect("readable");
    let file: ContextFile = toml::from_str(&contents).expect("parses");
    file.records.first().cloned().expect("one record")
}

fn asserted(suffix: &str, statement: &str) -> AssertedClaim {
    AssertedClaim {
        lineage_id: format!("ctx.acme.web.{suffix}"),
        statement: statement.to_string(),
    }
}

/// The witness for the churn guard: a one-line edit to the source retires
/// exactly one record. The unchanged record's file is byte-identical after
/// the refresh — age, appraisal history and identity all survive.
#[test]
fn a_dropped_claim_is_retired_and_an_unchanged_one_is_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    publish(root, "pkg", "use pnpm exclusively", "notes.md");
    let dropped_id = publish(root, "deploy", "deploys run from main", "notes.md");

    let before = std::fs::read_to_string(
        root.join(crate::context_records::RULES_DIR)
            .join("ctx.acme.web.pkg.toml"),
    )
    .expect("readable");

    apply(root, "notes.md", &[asserted("pkg", "use pnpm exclusively")]).expect("applies");

    let after = std::fs::read_to_string(
        root.join(crate::context_records::RULES_DIR)
            .join("ctx.acme.web.pkg.toml"),
    )
    .expect("readable");
    assert_eq!(before, after, "the unchanged record must not be rewritten");

    let retired = read_published(root, "deploy");
    assert_eq!(retired.status, Some(RecordStatus::Archived));
    assert_eq!(retired.supersedes_record_id.as_deref(), Some(&*dropped_id));
    assert_ne!(
        retired.record_id.as_deref(),
        Some(&*dropped_id),
        "the archived revision is a new revision, not an edit of the old one"
    );

    // The accountable half (spec §4, #2728): the retirement appended one
    // verifiable ledger event naming the record and the command.
    let events = crate::context_records::read_promotions(root).expect("the chain verifies");
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].action,
        stella_core::records::promotion::LedgerAction::Retired
    );
    assert_eq!(events[0].lineage_id, "ctx.acme.web.deploy");
    assert!(
        events[0].reason.contains(&dropped_id),
        "{}",
        events[0].reason
    );
    assert!(
        events[0].reason.contains("--refresh notes.md"),
        "{}",
        events[0].reason
    );
}

/// A changed claim retires nothing: the replacement is a proposal until a
/// reviewer keeps it, and retiring first would open a steering gap.
#[test]
fn a_changed_claim_leaves_the_published_record_live() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    publish(root, "pkg", "use npm", "notes.md");

    apply(root, "notes.md", &[asserted("pkg", "use pnpm, never npm")]).expect("applies");

    let record = read_published(root, "pkg");
    assert_eq!(record.status, Some(RecordStatus::Active));
    assert_eq!(record.supersedes_record_id, None);
}

/// Scope: a record published from a different source file is not this
/// refresh's to touch, whatever the new text says.
#[test]
fn records_from_other_sources_are_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    publish(root, "other", "an unrelated claim", "other.md");

    apply(root, "notes.md", &[]).expect("applies");

    let record = read_published(root, "other");
    assert_eq!(record.status, Some(RecordStatus::Active));
}

/// An already-archived record is not retired again: retirement is a one-way
/// act, and re-archiving would mint a fresh revision on every refresh.
#[test]
fn an_archived_record_is_not_retired_twice() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    publish(root, "deploy", "deploys run from main", "notes.md");
    apply(root, "notes.md", &[]).expect("first refresh retires");
    let first = read_published(root, "deploy");

    apply(root, "notes.md", &[]).expect("second refresh is a no-op");
    let second = read_published(root, "deploy");
    assert_eq!(
        first, second,
        "a second refresh must not re-revise the archived record"
    );
}

/// A workspace with no published records refreshes to a clean no-op.
#[test]
fn a_workspace_with_nothing_published_is_a_no_op() {
    let dir = tempfile::tempdir().expect("tempdir");
    apply(dir.path(), "notes.md", &[asserted("pkg", "anything")]).expect("applies");
}

/// The retirement survives the lineage machinery beside it: refreshing does
/// not disturb an alert dismissal recorded for the same source.
#[test]
fn refresh_does_not_touch_the_alert_ledger() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    crate::ingest_cmd::lineage::record_ingest(
        root,
        "notes.md",
        lineage::Lineage {
            source_hash: "git:aaaa".to_string(),
            commit: None,
            ingested_at: "2026-08-10T00:00:00Z".to_string(),
            ingest_run_id: "ing_test".to_string(),
            candidate_ids: Vec::new(),
            alerts: lineage::AlertState::Dismissed,
        },
        false,
    );
    publish(root, "deploy", "deploys run from main", "notes.md");

    apply(root, "notes.md", &[]).expect("applies");

    let ledger = crate::ingest_cmd::lineage::read_ledger(root);
    assert_eq!(
        ledger.sources["notes.md"].alerts,
        lineage::AlertState::Dismissed,
        "reconciling records must not re-arm a dismissed alert"
    );
}
