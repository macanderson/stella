//! Schema fidelity and identity tests for the TOML context-record surface.
//!
//! The real `docs/design/adaptive-context/context-record-examples/*.toml` files use *bare* TOML
//! datetimes; the surface types store timestamps as strings (module docs), so
//! these tests round-trip through TOML with quoted timestamps — the normalized
//! form the ingest command emits — and parse a `05`-shaped proposal file to prove
//! the schema matches the example shape.

use super::*;
use crate::context_record::kind::{Origin, RecordProposalKind, RecordProposalStatus, RecordStatus};

fn constraint_record() -> Record {
    Record {
        lineage_id: "ctx.acme.web.pkg-manager".to_string(),
        record_id: None,
        record_hash: None,
        kind: RecordKind::Constraint,
        statement: "This repository uses pnpm exclusively; npm and yarn must not be used."
            .to_string(),
        tags: vec!["build".to_string(), "tooling".to_string()],
        origin: None,
        sharing_scope: None,
        status: None,
        provenance: None,
        steering: Some(Steering {
            force: Force::Must,
            precedence: Some(100),
            applies_to: Some(AppliesTo {
                paths: vec!["**".to_string()],
                tasks: vec!["install".to_string(), "build".to_string()],
                keywords: vec![],
            }),
        }),
        enforcement: Some(Enforcement {
            mode: EnforcementMode::Hard,
            guard_tool: Some("Bash".to_string()),
            guard_deny_command: Some("npm *".to_string()),
            guard_deny_path: None,
            severity: Some("error".to_string()),
            on_violation: None,
            check: None,
            rubric: None,
        }),
        truth: Some(Truth {
            basis: TruthBasis::Decree,
            confidence: Some(100),
            verified_by: Some("@dana".to_string()),
            verified_at: None,
            valid_from: None,
            ttl: None,
            on_expiry: None,
            review_every: None,
            probe: Some(Probe {
                kind: ProbeKind::Manual,
                path: None,
                pattern: None,
                expect: None,
                note: Some("Confirm with the platform owner quarterly.".to_string()),
            }),
        }),
        links: vec![Link {
            relation: LinkRelation::Supports,
            target: "ctx.acme.web.foreign-lockfile-breaks-ci".to_string(),
        }],
    }
}

fn repository_defaults() -> Defaults {
    Defaults {
        sharing_scope: Some(SharingScope::Repository),
        origin: Some(Origin::Imported),
        status: Some(RecordStatus::Active),
        review_every: Some("P90D".to_string()),
        provenance: Some(Provenance {
            source_kind: Some("document".to_string()),
            source_uri: Some("CLAUDE.md".to_string()),
            source_digest: Some("sha256:6b1f".to_string()),
            source_lines: None,
            repo: Some("git@github.com:acme/web.git".to_string()),
            commit: Some("9f2c1ab".to_string()),
            ingest_run_id: None,
            extracted_at: Some("2026-07-27T09:00:00Z".to_string()),
            extractor: Some("claude-opus-5".to_string()),
        }),
    }
}

#[test]
fn stamp_is_deterministic_and_content_derived() {
    let defaults = repository_defaults();

    let mut a = constraint_record();
    a.stamp(&defaults).expect("stamp a");
    let mut b = constraint_record();
    b.stamp(&defaults).expect("stamp b");

    // Same content, same defaults → same identity: this is what makes
    // re-ingesting unchanged content a no-op.
    assert_eq!(a.record_id, b.record_id);
    assert_eq!(a.record_hash, b.record_hash);

    let id = a.record_id.clone().expect("id stamped");
    assert!(id.starts_with("rec_acme_web_pkg_manager_"), "id was {id}");
    let hash = a.record_hash.clone().expect("hash stamped");
    assert!(hash.starts_with("sha256:"));

    // A changed statement changes the id — an edit is a new revision.
    let mut c = constraint_record();
    c.statement = "This repository uses pnpm exclusively.".to_string();
    c.stamp(&defaults).expect("stamp c");
    assert_ne!(a.record_id, c.record_id);
}

#[test]
fn stamp_merges_file_defaults_onto_the_record() {
    let defaults = repository_defaults();
    let mut record = constraint_record();
    assert!(record.origin.is_none());

    record.stamp(&defaults).expect("stamp");

    assert_eq!(record.origin, Some(Origin::Imported));
    assert_eq!(record.sharing_scope, Some(SharingScope::Repository));
    assert_eq!(record.status, Some(RecordStatus::Active));
    let provenance = record.provenance.as_ref().expect("provenance merged");
    assert_eq!(provenance.source_uri.as_deref(), Some("CLAUDE.md"));
    assert_eq!(provenance.extractor.as_deref(), Some("claude-opus-5"));
}

#[test]
fn a_per_record_override_wins_over_the_default() {
    let defaults = repository_defaults();
    let mut record = constraint_record();
    // Personal overrides the repository default.
    record.sharing_scope = Some(SharingScope::Personal);
    record.provenance = Some(Provenance {
        source_uri: Some("AGENTS.md".to_string()),
        ..Default::default()
    });

    record.stamp(&defaults).expect("stamp");

    assert_eq!(record.sharing_scope, Some(SharingScope::Personal));
    let provenance = record.provenance.as_ref().expect("provenance");
    // Overridden field wins; unset fields still fall back to the default.
    assert_eq!(provenance.source_uri.as_deref(), Some("AGENTS.md"));
    assert_eq!(
        provenance.repo.as_deref(),
        Some("git@github.com:acme/web.git")
    );
}

#[test]
fn record_round_trips_through_toml() {
    let mut record = constraint_record();
    record.stamp(&repository_defaults()).expect("stamp");

    let serialized = toml::to_string(&record).expect("serialize");
    let parsed: Record = toml::from_str(&serialized).expect("parse");
    assert_eq!(record, parsed);
}

#[test]
fn a_whole_ingest_file_round_trips_through_toml() {
    let mut record = constraint_record();
    record.stamp(&repository_defaults()).expect("stamp");
    let file = ContextFile {
        schema: SCHEMA_TAG.to_string(),
        set_id: "acme.web".to_string(),
        ingest_run_id: Some("ing_01J8XQ".to_string()),
        defaults: Some(repository_defaults()),
        records: vec![],
        proposals: vec![Proposal {
            candidate_id: "pkg-manager-a41f9c2b".to_string(),
            proposal_kind: RecordProposalKind::Directive,
            status: RecordProposalStatus::Eligible,
            confidence: 95,
            observed_at: "2026-07-27T09:00:00Z".to_string(),
            eligibility: Some("explicit_instruction".to_string()),
            dismissed_reason: None,
            record,
            refutation: Some(Refutation {
                verdict: Verdict::Supported,
                checked_at: "2026-07-27T09:00:04Z".to_string(),
                probe_kind: ProbeKind::PathExists,
                detail: "pnpm-lock.yaml present.".to_string(),
                recommend: None,
                links: vec![],
            }),
            quarantine: None,
            validation: None,
        }],
    };

    let serialized = toml::to_string(&file).expect("serialize");
    let parsed: ContextFile = toml::from_str(&serialized).expect("parse");
    assert_eq!(file, parsed);
}

/// A `05-imported-proposals.toml`-shaped fixture with timestamps quoted (the
/// form ingest emits). Proves the schema parses the example shape, including the
/// refuted-with-link, unfalsifiable, and quarantined cases.
const PROPOSAL_FIXTURE: &str = r#"
schema        = "context-record/v0.1"
set_id        = "acme.web"
ingest_run_id = "ing_01J8XQ4M2K7ZP3VN5T0R9WB6HC"

[defaults]
sharing_scope = "repository"
origin        = "imported"

[defaults.provenance]
source_kind = "document"
source_uri  = "CLAUDE.md"

[[proposal]]
candidate_id  = "pkg-manager-a41f9c2b"
proposal_kind = "directive"
status        = "eligible"
confidence    = 95
observed_at   = "2026-07-27T09:00:00Z"
eligibility   = "explicit_instruction"

  [proposal.record]
  lineage_id = "ctx.acme.web.pkg-manager"
  kind       = "constraint"
  statement  = "This repository uses pnpm exclusively; npm and yarn must not be used."

    [proposal.record.steering]
    force      = "must"
    precedence = 100
    applies_to = { paths = ["**"], tasks = ["install", "build", "ci"] }

    [proposal.record.enforcement]
    mode               = "hard"
    guard_tool         = "Bash"
    guard_deny_command = "npm *"

    [proposal.record.truth]
    basis      = "decree"
    confidence = 95

  [proposal.refutation]
  verdict    = "supported"
  checked_at = "2026-07-27T09:00:04Z"
  probe_kind = "path_exists"
  detail     = "pnpm-lock.yaml present."

[[proposal]]
candidate_id  = "node-version-88b1e5df"
proposal_kind = "directive"
status        = "eligible"
confidence    = 90
observed_at   = "2026-07-27T09:00:00Z"

  [proposal.record]
  lineage_id = "ctx.acme.web.node-version"
  kind       = "constraint"
  statement  = "All development happens on Node 20.x."

    [proposal.record.steering]
    force = "must"

    [proposal.record.enforcement]
    mode = "soft"

    [proposal.record.truth]
    basis = "decree"

  [proposal.refutation]
  verdict    = "refuted"
  checked_at = "2026-07-27T09:00:05Z"
  probe_kind = "file_contains"
  detail     = ".nvmrc contains 22.11.0. Source document is stale."
  recommend  = "ignore"

    [[proposal.refutation.link]]
    relation = "contradicts"
    target   = "evidence:.nvmrc#L1"

[[proposal]]
candidate_id     = "reset-env-4ba7c012"
proposal_kind    = "directive"
status           = "dismissed"
confidence       = 40
observed_at      = "2026-07-27T09:00:00Z"
dismissed_reason = "quarantined_executable"

  [proposal.record]
  lineage_id = "ctx.acme.web.reset-env"
  kind       = "procedure"
  statement  = "A broken local environment is reset by clearing node_modules and reinstalling."

    [proposal.record.steering]
    force = "may"

    [proposal.record.enforcement]
    mode = "none"

    [proposal.record.truth]
    basis = "asserted"

  [proposal.quarantine]
  reason             = "executable_content_from_imported_origin"
  field              = "enforcement.autofix"
  raw                = "rm -rf node_modules && pnpm install --force"
  requires           = "human ratification; basis must be decree and verified_by set"
  never_honored_when = ["origin = imported", "origin = inferred"]
"#;

#[test]
fn proposal_file_parses_from_the_example_shape() {
    let file: ContextFile = toml::from_str(PROPOSAL_FIXTURE).expect("parse fixture");
    assert_eq!(file.schema, SCHEMA_TAG);
    assert_eq!(file.set_id, "acme.web");
    assert_eq!(file.proposals.len(), 3);

    let supported = &file.proposals[0];
    assert_eq!(supported.candidate_id, "pkg-manager-a41f9c2b");
    assert_eq!(supported.proposal_kind, RecordProposalKind::Directive);
    assert_eq!(supported.record.kind, RecordKind::Constraint);
    let steering = supported.record.steering.as_ref().expect("steering");
    assert_eq!(steering.force, Force::Must);
    assert!(steering.force.is_always_injected());
    let applies = steering.applies_to.as_ref().expect("applies_to");
    assert_eq!(applies.tasks, vec!["install", "build", "ci"]);
    let enforcement = supported.record.enforcement.as_ref().expect("enforcement");
    assert_eq!(enforcement.guard_deny_command.as_deref(), Some("npm *"));

    // The refuted proposal carries the contradiction link.
    let refuted = &file.proposals[1];
    let refutation = refuted.refutation.as_ref().expect("refutation");
    assert_eq!(refutation.verdict, Verdict::Refuted);
    assert_eq!(refutation.recommend.as_deref(), Some("ignore"));
    assert_eq!(refutation.links.len(), 1);
    assert_eq!(refutation.links[0].relation, LinkRelation::Contradicts);

    // The quarantined proposal is dismissed and preserves the raw command.
    let quarantined = &file.proposals[2];
    assert_eq!(quarantined.status, RecordProposalStatus::Dismissed);
    assert_eq!(
        quarantined.dismissed_reason.as_deref(),
        Some("quarantined_executable")
    );
    let quarantine = quarantined.quarantine.as_ref().expect("quarantine");
    assert_eq!(
        quarantine.raw,
        "rm -rf node_modules && pnpm install --force"
    );
}

#[test]
fn record_kind_maps_to_the_right_proposal_kind() {
    assert_eq!(
        RecordKind::Fact.proposal_kind(),
        RecordProposalKind::Knowledge
    );
    assert_eq!(
        RecordKind::Memory.proposal_kind(),
        RecordProposalKind::Knowledge
    );
    assert_eq!(
        RecordKind::Rule.proposal_kind(),
        RecordProposalKind::Directive
    );
    assert_eq!(
        RecordKind::Constraint.proposal_kind(),
        RecordProposalKind::Directive
    );
}
