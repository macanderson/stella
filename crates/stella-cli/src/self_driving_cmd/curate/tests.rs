// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What the curate pass reads, what it writes, and what it must never touch.

use std::path::Path;

use super::*;

/// A state directory on a tempdir. Both fields are `pub`, so a test can build
/// one without `LoopState::open`'s home resolution and migration.
fn loop_state(dir: &tempfile::TempDir) -> Durable {
    Durable {
        dir: dir.path().to_path_buf(),
        repo_root: dir.path().to_path_buf(),
    }
}

/// Write one journal line, as `audit::record` would have.
fn journal(durable: &Durable, run: &str, at: &str, action: Audit, outcome: &str) {
    let line = serde_json::json!({
        "at": at,
        "run_id": "cycle-1",
        "action": action,
        "subject": serde_json::Value::Null,
        "outcome": outcome,
        "session_id": run,
    });
    let mut text = std::fs::read_to_string(durable.audit_path()).unwrap_or_default();
    text.push_str(&serde_json::to_string(&line).unwrap());
    text.push('\n');
    std::fs::write(durable.audit_path(), text).unwrap();
}

/// A workspace whose governance is the one this repository runs under.
fn regulated(root: &Path) {
    let rules = root.join(".stella").join("rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(rules.join("governance.toml"), "mode = \"regulated\"\n").unwrap();
}

/// What the published-record directory holds, in a stable order — `read_dir`
/// promises none, so an unsorted comparison would flake rather than fail.
fn published(root: &Path) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(root.join(".stella/rules"))
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    paths.sort();
    paths
}

/// **The witness.** A wall met in three separate runs becomes one written
/// proposal that names every run behind it — and under `regulated` governance
/// the pass publishes nothing: no skill, no record under `.stella/rules/`, and
/// no line of the hash-chained promotion ledger.
#[test]
fn a_recurring_wall_becomes_a_proposal_and_nothing_is_applied() {
    let dir = tempfile::tempdir().unwrap();
    let durable = loop_state(&dir);
    regulated(dir.path());
    let before = published(dir.path());

    journal(
        &durable,
        "sd-1",
        "2026-09-01T00:00:00Z",
        Audit::Transient,
        "could not file `a`: 502",
    );
    journal(
        &durable,
        "sd-2",
        "2026-09-02T00:00:00Z",
        Audit::Transient,
        "could not file `b`: 500",
    );
    journal(
        &durable,
        "sd-3",
        "2026-09-03T00:00:00Z",
        Audit::Transient,
        "could not file `c`: timed out",
    );

    assert_eq!(pending(&durable), 1);
    assert!(pass(&durable, dir.path()));

    let rows = durable.proposals();
    assert_eq!(rows.len(), 1, "got {rows:?}");
    assert_eq!(rows[0].surface, "tool");
    assert_eq!(rows[0].runs, vec!["sd-1", "sd-2", "sd-3"]);
    assert_eq!(
        rows[0].evidence,
        vec![
            "2026-09-01T00:00:00Z",
            "2026-09-02T00:00:00Z",
            "2026-09-03T00:00:00Z"
        ]
    );
    assert_eq!(rows[0].governance, "regulated");

    // Nothing was applied. The published-record directory is as it was, and
    // neither the promotion ledger nor a skill exists.
    assert_eq!(published(dir.path()), before);
    assert!(!dir.path().join(".stella/rules/promotions.jsonl").exists());
    assert!(!dir.path().join(".stella/skills").exists());
}

/// A second pass over the same journal proposes nothing: the digest of a wall
/// already written down is what keeps a standing proposal from being made
/// again on every run.
#[test]
fn a_wall_already_written_down_is_not_proposed_again() {
    let dir = tempfile::tempdir().unwrap();
    let durable = loop_state(&dir);
    regulated(dir.path());

    for (n, run) in ["sd-1", "sd-2", "sd-3"].iter().enumerate() {
        journal(
            &durable,
            run,
            &format!("2026-09-0{}T00:00:00Z", n + 1),
            Audit::WorkFailed,
            &format!("the turn did not finish: exit {n}"),
        );
    }

    assert!(pass(&durable, dir.path()));
    assert_eq!(pending(&durable), 0);
    assert!(!pass(&durable, dir.path()));
    assert_eq!(durable.proposals().len(), 1);
}

/// A loop that has never repeated itself proposes nothing, so a build that
/// gains this code changes nothing for a workspace with an ordinary journal.
#[test]
fn a_journal_with_no_repetition_proposes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let durable = loop_state(&dir);
    regulated(dir.path());

    journal(
        &durable,
        "sd-1",
        "2026-09-01T00:00:00Z",
        Audit::Transient,
        "could not file `a`: 502",
    );
    journal(
        &durable,
        "sd-2",
        "2026-09-02T00:00:00Z",
        Audit::Waived,
        "the baseline is already red (cargo) — advisory",
    );

    assert_eq!(pending(&durable), 0);
    assert!(!pass(&durable, dir.path()));
    assert!(durable.proposals().is_empty());
}

/// A journal nothing has written yet is read as no walls rather than as an
/// error, so the first poll of a fresh loop costs nothing.
#[test]
fn a_missing_journal_is_read_as_no_walls() {
    let dir = tempfile::tempdir().unwrap();
    let durable = loop_state(&dir);

    assert!(sightings(&durable.audit_path()).is_empty());
    assert_eq!(pending(&durable), 0);
}

/// A rule proposal rests on the loop's own judgement, and judgement is a
/// `ModelCritique` however many runs agree. A steering directive costs an
/// `EnvironmentObservation`, so the loop can never publish one of these on its
/// own evidence — which is the line "propose any authority, grant itself
/// none" is drawn at, checked against the policy rather than asserted.
#[test]
fn a_rule_on_the_loops_own_judgement_could_never_be_published_by_the_loop() {
    let refused = authorises(
        Some(grade_of(Target::Rule)),
        PublicationAuthority::Agent,
        impact_of(Target::Rule),
    );

    assert!(matches!(
        refused,
        Err(PromotionRefusal::EvidenceTooWeak {
            required: ProvenanceGrade::EnvironmentObservation,
            actual: ProvenanceGrade::ModelCritique,
            ..
        })
    ));
}

/// A custom tool costs a deterministic proof and a person. The loop has
/// neither, and the row it writes says so instead of implying it could.
#[test]
fn a_tool_proposal_records_why_the_loop_could_not_publish_it() {
    let proposal = Proposal {
        target: Target::Tool,
        statement: "could not file `a`: 502".to_owned(),
        shape: "could not file".to_owned(),
        evidence: vec!["2026-09-01T00:00:00Z".to_owned()],
        runs: vec!["sd-1".to_owned(), "sd-2".to_owned(), "sd-3".to_owned()],
    };

    let row = row_for(&proposal, "regulated");

    assert_eq!(row.required_grade, "deterministic_proof");
    assert_eq!(row.required_authority, "local_human");
    assert!(matches!(
        row.refusal,
        Some(PromotionRefusal::EvidenceTooWeak { .. })
    ));
}

/// Every wall the journal can record points at a surface, and every surface
/// answers to an impact class — so no action reaches the ledger without the
/// policy having something to say about it.
#[test]
fn every_wall_names_a_surface_with_an_impact_class() {
    for (_, target) in WALLS {
        let impact = impact_of(*target);

        assert!(ImpactClass::ALL.contains(&impact), "{target:?}");
        assert!(
            ProvenanceGrade::ALL.contains(&grade_of(*target)),
            "{target:?}"
        );
    }
}
