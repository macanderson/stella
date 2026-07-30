//! Loader tests, plus the record builders the rest of the module's tests share.
//!
//! The builders are `pub(super)` on purpose: every submodule needs a plausible
//! record to test against, and a per-module copy of "how to build one" is how a test
//! suite ends up asserting against six subtly different notions of a valid record.

use super::super::context_record::kind::{Origin, RecordStatus};
use super::super::ingest::record::{
    AppliesTo, Enforcement, EnforcementMode, Force, Record, RecordKind, SharingScope, Steering,
    Truth,
};
use super::*;

/// A minimal valid record under `lineage`, unstamped.
pub(super) fn record_named(lineage: &str) -> Record {
    Record {
        lineage_id: lineage.to_string(),
        record_id: None,
        record_hash: None,
        kind: RecordKind::Rule,
        statement: "This repository uses pnpm exclusively.".to_string(),
        tags: Vec::new(),
        origin: Some(Origin::Imported),
        sharing_scope: Some(SharingScope::Repository),
        status: Some(RecordStatus::Active),
        provenance: None,
        steering: Some(Steering {
            force: Force::Must,
            precedence: Some(50),
            applies_to: None,
        }),
        enforcement: None,
        truth: None,
        links: Vec::new(),
    }
}

/// `record` with `truth` attached.
pub(super) fn with_truth(mut record: Record, truth: Truth) -> Record {
    record.truth = Some(truth);
    record
}

/// `record` at a different steering force.
pub(super) fn with_force(mut record: Record, force: Force) -> Record {
    record.steering = Some(Steering {
        force,
        precedence: Some(50),
        applies_to: None,
    });
    record
}

/// `record` scoped to `paths` at `precedence`.
pub(super) fn with_scope(mut record: Record, paths: &[&str], precedence: u32) -> Record {
    record.steering = Some(Steering {
        force: Force::Must,
        precedence: Some(precedence),
        applies_to: Some(AppliesTo {
            paths: paths.iter().map(|path| path.to_string()).collect(),
            tasks: Vec::new(),
            keywords: Vec::new(),
        }),
    });
    record
}

/// A `hard`-mode enforcement block with the given guard fields.
pub(super) fn hard_guard(
    tool: Option<&str>,
    deny_path: Option<&str>,
    deny_command: Option<&str>,
) -> Enforcement {
    Enforcement {
        mode: EnforcementMode::Hard,
        guard_tool: tool.map(str::to_string),
        guard_deny_command: deny_command.map(str::to_string),
        guard_deny_path: deny_path.map(str::to_string),
        severity: Some("error".to_string()),
        on_violation: None,
        check: None,
        rubric: None,
    }
}

/// A [`LoadedRecord`] around `record`, stamped, with no handle yet.
pub(super) fn loaded_from(mut record: Record) -> LoadedRecord {
    record.stamp(&Defaults::default()).expect("stamps");
    LoadedRecord {
        record,
        set_id: "acme.web".to_string(),
        source: ".stella/rules/acme.web.toml".to_string(),
        handle: String::new(),
        findings: Vec::new(),
    }
}

/// A published-record file with one record, in the shape ADR 0011 ratified.
const ONE_RECORD: &str = r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[defaults]
sharing_scope = "repository"
origin = "user"
status = "active"

[defaults.provenance]
source_kind = "document"
source_uri = "CLAUDE.md"
commit = "6ee3d4a"

[[record]]
lineage_id = "ctx.acme.web.pkg-manager"
kind = "rule"
statement = "This repository uses pnpm exclusively; npm and yarn must not be used."

[record.steering]
force = "must"
precedence = 60

[record.truth]
basis = "measured"
confidence = 95
"#;

#[test]
fn a_published_record_file_loads_with_defaults_merged() {
    let records = load_context_file(".stella/rules/acme.web.toml", ONE_RECORD).expect("loads");
    assert_eq!(records.len(), 1);
    let loaded = &records[0];
    assert_eq!(loaded.set_id, "acme.web");
    assert_eq!(loaded.record.origin, Some(Origin::User));
    assert_eq!(
        loaded.record.sharing_scope,
        Some(SharingScope::Repository),
        "file defaults must reach the record"
    );
    assert_eq!(
        loaded
            .record
            .provenance
            .as_ref()
            .and_then(|p| p.commit.as_deref()),
        Some("6ee3d4a"),
        "provenance is what makes a record citable with commit-level authority"
    );
}

#[test]
fn a_hand_authored_record_has_its_identity_stamped_from_content() {
    let records = load_context_file("f.toml", ONE_RECORD).expect("loads");
    let loaded = &records[0];
    assert!(
        loaded
            .record
            .record_id
            .as_deref()
            .unwrap()
            .starts_with("rec_acme_web_pkg_manager_"),
        "{:?}",
        loaded.record.record_id
    );
    assert!(
        loaded
            .record
            .record_hash
            .as_deref()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(loaded.findings, vec![RecordFinding::IdentityStamped]);
    assert_eq!(
        loaded.severity(),
        Some(Severity::Note),
        "stamping is not a problem"
    );
    assert!(loaded.is_selectable());
}

// The #886 acceptance criterion.

#[test]
fn record_hash_recomputes_to_the_stored_value_for_an_unedited_record() {
    let stamped = &load_context_file("f.toml", ONE_RECORD).expect("loads")[0];
    let hash = stamped.record.record_hash.clone().expect("stamped");
    let id = stamped.record.record_id.clone().expect("stamped");

    // Write the identity back into the file, exactly as publication does, and load
    // again. An unedited record must verify.
    let with_identity = ONE_RECORD.replace(
        "lineage_id = \"ctx.acme.web.pkg-manager\"",
        &format!(
            "lineage_id = \"ctx.acme.web.pkg-manager\"\nrecord_id = \"{id}\"\nrecord_hash = \"{hash}\""
        ),
    );
    let reloaded = &load_context_file("f.toml", &with_identity).expect("loads")[0];
    assert_eq!(reloaded.record.record_hash.as_deref(), Some(hash.as_str()));
    assert!(
        reloaded.findings.is_empty(),
        "an unedited record must load clean: {:?}",
        reloaded.findings
    );
}

#[test]
fn a_hand_edited_statement_mints_a_new_revision_rather_than_being_accepted() {
    let stamped = &load_context_file("f.toml", ONE_RECORD).expect("loads")[0];
    let stale_hash = stamped.record.record_hash.clone().expect("stamped");
    // Same stored hash, different statement — the shape of somebody editing the
    // semantics of a published record by hand.
    let edited = ONE_RECORD
        .replace(
            "lineage_id = \"ctx.acme.web.pkg-manager\"",
            &format!("lineage_id = \"ctx.acme.web.pkg-manager\"\nrecord_hash = \"{stale_hash}\""),
        )
        .replace("uses pnpm exclusively", "uses npm exclusively");

    let reloaded = &load_context_file("f.toml", &edited).expect("loads")[0];
    let mismatch = reloaded
        .findings
        .iter()
        .find_map(|finding| match finding {
            RecordFinding::HashMismatch { stored, recomputed } => Some((stored, recomputed)),
            _ => None,
        })
        .expect("the edit must be detected");
    assert_eq!(mismatch.0, &stale_hash);
    assert_ne!(
        mismatch.1, &stale_hash,
        "the new content must hash to a new identity"
    );
    assert_eq!(
        reloaded.record.record_hash.as_ref(),
        Some(mismatch.1),
        "the record adopts the recomputed hash — the stored one is never written back \
         over new content"
    );
    assert!(
        reloaded.is_selectable(),
        "a hand-edit is reported, not silently disarmed: the statement is still policy"
    );
}

#[test]
fn a_record_missing_its_source_uri_inherits_the_file_it_was_read_from() {
    let no_provenance = r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[[record]]
lineage_id = "ctx.acme.web.orphan"
kind = "fact"
statement = "A statement with no declared provenance."
"#;
    let loaded =
        &load_context_file(".stella/rules/found-here.toml", no_provenance).expect("loads")[0];
    assert_eq!(
        loaded
            .record
            .provenance
            .as_ref()
            .and_then(|p| p.source_uri.as_deref()),
        Some(".stella/rules/found-here.toml"),
        "a record with no provenance must still be traceable to an authority"
    );
}

#[test]
fn a_retracted_record_is_not_selectable() {
    let retracted = ONE_RECORD.replace("status = \"active\"", "status = \"retracted\"");
    let loaded = &load_context_file("f.toml", &retracted).expect("loads")[0];
    assert!(
        !loaded.is_selectable(),
        "a retracted record must leave selection — that is what revert relies on"
    );
}

#[test]
fn a_proposal_file_yields_no_published_records() {
    let proposals = r#"
schema = "context-record/v0.1"
set_id = "acme.web"
ingest_run_id = "ing_01"

[[proposal]]
candidate_id = "pkg-manager-9f3c1a2b"
proposal_kind = "directive"
status = "eligible"
confidence = 92
observed_at = "2026-07-20T18:30:00Z"

[proposal.record]
lineage_id = "ctx.acme.web.pkg-manager"
kind = "rule"
statement = "This repository uses pnpm exclusively."
"#;
    assert!(
        load_context_file("p.toml", proposals)
            .expect("loads")
            .is_empty(),
        "a proposal is not a record until somebody keeps it"
    );
}

// Malformed input

#[test]
fn malformed_toml_is_an_error_the_caller_must_surface() {
    let err = load_context_file(
        "broken.toml",
        "schema = \"context-record/v0.1\"\n[[record]\n",
    )
    .expect_err("must not load");
    assert_eq!(err.source, "broken.toml");
    assert!(!err.detail.is_empty());
    assert!(err.to_string().starts_with("broken.toml: "));
}

#[test]
fn an_unknown_schema_tag_is_refused_rather_than_guessed_at() {
    let future = r#"
schema = "context-record/v9.9"
set_id = "acme.web"
"#;
    let err = load_context_file("f.toml", future).expect_err("must not load");
    assert!(err.detail.contains("context-record/v0.1"), "{}", err.detail);
}

// The substrate rule (#893): sharing_scope selects the location.

#[test]
fn sharing_scope_selects_which_location_a_record_is_published_to() {
    assert_eq!(
        publication_dir(SharingScope::Personal),
        PublicationDir::User
    );
    assert_eq!(
        publication_dir(SharingScope::Repository),
        PublicationDir::Repository
    );
    assert_eq!(
        publication_dir(SharingScope::Organization),
        PublicationDir::Repository,
        "an organization record is published through a repository the org owns"
    );
}

// The whole chain, in one test: load → handle → validate → sweep → render.

#[test]
fn a_kept_record_reaches_the_prompt_citably_and_a_refuted_one_does_not() {
    let file = r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[defaults]
origin = "user"
status = "active"

[[record]]
lineage_id = "ctx.acme.web.pkg-manager"
kind = "rule"
statement = "This repository uses pnpm exclusively."

[record.steering]
force = "must"
precedence = 60

[[record]]
lineage_id = "ctx.acme.web.node-version"
kind = "fact"
statement = "Development runs on Node 20."

[record.steering]
force = "must"
precedence = 60

[record.truth]
basis = "measured"

[record.truth.probe]
kind = "file_contains"
path = ".nvmrc"
pattern = "20"
"#;
    let mut records = load_context_file(".stella/rules/acme.web.toml", file).expect("loads");
    assign_handles(&mut records);
    let conflicts = validate_records(&mut records);
    assert!(conflicts.is_empty(), "{conflicts:?}");
    assert_eq!(
        records
            .iter()
            .map(|r| r.handle.as_str())
            .collect::<Vec<_>>(),
        vec!["pkg-manager", "node-version"]
    );

    // The `.nvmrc` now says 22, so the node-version claim is refuted.
    let dispositions: Vec<Disposition> = records
        .iter()
        .map(|loaded| {
            let verdict = (loaded.handle == "node-version")
                .then_some(super::super::ingest::record::Verdict::Refuted);
            sweep::disposition(&SweepInput {
                record: &loaded.record,
                verdict,
                last_checked: None,
                now: "2026-07-20T00:00:00Z",
            })
        })
        .collect();
    let inputs: Vec<render::RenderInput<'_>> = records
        .iter()
        .zip(&dispositions)
        .map(|(record, disposition)| render::RenderInput {
            record,
            disposition,
            enforced: false,
        })
        .collect();

    let cached = render_channel(&inputs, Channel::Cached, None);
    assert_eq!(cached.rendered, vec!["pkg-manager"]);
    assert!(
        !cached.text.contains("Node 20"),
        "the refuted claim must not reach the model at all: {}",
        cached.text
    );
    assert!(cached.text.contains("^pkg-manager"), "{}", cached.text);
    assert!(
        render_channel(&inputs, Channel::Volatile, None)
            .text
            .is_empty(),
        "a dropped record does not reappear in the other channel"
    );
}
