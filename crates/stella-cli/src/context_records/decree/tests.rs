// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a bang mark leaves behind. One record the next session loads. One
//! ledger line that says who left it.

use super::*;

use stella_records::records::Trust;

use crate::context_records::{PROMOTION_LEDGER, load_registry, read_decisions};

fn workspace() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temp workspace")
}

/// The saved record for `lineage`, as a fresh load sees it. That is the only
/// view a later session has.
fn loaded(root: &Path, lineage: &str) -> stella_records::ingest::record::Record {
    let registry = load_registry(root);
    registry
        .entries
        .iter()
        .find(|entry| entry.record.record.lineage_id == lineage)
        .unwrap_or_else(|| panic!("{lineage} is not in the registry"))
        .record
        .record
        .clone()
}

/// **The witness for `!!`.** Words kept mid-turn come back at `should` in the
/// next session. The `origin` says a person put them there.
#[test]
fn guidance_is_published_at_should_and_a_fresh_load_finds_it() {
    let root = workspace();
    let saved = publish(root.path(), KeepStrength::Guidance, "Use short sentences.")
        .expect("the record publishes");
    assert!(!saved.unchanged);
    assert!(saved.path.exists(), "the mark writes a Git-tracked file");

    let record = loaded(root.path(), &saved.lineage_id);
    assert_eq!(record.statement, "Use short sentences.");
    assert_eq!(force_of(&record), Some(rec::Force::Should));
    assert_eq!(
        record.origin,
        Some(stella_records::context_record::Origin::User)
    );
    assert_eq!(
        record.truth.as_ref().map(|t| t.basis),
        Some(rec::TruthBasis::Decree),
        "a person said so, which is what a decree means"
    );
}

/// **The witness for `!!!`.** The rule strength is `must`. It is saved at the
/// top `precedence`, so nothing beats it and no budget drops it first.
#[test]
fn a_rule_is_published_at_must_and_at_the_precedence_ceiling() {
    let root = workspace();
    let saved = publish(root.path(), KeepStrength::Rule, "Never force-push to main.")
        .expect("the record publishes");

    let record = loaded(root.path(), &saved.lineage_id);
    assert_eq!(force_of(&record), Some(rec::Force::Must));
    assert_eq!(record.precedence(), RULE_PRECEDENCE);
    assert!(
        record.enforcement.is_none(),
        "a sentence carries no guard to evaluate, so it claims no blocking"
    );
}

/// A saved record has to check out on load. If it does not, the next session
/// reads a finding, not a rule.
#[test]
fn what_the_mark_writes_verifies_when_it_is_read_back() {
    let root = workspace();
    let saved =
        publish(root.path(), KeepStrength::Rule, "Never force-push to main.").expect("publishes");
    let registry = load_registry(root.path());
    let entry = registry
        .entries
        .iter()
        .find(|entry| entry.record.record.lineage_id == saved.lineage_id)
        .expect("the record loads");
    assert!(
        entry.record.findings.is_empty(),
        "a freshly kept record must verify: {:?}",
        entry.record.findings
    );
}

/// **The witness for the ledger.** Raise one claim from guidance to a rule.
/// The old draft is retired. The shared ledger says so. This is the path
/// `stella context keep` walks. It is not a side door.
#[test]
fn raising_guidance_to_a_rule_supersedes_it_and_the_ledger_says_so() {
    let root = workspace();
    let first = publish(
        root.path(),
        KeepStrength::Guidance,
        "Never force-push to main.",
    )
    .expect("the first save publishes");
    let second = publish(root.path(), KeepStrength::Rule, "Never force-push to main.")
        .expect("the second save supersedes");

    assert_eq!(
        first.lineage_id, second.lineage_id,
        "the same sentence keeps the same lineage, so the two do not argue"
    );
    assert!(
        second.superseded.is_some(),
        "the raise replaces the revision it outranks"
    );
    let ledger = std::fs::read_to_string(root.path().join(PROMOTION_LEDGER))
        .expect("the promotion ledger exists");
    let tail = ledger.lines().last().expect("it has a line");
    assert!(
        tail.contains("\"superseded\"") && tail.contains(&second.lineage_id),
        "the ledger tail names the supersession: {tail}"
    );
    assert_eq!(
        force_of(&loaded(root.path(), &second.lineage_id)),
        Some(rec::Force::Must),
        "the rule strength is what a later session now loads"
    );
}

/// Say a thing twice. It is not an error, and it is not a second record.
#[test]
fn the_same_claim_at_the_same_strength_writes_nothing_new() {
    let root = workspace();
    publish(root.path(), KeepStrength::Guidance, "Use short sentences.").expect("publishes");
    let again = publish(root.path(), KeepStrength::Guidance, "Use short sentences.")
        .expect("a repeat is not a failure");
    assert!(again.unchanged);
    assert_eq!(
        read_decisions(root.path()).len(),
        1,
        "a write that did not happen is not a decision"
    );
}

/// Every save is on the ledger, with the file it wrote. So one log says where
/// a record came from.
#[test]
fn every_save_is_on_the_decision_ledger_with_the_file_it_wrote() {
    let root = workspace();
    let saved =
        publish(root.path(), KeepStrength::Rule, "Never force-push to main.").expect("publishes");
    let decisions = read_decisions(root.path());
    let event = decisions.first().expect("one decision");
    assert_eq!(event.lineage_id, saved.lineage_id);
    assert!(
        event
            .published_to
            .as_deref()
            .is_some_and(|to| to.starts_with(".stella/rules/")),
        "the path is recorded repo-relative: {:?}",
        event.published_to
    );
    assert!(
        !event.approved_blocking,
        "keeping a rule is not authorising it to block a tool call"
    );
}

/// **The witness for the sweep gate.** The gate asks a record for two things.
/// One: an `origin` that is not `imported` or `inferred`. Two: a `decree` with
/// a name on it. What the mark builds has both. So the gate honors a gated
/// probe on it. That is the read the live sweep makes.
#[test]
fn the_minted_record_clears_the_sweep_gate() {
    let mut record = record("acme.web", KeepStrength::Rule, "Never force-push.", "mac")
        .expect("the record builds");
    let origin = record.origin.expect("an origin is stamped");
    assert!(
        !stella_records::ingest::gate::origin_is_untrusted(origin),
        "a decree must not be read as imported or inferred"
    );
    if let Some(truth) = record.truth.as_mut() {
        truth.probe = Some(rec::Probe {
            kind: rec::ProbeKind::CommandSucceeds,
            path: None,
            pattern: None,
            expect: None,
            note: None,
        });
    }
    assert!(
        stella_records::records::sweep::honored_probe(&record, Trust::User).is_some(),
        "origin plus a signed decree is what the gate asks of the record"
    );
}

/// Pasted output is not a claim. Refuse it here. That is cheaper than a file
/// the workspace's own check then flags.
#[test]
fn a_statement_that_reads_as_pasted_is_refused_before_anything_is_written() {
    let root = workspace();
    let pasted = "error: one\nerror: two\nerror: three\n".repeat(20);
    let err = publish(root.path(), KeepStrength::Rule, &pasted).unwrap_err();
    assert!(err.contains("one sentence"), "{err}");
    assert!(
        read_decisions(root.path()).is_empty(),
        "a refused save records nothing"
    );
}

/// The mark with nothing after it is refused. An empty claim is not a rule.
#[test]
fn an_empty_statement_is_refused() {
    let root = workspace();
    let err = publish(root.path(), KeepStrength::Guidance, "   ").unwrap_err();
    assert!(err.contains("nothing after the mark"), "{err}");
}

/// The lineage comes from the words, not the strength. That is what lets a
/// raise retire the old draft in place of a rival record.
#[test]
fn the_lineage_follows_the_sentence_and_not_the_strength() {
    let one = record(
        "acme.web",
        KeepStrength::Guidance,
        "Use short sentences.",
        "mac",
    )
    .unwrap();
    let two = record(
        "acme.web",
        KeepStrength::Rule,
        "Use short sentences.",
        "mac",
    )
    .unwrap();
    assert_eq!(one.lineage_id, two.lineage_id);
    assert!(
        one.lineage_id.contains("use-short-sentences"),
        "the file is findable by eye: {}",
        one.lineage_id
    );
    let other = record(
        "acme.web",
        KeepStrength::Guidance,
        "Use long sentences.",
        "mac",
    )
    .unwrap();
    assert_ne!(one.lineage_id, other.lineage_id);
}
