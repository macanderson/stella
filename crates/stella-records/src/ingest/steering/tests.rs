//! Parse and refusal tests for the `steering-record/v1` reader.
//!
//! [`FIXTURE`] is the record Oxagen writes for the refunds rule, byte for
//! byte. Every refusal test starts from it and changes one thing, so a test
//! fails for the reason its name gives and no other.

use super::*;

/// The refunds record, just as a steering PR writes it.
const FIXTURE: &str = "---
schema: steering-record/v1
lineage: a-intel.core-platform.refunds-over-100
label: Refunds over $100
kind: business-rule
force: must
scope: workspace
status: active
origin: user
provenance:
  source: proposal
  uri: oxagen:proposal/prp_01K5RW2P
id: rec_a_intel_core_platform_refunds_over_100_f7e8bb90125e
hash: sha256:c9365650e669e54d69a420a621d98c45866445ffe98eae67a2b56b0e507577e3
---

Refunds over $100 need a person's approval before the agent calls Stripe.
";

const FIXTURE_ID: &str = "rec_a_intel_core_platform_refunds_over_100_f7e8bb90125e";
const FIXTURE_HASH: &str =
    "sha256:c9365650e669e54d69a420a621d98c45866445ffe98eae67a2b56b0e507577e3";

/// The fixture with its first `from` swapped for `to`. It panics when the
/// fixture has no `from`, so a typo in a test fails.
fn with(from: &str, to: &str) -> String {
    assert!(FIXTURE.contains(from), "the fixture has no `{from}`");
    FIXTURE.replacen(from, to, 1)
}

fn refusal(text: &str) -> SteeringRecordError {
    SteeringRecord::parse(text).expect_err("the file should be refused")
}

#[test]
fn the_fixture_parses_field_for_field() {
    let record = SteeringRecord::parse(FIXTURE).expect("the fixture parses");
    assert_eq!(
        record,
        SteeringRecord {
            lineage: "a-intel.core-platform.refunds-over-100".to_string(),
            label: "Refunds over $100".to_string(),
            description: None,
            kind: SteeringKind::BusinessRule,
            name: None,
            effect: None,
            force: Force::Must,
            scope: SteeringScope::Workspace,
            repos: Vec::new(),
            tools: Vec::new(),
            skills: Vec::new(),
            toolbelt: None,
            applies_to: Vec::new(),
            load: None,
            status: RecordStatus::Active,
            origin: Origin::User,
            provenance: SteeringProvenance {
                source: SteeringSource::Proposal,
                uri: "oxagen:proposal/prp_01K5RW2P".to_string(),
                agent: None,
                memories: Vec::new(),
            },
            id: Some(FIXTURE_ID.to_string()),
            hash: Some(FIXTURE_HASH.to_string()),
            statement: "Refunds over $100 need a person's approval before the agent calls Stripe."
                .to_string(),
        }
    );
    assert_eq!(record.effective_load(), SteeringLoad::Always);
}

#[test]
fn the_fixture_keeps_oxagen_identity_on_the_record() {
    let record = SteeringRecord::parse(FIXTURE)
        .and_then(|steering| steering.to_record())
        .expect("the fixture loads");
    assert_eq!(record.lineage_id, "a-intel.core-platform.refunds-over-100");
    assert_eq!(record.record_id.as_deref(), Some(FIXTURE_ID));
    assert_eq!(record.record_hash.as_deref(), Some(FIXTURE_HASH));
    assert_eq!(record.kind, RecordKind::Rule);
    assert_eq!(
        record.statement,
        "Refunds over $100 need a person's approval before the agent calls Stripe."
    );
    assert_eq!(record.origin, Some(Origin::User));
    assert_eq!(record.status, Some(RecordStatus::Active));
    assert_eq!(record.sharing_scope, Some(SharingScope::Repository));
    assert_eq!(record.tier(), Tier::Pinned);
    assert!(record.tags.is_empty());
    let provenance = record.provenance.expect("provenance is carried");
    assert_eq!(provenance.source_kind.as_deref(), Some("proposal"));
    assert_eq!(
        provenance.source_uri.as_deref(),
        Some("oxagen:proposal/prp_01K5RW2P")
    );
}

#[test]
fn a_wrong_schema_is_refused_by_name() {
    assert_eq!(
        refusal(&with(
            "schema: steering-record/v1",
            "schema: steering-record/v2"
        )),
        SteeringRecordError::WrongSchema {
            found: "steering-record/v2".to_string()
        }
    );
}

#[test]
fn a_missing_schema_is_refused() {
    assert_eq!(
        refusal(&with("schema: steering-record/v1\n", "")),
        SteeringRecordError::MissingSchema
    );
}

#[test]
fn a_file_without_a_frontmatter_fence_is_refused() {
    let text = FIXTURE
        .strip_prefix("---\n")
        .expect("the fixture opens with a fence");
    assert_eq!(refusal(text), SteeringRecordError::MissingFence);
}

#[test]
fn a_frontmatter_without_a_closing_fence_is_refused() {
    assert_eq!(
        refusal(&with("---\n\nRefunds", "\nRefunds")),
        SteeringRecordError::MissingClosingFence
    );
}

#[test]
fn an_unknown_kind_is_refused_by_name() {
    assert_eq!(
        refusal(&with("kind: business-rule", "kind: guideline")),
        SteeringRecordError::UnknownKind("guideline".to_string())
    );
}

#[test]
fn crlf_line_endings_are_refused_with_the_line() {
    assert_eq!(
        refusal(&FIXTURE.replace('\n', "\r\n")),
        SteeringRecordError::CarriageReturn { line: 1 }
    );
}

#[test]
fn a_missing_final_newline_is_refused() {
    let text = FIXTURE
        .strip_suffix('\n')
        .expect("the fixture ends with a newline");
    assert_eq!(refusal(text), SteeringRecordError::MissingFinalNewline);
}

#[test]
fn a_yaml_tag_is_refused() {
    assert_eq!(
        refusal(&with(
            "label: Refunds over $100",
            "label: !money Refunds over $100"
        )),
        SteeringRecordError::Tagged
    );
}

#[test]
fn an_unknown_field_is_refused() {
    assert!(matches!(
        refusal(&with("status: active\n", "status: active\npriority: 1\n")),
        SteeringRecordError::Frontmatter(_)
    ));
}

#[test]
fn an_explicit_null_is_refused() {
    assert!(matches!(
        refusal(&with("status: active\n", "status: active\nload: null\n")),
        SteeringRecordError::Frontmatter(_)
    ));
}

#[test]
fn an_empty_body_is_refused() {
    let (head, _) = FIXTURE
        .split_once("Refunds over $100 need")
        .expect("the fixture has a body");
    assert_eq!(refusal(head), SteeringRecordError::EmptyBody);
}

#[test]
fn a_constraint_needs_an_effect() {
    assert_eq!(
        refusal(&with("kind: business-rule", "kind: constraint")),
        SteeringRecordError::Invalid {
            field: "effect",
            detail: "is required on a constraint record",
        }
    );
}

#[test]
fn a_constraint_effect_rides_as_a_tag() {
    let record = SteeringRecord::parse(&with(
        "kind: business-rule",
        "kind: constraint\neffect: forbid",
    ))
    .and_then(|steering| steering.to_record())
    .expect("the constraint loads");
    assert_eq!(record.kind, RecordKind::Constraint);
    assert_eq!(record.tags, vec!["effect:forbid".to_string()]);
}

#[test]
fn a_bad_record_id_is_refused() {
    assert_eq!(
        refusal(&with(FIXTURE_ID, "rec_refunds_F7E8BB90125E")),
        SteeringRecordError::Invalid {
            field: "id",
            detail: "must be `rec_`, the lineage slug, `_`, and 12 lowercase hex digits",
        }
    );
}

#[test]
fn a_label_over_36_characters_is_refused() {
    assert_eq!(
        refusal(&with(
            "label: Refunds over $100",
            "label: Refunds over one hundred dollars need approval"
        )),
        SteeringRecordError::Invalid {
            field: "label",
            detail: "must be 1 to 36 characters long",
        }
    );
}

#[test]
fn a_declared_load_sets_the_tier() {
    let record = SteeringRecord::parse(&with(
        "status: active\n",
        "status: active\nload: match\napplies_to:\n  - src/billing/**\n",
    ))
    .and_then(|steering| steering.to_record())
    .expect("the record loads");
    assert_eq!(record.tier(), Tier::Scoped);
    let applies_to = record
        .steering
        .and_then(|steering| steering.applies_to)
        .expect("applies_to is carried");
    assert_eq!(applies_to.paths, vec!["src/billing/**".to_string()]);
}

/// A record whose targeting Stella cannot match parses, and `to_record`
/// withholds it with the field it cannot match.
fn withheld_for(text: &str) -> SteeringRecordError {
    SteeringRecord::parse(text)
        .expect("the record parses")
        .to_record()
        .expect_err("the record should be withheld")
}

#[test]
fn a_tool_targeted_record_is_withheld_not_broadened() {
    let text = with(
        "status: active\n",
        "status: active\nload: match\ntools:\n  - stripe.refund\n",
    );
    assert_eq!(
        withheld_for(&text),
        SteeringRecordError::UnsupportedTarget("tools")
    );
}

#[test]
fn a_skill_targeted_record_is_withheld_not_broadened() {
    let text = with(
        "status: active\n",
        "status: active\nload: match\nskills:\n  - refund-review\n",
    );
    assert_eq!(
        withheld_for(&text),
        SteeringRecordError::UnsupportedTarget("skills")
    );
}

#[test]
fn a_toolbelt_targeted_record_is_withheld_not_broadened() {
    let text = with(
        "status: active\n",
        "status: active\nload: match\ntoolbelt: billing\n",
    );
    assert_eq!(
        withheld_for(&text),
        SteeringRecordError::UnsupportedTarget("toolbelt")
    );
}

#[test]
fn a_repository_scoped_record_is_withheld_not_broadened() {
    let text = with(
        "scope: workspace\n",
        "scope: repository\nrepos:\n  - acme/api\n",
    );
    assert_eq!(
        withheld_for(&text),
        SteeringRecordError::UnsupportedTarget("repos")
    );
}

#[test]
fn a_skill_record_parses_and_does_not_load() {
    let text = with(
        "kind: business-rule",
        "kind: skill\nname: refund-review\ndescription: Review a refund before it goes out",
    );
    let steering = SteeringRecord::parse(&text).expect("the skill record parses");
    assert_eq!(steering.kind, SteeringKind::Skill);
    assert_eq!(
        steering.to_record(),
        Err(SteeringRecordError::UnsupportedKind("skill"))
    );
}

#[test]
fn a_run_source_needs_memories() {
    assert_eq!(
        refusal(&with("source: proposal", "source: run")),
        SteeringRecordError::Invalid {
            field: "provenance.memories",
            detail: "is required when the source is run",
        }
    );
}

#[test]
fn a_run_memory_keeps_a_null_agent() {
    let text = with(
        "  uri: oxagen:proposal/prp_01K5RW2P\n",
        "  uri: oxagen:run/run_01\n  memories:\n    - agent: null\n      run: run_01\n      statement: A person approved the refund.\n      evidence: []\n",
    )
    .replacen("source: proposal", "source: run", 1);
    let record = SteeringRecord::parse(&text).expect("the run record parses");
    assert_eq!(
        record.provenance.memories,
        vec![SteeringMemory {
            agent: None,
            run: Some("run_01".to_string()),
            statement: "A person approved the refund.".to_string(),
            evidence: Vec::new(),
        }]
    );
}

// Tests that load records through the registry.

/// A project file with one native record. Its statement has two line
/// breaks. A native record must be one sentence, so this one is blocked.
const NATIVE_MULTILINE: &str = r#"
schema = "context-record/v0.1"
set_id = "acme.web"

[defaults]
origin = "user"
status = "active"

[[record]]
lineage_id = "ctx.acme.web.build"
kind = "rule"
statement = "Run the build.\n$ pnpm build\nDone in 4.2s"

[record.steering]
force = "must"
precedence = 50
"#;

/// The fixture as a procedure with this body.
fn procedure(body: &str) -> String {
    with(
        "Refunds over $100 need a person's approval before the agent calls Stripe.\n",
        body,
    )
    .replacen("kind: business-rule", "kind: procedure", 1)
}

/// Load one native file and one steering file.
fn load(native: &str, steering: &str) -> crate::records::Registry {
    let file = |path: &str, contents: &str| stella_learn::rules::RuleFile {
        path: path.to_string(),
        contents: contents.to_string(),
        contributed_by: None,
    };
    crate::records::registry::load_with_steering(
        &[],
        &[file(".stella/rules/acme.web.toml", native)],
        &[file("steering/procedures/refunds.md", steering)],
        &crate::records::Facts {
            now: "2026-09-28T00:00:00Z",
            ..crate::records::Facts::default()
        },
    )
}

fn entry<'a>(registry: &'a crate::records::Registry, lineage: &str) -> &'a crate::records::Entry {
    registry
        .entries
        .iter()
        .find(|entry| entry.record.record.lineage_id == lineage)
        .unwrap_or_else(|| panic!("no entry for {lineage}: {:?}", registry.diagnostics))
}

fn blocking(entry: &crate::records::Entry) -> Vec<&crate::records::RecordFinding> {
    entry
        .record
        .findings
        .iter()
        .filter(|finding| finding.severity() == crate::records::Severity::Blocking)
        .collect()
}

#[test]
fn a_multiline_steering_body_steers_and_a_multiline_native_statement_does_not() {
    let steering = procedure(
        "1. Read the refund and its order.\n2. Ask a person to approve a refund over $100.\n3. Call Stripe after the approval.\n",
    );
    let registry = load(NATIVE_MULTILINE, &steering);
    assert!(
        registry.diagnostics.is_empty(),
        "{:?}",
        registry.diagnostics
    );

    let steered = entry(&registry, "a-intel.core-platform.refunds-over-100");
    assert_eq!(steered.record.record.statement.lines().count(), 3);
    assert!(
        blocking(steered).is_empty(),
        "{:?}",
        steered.record.findings
    );
    assert!(
        !matches!(
            steered.disposition,
            crate::records::Disposition::Block { .. }
        ),
        "{:?}",
        steered.disposition
    );

    let native = entry(&registry, "ctx.acme.web.build");
    assert!(
        native.record.findings.iter().any(|finding| matches!(
            finding,
            crate::records::RecordFinding::GuardLint(detail) if detail.contains("single")
        )),
        "{:?}",
        native.record.findings
    );
    assert!(
        matches!(
            native.disposition,
            crate::records::Disposition::Block { .. }
        ),
        "{:?}",
        native.disposition
    );
}

#[test]
fn a_steering_body_with_a_list_skips_the_compound_claim_check() {
    let statement = "Read the refund, the order, and the invoice before you call Stripe.";
    let pasted = r"Run the build.\n$ pnpm build\nDone in 4.2s";
    assert!(
        NATIVE_MULTILINE.contains(pasted),
        "the native file has no `{pasted}`"
    );
    let native = NATIVE_MULTILINE.replacen(pasted, statement, 1);
    let registry = load(&native, &procedure(&format!("{statement}\n")));
    assert!(
        registry.diagnostics.is_empty(),
        "{:?}",
        registry.diagnostics
    );

    let steered = entry(&registry, "a-intel.core-platform.refunds-over-100");
    assert!(
        blocking(steered).is_empty(),
        "{:?}",
        steered.record.findings
    );

    let native = entry(&registry, "ctx.acme.web.build");
    assert!(!blocking(native).is_empty(), "{:?}", native.record.findings);
}
