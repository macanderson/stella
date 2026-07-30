//! Tests for `stella context validate`: datetime-tolerant loading, the
//! verdict→tally logic, and record enumeration. The probe runner itself is
//! tested in `ingest_cmd::probe`.

use std::path::{Path, PathBuf};

use stella_core::context_record::Origin;
use stella_core::ingest::{Probe, ProbeKind, Record, RecordKind, Truth, TruthBasis};

use super::*;

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("stella-ctxval-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// A record carrying `probe` in its truth, origin imported (ingest output).
fn record_with_probe(statement: &str, probe: Option<Probe>) -> Record {
    Record {
        lineage_id: "ctx.acme.web.x".to_string(),
        record_id: None,
        record_hash: None,
        kind: RecordKind::Fact,
        statement: statement.to_string(),
        tags: Vec::new(),
        origin: Some(Origin::Imported),
        sharing_scope: None,
        status: None,
        provenance: None,
        steering: None,
        enforcement: None,
        truth: Some(Truth {
            basis: TruthBasis::Measured,
            confidence: None,
            verified_by: None,
            verified_at: None,
            valid_from: None,
            ttl: None,
            on_expiry: None,
            review_every: None,
            probe,
        }),
        links: Vec::new(),
    }
}

fn path_probe(kind: ProbeKind, path: &str) -> Probe {
    Probe {
        kind,
        path: Some(path.to_string()),
        pattern: None,
        expect: None,
        note: None,
    }
}

#[test]
fn check_record_tallies_supported_refuted_and_unchecked() {
    let root = temp_root("tally");
    std::fs::write(root.join("pnpm-lock.yaml"), "lock").expect("write");
    let mut tally = Tally::default();

    // Supported: the path exists.
    check_record(
        &root,
        &record_with_probe(
            "pnpm is used",
            Some(path_probe(ProbeKind::PathExists, "pnpm-lock.yaml")),
        ),
        "t",
        &mut tally,
    );
    // Refuted: the path is missing.
    check_record(
        &root,
        &record_with_probe(
            "yarn is used",
            Some(path_probe(ProbeKind::PathExists, "yarn.lock")),
        ),
        "t",
        &mut tally,
    );
    // Unchecked: no probe at all.
    check_record(
        &root,
        &record_with_probe("a preference", None),
        "t",
        &mut tally,
    );

    assert_eq!(tally.supported, 1);
    assert_eq!(tally.refuted, 1);
    assert_eq!(tally.unchecked, 1);
    assert_eq!(tally.total(), 3);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_gated_probe_is_never_run_and_counts_as_unchecked() {
    let root = temp_root("gated");
    let mut tally = Tally::default();
    // An http_ok probe on an imported record is never honored → unchecked, not
    // a network call.
    check_record(
        &root,
        &record_with_probe(
            "the API is up",
            Some(Probe {
                kind: ProbeKind::HttpOk,
                path: Some("https://evil.example".to_string()),
                pattern: None,
                expect: None,
                note: None,
            }),
        ),
        "t",
        &mut tally,
    );
    assert_eq!(tally.unchecked, 1);
    assert_eq!(tally.refuted, 0);
    assert_eq!(tally.supported, 0);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn load_tolerates_bare_toml_datetimes() {
    let root = temp_root("datetime");
    let path = root.join("proposals.toml");
    // Bare (unquoted) datetimes, as the hand-authored examples use.
    std::fs::write(
        &path,
        r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[[record]]
lineage_id = "ctx.acme.web.staging"
kind       = "fact"
statement  = "Staging is at api.staging.acme.dev."

  [record.provenance]
  extracted_at = 2026-07-27T09:00:00Z

  [record.truth]
  basis        = "measured"
  verified_at  = 2026-07-25T11:30:00Z
"#,
    )
    .expect("write");

    let context = load_context_file(&path).expect("bare datetimes should load");
    assert_eq!(context.records.len(), 1);
    // The datetime survived as its string form.
    let provenance = context.records[0].provenance.as_ref().expect("provenance");
    assert_eq!(
        provenance.extracted_at.as_deref(),
        Some("2026-07-27T09:00:00Z")
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn records_of_includes_both_published_records_and_proposal_records() {
    // A file with one published record and one proposal.
    let toml = r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[[record]]
lineage_id = "ctx.acme.web.a"
kind       = "fact"
statement  = "A."

[[proposal]]
candidate_id  = "b-0000"
proposal_kind = "directive"
status        = "eligible"
confidence    = 90
observed_at   = "2026-07-27T09:00:00Z"

  [proposal.record]
  lineage_id = "ctx.acme.web.b"
  kind       = "rule"
  statement  = "B."
"#;
    let context: ContextFile = toml::from_str(toml).expect("parse");
    let records = records_of(&context);
    assert_eq!(records.len(), 2);
    let statements: Vec<&str> = records.iter().map(|r| r.statement.as_str()).collect();
    assert!(statements.contains(&"A."));
    assert!(statements.contains(&"B."));
}

#[test]
fn validate_missing_file_reports_and_does_not_panic() {
    // An explicit path that does not exist is reported, not fatal.
    let missing = Path::new("/nonexistent/definitely/not/here.toml").to_path_buf();
    // strict=false, so a load error is surfaced and the run still returns Ok.
    let result = validate(&[missing], false);
    assert!(result.is_ok());
}
